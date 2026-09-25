//! `kannaka mail …` — ADR-0064 Phase 0 (read-only). No HRM: the mailbox is
//! the record, and the only local state is the `<data_dir>/mail/` sidecar.
//!
//!   kannaka mail accounts [--json]
//!   kannaka mail sync     [--days N] [--account NAME]
//!   kannaka mail status   [--account NAME] [--all] [--json]
//!   kannaka mail thread   <ref> [--bodies]
//!   kannaka mail close    <ref> --note "…"
//!
//! `<ref>` is a thread id (`t:3fa9c1d2e0`, any unique prefix of 4+ hex), a
//! message anchor (`zoho:INBOX:49`), or a Message-ID (`<…@…>`).

use std::path::Path;
use std::process;

use chrono::Utc;
use kannaka_memory::mail::config::MailConfig;
use kannaka_memory::mail::rules::{build_threads, classify, status_all, State, Thread, ThreadStatus};
use kannaka_memory::mail::store::{Closure, MailStore};
use kannaka_memory::mail::sync::{fetch_live, sync_accounts};
use kannaka_memory::mail::MailRef;

const USAGE: &str = "Usage: kannaka mail <accounts|sync|status|thread|close> [args]\n\
  accounts [--json]                         the agent's addresses (never credentials)\n\
  sync [--days N] [--account NAME]          pull headers + flags (default 30 days) into <data_dir>/mail/refs.jsonl\n\
  status [--account NAME] [--all] [--json]  open loops per thread (--all: every thread)\n\
  thread <ref> [--bodies]                   a thread fetched live from the server (--bodies: own words, not stored)\n\
  close <ref> --note \"...\"                  record that an open loop was resolved elsewhere";

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn die(msg: impl std::fmt::Display, code: i32) -> ! {
    eprintln!("mail: {msg}");
    process::exit(code);
}

pub fn handle_mail(data_dir: &Path, args: &[String]) {
    let sub = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let rest = if args.len() > 2 { &args[2..] } else { &[][..] };
    let cfg = MailConfig::load(data_dir).unwrap_or_else(|e| die(e, 1));
    let store = MailStore::new(data_dir);
    match sub {
        "accounts" => accounts(&cfg, rest),
        "sync" => sync(&cfg, &store, rest),
        "status" => status(&cfg, &store, rest),
        "thread" => thread(&cfg, &store, rest),
        "close" => close(&cfg, &store, rest),
        _ => {
            eprintln!("{USAGE}");
            process::exit(2);
        }
    }
}

fn accounts(cfg: &MailConfig, rest: &[String]) {
    if rest.iter().any(|a| a == "--json") {
        let v: Vec<_> = cfg
            .accounts
            .iter()
            .map(|a| serde_json::json!({
                "name": a.name, "address": a.address, "transport": a.transport,
                "host": a.host, "port": a.port, "url": a.url, "credentials_file": a.credentials,
                "credentials_present": kannaka_memory::mail::config::expand_home(&a.credentials).exists(),
            }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"source": cfg.source, "internal_domains": cfg.internal_domains, "accounts": v})).unwrap());
        return;
    }
    println!("accounts ({}):", cfg.source);
    if cfg.accounts.is_empty() {
        println!("  none — write {} or add ~/.kannaka-mail.env", MailConfig::path(Path::new("<data_dir>")).display());
    }
    for a in &cfg.accounts {
        let present = kannaka_memory::mail::config::expand_home(&a.credentials).exists();
        println!("  {}{}", a.describe(), if present { "" } else { "  (MISSING)" });
    }
    if !cfg.internal_domains.is_empty() {
        println!("internal domains (agent-to-agent, never an open loop): {}", cfg.internal_domains.join(", "));
    }
}

