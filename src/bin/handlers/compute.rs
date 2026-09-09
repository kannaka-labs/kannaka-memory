//! `kannaka compute` — operator commands for the KAX Compute District
//! (the machines run by kannaka-labs/kax-computer on skywave).
//!
//! Sub-verbs:
//!   list      GET the public KAX roster (HTTP, no NATS needed)
//!   status    watch KAX.machines.status + KAX.machine.*.events for a window
//!   wake      sign + publish a job envelope (optionally wait for the reply)
//!   grant     sign + publish a credit_grant envelope
//!   events    tail KAX.machine.<id>.events
//!   identity  the machine's Nostr pubkey (roster first, then .identity)
//!   keygen    mint an operator Ed25519 key + trusted_keys.json snippet
//!
//! The envelope bytes and signing live in `kannaka_memory::compute_envelope`
//! (pure, golden-vector tested). This file is the I/O: argument parsing,
//! HTTP to kax.ninja-portal.com, NATS subscribe/publish, and printing.
//!
//! Never prints a private key. Never publishes on `--dry-run`.

use std::process;

use kannaka_memory::compute_envelope as ce;

use super::KannakaConfig;

const USAGE: &str = "Usage: kannaka compute <list|status|wake|grant|events|identity|keygen> [args]
  list [--json]                                   KAX roster (HTTP; state/balance/jobs/last event)
  status [--wait SECS] [--json]                   live fleet snapshot from KAX.machines.status
  wake <machine> <prompt> [--wait SECS] [--dry-run] [--signer NAME] [--key PATH] [--json]
  grant <machine> <credits> [--allow-fraction] [--wait SECS] [--dry-run] [--signer NAME] [--key PATH]
  events <machine> [--follow] [--wait SECS] [--last N] [--json]
  identity <machine> [--wait SECS] [--json]
  keygen [--out PATH] [--signer NAME]
  --nats-url URL on any bus-facing verb; creds: NATS_USER/NATS_PASSWORD or ~/.kannaka-nats.env
  key: --key PATH > $KAX_OPERATOR_KEY > ~/.kannaka/kax-operator.key (hex Ed25519 seed)";

const DEFAULT_KAX_API: &str = "https://kax.ninja-portal.com";
/// Manager snapshot + identity announce cadence is 60s; a window a hair
/// longer guarantees one arrives.
const ONE_CADENCE_SECS: u64 = 65;

pub(crate) fn handle_compute(cfg: &KannakaConfig, args: &[String]) {
    let sub = args.get(1).map(String::as_str).unwrap_or("");
    match sub {
        "list" => handle_list(args),
        "status" => handle_status(cfg, args),
        "wake" => handle_wake(cfg, args),
        "grant" => handle_grant(cfg, args),
        "events" => handle_events(cfg, args),
        "identity" => handle_identity(cfg, args),
        "keygen" => handle_keygen(args),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
        }
        "" => {
            eprintln!("{USAGE}");
            process::exit(1);
        }
        other => {
            eprintln!("compute: unknown sub-command '{other}'");
            eprintln!("{USAGE}");
            process::exit(1);
        }
    }
}

// ── argument parsing ───────────────────────────────────────────────────────

/// Minimal parser: positionals in order, `--flag value` pairs, and a fixed
/// set of boolean switches. `--nats-url` is left in `args` for
/// `resolve_nats_url` and also recorded here. Unknown `--flags` are a
/// usage error (exit 2) rather than a silent no-op.
struct Parsed {
    positionals: Vec<String>,
    values: std::collections::HashMap<String, String>,
    switches: std::collections::HashSet<String>,
}

impl Parsed {
    fn parse(args: &[String], start: usize, bool_flags: &[&str], value_flags: &[&str]) -> Parsed {
        let mut p = Parsed {
            positionals: Vec::new(),
            values: Default::default(),
            switches: Default::default(),
        };
        let mut i = start;
        while i < args.len() {
            let a = args[i].as_str();
            if let Some(name) = a.strip_prefix("--") {
                if bool_flags.contains(&name) {
                    p.switches.insert(name.to_string());
                    i += 1;
                } else if value_flags.contains(&name) || name == "nats-url" {
                    let v = super::flag_value(args, i, a, USAGE);
                    p.values.insert(name.to_string(), v.to_string());
                    i += 2;
                } else {
                    eprintln!("compute: unknown flag {a}");
                    eprintln!("{USAGE}");
                    process::exit(2);
                }
            } else {
                p.positionals.push(a.to_string());
                i += 1;
            }
        }
        p
    }

    fn has(&self, s: &str) -> bool {
        self.switches.contains(s)
    }

    fn value(&self, s: &str) -> Option<&str> {
        self.values.get(s).map(String::as_str)
    }

