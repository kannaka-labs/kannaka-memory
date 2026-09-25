//! Accounts: which addresses this agent has, where each one's authority is,
//! and which credential file holds its secret.
//!
//! `<data_dir>/mail/accounts.toml`:
//!
//! ```toml
//! internal_domains = ["ninja-portal.com"]   # agent-to-agent mail, never an open loop
//!
//! [[account]]
//! name = "zoho"
//! address = "kannaka@spacechild.love"
//! display_name = "Kannaka"
//! transport = "imap"                        # imap | jmap
//! host = "imap.zoho.com"
//! port = 993
//! credentials = "~/.kannaka-mail.env"       # KANNAKA_MAIL_USER / KANNAKA_MAIL_PASS
//!
//! [[account]]
//! name = "np"
//! address = "kannaka@ninja-portal.com"
//! display_name = "Kannaka"
//! transport = "jmap"
//! url = "https://mail.ninja-portal.com"
//! credentials = "~/.kannaka-mail-ninja-portal.env"
//! ```
//!
//! Without the file, accounts are discovered from `~/.kannaka-mail.env`
//! (`primary`, IMAP) and `~/.kannaka-mail-<name>.env` (`<name>`, IMAP on
//! `KANNAKA_MAIL_IMAP_HOST`). The secret is read only when a transport logs
//! in, is never logged, and never reaches the refs.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{MailError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Imap,
    Jmap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub transport: Transport,
    /// IMAP host.
    #[serde(default)]
    pub host: Option<String>,
    /// IMAP port (implicit TLS), default 993.
    #[serde(default)]
    pub port: Option<u16>,
    /// JMAP base URL (the session is found at `<url>/.well-known/jmap`).
    #[serde(default)]
    pub url: Option<String>,
    /// Path of the KEY=value credential file.
    pub credentials: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MailConfig {
    #[serde(default)]
    pub internal_domains: Vec<String>,
    #[serde(default, rename = "account")]
    pub accounts: Vec<Account>,
    /// Where it came from (file path or "discovered"), for `accounts`.
    #[serde(skip)]
    pub source: String,
}

/// A secret that cannot be printed by accident.
pub struct Secret(String);
impl Secret {
    pub(crate) fn new(s: String) -> Self {
        Secret(s)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug)]
pub struct Credentials {
    pub user: String,
    pub pass: Secret,
}

pub fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(p)
}

/// Parse a KEY=value env file (comments, blank lines, optional quotes).
pub fn read_env_file(path: &Path) -> Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| MailError::Config(format!("cannot read credential file {}: {e}", path.display())))?;
    Ok(text
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (k, v) = l.split_once('=')?;
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v);
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect())
}

fn env_get<'a>(kv: &'a [(String, String)], k: &str) -> Option<&'a str> {
    kv.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
}

impl Account {
    /// Read the credential file. The password is wrapped in [`Secret`].
    pub fn credentials(&self) -> Result<Credentials> {
        let kv = read_env_file(&expand_home(&self.credentials))?;
        let user = env_get(&kv, "KANNAKA_MAIL_USER")
            .ok_or_else(|| MailError::Config(format!("{}: KANNAKA_MAIL_USER missing in {}", self.name, self.credentials)))?;
        let pass = env_get(&kv, "KANNAKA_MAIL_PASS")
            .ok_or_else(|| MailError::Config(format!("{}: KANNAKA_MAIL_PASS missing in {}", self.name, self.credentials)))?;
        Ok(Credentials { user: user.to_string(), pass: Secret(pass.to_string()) })
    }
    /// One line for `mail accounts` — never a secret.
    pub fn describe(&self) -> String {
        let where_ = match self.transport {
            Transport::Imap => format!("imap {}:{}", self.host.as_deref().unwrap_or("?"), self.port.unwrap_or(993)),
            Transport::Jmap => format!("jmap {}", self.url.as_deref().unwrap_or("?")),
        };
        format!("{:<8} {:<28} {:<40} credentials: {}", self.name, self.address, where_, self.credentials)
    }
}

