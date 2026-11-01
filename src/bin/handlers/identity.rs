//! `kannaka identity` — spacechild-auth SSO identity for swarm agents.
//!
//! Step 1 of real agent identity: register/login against the SpaceChild
//! auth service, persist the session to `<data_dir>/identity.json`, and
//! introspect it with `whoami` (auto-refreshing on a stale access token).
//! Follows the handler-extraction pattern documented in
//! `handlers/substrate.rs`. All protocol logic lives in
//! `kannaka_memory::identity`; this file is CLI wiring only.
//!
//! Token hygiene: tokens are never printed. `whoami` shows user id,
//! email, and access-token expiry only.

use std::io::Write;
use std::process;

use kannaka_memory::config::KannakaConfig;
use kannaka_memory::identity::{
    self, AuthClient, IdentityError, IdentityStore, UserInfo,
};
use kannaka_memory::beacon::Beacon;
use kannaka_memory::provenance::{node_signing_key, verifying_key_bytes};
use kannaka_memory::reputation::{RepStore, SeedStatus};

const USAGE: &str = "Usage: kannaka identity \
    <register|login|whoami|logout|keygen|pubkey|enroll-seed|vouch|revoke|beacon|voice> [args]";

pub(crate) fn handle_identity(args: &[String]) {
    let sub = match args.get(1) {
        Some(s) => s.as_str(),
        None => {
            eprintln!("{USAGE}");
            process::exit(1);
        }
    };

    match sub {
        // SpaceChild SSO session.
        "register" => handle_register(args),
        "login" => handle_login(args),
        "whoami" => handle_whoami(),
        "logout" => handle_logout(),
        // inc-1 cryptographic swarm identity + corroboration trust root.
        "keygen" => handle_keygen(),
        "pubkey" => handle_pubkey(),
        "enroll-seed" => handle_enroll_seed(args),
        "vouch" => handle_vouch(args),
        "revoke" => handle_revoke(args),
        "beacon" => handle_beacon(),
        "voice" => handle_voice(args),
        _ => {
            eprintln!("{USAGE}");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

fn handle_register(args: &[String]) {
    let email = require_email(args, "Usage: kannaka identity register --email <addr>");
    let password = read_password(&email);
    let client = AuthClient::from_env();

    match client.register(&email, &password) {
        Ok(out) => {
            if let Some((access, refresh)) = out.tokens {
                let path = store_session(&out.user_id, &out.email, access, refresh);
                println!("  \u{2713} Registered as {} (user {})", out.email, out.user_id);
                println!("  Identity stored at {}", path.display());
            } else if out.requires_verification {
                println!("  \u{2713} Registered {} — email verification required.", out.email);
                if let Some(m) = out.message {
                    println!("  {m}");
                }
                println!("  After verifying, run: kannaka identity login --email {}", out.email);
            } else {
                // Registered but no tokens and no verification flag — odd,
                // but recoverable via a normal login.
                println!("  \u{2713} Registered {} (user {})", out.email, out.user_id);
                println!("  Run: kannaka identity login --email {}", out.email);
            }
        }
        Err(e) => {
            eprintln!("  identity register failed: {e}");
            process::exit(1);
        }
    }
}

fn handle_login(args: &[String]) {
    let email = require_email(args, "Usage: kannaka identity login --email <addr>");
    let password = read_password(&email);
    let client = AuthClient::from_env();

    match client.login(&email, &password) {
        Ok(out) => {
            let path = store_session(&out.user_id, &out.email, out.access_token, out.refresh_token);
            println!("  \u{2713} Logged in as {} (user {})", out.email, out.user_id);
            println!("  Identity stored at {}", path.display());
        }
        Err(e) => {
            eprintln!("  identity login failed: {e}");
            process::exit(1);
        }
    }
}

fn handle_whoami() {
    let path = identity::identity_path();
    let store = match IdentityStore::load(&path) {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!("  Not logged in — run: kannaka identity login --email <addr>");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("  identity whoami failed: {e}");
            process::exit(1);
        }
    };
    let client = AuthClient::from_env();

    match client.user(&store.access_token) {
        Ok(user) => print_whoami(&user, &store.access_token),
        // Stale access token — refresh once with the stored refreshToken,
        // persist the new pair, and retry.
        Err(IdentityError::Status { status: 401, .. }) => {
            match client.refresh(&store.refresh_token) {
                Ok((access, refresh)) => {
                    let refreshed = IdentityStore {
                        access_token: access,
                        refresh_token: refresh,
                        obtained_at: chrono::Utc::now(),
                        ..store
                    };
                    if let Err(e) = refreshed.save(&path) {
                        eprintln!("  warning: could not persist refreshed tokens: {e}");
                    }
                    match client.user(&refreshed.access_token) {
                        Ok(user) => print_whoami(&user, &refreshed.access_token),
                        Err(e) => {
                            eprintln!("  identity whoami failed after refresh: {e}");
                            process::exit(1);
                        }
                    }
                }
                Err(_) => {
                    eprintln!(
                        "  Session expired — run: kannaka identity login --email {}",
                        store.email
                    );
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("  identity whoami failed: {e}");
            process::exit(1);
        }
    }
}

const VOICE_USAGE: &str =
    "Usage: kannaka identity voice [show|set <provider> <voice_id> [--setting k=v]...|clear]";

/// `kannaka identity voice` (#985): resolution is identity > config > default.
fn handle_voice(args: &[String]) {
    use kannaka_memory::voice;
    let path = identity::identity_path();
    let rest = &args[2.min(args.len())..];
    let verb = rest.first().map(String::as_str).unwrap_or("show");
    let store = match IdentityStore::load(&path) {
        Ok(s) => s,
        // `show` can still answer from config/default; writing over an
        // unreadable identity file could destroy the session in it.
        Err(e) if verb == "show" => {
            eprintln!("identity voice: warning: cannot read identity ({e}); showing config/default");
            None
        }
        Err(e) => {
            eprintln!("identity voice: {e}");
            process::exit(1);
        }
    };
    match verb {
        "show" => {
            let cfg = KannakaConfig::load();
            let (spec, src) =
                voice::resolve(store.as_ref().and_then(|s| s.voice.as_ref()), cfg.voice.as_ref());
            println!("provider: {}", spec.provider);
            println!("voice_id: {}", spec.voice_id);
            for (k, v) in &spec.settings {
                println!("setting:  {k}={v}");
            }
            println!("source:   {}", src.as_str());
        }
        "set" => {
            let spec = match parse_voice_set(&rest[1..]) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("identity voice: {e}\n{VOICE_USAGE}");
                    process::exit(1);
                }
            };
            if let Err(e) = voice::validate_spec(&spec, &voice::spool_dir()) {
                eprintln!("identity voice: {e}");
                process::exit(1);
            }
            let Some(mut store) = store else {
                eprintln!("identity voice: no identity found; run `kannaka identity login` first");
                process::exit(1);
            };
            store.voice = Some(spec);
            if let Err(e) = store.save(&path) {
                eprintln!("identity voice: {e}");
                process::exit(1);
            }
            println!("voice set");
        }
        "clear" => {
            if let Some(mut store) = store {
                store.voice = None;
                if let Err(e) = store.save(&path) {
                    eprintln!("identity voice: {e}");
                    process::exit(1);
                }
            }
            println!("voice cleared");
        }
        _ => {
            eprintln!("{VOICE_USAGE}");
            process::exit(1);
        }
    }
}

/// `<provider> <voice_id> [--setting k=v]...` -> a spec, or why not.
fn parse_voice_set(args: &[String]) -> Result<kannaka_memory::voice::VoiceSpec, String> {
    let (Some(provider), Some(voice_id)) = (args.first(), args.get(1)) else {
        return Err("set needs <provider> <voice_id>".into());
    };
    let mut spec = kannaka_memory::voice::VoiceSpec::new(provider.clone(), voice_id.clone());
    let mut i = 2;
    while i < args.len() {
        if args[i] != "--setting" {
            return Err(format!("unexpected argument '{}'", args[i]));
        }
        match args.get(i + 1).and_then(|s| s.split_once('=')) {
            Some((k, v)) if !k.trim().is_empty() => {
                spec.settings.insert(k.to_string(), v.to_string());
            }
            _ => return Err("--setting expects k=v with a non-empty key".into()),
        }
        i += 2;
    }
    Ok(spec)
}

fn handle_logout() {
    let path = identity::identity_path();
    match IdentityStore::delete(&path) {
        Ok(true) => println!("  \u{2713} Logged out — credentials removed from {}", path.display()),
        Ok(false) => println!("  Not logged in (no stored identity)."),
        Err(e) => {
            eprintln!("  identity logout failed: {e}");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// inc-1 corroboration trust model — node key + seed/vouch/revoke
// ---------------------------------------------------------------------------
//
// These verbs manage the node's CRYPTOGRAPHIC swarm identity (ed25519 node key)
// and the operator trust root (pinned seeds + vouch lineage). They are
// independent of the SpaceChild SSO session above. They change NO automatic
// runtime behaviour: the corroboration gate stays dormant until an operator
// pins a seed AND sets `swarm_trust.corroboration_gate_enabled`.
//
// config-vs-store split (judgment call):
//   • seeds live in CONFIG (`swarm_trust.seed_pubkeys`) — operator-pinned,
//     persisted to config.toml, the root `RepStore::seeds_from_cfg` reads.
//   • vouch edges / poison live in the STORE (reputation.{log,snapshot.gz}) —
//     derived trust state the DAG walks to a seed_root.

/// `kannaka identity keygen` — ensure `<data_dir>/node_key.ed25519` exists and
/// print this node's base64 verifying pubkey. Idempotent: a re-run prints the
/// same key. The pubkey goes to STDOUT (scriptable); the note to stderr, so
/// `keygen` and `pubkey` produce byte-identical stdout.
fn handle_keygen() {
    let (pk_b64, path) = ensure_node_pubkey();
    println!("{pk_b64}");
    eprintln!("  \u{2713} node key ensured at {}", path.display());
    eprintln!(
        "  This base64 pubkey is this node's swarm identity — share it to be \
         enrolled as a seed or vouched into the web of trust."
    );
}

/// `kannaka identity pubkey` — print this node's base64 verifying pubkey. Like
/// keygen it ensures the key exists first (no additional side effect), but is
/// framed as a plain read (stdout only).
fn handle_pubkey() {
    let (pk_b64, _path) = ensure_node_pubkey();
    println!("{pk_b64}");
}

/// Ensure the node key exists; return `(base64_pubkey, key_path)`.
fn ensure_node_pubkey() -> (String, std::path::PathBuf) {
    let dir = KannakaConfig::data_dir();
    let seed = match node_signing_key(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  identity: could not load/create node key: {e}");
            process::exit(1);
        }
    };
    let pk = verifying_key_bytes(&seed);
    (crate::b64_encode_std(&pk), dir.join("node_key.ed25519"))
}

/// Add a base64 seed pubkey to `cfg.swarm_trust.seed_pubkeys`, deduping by the
/// decoded 32-byte key (so re-enrolling the same key, even with different
/// padding/whitespace, is a no-op). Returns `(new_seed_count, added)` or an
/// error if the key is malformed. Pure — mutates only the passed cfg — so it
/// is unit-testable without touching disk.
pub(crate) fn enroll_seed(cfg: &mut KannakaConfig, b64: &str) -> Result<(usize, bool), String> {
    let pk = crate::b64_decode_32(b64)?;
    let canon = crate::b64_encode_std(&pk);
    let already = cfg
        .swarm_trust
        .seed_pubkeys
        .iter()
        .any(|s| crate::b64_decode_32(s).map(|d| d == pk).unwrap_or(false));
    if !already {
        cfg.swarm_trust.seed_pubkeys.push(canon);
    }
    Ok((cfg.swarm_trust.seed_pubkeys.len(), !already))
}

/// `kannaka identity enroll-seed <b64-pubkey>` — operator: pin a base64 pubkey
/// as a trust-root seed. Validates 32-byte decode, dedups, persists to config.
fn handle_enroll_seed(args: &[String]) {
    let usage = "Usage: kannaka identity enroll-seed <base64-pubkey>";
    let b64 = require_arg(args, 2, usage);
    // load_unmodified so persisting the seed set doesn't bake unrelated
    // env-only values (KANNAKA_AGENT_ID, KANNAKA_NATS_URL, …) into the file.
    let mut cfg = KannakaConfig::load_unmodified();
    match enroll_seed(&mut cfg, &b64) {
        Ok((count, added)) => {
            if !added {
                println!("  seed already enrolled — {count} seed(s) pinned");
                return;
            }
            if let Err(e) = cfg.save() {
                eprintln!("  enroll-seed: failed to save config: {e}");
                process::exit(1);
            }
            println!("  \u{2713} enrolled seed — {count} seed(s) now pinned");
            println!(
                "  Seeds are the trust root: every promotion lineage must resolve to a \
                 pinned seed. The gate stays dormant until swarm_trust.corroboration_gate_enabled."
            );
        }
        Err(e) => {
            eprintln!("  enroll-seed: malformed pubkey — {e}");
            process::exit(1);
        }
    }
}

/// `kannaka identity vouch <b64-pubkey>` — if THIS node is itself a pinned
/// seed, record a vouch edge `this_node -> target` in the reputation DAG
/// (persisted), building `seed_root` lineage for a non-seed peer. Non-seeds
/// are refused.
fn handle_vouch(args: &[String]) {
    let usage = "Usage: kannaka identity vouch <base64-pubkey>";
    let target_b64 = require_arg(args, 2, usage);
    let target = match crate::b64_decode_32(&target_b64) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  vouch: malformed pubkey — {e}");
            process::exit(1);
        }
    };
    let dir = KannakaConfig::data_dir();
    let cfg = KannakaConfig::load();
    let me = match node_signing_key(&dir) {
        Ok(seed) => verifying_key_bytes(&seed),
        Err(e) => {
            eprintln!("  vouch: node key error: {e}");
            process::exit(1);
        }
    };
    let mut store = RepStore::load(&dir, &cfg.swarm_trust);
    // #867 finding 2: same refusal as hard-reject — a poisoned store's zeroed
    // head means a vouch written now can never verify on the next load.
    if store.refuses_promotion() {
        eprintln!(
            "  vouch REFUSED: the reputation store is fail-closed (corrupt or              unverifiable log). Repair or remove reputation.log first."
        );
        process::exit(1);
    }
    if store.seed_status(&me) != SeedStatus::Seed {
        eprintln!("  vouch refused: only pinned seeds may vouch, and this node is not a seed.");
        eprintln!("  this node's pubkey: {}", crate::b64_encode_std(&me));
        eprintln!(
            "  an operator can pin it with: kannaka identity enroll-seed {}",
            crate::b64_encode_std(&me)
        );
        process::exit(1);
    }
    if me == target {
        println!("  a seed already roots to itself — nothing to vouch.");
        return;
    }
    store.record_vouch(me, target);
    let root = store
        .seed_root_of(&target)
        .map(|r| crate::b64_encode_std(&r))
        .unwrap_or_else(|| "-".to_string());
    println!("  \u{2713} vouched {} -> {}", crate::b64_encode_std(&me), target_b64);
    println!("  target seed_root now: {root}");
}

/// `kannaka identity revoke <b64-pubkey>` — operator: remove the key from the
/// pinned seed set (if present) and record a revocation in the store, flooring
/// its reputation to 0. The store's only rep-lowering lever is the operator
/// poison (`P_POISON`), so this applies it until rep hits 0 (rep ∈ [0,1] and
/// P_POISON = 0.5 ⇒ at most 2 events for a former non-seed).
fn handle_revoke(args: &[String]) {
    let usage = "Usage: kannaka identity revoke <base64-pubkey>";
    let b64 = require_arg(args, 2, usage);
    let pk = match crate::b64_decode_32(&b64) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  revoke: malformed pubkey — {e}");
            process::exit(1);
        }
    };

    // 1. Config: drop it from the pinned seed set (dedup-safe by decoded bytes).
    let mut cfg = KannakaConfig::load_unmodified();
    let before = cfg.swarm_trust.seed_pubkeys.len();
    cfg.swarm_trust
        .seed_pubkeys
        .retain(|s| crate::b64_decode_32(s).map(|d| d != pk).unwrap_or(true));
    let removed_seed = cfg.swarm_trust.seed_pubkeys.len() != before;
    if removed_seed {
        if let Err(e) = cfg.save() {
            eprintln!("  revoke: failed to save config: {e}");
            process::exit(1);
        }
    }

    // 2. Store: record the revocation (operator poison) and floor rep to 0.
    //    Load the store with the POST-removal seed set so a former seed is now
    //    a plain handle whose record rep can actually be driven down.
    let dir = KannakaConfig::data_dir();
    let mut store = RepStore::load(&dir, &cfg.swarm_trust);
    let mut events = 0;
    while store.rep(&pk) > 0.0 && events < 8 {
        store.record_poison(pk, &cfg.swarm_trust);
        events += 1;
    }

    if removed_seed {
        println!(
            "  \u{2713} revoked — unpinned from seeds ({} remain), rep floored to 0 ({events} poison event(s))",
            cfg.swarm_trust.seed_pubkeys.len()
        );
    } else {
        println!(
            "  \u{2713} revoked — was not a pinned seed; rep floored to 0 ({events} poison event(s))"
        );
    }
}