    fn secs(&self, flag: &str, default: Option<u64>) -> Option<u64> {
        match self.value(flag) {
            None => default,
            Some(v) => match v.parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    eprintln!("--{flag}: invalid value '{v}' (whole seconds)");
                    process::exit(2);
                }
            },
        }
    }

    fn positional(&self, idx: usize, what: &str) -> &str {
        match self.positionals.get(idx) {
            Some(s) => s.as_str(),
            None => {
                eprintln!("compute: missing <{what}>");
                eprintln!("{USAGE}");
                process::exit(2);
            }
        }
    }
}

fn require_machine(id: &str) {
    if let Err(e) = ce::validate_machine_id(id) {
        eprintln!("compute: {e}");
        process::exit(2);
    }
}

// ── keys ───────────────────────────────────────────────────────────────────

fn default_key_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("KAX_OPERATOR_KEY") {
        if !p.is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(ce::DEFAULT_KEY_RELPATH)
}

fn load_seed(explicit: Option<&str>) -> [u8; 32] {
    let path = explicit
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_key_path);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("compute: cannot read operator key {}: {e}", path.display());
            eprintln!("  mint one with `kannaka compute keygen` and add its pubkey to the manager's trusted_keys.json");
            process::exit(1);
        }
    };
    match ce::parse_seed_hex(&contents) {
        Ok(seed) => seed,
        Err(e) => {
            eprintln!("compute: operator key {} is not a 32-byte hex seed: {e}", path.display());
            process::exit(1);
        }
    }
}

fn handle_keygen(args: &[String]) {
    let p = Parsed::parse(args, 2, &[], &["out", "signer"]);
    let path = p
        .value("out")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_key_path);
    let signer = p
        .value("signer")
        .map(String::from)
        .unwrap_or_else(|| {
            let user = std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_else(|_| "operator".into());
            format!("operator-{}", user.to_lowercase())
        });
    let mut seed = [0u8; 32];
    {
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut seed);
    }
    let pubkey = match ce::write_seed_file(&path, &seed) {
        Ok(pk) => pk,
        Err(e) => {
            eprintln!("compute keygen: {e}");
            process::exit(1);
        }
    };
    println!("operator key written: {}", path.display());
    println!("public key (hex):     {}", ce::hex_encode(&pubkey));
    println!();
    println!("trusted_keys.json entry for the manager (merge into /srv/kax/manager/trusted_keys.json on the host,");
    println!("then `systemctl restart kax-manager` — trusted keys load once at manager start; only the roster hot-reloads):");
    println!("  {}", ce::trusted_keys_snippet(&signer, &pubkey));
    println!();
    println!("then sign with:  kannaka compute wake <machine> \"...\" --signer {signer}");
    #[cfg(unix)]
    println!("(file mode 0600; the seed is never printed)");
    #[cfg(not(unix))]
    println!("(the seed is never printed; on Windows the file inherits your profile ACL — keep it out of shared folders)");
}

// ── HTTP: roster ───────────────────────────────────────────────────────────

fn kax_api_base() -> String {
    std::env::var("KAX_API_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_KAX_API.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn fetch_roster() -> Result<Vec<serde_json::Value>, String> {
    let url = format!("{}/api/compute/machines", kax_api_base());
    let body = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?
        .into_string()
        .map_err(|e| format!("read {url}: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("{url}: not JSON: {e}"))?;
    let machines = v
        .get("machines")
        .and_then(|m| m.as_array())
        .ok_or_else(|| format!("{url}: no `machines` array in response"))?;
    Ok(machines.clone())
}

fn human_age(secs: i64) -> String {
    if secs < 0 {
        return "future".into();
    }
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

fn age_of_rfc3339(s: Option<&str>) -> String {
    match s.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) {
        Some(t) => human_age((chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds()),
        None => "-".into(),
    }
}

fn handle_list(args: &[String]) {
    let p = Parsed::parse(args, 2, &["json"], &[]);
    let machines = match fetch_roster() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("compute list: {e}");
            process::exit(1);
        }
    };
    if p.has("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Array(machines)).unwrap_or_default()
        );
        return;
    }
    if machines.is_empty() {
        println!("(roster is empty)");
        return;
    }
    // Balance is in KAX credits (1 credit = 1,000,000 minor units). Credits
    // are internal accounting, never a money amount or an exchange rate.
    println!(
        "{:<14} {:<11} {:>12} {:>5}  {:<18} {:>7}  HOST",
        "MACHINE", "STATE", "BALANCE(cr)", "JOBS", "LAST EVENT", "AGE"
    );
    for m in &machines {
        let s = |k: &str| m.get(k).and_then(|v| v.as_str()).unwrap_or("-").to_string();
        let balance = m
            .get("balanceCredits")
            .and_then(|v| v.as_f64())
            .or_else(|| m.get("balance_minor").and_then(|v| v.as_i64()).map(ce::credits_from_minor));
        let jobs = m.get("jobsServed").and_then(|v| v.as_i64()).unwrap_or(0);
        println!(
            "{:<14} {:<11} {:>12} {:>5}  {:<18} {:>7}  {}",
            kannaka_memory::sanitize_display(&s("machineId")),
            kannaka_memory::sanitize_display(&s("state")),
            balance.map(|b| format!("{b:.4}")).unwrap_or_else(|| "-".into()),
            jobs,
            kannaka_memory::sanitize_display(&s("lastEvent")),
            age_of_rfc3339(m.get("lastEventAt").and_then(|v| v.as_str())),
            kannaka_memory::sanitize_display(&s("host")),
        );
    }
}

// ── envelope building (shared by wake/grant) ───────────────────────────────

fn now_ts() -> ce::Ts {
    ce::Ts::Int(chrono::Utc::now().timestamp())
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn print_dry_run(signed: &ce::Signed, subject: &str) {
    println!("subject:   {subject}");
    println!("canonical: {}", signed.canonical);
    println!("sig:       {}", signed.sig_hex);
    println!(
        "envelope:  {}",
        serde_json::to_string(&signed.envelope).unwrap_or_default()
    );
    println!("(dry-run: nothing published)");
}

/// Wire the same signing path for wake and grant: build, sign, dry-run or
/// publish. Returns the signed envelope for the caller's wait loop.
fn sign_or_exit(unsigned: &serde_json::Value, key: Option<&str>) -> ce::Signed {
    let seed = load_seed(key);
    match ce::sign_envelope(unsigned, &seed) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("compute: cannot sign envelope: {e}");
            process::exit(1);
        }
    }
}

