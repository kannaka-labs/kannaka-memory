//! A minimal, strictly read-only JMAP (RFC 8620/8621) client for Stalwart.
//!
//! Only `Mailbox/get`, `Email/query`, `Email/get` and `Thread/get` can be
//! called; [`guard_method`] refuses anything else (`Email/set`,
//! `EmailSubmission/set`, `Mailbox/set`, …) before a request is built.
//! Auth is HTTP Basic with the account's own credential; the header value is
//! held in a [`Secret`] and redirects are followed by hand, same host only, so
//! it is never sent anywhere else.

use base64::Engine as _;
use serde_json::{json, Value};

use super::config::{Credentials, Secret};
use super::{MailError, Result};

pub const ALLOWED_METHODS: &[&str] = &["Mailbox/get", "Email/query", "Email/get", "Thread/get"];

pub fn guard_method(name: &str) -> Result<()> {
    if ALLOWED_METHODS.contains(&name) {
        Ok(())
    } else {
        Err(MailError::ReadOnly(format!("JMAP method `{name}` is not allowed in read-only P0")))
    }
}

pub struct JmapSession {
    agent: ureq::Agent,
    auth: Secret,
    api_url: String,
    pub account_id: String,
}

fn host_of(url: &str) -> &str {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("")
}

impl JmapSession {
    pub fn connect(base_url: &str, c: &Credentials) -> Result<Self> {
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(std::time::Duration::from_secs(60))
            .build();
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", c.user, c.pass.expose()));
        let auth = Secret::new(format!("Basic {token}"));
        let base = base_url.trim_end_matches('/');
        let mut url = format!("{base}/.well-known/jmap");
        let mut session: Option<Value> = None;
        for _ in 0..4 {
            let resp = agent.get(&url).set("Authorization", auth.expose()).call();
            match resp {
                Ok(r) if (300..400).contains(&r.status()) => {
                    let loc = r.header("Location").unwrap_or("").to_string();
                    let next = if loc.starts_with('/') { format!("{base}{loc}") } else { loc };
                    if host_of(&next) != host_of(base) {
                        return Err(MailError::Jmap(format!("refusing cross-host redirect to {}", host_of(&next))));
                    }
                    url = next;
                }
                Ok(r) => {
                    session = Some(r.into_json().map_err(|e| MailError::Jmap(format!("session json: {e}")))?);
                    break;
                }
                Err(ureq::Error::Status(code, _)) => {
                    return Err(MailError::Jmap(format!("session: HTTP {code} from {}", host_of(&url))))
                }
                Err(e) => return Err(MailError::Jmap(format!("session: {}", e.kind()))),
            }
        }
        let s = session.ok_or_else(|| MailError::Jmap("session: too many redirects".into()))?;
        let api_url = s["apiUrl"].as_str().ok_or_else(|| MailError::Jmap("session has no apiUrl".into()))?.to_string();
        if host_of(&api_url) != host_of(base) {
            return Err(MailError::Jmap(format!("apiUrl is on another host ({})", host_of(&api_url))));
        }
        let account_id = s["primaryAccounts"]["urn:ietf:params:jmap:mail"]
            .as_str()
            .ok_or_else(|| MailError::Jmap("session has no mail account".into()))?
            .to_string();
        Ok(JmapSession { agent, auth, api_url, account_id })
    }

    /// One request with the given method calls; returns their responses in order.
    pub fn call(&self, calls: Vec<(&str, Value)>) -> Result<Vec<Value>> {
        for (m, _) in &calls {
            guard_method(m)?;
        }
        let method_calls: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, (m, args))| json!([m, args, format!("c{i}")]))
            .collect();
        let body = json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
            "methodCalls": method_calls,
        });
        let resp = self
            .agent
            .post(&self.api_url)
            .set("Authorization", self.auth.expose())
            .send_json(body)
            .map_err(|e| match e {
                ureq::Error::Status(code, _) => MailError::Jmap(format!("HTTP {code}")),
                other => MailError::Jmap(other.kind().to_string()),
            })?;
        let v: Value = resp.into_json().map_err(|e| MailError::Jmap(format!("response json: {e}")))?;
        let arr = v["methodResponses"].as_array().cloned().unwrap_or_default();
        let mut out = Vec::new();
        for r in arr {
            if r[0] == "error" {
                return Err(MailError::Jmap(format!("method error: {}", r[1])));
            }
            out.push(r[1].clone());
        }
        Ok(out)
    }
}

/// JMAP ids lack angle brackets; MailRefs keep the RFC 5322 form.
pub fn bracket(id: &str) -> String {
    if id.starts_with('<') {
        id.to_string()
    } else {
        format!("<{id}>")
    }
}

/// JMAP keywords → IMAP flag spelling.
pub fn keyword_to_flag(k: &str) -> String {
    match k {
        "$seen" => "\\Seen".into(),
        "$answered" => "\\Answered".into(),
        "$flagged" => "\\Flagged".into(),
        "$draft" => "\\Draft".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_methods_pass() {
        for ok in ALLOWED_METHODS {
            assert!(guard_method(ok).is_ok());
        }
        for bad in ["Email/set", "EmailSubmission/set", "Mailbox/set", "Email/import", "Email/copy", "Identity/set", "x:Bootstrap/set"] {
            assert!(guard_method(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids_and_keywords() {
        assert_eq!(bracket("a@b"), "<a@b>");
        assert_eq!(bracket("<a@b>"), "<a@b>");
        assert_eq!(keyword_to_flag("$answered"), "\\Answered");
    }

    #[test]
    fn hosts() {
        assert_eq!(host_of("https://mail.ninja-portal.com/jmap/"), "mail.ninja-portal.com");
    }
}
