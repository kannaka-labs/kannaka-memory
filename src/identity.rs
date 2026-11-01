//! Identity — spacechild-auth SSO credentials for swarm agents.
//!
//! Step 1 of giving Kannaka swarm agents real cryptographic identities:
//! a thin client for the spacechild-auth SSO service
//! (<https://auth.spacechild.love>) plus a token store at
//! `<data_dir>/identity.json`. Today the swarm identifies agents by bare
//! NATS agent_id strings; this module is the substrate for upgrading
//! those to verifiable identities without touching the HRM core or the
//! swarm/NATS internals.
//!
//! Contract (source of truth: spacechild-auth `src/routes.ts`):
//! - `POST /auth/register {email,password}` →
//!   `{success, user{id,email}, accessToken?, refreshToken?, requiresVerification?}`
//!   (tokens are absent when email verification is required first)
//! - `POST /auth/login {email,password}` → same shape; may instead return
//!   `{requiresMfa, partialToken, ...}` which this CLI does not support
//! - `POST /auth/refresh {refreshToken}` → `{accessToken, refreshToken}`
//! - `GET /auth/user` with `Authorization: Bearer` → `{id, email, ...}`
//!
//! Endpoint override: `KANNAKA_AUTH_URL` env > built-in default. Tokens
//! are never logged or printed — `whoami` surfaces user id/email/expiry
//! only, with expiry read from the access token's JWT `exp` claim.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default spacechild-auth base URL. Override with `KANNAKA_AUTH_URL`.
pub const DEFAULT_AUTH_URL: &str = "https://auth.spacechild.love";

/// Per-request timeout. Auth round-trips are small; anything slower than
/// this means the service is down and the operator should hear about it.
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Resolve the auth base URL: `KANNAKA_AUTH_URL` env > built-in default.
pub fn auth_base_url() -> String {
    resolve_auth_url(std::env::var("KANNAKA_AUTH_URL").ok())
}

/// Pure resolution logic for the auth base URL — split out from
/// `auth_base_url` so it's unit-testable without mutating process env
/// (env-var writes race across the parallel test harness).
pub fn resolve_auth_url(env_value: Option<String>) -> String {
    match env_value {
        Some(v) if !v.trim().is_empty() => v.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_AUTH_URL.to_string(),
    }
}

/// Path to the stored identity inside the data directory
/// (respects `KANNAKA_DATA_DIR` via `KannakaConfig::data_dir`).
pub fn identity_path() -> PathBuf {
    crate::config::KannakaConfig::data_dir().join("identity.json")
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// HTTP-level rejection from the auth service. `message` is the
    /// server's `error`/`message` field when parseable, else raw body.
    #[error("auth service error ({status}): {message}")]
    Status { status: u16, message: String },
    /// Transport failure — DNS, TLS, timeout, connection refused.
    #[error("auth service unreachable: {0}")]
    Transport(String),
    /// 2xx response whose body didn't match the documented contract.
    #[error("unexpected response from auth service: {0}")]
    Parse(String),
    /// A documented server behavior this CLI deliberately doesn't handle
    /// (e.g. MFA-gated login).
    #[error("{0}")]
    Unsupported(String),
}

// ---------------------------------------------------------------------------
// Token store
// ---------------------------------------------------------------------------

/// On-disk session: who we are plus the token pair. Written to
/// `identity.json` with owner-only permissions where supported. NEVER
/// print the token fields — `whoami` output is built from user_id/email
/// and the access token's decoded `exp` claim only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityStore {
    pub user_id: String,
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
    /// When this token pair was obtained (UTC). Informational; expiry
    /// authority is the JWT `exp` claim.
    pub obtained_at: chrono::DateTime<chrono::Utc>,
    /// Persistent voice for this identity (#985). Absent in legacy files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<crate::voice::VoiceSpec>,
}

