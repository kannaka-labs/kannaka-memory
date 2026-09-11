//! Provenance — ed25519 signing/verification substrate for swarm memory.
//!
//! Increment-1a (crypto substrate ONLY). This module adds the identity /
//! attribution / integrity primitives the swarm needs to prove *who* signed a
//! wire memory or phase update, and to reject replays — **without** changing
//! any absorb behaviour. It defines:
//!
//! - [`ProvenanceSig`] — the optional `"provenance_sig"` wire key: alg +
//!   ed25519 pubkey + per-message nonce + timestamp + 64-byte signature. The
//!   fixed-width byte fields are base64-encoded on the JSON wire.
//! - Domain-separated, length-prefixed canonical byte builders
//!   ([`canonical_mem`], [`canonical_phase`]) — the EXACT bytes that get
//!   signed. Domain separation (distinct `DOMAIN_*` prefixes) makes a
//!   signature minted for one statement type fail verification as any other.
//! - [`sign_mem`] / [`verify_mem`] — sign and verify a memory statement.
//!   `verify_mem` is PURE: it never reads the clock; the caller passes
//!   `now_ms` so tests are deterministic.
//! - [`ReplayLru`] — a bounded, fail-closed `(memory_id, nonce)` replay set.
//! - [`node_signing_key`] — a per-node ed25519 seed at rest
//!   (`<data_dir>/node_key.ed25519`, `0600` on unix, best-effort ACL on
//!   Windows).
//!
//! **Trust/enrollment/reputation lives elsewhere** (lands after a design
//! review): this module knows how to *verify a signature over exact bytes*,
//! not whether a key is *trusted*. `verify_mem` returns the verified pubkey;
//! deciding what that pubkey is allowed to do is a separate layer.
//!
//! Byte layouts are frozen by inc-1 synthesis §2.1 — do not reorder fields.

use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// ed25519 signature algorithm tag carried in [`ProvenanceSig::alg`].
pub const PROV_ALG_ED25519: u8 = 1;

/// Accepted clock skew for a signed timestamp, in milliseconds (±5 min).
/// A `ts_ms` outside `[now - SKEW_MS, now + SKEW_MS]` is [`VerifyErr::Stale`].
pub const SKEW_MS: i64 = 300_000;

/// Hard bound on the [`ReplayLru`] `(id, nonce)` set before low-water
/// eviction of entries older than `now - SKEW_MS` kicks in.
pub const REPLAY_CAP: usize = 100_000;

// Domain-separation prefixes (inc-1 synthesis §2.1). Each is
// `b"kannaka/prov/<x>/v1\0"`. The trailing NUL is part of the signed bytes.
/// Domain prefix for a memory statement ([`canonical_mem`]).
pub const DOMAIN_MEM: &[u8] = b"kannaka/prov/mem/v1\0";
/// Domain prefix for a phase / metrics statement ([`canonical_phase`]).
pub const DOMAIN_PHASE: &[u8] = b"kannaka/prov/phase/v1\0";
/// Domain prefix for a pubkey↔SSO-sub binding statement.
pub const DOMAIN_BIND: &[u8] = b"kannaka/prov/bind/v1\0";
/// Domain prefix for a key-rotation statement.
pub const DOMAIN_ROT: &[u8] = b"kannaka/prov/rotate/v1\0";
/// Domain prefix for a qos boot-attestation statement.
pub const DOMAIN_BOOT: &[u8] = b"kannaka/prov/boot/v1\0";
/// Domain prefix for a `.hrm` at-rest authenticity signature.
pub const DOMAIN_HRM: &[u8] = b"kannaka/prov/hrm/v1\0";
/// Domain prefix for a heartbeat beacon statement ([`canonical_beacon`]).
pub const DOMAIN_BEACON: &[u8] = b"kannaka/prov/beacon/v1\0";
/// Domain prefix for a reputation-store snapshot at-rest signature
/// ([`sign_snapshot`] / [`verify_snapshot`]).
pub const DOMAIN_SNAPSHOT: &[u8] = b"kannaka/prov/repsnap/v1\0";

// ---------------------------------------------------------------------------
// ProvenanceSig
// ---------------------------------------------------------------------------

/// A detached provenance signature that rides as the OPTIONAL JSON key
/// `"provenance_sig"` alongside a wire memory / phase payload.
///
/// The `pubkey`, `nonce`, and `sig` byte arrays are base64-encoded on the
/// JSON wire (see the hand-rolled `Serialize`/`Deserialize` impls) — the
/// crate has no `base64` dependency, so this mirrors the standard-alphabet
/// codec in `nats.rs` rather than pulling one in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceSig {
    /// Signature algorithm tag; only [`PROV_ALG_ED25519`] is understood.
    pub alg: u8,
    /// The signer's ed25519 verifying (public) key.
    pub pubkey: [u8; 32],
    /// Per-message nonce — bound into the signed bytes and used as the
    /// replay key together with the memory id.
    pub nonce: [u8; 16],
    /// Signing time, unix milliseconds. Checked against `now ± SKEW_MS`.
    pub ts_ms: i64,
    /// The 64-byte ed25519 signature over the canonical bytes.
    pub sig: [u8; 64],
}

