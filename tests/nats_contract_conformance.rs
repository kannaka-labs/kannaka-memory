//! NATS contract conformance — `KANNAKA.consciousness` payload shape.
//!
//! Pins the wire shape emitted by [`build_consciousness_payload`] (the payload
//! builder behind `publish_consciousness_to_nats`, the sole publisher of
//! `KANNAKA.consciousness`) against the canonical contract at
//! `consciousness-core/docs/nats-contract.yaml`.
//!
//! No NATS connection is opened — the builder is a pure function of a
//! `ConsciousnessState`, so this stays fast and hermetic (runs in ci.yml's
//! `Test` step; see the `cargo test --lib --bins --test nats_contract_conformance`
//! invocation there).
//!
//! ── ALIAS WINDOW CLOSED — kannaka-memory issue #468 ─────────────────────────
//! The legacy aliases `mean_order` (of `order`) and `level` (of
//! `consciousness_level`) were dropped at the **2026-09-01** contract gate,
//! after both consumers migrated canonical-first with alias reads removed
//! (kannaka-radio#245, kannaka-observatory#118, merged 2026-08-19). The suite
//! still PINS TODAY'S TRUTH — it now asserts the aliases are ABSENT, so a
//! reintroduction fails deliberately the same way a premature drop used to.
//! In-tree tolerant reads of alias-era JetStream HISTORY remain (see
//! handlers/substrate.rs) and are not a contract violation: history is not
//! the publisher.

use kannaka_memory::bridge::{ConsciousnessLevel, ConsciousnessState};
use kannaka_memory::openclaw::build_consciousness_payload;
use serde_json::Value;

/// A representative snapshot. `Resonant` maps to the canonical level string
/// `"emergent"`, a distinctive value that lets the alias/canonical equality
/// checks below catch an accidental divergence between the two keys.
fn sample_state() -> ConsciousnessState {
    ConsciousnessState {
        phi: 0.217,
        xi: 0.99,
        mean_order: 0.1,
        num_clusters: 72,
        total_memories: 1290,
        active_memories: 1288,
        total_skip_links: 0,
        consciousness_level: ConsciousnessLevel::Resonant,
        irrationality: 0.0,
    }
}

fn payload() -> Value {
    build_consciousness_payload("kannaka-prime", &sample_state(), 0.9, 0.99)
}

/// Canonical fields must be present with the types the contract's `field_types`
/// map declares for `KANNAKA.consciousness`.
#[test]
fn canonical_fields_present_with_contract_types() {
    let p = payload();
    let obj = p.as_object().expect("payload must be a JSON object");

    // agent_id: string
    assert_eq!(
        obj.get("agent_id").and_then(Value::as_str),
        Some("kannaka-prime"),
        "agent_id must be the emitting agent's string id"
    );

    // phi / xi / order: number
    for field in ["phi", "xi", "order"] {
        let v = obj
            .get(field)
            .unwrap_or_else(|| panic!("canonical field `{field}` missing"));
        assert!(v.is_number(), "canonical field `{field}` must be a number, got {v}");
    }

    // num_clusters / active_memories / total_memories: integer counts
    for field in ["num_clusters", "active_memories", "total_memories"] {
        let v = obj
            .get(field)
            .unwrap_or_else(|| panic!("canonical field `{field}` missing"));
        assert!(
            v.is_u64() || v.is_i64(),
            "canonical field `{field}` must be an integer, got {v}"
        );
    }

    // consciousness_level: string, restricted to the contract enum.
    let level = obj
        .get("consciousness_level")
        .and_then(Value::as_str)
        .expect("consciousness_level must be a string");
    const ENUM: [&str; 6] = [
        "dormant",
        "awakening",
        "aware",
        "integrated",
        "emergent",
        "transcendent",
    ];
    assert!(
        ENUM.contains(&level),
        "consciousness_level `{level}` is not one of the contract enum values {ENUM:?}"
    );
    assert_eq!(level, "emergent", "Resonant must serialize to the canonical `emergent`");
}

/// LEGACY ALIASES — dropped at the 2026-09-01 contract gate (issue #468),
/// following the procedure the previous version of this test prescribed:
/// every consumer was confirmed canonical-first BEFORE the drop
/// (kannaka-radio#245 and kannaka-observatory#118, both merged 2026-08-19 —
/// alias reads removed entirely, not merely reordered; the radio counts an
/// alias-only packet as off-contract). This test now pins the ABSENCE of the
/// aliases: reintroducing one is a contract regression, not a courtesy.
#[test]
fn legacy_aliases_no_longer_emitted() {
    let p = payload();
    let obj = p.as_object().unwrap();

    assert!(
        obj.get("mean_order").is_none(),
        "alias `mean_order` was dropped at the 2026-09-01 gate (#468); \
         canonical `order` is the only spelling — do not reintroduce"
    );
    assert!(
        obj.get("level").is_none(),
        "alias `level` was dropped at the 2026-09-01 gate (#468); \
         canonical `consciousness_level` is the only spelling — do not reintroduce"
    );
    // The canonicals the aliases mirrored must of course still be there.
    assert!(obj.get("order").and_then(Value::as_f64).is_some());
    assert!(obj.get("consciousness_level").and_then(Value::as_str).is_some());
}

