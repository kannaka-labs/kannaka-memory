//! Voice — a persistent, provider-pluggable identity attribute (#985).
//!
//! A voice is `(provider, voice_id, settings)`. It lives on the identity
//! record (`identity.json`), may be defaulted in `config.toml` (`[voice]`),
//! and resolves in the order identity > config > built-in default.
//!
//! Providers implement [`VoiceProvider`]. Built in: `noop` (accepts anything,
//! produces nothing) and `file` (writes the text to be spoken into a spool
//! directory, a stand-in for tests and for an external synthesizer to pick
//! up). The repo has no network TTS client today; a real provider is a new
//! `VoiceProvider` impl registered in [`provider_for`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_PROVIDER: &str = "noop";
pub const DEFAULT_VOICE_ID: &str = "default";
pub const KNOWN_PROVIDERS: &[&str] = &["noop", "file"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceSpec {
    pub provider: String,
    pub voice_id: String,
    /// Provider-specific knobs (speed, pitch, ...). Opaque to this module.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, String>,
}

impl VoiceSpec {
    pub fn new(provider: impl Into<String>, voice_id: impl Into<String>) -> Self {
        Self { provider: provider.into(), voice_id: voice_id.into(), settings: BTreeMap::new() }
    }
}

impl Default for VoiceSpec {
    fn default() -> Self {
        Self::new(DEFAULT_PROVIDER, DEFAULT_VOICE_ID)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VoiceError {
    #[error("unknown voice provider '{0}' (known: {})", KNOWN_PROVIDERS.join(", "))]
    UnknownProvider(String),
    #[error("invalid voice: {0}")]
    Invalid(String),
    #[error("voice io error: {0}")]
    Io(String),
}

pub trait VoiceProvider {
    fn name(&self) -> &'static str;
    /// Check a spec is usable by this provider.
    fn validate(&self, spec: &VoiceSpec) -> Result<(), VoiceError>;
    /// Speak `text`. Returns where output went, if anywhere.
    fn synthesize(&self, spec: &VoiceSpec, text: &str) -> Result<Option<PathBuf>, VoiceError>;
}

pub struct NoopProvider;

impl VoiceProvider for NoopProvider {
    fn name(&self) -> &'static str {
        "noop"
    }
    fn validate(&self, _: &VoiceSpec) -> Result<(), VoiceError> {
        Ok(())
    }
    fn synthesize(&self, _: &VoiceSpec, _: &str) -> Result<Option<PathBuf>, VoiceError> {
        Ok(None)
    }
}

/// Writes `<dir>/<voice_id>-<n>.txt` containing the text.
pub struct FileProvider {
    dir: PathBuf,
}

impl FileProvider {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl VoiceProvider for FileProvider {
    fn name(&self) -> &'static str {
        "file"
    }
    fn validate(&self, spec: &VoiceSpec) -> Result<(), VoiceError> {
        let id = &spec.voice_id;
        let ok_chars = id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if id.is_empty() || !ok_chars || id.starts_with('.') {
            return Err(VoiceError::Invalid(format!(
                "file provider voice_id '{}' must be [A-Za-z0-9._-] and not start with '.'",
                id
            )));
        }
        Ok(())
    }
    fn synthesize(&self, spec: &VoiceSpec, text: &str) -> Result<Option<PathBuf>, VoiceError> {
        self.validate(spec)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| VoiceError::Io(e.to_string()))?;
        // create_new, not exists()-then-write: two writers can never pick
        // the same name and overwrite each other.
        let mut n = 0u32;
        loop {
            let p = self.dir.join(format!("{}-{}.txt", spec.voice_id, n));
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(text.as_bytes()).map_err(|e| VoiceError::Io(e.to_string()))?;
                    return Ok(Some(p));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(VoiceError::Io(e.to_string())),
            }
        }
    }
}

/// Look up a provider by name. `spool_dir` is used by the `file` provider.
pub fn provider_for(name: &str, spool_dir: &Path) -> Result<Box<dyn VoiceProvider>, VoiceError> {
    match name {
        "noop" => Ok(Box::new(NoopProvider)),
        "file" => Ok(Box::new(FileProvider::new(spool_dir))),
        other => Err(VoiceError::UnknownProvider(other.to_string())),
    }
}