/// `kannaka identity beacon` — if THIS node is a pinned seed, emit a signed
/// heartbeat beacon for the current epoch (PART A anti-eclipse). Prints the
/// beacon as JSON to stdout so an operator can publish it to the swarm; a
/// receiver verifies it and feeds `QuarantineStaging::ingest_beacon`
/// (`BeaconTracker::ingest`). A non-seed is refused — only seed beacons count
/// toward freshness. Changes NO automatic runtime behaviour (the gate stays
/// dormant until seeds are pinned AND `corroboration_gate_enabled`).
fn handle_beacon() {
    let dir = KannakaConfig::data_dir();
    let cfg = KannakaConfig::load();
    let seed = match node_signing_key(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  beacon: node key error: {e}");
            process::exit(1);
        }
    };
    let me = verifying_key_bytes(&seed);
    let store = RepStore::load(&dir, &cfg.swarm_trust);
    if store.seed_status(&me) != SeedStatus::Seed {
        eprintln!("  beacon refused: only pinned seeds emit heartbeat beacons; this node is not a seed.");
        eprintln!("  this node's pubkey: {}", crate::b64_encode_std(&me));
        process::exit(1);
    }

    let now = kannaka_memory::provenance::now_ms();
    let epoch = kannaka_memory::absorb_gate::epoch_now(&cfg, now);
    // prev_reject_root: lightweight stand-in — the empty root until a persisted
    // operator-reject log is wired (then fold each reject via
    // kannaka_memory::beacon::roll_reject_root before signing).
    let beacon = Beacon::sign(&seed, epoch, kannaka_memory::beacon::EMPTY_REJECT_ROOT);
    // Print the beacon's JSON wire form (the SAME serde shape published below,
    // so a captured line can be replayed by an operator/cron by hand).
    match serde_json::to_string(&beacon) {
        Ok(j) => println!("{j}"),
        Err(e) => {
            eprintln!("  beacon: failed to encode JSON: {e}");
            process::exit(1);
        }
    }
    eprintln!("  \u{2713} signed heartbeat beacon for epoch {epoch}");

    // PART A: PUBLISH to the swarm so peers' freshness advances automatically.
    // The `swarm listen --auto-sync` loop subscribes to BEACON_SUBJECT, verifies
    // and feeds QuarantineStaging::ingest_beacon (a stale/absent seed beacon
    // freezes promotion — anti-eclipse fail-closed). The beacon is already
    // printed above, so a publish failure is a warning, not a hard error — an
    // operator can still relay the printed line.
    let nats_url = if cfg.swarm.nats_url.is_empty() {
        kannaka_memory::nats::DEFAULT_NATS_URL.to_string()
    } else {
        cfg.swarm.nats_url.clone()
    };
    match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(transport) => match transport.publish_beacon(&beacon) {
            Ok(()) => eprintln!(
                "  \u{2713} published beacon to {} on {}",
                nats_url,
                kannaka_memory::BEACON_SUBJECT
            ),
            Err(e) => eprintln!("  warning: beacon signed but NATS publish failed: {e}"),
        },
        Err(e) => eprintln!("  warning: beacon signed but NATS connect failed ({nats_url}): {e}"),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A required positional at `idx` (rejects a missing value or a `--flag`).