/// On-wire shape of [`ProvenanceSig`] — byte arrays as base64 strings.
#[derive(Serialize, Deserialize)]
struct ProvenanceSigWire {
    alg: u8,
    pubkey: String,
    nonce: String,
    ts_ms: i64,
    sig: String,
}

impl Serialize for ProvenanceSig {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        ProvenanceSigWire {
            alg: self.alg,
            pubkey: b64_encode(&self.pubkey),
            nonce: b64_encode(&self.nonce),
            ts_ms: self.ts_ms,
            sig: b64_encode(&self.sig),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for ProvenanceSig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = ProvenanceSigWire::deserialize(d)?;
        Ok(ProvenanceSig {
            alg: w.alg,
            pubkey: b64_decode_arr::<32>(&w.pubkey).map_err(serde::de::Error::custom)?,
            nonce: b64_decode_arr::<16>(&w.nonce).map_err(serde::de::Error::custom)?,
            ts_ms: w.ts_ms,
            sig: b64_decode_arr::<64>(&w.sig).map_err(serde::de::Error::custom)?,
        })
    }
}

// ---------------------------------------------------------------------------
// VerifyErr
// ---------------------------------------------------------------------------

/// Why a provenance check was rejected. `KeyMismatch`/`Revoked` are produced
/// by the (later) trust layer, not by the pure signature checks here — they
/// are part of the shared vocabulary so callers have one error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyErr {
    /// `alg` is not [`PROV_ALG_ED25519`].
    BadAlg,
    /// The ed25519 signature did not verify over the canonical bytes (also
    /// covers a malformed pubkey point). Domain/field mismatches surface
    /// here too — a valid signature over *different* bytes is `BadSig`.
    BadSig,
    /// `ts_ms` is outside `now ± SKEW_MS`.
    Stale,
    /// `(memory_id, nonce)` was already seen inside the replay window.
    Replay,
    /// Embedded pubkey is not the enrolled key for the claimed agent
    /// (trust-layer only; unused by the pure checks in 1a).
    KeyMismatch,
    /// The signing key has been revoked (trust-layer only; unused in 1a).
    Revoked,
}

impl std::fmt::Display for VerifyErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::BadAlg => "unsupported signature algorithm",
            Self::BadSig => "signature verification failed",
            Self::Stale => "timestamp outside accepted skew",
            Self::Replay => "replayed (memory_id, nonce)",
            Self::KeyMismatch => "pubkey is not the enrolled key",
            Self::Revoked => "signing key is revoked",
        };
        f.write_str(s)
    }
}

impl std::error::Error for VerifyErr {}

// ---------------------------------------------------------------------------
// Canonical bytes (inc-1 synthesis §2.1)
// ---------------------------------------------------------------------------

/// Length-prefixed field: `u32le(len) ++ f`. Variable-width fields go
/// through `put` so a boundary shift (e.g. moving a byte from `agent_id` to
/// `subject`) changes the bytes and thus the signature.
fn put(b: &mut Vec<u8>, f: &[u8]) {
    b.extend_from_slice(&(f.len() as u32).to_le_bytes());
    b.extend_from_slice(f);
}

/// Canonical signed bytes for a memory statement:
///
/// ```text
/// DOMAIN_MEM ‖ put(agent_id) ‖ memory_id[16] ‖ nonce[16] ‖ ts_ms[8le]
///            ‖ blake3(content)[32] ‖ put(subject) ‖ amp_q16[4le] ‖ tier[1]
/// ```
///
/// `amp_q16` is amplitude in Q16 fixed point (see [`amp_to_q16`]); `tier` is
/// the tier discriminant. Binding both closes the "amplitude/tier ride
/// unsigned" gap (inc-1 K1).
pub fn canonical_mem(
    agent_id: &str,
    memory_id: Uuid,
    nonce: &[u8; 16],
    ts_ms: i64,
    content: &str,
    subject: &str,
    amp_q16: u32,
    tier: u8,
) -> Vec<u8> {
    let mut b = Vec::with_capacity(DOMAIN_MEM.len() + agent_id.len() + subject.len() + 96);
    b.extend_from_slice(DOMAIN_MEM);
    put(&mut b, agent_id.as_bytes());
    b.extend_from_slice(memory_id.as_bytes()); // [u8; 16]
    b.extend_from_slice(nonce);
    b.extend_from_slice(&ts_ms.to_le_bytes());
    b.extend_from_slice(blake3::hash(content.as_bytes()).as_bytes()); // [u8; 32]
    put(&mut b, subject.as_bytes());
    b.extend_from_slice(&amp_q16.to_le_bytes());
    b.push(tier);
    b
}

