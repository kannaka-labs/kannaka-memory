//! A minimal, strictly read-only IMAP4rev1 client over implicit TLS (993).
//!
//! Only the commands P0 needs exist, and every command line passes
//! [`guard_read_only`] before it is written: `LOGIN`, `LIST`, `EXAMINE` (never
//! `SELECT`), `UID SEARCH`, `UID FETCH` with `BODY.PEEK[...]` (never `BODY[`,
//! never `RFC822`, which set `\Seen`), `LOGOUT`. No STORE / APPEND / COPY /
//! MOVE / EXPUNGE can be sent.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::config::Credentials;
use super::{MailError, Result};

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// Refuse any IMAP command line that could change server state.
pub fn guard_read_only(line: &str) -> Result<()> {
    let mut it = line.split_whitespace();
    let _tag = it.next();
    let verb = it.next().unwrap_or("").to_ascii_uppercase();
    let upper = line.to_ascii_uppercase();
    let ok = match verb.as_str() {
        "CAPABILITY" | "LOGIN" | "LIST" | "EXAMINE" | "LOGOUT" | "NOOP" => true,
        "UID" => {
            let sub = it.next().unwrap_or("").to_ascii_uppercase();
            match sub.as_str() {
                "SEARCH" => true,
                // A FETCH may only PEEK: `BODY[` and `RFC822` (except .SIZE) set \Seen.
                "FETCH" => {
                    !upper.contains(" BODY[")
                        && !upper.contains("(BODY[")
                        && !upper.replace("RFC822.SIZE", "").contains("RFC822")
                }
                _ => false,
            }
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        // Never echo a LOGIN line (it cannot reach here, but be sure).
        Err(MailError::ReadOnly(format!("IMAP command `{verb}` is not allowed in read-only P0")))
    }
}

/// IMAP quoted string. Refuses CR/LF/NUL and non-ASCII (would need a literal).
fn quote(s: &str) -> Result<String> {
    if s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0 || b > 0x7e) {
        return Err(MailError::Imap("a login value contains characters that need an IMAP literal".into()));
    }
    Ok(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// Quote a mailbox name for EXAMINE.
fn mailbox_arg(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One untagged response, with its literals pulled out.
#[derive(Debug, Default)]
pub struct Untagged {
    pub text: String,
    pub literals: Vec<Vec<u8>>,
}

/// A fetched message's metadata (and body when asked for).
#[derive(Debug)]
pub struct Fetched {
    pub uid: u32,
    pub flags: Vec<String>,
    pub internaldate: Option<DateTime<Utc>>,
    pub data: Option<Vec<u8>>,
}

pub struct ImapSession {
    io: BufReader<Tls>,
    tag: u32,
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(cfg)
}

/// Compress sorted UIDs into an IMAP sequence set: `1:5,7,9:12`.
pub fn uid_set(uids: &[u32]) -> String {
    let mut v: Vec<u32> = uids.to_vec();
    v.sort_unstable();
    v.dedup();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        let mut end = start;
        while i + 1 < v.len() && v[i + 1] == end + 1 {
            i += 1;
            end = v[i];
        }
        parts.push(if start == end { start.to_string() } else { format!("{start}:{end}") });
        i += 1;
    }
    parts.join(",")
}

fn between<'a>(s: &'a str, open: &str, close: char) -> Option<&'a str> {
    let a = s.find(open)? + open.len();
    let b = s[a..].find(close)? + a;
    Some(&s[a..b])
}