fn require_arg(args: &[String], idx: usize, usage: &str) -> String {
    match args.get(idx) {
        Some(s) if !s.is_empty() && !s.starts_with("--") => s.clone(),
        _ => {
            eprintln!("{usage}");
            process::exit(1);
        }
    }
}

/// id/email/expiry only — tokens never reach stdout.
fn print_whoami(user: &UserInfo, access_token: &str) {
    println!("  Identity: {}", user.email);
    println!("  User id:  {}", user.id);
    if let Some(verified) = user.email_verified {
        println!("  Verified: {}", if verified { "yes" } else { "no" });
    }
    if let Some(created) = &user.created_at {
        println!("  Created:  {created}");
    }
    match identity::jwt_exp_unix(access_token)
        .and_then(|exp| chrono::DateTime::from_timestamp(exp, 0))
    {
        Some(exp) => println!("  Token expires: {}", exp.format("%Y-%m-%d %H:%M:%S UTC")),
        None => println!("  Token expires: unknown"),
    }
}

/// Persist a fresh session, returning the path it was written to.
fn store_session(
    user_id: &str,
    email: &str,
    access_token: String,
    refresh_token: String,
) -> std::path::PathBuf {
    let path = identity::identity_path();
    // A re-login must not drop the identity's voice.
    // Only the same user's voice carries over; another login starts clean.
    let voice = IdentityStore::load(&path)
        .ok()
        .flatten()
        .filter(|s| s.user_id == user_id)
        .and_then(|s| s.voice);
    let store = IdentityStore {
        user_id: user_id.to_string(),
        email: email.to_string(),
        access_token,
        refresh_token,
        obtained_at: chrono::Utc::now(),
        voice,
    };
    if let Err(e) = store.save(&path) {
        eprintln!("  identity: failed to store session: {e}");
        process::exit(1);
    }
    path
}