fn handle_wake(cfg: &KannakaConfig, args: &[String]) {
    let p = Parsed::parse(args, 2, &["dry-run", "json"], &["wait", "signer", "key"]);
    let machine = p.positional(0, "machine").to_string();
    let prompt = p.positional(1, "prompt").to_string();
    require_machine(&machine);
    if prompt.trim().is_empty() {
        eprintln!("compute wake: prompt is empty");
        process::exit(2);
    }
    let signer = p.value("signer").unwrap_or(ce::DEFAULT_SIGNER);
    let id = new_id();
    let unsigned = ce::job_envelope(&machine, &id, now_ts(), &prompt, signer);
    let signed = sign_or_exit(&unsigned, p.value("key"));
    let subject = ce::inbox_subject(&machine);
    if p.has("dry-run") {
        print_dry_run(&signed, &subject);
        return;
    }
    let wait = p.secs("wait", None);
    publish_and_wait(cfg, args, &machine, &signed, wait, WaitFor::Job, p.has("json"));
}

fn handle_grant(cfg: &KannakaConfig, args: &[String]) {
    let p = Parsed::parse(
        args,
        2,
        &["dry-run", "allow-fraction", "json"],
        &["wait", "signer", "key"],
    );
    let machine = p.positional(0, "machine").to_string();
    let amount = p.positional(1, "credits").to_string();
    require_machine(&machine);
    let credits = match ce::parse_credits(&amount, p.has("allow-fraction")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("compute grant: {e}");
            process::exit(2);
        }
    };
    let signer = p.value("signer").unwrap_or(ce::DEFAULT_SIGNER);
    let id = new_id();
    let unsigned = ce::grant_envelope(&machine, &id, now_ts(), credits, signer);
    let signed = sign_or_exit(&unsigned, p.value("key"));
    let subject = ce::inbox_subject(&machine);
    if p.has("dry-run") {
        print_dry_run(&signed, &subject);
        return;
    }
    let wait = p.secs("wait", None);
    publish_and_wait(cfg, args, &machine, &signed, wait, WaitFor::Grant, p.has("json"));
}

#[derive(Clone, Copy, PartialEq)]
enum WaitFor {
    Job,
    Grant,
}

// ── NATS (feature-gated) ───────────────────────────────────────────────────

#[cfg(feature = "nats")]
mod bus {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use kannaka_memory::nats::{SubEvent, SwarmTransport};

