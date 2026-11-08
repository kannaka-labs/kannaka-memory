//! #1156 — `kannaka digest`: a public, read-only commitment to the memory set.
//!
//! A digest says "these are the memories the store held when it was read",
//! in a form that can be published without revealing any memory's text, and
//! that can be balanced against the `KANNAKA.events.memory.<agent>.*` stream:
//! between two digests, every id that appeared needs a `remember` event and
//! every id that disappeared needs a `forget` event. Whatever does not
//! balance is a write the log missed.
//!
//! The byte layout is specified in `docs/digest-v1.md`, with golden vectors in
//! `tests/digest_v1_vectors.json` and an independent, standard-library Python
//! verifier in `scripts/verify_digest.py`. In short:
//!
//! - **What is committed.** The rows `memory_cache` is built from (the right
//!   hemisphere's metadata, last row wins per id) that [`is_committed_memory`]
//!   accepts: not an ADR-0049 facet, not a `__consolidation*` bookkeeping row.
//!   The remember publisher uses the same predicate, so a digest counts
//!   exactly the rows that can ever be announced.
//! - **Leaf** `sha256(0x00 || id[16] || sha256(content)[32])`. The id is in
//!   RFC 4122 byte order (Python's `uuid.bytes`). Wave state, timestamps and
//!   `origin_agent` are left out on purpose: they change on every dream or
//!   are not persisted, and a leaf must be rebuildable from a `remember`
//!   event alone (`memory_id` + `content_sha256`).
//! - **Order.** Leaves sorted by id bytes.
//! - **`merkle_root`** `sha256(0x02 || count_le_u64 || tree)`. Internal node
//!   `sha256(0x01 || L || R)`; an odd last node is promoted, not duplicated;
//!   the tree of zero leaves is 32 zero bytes. Committing the count lets a
//!   later proof show a memory is absent.
//! - **`ids_root`** the same construction over id leaves
//!   `sha256(0x03 || id[16])`. This is the root an outsider can recompute:
//!   ids are random v4 UUIDs that the `ids`-level events already publish, so
//!   publishing the sorted id list reveals no content.
//! - **`set_hash`** `sha256("kannaka-digest/v1\n" || count_le_u64 || leaves)`,
//!   a flat hash for anyone who holds the full leaf list.
//!
//! The leaf list (`id`, `content_sha256`) is for local use only. Publishing it
//! would let anyone confirm a guessed short text for memories written before
//! #1057, whose hashes were never public.
//!
//! **What this does and does not catch.** v1 catches bugs and write paths
//! that do not log. It does not stop a dishonest operator, who controls both
//! the store and the event stream; signing digests with Kannaka's identity
//! key and publishing them somewhere she does not control are follow-ups.
//!
//! **Read-only.** [`digest_hrm_file`] reads the `.hrm` with
//! `ChiralMedium::load` and nothing else: no `HrmStore`, so no temp-litter
//! sweep, no sidecar loads, no encoder stamp, no `Drop` autosave, no NATS.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::medium::WavefrontMeta;

/// Schema tag carried in the JSON and hashed into `set_hash`.
pub const SCHEMA: &str = "kannaka-digest/v1";

/// Whether this build publishes a `forget` event for every row a delete
/// removes. Until it does, every delete (CLI forget, dream prune, dedupe,
/// rollback) shows up as an unexplained shortfall between two digests, and
/// the digest output says the log is not expected to balance. The forget
/// lane flips this when deletes emit `MemoryForget`.
pub const FORGET_EVENTS_EMITTED: bool = false;

const TAG_LEAF: u8 = 0x00;
const TAG_NODE: u8 = 0x01;
const TAG_COUNT: u8 = 0x02;
const TAG_ID_LEAF: u8 = 0x03;