/// Canonical signed bytes for a phase / metrics statement:
///
/// ```text
/// DOMAIN_PHASE ‖ put(agent_id) ‖ nonce[16] ‖ ts_ms[8le]
///              ‖ blake3(phase_json)[32] ‖ put(subject)
/// ```
pub fn canonical_phase(
    agent_id: &str,
    nonce: &[u8; 16],
    ts_ms: i64,
    phase_json: &str,
    subject: &str,
) -> Vec<u8> {
    let mut b = Vec::with_capacity(DOMAIN_PHASE.len() + agent_id.len() + subject.len() + 64);
    b.extend_from_slice(DOMAIN_PHASE);
    put(&mut b, agent_id.as_bytes());
    b.extend_from_slice(nonce);
    b.extend_from_slice(&ts_ms.to_le_bytes());
    b.extend_from_slice(blake3::hash(phase_json.as_bytes()).as_bytes()); // [u8; 32]
    put(&mut b, subject.as_bytes());
    b
}

/// Canonical signed bytes for a heartbeat beacon statement:
///
/// ```text
/// DOMAIN_BEACON ‖ epoch[8le] ‖ prev_reject_root[32]
/// ```
///
/// A seed signs one per epoch (see [`crate::beacon`]) so partitioned/eclipsed
/// nodes can detect the ABSENCE of fresh beacons and fail closed (freeze
/// promotion). `prev_reject_root` is a lightweight blake3 rolling stand-in for a
/// full Merkle root of recently operator-rejected mem-hashes — it is *bound into*
/// the signature so a beacon can't be replayed with a swapped reject summary.
pub fn canonical_beacon(epoch: u64, prev_reject_root: &[u8; 32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(DOMAIN_BEACON.len() + 8 + 32);
    b.extend_from_slice(DOMAIN_BEACON);
    b.extend_from_slice(&epoch.to_le_bytes());
    b.extend_from_slice(prev_reject_root);
    b
}

/// Amplitude → Q16 fixed point, the encoding bound into [`canonical_mem`].
/// Saturating so a NaN/negative/overflowing amplitude can't wrap.
pub fn amp_to_q16(amp: f32) -> u32 {
    let scaled = (amp as f64) * 65_536.0;
    if !scaled.is_finite() || scaled <= 0.0 {
        0
    } else if scaled >= u32::MAX as f64 {
        u32::MAX
    } else {
        scaled as u32
    }
}

// ---------------------------------------------------------------------------
// Sign / verify
// ---------------------------------------------------------------------------

/// Sign a memory statement with a raw 32-byte ed25519 seed. Returns the
/// detached [`ProvenanceSig`] (pubkey + nonce + ts_ms + signature).
#[allow(clippy::too_many_arguments)]
pub fn sign_mem(
    signing_seed: &[u8; 32],
    agent_id: &str,
    memory_id: Uuid,
    nonce: &[u8; 16],
    ts_ms: i64,
    content: &str,
    subject: &str,
    amp_q16: u32,
    tier: u8,
) -> ProvenanceSig {
    let sk = SigningKey::from_bytes(signing_seed);
    let msg = canonical_mem(agent_id, memory_id, nonce, ts_ms, content, subject, amp_q16, tier);
    let sig = sk.sign(&msg);
    ProvenanceSig {
        alg: PROV_ALG_ED25519,
        pubkey: sk.verifying_key().to_bytes(),
        nonce: *nonce,
        ts_ms,
        sig: sig.to_bytes(),
    }
}

/// Verify a memory statement's signature over its canonical bytes. PURE — it
/// does NOT read the clock or touch any replay set; the caller passes
/// `now_ms` and (separately) runs [`ReplayLru::check_and_insert`] *after*
/// this returns `Ok`.
///
/// Checks, in order: `alg == ED25519` (else [`VerifyErr::BadAlg`]); `ts_ms`
/// within `now_ms ± SKEW_MS` (else [`VerifyErr::Stale`]); ed25519 signature
/// valid over `canonical_mem(...)` reconstructed from the caller's fields and
/// the signed `nonce`/`ts_ms` (else [`VerifyErr::BadSig`]). On success returns
/// the verified signer pubkey — the trust layer decides what it may do.
#[allow(clippy::too_many_arguments)]
pub fn verify_mem(
    sig: &ProvenanceSig,
    agent_id: &str,
    memory_id: Uuid,
    content: &str,
    subject: &str,
    amp_q16: u32,
    tier: u8,
    now_ms: i64,
) -> Result<[u8; 32], VerifyErr> {
    if sig.alg != PROV_ALG_ED25519 {
        return Err(VerifyErr::BadAlg);
    }
    // i128 widening so a hostile ts_ms can't overflow the skew subtraction.
    if (now_ms as i128 - sig.ts_ms as i128).abs() > SKEW_MS as i128 {
        return Err(VerifyErr::Stale);
    }
    let vk = VerifyingKey::from_bytes(&sig.pubkey).map_err(|_| VerifyErr::BadSig)?;
    let signature = Signature::from_bytes(&sig.sig);
    let msg = canonical_mem(
        agent_id,
        memory_id,
        &sig.nonce,
        sig.ts_ms,
        content,
        subject,
        amp_q16,
        tier,
    );
    vk.verify_strict(&msg, &signature).map_err(|_| VerifyErr::BadSig)?;
    Ok(sig.pubkey)
}

// ---------------------------------------------------------------------------
// ReplayLru
// ---------------------------------------------------------------------------

/// Bounded, fail-closed replay set over `(memory_id, nonce)` pairs.
///
/// **Ordering contract:** the caller MUST verify the signature (and, later,
/// the enrolled-pubkey check) FIRST, then call [`check_and_insert`] — never
/// insert unverified input, or an attacker can pre-poison the set (inc-1
/// K7/A9). Insert stores `now_ms`; entries older than `now - SKEW_MS` are
/// evicted (they can no longer replay — they'd be [`VerifyErr::Stale`]).
///
/// [`check_and_insert`]: ReplayLru::check_and_insert
#[derive(Debug, Clone)]
pub struct ReplayLru {
    seen: std::collections::HashMap<(Uuid, [u8; 16]), i64>,
    cap: usize,
    /// Set when [`load`] hit a read/parse error. Fail-closed: the caller must
    /// refuse Live absorb until the poison window (`now - SKEW_MS` past the
    /// load time) elapses, rather than treating an empty set as "nothing
    /// seen" and reopening the replay window across a restart.
    ///
    /// [`load`]: ReplayLru::load
    poisoned: bool,
    /// Load time (ms) when `poisoned` was set; bounds the refuse window.
    poisoned_at_ms: Option<i64>,
}

impl Default for ReplayLru {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplayLru {
    /// A clean, empty, non-poisoned set with the default [`REPLAY_CAP`].
    pub fn new() -> Self {
        Self::with_capacity(REPLAY_CAP)
    }

    /// A clean, empty, non-poisoned set with an explicit cap (for tests).
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            seen: std::collections::HashMap::new(),
            cap: cap.max(1),
            poisoned: false,
            poisoned_at_ms: None,
        }
    }

    /// True if this set came from a failed [`load`](ReplayLru::load) and the
    /// caller must still refuse Live absorb.
    pub fn poisoned(&self) -> bool {
        self.poisoned
    }

    /// Whether the caller should refuse Live absorb right now: while poisoned
    /// AND still inside `poisoned_at + SKEW_MS`. After the window elapses the
    /// worst pre-restart replay would already be [`VerifyErr::Stale`], so the
    /// set can be trusted as effectively empty again.
    pub fn refuses_live(&self, now_ms: i64) -> bool {
        match (self.poisoned, self.poisoned_at_ms) {
            (true, Some(at)) => now_ms - at <= SKEW_MS,
            (true, None) => true,
            _ => false,
        }
    }

    /// Number of tracked `(id, nonce)` pairs.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// True when no pairs are tracked.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Record a freshly *verified* `(id, nonce)`; `Err(Replay)` if already
    /// seen within the window. Call ONLY after signature verification passes.
    pub fn check_and_insert(
        &mut self,
        id: Uuid,
        nonce: &[u8; 16],
        now_ms: i64,
    ) -> Result<(), VerifyErr> {
        let key = (id, *nonce);
        if self.seen.contains_key(&key) {
            return Err(VerifyErr::Replay);
        }
        if self.seen.len() >= self.cap {
            self.evict(now_ms);
        }
        self.seen.insert(key, now_ms);
        Ok(())
    }

    /// Low-water eviction: drop everything older than `now - SKEW_MS`; if the
    /// set is still at/over cap, bulk-evict the oldest 20% in one O(n log n)
    /// pass rather than the previous O(n²) one-at-a-time scan.
    fn evict(&mut self, now_ms: i64) {
        self.seen.retain(|_, &mut ts| now_ms - ts <= SKEW_MS);
        if self.seen.len() >= self.cap {
            let target = self.cap * 4 / 5;
            let to_drop = self.seen.len().saturating_sub(target);
            if to_drop > 0 {
                let mut by_age: Vec<_> = self.seen.iter().map(|(k, &ts)| (ts, *k)).collect();
                by_age.sort_unstable_by_key(|(ts, _)| *ts);
                for (_, k) in by_age.into_iter().take(to_drop) {
                    self.seen.remove(&k);
                }
            }
        }
    }

    /// Persist the set to `path` (atomic temp-write + fsync + rename, mirroring
    /// the crate's other sidecar writers). The poison flag is NOT persisted —
    /// a good file always loads clean.
    pub fn persist(&self, path: &Path) -> Result<(), String> {
        let wire = ReplayLruWire {
            entries: self
                .seen
                .iter()
                .map(|((id, nonce), ts)| ReplayEntryWire {
                    id: *id,
                    nonce: b64_encode(nonce),
                    ts: *ts,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&wire)
            .map_err(|e| format!("failed to serialize replay set: {e}"))?;
        atomic_write_bytes(path, &bytes)
    }

    /// Load the set from `path`. **Fail-closed:** a read or parse error
    /// returns an empty set flagged `poisoned` (with `poisoned_at = now_ms`)
    /// so [`refuses_live`](ReplayLru::refuses_live) holds until the window
    /// elapses — rather than silently returning an empty, trusting set. A
    /// missing file (first run) is a clean empty set, not a failure.
    pub fn load(path: &Path, now_ms: i64) -> ReplayLru {
        if !path.exists() {
            return ReplayLru::new();
        }
        let parsed = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<ReplayLruWire>(&b).ok());
        let wire = match parsed {
            Some(w) => w,
            None => {
                // Fail closed: empty but poisoned.
                let mut lru = ReplayLru::new();
                lru.poisoned = true;
                lru.poisoned_at_ms = Some(now_ms);
                return lru;
            }
        };
        let mut lru = ReplayLru::new();
        for e in wire.entries {
            if let Ok(nonce) = b64_decode_arr::<16>(&e.nonce) {
                // Drop already-stale entries on load. #11: widen to i128 so a
                // hostile persisted `ts` (e.g. `i64::MIN`) can't overflow the
                // subtraction (debug panic / release wrap) — mirrors verify_mem.
                if (now_ms as i128 - e.ts as i128) <= SKEW_MS as i128 {
                    lru.seen.insert((e.id, nonce), e.ts);
                }
            }
        }
        lru
    }
}

#[derive(Serialize, Deserialize)]
struct ReplayEntryWire {
    id: Uuid,
    nonce: String,
    ts: i64,
}

#[derive(Serialize, Deserialize)]
struct ReplayLruWire {
    entries: Vec<ReplayEntryWire>,
}

// ---------------------------------------------------------------------------
// Node key at rest
// ---------------------------------------------------------------------------

/// Load — or, on first call, generate and persist — this node's raw 32-byte
/// ed25519 signing seed at `<data_dir>/node_key.ed25519`.
///
/// On unix the file is `chmod 0600` (mirrors `IdentityStore::save`). On
/// Windows there is no `0600` equivalent; see [`restrict_key_permissions`].
/// A wrong-length existing file is treated as corrupt and regenerated.
pub fn node_signing_key(data_dir: &Path) -> Result<[u8; 32], String> {
    let path = data_dir.join("node_key.ed25519");
    if path.exists() {
        match std::fs::read(&path) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut seed = [0u8; 32];
                seed.copy_from_slice(&bytes);
                return Ok(seed);
            }
            Ok(_) => { /* wrong length — fall through and regenerate */ }
            Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
        }
    }
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("failed to create {}: {e}", data_dir.display()))?;
    let mut seed = [0u8; 32];
    fill_os_random(&mut seed);
    write_owner_only(&path, &seed)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    restrict_key_permissions(&path); // Windows ACL (+ redundant Unix re-tighten)
    Ok(seed)
}

