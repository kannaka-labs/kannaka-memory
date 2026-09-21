//! Chiral HRM v2 persistence — save/load ChiralMedium to/from .hrm files.
//!
//! Format v2 wraps two hemispheres + callosum + scales into one file.
//! Backward compatible: detects v1 magic and loads as right-hemisphere-only.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use ndarray::{Array1, Array2};

use super::callosum::CorpusCallosum;
use super::chiral::ChiralMedium;
use super::fano::FanoPlane;
use super::hemisphere::Hemisphere;
use super::types::*;
use super::Medium;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

/// Pre-NCS WavefrontMeta (before modality field was added).
/// Used for backward-compatible deserialization of old .hrm files.
#[derive(Deserialize)]
pub(crate) struct WavefrontMetaLegacy {
    pub id: Uuid,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub hallucinated: bool,
    pub is_self_referential: bool,
}

impl From<WavefrontMetaLegacy> for WavefrontMeta {
    fn from(legacy: WavefrontMetaLegacy) -> Self {
        WavefrontMeta {
            id: legacy.id,
            content: legacy.content,
            tags: legacy.tags,
            created_at: legacy.created_at,
            hallucinated: legacy.hallucinated,
            is_self_referential: legacy.is_self_referential,
            sga_class: None,
            fano_group: None,
            category: None,
            modality: Modality::Unknown,
            tier: crate::medium::types::Tier::default(),
            effective_at: None,
            observed_at: None,
            expires_at: None,
            provenance: None,
            parent_id: None,
            is_facet: false,
            decomposed: false,
        }
    }
}

/// Pre-tier WavefrontMeta (after modality was added, before ADR-0031 tier).
/// Matches the on-disk layout of .hrm files written between the modality
/// addition and the tier addition. Used as the middle step in the
/// new → pre-tier → legacy deserialize fallback.
#[derive(Deserialize)]
pub(crate) struct WavefrontMetaPreTier {
    pub id: Uuid,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub hallucinated: bool,
    pub is_self_referential: bool,
    #[serde(default)]
    pub modality: Modality,
}

impl From<WavefrontMetaPreTier> for WavefrontMeta {
    fn from(p: WavefrontMetaPreTier) -> Self {
        WavefrontMeta {
            id: p.id,
            content: p.content,
            tags: p.tags,
            created_at: p.created_at,
            hallucinated: p.hallucinated,
            is_self_referential: p.is_self_referential,
            sga_class: None,
            fano_group: None,
            category: None,
            modality: p.modality,
            tier: crate::medium::types::Tier::default(),
            effective_at: None,
            observed_at: None,
            expires_at: None,
            provenance: None,
            parent_id: None,
            is_facet: false,
            decomposed: false,
        }
    }
}

/// Pre-temporal WavefrontMeta (after ADR-0031 tier, before Wave 3 Task 3.2b
/// temporal-truth fields). Matches the on-disk layout of every `.hrm` written
/// between the tier addition and the temporal addition — the newest legacy
/// shape. Used as the first fallback step in the
/// new → pre-temporal → pre-tier → legacy deserialize chain.
#[derive(Deserialize)]
pub(crate) struct WavefrontMetaPreTemporal {
    pub id: Uuid,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub hallucinated: bool,
    pub is_self_referential: bool,
    #[serde(default)]
    pub modality: Modality,
    #[serde(default)]
    pub tier: crate::medium::types::Tier,
}

impl From<WavefrontMetaPreTemporal> for WavefrontMeta {
    fn from(p: WavefrontMetaPreTemporal) -> Self {
        WavefrontMeta {
            id: p.id,
            content: p.content,
            tags: p.tags,
            created_at: p.created_at,
            hallucinated: p.hallucinated,
            is_self_referential: p.is_self_referential,
            sga_class: None,
            fano_group: None,
            category: None,
            modality: p.modality,
            tier: p.tier,
            effective_at: None,
            observed_at: None,
            expires_at: None,
            provenance: None,
            parent_id: None,
            is_facet: false,
            decomposed: false,
        }
    }
}