    /// Credentials for the bus. Ambient `NATS_USER`/`NATS_PASSWORD` win (the
    /// transport reads them itself); otherwise `~/.kannaka-nats.env`
    /// (`KEY=VALUE` lines, `export` prefix and quotes tolerated) is loaded
    /// and passed explicitly. `None` means "use whatever the transport
    /// finds", which for a bare shell is anonymous — and anon cannot
    /// subscribe `KAX.>` on the ADR-0042 bus, so we say so on failure.
    pub fn creds() -> Option<(String, String)> {
        let env_user = std::env::var("NATS_USER").unwrap_or_default();
        let env_pass = std::env::var("NATS_PASSWORD").unwrap_or_default();
        if !env_user.is_empty() && !env_pass.is_empty() {
            return None;
        }
        let path = match std::env::var("KANNAKA_NATS_ENV") {
            Ok(p) if !p.is_empty() => std::path::PathBuf::from(p),
            _ => dirs::home_dir()?.join(".kannaka-nats.env"),
        };
        let text = std::fs::read_to_string(path).ok()?;
        let mut user = None;
        let mut pass = None;
        for line in text.lines() {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                match k.trim() {
                    "NATS_USER" => user = Some(v),
                    "NATS_PASSWORD" => pass = Some(v),
                    _ => {}
                }
            }
        }
        Some((user?, pass?))
    }

    pub fn connect(url: &str) -> Result<SwarmTransport, String> {
        SwarmTransport::connect_with_creds(url, creds()).map_err(|e| format!("{url}: {e}"))
    }

    pub struct Received {
        pub subject: String,
        pub payload: serde_json::Value,
    }

    /// Subscribe every subject on its own connection (the transport is
    /// one-subscription-per-socket) and fan messages into one channel until
    /// `deadline`. Returns once every subscription is CONFIRMED by the
    /// broker, so a caller may publish immediately afterwards without
    /// racing its own reply. `None` deadline = follow forever.
    pub fn subscribe_all(
        url: &str,
        subjects: &[String],
        deadline: Option<Instant>,
    ) -> Result<mpsc::Receiver<Received>, String> {
        let (tx, rx) = mpsc::channel::<Received>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String, String>>();
        for subj in subjects {
            let url = url.to_string();
            let subj = subj.clone();
            let tx = tx.clone();
            let ready_tx = ready_tx.clone();
            std::thread::spawn(move || {
                let transport = match connect(&url) {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("connect for {subj}: {e}")));
                        return;
                    }
                };
                let mut sub = match transport.subscribe(&subj) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("subscribe {subj}: {e}")));
                        return;
                    }
                };
                let _ = sub.set_timeout(Some(Duration::from_millis(500)));
                let _ = ready_tx.send(Ok(subj.clone()));
                loop {
                    if let Some(d) = deadline {
                        if Instant::now() >= d {
                            return;
                        }
                    }
                    match sub.next_event() {
                        SubEvent::Msg(msg) => {
                            let text = String::from_utf8_lossy(&msg.payload);
                            let payload = serde_json::from_str(&text).unwrap_or_else(|_| {
                                serde_json::Value::String(kannaka_memory::sanitize_display(&text))
                            });
                            if tx
                                .send(Received {
                                    subject: kannaka_memory::sanitize_display(&msg.subject),
                                    payload,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        SubEvent::Timeout => continue,
                        SubEvent::Closed => {
                            eprintln!("[compute] {subj}: connection closed");
                            return;
                        }
                    }
                }
            });
        }
        drop(ready_tx);
        for _ in subjects {
            match ready_rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    return Err(format!(
                        "{e}\n  (KAX.> needs authenticated NATS credentials: NATS_USER/NATS_PASSWORD or ~/.kannaka-nats.env)"
                    ))
                }
                Err(_) => return Err("subscription not confirmed within 20s".into()),
            }
        }
        Ok(rx)
    }

    /// Publish and confirm the broker processed it (PING/PONG round-trip
    /// after the write). `publish` alone buffers on a dead socket and would
    /// report success for bytes that never left this machine.
    pub fn publish_confirmed(url: &str, subject: &str, payload: &[u8]) -> Result<(), String> {
        let t = connect(url)?;
        t.publish(subject, payload).map_err(|e| format!("publish {subject}: {e}"))?;
        t.ping().map_err(|e| format!("publish {subject}: broker did not acknowledge: {e}"))?;
        Ok(())
    }

    /// `--last N` history probe. No JetStream stream retains `KAX.>` on the
    /// production bus (checked 2026-09-01: 13 streams, none covering KAX),
    /// so this normally reports "not retained"; if a `KAX_EVENTS` stream is
    /// ever created it starts working without a code change.
    pub fn history(url: &str, subject: &str, n: usize) -> Result<Vec<serde_json::Value>, String> {
        let t = connect(url)?;
        if !t.has_jetstream() {
            return Err("JetStream not readable on this connection".into());
        }
        t.get_stream_messages("KAX_EVENTS", subject, n)
            .map_err(|e| format!("no retained history ({e})"))
    }
}

#[cfg(feature = "nats")]
fn nats_url(cfg: &KannakaConfig, args: &[String]) -> String {
    super::resolve_nats_url(args, 0, &cfg.swarm.nats_url)
}