/// The verifying (public) key bytes derived from a raw signing seed.
pub fn verifying_key_bytes(signing_seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(signing_seed).verifying_key().to_bytes()
}

/// This node's verifying (public) key IF its signing seed already exists on
/// disk — READ-ONLY: unlike [`node_signing_key`] it NEVER generates one
/// (finding #4). `None` when the key file is absent or the wrong length, so a
/// fail-closed loader that must verify a snapshot against its OWN key can treat
/// "no key" as "can't have signed this" ⇒ refuse.
pub fn node_verifying_key(data_dir: &Path) -> Option<[u8; 32]> {
    let path = data_dir.join("node_key.ed25519");
    let bytes = std::fs::read(&path).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Some(verifying_key_bytes(&seed))
}

/// The domain-separated message signed for a reputation snapshot:
/// `DOMAIN_SNAPSHOT ‖ blake3(snapshot_bytes)` (finding #4). Hashing keeps the
/// signed message small regardless of snapshot size while binding every byte.
fn snapshot_signing_msg(snapshot_bytes: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(DOMAIN_SNAPSHOT.len() + 32);
    msg.extend_from_slice(DOMAIN_SNAPSHOT);
    msg.extend_from_slice(blake3::hash(snapshot_bytes).as_bytes());
    msg
}