/// The one definition of "a memory the event log is responsible for".
///
/// ADR-0049 facet rows are derived from a parent and `__consolidation*` rows
/// are a dream's layer-summary bookkeeping; neither is ever announced by a
/// `remember` event. The digest, the remember publisher and the forget
/// publisher must all use this predicate, or a digest can never balance
/// against the log (a facet counted here with no event to explain it, or a
/// forget for a summary row nobody announced).
pub fn is_committed_memory(content: &str, is_facet: bool) -> bool {
    !is_facet && !content.starts_with("__consolidation")
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// Raw SHA-256 of a memory's content (the bytes behind the hex
/// `content_sha256` a `remember` event carries).
pub fn content_sha256_bytes(content: &str) -> [u8; 32] {
    sha256(&[content.as_bytes()])
}

/// One memory's leaf: `sha256(0x00 || id || content_sha256)`.
pub fn leaf_hash(id: &Uuid, content_sha256: &[u8; 32]) -> [u8; 32] {
    sha256(&[&[TAG_LEAF], id.as_bytes(), content_sha256])
}

/// One id's leaf in the id tree: `sha256(0x03 || id)`.
pub fn id_leaf_hash(id: &Uuid) -> [u8; 32] {
    sha256(&[&[TAG_ID_LEAF], id.as_bytes()])
}

/// Root of the binary tree over `leaves` (already in order). Odd last node is
/// promoted; zero leaves give 32 zero bytes.
pub fn tree_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            match pair {
                [l, r] => next.push(sha256(&[&[TAG_NODE], l, r])),
                [only] => next.push(*only),
                _ => unreachable!(),
            }
        }
        level = next;
    }
    level[0]
}

/// `sha256(0x02 || count_le_u64 || root)`.
fn commit_count(count: u64, root: &[u8; 32]) -> [u8; 32] {
    sha256(&[&[TAG_COUNT], &count.to_le_bytes(), root])
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The commitment over one memory set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDigest {
    pub count: u64,
    pub set_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub ids_root: [u8; 32],
    /// `(id, content_sha256)` sorted by id bytes. Local use only.
    pub leaves: Vec<(Uuid, [u8; 32])>,
}

impl MemoryDigest {
    /// Digest a set of `(id, content_sha256)` rows. The order of the input
    /// does not matter. Ids must be unique; a repeated id keeps the last row
    /// (callers that read a store resolve duplicates first, in file order).
    pub fn from_leaves<I: IntoIterator<Item = (Uuid, [u8; 32])>>(rows: I) -> Self {
        let sorted: BTreeMap<[u8; 16], [u8; 32]> =
            rows.into_iter().map(|(id, c)| (*id.as_bytes(), c)).collect();
        let count = sorted.len() as u64;
        let mut flat = Sha256::new();
        flat.update(SCHEMA.as_bytes());
        flat.update(b"\n");
        flat.update(count.to_le_bytes());
        let mut leaves = Vec::with_capacity(sorted.len());
        let mut id_leaves = Vec::with_capacity(sorted.len());
        let mut rows_out = Vec::with_capacity(sorted.len());
        for (idb, c) in &sorted {
            let id = Uuid::from_bytes(*idb);
            let leaf = leaf_hash(&id, c);
            flat.update(leaf);
            leaves.push(leaf);
            id_leaves.push(id_leaf_hash(&id));
            rows_out.push((id, *c));
        }
        Self {
            count,
            set_hash: flat.finalize().into(),
            merkle_root: commit_count(count, &tree_root(&leaves)),
            ids_root: commit_count(count, &tree_root(&id_leaves)),
            leaves: rows_out,
        }
    }

    /// Digest `(id, content)` rows.
    pub fn from_contents<'a, I: IntoIterator<Item = (Uuid, &'a str)>>(rows: I) -> Self {
        Self::from_leaves(rows.into_iter().map(|(id, c)| (id, content_sha256_bytes(c))))
    }

    /// Committed ids in digest order.
    pub fn ids(&self) -> Vec<Uuid> {
        self.leaves.iter().map(|(id, _)| *id).collect()
    }
}

/// Rows present in the store but outside the commitment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Excluded {
    pub facets: u64,
    pub bookkeeping: u64,
}

