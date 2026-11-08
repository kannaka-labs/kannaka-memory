//! #1158 — who wrote what: a read-only count of the store by what it records
//! about each memory's origin.
//!
//! The store does not persist a per-row author. `HyperMemory::origin_agent`
//! exists in RAM, but `WavefrontMeta` has no field for it and every row loaded
//! from disk comes back as `"local"`, so counting by it would report 100%
//! "local" and say nothing. What IS stored, and what this counts:
//!
//! * a provenance prefix at the start of the content, written by the radio
//!   (`server/voice-writeback.js`: `[Provenance: said publicly under my name
//!   on ...`; `server/on-air-digest.js`: `[Provenance: said on air on Kannaka
//!   Radio ...`), or any other `[Provenance:` label;
//! * the `hallucinated` flag a dream sets on the rows it makes (only when
//!   the row was inserted with it set: `KannakaMemorySystem::hallucinate`
//!   sets it on the cache after `absorb`, which a reload loses, so
//!   `dream_made` can undercount until that is fixed);
//! * nothing at all (`unattributed`).
//!
//! The buckets are disjoint and add up to `total`, with precedence
//! `said_publicly > said_on_air > provenance_other > dream_made >
//! unattributed`. ADR-0049 facets and `__consolidation*` bookkeeping rows are
//! internal atoms, not memories anyone wrote, and are counted separately,
//! outside `total`.
//!
//! [`from_data_dir`] opens the store READ-ONLY and builds no memory system, so
//! it writes nothing: no `.hrm` save, no status cache, no encoder stamp, no
//! NATS. (`HrmStore::load` does sweep `.kannaka-tmp-*` litter older than an
//! hour, as every open of the store does.)

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::store::MediumBackend;

/// `server/voice-writeback.js` in kannaka-radio.
pub const PREFIX_SAID_PUBLICLY: &str = "[Provenance: said publicly under my name";
/// `server/on-air-digest.js` in kannaka-radio.
pub const PREFIX_SAID_ON_AIR: &str = "[Provenance: said on air";
/// Any other provenance label.
pub const PREFIX_PROVENANCE: &str = "[Provenance:";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProvenanceCounts {
    pub said_publicly: usize,
    pub said_on_air: usize,
    pub other: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttributionStats {
    /// Memories counted (facets and bookkeeping excluded).
    pub total: usize,
    pub provenance: ProvenanceCounts,
    /// Rows a dream made (`hallucinated`) with no provenance label.
    pub dream_made: usize,
    /// Rows that record nothing about where they came from.
    pub unattributed: usize,
    /// ADR-0049 facet rows (outside `total`).
    pub facets: usize,
    /// `__consolidation*` rows (outside `total`).
    pub bookkeeping: usize,
    /// `total`, split by modality.
    pub by_modality: BTreeMap<String, usize>,
    /// Always false today: the per-row author is not saved to disk.
    pub origin_agent_persisted: bool,
    pub note: &'static str,
}

const NOTE: &str = "The store does not save which agent or person wrote a row \
(origin_agent is not persisted; every row loaded from disk reads \"local\"). \
These buckets count only what each row itself records: a provenance prefix, \
or the dream flag. Rows made through the hallucinate() API set the dream flag \
only after insert, so it is not saved and they count as unattributed: dream_made \
is an undercount.";

/// Which bucket a memory falls in. `None` for facets/bookkeeping is decided by
/// the caller; this only reads content and the dream flag.
fn bucket(content: &str, hallucinated: bool) -> &'static str {
    let c = content.trim_start();
    if c.starts_with(PREFIX_SAID_PUBLICLY) {
        "said_publicly"
    } else if c.starts_with(PREFIX_SAID_ON_AIR) {
        "said_on_air"
    } else if c.starts_with(PREFIX_PROVENANCE) {
        "provenance_other"
    } else if hallucinated {
        "dream_made"
    } else {
        "unattributed"
    }
}

/// Count `store`. One O(n) pass; reads only.
pub fn count(store: &dyn MediumBackend) -> AttributionStats {
    let facets = store.facet_row_ids();
    let mut s = AttributionStats {
        total: 0,
        provenance: ProvenanceCounts::default(),
        dream_made: 0,
        unattributed: 0,
        facets: 0,
        bookkeeping: 0,
        by_modality: BTreeMap::new(),
        origin_agent_persisted: false,
        note: NOTE,
    };
    for m in store.all_memories().unwrap_or_default() {
        if facets.contains(&m.id) {
            s.facets += 1;
            continue;
        }
        if m.content.starts_with("__consolidation") {
            s.bookkeeping += 1;
            continue;
        }
        s.total += 1;
        match bucket(&m.content, m.hallucinated) {
            "said_publicly" => s.provenance.said_publicly += 1,
            "said_on_air" => s.provenance.said_on_air += 1,
            "provenance_other" => s.provenance.other += 1,
            "dream_made" => s.dream_made += 1,
            _ => s.unattributed += 1,
        }
        *s.by_modality.entry(m.modality.to_string()).or_default() += 1;
    }
    let sum = s.provenance.said_publicly
        + s.provenance.said_on_air
        + s.provenance.other
        + s.dream_made
        + s.unattributed;
    assert_eq!(sum, s.total, "attribution buckets must partition the total");
    s
}