impl ImapSession {
    pub fn connect(host: &str, port: u16) -> Result<Self> {
        let addr = (host, port)
            .to_socket_addrs()
            .map_err(|e| MailError::Imap(format!("resolve {host}: {e}")))?
            .next()
            .ok_or_else(|| MailError::Imap(format!("no address for {host}")))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(20))?;
        tcp.set_read_timeout(Some(Duration::from_secs(60)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(30)))?;
        let name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| MailError::Imap(format!("bad server name {host}: {e}")))?;
        let conn = rustls::ClientConnection::new(tls_config(), name)
            .map_err(|e| MailError::Imap(format!("tls: {e}")))?;
        let mut s = ImapSession { io: BufReader::new(rustls::StreamOwned::new(conn, tcp)), tag: 0 };
        let greeting = s.read_line()?;
        if !greeting.starts_with(b"* OK") && !greeting.starts_with(b"* PREAUTH") {
            return Err(MailError::Imap(format!("unexpected greeting: {}", String::from_utf8_lossy(&greeting).trim())));
        }
        Ok(s)
    }

    fn read_line(&mut self) -> Result<Vec<u8>> {
        let mut line = Vec::new();
        let n = self.io.read_until(b'\n', &mut line)?;
        if n == 0 {
            return Err(MailError::Imap("connection closed".into()));
        }
        Ok(line)
    }

    /// Send one command and collect its untagged responses. `secret_arg`
    /// marks a command whose text must never appear in an error.
    fn run(&mut self, command: &str, secret_arg: bool) -> Result<Vec<Untagged>> {
        self.tag += 1;
        let tag = format!("k{}", self.tag);
        let line = format!("{tag} {command}");
        guard_read_only(&line)?;
        let s = self.io.get_mut();
        s.write_all(line.as_bytes())?;
        s.write_all(b"\r\n")?;
        s.flush()?;
        let mut out: Vec<Untagged> = Vec::new();
        let mut cur: Option<Untagged> = None;
        loop {
            let raw = self.read_line()?;
            let text = String::from_utf8_lossy(&raw).to_string();
            let trimmed = text.trim_end_matches(['\r', '\n']);
            // literal announced at end of line: {n}
            let lit = trimmed
                .strip_suffix('}')
                .and_then(|t| t.rfind('{').map(|i| &t[i + 1..]))
                .and_then(|n| n.trim_end_matches('+').parse::<usize>().ok());
            let continuing = cur.is_some();
            if !continuing {
                if let Some(rest) = trimmed.strip_prefix(&format!("{tag} ")) {
                    let status = rest.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                    if status == "OK" {
                        return Ok(out);
                    }
                    let what = if secret_arg { "LOGIN".to_string() } else { command.split_whitespace().take(2).collect::<Vec<_>>().join(" ") };
                    return Err(MailError::Imap(format!("{what} failed: {rest}")));
                }
                cur = Some(Untagged::default());
            }
            let u = cur.as_mut().unwrap();
            if let Some(n) = lit {
                let brace = trimmed.rfind('{').unwrap();
                u.text.push_str(&trimmed[..brace]);
                u.text.push_str(&format!("{{LIT{}}}", u.literals.len()));
                let mut buf = vec![0u8; n];
                self.io.read_exact(&mut buf)?;
                u.literals.push(buf);
                continue; // the response goes on after the literal
            }
            u.text.push_str(trimmed);
            out.push(cur.take().unwrap());
        }
    }

    pub fn login(&mut self, c: &Credentials) -> Result<()> {
        let cmd = format!("LOGIN {} {}", quote(&c.user)?, quote(c.pass.expose())?);
        self.run(&cmd, true).map(|_| ())
    }

    /// `(attributes, name)` of every mailbox.
    pub fn list(&mut self) -> Result<Vec<(String, String)>> {
        let rs = self.run("LIST \"\" \"*\"", false)?;
        Ok(rs
            .into_iter()
            .filter(|u| u.text.starts_with("* LIST"))
            .filter_map(|u| {
                let attrs = between(&u.text, "(", ')')?.to_string();
                let name = if let Some(i) = u.text.find("{LIT0}") {
                    let _ = i;
                    String::from_utf8_lossy(u.literals.first()?).to_string()
                } else {
                    let t = u.text.trim_end();
                    if t.ends_with('"') {
                        let inner = &t[..t.len() - 1];
                        let start = inner.rfind('"')? + 1;
                        inner[start..].to_string()
                    } else {
                        t.rsplit(' ').next()?.to_string()
                    }
                };
                Some((attrs, name))
            })
            .collect())
    }

    /// EXAMINE (read-only select). Returns UIDVALIDITY.
    pub fn examine(&mut self, mailbox: &str) -> Result<u32> {
        let rs = self.run(&format!("EXAMINE {}", mailbox_arg(mailbox)), false)?;
        rs.iter()
            .find_map(|u| between(&u.text, "[UIDVALIDITY ", ']').and_then(|v| v.trim().parse().ok()))
            .ok_or_else(|| MailError::Imap(format!("no UIDVALIDITY for {mailbox}")))
    }

    pub fn uid_search_since(&mut self, since: DateTime<Utc>) -> Result<Vec<u32>> {
        let rs = self.run(&format!("UID SEARCH SINCE {}", since.format("%d-%b-%Y")), false)?;
        Ok(rs
            .iter()
            .filter(|u| u.text.starts_with("* SEARCH"))
            .flat_map(|u| u.text["* SEARCH".len()..].split_whitespace().filter_map(|n| n.parse().ok()).collect::<Vec<u32>>())
            .collect())
    }

    /// `UID FETCH <set> <items>`; items must be PEEK-only (guarded).
    pub fn uid_fetch(&mut self, uids: &[u32], items: &str) -> Result<Vec<Fetched>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let rs = self.run(&format!("UID FETCH {} {}", uid_set(uids), items), false)?;
        Ok(rs
            .into_iter()
            .filter(|u| u.text.contains(" FETCH ("))
            .filter_map(|u| {
                let uid: u32 = {
                    let i = u.text.find("UID ")? + 4;
                    u.text[i..].split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?
                };
                let flags = between(&u.text, "FLAGS (", ')')
                    .map(|f| f.split_whitespace().map(|s| s.to_string()).collect())
                    .unwrap_or_default();
                let internaldate = between(&u.text, "INTERNALDATE \"", '"').and_then(super::parse::parse_internaldate);
                let data = u.literals.into_iter().next();
                Some(Fetched { uid, flags, internaldate, data })
            })
            .collect())
    }

    pub fn logout(mut self) {
        let _ = self.run("LOGOUT", false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_allows_only_read_only_commands() {
        for ok in [
            "k1 LOGIN \"u\" \"p\"",
            "k2 LIST \"\" \"*\"",
            "k3 EXAMINE \"INBOX\"",
            "k4 UID SEARCH SINCE 26-Aug-2026",
            "k5 UID FETCH 1:5 (UID FLAGS INTERNALDATE)",
            "k6 UID FETCH 7 (UID FLAGS INTERNALDATE BODY.PEEK[])",
            "k7 UID FETCH 7 (BODY.PEEK[HEADER.FIELDS (FROM TO)] RFC822.SIZE)",
            "k8 LOGOUT",
        ] {
            assert!(guard_read_only(ok).is_ok(), "{ok}");
        }
        for bad in [
            "k1 SELECT \"INBOX\"",
            "k2 UID STORE 5 +FLAGS (\\Seen)",
            "k3 STORE 5 +FLAGS (\\Answered)",
            "k4 APPEND \"Sent\" {10}",
            "k5 EXPUNGE",
            "k6 UID EXPUNGE 5",
            "k7 UID FETCH 7 (BODY[])",
            "k8 UID FETCH 7 (UID BODY[TEXT])",
            "k9 UID FETCH 7 RFC822",
            "k10 UID COPY 7 \"Archive\"",
            "k11 UID MOVE 7 \"Trash\"",
            "k12 DELETE \"INBOX\"",
        ] {
            assert!(guard_read_only(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn uid_sets_compress() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 7, 9, 10, 11, 3]), "1:3,5,7,9:11");
        assert_eq!(uid_set(&[4]), "4");
    }

    #[test]
    fn quote_refuses_injection() {
        assert!(quote("a\r\nk2 STORE 1 +FLAGS (\\Seen)").is_err());
        assert_eq!(quote("p\"q\\r").unwrap(), "\"p\\\"q\\\\r\"");
    }
}