/// Digest a hemisphere's metadata the way `HrmStore::rebuild_cache` reads it:
/// iterate in file order, last row wins per id, then keep committed rows.
pub fn digest_metadata(metas: &[WavefrontMeta]) -> (MemoryDigest, Excluded) {
    let mut by_id: HashMap<Uuid, &WavefrontMeta> = HashMap::with_capacity(metas.len());
    for m in metas {
        by_id.insert(m.id, m);
    }
    let mut ex = Excluded::default();
    let mut rows = Vec::with_capacity(by_id.len());
    for (id, m) in by_id {
        if is_committed_memory(&m.content, m.is_facet) {
            rows.push((id, content_sha256_bytes(&m.content)));
        } else if m.is_facet {
            ex.facets += 1;
        } else {
            ex.bookkeeping += 1;
        }
    }
    (MemoryDigest::from_leaves(rows), ex)
}

/// Which file state a digest read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub hrm_size: u64,
    /// Modification time of the file that was read, RFC 3339 UTC.
    pub hrm_mtime: String,
    /// #984 content id (blake3 over the file minus its save timestamp).
    pub hrm_content_id: String,
}

fn file_stamp(path: &Path) -> std::io::Result<(u64, std::time::SystemTime)> {
    let md = std::fs::metadata(path)?;
    Ok((md.len(), md.modified()?))
}

/// How many times [`digest_hrm_file`] re-reads a file that changed under it.
pub const READ_ATTEMPTS: usize = 3;