fn event_line(ev: &serde_json::Value) -> String {
    let ts = ev
        .get("ts")
        .and_then(|t| t.as_f64())
        .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
        .map(|t| t.format("%Y-%m-%d %H:%M:%SZ").to_string())
        .unwrap_or_else(|| "-".into());
    let name = ev.get("event").and_then(|e| e.as_str()).unwrap_or("?");
    let machine = ev.get("machine").and_then(|e| e.as_str()).unwrap_or("-");
    let mut extras = Vec::new();
    for key in ["reason", "id", "balance_minor", "minor", "tokens", "runtime_s", "signer", "grant_id"] {
        if let Some(v) = ev.get(key) {
            let shown = match (key, v) {
                ("balance_minor", serde_json::Value::Number(n)) | ("minor", serde_json::Value::Number(n)) => {
                    format!("{:.4}cr", ce::credits_from_minor(n.as_i64().unwrap_or(0)))
                }
                (_, serde_json::Value::String(s)) => kannaka_memory::sanitize_display(s),
                (_, other) => other.to_string(),
            };
            let label = match key {
                "balance_minor" => "balance",
                "minor" => "delta",
                k => k,
            };
            extras.push(format!("{label}={shown}"));
        }
    }
    format!(
        "{ts}  {:<10} {:<18} {}",
        kannaka_memory::sanitize_display(machine),
        kannaka_memory::sanitize_display(name),
        extras.join(" ")
    )
}

#[cfg(feature = "nats")]
fn publish_and_wait(
    cfg: &KannakaConfig,
    args: &[String],
    machine: &str,
    signed: &ce::Signed,
    wait: Option<u64>,
    what: WaitFor,
    want_json: bool,
) {
    use std::time::{Duration, Instant};

    let url = nats_url(cfg, args);
    let subject = ce::inbox_subject(machine);
    let id = signed.envelope["id"].as_str().unwrap_or("").to_string();
    let wire = serde_json::to_string(&signed.envelope).unwrap_or_default();

    // Subscribe BEFORE publishing so a fast reply cannot slip past us.
    let rx = match wait {
        Some(secs) => {
            let deadline = Instant::now() + Duration::from_secs(secs);
            let mut subjects = vec![ce::events_subject(machine)];
            if what == WaitFor::Job {
                subjects.push(ce::outbox_subject(machine));
            }
            match bus::subscribe_all(&url, &subjects, Some(deadline)) {
                Ok(rx) => Some((rx, deadline)),
                Err(e) => {
                    eprintln!("compute: {e}");
                    process::exit(1);
                }
            }
        }
        None => None,
    };

    if let Err(e) = bus::publish_confirmed(&url, &subject, wire.as_bytes()) {
        eprintln!("compute: {e}");
        process::exit(1);
    }
    eprintln!("[compute] published {subject} id={id} signer={}", signed.envelope["signer"].as_str().unwrap_or("?"));

    let Some((rx, deadline)) = rx else {
        println!("{id}");
        return;
    };

    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let msg = match rx.recv_timeout(left) {
            Ok(m) => m,
            Err(_) => break,
        };
        let ev = &msg.payload;
        if msg.subject.ends_with(".events") {
            let name = ev.get("event").and_then(|e| e.as_str()).unwrap_or("");
            let ev_id = ev.get("id").and_then(|e| e.as_str()).unwrap_or("");
            let grant_id = ev.get("grant_id").and_then(|e| e.as_str()).unwrap_or("");
            match name {
                "job_rejected" => {
                    // Rejections carry no id (the manager may not have got
                    // that far), so any rejection inside our window after
                    // our publish is attributed to this envelope.
                    let reason = ev.get("reason").and_then(|r| r.as_str()).unwrap_or("?");
                    if want_json {
                        println!("{ev}");
                    } else {
                        eprintln!("REJECTED: {}", kannaka_memory::sanitize_display(reason));
                        eprintln!("  {}", event_line(ev));
                    }
                    process::exit(2);
                }
                "job_in" if ev_id == id => eprintln!("[compute] accepted — machine waking"),
                "credit" if what == WaitFor::Grant && grant_id == id => {
                    if want_json {
                        println!("{ev}");
                    } else {
                        println!("{}", event_line(ev));
                    }
                    return;
                }
                "job_out" if ev_id == id => eprintln!("[compute] job_out — reply on the way"),
                _ if !want_json => eprintln!("[compute] {}", event_line(ev)),
                _ => {}
            }
            continue;
        }
        // outbox
        let reply_id = ev.get("id").and_then(|e| e.as_str()).unwrap_or("");
        if reply_id != id {
            continue;
        }
        if want_json {
            println!("{ev}");
        } else {
            let reply = ev.get("reply").and_then(|r| r.as_str()).unwrap_or("");
            println!("{}", kannaka_memory::sanitize_display(reply));
            let tokens = ev
                .get("usage")
                .and_then(|u| u.get("total_tokens"))
                .and_then(|t| t.as_i64());
            let elapsed = ev.get("elapsed_s").and_then(|t| t.as_f64());
            eprintln!(
                "[compute] tokens={} elapsed={}",
                tokens.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
                elapsed.map(|t| format!("{t:.1}s")).unwrap_or_else(|| "-".into())
            );
        }
        return;
    }
    eprintln!(
        "compute: no {} within {}s (id={id}); the machine may still be waking — watch `kannaka compute events {machine}`",
        if what == WaitFor::Job { "reply" } else { "credit event" },
        wait.unwrap_or(0)
    );
    process::exit(3);
}