/// Sign a reputation-store snapshot's bytes with this node's raw ed25519 seed
/// (finding #4). Domain-separated (see [`snapshot_signing_msg`]) so the 64-byte
/// signature can't be lifted onto another statement type. The caller stores the
/// returned signature beside the snapshot and, on load, verifies it against the
/// node's OWN verifying key — a planted/foreign or tampered snapshot fails.
pub fn sign_snapshot(signing_seed: &[u8; 32], snapshot_bytes: &[u8]) -> [u8; 64] {
    let sk = SigningKey::from_bytes(signing_seed);
    sk.sign(&snapshot_signing_msg(snapshot_bytes)).to_bytes()
}

/// Verify a reputation-store snapshot signature (finding #4). Returns `true`
/// only when `sig` is a valid ed25519 signature by `pubkey` over the
/// domain-separated hash of `snapshot_bytes`. A malformed key/point ⇒ `false`
/// (fail-closed).
pub fn verify_snapshot(pubkey: &[u8; 32], snapshot_bytes: &[u8], sig: &[u8; 64]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    let signature = Signature::from_bytes(sig);
    vk.verify_strict(&snapshot_signing_msg(snapshot_bytes), &signature)
        .is_ok()
}

/// Fill a buffer with OS CSPRNG bytes. Uses `rand`'s `OsRng` (already a dep;
/// getrandom-backed) rather than `SigningKey::generate` so keygen doesn't
/// depend on an rng-trait version match.
fn fill_os_random(buf: &mut [u8]) {
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(buf);
}