impl MailConfig {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("mail").join("accounts.toml")
    }

    pub fn load(data_dir: &Path) -> Result<MailConfig> {
        let p = Self::path(data_dir);
        if p.exists() {
            let text = std::fs::read_to_string(&p)?;
            let mut cfg: MailConfig = toml::from_str(&text)
                .map_err(|e| MailError::Config(format!("{}: {e}", p.display())))?;
            cfg.source = p.display().to_string();
            for a in &cfg.accounts {
                match a.transport {
                    Transport::Imap if a.host.is_none() => {
                        return Err(MailError::Config(format!("account {}: imap needs `host`", a.name)))
                    }
                    Transport::Jmap if a.url.is_none() => {
                        return Err(MailError::Config(format!("account {}: jmap needs `url`", a.name)))
                    }
                    _ => {}
                }
            }
            return Ok(cfg);
        }
        Ok(Self::discover())
    }

    /// Accounts from `~/.kannaka-mail.env` and `~/.kannaka-mail-<name>.env`.
    /// Reads only the non-secret keys (user, host, port).
    pub fn discover() -> MailConfig {
        let mut cfg = MailConfig { source: "discovered from ~/.kannaka-mail*.env".into(), ..Default::default() };
        let Some(home) = dirs::home_dir() else { return cfg };
        let mut files: Vec<(String, PathBuf)> = Vec::new();
        if home.join(".kannaka-mail.env").exists() {
            files.push(("primary".into(), home.join(".kannaka-mail.env")));
        }
        if let Ok(rd) = std::fs::read_dir(&home) {
            let mut extra: Vec<(String, PathBuf)> = rd
                .flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    let name = n.strip_prefix(".kannaka-mail-")?.strip_suffix(".env")?.to_string();
                    Some((name, e.path()))
                })
                .collect();
            extra.sort();
            files.extend(extra);
        }
        for (name, path) in files {
            let Ok(kv) = read_env_file(&path) else { continue };
            let Some(user) = env_get(&kv, "KANNAKA_MAIL_USER") else { continue };
            let host = env_get(&kv, "KANNAKA_MAIL_IMAP_HOST").unwrap_or(if name == "primary" { "imap.zoho.com" } else { "" });
            if host.is_empty() {
                continue;
            }
            cfg.accounts.push(Account {
                name,
                address: user.to_ascii_lowercase(),
                display_name: None,
                transport: Transport::Imap,
                host: Some(host.to_string()),
                port: env_get(&kv, "KANNAKA_MAIL_IMAP_PORT").and_then(|p| p.parse().ok()),
                url: None,
                credentials: format!("~/{}", path.file_name().unwrap().to_string_lossy()),
            });
        }
        cfg
    }

    pub fn policy(&self) -> super::rules::Policy {
        super::rules::Policy {
            self_addresses: self.accounts.iter().map(|a| a.address.to_ascii_lowercase()).collect(),
            internal_domains: self.internal_domains.clone(),
            display_names: self
                .accounts
                .iter()
                .filter_map(|a| a.display_name.clone().map(|d| (a.name.clone(), d)))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_formats() {
        let c = Credentials { user: "u@x".into(), pass: Secret("hunter2".into()) };
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("hunter2"), "{dbg}");
    }

    #[test]
    fn config_parses_both_transports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("mail")).unwrap();
        std::fs::write(
            MailConfig::path(dir.path()),
            "internal_domains = [\"ninja-portal.com\"]\n[[account]]\nname=\"zoho\"\naddress=\"k@s.love\"\ntransport=\"imap\"\nhost=\"imap.zoho.com\"\ncredentials=\"~/.k.env\"\n[[account]]\nname=\"np\"\naddress=\"k@np.com\"\ndisplay_name=\"Kannaka\"\ntransport=\"jmap\"\nurl=\"https://mail.np.com\"\ncredentials=\"~/.k2.env\"\n",
        )
        .unwrap();
        let c = MailConfig::load(dir.path()).unwrap();
        assert_eq!(c.accounts.len(), 2);
        assert_eq!(c.accounts[1].transport, Transport::Jmap);
        let p = c.policy();
        assert!(p.is_self("K@S.love"));
        assert_eq!(p.display_names.get("np").map(|s| s.as_str()), Some("Kannaka"));
        assert!(!c.accounts[0].describe().contains("PASS"));
    }

    #[test]
    fn env_file_quotes_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.env");
        std::fs::write(&p, "# c\nexport KANNAKA_MAIL_USER=\"a@b\"\nKANNAKA_MAIL_PASS='p=q'\n").unwrap();
        let kv = read_env_file(&p).unwrap();
        assert_eq!(env_get(&kv, "KANNAKA_MAIL_USER"), Some("a@b"));
        assert_eq!(env_get(&kv, "KANNAKA_MAIL_PASS"), Some("p=q"));
    }
}