#[cfg(not(feature = "nats"))]
fn publish_and_wait(
    _cfg: &KannakaConfig,
    _args: &[String],
    _machine: &str,
    _signed: &ce::Signed,
    _wait: Option<u64>,
    _what: WaitFor,
    _want_json: bool,
) {
    eprintln!("compute: publishing requires the 'nats' feature (use --dry-run to inspect the envelope)");
    process::exit(1);
}

#[cfg(feature = "nats")]
fn handle_status(cfg: &KannakaConfig, args: &[String]) {
    use std::time::{Duration, Instant};

    let p = Parsed::parse(args, 2, &["json"], &["wait"]);
    let wait = p.secs("wait", Some(ONE_CADENCE_SECS)).unwrap_or(ONE_CADENCE_SECS);
    let url = nats_url(cfg, args);
    let deadline = Instant::now() + Duration::from_secs(wait);
    let subjects = vec![
        ce::STATUS_SUBJECT.to_string(),
        "KAX.machine.*.events".to_string(),
    ];
    let rx = match bus::subscribe_all(&url, &subjects, Some(deadline)) {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("compute status: {e}");
            process::exit(1);
        }
    };
    eprintln!("[compute] waiting up to {wait}s for a fleet snapshot on {} (published every 60s)", ce::STATUS_SUBJECT);
    let mut last_event: std::collections::BTreeMap<String, serde_json::Value> = Default::default();
    let mut snapshot: Option<serde_json::Value> = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(msg) = rx.recv_timeout(left) else { break };
        if msg.subject == ce::STATUS_SUBJECT {
            snapshot = Some(msg.payload);
            break;
        }
        if let Some(m) = msg.payload.get("machine").and_then(|m| m.as_str()) {
            last_event.insert(m.to_string(), msg.payload.clone());
        }
    }
    let Some(snap) = snapshot else {
        eprintln!(
            "compute status: no snapshot within {wait}s — the manager on skywave may be down; `kannaka compute list` shows the last mirrored state"
        );
        if !last_event.is_empty() {
            for ev in last_event.values() {
                println!("{}", event_line(ev));
            }
        }
        process::exit(3);
    };
    if p.has("json") {
        let out = serde_json::json!({
            "snapshot": snap,
            "events_seen": last_event,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return;
    }
    let host = snap.get("host").and_then(|h| h.as_str()).unwrap_or("?");
    let age = snap
        .get("ts")
        .and_then(|t| t.as_f64())
        .map(|t| human_age(chrono::Utc::now().timestamp() - t as i64))
        .unwrap_or_else(|| "-".into());
    println!("host {}  snapshot age {age}", kannaka_memory::sanitize_display(host));
    if let Some(hm) = snap.get("host_memory") {
        if let Some(obj) = hm.as_object() {
            let brief: Vec<String> = obj
                .iter()
                .filter(|(_, v)| v.is_number())
                .map(|(k, v)| format!("{}={}", kannaka_memory::sanitize_display(k), v))
                .collect();
            if !brief.is_empty() {
                println!("host memory: {}", brief.join(" "));
            }
        }
    }
    println!(
        "{:<14} {:<9} {:>12} {:>5}  LAST EVENT (this window)",
        "MACHINE", "STATE", "BALANCE(cr)", "JOBS"
    );
    let machines = snap.get("machines").and_then(|m| m.as_object());
    match machines {
        Some(map) if !map.is_empty() => {
            for (id, st) in map {
                let running = st.get("running").and_then(|r| r.as_bool()).unwrap_or(false);
                let bal = st.get("balance_minor").and_then(|b| b.as_i64());
                let jobs = st.get("jobs_served").and_then(|j| j.as_i64()).unwrap_or(0);
                let state = match (running, bal) {
                    (true, _) => "active",
                    (false, Some(b)) if b <= 0 => "suspended",
                    (false, _) => "hibernated",
                };
                let last = last_event
                    .get(id)
                    .and_then(|e| e.get("event"))
                    .and_then(|e| e.as_str())
                    .unwrap_or("-");
                println!(
                    "{:<14} {:<9} {:>12} {:>5}  {}",
                    kannaka_memory::sanitize_display(id),
                    state,
                    bal.map(|b| format!("{:.4}", ce::credits_from_minor(b))).unwrap_or_else(|| "-".into()),
                    jobs,
                    kannaka_memory::sanitize_display(last)
                );
            }
        }
        _ => println!("(snapshot lists no machines)"),
    }
}

#[cfg(feature = "nats")]
fn handle_events(cfg: &KannakaConfig, args: &[String]) {
    use std::time::{Duration, Instant};

    let p = Parsed::parse(args, 2, &["follow", "json"], &["wait", "last"]);
    let machine = p.positional(0, "machine").to_string();
    require_machine(&machine);
    let url = nats_url(cfg, args);
    let subject = ce::events_subject(&machine);
    let want_json = p.has("json");

    if let Some(n) = p.value("last") {
        let n: usize = match n.parse() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("--last: invalid value '{n}'");
                process::exit(2);
            }
        };
        match bus::history(&url, &subject, n) {
            Ok(rows) if !rows.is_empty() => {
                for ev in rows {
                    if want_json {
                        println!("{ev}");
                    } else {
                        println!("{}", event_line(&ev));
                    }
                }
            }
            Ok(_) => eprintln!(
                "[compute] no retained history for {subject}: KAX.> is plain (non-JetStream) traffic on the bus, so --last only works if a KAX_EVENTS stream is created; showing live only"
            ),
            Err(e) => eprintln!(
                "[compute] history is not retained on the bus for {subject}: {e}\n  (plain subjects only; the manager re-publishes a full snapshot every 60s — see `kannaka compute status`)"
            ),
        }
    }

    let follow = p.has("follow");
    let wait = p.secs("wait", Some(30)).unwrap_or(30);
    let deadline = if follow { None } else { Some(Instant::now() + Duration::from_secs(wait)) };
    let rx = match bus::subscribe_all(&url, std::slice::from_ref(&subject), deadline) {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("compute events: {e}");
            process::exit(1);
        }
    };
    if follow {
        eprintln!("[compute] following {subject} (Ctrl+C to stop)");
    } else {
        eprintln!("[compute] tailing {subject} for {wait}s (--follow for continuous)");
    }
    let mut n = 0usize;
    loop {
        let msg = match deadline {
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match rx.recv_timeout(left) {
                    Ok(m) => m,
                    Err(_) => break,
                }
            }
            None => match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            },
        };
        n += 1;
        if want_json {
            println!("{}", msg.payload);
        } else {
            println!("{}", event_line(&msg.payload));
        }
    }
    if !follow && n == 0 {
        eprintln!("[compute] no events in {wait}s (a hibernated machine is silent until woken)");
    }
}