/// Write `bytes` to `path` as an owner-only (0600) file on Unix, created that way
/// FROM THE START so a secret is never world-readable — closing the create→chmod
/// TOCTOU window that `std::fs::write` (0644 under umask 022) + a post-hoc
/// `chmod 0600` leaves open, and avoiding a permanently-exposed file when a
/// discarded post-hoc chmod silently fails. Also re-tightens a file that already
/// existed from an older 0644 write.
///
/// #930 / ADR-0059 §1: the write is temp-and-rename in the target's own
/// directory, never a truncate-in-place — a crash or a full disk mid-write
/// used to leave `config.toml` (and the identity, tokens and API key in it)
/// as an empty or half-written file. The 0600 temp sibling is created with
/// that mode and renamed over the target, so a reader sees the old file or
/// the new one, whole. On non-Unix the mode is ignored (Windows secret ACLs
/// are applied separately, e.g. via `restrict_key_permissions`).
pub(crate) fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::fs_util::atomic_write_bytes_mode(path, bytes, Some(OWNER_ONLY_MODE))
        .map_err(std::io::Error::other)
}

/// The unix mode every secret this module writes is created with. Named so
/// the test can assert both halves of the claim (#933): that `write_owner_only`
/// requests owner-only, AND that requesting it yields an owner-only file from
/// creation. Asserting only against the constant would be a check that moves
/// whenever the thing it checks moves.
pub(crate) const OWNER_ONLY_MODE: u32 = 0o600;