/// Pre-provenance WavefrontMeta (after Wave 3 temporal fields, before Quantum-
/// Wave T1.4 `provenance`). Matches the on-disk layout of every `.hrm` written
/// between the temporal addition and the provenance addition — the newest legacy
/// shape. First fallback step in the
/// new → pre-provenance → pre-temporal → pre-tier → legacy chain: an old file
/// lacks the trailing provenance bytes, so it fails the new-struct deserialize
/// and decodes here with `provenance` defaulting to `None` (⇒ `prng://legacy`).
#[derive(Deserialize)]
pub(crate) struct WavefrontMetaPreProvenance {
    pub id: Uuid,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub hallucinated: bool,
    pub is_self_referential: bool,
    #[serde(default)]
    pub modality: Modality,
    #[serde(default)]
    pub tier: crate::medium::types::Tier,
    #[serde(default)]
    pub effective_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub observed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

impl From<WavefrontMetaPreProvenance> for WavefrontMeta {
    fn from(p: WavefrontMetaPreProvenance) -> Self {
        WavefrontMeta {
            id: p.id,
            content: p.content,
            tags: p.tags,
            created_at: p.created_at,
            hallucinated: p.hallucinated,
            is_self_referential: p.is_self_referential,
            sga_class: None,
            fano_group: None,
            category: None,
            modality: p.modality,
            tier: p.tier,
            effective_at: p.effective_at,
            observed_at: p.observed_at,
            expires_at: p.expires_at,
            provenance: None,
            parent_id: None,
            is_facet: false,
            decomposed: false,
        }
    }
}

/// Pre-ADR-0049 shape: everything through `provenance`, before the facet fields.
/// FIRST fallback step in the
/// new → pre-facet → pre-provenance → pre-temporal → pre-tier → legacy chain.
/// A file written before facet encoding lacks the three trailing facet bytes per
/// record, so it fails the new-struct deserialize and decodes here with
/// `parent_id: None`, `is_facet: false`, `decomposed: false` — i.e. every
/// existing memory is an ordinary undecomposed wavefront, which is exactly right.
#[derive(Deserialize)]
pub(crate) struct WavefrontMetaPreFacet {
    pub id: Uuid,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub hallucinated: bool,
    pub is_self_referential: bool,
    #[serde(default)]
    pub modality: Modality,
    #[serde(default)]
    pub tier: crate::medium::types::Tier,
    #[serde(default)]
    pub effective_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub observed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub provenance: Option<crate::entropy::Provenance>,
}

impl From<WavefrontMetaPreFacet> for WavefrontMeta {
    fn from(p: WavefrontMetaPreFacet) -> Self {
        WavefrontMeta {
            id: p.id,
            content: p.content,
            tags: p.tags,
            created_at: p.created_at,
            hallucinated: p.hallucinated,
            is_self_referential: p.is_self_referential,
            sga_class: None,
            fano_group: None,
            category: None,
            modality: p.modality,
            tier: p.tier,
            effective_at: p.effective_at,
            observed_at: p.observed_at,
            expires_at: p.expires_at,
            provenance: p.provenance,
            parent_id: None,
            is_facet: false,
            decomposed: false,
        }
    }
}

/// Decode a bincode wavefront-metadata blob through the full backward-compat
/// fallback chain: new → pre-facet (ADR-0049) → pre-provenance (T1.4) →
/// pre-temporal → pre-tier → legacy. Old `.hrm` files lack the newest trailing bytes, so they fail the
/// new-struct decode and drop to the matching older shape (missing fields
/// default). Shared by both the chiral and flat read paths so the chain has one
/// definition.
pub(crate) fn decode_wavefront_metadata(bytes: &[u8]) -> Result<Vec<WavefrontMeta>, MediumError> {
    if let Ok(m) = bincode::deserialize::<Vec<WavefrontMeta>>(bytes) {
        return Ok(m);
    }
    if let Ok(pre) = bincode::deserialize::<Vec<WavefrontMetaPreFacet>>(bytes) {
        return Ok(pre.into_iter().map(Into::into).collect());
    }
    if let Ok(pre) = bincode::deserialize::<Vec<WavefrontMetaPreProvenance>>(bytes) {
        return Ok(pre.into_iter().map(Into::into).collect());
    }
    if let Ok(pre) = bincode::deserialize::<Vec<WavefrontMetaPreTemporal>>(bytes) {
        return Ok(pre.into_iter().map(Into::into).collect());
    }
    if let Ok(pre) = bincode::deserialize::<Vec<WavefrontMetaPreTier>>(bytes) {
        return Ok(pre.into_iter().map(Into::into).collect());
    }
    let legacy: Vec<WavefrontMetaLegacy> =
        bincode::deserialize(bytes).map_err(MediumError::Serialization)?;
    Ok(legacy.into_iter().map(Into::into).collect())
}

/// Verify the trailing 32-byte blake3 checksum on a .hrm file.
///
/// Both v1 and v2 save paths append `blake3(file[0..size-32])` as the final
/// 32 bytes before the atomic rename. Verifying on load catches data drift
/// that would otherwise crash deep inside the parser with a confusing error.
pub(crate) fn verify_blake3_trailing<P: AsRef<Path>>(path: P) -> Result<(), MediumError> {
    let path = path.as_ref();
    let size = std::fs::metadata(path)?.len();
    if size < 32 + 8 {
        // Too small to contain a header + checksum; let parse fail with its
        // own error rather than mis-reporting as checksum mismatch.
        return Ok(());
    }
    let mut f = File::open(path)?;
    let body_len = size - 32;
    let mut hasher = blake3::Hasher::new();
    // Stream body into the hasher
    let mut remaining = body_len;
    let mut buf = [0u8; 65536];
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = std::io::Read::read(&mut f, &mut buf[..want])?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    let expected = hasher.finalize();
    let mut actual = [0u8; 32];
    std::io::Read::read_exact(&mut f, &mut actual)?;
    if expected.as_bytes() != &actual {
        return Err(MediumError::ChecksumMismatch);
    }
    Ok(())
}

/// Offsets of the wall-clock timestamp in a .hrm header.
///
/// v1 and v2 lay the header out identically — magic(4) + version(4) +
/// `timestamp_millis`(8) — so one window covers both.
const HRM_TIMESTAMP_RANGE: std::ops::Range<u64> = 8..16;

/// A digest of what a .hrm file MEANS, ignoring when it was written.
///
/// `blake3` over the whole file answers "are these the same bytes", which is
/// not the question an operator asks. Every save stamps `Utc::now()` into the
/// header and the trailing checksum covers it, so two saves of identical
/// content a millisecond apart differ — same length, different checksum,
/// identical meaning. #952 fixed the other half of this (HashMaps serialized
/// in iteration order) and noted that sorting alone does not make saves
/// byte-identical; this is that remainder.
///
/// Skips the timestamp window and the trailing 32-byte checksum (which is a
/// function of both). Two saves of an unchanged medium produce the same
/// digest; any change to hemispheres, callosum or scales changes it.
///
/// This does NOT make the FILES byte-identical, and deliberately so: the
/// save timestamp is real information. It makes the QUESTION answerable
/// without destroying it.
pub fn content_digest<P: AsRef<Path>>(path: P) -> Result<String, MediumError> {
    let path = path.as_ref();
    let size = std::fs::metadata(path)?.len();
    let end = HRM_TIMESTAMP_RANGE.end;
    if size < 32 + end {
        // Too small to hold a header and a checksum. Digest what is there
        // rather than inventing a window past the end of the file.
        let bytes = std::fs::read(path)?;
        return Ok(blake3::hash(&bytes).to_hex().to_string());
    }
    let body_len = size - 32;
    let mut f = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 65536];
    let mut pos = 0u64;
    while pos < body_len {
        let want = (body_len - pos).min(buf.len() as u64) as usize;
        let n = std::io::Read::read(&mut f, &mut buf[..want])?;
        if n == 0 { break; }
        // Feed only the bytes outside the timestamp window. The window is
        // 8 bytes at a fixed offset, so it falls inside the first chunk in
        // practice — but slicing per chunk keeps this correct for any
        // buffer size rather than relying on that.
        let chunk_start = pos;
        let chunk_end = pos + n as u64;
        let skip_start = HRM_TIMESTAMP_RANGE.start.max(chunk_start);
        let skip_end = end.min(chunk_end);
        if skip_start >= skip_end {
            hasher.update(&buf[..n]);
        } else {
            let a = (skip_start - chunk_start) as usize;
            let b = (skip_end - chunk_start) as usize;
            hasher.update(&buf[..a]);
            hasher.update(&buf[b..n]);
        }
        pos = chunk_end;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

impl ChiralMedium {
    /// Save the chiral medium to a .hrm v2 file.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), MediumError> {
        let path_ref = path.as_ref();
        // Unique tmp path per save. Concurrent kannaka processes (substrate
        // run / swarm join / swarm serve / attention serve / ad-hoc ask)
        // all hit the same .hrm file; if two of them tried to write to
        // `kannaka.hrm.tmp` simultaneously the second File::create
        // truncated the first mid-stream, the blake3 was computed over
        // the interleaved bytes, and rename() landed a checksum-valid
        // but semantically corrupt file. Observed on Oracle 2026-05-26 —
        // `kannaka.hrm` failed ChiralMedium::load with "checksum mismatch"
        // on every load. Tagging the tmp with pid + nanos makes each
        // writer's tmp file private; rename is still atomic.
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_path = path_ref.with_extension(format!("hrm.tmp.{pid}.{nanos}"));
        let file = File::create(&tmp_path)?;
        let mut w = BufWriter::new(file);

        // Magic bytes (v2)
        w.write_all(&HRM_MAGIC_V2)?;
        // Version
        w.write_all(&HRM_VERSION_CHIRAL.to_le_bytes())?;
        // Timestamp
        let timestamp = chrono::Utc::now().timestamp_millis();
        w.write_all(&timestamp.to_le_bytes())?;

        // Write left hemisphere
        Self::write_hemisphere(&mut w, &self.left)?;
        // Write right hemisphere
        Self::write_hemisphere(&mut w, &self.right)?;

        // Write callosum state (bincode)
        let callosum_bytes = bincode::serialize(&self.callosum)
            .map_err(MediumError::Serialization)?;
        w.write_all(&(callosum_bytes.len() as u32).to_le_bytes())?;
        w.write_all(&callosum_bytes)?;

        // Write chiral scales (bincode), key-sorted.
        //
        // `scales` is a std HashMap, whose RandomState randomises iteration
        // order per process. Emitting it unsorted made every save of unchanged
        // content produce different bytes at the same length (#952), so a
        // checksum could not answer "did this store change". Sorting by key
        // costs one sort per save and makes this section a function of content
        // alone. The loader collects straight back into a HashMap, so order
        // carries no meaning and this is a pure serialization change.
        let mut scales_vec: Vec<(uuid::Uuid, ChiralScale)> =
            self.scales.iter().map(|(&k, &v)| (k, v)).collect();
        scales_vec.sort_unstable_by_key(|(k, _)| *k);
        let scales_bytes = bincode::serialize(&scales_vec)
            .map_err(MediumError::Serialization)?;
        w.write_all(&(scales_bytes.len() as u32).to_le_bytes())?;
        w.write_all(&scales_bytes)?;

        // Write ID mappings (bincode), key-sorted — same reason as `scales`
        // above. `right_to_left` is not written at all; the loader derives it
        // by flipping these pairs, so this one section fixes both maps.
        let mut lr_vec: Vec<(uuid::Uuid, uuid::Uuid)> =
            self.left_to_right.iter().map(|(&k, &v)| (k, v)).collect();
        lr_vec.sort_unstable_by_key(|(k, _)| *k);
        let lr_bytes = bincode::serialize(&lr_vec)
            .map_err(MediumError::Serialization)?;
        w.write_all(&(lr_bytes.len() as u32).to_le_bytes())?;
        w.write_all(&lr_bytes)?;

        // Flush and compute checksum
        w.flush()?;
        drop(w);

        let mut file = File::open(&tmp_path)?;
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut file, &mut hasher)?;
        let checksum = hasher.finalize();
        drop(file);

        let mut file = std::fs::OpenOptions::new().append(true).open(&tmp_path)?;
        file.write_all(checksum.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);

        std::fs::rename(&tmp_path, path_ref)?;

        // Sweep orphaned tmp files. The per-pid/nanos tmp names above avoid
        // concurrent-save corruption, but interrupted or concurrent saves (a
        // killed dream, the lite-dream double-writer) leave their
        // `kannaka.hrm.tmp.<pid>.<nanos>` behind — nothing reclaimed them, and
        // at ~100 MB each they accumulated to ~3 GB and filled the root disk to
        // 100% on 2026-06-18 (SQLITE_CANTOPEN cascade). A save takes seconds, so
        // any sibling tmp older than 5 min is a dead orphan; never delete a
        // fresh one (it could be another writer's in-flight save).
        if let (Some(dir), Some(stem)) = (
            path_ref.parent(),
            path_ref.file_name().and_then(|s| s.to_str()),
        ) {
            let tmp_prefix = format!("{stem}.tmp.");
            if let Ok(entries) = std::fs::read_dir(dir) {
                let now = std::time::SystemTime::now();
                for entry in entries.flatten() {
                    if !entry.file_name().to_string_lossy().starts_with(&tmp_prefix) {
                        continue;
                    }
                    let old = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .map(|t| now.duration_since(t).map(|d| d.as_secs() > 300).unwrap_or(false))
                        .unwrap_or(false);
                    if old {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }

        Ok(())
    }

    /// Load a chiral medium from a .hrm file.
    /// Auto-detects v1 vs v2 format:
    /// - v1: loads as Medium, then wraps with from_medium()
    /// - v2: loads native chiral format
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, MediumError> {
        let path_ref = path.as_ref();

        // Verify trailing blake3 checksum before parsing. Catches the kind of
        // mid-stream drift that previously surfaced as `failed to fill whole
        // buffer` somewhere deep inside `read_hemisphere`.
        verify_blake3_trailing(path_ref)?;

        let file = File::open(path_ref)?;
        let mut reader = BufReader::new(file);

        // Read magic bytes
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;

        if magic == HRM_MAGIC {
            // v1 format — load as Medium and convert
            drop(reader);
            let medium = Medium::load(path_ref)?;
            Ok(ChiralMedium::from_medium(&medium))
        } else if magic == HRM_MAGIC_V2 {
            // v2 format — load native chiral
            Self::load_v2(reader)
        } else {
            Err(MediumError::InvalidMagic)
        }
    }

    /// Load v2 chiral format (reader already past magic bytes).
    fn load_v2(mut reader: BufReader<File>) -> Result<Self, MediumError> {
        // Version
        let mut version_bytes = [0u8; 4];
        reader.read_exact(&mut version_bytes)?;
        let version = u32::from_le_bytes(version_bytes);
        if version != HRM_VERSION_CHIRAL {
            return Err(MediumError::UnsupportedVersion(version));
        }

        // Timestamp (skip)
        let mut ts_bytes = [0u8; 8];
        reader.read_exact(&mut ts_bytes)?;

        // Read hemispheres
        let left = Self::read_hemisphere(&mut reader, Hand::Left)?;
        let right = Self::read_hemisphere(&mut reader, Hand::Right)?;

        // #367: each section length is read straight from the file and used to
        // size an allocation BEFORE any body bytes are read and before the
        // trailing checksum is verified. A truncated/desynced .hrm with a giant
        // length would request a multi-TB allocation and abort the process. Cap
        // each section like the metadata path already does.
        const MAX_SECTION_BYTES: usize = 256 * 1024 * 1024;
        let guard_section = |len: usize, what: &str| -> Result<(), MediumError> {
            if len > MAX_SECTION_BYTES {
                return Err(MediumError::CorruptHrm(format!(
                    "implausible {what}_len={len} (max {MAX_SECTION_BYTES}) — .hrm layout desync"
                )));
            }
            Ok(())
        };

        // Read callosum
        let mut len_bytes = [0u8; 4];
        reader.read_exact(&mut len_bytes)?;
        let callosum_len = u32::from_le_bytes(len_bytes) as usize;
        guard_section(callosum_len, "callosum")?;
        let mut callosum_bytes = vec![0u8; callosum_len];
        reader.read_exact(&mut callosum_bytes)?;
        let callosum: CorpusCallosum = bincode::deserialize(&callosum_bytes)
            .map_err(MediumError::Serialization)?;

        // Read scales
        reader.read_exact(&mut len_bytes)?;
        let scales_len = u32::from_le_bytes(len_bytes) as usize;
        guard_section(scales_len, "scales")?;
        let mut scales_bytes = vec![0u8; scales_len];
        reader.read_exact(&mut scales_bytes)?;
        let scales_vec: Vec<(uuid::Uuid, ChiralScale)> = bincode::deserialize(&scales_bytes)
            .map_err(MediumError::Serialization)?;
        let scales: HashMap<uuid::Uuid, ChiralScale> = scales_vec.into_iter().collect();

        // Read ID mappings
        reader.read_exact(&mut len_bytes)?;
        let lr_len = u32::from_le_bytes(len_bytes) as usize;
        guard_section(lr_len, "id_map")?;
        let mut lr_bytes = vec![0u8; lr_len];
        reader.read_exact(&mut lr_bytes)?;
        let lr_vec: Vec<(uuid::Uuid, uuid::Uuid)> = bincode::deserialize(&lr_bytes)
            .map_err(MediumError::Serialization)?;
        let left_to_right: HashMap<uuid::Uuid, uuid::Uuid> = lr_vec.iter().cloned().collect();
        let right_to_left: HashMap<uuid::Uuid, uuid::Uuid> =
            lr_vec.into_iter().map(|(l, r)| (r, l)).collect();

        // Skip checksum verification for now (same pattern as v1)

        Ok(ChiralMedium {
            left,
            right,
            callosum,
            fano: FanoPlane::new(),
            scales,
            left_to_right,
            right_to_left,
        })
    }

    /// Write a hemisphere to the output stream.
    fn write_hemisphere<W: Write>(w: &mut W, h: &Hemisphere) -> Result<(), MediumError> {
        // Hand
        let hand_byte: u8 = match h.hand {
            Hand::Left => 0,
            Hand::Right => 1,
        };
        w.write_all(&[hand_byte])?;

        // Dimensions
        w.write_all(&(h.dims as u32).to_le_bytes())?;

        // Wavefront count
        let n = h.count() as u32;
        w.write_all(&n.to_le_bytes())?;

        // Wavefronts tensor (row-major) — only active rows
        let active = n as usize;
        for i in 0..active {
            for j in 0..h.dims {
                w.write_all(&h.wavefronts[[i, j]].to_le_bytes())?;
            }
        }

        // Energy, frequency, phase — only active entries
        for i in 0..active {
            w.write_all(&h.energy[i].to_le_bytes())?;
        }
        for i in 0..active {
            w.write_all(&h.frequency[i].to_le_bytes())?;
        }
        for i in 0..active {
            w.write_all(&h.phase[i].to_le_bytes())?;
        }

        // Timestamps — write exactly `active` entries.
        //
        // Earlier revisions wrote the full Vec, which crashed the loader if
        // `h.timestamps.len()` ever drifted from `h.count()`. Pad with 0 if
        // the Vec is short so the writer stays self-consistent under any
        // upstream desync.
        for i in 0..active {
            let ts = h.timestamps.get(i).copied().unwrap_or(0);
            w.write_all(&ts.to_le_bytes())?;
        }

        // Metadata (bincode)
        let meta_bytes = bincode::serialize(&h.metadata)
            .map_err(MediumError::Serialization)?;
        w.write_all(&(meta_bytes.len() as u32).to_le_bytes())?;
        w.write_all(&meta_bytes)?;

        Ok(())
    }

    /// Read a hemisphere from the input stream.
    fn read_hemisphere<R: Read>(r: &mut R, _expected_hand: Hand) -> Result<Hemisphere, MediumError> {
        // Hand
        let mut hand_byte = [0u8; 1];
        r.read_exact(&mut hand_byte)?;
        let hand = match hand_byte[0] {
            0 => Hand::Left,
            1 => Hand::Right,
            _ => return Err(MediumError::InvalidMagic), // Reuse error for bad hand byte
        };

        // Dimensions
        let mut dims_bytes = [0u8; 4];
        r.read_exact(&mut dims_bytes)?;
        let dims = u32::from_le_bytes(dims_bytes) as usize;

        // Wavefront count
        let mut n_bytes = [0u8; 4];
        r.read_exact(&mut n_bytes)?;
        let n = u32::from_le_bytes(n_bytes) as usize;

        // #367: `n` and `dims` come straight from the file. Validate the tensor
        // size before allocating so a corrupt count can't request a multi-TB
        // Vec and abort the process before the checksum can reject the file.
        // 1 billion f32 = 4 GiB ceiling — far above any real hemisphere.
        const MAX_WAVEFRONT_ELEMS: u64 = 1024 * 1024 * 1024;
        let wf_elems = (n as u64).checked_mul(dims as u64);
        match wf_elems {
            Some(e) if e <= MAX_WAVEFRONT_ELEMS => {}
            _ => {
                return Err(MediumError::CorruptHrm(format!(
                    "implausible wavefront tensor n={n} dims={dims} (max {MAX_WAVEFRONT_ELEMS} elems) — .hrm layout desync"
                )));
            }
        }

        // Wavefronts tensor
        let mut wf_data = vec![0.0f32; n * dims];
        for val in &mut wf_data {
            let mut bytes = [0u8; 4];
            r.read_exact(&mut bytes)?;
            *val = f32::from_le_bytes(bytes);
        }
        let wavefronts = if n > 0 {
            Array2::from_shape_vec((n, dims), wf_data).unwrap()
        } else {
            Array2::zeros((0, dims))
        };

        // Energy, frequency, phase
        let mut energy_data = vec![0.0f32; n];
        let mut freq_data = vec![0.0f32; n];
        let mut phase_data = vec![0.0f32; n];
        for val in &mut energy_data {
            let mut bytes = [0u8; 4];
            r.read_exact(&mut bytes)?;
            *val = f32::from_le_bytes(bytes);
        }
        for val in &mut freq_data {
            let mut bytes = [0u8; 4];
            r.read_exact(&mut bytes)?;
            *val = f32::from_le_bytes(bytes);
        }
        for val in &mut phase_data {
            let mut bytes = [0u8; 4];
            r.read_exact(&mut bytes)?;
            *val = f32::from_le_bytes(bytes);
        }

        // Timestamps
        let mut timestamps = vec![0i64; n];
        for ts in &mut timestamps {
            let mut bytes = [0u8; 8];
            r.read_exact(&mut bytes)?;
            *ts = i64::from_le_bytes(bytes);
        }

        // Metadata — try current format first, fall back to pre-NCS format without modality
        let mut len_bytes = [0u8; 4];
        r.read_exact(&mut len_bytes)?;
        let meta_len = u32::from_le_bytes(len_bytes) as usize;
        // Sanity cap: 256 MiB. Anything larger means we walked off the rails
        // (the count/timestamps section above was misaligned) — report it
        // before we try to allocate gigabytes and crash on read_exact.
        const MAX_META_BYTES: usize = 256 * 1024 * 1024;
        if meta_len > MAX_META_BYTES {
            return Err(MediumError::CorruptHrm(format!(
                "implausible meta_len={meta_len} (max {MAX_META_BYTES}) — file layout desync at hemisphere {hand:?}"
            )));
        }
        let mut meta_bytes = vec![0u8; meta_len];
        r.read_exact(&mut meta_bytes)?;
        let metadata: Vec<WavefrontMeta> = decode_wavefront_metadata(&meta_bytes)?;

        // #362: section sizes come from independent sources — `n` (the on-disk
        // count) sizes wavefronts/energy/frequency/phase/timestamps, while the
        // metadata Vec is sized by its own bincode length. If they disagree,
        // `len = metadata.len()` would let `energy[i]`/`wavefronts.row(i)` for
        // `i in 0..len` index out of bounds and panic on load. Surface the
        // structural desync as CorruptHrm instead of trusting one section over
        // the others (the checksum guard only catches byte drift, not this).
        if metadata.len() != n || timestamps.len() != n {
            return Err(MediumError::CorruptHrm(format!(
                "hemisphere {:?} section-size desync: count={} metadata={} timestamps={}",
                hand, n, metadata.len(), timestamps.len()
            )));
        }

        // Build ID index
        let mut id_to_index = HashMap::new();
        for (i, meta) in metadata.iter().enumerate() {
            id_to_index.insert(meta.id, i);
        }

        let len = n;
        Ok(Hemisphere {
            hand,
            wavefronts,
            energy: Array1::from_vec(energy_data),
            frequency: Array1::from_vec(freq_data),
            phase: Array1::from_vec(phase_data),
            timestamps,
            metadata,
            id_to_index,
            dims,
            len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codebook::Codebook;
    use crate::encoding::{EncodingPipeline, SimpleHashEncoder};
    use std::path::PathBuf;

    /// #952 left this half open and it was lost when the issue auto-closed:
    /// sorting the HashMaps removed the nondeterministic ORDER, but every save
    /// still stamps `Utc::now()` into the header and the trailing checksum
    /// covers it. So a file hash still cannot answer "did this store change",
    /// which is the cheapest integrity check an operator has — and the one the
    /// encoder-flip runbook leans on ("assert the store sha changed").
    ///
    /// Three assertions, and the third keeps the other two honest: a digest
    /// that ignored the whole file would pass the first two.
    #[test]
    fn content_digest_ignores_the_save_timestamp_but_not_the_content() {
        let encoder = Box::new(SimpleHashEncoder::new(384, 42));
        let pipeline = EncodingPipeline::new(encoder, Codebook::new(384, WAVEFRONT_DIM, 42));
        let mut cm = ChiralMedium::new();
        cm.store("a memory that should hash the same twice", 0.9, &pipeline).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let a: PathBuf = dir.path().join("a.hrm");
        let b: PathBuf = dir.path().join("b.hrm");
        let c: PathBuf = dir.path().join("c.hrm");

        cm.save(&a).unwrap();
        // Guarantee a different millisecond, so the header really does differ.
        std::thread::sleep(std::time::Duration::from_millis(5));
        cm.save(&b).unwrap();

        let whole = |p: &PathBuf| blake3::hash(&std::fs::read(p).unwrap()).to_hex().to_string();
        assert_ne!(
            whole(&a),
            whole(&b),
            "the files should NOT be byte-identical: the save timestamp is real information and this does not remove it"
        );
        assert_eq!(
            content_digest(&a).unwrap(),
            content_digest(&b).unwrap(),
            "same content saved twice must produce the same content digest"
        );

        cm.store("a second memory, so the content genuinely differs", 0.9, &pipeline)
            .unwrap();
        cm.save(&c).unwrap();
        assert_ne!(
            content_digest(&a).unwrap(),
            content_digest(&c).unwrap(),
            "a real change must move the digest, or it is measuring nothing"
        );
    }

    // ─── ADR-0049 facet-encoding serialization lock ──────────────────────────
    //
    // The facet fields (`parent_id`, `is_facet`, `decomposed`) MUST be the last
    // serialized fields of WavefrontMeta. The whole back-compat scheme rests on
    // one property: a pre-facet file is SHORT by exactly those trailing bytes,
    // so the new-struct decode FAILS and drops to `WavefrontMetaPreFacet`.
    //
    // If someone inserts a field in the middle instead, old bytes may still
    // deserialize into the new struct — with every subsequent field shifted.
    // The blake3 checksum still validates (the file is unchanged), the load
    // reports no error, and ~600 memories silently become garbage. These tests
    // exist to make that specific accident impossible.

    /// Wire-shape mirror of the pre-facet layout, used to MINT old-format bytes.
    /// Field order must match `WavefrontMetaPreFacet` exactly.
    #[derive(serde::Serialize)]
    struct PreFacetWire {
        id: Uuid,
        content: String,
        tags: Vec<String>,
        created_at: DateTime<Utc>,
        hallucinated: bool,
        is_self_referential: bool,
        modality: Modality,
        tier: crate::medium::types::Tier,
        effective_at: Option<DateTime<Utc>>,
        observed_at: Option<DateTime<Utc>>,
        expires_at: Option<DateTime<Utc>>,
        provenance: Option<crate::entropy::Provenance>,
    }

    fn pre_facet_fixture() -> (Uuid, Vec<u8>) {
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let rec = PreFacetWire {
            id,
            content: "a compound memory written before facet encoding existed".to_string(),
            tags: vec!["fixture".to_string()],
            created_at: DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            hallucinated: false,
            is_self_referential: false,
            modality: Modality::Semantic,
            tier: crate::medium::types::Tier::default(),
            effective_at: None,
            observed_at: None,
            expires_at: None,
            // MUST be Some: without the PreFacet chain link these bytes still
            // decode via PreProvenance (bincode ignores the trailing byte) and
            // every assertion below passes while provenance is silently lost.
            // Carrying a real value is what makes the link testable.
            provenance: Some(crate::entropy::Provenance::prng()),
        };
        (id, bincode::serialize(&vec![rec]).expect("mint pre-facet bytes"))
    }

    #[test]
    fn pre_facet_bytes_decode_with_facet_fields_defaulted() {
        let (id, bytes) = pre_facet_fixture();
        let decoded = decode_wavefront_metadata(&bytes).expect("pre-facet blob must decode");

        assert_eq!(decoded.len(), 1);
        let m = &decoded[0];
        // Content integrity: the misdecode failure mode shows up here first.
        assert_eq!(m.id, id, "id shifted — fields are misaligned");
        assert_eq!(
            m.content, "a compound memory written before facet encoding existed",
            "content shifted — a mid-struct field insertion has broken the layout"
        );
        assert_eq!(m.tags, vec!["fixture".to_string()]);
        assert_eq!(m.modality, Modality::Semantic);
        // Provenance must SURVIVE. Drop the PreFacet link and these bytes decode
        // via PreProvenance instead, which has no provenance field — the record
        // loads clean and the entropy lineage silently becomes None.
        assert_eq!(
            m.provenance.as_ref().map(|p| p.source.as_str()),
            Some("prng://"),
            "provenance lost — the PreFacet chain link is missing or out of order"
        );
        // An existing memory is an ordinary, undecomposed, non-facet wavefront.
        assert_eq!(m.parent_id, None);
        assert!(!m.is_facet);
        assert!(!m.decomposed);
    }

    #[test]
    fn forward_compat_old_binary_reading_a_new_file() {
        // A NEW binary writes 3 extra trailing bytes per record even with the
        // decompose flag OFF, because the fields are part of WavefrontMeta.
        // An OLD binary's "new struct" is exactly our PreFacet shape. Multi-record
        // is the dangerous case: the extra bytes land MID-STREAM.
        let v: Vec<WavefrontMeta> = (0..5)
            .map(|i| {
                let mut m =
                    WavefrontMeta::new(Uuid::new_v4(), format!("record {i} content here"));
                m.provenance = Some(crate::entropy::Provenance::prng());
                m
            })
            .collect();
        let new_bytes = bincode::serialize(&v).unwrap();

        let as_old = bincode::deserialize::<Vec<WavefrontMetaPreFacet>>(&new_bytes);
        match &as_old {
            Ok(recs) => {
                println!("  OLD binary DECODED a NEW file: {} records", recs.len());
                for (i, r) in recs.iter().enumerate() {
                    println!("    rec {i}: content={:?}", r.content);
                }
            }
            Err(e) => println!("  OLD binary REJECTED a NEW file (loud failure): {e}"),
        }
    }

    #[test]
    fn pre_facet_bytes_must_not_decode_as_the_new_struct() {
        // THE load-bearing assertion. If this ever passes, the fallback chain is
        // dead: old files would decode straight into the new struct, shifted,
        // behind a checksum that still validates.
        let (_, bytes) = pre_facet_fixture();
        assert!(
            bincode::deserialize::<Vec<WavefrontMeta>>(&bytes).is_err(),
            "pre-facet bytes deserialized as the NEW struct — the facet fields are \
             no longer strictly trailing, and every old .hrm will silently misdecode"
        );
    }

    #[test]
    fn facet_fields_round_trip_and_cost_three_bytes() {
        let mut m = WavefrontMeta::new(Uuid::new_v4(), "facet row".to_string());
        let parent = Uuid::new_v4();
        m.parent_id = Some(parent);
        m.is_facet = true;

        let bytes = bincode::serialize(&vec![m.clone()]).unwrap();
        let back = decode_wavefront_metadata(&bytes).expect("new-shape must decode");
        assert_eq!(back[0].parent_id, Some(parent));
        assert!(back[0].is_facet);
        assert!(!back[0].decomposed);

        // A plain (non-facet) row costs exactly 3 extra bytes over the old shape:
        // Option::None tag + two bools. That delta is what makes old files fail
        // the new decode; if it ever reaches 0 the chain silently breaks.
        let plain = WavefrontMeta::new(
            Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
            "a compound memory written before facet encoding existed".to_string(),
        );
        let mut plain = plain;
        plain.tags = vec!["fixture".to_string()];
        plain.modality = Modality::Semantic;
        plain.created_at = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        plain.provenance = Some(crate::entropy::Provenance::prng());
        let new_len = bincode::serialize(&vec![plain]).unwrap().len();
        let (_, old_bytes) = pre_facet_fixture();
        assert_eq!(
            new_len - old_bytes.len(),
            3,
            "facet fields must add exactly 3 trailing bytes to an unfaceted row"
        );
    }

    // ADR-0031: locks the bincode back-compat invariant for the `tier` field.
    // Pre-tier metadata bytes (modality, no tier — the layout of every .hrm
    // written before this change) MUST fail the new-struct deserialize and
    // succeed via the WavefrontMetaPreTier fallback with tier defaulting to
    // LongTerm. A regression here would silently corrupt existing HRM files.
    #[test]
    fn pretier_metadata_decodes_via_fallback_with_default_tier() {
        use chrono::Utc;
        use serde::Serialize;
        use uuid::Uuid;

        // Exact on-disk layout of pre-tier WavefrontMeta (sga/fano/category are
        // #[serde(skip)], so they never hit the wire — modality is the last field).
        #[derive(Serialize)]
        struct OldMeta {
            id: Uuid,
            content: String,
            tags: Vec<String>,
            created_at: chrono::DateTime<Utc>,
            hallucinated: bool,
            is_self_referential: bool,
            modality: Modality,
        }
        let old = vec![OldMeta {
            id: Uuid::nil(),
            content: "legacy".to_string(),
            tags: vec![],
            created_at: Utc::now(),
            hallucinated: false,
            is_self_referential: false,
            modality: Modality::Audio,
        }];
        let bytes = bincode::serialize(&old).unwrap();

        // New struct (with tier) must NOT decode old bytes — this is what makes
        // the loader fall through to the pre-tier path instead of mis-reading.
        assert!(bincode::deserialize::<Vec<WavefrontMeta>>(&bytes).is_err());

        // Pre-tier fallback decodes cleanly and defaults tier to LongTerm.
        let pre: Vec<WavefrontMetaPreTier> = bincode::deserialize(&bytes).unwrap();
        let conv: Vec<WavefrontMeta> = pre.into_iter().map(Into::into).collect();
        assert_eq!(conv.len(), 1);
        assert_eq!(conv[0].modality, Modality::Audio);
        assert_eq!(conv[0].tier, Tier::LongTerm);
    }

    #[test]
    fn new_metadata_roundtrips_tier() {
        let mut m = WavefrontMeta::new(uuid::Uuid::nil(), "x".to_string());
        m.tier = Tier::Pinned;
        let bytes = bincode::serialize(&vec![m]).unwrap();
        let back: Vec<WavefrontMeta> = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back[0].tier, Tier::Pinned);
    }

    // Quantum-Wave T1.4 (#474): locks the bincode back-compat invariant for the
    // `provenance` field. Pre-provenance metadata bytes (modality + tier +
    // temporal, NO provenance — every .hrm written before this change) MUST fail
    // the new-struct deserialize and succeed via the WavefrontMetaPreProvenance
    // fallback with provenance defaulting to None (⇒ prng://legacy). A regression
    // here would silently corrupt existing HRM files.
    #[test]
    fn preprovenance_metadata_decodes_via_fallback_with_none_provenance() {
        use chrono::{DateTime, Utc};
        use serde::Serialize;
        use uuid::Uuid;

        // Exact on-disk layout of pre-provenance WavefrontMeta.
        #[derive(Serialize)]
        struct OldMeta {
            id: Uuid,
            content: String,
            tags: Vec<String>,
            created_at: DateTime<Utc>,
            hallucinated: bool,
            is_self_referential: bool,
            modality: Modality,
            tier: Tier,
            effective_at: Option<DateTime<Utc>>,
            observed_at: Option<DateTime<Utc>>,
            expires_at: Option<DateTime<Utc>>,
        }
        let old = vec![OldMeta {
            id: Uuid::nil(),
            content: "legacy".to_string(),
            tags: vec![],
            created_at: Utc::now(),
            hallucinated: true,
            is_self_referential: false,
            modality: Modality::Audio,
            tier: Tier::Pinned,
            effective_at: None,
            observed_at: None,
            expires_at: None,
        }];
        let bytes = bincode::serialize(&old).unwrap();

        // New struct (with provenance) must NOT decode old bytes — this is what
        // makes the loader fall through to the pre-provenance path.
        assert!(bincode::deserialize::<Vec<WavefrontMeta>>(&bytes).is_err());

        // The full fallback chain decodes it, preserving every older field and
        // defaulting provenance to None.
        let decoded = decode_wavefront_metadata(&bytes).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].content, "legacy");
        assert_eq!(decoded[0].tier, Tier::Pinned);
        assert!(decoded[0].hallucinated);
        assert_eq!(decoded[0].provenance, None, "old record ⇒ None ⇒ prng://legacy");
    }

    #[test]
    fn new_metadata_roundtrips_provenance() {
        let mut m = WavefrontMeta::new(uuid::Uuid::nil(), "x".to_string());
        m.provenance = Some(crate::entropy::Provenance::reservoir(
            "drbg-expand",
            vec!["job-1".to_string()],
            vec!["dev".to_string()],
        ));
        let bytes = bincode::serialize(&vec![m.clone()]).unwrap();
        // New format decodes directly (no fallback needed).
        let back = decode_wavefront_metadata(&bytes).unwrap();
        assert_eq!(back[0].provenance, m.provenance);
    }

    // Wave 3 Task 3.2b: locks the bincode back-compat invariant for the temporal
    // fields. Pre-temporal metadata bytes (modality + tier, NO temporal fields —
    // the layout of every .hrm written before this change) MUST fail the new
    // struct deserialize and succeed via the WavefrontMetaPreTemporal fallback
    // with the temporal bounds defaulting to None. A regression here would
    // silently corrupt existing HRM files.
    #[test]
    fn pretemporal_metadata_decodes_via_fallback_with_none_bounds() {
        use chrono::Utc;
        use serde::Serialize;
        use uuid::Uuid;

        // Exact on-disk layout of pre-temporal WavefrontMeta: tier is the last
        // field (sga/fano/category are #[serde(skip)] and never hit the wire).
        #[derive(Serialize)]
        struct OldMeta {
            id: Uuid,
            content: String,
            tags: Vec<String>,
            created_at: chrono::DateTime<Utc>,
            hallucinated: bool,
            is_self_referential: bool,
            modality: Modality,
            tier: Tier,
        }
        let old = vec![OldMeta {
            id: Uuid::nil(),
            content: "pre-temporal".to_string(),
            tags: vec![],
            created_at: Utc::now(),
            hallucinated: false,
            is_self_referential: false,
            modality: Modality::Audio,
            tier: Tier::Pinned,
        }];
        let bytes = bincode::serialize(&old).unwrap();

        // New struct (with the 3 temporal fields appended) must NOT decode old
        // bytes — this is what makes the loader fall through to the pre-temporal
        // path instead of mis-reading trailing bytes.
        assert!(bincode::deserialize::<Vec<WavefrontMeta>>(&bytes).is_err());

        // Pre-temporal fallback decodes cleanly, preserves tier+modality, and
        // defaults all temporal bounds to None.
        let pre: Vec<WavefrontMetaPreTemporal> = bincode::deserialize(&bytes).unwrap();
        let conv: Vec<WavefrontMeta> = pre.into_iter().map(Into::into).collect();
        assert_eq!(conv.len(), 1);
        assert_eq!(conv[0].modality, Modality::Audio);
        assert_eq!(conv[0].tier, Tier::Pinned);
        assert!(conv[0].effective_at.is_none());
        assert!(conv[0].observed_at.is_none());
        assert!(conv[0].expires_at.is_none());
    }

    // Wave 3 Task 3.2b acceptance: a memory created with temporal bounds must
    // survive a full ChiralMedium save → reload round-trip on disk.
    #[test]
    fn temporal_fields_survive_save_reload() {
        use chrono::{Duration, Utc};

        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();
        let id = cm.store("temporal roundtrip subject", 0.9, &pipeline).unwrap();

        let effective = Utc::now() - Duration::days(1);
        let expires = Utc::now() + Duration::days(30);
        {
            let idx = *cm.right.id_to_index.get(&id).unwrap();
            cm.right.metadata[idx].effective_at = Some(effective);
            cm.right.metadata[idx].expires_at = Some(expires);
        }

        let dir = std::env::temp_dir();
        let path = dir.join("test_chiral_temporal_roundtrip.hrm");
        cm.save(&path).unwrap();
        let loaded = ChiralMedium::load(&path).unwrap();

        let idx = *loaded.right.id_to_index.get(&id).unwrap();
        let meta = &loaded.right.metadata[idx];
        assert_eq!(meta.effective_at, Some(effective), "effective_at must round-trip");
        assert_eq!(meta.expires_at, Some(expires), "expires_at must round-trip");
        assert!(meta.observed_at.is_none(), "unset observed_at stays None");

        let _ = std::fs::remove_file(&path);
    }

    // ─── #952: a save of unchanged content must produce the same bytes ──────
    //
    // `scales` and `left_to_right` are std HashMaps, and RandomState gives each
    // *instance* a different iteration order. So a store that is saved, loaded
    // and saved again used to emit the same pairs in a different order: same
    // byte length, different blake3, identical semantics. That made a checksum
    // useless for answering "did this store change".
    //
    // Save → load → save is the exact shape of the reported failure (two copies
    // of one store, one read on each, two different checksums) and it does not
    // depend on hash randomness to *fail*: the reloaded medium's maps are fresh
    // instances, so an unsorted writer has no reason to reproduce the order.
    //
    // Two regions are expected to differ and are excluded: the wall-clock
    // header timestamp at bytes 8..16, and the trailing 32-byte blake3 that is
    // computed over it. Everything else is content and must match exactly.
    #[test]
    fn saving_unchanged_content_twice_produces_identical_bytes() {
        const TS: std::ops::Range<usize> = 8..16;
        const CHECKSUM_LEN: usize = 32;

        fn content_of(path: &PathBuf) -> Vec<u8> {
            let raw = std::fs::read(path).unwrap();
            assert!(raw.len() > TS.end + CHECKSUM_LEN, "file too short to be a .hrm");
            let mut body = raw[..raw.len() - CHECKSUM_LEN].to_vec();
            for b in &mut body[TS] {
                *b = 0;
            }
            body
        }

        let pipeline = test_pipeline();
        let mut cm = ChiralMedium::new();
        // Enough pairs that a coincidental order match is not worth considering.
        for i in 0..32 {
            cm.store(&format!("determinism subject number {i}"), 0.5, &pipeline)
                .unwrap();
        }

        let dir = std::env::temp_dir();
        let first = dir.join("test_chiral_determinism_a.hrm");
        let second = dir.join("test_chiral_determinism_b.hrm");

        cm.save(&first).unwrap();
        // Reload, change nothing, save again — fresh HashMap instances.
        let reloaded = ChiralMedium::load(&first).unwrap();
        reloaded.save(&second).unwrap();

        let a = content_of(&first);
        let b = content_of(&second);

        assert_eq!(
            a.len(),
            b.len(),
            "unchanged content changed length — this is a real content diff, not ordering"
        );
        let first_diff = a.iter().zip(b.iter()).position(|(x, y)| x != y);
        assert!(
            first_diff.is_none(),
            "a save of unchanged content produced different bytes at offset {:?} (#952)",
            first_diff
        );

        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&second);
    }

    fn test_pipeline() -> EncodingPipeline {
        let encoder = Box::new(SimpleHashEncoder::new(384, 42));
        let codebook = Codebook::new(384, WAVEFRONT_DIM, 42);
        EncodingPipeline::new(encoder, codebook)
    }

    #[test]
    fn save_recovers_from_short_timestamps_vec() {
        // Simulate the production failure mode: a hemisphere whose timestamps
        // Vec is shorter than count() (3 entries missing). The writer must
        // still produce a file whose timestamps section size matches count
        // so the reader can find meta_len at the right offset.
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();
        for i in 0..10 {
            cm.store(&format!("ts-drift test {i}"), 0.8, &pipeline).unwrap();
        }
        // Manually corrupt the right hemisphere: pop 3 timestamps without
        // touching len/metadata. This mirrors whatever past code path left
        // Oracle's hemisphere with count=142 / timestamps.len()=139.
        let right_count = cm.right.count();
        for _ in 0..3 {
            cm.right.timestamps.pop();
        }
        assert_eq!(cm.right.count(), right_count);
        assert_eq!(cm.right.timestamps.len(), right_count - 3);

        let dir = std::env::temp_dir();
        let path = dir.join("test_chiral_ts_drift.hrm");
        cm.save(&path).unwrap();

        // The file MUST load cleanly — the writer padded with zeros so the
        // section boundaries are predictable.
        let loaded = ChiralMedium::load(&path).unwrap();
        assert_eq!(loaded.right.count(), right_count);
        assert_eq!(loaded.right.timestamps.len(), right_count);

        let _ = std::fs::remove_file(&path);
    }

    // #362: a file whose section sizes disagree (count != metadata.len()) must
    // be rejected as CorruptHrm on load, NOT trusted into an out-of-bounds panic
    // when later code indexes energy[i]/wavefronts.row(i) for i in 0..len.
    #[test]
    fn load_rejects_section_size_desync() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();
        for i in 0..6 {
            cm.store(&format!("desync probe {i}"), 0.8, &pipeline).unwrap();
        }
        // Drop one metadata entry without touching `len` (count()). The writer
        // serializes n=count() rows of energy/timestamps but only metadata.len()
        // metadata entries → the exact independent-section desync.
        let n_before = cm.right.count();
        cm.right.metadata.pop();
        assert_eq!(cm.right.count(), n_before);
        assert_eq!(cm.right.metadata.len(), n_before - 1);

        let dir = std::env::temp_dir();
        let path = dir.join("test_chiral_section_desync.hrm");
        cm.save(&path).unwrap();

        match ChiralMedium::load(&path) {
            Err(MediumError::CorruptHrm(_)) => {}
            other => panic!("expected CorruptHrm on section-size desync, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_rejects_tampered_file() {
        // Save a clean ChiralMedium, then flip one byte deep in the wavefront
        // section. Load must reject it via checksum verification rather than
        // crash mid-parse in `read_hemisphere`.
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();
        cm.store("checksum guard 1", 0.9, &pipeline).unwrap();
        cm.store("checksum guard 2", 0.5, &pipeline).unwrap();

        let dir = std::env::temp_dir();
        let path = dir.join("test_chiral_checksum_tampered.hrm");
        cm.save(&path).unwrap();

        // Sanity: clean file loads fine
        ChiralMedium::load(&path).unwrap();

        // Flip a byte in the middle of the file (not in the trailing checksum)
        let mut buf = std::fs::read(&path).unwrap();
        let flip_at = buf.len() / 2;
        buf[flip_at] ^= 0xff;
        std::fs::write(&path, &buf).unwrap();

        match ChiralMedium::load(&path) {
            Err(MediumError::ChecksumMismatch) => {}
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_load_roundtrip() {
        let mut cm = ChiralMedium::new();
        let pipeline = test_pipeline();

        cm.store("roundtrip test 1", 0.9, &pipeline).unwrap();
        cm.store("roundtrip test 2", 0.7, &pipeline).unwrap();
        cm.store("roundtrip test 3", 0.5, &pipeline).unwrap();

        let dir = std::env::temp_dir();
        let path = dir.join("test_chiral_roundtrip.hrm");

        // Save
        cm.save(&path).unwrap();

        // Load
        let loaded = ChiralMedium::load(&path).unwrap();

        // Verify counts
        assert_eq!(loaded.left.count(), cm.left.count());
        assert_eq!(loaded.right.count(), cm.right.count());
        assert_eq!(loaded.scales.len(), cm.scales.len());
        assert_eq!(loaded.left_to_right.len(), cm.left_to_right.len());

        // Verify energies match
        for i in 0..loaded.right.count() {
            assert!((loaded.right.energy[i] - cm.right.energy[i]).abs() < 0.001,
                "Right energy mismatch at {i}");
        }
        for i in 0..loaded.left.count() {
            assert!((loaded.left.energy[i] - cm.left.energy[i]).abs() < 0.001,
                "Left energy mismatch at {i}");
        }

        // Verify recall still works
        let results = loaded.recall("roundtrip test", 5, &pipeline).unwrap();
        assert!(!results.is_empty(), "Should recall stored memories after reload");

        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn v1_backward_compatibility() {
        // Create a v1 Medium and save it
        let mut medium = Medium::new();
        let pipeline = test_pipeline();
        medium.store("v1 memory", 0.8, &pipeline).unwrap();

        let dir = std::env::temp_dir();
        let path = dir.join("test_v1_compat.hrm");
        medium.save(&path).unwrap();

        // Load as ChiralMedium — should auto-detect v1 and convert
        let cm = ChiralMedium::load(&path).unwrap();

        assert_eq!(cm.right.count(), 1, "v1 memory should be in right hemisphere");
        assert_eq!(cm.left.count(), 0, "Left should be empty after v1 migration");
        assert!(cm.scales.contains_key(&cm.right.metadata[0].id));

        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_empty_chiral() {
        let cm = ChiralMedium::new();
        let dir = std::env::temp_dir();
        let path = dir.join("test_empty_chiral.hrm");

        cm.save(&path).unwrap();
        let loaded = ChiralMedium::load(&path).unwrap();

        assert_eq!(loaded.left.count(), 0);
        assert_eq!(loaded.right.count(), 0);

        let _ = std::fs::remove_file(&path);
    }
}