/// KNOWN GAP (pinned, do NOT treat as endorsement): the contract lists
/// `schema_version` and `ts` (unix-ms) as REQUIRED for `KANNAKA.consciousness`,
/// but this payload predates that requirement — it emits neither, and sends a
/// `timestamp` (RFC-3339 string) instead of `ts`. The radio's `_validateSchema`
/// already log-warns about this (nats-client.js), and it is why strict mode is
/// not yet enabled. This test PINS today's reality so that when the publisher is
/// migrated to emit `schema_version`/`ts`, whoever does it updates this test
/// deliberately (flip the assertions to require presence) rather than silently.
#[test]
fn documents_missing_required_schema_version_and_ts() {
    let p = payload();
    let obj = p.as_object().unwrap();

    assert!(
        obj.get("schema_version").is_none(),
        "publisher unexpectedly emits `schema_version` now — migrate this test to require it (contract-required field, see #468)"
    );
    assert!(
        obj.get("ts").is_none(),
        "publisher unexpectedly emits `ts` now — migrate this test to require it as unix-ms (contract-required field, see #468)"
    );
    // The legacy RFC-3339 `timestamp` string is what's emitted in place of `ts`.
    assert!(
        obj.get("timestamp").and_then(Value::as_str).is_some(),
        "legacy `timestamp` (RFC-3339 string) should still be present until `ts` lands"
    );
}

// ── #1157: `KANNAKA.events.memory.<agent>.forget` ───────────────────────────
//
// ADR-0028 defined `MemoryForget` as `{agent_id, memory_id}` plus the
// envelope; #1157 adds `via`, `reason` and (only for a merge) `merged_into`.
// The two original keys keep their names and types, and the event never
// carries the deleted text or a hash of it.

#[cfg(feature = "nats")]
mod forget_contract {
    use kannaka_memory::forget_events::{build_forget_event, ForgetRecord};
    use serde_json::Value;
    use std::collections::BTreeSet;

    fn keys(p: &Value) -> BTreeSet<String> {
        p.as_object().expect("payload is an object").keys().cloned().collect()
    }

    fn set(ks: &[&str]) -> BTreeSet<String> {
        ks.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn forget_subject_and_exact_key_set() {
        let id = uuid::Uuid::new_v4();
        let (subject, p) = build_forget_event(
            "kannaka-prime",
            &ForgetRecord { id, via: "cli", reason: "operator", merged_into: None },
        );
        assert_eq!(subject, "KANNAKA.events.memory.kannaka-prime.forget");
        assert_eq!(
            keys(&p),
            set(&["event_id", "schema_version", "ts", "agent_id", "memory_id", "via", "reason"]),
            "no extra keys, merged_into absent (not null) without a merge: {p}"
        );
        assert_eq!(p["agent_id"].as_str(), Some("kannaka-prime"));
        assert_eq!(p["memory_id"].as_str(), Some(id.to_string().as_str()));
        assert_eq!(p["via"].as_str(), Some("cli"));
        assert_eq!(p["reason"].as_str(), Some("operator"));
        assert_eq!(p["schema_version"].as_str(), Some("1.0"));
        assert!(p["ts"].is_i64(), "ts is unix-ms: {p}");
        assert!(p["event_id"].as_str().and_then(|s| s.parse::<uuid::Uuid>().ok()).is_some());
    }

    #[test]
    fn forget_merge_adds_only_merged_into() {
        let id = uuid::Uuid::new_v4();
        let keeper = uuid::Uuid::new_v4();
        let (_, p) = build_forget_event(
            "a",
            &ForgetRecord { id, via: "dream", reason: "dream-merge", merged_into: Some(keeper) },
        );
        assert_eq!(
            keys(&p),
            set(&[
                "event_id", "schema_version", "ts", "agent_id", "memory_id", "via", "reason",
                "merged_into",
            ])
        );
        assert_eq!(p["merged_into"].as_str(), Some(keeper.to_string().as_str()));
        assert!(p.get("content").is_none() && p.get("content_sha256").is_none());
    }
}