fn sync(cfg: &MailConfig, store: &MailStore, rest: &[String]) {
    let days: i64 = match flag_value(rest, "--days") {
        Some(d) => d.parse().unwrap_or_else(|_| die("--days needs a number", 2)),
        None => 30,
    };
    let only = flag_value(rest, "--account");
    let selected: Vec<_> = cfg.accounts.iter().filter(|a| only.map(|o| o == a.name).unwrap_or(true)).collect();
    if selected.is_empty() {
        die(format!("no account{} configured", only.map(|o| format!(" named {o}")).unwrap_or_default()), 2);
    }
    let existing = store.load_refs().unwrap_or_else(|e| die(e, 1));
    let (refs, reports) = sync_accounts(&selected, days, existing, Utc::now());
    let mut failed = 0;
    for r in &reports {
        match r {
            Ok(rep) => {
                let folders: Vec<String> = rep.folders.iter().map(|(f, n)| format!("{f} {n}")).collect();
                println!("{:<8} {}  (bodies read for hashing: {}, unchanged refs reused: {})", rep.account, folders.join(", "), rep.fetched_bodies, rep.reused);
            }
            Err(e) => {
                failed += 1;
                eprintln!("mail: sync FAILED — {e}");
            }
        }
    }
    store.save_refs(&refs).unwrap_or_else(|e| die(e, 1));
    println!("{} refs in {} (headers, flags, body hash; no bodies)", refs.len(), store.refs_path().display());
    if failed > 0 {
        process::exit(1);
    }
}

fn load(cfg: &MailConfig, store: &MailStore) -> (Vec<MailRef>, Vec<Closure>) {
    let refs = store.load_refs().unwrap_or_else(|e| die(e, 1));
    if refs.is_empty() {
        die("no refs yet — run `kannaka mail sync` first", 1);
    }
    let _ = cfg;
    let closures = store.load_closures().unwrap_or_else(|e| die(e, 1));
    (refs, closures)
}

fn fmt_age(d: Option<f64>) -> String {
    match d {
        Some(d) if d < 1.0 => format!("{:.0}h", d * 24.0),
        Some(d) => format!("{d:.1}d"),
        None => "?".into(),
    }
}

