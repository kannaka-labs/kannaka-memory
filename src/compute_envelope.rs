//! KAX Compute District envelope contract — canonical JSON + Ed25519 signing.
//!
//! The machines in the KAX Compute District (kannaka-labs/kax-computer) are
//! woken by a signed job envelope published to `KAX.machine.<id>.inbox`:
//!
//! ```text
//! job:   {v:1, machine, id, ts, prompt, signer, sig}
//! grant: {v:1, type:"credit_grant", machine, id, ts, credits, signer, sig}
//! ```
//!
//! `sig` is an Ed25519 signature (hex) over the **canonical JSON** of the
//! envelope minus `sig`. Canonical means exactly what the manager computes
//! with Python's `json.dumps(obj, sort_keys=True, separators=(",", ":"))`:
//! keys sorted by code point, no whitespace, non-ASCII escaped as `\uXXXX`
//! (`ensure_ascii=True` is Python's default), floats in Python `repr` form.
//! One byte of drift anywhere and the manager answers `bad_signature`, so
//! every rule here is pinned by golden vectors generated with CPython.
//!
//! This module is pure (no NATS, no HTTP) so the CLI handler, the tests, and
//! any future signer share one implementation of the wire bytes.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::Value;

/// Envelope schema version spoken by the manager.
pub const ENVELOPE_VERSION: u64 = 1;
/// Default signer name — the key the manager's `trusted_keys.json` was
/// bootstrapped with (see kax-computer `operator/kax_send.py`).
pub const DEFAULT_SIGNER: &str = "operator-nick";
/// Default operator key path, relative to the home directory.
pub const DEFAULT_KEY_RELPATH: &str = ".kannaka/kax-operator.key";
/// The manager rejects an envelope whose `ts` is more than this many
/// seconds from its own clock (`MAX_SKEW_S` in manager.py).
pub const MAX_SKEW_SECS: u64 = 60;
/// KAX accounting scale: 1 credit = 1,000,000 minor units.
pub const MINOR_PER_CREDIT: i64 = 1_000_000;
/// Fleet snapshot subject (whole roster, every 60s).
pub const STATUS_SUBJECT: &str = "KAX.machines.status";

// ── subjects ───────────────────────────────────────────────────────────────

/// `KAX.machine.<id>.inbox` — signed wakes and grants go here.
pub fn inbox_subject(machine: &str) -> String {
    format!("KAX.machine.{machine}.inbox")
}
/// `KAX.machine.<id>.outbox` — job results.
pub fn outbox_subject(machine: &str) -> String {
    format!("KAX.machine.{machine}.outbox")
}
/// `KAX.machine.<id>.events` — mirrored ledger rows (lifecycle + wallet).
pub fn events_subject(machine: &str) -> String {
    format!("KAX.machine.{machine}.events")
}
/// `KAX.machine.<id>.identity` — the machine's Nostr public key.
pub fn identity_subject(machine: &str) -> String {
    format!("KAX.machine.{machine}.identity")
}

/// A machine id is one NATS subject token: it must not contain `.`, `*`,
/// `>`, whitespace or control characters, or the envelope would publish to
/// (or subscribe on) a different subject than the one it names. The
/// manager compares `env.machine` to the subject's token, so a mismatch is
/// rejected anyway — this just makes the error local and legible.
pub fn validate_machine_id(machine: &str) -> Result<(), String> {
    if machine.is_empty() {
        return Err("machine id is empty".into());
    }
    if let Some(c) = machine
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(format!(
            "machine id {machine:?} contains {c:?}; allowed: A-Z a-z 0-9 - _"
        ));
    }
    Ok(())
}

// ── numerics ───────────────────────────────────────────────────────────────

/// Timestamp field. The manager accepts either an int or a float
/// (`isinstance(ts, (int, float))`); the CLI sends integer seconds because
/// an integer has exactly one JSON spelling on both sides of the wire.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ts {
    Int(i64),
    Float(f64),
}