impl IdentityStore {
    /// Load the stored identity. `Ok(None)` when no identity file exists
    /// (not logged in); `Err` only on read/parse failure of a present file.
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        let store: Self = serde_json::from_str(&text)
            .map_err(|e| format!("failed to parse {}: {}", path.display(), e))?;
        Ok(Some(store))
    }

    /// Persist to `path`, creating the parent directory if needed.
    /// On Unix, sets file permissions to 0600 (owner-only) — same token
    /// hygiene as `KannakaConfig::save` applies to config.toml.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {}", parent.display(), e))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize identity: {e}"))?;
        // Owner-only (0600) from creation — no world-readable window for the SSO
        // access/refresh tokens (was std::fs::write + discarded post-hoc chmod).
        crate::provenance::write_owner_only(path, text.as_bytes())
            .map_err(|e| format!("failed to write {}: {}", path.display(), e))?;

        Ok(())
    }

    /// Delete the stored identity. Returns `Ok(true)` when a file was
    /// removed, `Ok(false)` when there was nothing to remove.
    pub fn delete(path: &Path) -> Result<bool, String> {
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(path)
            .map_err(|e| format!("failed to remove {}: {}", path.display(), e))?;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// JWT expiry (display only — no signature verification)
// ---------------------------------------------------------------------------

/// Extract the `exp` claim (unix seconds) from a JWT access token so
/// `whoami` can show expiry without printing the token itself. This is a
/// display helper, NOT verification — the auth service is the authority.
pub fn jwt_exp_unix(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("exp")?.as_i64()
}

/// Minimal base64url (RFC 4648 §5, no padding) decoder. Std-only — not
/// worth a `base64` dependency for one 12-byte JWT payload read.
fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let bytes = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc: u32 = 0;
        for &c in chunk {
            acc = (acc << 6) | val(c)?;
        }
        match chunk.len() {
            4 => {
                out.push((acc >> 16) as u8);
                out.push((acc >> 8) as u8);
                out.push(acc as u8);
            }
            3 => {
                acc <<= 6;
                out.push((acc >> 16) as u8);
                out.push((acc >> 8) as u8);
            }
            2 => {
                acc <<= 12;
                out.push((acc >> 16) as u8);
            }
            _ => return None, // 1 trailing char is never valid base64
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Auth client
// ---------------------------------------------------------------------------

/// Outcome of `POST /auth/register`. Tokens are absent when the service
/// requires email verification before the first login.
#[derive(Debug, Clone)]
pub struct RegisterOutcome {
    pub user_id: String,
    pub email: String,
    /// `(access_token, refresh_token)` when the service logged us in
    /// directly on registration.
    pub tokens: Option<(String, String)>,
    pub requires_verification: bool,
    pub message: Option<String>,
}

/// Outcome of a successful `POST /auth/login`.
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    pub user_id: String,
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
}

/// Subset of `GET /auth/user` that `whoami` surfaces.
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub id: String,
    pub email: String,
    pub email_verified: Option<bool>,
    pub created_at: Option<String>,
}

/// Blocking JSON client for spacechild-auth, mirroring the `ureq`
/// patterns in `agent.rs` / `handlers/services.rs`.
pub struct AuthClient {
    base: String,
}

impl AuthClient {
    pub fn new(base: impl Into<String>) -> Self {
        let base = base.into();
        Self { base: base.trim_end_matches('/').to_string() }
    }