/// Validate a spec against its provider (unknown provider is an error).
pub fn validate_spec(spec: &VoiceSpec, spool_dir: &Path) -> Result<(), VoiceError> {
    provider_for(&spec.provider, spool_dir)?.validate(spec)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceSource {
    Identity,
    Config,
    Default,
}

impl VoiceSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Config => "config",
            Self::Default => "default",
        }
    }
}

/// identity > config > default.
pub fn resolve(identity: Option<&VoiceSpec>, config: Option<&VoiceSpec>) -> (VoiceSpec, VoiceSource) {
    if let Some(v) = identity {
        (v.clone(), VoiceSource::Identity)
    } else if let Some(v) = config {
        (v.clone(), VoiceSource::Config)
    } else {
        (VoiceSpec::default(), VoiceSource::Default)
    }
}

/// Default spool directory for the `file` provider.
pub fn spool_dir() -> PathBuf {
    crate::config::KannakaConfig::data_dir().join("voice_spool")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::IdentityStore;

    fn store(voice: Option<VoiceSpec>) -> IdentityStore {
        IdentityStore {
            user_id: "u".into(),
            email: "e@x".into(),
            access_token: "a".into(),
            refresh_token: "r".into(),
            obtained_at: chrono::Utc::now(),
            voice,
        }
    }

    #[test]
    fn voice_persists_with_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        let mut spec = VoiceSpec::new("file", "aria");
        spec.settings.insert("speed".into(), "1.2".into());
        store(Some(spec.clone())).save(&path).unwrap();
        let loaded = IdentityStore::load(&path).unwrap().unwrap();
        assert_eq!(loaded.voice, Some(spec));
    }

    #[test]
    fn legacy_identity_without_voice_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        // A file written before #985: no `voice` key at all.
        std::fs::write(
            &path,
            r#"{"user_id":"u","email":"e@x","access_token":"a","refresh_token":"r","obtained_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(IdentityStore::load(&path).unwrap().unwrap().voice, None);
        // And saving without a voice does not add the key.
        store(None).save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("voice"));
    }

    #[test]
    fn resolution_order_identity_config_default() {
        let id = VoiceSpec::new("file", "from-identity");
        let cfg = VoiceSpec::new("noop", "from-config");
        assert_eq!(resolve(Some(&id), Some(&cfg)), (id.clone(), VoiceSource::Identity));
        assert_eq!(resolve(None, Some(&cfg)), (cfg.clone(), VoiceSource::Config));
        assert_eq!(resolve(None, None), (VoiceSpec::default(), VoiceSource::Default));
    }

    #[test]
    fn config_voice_parses_from_toml() {
        let src = "[voice]\nprovider = \"file\"\nvoice_id = \"c\"\n[voice.settings]\npitch = \"2\"\n";
        let cfg: crate::config::KannakaConfig = toml::from_str(src).unwrap();
        let v = cfg.voice.unwrap();
        assert_eq!((v.provider.as_str(), v.settings["pitch"].as_str()), ("file", "2"));
        assert!(crate::config::KannakaConfig::default().voice.is_none());
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = validate_spec(&VoiceSpec::new("bogus", "x"), dir.path()).unwrap_err();
        assert_eq!(err, VoiceError::UnknownProvider("bogus".into()));
        assert!(provider_for("bogus", dir.path()).is_err());
    }

    #[test]
    fn providers_work() {
        let dir = tempfile::tempdir().unwrap();
        let noop = provider_for("noop", dir.path()).unwrap();
        assert_eq!(noop.synthesize(&VoiceSpec::default(), "hi").unwrap(), None);
        let file = provider_for("file", dir.path()).unwrap();
        let spec = VoiceSpec::new("file", "aria");
        let p = file.synthesize(&spec, "hello").unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello");
        let p2 = file.synthesize(&spec, "again").unwrap().unwrap();
        assert_ne!(p, p2);
        assert!(validate_spec(&VoiceSpec::new("file", "../evil"), dir.path()).is_err());
    }
}