impl Ts {
    fn to_value(self) -> Value {
        match self {
            Ts::Int(i) => Value::from(i),
            Ts::Float(f) => Value::from(f),
        }
    }
}

/// Credit amount for a grant. Integers are preferred (same reason as
/// `Ts`); fractions are allowed but must be explicitly opted into by the
/// caller (`--allow-fraction`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Credits {
    Int(u64),
    Float(f64),
}

impl Credits {
    fn to_value(self) -> Value {
        match self {
            Credits::Int(i) => Value::from(i),
            Credits::Float(f) => Value::from(f),
        }
    }

    pub fn as_f64(self) -> f64 {
        match self {
            Credits::Int(i) => i as f64,
            Credits::Float(f) => f,
        }
    }
}

/// Parse a user-supplied credit amount. Positive integers parse to
/// `Credits::Int`; anything with a fractional part needs `allow_fraction`.
/// Zero, negatives, NaN/inf and garbage are refused (the manager would
/// reject them as `bad_grant_amount` — better to fail before signing).
pub fn parse_credits(s: &str, allow_fraction: bool) -> Result<Credits, String> {
    let s = s.trim();
    if let Ok(i) = s.parse::<u64>() {
        if i == 0 {
            return Err("credits must be positive".into());
        }
        return Ok(Credits::Int(i));
    }
    let f: f64 = s
        .parse()
        .map_err(|_| format!("credits {s:?} is not a number"))?;
    if !f.is_finite() || f <= 0.0 {
        return Err("credits must be a positive finite number".into());
    }
    if f.fract() == 0.0 && f < (u64::MAX as f64) {
        // "5.0" — whole; send as the integer the manager would round to anyway.
        return Ok(Credits::Int(f as u64));
    }
    if !allow_fraction {
        return Err(format!(
            "credits {s:?} has a fractional part; pass --allow-fraction to grant it"
        ));
    }
    Ok(Credits::Float(f))
}

/// Credits from integer minor units (1 credit = 1,000,000 minor).
pub fn credits_from_minor(minor: i64) -> f64 {
    minor as f64 / MINOR_PER_CREDIT as f64
}

// ── envelopes ──────────────────────────────────────────────────────────────

/// Unsigned job (wake) envelope. Key order here is irrelevant — the
/// canonical form sorts — but `serde_json::json!` keeps this readable.
pub fn job_envelope(machine: &str, id: &str, ts: Ts, prompt: &str, signer: &str) -> Value {
    serde_json::json!({
        "v": ENVELOPE_VERSION,
        "machine": machine,
        "id": id,
        "ts": ts.to_value(),
        "prompt": prompt,
        "signer": signer,
    })
}

/// Unsigned credit-grant envelope.
pub fn grant_envelope(machine: &str, id: &str, ts: Ts, credits: Credits, signer: &str) -> Value {
    serde_json::json!({
        "v": ENVELOPE_VERSION,
        "type": "credit_grant",
        "machine": machine,
        "id": id,
        "ts": ts.to_value(),
        "credits": credits.to_value(),
        "signer": signer,
    })
}

/// A signed envelope plus the exact bytes that were signed.
#[derive(Debug, Clone, PartialEq)]
pub struct Signed {
    /// The envelope with `sig` attached — this is what goes on the wire
    /// (serialized however you like; the manager re-canonicalizes).
    pub envelope: Value,
    /// Canonical JSON of the envelope minus `sig` — the signed message.
    pub canonical: String,
    /// Ed25519 signature over `canonical`, lowercase hex.
    pub sig_hex: String,
}

/// Sign an unsigned envelope with a 32-byte Ed25519 seed. Any pre-existing
/// `sig` field is discarded before canonicalization, so re-signing is safe.
pub fn sign_envelope(unsigned: &Value, seed: &[u8; 32]) -> Result<Signed, String> {
    let mut obj = match unsigned {
        Value::Object(m) => m.clone(),
        _ => return Err("envelope must be a JSON object".into()),
    };
    obj.remove("sig");
    let base = Value::Object(obj);
    let canonical = canonical_json(&base)?;
    let key = SigningKey::from_bytes(seed);
    let sig = key.sign(canonical.as_bytes());
    let sig_hex = hex_encode(&sig.to_bytes());
    let mut envelope = base;
    envelope["sig"] = Value::String(sig_hex.clone());
    Ok(Signed {
        envelope,
        canonical,
        sig_hex,
    })
}

