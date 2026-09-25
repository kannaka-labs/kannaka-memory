//! The mail sidecar: `<data_dir>/mail/refs.jsonl` (MailRefs, rewritten
//! atomically by each sync) and `<data_dir>/mail/closures.jsonl` (append-only
//! `mail close` records). Not the HRM: nothing here is a wave (ADR-0063).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{MailRef, Result};

/// "This open loop was resolved elsewhere", with the note as provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Closure {
    pub thread_id: String,
    pub thread_key: String,
    pub account: String,
    pub note: String,
    pub closed_at: DateTime<Utc>,
    /// The thread's newest person-facing message when it was closed; a newer
    /// message re-opens the thread.
    pub covers_through: Option<DateTime<Utc>>,
}

pub struct MailStore {
    dir: PathBuf,
}

impl MailStore {
    pub fn new(data_dir: &Path) -> Self {
        MailStore { dir: data_dir.join("mail") }
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn refs_path(&self) -> PathBuf {
        self.dir.join("refs.jsonl")
    }
    pub fn closures_path(&self) -> PathBuf {
        self.dir.join("closures.jsonl")
    }

    fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(v) => out.push(v),
                // Loud, not silent: a bad row is reported, never dropped quietly.
                Err(e) => eprintln!("mail: skipping unreadable row {} of {}: {e}", n + 1, path.display()),
            }
        }
        Ok(out)
    }

    pub fn load_refs(&self) -> Result<Vec<MailRef>> {
        Self::read_jsonl(&self.refs_path())
    }

    /// Replace the refs file atomically (write a temp file, then rename).
    pub fn save_refs(&self, refs: &[MailRef]) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("refs.jsonl.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            for r in refs {
                let line = serde_json::to_string(r).map_err(|e| std::io::Error::other(e.to_string()))?;
                f.write_all(line.as_bytes())?;
                f.write_all(b"\n")?;
            }
            f.sync_all()?;
        }
        fs::rename(&tmp, self.refs_path())?;
        Ok(())
    }

    pub fn load_closures(&self) -> Result<Vec<Closure>> {
        Self::read_jsonl(&self.closures_path())
    }

    pub fn append_closure(&self, c: &Closure) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut f = fs::OpenOptions::new().create(true).append(true).open(self.closures_path())?;
        let line = serde_json::to_string(c).map_err(|e| std::io::Error::other(e.to_string()))?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closures_append_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let s = MailStore::new(dir.path());
        assert!(s.load_closures().unwrap().is_empty());
        let c = Closure {
            thread_id: "t:abc".into(),
            thread_key: "<a@b>".into(),
            account: "zoho".into(),
            note: "deployed elsewhere".into(),
            closed_at: Utc::now(),
            covers_through: None,
        };
        s.append_closure(&c).unwrap();
        s.append_closure(&c).unwrap();
        assert_eq!(s.load_closures().unwrap().len(), 2);
        assert!(s.load_refs().unwrap().is_empty());
        s.save_refs(&[]).unwrap();
        assert!(s.refs_path().exists());
    }
}