#[cfg(feature = "nats")]
fn handle_identity(cfg: &KannakaConfig, args: &[String]) {
    use std::time::{Duration, Instant};

    let p = Parsed::parse(args, 2, &["json"], &["wait"]);
    let machine = p.positional(0, "machine").to_string();
    require_machine(&machine);
    let want_json = p.has("json");

    // 1. The KAX roster mirrors the announced pubkey — instant when present.
    let mut pubkey: Option<String> = None;
    let mut source = "roster";
    match fetch_roster() {
        Ok(machines) => {
            let row = machines
                .iter()
                .find(|m| m.get("machineId").and_then(|v| v.as_str()) == Some(machine.as_str()));
            match row {
                None => eprintln!("[compute] {machine} is not on the KAX roster (checking the bus anyway)"),
                Some(r) => {
                    pubkey = r.get("nostrPubkey").and_then(|v| v.as_str()).map(String::from);
                }
            }
        }
        Err(e) => eprintln!("[compute] roster lookup failed ({e}); checking the bus"),
    }

    // 2. Otherwise wait for the bridge's 60s re-announce on .identity.
    if pubkey.is_none() {
        source = "bus";
        let wait = p.secs("wait", Some(ONE_CADENCE_SECS)).unwrap_or(ONE_CADENCE_SECS);
        let url = nats_url(cfg, args);
        let subject = ce::identity_subject(&machine);
        let deadline = Instant::now() + Duration::from_secs(wait);
        let rx = match bus::subscribe_all(&url, std::slice::from_ref(&subject), Some(deadline)) {
            Ok(rx) => rx,
            Err(e) => {
                eprintln!("compute identity: {e}");
                process::exit(1);
            }
        };
        eprintln!("[compute] waiting up to {wait}s for {subject} (announced every 60s)");
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Ok(msg) = rx.recv_timeout(left) else { break };
            if let Some(pk) = msg.payload.get("nostr_pubkey").and_then(|v| v.as_str()) {
                pubkey = Some(pk.to_string());
                break;
            }
        }
    }

    let Some(pk) = pubkey else {
        eprintln!("compute identity: no Nostr pubkey known for {machine} (roster has none; nothing announced on the bus). The machine may not be bound to an owner yet — see kax-computer bridge/bind_owner.py");
        process::exit(3);
    };
    let pk = kannaka_memory::sanitize_display(&pk);
    #[cfg(feature = "nostr")]
    let npub = kannaka_memory::nostr::npub_from_pubkey_hex(&pk).ok();
    #[cfg(not(feature = "nostr"))]
    let npub: Option<String> = None;
    if want_json {
        println!(
            "{}",
            serde_json::json!({"machine": machine, "nostr_pubkey": pk, "npub": npub, "source": source})
        );
    } else {
        println!("machine:      {machine}");
        println!("nostr pubkey: {pk}");
        if let Some(n) = npub {
            println!("npub:         {n}");
        }
        println!("source:       {source}");
    }
}