/// Verify a signed envelope against a 32-byte Ed25519 public key, exactly
/// the way the manager does: strip `sig`, canonicalize, verify.
pub fn verify_envelope(signed: &Value, pubkey: &[u8; 32]) -> Result<(), String> {
    let obj = match signed {
        Value::Object(m) => m,
        _ => return Err("envelope must be a JSON object".into()),
    };
    let sig_hex = obj
        .get("sig")
        .and_then(Value::as_str)
        .ok_or("envelope has no sig")?;
    let sig_bytes = hex_decode(sig_hex)?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|e| format!("bad sig: {e}"))?;
    let mut base = obj.clone();
    base.remove("sig");
    let canonical = canonical_json(&Value::Object(base))?;
    let vk = VerifyingKey::from_bytes(pubkey).map_err(|e| format!("bad pubkey: {e}"))?;
    vk.verify(canonical.as_bytes(), &sig)
        .map_err(|_| "signature does not verify".to_string())
}

/// Public key (32 bytes) for a seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

/// Parse the contents of an operator key file: a 64-char hex seed,
/// surrounding whitespace tolerated. This is the format `kax_send.py`
/// reads (`bytes.fromhex(open(path).read().strip())`).
pub fn parse_seed_hex(contents: &str) -> Result<[u8; 32], String> {
    let bytes = hex_decode(contents.trim())?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("seed must be 32 bytes, got {}", v.len()))?;
    Ok(arr)
}

/// The `trusted_keys.json` entry a manager needs to accept this signer.
pub fn trusted_keys_snippet(signer: &str, pubkey: &[u8; 32]) -> String {
    format!("{{\"{}\": \"{}\"}}", signer, hex_encode(pubkey))
}

// ── canonical JSON (Python json.dumps sort_keys + compact) ─────────────────

/// Serialize `v` exactly as CPython's
/// `json.dumps(v, sort_keys=True, separators=(",", ":"))` would.
///
/// Rules pinned by the golden vectors below:
/// - object keys sorted by Unicode code point (byte order of UTF-8);
/// - no whitespace anywhere;
/// - strings: `"` `\` escaped, `\n \r \t \b \f` short forms, every other
///   char outside `0x20..=0x7e` as `\uXXXX` (lowercase hex; astral chars as
///   a UTF-16 surrogate pair) — Python's `ensure_ascii=True` default;
/// - integers verbatim; floats in Python `repr` form (`5.0`, `0.5`,
///   `1e+16`, `1e-05`).
///
/// Errors on non-finite floats: Python would emit `NaN`/`Infinity`, which
/// the manager's `json.loads` accepts but no signer should ever send.
pub fn canonical_json(v: &Value) -> Result<String, String> {
    let mut out = String::new();
    write_canonical(v, &mut out)?;
    Ok(out)
}

fn write_canonical(v: &Value, out: &mut String) -> Result<(), String> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else if let Some(f) = n.as_f64() {
                out.push_str(&python_float_repr(f)?);
            } else {
                return Err(format!("unrepresentable number {n}"));
            }
        }
        Value::String(s) => write_python_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // Sort explicitly rather than trusting the map's iteration
            // order: with serde_json's `preserve_order` feature (which any
            // dependency can switch on for the whole build) the map is
            // insertion-ordered, and the canonical form must not depend
            // on a Cargo feature.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_python_string(k, out);
                out.push(':');
                write_canonical(&map[*k], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Python `json.dumps` string escaping with `ensure_ascii=True`
/// (`ESCAPE_ASCII = r'([\\"]|[^\ -~])'`).
fn write_python_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
        }
    }
    out.push('"');
}