/// Digest a `.hrm` file without opening it as a store.
///
/// Saves replace the file by atomic rename, so a read that starts and ends on
/// the same `(size, mtime)` read one consistent save. If a save lands during
/// the read, the read is retried, up to [`READ_ATTEMPTS`] times.
pub fn digest_hrm_file(path: &Path) -> Result<(MemoryDigest, Excluded, SourceInfo), String> {
    if !path.is_file() {
        return Err(format!("no store file at {}", path.display()));
    }
    for _ in 0..READ_ATTEMPTS {
        let before = file_stamp(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        let content_id = crate::medium::chiral_persistence::content_digest(path)
            .map_err(|e| format!("content id of {}: {e}", path.display()))?;
        let chiral = crate::medium::chiral::ChiralMedium::load(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let after = file_stamp(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        if before != after {
            continue;
        }
        let (digest, excluded) = digest_metadata(&chiral.right.metadata);
        let mtime: chrono::DateTime<chrono::Utc> = after.1.into();
        return Ok((
            digest,
            excluded,
            SourceInfo {
                hrm_size: after.0,
                hrm_mtime: mtime.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                hrm_content_id: content_id,
            },
        ));
    }
    Err(format!(
        "{} changed during each of {READ_ATTEMPTS} reads; a writer is saving continuously, try again",
        path.display()
    ))
}

/// What the reader needs to know about whether the event log is expected to
/// account for every change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogContext {
    /// The agent whose `KANNAKA.events.memory.<agent>.*` lane to read.
    pub agent_id: String,
    /// Configured `[events] remember` level, or `"default"` (per-origin).
    pub remember_level: String,
}

impl LogContext {
    /// Remember events are published at all for this configuration.
    pub fn remember_events_on(&self) -> bool {
        !self.agent_id.is_empty() && self.remember_level != "off"
    }
}

/// The JSON document `kannaka digest --json` prints.
///
/// No paths, hostnames or user names: `store_label` is the file name only.
#[allow(clippy::too_many_arguments)]
pub fn report_json(
    digest: &MemoryDigest,
    excluded: &Excluded,
    source: &SourceInfo,
    ctx: &LogContext,
    store_label: &str,
    computed_at: chrono::DateTime<chrono::Utc>,
    include_ids: bool,
    include_leaves: bool,
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "schema": SCHEMA,
        "store_label": store_label,
        "agent_id": ctx.agent_id,
        "computed_at": computed_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "count": digest.count,
        "set_hash": hex(&digest.set_hash),
        "merkle_root": hex(&digest.merkle_root),
        "ids_root": hex(&digest.ids_root),
        "excluded": { "facets": excluded.facets, "bookkeeping": excluded.bookkeeping },
        "source": {
            "hrm_size": source.hrm_size,
            "hrm_mtime": source.hrm_mtime,
            "hrm_content_id": source.hrm_content_id,
        },
        "log": {
            "remember_level": ctx.remember_level,
            "agent_id_set": !ctx.agent_id.is_empty(),
            "remember_events": ctx.remember_events_on(),
            "forget_events": FORGET_EVENTS_EMITTED,
            "expected_to_balance": ctx.remember_events_on() && FORGET_EVENTS_EMITTED,
        },
    });
    if include_ids {
        v["ids"] = serde_json::Value::from(
            digest.ids().iter().map(|i| i.to_string()).collect::<Vec<_>>(),
        );
    }
    if include_leaves {
        v["leaves_private"] = serde_json::Value::from(
            digest
                .leaves
                .iter()
                .map(|(id, c)| serde_json::json!({ "id": id.to_string(), "content_sha256": hex(c) }))
                .collect::<Vec<_>>(),
        );
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Uuid {
        s.parse().unwrap()
    }

    fn three() -> Vec<(Uuid, &'static str)> {
        vec![
            (id("6f1c2b4e-8d3a-4f5b-9c7e-0a1b2c3d4e5f"), "the heron stands in the shallows"),
            (id("0b9e7d6c-5a4f-4e3d-8c2b-1a0f9e8d7c6b"), "Nick said the radio is hers to run"),
            (id("f0e1d2c3-b4a5-4968-8776-655443322110"), "a third memory"),
        ]
    }

    #[test]
    fn same_rows_same_digest() {
        assert_eq!(MemoryDigest::from_contents(three()), MemoryDigest::from_contents(three()));
    }

    #[test]
    fn input_order_does_not_matter() {
        let mut rev = three();
        rev.reverse();
        let mut rot = three();
        rot.rotate_left(1);
        let a = MemoryDigest::from_contents(three());
        assert_eq!(a, MemoryDigest::from_contents(rev));
        assert_eq!(a, MemoryDigest::from_contents(rot));
    }

    #[test]
    fn adding_a_memory_changes_every_root() {
        let a = MemoryDigest::from_contents(three());
        let mut more = three();
        more.push((Uuid::new_v4(), "one more"));
        let b = MemoryDigest::from_contents(more);
        assert_eq!(b.count, a.count + 1);
        assert_ne!(a.set_hash, b.set_hash);
        assert_ne!(a.merkle_root, b.merkle_root);
        assert_ne!(a.ids_root, b.ids_root);
    }

    #[test]
    fn same_id_different_text_changes_content_roots_not_ids_root() {
        let a = MemoryDigest::from_contents(three());
        let mut edited = three();
        edited[0].1 = "the heron left";
        let b = MemoryDigest::from_contents(edited);
        assert_ne!(a.merkle_root, b.merkle_root);
        assert_ne!(a.set_hash, b.set_hash);
        assert_eq!(a.ids_root, b.ids_root);
    }

    #[test]
    fn empty_set_is_a_fixed_literal() {
        let e = MemoryDigest::from_leaves(std::iter::empty());
        // sha256(0x02 || 0u64 || [0; 32])
        let want = "c6c0aa07f27493d2f2e5cff56c890a353a20086d6c25ec825128e12ae752b2d9";
        assert_eq!(hex(&e.merkle_root), want);
        assert_eq!(hex(&e.ids_root), want);
        assert_eq!(e.count, 0);
    }

    #[test]
    fn odd_node_is_promoted_not_duplicated() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];
        let ab = sha256(&[&[TAG_NODE], &a, &b]);
        assert_eq!(tree_root(&[a, b, c]), sha256(&[&[TAG_NODE], &ab, &c]));
        assert_eq!(tree_root(&[a]), a);
        // Duplicating the last leaf (Bitcoin-style) must NOT collide.
        assert_ne!(tree_root(&[a, b, c]), tree_root(&[a, b, c, c]));
    }

    /// The vectors the Python reference verifier also checks.
    #[test]
    fn golden_vectors_match() {
        let raw = include_str!("../tests/digest_v1_vectors.json");
        let doc: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(doc["schema"], SCHEMA);
        let vectors = doc["vectors"].as_array().unwrap();
        assert!(vectors.len() >= 4);
        for v in vectors {
            let rows: Vec<(Uuid, String)> = v["memories"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| (id(m["id"].as_str().unwrap()), m["content"].as_str().unwrap().to_string()))
                .collect();
            let d = MemoryDigest::from_contents(rows.iter().map(|(i, c)| (*i, c.as_str())));
            let name = v["name"].as_str().unwrap();
            assert_eq!(d.count, v["count"].as_u64().unwrap(), "{name}");
            assert_eq!(hex(&d.set_hash), v["set_hash"].as_str().unwrap(), "{name} set_hash");
            assert_eq!(hex(&d.merkle_root), v["merkle_root"].as_str().unwrap(), "{name} merkle_root");
            assert_eq!(hex(&d.ids_root), v["ids_root"].as_str().unwrap(), "{name} ids_root");
        }
    }

    /// An outsider rebuilds a leaf from a `remember` event alone.
    #[cfg(feature = "nats")]
    #[test]
    fn leaf_rebuilds_from_a_remember_event() {
        use crate::remember_events::{build_remember_event, RememberLevel, VIA_DREAM};
        let (mid, text) = three()[1];
        let (_, payload) =
            build_remember_event(RememberLevel::Ids, "kannaka", &mid, text, 0.5, "text", VIA_DREAM)
                .unwrap();
        assert!(payload.get("content").is_none());
        let ev_id = id(payload["memory_id"].as_str().unwrap());
        let ev_sha_hex = payload["content_sha256"].as_str().unwrap();
        let mut ev_sha = [0u8; 32];
        for (i, b) in ev_sha.iter_mut().enumerate() {
            *b = u8::from_str_radix(&ev_sha_hex[2 * i..2 * i + 2], 16).unwrap();
        }
        let d = MemoryDigest::from_contents(three());
        let (sid, ssha) = d.leaves.iter().find(|(i, _)| *i == mid).unwrap();
        assert_eq!(leaf_hash(&ev_id, &ev_sha), leaf_hash(sid, ssha));
    }

    #[test]
    fn facets_and_bookkeeping_are_excluded_and_last_row_wins() {
        let a = Uuid::new_v4();
        let mut facet = WavefrontMeta::new(Uuid::new_v4(), "a facet of a".into());
        facet.is_facet = true;
        let summary = WavefrontMeta::new(Uuid::new_v4(), "__consolidation_summary_L1".into());
        let metas = vec![
            WavefrontMeta::new(a, "first write".into()),
            facet,
            summary,
            WavefrontMeta::new(a, "kept".into()),
        ];
        let (d, ex) = digest_metadata(&metas);
        assert_eq!(ex, Excluded { facets: 1, bookkeeping: 1 });
        assert_eq!(d, MemoryDigest::from_contents([(a, "kept")]));
    }

    #[test]
    fn report_has_no_paths_and_says_the_log_cannot_balance_yet() {
        let d = MemoryDigest::from_contents(three());
        let src = SourceInfo {
            hrm_size: 1,
            hrm_mtime: "2026-11-08T00:00:00.000Z".into(),
            hrm_content_id: "00".into(),
        };
        let ctx = LogContext { agent_id: "kannaka".into(), remember_level: "default".into() };
        let now = chrono::Utc::now();
        let j = report_json(&d, &Excluded::default(), &src, &ctx, "kannaka.hrm", now, true, false);
        assert_eq!(j["schema"], SCHEMA);
        assert_eq!(j["count"], 3);
        assert_eq!(j["ids"].as_array().unwrap().len(), 3);
        assert!(j.get("leaves_private").is_none());
        assert_eq!(j["log"]["remember_events"], true);
        assert_eq!(j["log"]["expected_to_balance"], FORGET_EVENTS_EMITTED);
        let s = j.to_string();
        assert!(!s.contains("/home/") && !s.contains(":\\\\"), "no paths: {s}");
        let off = LogContext { agent_id: String::new(), remember_level: "default".into() };
        let j = report_json(&d, &Excluded::default(), &src, &off, "kannaka.hrm", now, false, true);
        assert_eq!(j["log"]["remember_events"], false);
        assert!(j.get("ids").is_none());
        assert_eq!(j["leaves_private"].as_array().unwrap().len(), 3);
    }

    // ── against a real store file ─────────────────────────────────────

    use crate::codebook::Codebook;
    use crate::encoding::{EncodingPipeline, SimpleHashEncoder};
    use crate::hrm_store::HrmStore;
    use crate::medium::WAVEFRONT_DIM;
    use crate::memory::HyperMemory;
    use crate::store::MediumBackend;

    fn pipeline() -> EncodingPipeline {
        EncodingPipeline::new(
            Box::new(SimpleHashEncoder::new(384, 42)),
            Codebook::new(384, WAVEFRONT_DIM, 42),
        )
    }

    fn insert(store: &mut HrmStore, text: &str) -> Uuid {
        let v = pipeline().encode_text(text).unwrap();
        store.insert(HyperMemory::new(v, text.to_string())).unwrap()
    }

    /// Every file in `dir` with its bytes and mtime.
    fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, std::time::SystemTime)> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let md = e.metadata().unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    (std::fs::read(e.path()).unwrap(), md.modified().unwrap()),
                )
            })
            .collect()
    }

    #[test]
    fn store_file_digest_tracks_add_and_forget_and_ignores_wave_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kannaka.hrm");
        let mut store = HrmStore::new(pipeline(), path.clone());
        let a = insert(&mut store, "the heron stands in the shallows");
        let b = insert(&mut store, "the tide turns at noon");
        store.flush().unwrap();
        let (d1, ex, src) = digest_hrm_file(&path).unwrap();
        assert_eq!(ex, Excluded::default());
        assert_eq!(
            d1,
            MemoryDigest::from_contents([
                (a, "the heron stands in the shallows"),
                (b, "the tide turns at noon"),
            ])
        );
        assert_eq!(src.hrm_size, std::fs::metadata(&path).unwrap().len());

        // Read-only and wave-only operations: same memory set, same digest.
        let _ = store.recall_resonance_readonly("heron", 2).unwrap();
        let _ = store.search(&store.get(&a).unwrap().unwrap().vector.clone(), 2).unwrap();
        let _ = store.all_memories().unwrap();
        if let Some(m) = store.get_mut(&b).unwrap() {
            m.amplitude *= 0.5; // a boost/decay changes wave state, not the set
        }
        store.flush().unwrap();
        let (d_ro, _, _) = digest_hrm_file(&path).unwrap();
        assert_eq!(d_ro, d1, "wave state and reads must not move the digest");

        // Add.
        let c = insert(&mut store, "a third, later memory");
        store.flush().unwrap();
        let (d2, _, _) = digest_hrm_file(&path).unwrap();
        assert_eq!(d2.count, 3);
        assert_ne!(d2.merkle_root, d1.merkle_root);

        // Forget: equal to a store that never had it.
        assert!(store.delete(&c).unwrap());
        store.flush().unwrap();
        let (d3, _, _) = digest_hrm_file(&path).unwrap();
        assert_eq!(d3, d1, "after forget the digest equals the store without it");
        drop(store);
    }

    #[test]
    fn digesting_a_store_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kannaka.hrm");
        {
            let mut store = HrmStore::new(pipeline(), path.clone());
            insert(&mut store, "a memory to read");
            store.flush().unwrap();
        }
        // A stale temp file a store load would sweep (#934) must survive.
        let litter = dir.path().join(".kannaka-tmp-stale");
        std::fs::write(&litter, b"x").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        std::fs::File::options().write(true).open(&litter).unwrap().set_modified(old).unwrap();
        let before = snapshot(dir.path());
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _ = digest_hrm_file(&path).unwrap();
        let _ = digest_hrm_file(&path).unwrap();
        assert_eq!(snapshot(dir.path()), before, "digest touched the data dir");
    }

    #[test]
    fn missing_store_is_an_error_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kannaka.hrm");
        assert!(digest_hrm_file(&path).is_err());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }
}
