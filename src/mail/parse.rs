//! Minimal, dependency-free RFC 5322 / MIME reading for ADR-0064 P0.
//!
//! Only what `sync` needs to build a [`MailRef`](super::MailRef): header
//! unfolding, RFC 2047 encoded words, address lists, message-id lists, dates,
//! and the sender's *own* words of a body (quotes and forwarded blocks cut),
//! reduced to three surface features. The text itself is never returned to
//! the store: callers keep only [`OwnText`].

use base64::Engine as _;
use chrono::{DateTime, Utc};

use super::Addr;

/// Split a raw message into (header block, body) at the first blank line.
pub fn split_message(raw: &[u8]) -> (&[u8], &[u8]) {
    for i in 0..raw.len() {
        if raw[i..].starts_with(b"\r\n\r\n") {
            return (&raw[..i], &raw[i + 4..]);
        }
        if raw[i..].starts_with(b"\n\n") {
            return (&raw[..i], &raw[i + 2..]);
        }
    }
    (raw, &[])
}

/// Unfolded headers, in order, names as written.
pub fn parse_headers(block: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(block);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            out.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    out
}

/// First value of header `name` (case-insensitive).
pub fn header<'a>(hs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    hs.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn decode_charset(bytes: &[u8], charset: &str) -> String {
    let cs = charset.to_ascii_lowercase();
    if cs == "iso-8859-1" || cs == "latin1" || cs == "windows-1252" || cs == "us-ascii" {
        // Byte-per-char: exact for latin-1, close enough for cp1252's printables.
        bytes.iter().map(|&b| b as char).collect()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn decode_qp(input: &[u8], header_mode: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        if c == b'=' {
            // soft line break
            if input.get(i + 1) == Some(&b'\r') && input.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if input.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            if i + 2 < input.len() {
                let h = std::str::from_utf8(&input[i + 1..i + 3]).ok();
                if let Some(v) = h.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(c);
            i += 1;
        } else if header_mode && c == b'_' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn decode_b64(input: &[u8]) -> Vec<u8> {
    let clean: Vec<u8> = input.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(&clean)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&clean))
        .unwrap_or_default()
}

/// Decode RFC 2047 encoded words (`=?utf-8?B?...?=`, `=?iso-8859-1?Q?...?=`).
/// Whitespace between two adjacent encoded words is dropped, per the RFC.
pub fn decode_words(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let (before, after) = rest.split_at(start);
        let parsed = (|| {
            let body = &after[2..];
            let q1 = body.find('?')?;
            let charset = &body[..q1];
            let enc = body.get(q1 + 1..q1 + 2)?;
            if body.get(q1 + 2..q1 + 3)? != "?" {
                return None;
            }
            let text_start = q1 + 3;
            let end = body[text_start..].find("?=")? + text_start;
            let text = &body[text_start..end];
            let bytes = match enc {
                "B" | "b" => decode_b64(text.as_bytes()),
                "Q" | "q" => decode_qp(text.as_bytes(), true),
                _ => return None,
            };
            let charset = charset.split('*').next().unwrap_or(charset);
            Some((decode_charset(&bytes, charset), 2 + end + 2))
        })();
        match parsed {
            Some((decoded, consumed)) => {
                if !(last_was_word && before.trim().is_empty()) {
                    out.push_str(before);
                }
                out.push_str(&decoded);
                rest = &after[consumed..];
                last_was_word = true;
            }
            None => {
                out.push_str(before);
                out.push_str("=?");
                rest = &after[2..];
                last_was_word = false;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Split an address list on commas that are outside quotes and angle brackets.
fn split_addr_list(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let (mut in_q, mut depth) = (false, 0i32);
    let mut prev_bs = false;
    for ch in s.chars() {
        match ch {
            '"' if !prev_bs => in_q = !in_q,
            '<' if !in_q => depth += 1,
            '>' if !in_q => depth -= 1,
            ',' if !in_q && depth <= 0 => {
                parts.push(std::mem::take(&mut cur));
                prev_bs = false;
                continue;
            }
            _ => {}
        }
        prev_bs = ch == '\\' && !prev_bs;
        cur.push(ch);
    }
    parts.push(cur);
    parts.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

/// Parse `Name <a@b>, "Last, First" <c@d>, e@f` into addresses (emails lowercased).
pub fn parse_addrs(s: &str) -> Vec<Addr> {
    split_addr_list(s)
        .into_iter()
        .filter_map(|p| {
            let p = p.trim();
            let (name, email) = match (p.rfind('<'), p.rfind('>')) {
                (Some(a), Some(b)) if b > a => (p[..a].trim(), p[a + 1..b].trim()),
                _ => ("", p),
            };
            if !email.contains('@') {
                return None;
            }
            let name = decode_words(name.trim_matches('"').trim());
            Some(Addr {
                name: name.replace("\\\"", "\""),
                email: email.trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase(),
            })
        })
        .collect()
}

/// All `<...>` message ids in a header value, in order.
pub fn parse_ids(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(a) = rest.find('<') {
        match rest[a..].find('>') {
            Some(b) => {
                let id = rest[a..a + b + 1].trim().to_string();
                if id.len() > 2 && !id.contains(char::is_whitespace) {
                    out.push(id);
                }
                rest = &rest[a + b + 1..];
            }
            None => break,
        }
    }
    out
}

/// RFC 5322 date → UTC. Accepts `GMT`, `-0000` (read as UTC, never as local
/// time), trailing `(CDT)` comments and single-digit days.
pub fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    let mut t = s.trim().to_string();
    if let Some(i) = t.find('(') {
        t.truncate(i);
    }
    let t = t.trim();
    if let Ok(d) = DateTime::parse_from_rfc2822(t) {
        return Some(d.with_timezone(&Utc));
    }
    // Obsolete zone names chrono does not take, e.g. "UT", "Z".
    for (z, off) in [(" UT", " +0000"), (" Z", " +0000"), (" UTC", " +0000")] {
        if let Some(stripped) = t.strip_suffix(z.trim_start()) {
            let fixed = format!("{}{}", stripped.trim_end(), off);
            if let Ok(d) = DateTime::parse_from_rfc2822(&fixed) {
                return Some(d.with_timezone(&Utc));
            }
        }
    }
    None
}

/// IMAP INTERNALDATE, e.g. `17-Sep-2026 00:47:08 +0000`.
pub fn parse_internaldate(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_str(s.trim(), "%d-%b-%Y %H:%M:%S %z")
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Surface features of the sender's own words, the only body-derived facts a
/// MailRef keeps. Not content: a length, and two booleans.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OwnText {
    /// Characters of own text (quotes, forwarded blocks and URLs removed).
    pub chars: usize,
    /// The own text contains a `?` (URLs removed first, so query strings don't count).
    pub question: bool,
    /// The own text starts with "fyi".
    pub fyi: bool,
}

fn strip_urls(s: &str) -> String {
    s.split_inclusive(char::is_whitespace)
        .filter(|w| {
            let w = w.trim().trim_start_matches(['<', '(', '[', '"']);
            !(w.starts_with("http://") || w.starts_with("https://") || w.starts_with("mailto:"))
        })
        .collect()
}

pub(crate) fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let lower = s.to_ascii_lowercase();
    // Drop <style>/<script> blocks entirely.
    let mut skip_until: Option<usize> = None;
    for (i, ch) in s.char_indices() {
        if let Some(end) = skip_until {
            if i < end {
                continue;
            }
            skip_until = None;
        }
        if ch == '<' {
            for tag in ["<style", "<script"] {
                if lower[i..].starts_with(tag) {
                    let close = format!("</{}>", &tag[1..]);
                    skip_until = lower[i..].find(&close).map(|e| i + e + close.len());
                    if skip_until.is_none() {
                        return out;
                    }
                }
            }
            if skip_until.is_some() {
                continue;
            }
            in_tag = true;
            let rest = &lower[i..];
            if rest.starts_with("<br") || rest.starts_with("<p") || rest.starts_with("<div") {
                out.push('\n');
            }
            continue;
        }
        if ch == '>' && in_tag {
            in_tag = false;
            continue;
        }
        if !in_tag {
            out.push(ch);
        }
    }
    out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
        .replace("&quot;", "\"").replace("&#39;", "'")
}

/// Is `line` the start of quoted or forwarded material?
fn is_quote_boundary(line: &str, next: Option<&str>) -> bool {
    let l = line.trim();
    if l.starts_with("-----Original Message")
        || l.contains("Forwarded message")
        || (l.starts_with("---- On ") && l.ends_with("----"))
    {
        return true;
    }
    if l.starts_with("On ") && l.ends_with("wrote:") {
        return true;
    }
    // Gmail wraps long attributions: "On Tue, Sep 22, 2026 at 9:43 AM Nick <\nx@y> wrote:"
    if l.starts_with("On ") && next.map(|n| n.trim().ends_with("wrote:")).unwrap_or(false) {
        return true;
    }
    // Outlook: "From: X" followed by "Sent:" / "Date:"
    if l.starts_with("From:") {
        if let Some(n) = next {
            let n = n.trim_start();
            return n.starts_with("Sent:") || n.starts_with("Date:");
        }
    }
    false
}

/// The sender's own words of a decoded text body: quoted lines and anything
/// after a reply attribution or a forwarded block are cut. Used for live
/// display only; never stored.
pub fn own_text_string(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut own = String::new();
    for (i, line) in lines.iter().enumerate() {
        if is_quote_boundary(line, lines.get(i + 1).copied()) {
            break;
        }
        if line.trim_start().starts_with('>') {
            continue;
        }
        own.push_str(line);
        own.push('\n');
    }
    own.trim().to_string()
}

/// Reduce a decoded text body to the three stored features of its own words.
pub fn own_text_of(text: &str) -> OwnText {
    let own = strip_urls(&own_text_string(text));
    let trimmed = own.trim();
    OwnText {
        chars: trimmed.chars().count(),
        question: trimmed.contains('?'),
        fyi: trimmed.to_ascii_lowercase().starts_with("fyi"),
    }
}

fn param(value: &str, name: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        if k.trim().eq_ignore_ascii_case(name) {
            Some(v.trim().trim_matches('"').to_string())
        } else {
            None
        }
    })
}

/// Find the best text part of a MIME entity: the first `text/plain`, else the
/// first `text/html` (tags stripped). Returns the decoded text.
pub fn best_text(headers: &[(String, String)], body: &[u8]) -> Option<String> {
    fn walk(headers: &[(String, String)], body: &[u8], depth: usize) -> (Option<String>, Option<String>) {
        if depth > 8 {
            return (None, None);
        }
        let ctype = header(headers, "Content-Type").unwrap_or("text/plain");
        let mime = ctype.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        if mime.starts_with("multipart/") {
            let Some(boundary) = param(ctype, "boundary") else { return (None, None) };
            let delim = format!("--{boundary}");
            let text = body;
            let mut plain = None;
            let mut html = None;
            let mut parts: Vec<&[u8]> = Vec::new();
            let mut starts: Vec<usize> = Vec::new();
            let d = delim.as_bytes();
            let mut i = 0;
            while i + d.len() <= text.len() {
                if &text[i..i + d.len()] == d && (i == 0 || text[i - 1] == b'\n') {
                    starts.push(i);
                    i += d.len();
                } else {
                    i += 1;
                }
            }
            for w in starts.windows(2) {
                let seg = &text[w[0] + d.len()..w[1]];
                parts.push(seg);
            }
            for seg in parts {
                let seg = seg.strip_prefix(b"\r\n").or_else(|| seg.strip_prefix(b"\n")).unwrap_or(seg);
                let (h, b) = split_message(seg);
                let hs = parse_headers(h);
                let (p, ht) = walk(&hs, b, depth + 1);
                if plain.is_none() {
                    plain = p;
                }
                if html.is_none() {
                    html = ht;
                }
            }
            return (plain, html);
        }
        if header(headers, "Content-Disposition")
            .map(|d| d.to_ascii_lowercase().starts_with("attachment"))
            .unwrap_or(false)
        {
            return (None, None);
        }
        let cte = header(headers, "Content-Transfer-Encoding").unwrap_or("7bit").to_ascii_lowercase();
        let bytes = match cte.trim() {
            "base64" => decode_b64(body),
            "quoted-printable" => decode_qp(body, false),
            _ => body.to_vec(),
        };
        let charset = param(ctype, "charset").unwrap_or_else(|| "utf-8".into());
        let text = decode_charset(&bytes, &charset);
        match mime.as_str() {
            "text/plain" => (Some(text), None),
            "text/html" => (None, Some(strip_html(&text))),
            _ => (None, None),
        }
    }
    let (plain, html) = walk(headers, body, 0);
    plain.or(html)
}

/// blake3 of the body bytes as served by the authority (hex). Not comparable
/// across transports (IMAP hashes the raw body, JMAP its decoded text parts).
pub fn body_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minus_zero_zone_is_utc_not_local() {
        // Sent copies in Zoho carry "-0000"; a naive parse shifts them by the
        // local offset and reorders a thread (seen in the P0 survey).
        let d = parse_date("Thu, 24 Sep 2026 18:22:20 -0000").unwrap();
        assert_eq!(d.to_rfc3339(), "2026-09-24T18:22:20+00:00");
        let g = parse_date("Mon, 14 Sep 2026 14:00:45 GMT").unwrap();
        assert_eq!(g.to_rfc3339(), "2026-09-14T14:00:45+00:00");
        let c = parse_date("Wed, 9 Sep 2026 21:03:24 -0500 (CDT)").unwrap();
        assert_eq!(c.to_rfc3339(), "2026-09-10T02:03:24+00:00");
    }

    #[test]
    fn addresses_with_quoted_commas_and_encoded_names() {
        let a = parse_addrs("\"Flach, Nick\" <Nick@Example.com>, bare@x.org, =?utf-8?Q?Ren=C3=A9?= <r@y.z>");
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].name, "Flach, Nick");
        assert_eq!(a[0].email, "nick@example.com");
        assert_eq!(a[1].email, "bare@x.org");
        assert_eq!(a[2].name, "René");
    }

    #[test]
    fn ids_and_words() {
        assert_eq!(parse_ids("<a@b> <c@d>\r\n <e@f>"), vec!["<a@b>", "<c@d>", "<e@f>"]);
        assert_eq!(decode_words("=?UTF-8?B?SGVsbG8=?= =?UTF-8?B?IHdvcmxk?="), "Hello world");
    }

    #[test]
    fn own_text_cuts_quotes_and_forwards_and_ignores_url_queries() {
        let reply = "Test good\n\nOn Wed, Sep 9, 2026 at 9:02 PM Kannaka <k@x> wrote:\n> Did it arrive?\n";
        let o = own_text_of(reply);
        assert_eq!(o.chars, 9);
        assert!(!o.question, "the quoted question is not ours");

        let fwd = "Fyi.. \n\n============ Forwarded message ============\nFrom: V\nWhat do you think?\n";
        let f = own_text_of(fwd);
        assert!(f.fyi && !f.question);

        let link = "see https://example.com/a?utm_source=x&b=1 for more";
        assert!(!own_text_of(link).question);
        assert!(own_text_of("Which tables feed msgs_per_bot?").question);
    }

    #[test]
    fn multipart_prefers_plain_and_decodes_qp() {
        let raw = b"Content-Type: multipart/alternative; boundary=\"XX\"\r\n\r\n--XX\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nHi =E2=80=94 there?\r\n--XX\r\nContent-Type: text/html\r\n\r\n<p>Hi</p>\r\n--XX--\r\n";
        let (h, b) = split_message(raw);
        let hs = parse_headers(h);
        let t = best_text(&hs, b).unwrap();
        assert!(t.contains("Hi — there?"), "{t}");
    }
}