/// CPython `float.__repr__`: shortest round-trip digits, fixed notation when
/// the decimal exponent is in `-4..=15`, else scientific with a signed,
/// at-least-two-digit exponent; a whole number always keeps `.0`.
pub fn python_float_repr(f: f64) -> Result<String, String> {
    if !f.is_finite() {
        return Err(format!("non-finite float {f} has no canonical form"));
    }
    if f == 0.0 {
        return Ok(if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() });
    }
    // Rust's `{:e}` is shortest-round-trip (same digits as Python's repr).
    let sci = format!("{:e}", f);
    let (mantissa, exp) = sci.split_once('e').ok_or("no exponent")?;
    let exp: i32 = exp.parse().map_err(|_| "bad exponent")?;
    let (neg, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => (true, m),
        None => (false, mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    if (-4..=15).contains(&exp) {
        // Fixed notation. decpt = number of digits before the point.
        let decpt = exp + 1;
        if decpt <= 0 {
            s.push_str("0.");
            for _ in 0..(-decpt) {
                s.push('0');
            }
            s.push_str(&digits);
        } else if (decpt as usize) >= digits.len() {
            s.push_str(&digits);
            for _ in 0..(decpt as usize - digits.len()) {
                s.push('0');
            }
            s.push_str(".0");
        } else {
            s.push_str(&digits[..decpt as usize]);
            s.push('.');
            s.push_str(&digits[decpt as usize..]);
        }
    } else {
        s.push_str(&digits[..1]);
        if digits.len() > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        s.push('e');
        s.push(if exp < 0 { '-' } else { '+' });
        s.push_str(&format!("{:02}", exp.abs()));
    }
    Ok(s)
}

// ── hex ────────────────────────────────────────────────────────────────────

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err(format!("hex string has odd length {}", s.len()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("bad hex at offset {i}"))
        })
        .collect()
}

// ── key file ───────────────────────────────────────────────────────────────