/// Restrict a key file to the current user. Unix: `chmod 0600`. Windows:
/// best-effort ACL via `icacls`.
fn restrict_key_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(windows)]
    {
        // SECURITY: Windows key-at-rest is best-effort ACL only (no 0600 equiv) — see inc-1 human-decision #5
        if let Ok(user) = std::env::var("USERNAME") {
            if !user.is_empty() {
                let _ = std::process::Command::new("icacls")
                    .arg(path)
                    .arg("/inheritance:r")
                    .arg("/grant:r")
                    .arg(format!("{user}:F"))
                    .output(); // non-fatal if it fails
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

/// Current wall-clock in unix milliseconds. NOT used inside `verify_mem`
/// (that stays pure) — a convenience for callers building `now_ms`.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// #777: atomic write now lives in crate::fs_util — one implementation.
use crate::fs_util::atomic_write_bytes;
// --- base64 (standard alphabet, with padding) ------------------------------
// The crate has no `base64` dep; this mirrors the codec in `nats.rs`.

const B64_ALPHA: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(B64_ALPHA[((n >> 18) & 63) as usize] as char);
        out.push(B64_ALPHA[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHA[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHA[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in s.as_bytes() {
        if b == b'=' || b == b'\n' || b == b'\r' || b == b' ' {
            continue;
        }
        let val: u32 = match b {
            b'A'..=b'Z' => u32::from(b - b'A'),
            b'a'..=b'z' => u32::from(b - b'a') + 26,
            b'0'..=b'9' => u32::from(b - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(format!("invalid base64 char: {}", b as char)),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// Decode base64 into a fixed-size array, erroring on the wrong length.
pub(crate) fn b64_decode_arr<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let v = b64_decode(s)?;
    if v.len() != N {
        return Err(format!("expected {N} bytes, got {}", v.len()));
    }
    let mut a = [0u8; N];
    a.copy_from_slice(&v);
    Ok(a)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    // Needed to hand-build a cross-domain signature in the domain-separation
    // test; the module's own `use` imports aren't visible via `super::*`.
    use ed25519_dalek::{Signer, SigningKey};

    const SEED: [u8; 32] = [7u8; 32];
    const NONCE: [u8; 16] = [3u8; 16];
    const TS: i64 = 1_700_000_000_000;

    fn fixed_id() -> Uuid {
        Uuid::from_bytes([9u8; 16])
    }

    // hunt: secret files (signing key, tokens, API key) must be created owner-only
    // (0600) from the start, never a world-readable write-then-chmod. Unix-only.
    #[cfg(unix)]
    #[test]
    fn write_owner_only_creates_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        write_owner_only(&path, b"top secret seed").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secret must be created owner-only, got {mode:o}");
        assert_eq!(std::fs::read(&path).unwrap(), b"top secret seed");

        // #933: the assertions above are satisfied by a create-then-chmod
        // implementation too, so they do not prove "created owner-only" —
        // only "owner-only afterwards". The two halves of the real claim:
        // the mode this function requests IS owner-only, and staging with it
        // produces a file that is owner-only before it has the target's name.
        assert_eq!(OWNER_ONLY_MODE, 0o600, "write_owner_only must request owner-only");
        let staged =
            crate::fs_util::write_temp_sibling(&path, b"top secret seed", Some(OWNER_ONLY_MODE))
                .unwrap();
        let smode = std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777;
        assert_eq!(smode, 0o600, "temp file before the rename, got {smode:o}");
        std::fs::remove_file(&staged).unwrap();
    }

    // (a) sign -> verify round-trip returns the signer pubkey.
    #[test]
    fn sign_verify_round_trip_returns_pubkey() {
        let id = fixed_id();
        let sig = sign_mem(&SEED, "agent-a", id, &NONCE, TS, "hello world", "swarm:x", 65_536, 2);
        let pk = verify_mem(&sig, "agent-a", id, "hello world", "swarm:x", 65_536, 2, TS)
            .expect("verify");
        assert_eq!(pk, verifying_key_bytes(&SEED));
        assert_eq!(pk, sig.pubkey);
    }

    // (b) flipping one content byte -> BadSig.
    #[test]
    fn flipped_content_is_bad_sig() {
        let id = fixed_id();
        let sig = sign_mem(&SEED, "agent-a", id, &NONCE, TS, "hello world", "swarm:x", 65_536, 2);
        let r = verify_mem(&sig, "agent-a", id, "hallo world", "swarm:x", 65_536, 2, TS);
        assert_eq!(r, Err(VerifyErr::BadSig));
    }

    // (c) cross-domain: a phase signature must FAIL verify_mem (domain sep).
    #[test]
    fn cross_domain_phase_sig_fails_verify_mem() {
        let id = fixed_id();
        let phase_msg = canonical_phase("agent-a", &NONCE, TS, "{\"phase\":0.1}", "swarm:x");
        let sk = SigningKey::from_bytes(&SEED);
        let raw = sk.sign(&phase_msg);
        // Same key/nonce/ts, but the signature is over DOMAIN_PHASE bytes.
        let forged = ProvenanceSig {
            alg: PROV_ALG_ED25519,
            pubkey: sk.verifying_key().to_bytes(),
            nonce: NONCE,
            ts_ms: TS,
            sig: raw.to_bytes(),
        };
        let r = verify_mem(&forged, "agent-a", id, "{\"phase\":0.1}", "swarm:x", 65_536, 2, TS);
        assert_eq!(r, Err(VerifyErr::BadSig));
    }

    // (d) stale ts (now far past ts_ms + SKEW) -> Stale.
    #[test]
    fn stale_timestamp_is_stale() {
        let id = fixed_id();
        let sig = sign_mem(&SEED, "agent-a", id, &NONCE, TS, "hello world", "swarm:x", 65_536, 2);
        let now = TS + SKEW_MS + 1;
        let r = verify_mem(&sig, "agent-a", id, "hello world", "swarm:x", 65_536, 2, now);
        assert_eq!(r, Err(VerifyErr::Stale));
    }

    // (e) ReplayLru: same (id, nonce) twice -> second is Replay.
    #[test]
    fn replay_lru_rejects_duplicate() {
        let mut lru = ReplayLru::new();
        let id = fixed_id();
        assert!(lru.check_and_insert(id, &NONCE, TS).is_ok());
        assert_eq!(lru.check_and_insert(id, &NONCE, TS), Err(VerifyErr::Replay));
        // A different nonce for the same id is fine.
        assert!(lru.check_and_insert(id, &[4u8; 16], TS).is_ok());
    }

    // (f) canonical_mem is deterministic AND every field shifts the bytes.
    #[test]
    fn canonical_mem_stable_and_field_sensitive() {
        let id = fixed_id();
        let base = canonical_mem("agent-a", id, &NONCE, TS, "content", "subj", 100, 1);
        // Deterministic.
        assert_eq!(base, canonical_mem("agent-a", id, &NONCE, TS, "content", "subj", 100, 1));
        // Golden length: 20 (domain) + 4+7 (agent) + 16 + 16 + 8 + 32
        //              + 4+4 (subject) + 4 (amp) + 1 (tier) = 116.
        assert_eq!(base.len(), 116);
        // Each field changes the bytes (length-prefix boundary integrity).
        assert_ne!(base, canonical_mem("agent-b", id, &NONCE, TS, "content", "subj", 100, 1));
        assert_ne!(base, canonical_mem("agent-a", id, &NONCE, TS, "content", "subj2", 100, 1));
        assert_ne!(base, canonical_mem("agent-a", id, &NONCE, TS, "content", "subj", 101, 1));
        assert_ne!(base, canonical_mem("agent-a", id, &NONCE, TS, "content", "subj", 100, 2));
        // Boundary shift: moving a byte from agent_id into subject must NOT
        // collide (this is what length-prefixing buys us).
        assert_ne!(
            canonical_mem("agent-ax", id, &NONCE, TS, "content", "subj", 100, 1),
            canonical_mem("agent-a", id, &NONCE, TS, "content", "xsubj", 100, 1),
        );
    }

    // (g) node_signing_key generates then re-loads the SAME key.
    #[test]
    fn node_signing_key_generates_then_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let k1 = node_signing_key(dir.path()).expect("generate");
        assert!(dir.path().join("node_key.ed25519").exists());
        let k2 = node_signing_key(dir.path()).expect("reload");
        assert_eq!(k1, k2);
    }

    // Bonus: ProvenanceSig JSON round-trips through the base64 wire form.
    #[test]
    fn provenance_sig_json_round_trip() {
        let id = fixed_id();
        let sig = sign_mem(&SEED, "agent-a", id, &NONCE, TS, "hello", "swarm:x", 65_536, 2);
        let json = serde_json::to_string(&sig).unwrap();
        let back: ProvenanceSig = serde_json::from_str(&json).unwrap();
        assert_eq!(sig, back);
    }

    // Bonus: ReplayLru persist -> load survives a round-trip and still
    // rejects the persisted (id, nonce) as a replay.
    #[test]
    fn replay_lru_persist_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replay.json");
        let id = fixed_id();
        let mut lru = ReplayLru::new();
        lru.check_and_insert(id, &NONCE, TS).unwrap();
        lru.persist(&path).unwrap();

        let mut loaded = ReplayLru::load(&path, TS);
        assert!(!loaded.poisoned());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.check_and_insert(id, &NONCE, TS), Err(VerifyErr::Replay));
    }

    // Bonus: a corrupt persisted file loads fail-closed (poisoned).
    #[test]
    fn replay_lru_load_corrupt_is_poisoned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replay.json");
        std::fs::write(&path, "not json at all").unwrap();
        let lru = ReplayLru::load(&path, TS);
        assert!(lru.poisoned());
        assert!(lru.refuses_live(TS));
        assert!(!lru.refuses_live(TS + SKEW_MS + 1));
    }

    // (#11) a persisted replay entry with a hostile `ts` (i64::MIN) must not
    // overflow the freshness subtraction on load — the entry is treated as
    // stale and dropped, and `load` does NOT panic. Without the i128 widening
    // `now_ms - e.ts` overflows i64 (debug panic / release wrap).
    #[test]
    fn replay_lru_load_hostile_ts_no_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replay.json");
        // Hand-craft a well-formed sidecar whose only entry carries ts=i64::MIN.
        let wire = format!(
            "{{\"entries\":[{{\"id\":\"{}\",\"nonce\":\"{}\",\"ts\":{}}}]}}",
            fixed_id(),
            b64_encode(&NONCE),
            i64::MIN
        );
        std::fs::write(&path, wire).unwrap();
        // Must NOT panic (i128 widening); the ancient entry is dropped as stale.
        let lru = ReplayLru::load(&path, TS);
        assert!(!lru.poisoned(), "well-formed JSON loads (not poisoned)");
        assert_eq!(lru.len(), 0, "i64::MIN ts is ancient ⇒ dropped as stale");
    }

    // (#4) snapshot signing round-trips; a tampered snapshot fails verification.
    #[test]
    fn snapshot_sign_verify_round_trip_and_tamper() {
        let bytes = b"a reputation snapshot payload";
        let sig = sign_snapshot(&SEED, bytes);
        let vk = verifying_key_bytes(&SEED);
        assert!(verify_snapshot(&vk, bytes, &sig), "own-key signature verifies");
        // A different key must NOT verify.
        assert!(!verify_snapshot(&verifying_key_bytes(&[8u8; 32]), bytes, &sig));
        // Tampered bytes must NOT verify.
        assert!(!verify_snapshot(&vk, b"a reputation snapshot payloaX", &sig));
    }
}