fn status(cfg: &MailConfig, store: &MailStore, rest: &[String]) {
    let (refs, closures) = load(cfg, store);
    let only = flag_value(rest, "--account");
    let all = rest.iter().any(|a| a == "--all");
    let json = rest.iter().any(|a| a == "--json");
    let refs: Vec<MailRef> = refs.into_iter().filter(|r| only.map(|o| o == r.account).unwrap_or(true)).collect();
    let mut st = status_all(&refs, &cfg.policy(), &closures, Utc::now());
    st.sort_by(|a, b| a.state.cmp(&b.state).then(b.last_at.cmp(&a.last_at)));
    let shown: Vec<&ThreadStatus> = st.iter().filter(|s| all || s.state.is_open()).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&shown).unwrap());
        return;
    }
    let count = |x: State| st.iter().filter(|s| s.state == x).count();
    println!(
        "{} threads: needs_reply {} · waiting_on_them {} · done {} · ignored {}",
        st.len(), count(State::NeedsReply), count(State::WaitingOnThem), count(State::Done), count(State::Ignored)
    );
    for s in shown {
        let who = s.counterpart.join(", ");
        println!(
            "  {:<15} {:>6}  {}  {:<5} {:<34} {}",
            s.state.as_str(), fmt_age(s.age_days), s.id, s.account, truncate(&who, 34), truncate(&s.subject, 60)
        );
        if all {
            println!("  {:<15} {:>6}  └ {}", "", "", s.reason);
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    let s = s.replace(['\r', '\n'], " ");
    if s.chars().count() <= n {
        s
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

/// Resolve a `<ref>` to exactly one thread.
fn resolve<'a>(threads: &'a [Thread<'a>], r: &str) -> &'a Thread<'a> {
    let r = r.trim();
    let matches: Vec<&Thread> = if let Some(hex) = r.strip_prefix("t:") {
        if hex.len() < 4 {
            die("a thread id prefix needs at least 4 hex digits", 2);
        }
        threads.iter().filter(|t| t.id[2..].starts_with(hex)).collect()
    } else if r.starts_with('<') {
        threads.iter().filter(|t| t.messages.iter().any(|m| m.message_id == r)).collect()
    } else {
        threads.iter().filter(|t| t.messages.iter().any(|m| m.anchor() == r)).collect()
    };
    match matches.len() {
        1 => matches[0],
        0 => die(format!("no thread matches {r}"), 1),
        n => die(format!("{r} matches {n} threads: {}", matches.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(" ")), 2),
    }
}

fn thread(cfg: &MailConfig, store: &MailStore, rest: &[String]) {
    let Some(r) = rest.iter().find(|a| !a.starts_with("--")) else { die(USAGE, 2) };
    let bodies = rest.iter().any(|a| a == "--bodies");
    let (refs, closures) = load(cfg, store);
    let threads = build_threads(&refs);
    let t = resolve(&threads, r);
    let st = classify(t, &cfg.policy(), &closures, Utc::now());
    println!("{}  {}  {} messages  state: {} ({}), age {}", t.id, t.account, t.messages.len(), st.state.as_str(), st.reason, fmt_age(st.age_days));
    println!("root {}", t.key);
    let Some(acct) = cfg.accounts.iter().find(|a| a.name == t.account) else {
        die(format!("account {} is no longer configured", t.account), 1)
    };
    let live = fetch_live(acct, &t.messages, bodies).unwrap_or_else(|e| die(format!("fetch from {}: {e}", acct.name), 1));
    if bodies {
        println!("(own words fetched live for display; not stored. Mail bodies can carry secrets — do not paste this output.)");
    }
    for m in &t.messages {
        let l = live.iter().find(|l| l.anchor == m.anchor());
        println!("\n── {}", m.anchor());
        match l.and_then(|l| l.present.as_ref()) {
            None => println!("   gone: the server no longer has this message (ref kept, resolves to nothing)"),
            Some(h) => {
                println!("   Date:    {}", h.date);
                println!("   From:    {}", h.from);
                println!("   To:      {}", h.to);
                if !h.cc.is_empty() {
                    println!("   Cc:      {}", h.cc);
                }
                println!("   Subject: {}", truncate(&h.subject, 100));
                println!("   Flags:   {}", if h.flags.is_empty() { "(none)".to_string() } else { h.flags.join(" ") });
                if let Some(t) = &h.own_text {
                    for line in t.lines().take(40) {
                        println!("   | {line}");
                    }
                }
            }
        }
    }
}

fn close(cfg: &MailConfig, store: &MailStore, rest: &[String]) {
    let Some(r) = rest.iter().enumerate().find(|(i, a)| !a.starts_with("--") && (*i == 0 || rest[i - 1] != "--note")).map(|(_, a)| a) else {
        die(USAGE, 2)
    };
    let note = flag_value(rest, "--note").map(|s| s.trim()).filter(|s| !s.is_empty()).unwrap_or_else(|| die("close needs --note \"why it is resolved\" (the note is the provenance)", 2));
    let (refs, closures) = load(cfg, store);
    let threads = build_threads(&refs);
    let t = resolve(&threads, r);
    let policy = cfg.policy();
    let before = classify(t, &policy, &closures, Utc::now());
    let c = Closure {
        thread_id: t.id.clone(),
        thread_key: t.key.clone(),
        account: t.account.clone(),
        note: note.to_string(),
        closed_at: Utc::now(),
        covers_through: before.last_at,
    };
    store.append_closure(&c).unwrap_or_else(|e| die(e, 1));
    let mut all = closures;
    all.push(c);
    let after = classify(t, &policy, &all, Utc::now());
    println!("{} {}: {} -> {}  (\"{}\"; recorded in {}, the mailbox is untouched)", t.id, truncate(&before.subject, 50), before.state.as_str(), after.state.as_str(), note, store.closures_path().display());
}