/// Write a fresh hex seed to `path`, owner-only (0600 on Unix), refusing to
/// overwrite. Returns the public key.
pub fn write_seed_file(path: &std::path::Path, seed: &[u8; 32]) -> Result<[u8; 32], String> {
    if path.exists() {
        return Err(format!(
            "{} already exists — refusing to overwrite an operator key (move it aside first)",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    let contents = format!("{}\n", hex_encode(seed));
    crate::provenance::write_owner_only(path, contents.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(public_key(seed))
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Golden vectors generated 2026-09-01 with CPython 3.14.3 +
    // cryptography 46.0.5:
    //   json.dumps(obj, sort_keys=True, separators=(",", ":"))
    //   Ed25519PrivateKey.from_private_bytes(bytes(range(32))).sign(canon)
    // Regenerate with the same one-liners if the contract ever changes.
    const SEED_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const PUB_HEX: &str = "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8";
    const JOB_ID: &str = "00000000-0000-4000-8000-000000000001";
    const GRANT_ID: &str = "00000000-0000-4000-8000-000000000002";
    const UNI_ID: &str = "00000000-0000-4000-8000-000000000003";

    const GOLD_JOB_INT_TS: &str = r#"{"id":"00000000-0000-4000-8000-000000000001","machine":"agent001","prompt":"hello world","signer":"operator-nick","ts":1756700000,"v":1}"#;
    const GOLD_JOB_FLOAT_TS: &str = r#"{"id":"00000000-0000-4000-8000-000000000001","machine":"agent001","prompt":"hello world","signer":"operator-nick","ts":1756700000.0,"v":1}"#;
    const GOLD_JOB_FLOAT_TS_MS: &str = r#"{"id":"00000000-0000-4000-8000-000000000001","machine":"agent001","prompt":"hello world","signer":"operator-nick","ts":1756700000.123,"v":1}"#;
    const GOLD_GRANT_INT: &str = r#"{"credits":5,"id":"00000000-0000-4000-8000-000000000002","machine":"kannaka-01","signer":"operator-nick","ts":1756700000,"type":"credit_grant","v":1}"#;
    const GOLD_GRANT_FRACTION: &str = r#"{"credits":0.5,"id":"00000000-0000-4000-8000-000000000002","machine":"kannaka-01","signer":"operator-nick","ts":1756700000,"type":"credit_grant","v":1}"#;
    const GOLD_GRANT_FLOAT_WHOLE: &str = r#"{"credits":5.0,"id":"00000000-0000-4000-8000-000000000002","machine":"kannaka-01","signer":"operator-nick","ts":1756700000,"type":"credit_grant","v":1}"#;
    // Non-ASCII, astral (surrogate pair), quotes, backslash, \n, \t, a
    // C0 control (U+0001) and DEL (U+007F) — every branch of the escaper.
    // Raw string: the backslashes below are literal wire bytes.
    const GOLD_UNICODE: &str = r#"{"id":"00000000-0000-4000-8000-000000000003","machine":"agent002","prompt":"h\u00e9llo w\u00f6rld \u2014 \u65e5\u672c\u8a9e \ud83d\ude80 \"quoted\" \\ back\nnewline\ttab/slash \u0001ctl \u007fdel","signer":"operator-nick","ts":1756700001,"v":1}"#;
    const UNICODE_PROMPT: &str =
        "h\u{e9}llo w\u{f6}rld \u{2014} \u{65e5}\u{672c}\u{8a9e} \u{1f680} \"quoted\" \\ back\nnewline\ttab/slash \u{1}ctl \u{7f}del";

    // Python-produced signatures (Ed25519 is deterministic, so Rust must
    // reproduce these byte for byte).
    const PY_SIG_JOB_INT_TS: &str = "91539e7a458134a6c4af43ac1b4a6040c8eae13b33c8c0591db533c8d33e3db0221a1eb57e4c0389de09102125009a36421a28c38bef0e97fa86f8ec5c9ab20d";
    const PY_SIG_UNICODE: &str = "681da2c97fae7e145904e1a0433df2e5ef5a1f99ba298e8693b2873e65f81fb4628beaffa6c32dd29ff436f37eaac40d03364e7ce8798df25486406ba3106809";

    fn seed() -> [u8; 32] {
        parse_seed_hex(SEED_HEX).unwrap()
    }

    #[test]
    fn golden_job_int_ts() {
        let e = job_envelope("agent001", JOB_ID, Ts::Int(1756700000), "hello world", DEFAULT_SIGNER);
        assert_eq!(canonical_json(&e).unwrap(), GOLD_JOB_INT_TS);
    }

    #[test]
    fn golden_job_float_ts_int_and_float_differ() {
        let e = job_envelope("agent001", JOB_ID, Ts::Float(1756700000.0), "hello world", DEFAULT_SIGNER);
        assert_eq!(canonical_json(&e).unwrap(), GOLD_JOB_FLOAT_TS);
        let e = job_envelope("agent001", JOB_ID, Ts::Float(1756700000.123), "hello world", DEFAULT_SIGNER);
        assert_eq!(canonical_json(&e).unwrap(), GOLD_JOB_FLOAT_TS_MS);
        // The two spellings are different signed messages — the whole
        // reason the CLI sends integer seconds.
        assert_ne!(GOLD_JOB_INT_TS, GOLD_JOB_FLOAT_TS);
    }

    #[test]
    fn golden_grants() {
        let e = grant_envelope("kannaka-01", GRANT_ID, Ts::Int(1756700000), Credits::Int(5), DEFAULT_SIGNER);
        assert_eq!(canonical_json(&e).unwrap(), GOLD_GRANT_INT);
        let e = grant_envelope("kannaka-01", GRANT_ID, Ts::Int(1756700000), Credits::Float(0.5), DEFAULT_SIGNER);
        assert_eq!(canonical_json(&e).unwrap(), GOLD_GRANT_FRACTION);
        let e = grant_envelope("kannaka-01", GRANT_ID, Ts::Int(1756700000), Credits::Float(5.0), DEFAULT_SIGNER);
        assert_eq!(canonical_json(&e).unwrap(), GOLD_GRANT_FLOAT_WHOLE);
    }

    #[test]
    fn golden_unicode_prompt_escapes_like_python() {
        let e = job_envelope("agent002", UNI_ID, Ts::Int(1756700001), UNICODE_PROMPT, DEFAULT_SIGNER);
        assert_eq!(canonical_json(&e).unwrap(), GOLD_UNICODE);
    }

    #[test]
    fn python_float_repr_matches_cpython() {
        let cases: &[(f64, &str)] = &[
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (123456789.125, "123456789.125"),
            (2.5e-7, "2.5e-07"),
            (1e22, "1e+22"),
            (0.1, "0.1"),
            (100.0, "100.0"),
            (5.0, "5.0"),
            (-0.5, "-0.5"),
            (1756700000.123, "1756700000.123"),
        ];
        for (f, want) in cases {
            assert_eq!(python_float_repr(*f).unwrap(), *want, "repr({f})");
        }
        assert!(python_float_repr(f64::NAN).is_err());
        assert!(python_float_repr(f64::INFINITY).is_err());
    }

    #[test]
    fn keys_sorted_regardless_of_insertion_order_and_nested() {
        let v: Value = serde_json::from_str(r#"{"z":{"b":[1,{"y":2,"x":null}],"a":true},"a":"s"}"#).unwrap();
        assert_eq!(
            canonical_json(&v).unwrap(),
            r#"{"a":"s","z":{"a":true,"b":[1,{"x":null,"y":2}]}}"#
        );
    }

    #[test]
    fn sign_matches_python_signature_and_verifies() {
        let e = job_envelope("agent001", JOB_ID, Ts::Int(1756700000), "hello world", DEFAULT_SIGNER);
        let signed = sign_envelope(&e, &seed()).unwrap();
        assert_eq!(signed.canonical, GOLD_JOB_INT_TS);
        assert_eq!(signed.sig_hex, PY_SIG_JOB_INT_TS);
        assert_eq!(hex_encode(&public_key(&seed())), PUB_HEX);
        let pk: [u8; 32] = hex_decode(PUB_HEX).unwrap().try_into().unwrap();
        verify_envelope(&signed.envelope, &pk).unwrap();

        let e = job_envelope("agent002", UNI_ID, Ts::Int(1756700001), UNICODE_PROMPT, DEFAULT_SIGNER);
        let signed = sign_envelope(&e, &seed()).unwrap();
        assert_eq!(signed.sig_hex, PY_SIG_UNICODE);
        verify_envelope(&signed.envelope, &pk).unwrap();
    }

    #[test]
    fn tamper_or_wrong_key_fails_verify() {
        let e = job_envelope("agent001", JOB_ID, Ts::Int(1756700000), "hello world", DEFAULT_SIGNER);
        let signed = sign_envelope(&e, &seed()).unwrap();
        let pk = public_key(&seed());
        let mut tampered = signed.envelope.clone();
        tampered["prompt"] = Value::String("hello world HAHA INJECTED".into());
        assert!(verify_envelope(&tampered, &pk).is_err());
        let other = public_key(&[7u8; 32]);
        assert!(verify_envelope(&signed.envelope, &other).is_err());
        let mut unsigned = signed.envelope.clone();
        unsigned.as_object_mut().unwrap().remove("sig");
        assert!(verify_envelope(&unsigned, &pk).is_err());
    }

    #[test]
    fn resigning_discards_stale_sig() {
        let e = job_envelope("agent001", JOB_ID, Ts::Int(1756700000), "hello world", DEFAULT_SIGNER);
        let once = sign_envelope(&e, &seed()).unwrap();
        let twice = sign_envelope(&once.envelope, &seed()).unwrap();
        assert_eq!(once, twice);
    }

    /// What `--dry-run` prints must round-trip: parsing the canonical bytes
    /// back through serde_json and re-canonicalizing yields the same bytes,
    /// and the wire envelope (serde's own serialization) re-canonicalizes to
    /// them too. This is the property the manager relies on: it never sees
    /// our bytes, only `json.loads` of whatever serialization we publish.
    #[test]
    fn dry_run_round_trips() {
        for (env, gold) in [
            (job_envelope("agent002", UNI_ID, Ts::Int(1756700001), UNICODE_PROMPT, DEFAULT_SIGNER), GOLD_UNICODE),
            (grant_envelope("kannaka-01", GRANT_ID, Ts::Int(1756700000), Credits::Float(0.5), DEFAULT_SIGNER), GOLD_GRANT_FRACTION),
            (job_envelope("agent001", JOB_ID, Ts::Float(1756700000.123), "hello world", DEFAULT_SIGNER), GOLD_JOB_FLOAT_TS_MS),
        ] {
            let signed = sign_envelope(&env, &seed()).unwrap();
            assert_eq!(signed.canonical, gold);
            let reparsed: Value = serde_json::from_str(&signed.canonical).unwrap();
            assert_eq!(canonical_json(&reparsed).unwrap(), gold);
            let wire = serde_json::to_string(&signed.envelope).unwrap();
            let from_wire: Value = serde_json::from_str(&wire).unwrap();
            let pk = public_key(&seed());
            verify_envelope(&from_wire, &pk).unwrap();
        }
    }

    #[test]
    fn credits_parsing() {
        assert_eq!(parse_credits("5", false).unwrap(), Credits::Int(5));
        assert_eq!(parse_credits("5.0", false).unwrap(), Credits::Int(5));
        assert!(parse_credits("0", false).is_err());
        assert!(parse_credits("-3", false).is_err());
        assert!(parse_credits("nan", true).is_err());
        assert!(parse_credits("inf", true).is_err());
        assert!(parse_credits("abc", true).is_err());
        assert!(parse_credits("0.5", false).is_err());
        assert_eq!(parse_credits("0.5", true).unwrap(), Credits::Float(0.5));
        assert_eq!(parse_credits("2.5", true).unwrap().as_f64(), 2.5);
    }

    #[test]
    fn machine_ids_are_single_subject_tokens() {
        validate_machine_id("agent001").unwrap();
        validate_machine_id("kannaka-01").unwrap();
        for bad in ["", "a.b", "a*", ">", "agent 1", "agent\n1", "\u{e9}"] {
            assert!(validate_machine_id(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn hex_and_seed_parsing() {
        assert_eq!(hex_encode(&[0, 15, 255]), "000fff");
        assert_eq!(hex_decode("000FFF").unwrap(), vec![0, 15, 255]);
        assert!(hex_decode("abc").is_err());
        assert!(hex_decode("zz").is_err());
        assert!(parse_seed_hex("abcd").is_err());
        assert_eq!(parse_seed_hex(&format!("  {SEED_HEX}\n")).unwrap(), seed());
        assert_eq!(credits_from_minor(1_790_568), 1.790568);
        assert_eq!(
            trusted_keys_snippet("operator-nick", &public_key(&seed())),
            format!("{{\"operator-nick\": \"{PUB_HEX}\"}}")
        );
    }

    #[test]
    fn subjects() {
        assert_eq!(inbox_subject("agent001"), "KAX.machine.agent001.inbox");
        assert_eq!(outbox_subject("agent001"), "KAX.machine.agent001.outbox");
        assert_eq!(events_subject("agent001"), "KAX.machine.agent001.events");
        assert_eq!(identity_subject("agent001"), "KAX.machine.agent001.identity");
    }

    #[test]
    fn write_seed_file_refuses_overwrite_and_is_owner_only() {
        let dir = std::env::temp_dir().join(format!("kax-keygen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("op.key");
        let pk = write_seed_file(&path, &seed()).unwrap();
        assert_eq!(hex_encode(&pk), PUB_HEX);
        let back = parse_seed_hex(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, seed());
        assert!(write_seed_file(&path, &[1u8; 32]).is_err(), "must refuse overwrite");
        assert_eq!(parse_seed_hex(&std::fs::read_to_string(&path).unwrap()).unwrap(), seed());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