#[cfg(not(feature = "nats"))]
fn handle_status(_: &KannakaConfig, _: &[String]) {
    eprintln!("compute status requires the 'nats' feature; `kannaka compute list` (HTTP) still works");
    process::exit(1);
}

#[cfg(not(feature = "nats"))]
fn handle_events(_: &KannakaConfig, _: &[String]) {
    eprintln!("compute events requires the 'nats' feature");
    process::exit(1);
}

#[cfg(not(feature = "nats"))]
fn handle_identity(_: &KannakaConfig, args: &[String]) {
    // Roster-only fallback: still answers when the KAX mirror has the key.
    let p = Parsed::parse(args, 2, &["json"], &["wait"]);
    let machine = p.positional(0, "machine").to_string();
    require_machine(&machine);
    let machines = fetch_roster().unwrap_or_default();
    let pk = machines
        .iter()
        .find(|m| m.get("machineId").and_then(|v| v.as_str()) == Some(machine.as_str()))
        .and_then(|m| m.get("nostrPubkey"))
        .and_then(|v| v.as_str())
        .map(kannaka_memory::sanitize_display);
    match pk {
        Some(pk) if p.has("json") => println!("{}", serde_json::json!({"machine": machine, "nostr_pubkey": pk, "source": "roster"})),
        Some(pk) => println!("nostr pubkey: {pk}"),
        None => {
            eprintln!("compute identity: roster has no pubkey for {machine}; the bus path needs the 'nats' feature");
            process::exit(3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parser_splits_positionals_switches_and_values() {
        let args = argv(&["compute", "wake", "agent001", "hello there", "--wait", "30", "--dry-run", "--nats-url", "nats://x:4222"]);
        let p = Parsed::parse(&args, 2, &["dry-run", "json"], &["wait", "signer", "key"]);
        assert_eq!(p.positionals, vec!["agent001", "hello there"]);
        assert!(p.has("dry-run"));
        assert!(!p.has("json"));
        assert_eq!(p.value("wait"), Some("30"));
        assert_eq!(p.secs("wait", None), Some(30));
        assert_eq!(p.value("nats-url"), Some("nats://x:4222"));
        assert_eq!(p.secs("last", Some(7)), Some(7));
    }

    #[test]
    fn event_line_renders_minor_units_as_credits() {
        let ev = serde_json::json!({"ts": 1756700000.5, "machine": "agent001", "event": "debit",
            "reason": "tokens", "minor": -3080, "balance_minor": 1790568, "tokens": 154});
        let line = event_line(&ev);
        assert!(line.starts_with("2025-09-01 04:13:20Z"), "{line}");
        assert!(line.contains("agent001"));
        assert!(line.contains("debit"));
        assert!(line.contains("delta=-0.0031cr"), "{line}");
        assert!(line.contains("balance=1.7906cr"), "{line}");
        assert!(line.contains("tokens=154"));
        assert!(line.contains("reason=tokens"));
    }

    #[test]
    fn event_line_strips_control_sequences_from_wire_strings() {
        let ev = serde_json::json!({"event": "job_rejected", "machine": "agent001", "reason": "bad\u{1b}[31msig"});
        let line = event_line(&ev);
        assert!(!line.contains('\u{1b}'), "{line}");
    }

    #[test]
    fn age_formatting() {
        assert_eq!(human_age(5), "5s");
        assert_eq!(human_age(125), "2m");
        assert_eq!(human_age(3_725), "1h2m");
        assert_eq!(human_age(90_000), "1d1h");
        assert_eq!(human_age(-1), "future");
        assert_eq!(age_of_rfc3339(None), "-");
        assert_eq!(age_of_rfc3339(Some("garbage")), "-");
    }

    #[test]
    fn dry_run_output_reparses_to_the_same_canonical_bytes() {
        // The `canonical:` line of --dry-run is what an operator would paste
        // into Python to cross-check; it must survive a JSON round-trip.
        let seed = ce::parse_seed_hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f").unwrap();
        let unsigned = ce::job_envelope("agent001", "00000000-0000-4000-8000-000000000001", ce::Ts::Int(1756700000), "h\u{e9}llo \u{1f680}", ce::DEFAULT_SIGNER);
        let signed = ce::sign_envelope(&unsigned, &seed).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&signed.canonical).unwrap();
        assert_eq!(ce::canonical_json(&reparsed).unwrap(), signed.canonical);
        let wire: serde_json::Value = serde_json::from_str(&serde_json::to_string(&signed.envelope).unwrap()).unwrap();
        ce::verify_envelope(&wire, &ce::public_key(&seed)).unwrap();
    }
}