    /// Client against `KANNAKA_AUTH_URL` env > built-in default.
    pub fn from_env() -> Self {
        Self::new(auth_base_url())
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// `POST /auth/register {email, password}`.
    pub fn register(&self, email: &str, password: &str) -> Result<RegisterOutcome, IdentityError> {
        let v = self.post_json(
            "/auth/register",
            serde_json::json!({ "email": email, "password": password }),
        )?;
        let user_id = v["user"]["id"]
            .as_str()
            .ok_or_else(|| IdentityError::Parse("register response missing user.id".into()))?
            .to_string();
        let email = v["user"]["email"].as_str().unwrap_or(email).to_string();
        let tokens = match (v["accessToken"].as_str(), v["refreshToken"].as_str()) {
            (Some(a), Some(r)) => Some((a.to_string(), r.to_string())),
            _ => None,
        };
        Ok(RegisterOutcome {
            user_id,
            email,
            tokens,
            requires_verification: v["requiresVerification"].as_bool().unwrap_or(false),
            message: v["message"].as_str().map(ToString::to_string),
        })
    }

    /// `POST /auth/login {email, password}`. MFA-gated accounts are
    /// rejected with `Unsupported` — this CLI has no MFA flow yet.
    pub fn login(&self, email: &str, password: &str) -> Result<LoginOutcome, IdentityError> {
        let v = self.post_json(
            "/auth/login",
            serde_json::json!({ "email": email, "password": password }),
        )?;
        if v["requiresMfa"].as_bool().unwrap_or(false) {
            return Err(IdentityError::Unsupported(
                "this account requires MFA, which `kannaka identity` does not support yet".into(),
            ));
        }
        let access = v["accessToken"]
            .as_str()
            .ok_or_else(|| IdentityError::Parse("login response missing accessToken".into()))?
            .to_string();
        let refresh = v["refreshToken"]
            .as_str()
            .ok_or_else(|| IdentityError::Parse("login response missing refreshToken".into()))?
            .to_string();
        let user_id = v["user"]["id"]
            .as_str()
            .ok_or_else(|| IdentityError::Parse("login response missing user.id".into()))?
            .to_string();
        let email = v["user"]["email"].as_str().unwrap_or(email).to_string();
        Ok(LoginOutcome { user_id, email, access_token: access, refresh_token: refresh })
    }

    /// `POST /auth/refresh {refreshToken}` → new `(access, refresh)` pair.
    pub fn refresh(&self, refresh_token: &str) -> Result<(String, String), IdentityError> {
        let v = self.post_json(
            "/auth/refresh",
            serde_json::json!({ "refreshToken": refresh_token }),
        )?;
        let access = v["accessToken"]
            .as_str()
            .ok_or_else(|| IdentityError::Parse("refresh response missing accessToken".into()))?
            .to_string();
        let refresh = v["refreshToken"]
            .as_str()
            .ok_or_else(|| IdentityError::Parse("refresh response missing refreshToken".into()))?
            .to_string();
        Ok((access, refresh))
    }

    /// `GET /auth/user` with `Authorization: Bearer`. A 401 surfaces as
    /// `Status { status: 401, .. }` so callers can drive auto-refresh.
    pub fn user(&self, access_token: &str) -> Result<UserInfo, IdentityError> {
        let url = format!("{}/auth/user", self.base);
        let resp = ureq::get(&url)
            .set("Authorization", &format!("Bearer {access_token}"))
            .timeout(HTTP_TIMEOUT)
            .call();
        let v = Self::handle_response(resp)?;
        let id = v["id"]
            .as_str()
            .ok_or_else(|| IdentityError::Parse("user response missing id".into()))?
            .to_string();
        let email = v["email"].as_str().unwrap_or("?").to_string();
        Ok(UserInfo {
            id,
            email,
            email_verified: v["isEmailVerified"].as_bool(),
            created_at: v["createdAt"].as_str().map(ToString::to_string),
        })
    }

    fn post_json(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, IdentityError> {
        let url = format!("{}{}", self.base, path);
        let resp = ureq::post(&url)
            .set("Content-Type", "application/json")
            .timeout(HTTP_TIMEOUT)
            .send_json(body);
        Self::handle_response(resp)
    }

    /// Common ureq result → JSON mapping. The server reports failures as
    /// `{error: "..."}` (sometimes `{message: "..."}`) — extract that for
    /// the operator instead of dumping raw bodies.
    fn handle_response(
        resp: Result<ureq::Response, ureq::Error>,
    ) -> Result<serde_json::Value, IdentityError> {
        match resp {
            Ok(r) => r
                .into_json::<serde_json::Value>()
                .map_err(|e| IdentityError::Parse(e.to_string())),
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                let message = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v["error"]
                            .as_str()
                            .or_else(|| v["message"].as_str())
                            .map(ToString::to_string)
                    })
                    .unwrap_or_else(|| {
                        let mut b = body;
                        b.truncate(200);
                        b
                    });
                Err(IdentityError::Status { status: code, message })
            }
            Err(e) => Err(IdentityError::Transport(e.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only base64url encoder so the JWT tests build payloads instead
    // of relying on hand-computed strings.
    fn b64url_encode(data: &[u8]) -> String {
        const ALPHA: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let acc = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(ALPHA[(acc >> 18 & 63) as usize] as char);
            out.push(ALPHA[(acc >> 12 & 63) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHA[(acc >> 6 & 63) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(ALPHA[(acc & 63) as usize] as char);
            }
        }
        out
    }

    fn sample_store() -> IdentityStore {
        IdentityStore {
            user_id: "user-123".into(),
            email: "agent@spacechild.love".into(),
            access_token: "access.token.value".into(),
            refresh_token: "refresh-token-value".into(),
            obtained_at: chrono::Utc::now(),
            voice: None,
        }
    }

    // -- URL resolution --------------------------------------------------

    #[test]
    fn auth_url_defaults_when_unset() {
        assert_eq!(resolve_auth_url(None), DEFAULT_AUTH_URL);
    }

    #[test]
    fn auth_url_defaults_when_empty_or_blank() {
        assert_eq!(resolve_auth_url(Some(String::new())), DEFAULT_AUTH_URL);
        assert_eq!(resolve_auth_url(Some("   ".into())), DEFAULT_AUTH_URL);
    }

    #[test]
    fn auth_url_env_override_wins() {
        assert_eq!(
            resolve_auth_url(Some("http://localhost:5000".into())),
            "http://localhost:5000"
        );
    }

    #[test]
    fn auth_url_strips_trailing_slash() {
        assert_eq!(
            resolve_auth_url(Some("http://localhost:5000/".into())),
            "http://localhost:5000"
        );
    }

    #[test]
    fn client_base_trims_trailing_slash() {
        let c = AuthClient::new("https://auth.example.com/");
        assert_eq!(c.base_url(), "https://auth.example.com");
    }

    // -- Token store -----------------------------------------------------

    #[test]
    fn store_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        let store = sample_store();
        store.save(&path).unwrap();

        let loaded = IdentityStore::load(&path).unwrap().expect("stored identity");
        assert_eq!(loaded, store);
    }

    #[test]
    fn store_save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deeper").join("identity.json");
        sample_store().save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn store_load_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        assert_eq!(IdentityStore::load(&path).unwrap(), None);
    }

    #[test]
    fn store_load_malformed_is_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        std::fs::write(&path, "not json at all").unwrap();
        assert!(IdentityStore::load(&path).is_err());
    }

    #[test]
    fn store_delete_removes_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        sample_store().save(&path).unwrap();

        assert!(IdentityStore::delete(&path).unwrap());
        assert!(!path.exists());
        // Second delete: nothing there, not an error.
        assert!(!IdentityStore::delete(&path).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn store_save_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        sample_store().save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // -- JWT expiry ------------------------------------------------------

    #[test]
    fn jwt_exp_extracts_claim() {
        let header = b64url_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = b64url_encode(br#"{"sub":"user-123","exp":1893456000}"#);
        let token = format!("{header}.{payload}.fakesig");
        assert_eq!(jwt_exp_unix(&token), Some(1893456000));
    }

    #[test]
    fn jwt_exp_none_without_exp_claim() {
        let header = b64url_encode(br#"{"alg":"HS256"}"#);
        let payload = b64url_encode(br#"{"sub":"user-123"}"#);
        let token = format!("{header}.{payload}.fakesig");
        assert_eq!(jwt_exp_unix(&token), None);
    }

    #[test]
    fn jwt_exp_rejects_garbage() {
        assert_eq!(jwt_exp_unix("not-a-jwt"), None);
        assert_eq!(jwt_exp_unix(""), None);
        assert_eq!(jwt_exp_unix("a.!!!not-base64!!!.c"), None);
    }

    #[test]
    fn b64url_round_trips_all_remainders() {
        // Cover 3n, 3n+1, 3n+2 input lengths (4, 2, 3 output chars).
        for input in [&b"abc"[..], &b"abcd"[..], &b"abcde"[..], &b"a"[..]] {
            let enc = b64url_encode(input);
            assert_eq!(b64url_decode(&enc).as_deref(), Some(input), "input {input:?}");
        }
    }
}