/// Required `--email <addr>` flag.
fn require_email(args: &[String], usage: &str) -> String {
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--email" {
            return super::parse_flag_value::<String>(args, i, "--email", usage);
        }
        i += 1;
    }
    eprintln!("{usage}");
    process::exit(1);
}

/// Password resolution: `KANNAKA_AUTH_PASSWORD` env (the no-echo path for
/// scripts/agents) > a line read from stdin. There's no rpassword dep, so
/// an interactive TTY will echo — the env var is the recommended path.
fn read_password(email: &str) -> String {
    if let Ok(p) = std::env::var("KANNAKA_AUTH_PASSWORD") {
        if !p.is_empty() {
            return p;
        }
    }
    eprint!("Password for {email} (input echoes; set KANNAKA_AUTH_PASSWORD to avoid): ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        eprintln!("identity: failed to read password from stdin");
        process::exit(1);
    }
    let password = line.trim_end_matches(['\r', '\n']).to_string();
    if password.is_empty() {
        eprintln!("identity: no password provided (set KANNAKA_AUTH_PASSWORD or type one on stdin)");
        process::exit(1);
    }
    password
}

// ---------------------------------------------------------------------------
// Tests — inc-1 corroboration trust model verbs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn voice_set_parses_provider_id_and_settings() {
        let s = parse_voice_set(&sv(&["file", "aria", "--setting", "speed=1.2", "--setting", "pitch=-1"])).unwrap();
        assert_eq!((s.provider.as_str(), s.voice_id.as_str()), ("file", "aria"));
        assert_eq!(s.settings["speed"], "1.2");
        assert_eq!(s.settings["pitch"], "-1");
    }

    #[test]
    fn voice_set_rejects_bad_arguments() {
        assert!(parse_voice_set(&sv(&["file"])).is_err());
        assert!(parse_voice_set(&sv(&["file", "aria", "--setting"])).is_err());
        assert!(parse_voice_set(&sv(&["file", "aria", "--setting", "novalue"])).is_err());
        assert!(parse_voice_set(&sv(&["file", "aria", "--setting", "=v"])).is_err());
        assert!(parse_voice_set(&sv(&["file", "aria", "stray"])).is_err());
    }

    // keygen is idempotent: node_signing_key generates once then reloads the
    // SAME key, so the printed base64 pubkey is stable across re-runs.
    #[test]
    fn keygen_pubkey_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let k1 = node_signing_key(dir.path()).unwrap();
        let k2 = node_signing_key(dir.path()).unwrap();
        assert_eq!(
            crate::b64_encode_std(&verifying_key_bytes(&k1)),
            crate::b64_encode_std(&verifying_key_bytes(&k2)),
            "re-run must print the same pubkey"
        );
    }

    // enroll-seed adds to the config seed set and dedups a repeat by decoded key.
    #[test]
    fn enroll_seed_adds_then_dedups() {
        let mut cfg = KannakaConfig::default();
        assert!(cfg.swarm_trust.seed_pubkeys.is_empty());
        let pk_b64 = crate::b64_encode_std(&[7u8; 32]);

        let (count1, added1) = enroll_seed(&mut cfg, &pk_b64).unwrap();
        assert_eq!(count1, 1);
        assert!(added1, "first enroll adds");

        let (count2, added2) = enroll_seed(&mut cfg, &pk_b64).unwrap();
        assert_eq!(count2, 1, "same key must not grow the set");
        assert!(!added2, "repeat is a dedup no-op");
    }

    // enroll-seed rejects a malformed key (wrong length + invalid chars) and
    // leaves the config untouched.
    #[test]
    fn enroll_seed_rejects_bad_key() {
        let mut cfg = KannakaConfig::default();
        // 16 bytes decodes fine as base64 but is the wrong length for a key.
        assert!(enroll_seed(&mut cfg, &crate::b64_encode_std(&[1u8; 16])).is_err());
        // Non-alphabet characters.
        assert!(enroll_seed(&mut cfg, "!!! not base64 !!!").is_err());
        assert!(
            cfg.swarm_trust.seed_pubkeys.is_empty(),
            "bad keys leave config untouched"
        );
    }

    // A node that is NOT a pinned seed may not vouch — the refusal predicate
    // handle_vouch checks (seed_status != Seed) holds for a fresh node with an
    // empty seed set.
    #[test]
    fn non_seed_may_not_vouch() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = KannakaConfig::default(); // no seeds pinned
        let store = RepStore::load(dir.path(), &cfg.swarm_trust);
        let me = verifying_key_bytes(&node_signing_key(dir.path()).unwrap());
        assert_ne!(
            store.seed_status(&me),
            SeedStatus::Seed,
            "fresh node with no pinned seed is not a seed ⇒ vouch refused"
        );
    }
}