/// Open the store at `hrm_path` read-only and count it. A missing file is an
/// empty store, not created.
pub fn from_hrm_path(hrm_path: &Path) -> Result<AttributionStats, crate::store::StoreError> {
    if !hrm_path.exists() {
        let empty = crate::store::TestMedium::new();
        return Ok(count(&empty));
    }
    let mut store =
        crate::hrm_store::HrmStore::load(crate::openclaw::make_pipeline(), hrm_path.to_path_buf())?;
    store.set_readonly(true);
    Ok(count(&store))
}

/// [`from_hrm_path`] on `<data_dir>/kannaka.hrm`.
pub fn from_data_dir(data_dir: &Path) -> Result<AttributionStats, crate::store::StoreError> {
    from_hrm_path(&data_dir.join("kannaka.hrm"))
}

impl AttributionStats {
    /// Human-readable lines for `kannaka stats --attribution`.
    pub fn render(&self) -> String {
        let pct = |n: usize| {
            if self.total == 0 {
                0.0
            } else {
                100.0 * n as f64 / self.total as f64
            }
        };
        let mut out = String::new();
        out.push_str(&format!("Attribution ({} memories):\n", self.total));
        for (label, n) in [
            ("said publicly under her name", self.provenance.said_publicly),
            ("said on air", self.provenance.said_on_air),
            ("other provenance label", self.provenance.other),
            ("made by a dream", self.dream_made),
            ("unattributed", self.unattributed),
        ] {
            out.push_str(&format!("  {label:<30} {n:>7}  ({:.1}%)\n", pct(n)));
        }
        out.push_str(&format!(
            "  (not counted: {} facet row(s), {} bookkeeping row(s))\n",
            self.facets, self.bookkeeping
        ));
        out.push_str(&format!("Note: {}\n", self.note));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_and_trimming() {
        assert_eq!(bucket("  [Provenance: said publicly under my name on Colony] hi", false), "said_publicly");
        assert_eq!(bucket("[Provenance: said on air on Kannaka Radio] hi", true), "said_on_air");
        assert_eq!(bucket("[Provenance: imported] x", true), "provenance_other");
        assert_eq!(bucket("a dream", true), "dream_made");
        assert_eq!(bucket("plain", false), "unattributed");
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kannaka_attrib_{name}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Every file in `dir`, recursively, as (relative path, bytes, mtime).
    fn fingerprint(dir: &Path) -> Vec<(String, Vec<u8>, Option<std::time::SystemTime>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let e = e.unwrap();
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(dir).unwrap().to_string_lossy().to_string();
                    let mtime = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
                    out.push((rel, std::fs::read(&p).unwrap(), mtime));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Exact counts on a real store, and the count changes no file in the
    /// data dir (bytes or mtime), sidecars included.
    #[test]
    fn counts_a_real_store_and_writes_nothing() {
        let dir = temp_dir("real");
        {
            let mut sys = crate::openclaw::KannakaMemorySystem::init(dir.clone()).unwrap();
            sys.set_agent_id(String::new()); // never publish from a test store
            sys.set_nats_url("nats://127.0.0.1:1".into());
            sys.remember("[Provenance: said publicly under my name on The Colony, 2026-11-08] I will bring this to Nick").unwrap();
            sys.remember("  [Provenance: said publicly under my name on Moltbook] second").unwrap();
            sys.remember("[Provenance: said on air on Kannaka Radio, 2026-11-08] tonight's set").unwrap();
            sys.remember("[Provenance: imported from an old notebook] a note").unwrap();
            sys.remember("the heron stands in the shallows").unwrap();
            sys.remember("a second plain memory about tides").unwrap();
            // A dream row inserted with the flag set, the way the dream's
            // own stages insert them. (`hallucinate()` sets the flag after
            // `absorb` on the cache only, so it does not survive a reload;
            // that is a separate bug, filed as a follow-up to #1158.)
            let mut dream = crate::memory::HyperMemory::new(
                vec![0.5; crate::medium::WAVEFRONT_DIM],
                "a dreamt bridge between tides and herons".to_string(),
            );
            dream.hallucinated = true;
            let h = sys.engine.store.insert(dream).unwrap();
            assert!(sys.engine.store.get(&h).unwrap().unwrap().hallucinated);
            sys.save().unwrap();
        }
        let before = fingerprint(&dir);
        // Wait past filesystem mtime granularity so a rewrite would show.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let s = from_data_dir(&dir).unwrap();
        let after = fingerprint(&dir);
        assert_eq!(s.provenance.said_publicly, 2, "{s:?}");
        assert_eq!(s.provenance.said_on_air, 1, "{s:?}");
        assert_eq!(s.provenance.other, 1, "{s:?}");
        assert_eq!(s.dream_made, 1, "{s:?}");
        assert_eq!(s.unattributed, s.total - 5, "{s:?}");
        assert!(s.unattributed >= 2, "{s:?}");
        assert!(!s.origin_agent_persisted);
        assert_eq!(s.by_modality.values().sum::<usize>(), s.total);
        assert_eq!(
            before.iter().map(|f| &f.0).collect::<Vec<_>>(),
            after.iter().map(|f| &f.0).collect::<Vec<_>>(),
            "the count created or removed a file"
        );
        for (b, a) in before.iter().zip(after.iter()) {
            assert!(b.1 == a.1, "{} changed bytes", b.0);
            assert_eq!(b.2, a.2, "{} changed mtime", b.0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_store_is_empty_and_not_created() {
        let dir = temp_dir("missing");
        let s = from_data_dir(&dir).unwrap();
        assert_eq!(s.total, 0);
        assert!(!dir.join("kannaka.hrm").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
