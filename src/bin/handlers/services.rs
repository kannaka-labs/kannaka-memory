//! `kannaka radio`, `kannaka market`, `kannaka constellation` — handlers
//! that talk to external services (radio HTTP API, GhostSignals markets,
//! constellation overview). Plus the small `http_*` helpers they share.
//!
//! Extracted from `bin/kannaka.rs` in v0.3.30 following the pattern
//! documented in `handlers/substrate.rs`.

use std::process;

use kannaka_memory::config;

use super::{check_kannaktopus_installed, KannakaConfig};


// ---------------------------------------------------------------------------
// GhostSignals response shapes
//
// This file has now drifted from the live API three separate times (#591,
// #593, #594): a renamed route, a renamed collection key, and a wrapped
// payload. Each failed SILENTLY — the CLI printed "?" and 0 rather than an
// error — because every read is a fallback chain ending in a default.
//
// The shape decisions live here as pure functions so the contract can be
// pinned by tests against the exact payloads kannaka-radio emits, instead of
// being rediscovered by a human noticing that a column is always "?".
// ---------------------------------------------------------------------------

/// Unwrap `{ok, market: {...}}`, tolerating an already-unwrapped object.
fn market_payload(v: &serde_json::Value) -> &serde_json::Value {
    if v["market"].is_object() {
        &v["market"]
    } else {
        v
    }
}

/// The leaderboard rows, across every key the API has used.
fn leaderboard_rows(v: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    v.as_array()
        .or_else(|| v["traders"].as_array())
        .or_else(|| v["agents"].as_array())
        .or_else(|| v["leaderboard"].as_array())
}

/// A programming block's human-readable name.
fn block_label(b: &serde_json::Value) -> &str {
    b["label"]
        .as_str()
        .or_else(|| b["name"].as_str())
        .or_else(|| b["block"].as_str())
        .unwrap_or("?")
}

/// A programming block's descriptive text, if any.
fn block_description(b: &serde_json::Value) -> &str {
    b["description"]
        .as_str()
        .or_else(|| b["mood"].as_str())
        .unwrap_or("")
}

// ---------------------------------------------------------------------------

fn http_get(url: &str) -> Result<String, String> {
    ureq::get(url)
        .timeout(std::time::Duration::from_secs(5))
        .call()
        .map_err(|e| format!("HTTP error: {e}"))?
        .into_string()
        .map_err(|e| format!("Read error: {e}"))
}

fn http_get_with_token(url: &str, token: &str) -> Result<String, String> {
    ureq::get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(std::time::Duration::from_secs(5))
        .call()
        .map_err(|e| format!("HTTP error: {e}"))?
        .into_string()
        .map_err(|e| format!("Read error: {e}"))
}

fn http_post_json_with_token(url: &str, body: &str, token: &str) -> Result<String, String> {
    ureq::post(url)
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(std::time::Duration::from_secs(5))
        .send_string(body)
        .map_err(|e| format!("HTTP error: {e}"))?
        .into_string()
        .map_err(|e| format!("Read error: {e}"))
}

// ---------------------------------------------------------------------------
// KAX identity token plumbing (ADR-0041): labs-tier trading requires a
// KAX-issued EdDSA JWT. A human drops one in once (`kannaka market auth <jwt>`,
// minted on the KAX Bots page); the CLI then self-refreshes it against KAX
// `/api/auth/token/refresh` (tokens live 15 min; the refresh lineage is
// server-bounded, default 30 days) so swarm agents can trade unattended.
// ---------------------------------------------------------------------------

/// Minimal base64url (no padding) decoder — enough to read a JWT payload.
fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut rev = [255u8; 256];
    for (i, &c) in TABLE.iter().enumerate() { rev[c as usize] = i as u8; }
    let bytes = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4 + 3);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        let v = rev[b as usize];
        if v == 255 { return None; }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Decode a JWT's payload claims without verifying (verification is the
/// server's job — the CLI only needs exp/sub/kind for UX + refresh timing).
fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let raw = b64url_decode(payload)?;
    serde_json::from_slice(&raw).ok()
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Persist a (new) KAX token to the config file. Loads the on-disk config
/// unmodified (so env overrides aren't baked in) and saves 0600.
fn persist_kax_token(token: &str) {
    let mut on_disk = KannakaConfig::load_unmodified();
    on_disk.ghostsignals.kax_token = token.to_string();
    if let Err(e) = on_disk.save() {
        eprintln!("  warning: could not persist KAX token to config: {e}");
    }
}

/// POST the SpaceChild access token to KAX's federation exchange. Returns the
/// KAX token on success, or (status, message) on failure.
fn post_exchange(url: &str, spacechild_access: &str) -> Result<String, (u16, String)> {
    let body = serde_json::json!({ "spacechild_token": spacechild_access }).to_string();
    match ureq::post(url)
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(12))
        .send_string(&body)
    {
        Ok(resp) => {
            let text = resp.into_string().unwrap_or_default();
            serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["token"].as_str().map(String::from))
                .ok_or((0, "unexpected exchange response".to_string()))
        }
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().map(String::from))
                .unwrap_or(text);
            Err((code, msg))
        }
        Err(e) => Err((0, e.to_string())),
    }
}

/// SpaceChild federation (ADR-0041 Phase B): exchange the stored SpaceChild
/// session (`kannaka identity login`) for a KAX identity token — no browser,
/// no KAX password. Refreshes the SpaceChild session once on a 401 and
/// retries, so a live refresh token keeps an agent trading indefinitely.
fn exchange_spacechild_for_kax(cfg: &KannakaConfig) -> Option<String> {
    use kannaka_memory::identity::{identity_path, AuthClient, IdentityStore};
    let path = identity_path();
    let store = IdentityStore::load(&path).ok().flatten()?;
    let url = format!("{}/api/auth/token/exchange", cfg.ghostsignals.kax_url.trim_end_matches('/'));
    match post_exchange(&url, &store.access_token) {
        Ok(tok) => {
            persist_kax_token(&tok);
            eprintln!("  \u{2713} KAX identity federated via SpaceChild ({})", store.email);
            Some(tok)
        }
        Err((401, _)) => {
            // SpaceChild access token expired — refresh the session, retry once.
            let client = AuthClient::from_env();
            match client.refresh(&store.refresh_token) {
                Ok((access, refresh)) => {
                    let mut s2 = store.clone();
                    s2.access_token = access.clone();
                    s2.refresh_token = refresh;
                    s2.obtained_at = chrono::Utc::now();
                    let _ = s2.save(&path);
                    match post_exchange(&url, &access) {
                        Ok(tok) => {
                            persist_kax_token(&tok);
                            eprintln!("  \u{2713} KAX identity federated via SpaceChild ({})", s2.email);
                            Some(tok)
                        }
                        Err((code, m)) => {
                            eprintln!("  SpaceChild exchange failed after refresh ({code}): {m}");
                            None
                        }
                    }
                }
                Err(e) => {
                    eprintln!("  SpaceChild session expired ({e}). Run: kannaka identity login");
                    None
                }
            }
        }
        Err((code, m)) => {
            eprintln!("  SpaceChild exchange failed ({code}): {m}");
            None
        }
    }
}

/// Return a usable KAX token, self-refreshing when it is within 5 minutes of
/// expiry. Falls back to SpaceChild federation when no token is configured or
/// the refresh lineage is dead — so `kannaka identity login` alone is enough.
/// None only when every path is exhausted.
fn ensure_fresh_kax_token(cfg: &KannakaConfig) -> Option<String> {
    let tok = cfg.ghostsignals.kax_token.trim();
    if tok.is_empty() {
        return exchange_spacechild_for_kax(cfg).or_else(remint_help);
    }
    let exp = jwt_claims(tok).and_then(|c| c["exp"].as_i64()).unwrap_or(0);
    let now = now_epoch();
    if exp - now > 300 {
        return Some(tok.to_string()); // comfortably fresh
    }
    // Within 5 min of expiry (or past it, or unreadable) — try a refresh.
    let url = format!("{}/api/auth/token/refresh", cfg.ghostsignals.kax_url.trim_end_matches('/'));
    let body = serde_json::json!({ "token": tok }).to_string();
    match ureq::post(&url)
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(8))
        .send_string(&body)
    {
        Ok(resp) => {
            let text = resp.into_string().unwrap_or_default();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(fresh) = v["token"].as_str() {
                    persist_kax_token(fresh);
                    return Some(fresh.to_string());
                }
            }
            eprintln!("  KAX token refresh returned an unexpected response.");
            if exp > now { Some(tok.to_string()) } else { exchange_spacechild_for_kax(cfg).or_else(remint_help) }
        }
        Err(e) => {
            if exp > now {
                // Still valid — use it and let a later run retry the refresh.
                eprintln!("  warning: KAX token refresh failed ({e}); using current token");
                Some(tok.to_string())
            } else {
                eprintln!("  KAX token expired and refresh failed: {e}");
                // Dead lineage — federation can mint a fresh one if a
                // SpaceChild session is on disk.
                exchange_spacechild_for_kax(cfg).or_else(remint_help)
            }
        }
    }
}

fn remint_help() -> Option<String> {
    eprintln!("  Get a KAX identity one of two ways:");
    eprintln!("    kannaka identity login                 (SpaceChild SSO — fully automatic after)");
    eprintln!("    kannaka market auth <jwt>              (mint on kax.ninja-portal.com, Bots page)");
    None
}

/// Resolve an outcome argument to the hub's integer index: a bare integer is
/// used as-is; otherwise the market's outcome labels are fetched and matched
/// case-insensitively (so `yes` / `No` work).
fn resolve_outcome_index(base: &str, market_id: &str, outcome: &str) -> Result<(u64, String), String> {
    if let Ok(n) = outcome.parse::<u64>() {
        return Ok((n, outcome.to_string()));
    }
    let url = format!("{base}/api/markets/{market_id}");
    let body = http_get(&url)?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("bad market JSON: {e}"))?;
    let outcomes = v["market"]["outcomes"].as_array()
        .or_else(|| v["outcomes"].as_array())
        .ok_or_else(|| "market has no outcomes array".to_string())?;
    for (i, o) in outcomes.iter().enumerate() {
        let label = o.as_str().unwrap_or("");
        if label.eq_ignore_ascii_case(outcome) {
            return Ok((i as u64, label.to_string()));
        }
    }
    let labels: Vec<&str> = outcomes.iter().filter_map(|o| o.as_str()).collect();
    Err(format!("outcome '{}' not found; this market's outcomes: {}", outcome, labels.join(", ")))
}

// ---------------------------------------------------------------------------
// Radio commands
// ---------------------------------------------------------------------------

/// Resolve the live track + album from /api/state across multiple shapes.
/// Current radio publishes `current.title` / `currentAlbum`; older builds
/// (and the design doc) used `now_playing.title` / `now_playing.album`.
fn pick_track(v: &serde_json::Value) -> (&str, &str) {
    let track = v["now_playing"]["title"].as_str()
        .or_else(|| v["current"]["title"].as_str())
        .unwrap_or("Unknown");
    let album = v["now_playing"]["album"].as_str()
        .or_else(|| v["current"]["album"].as_str())
        .or_else(|| v["currentAlbum"].as_str())
        .unwrap_or("");
    (track, album)
}

pub(crate) fn handle_radio(cfg: &KannakaConfig, args: &[String]) {
    let sub = args.get(1).map(String::as_str).unwrap_or("status");
    let base = &cfg.constellation.radio_url;

    match sub {
        "status" => {
            let url = format!("{base}/api/state");
            match http_get(&url) {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        let (track, album) = pick_track(&v);
                        let block = v["programming_block"].as_str()
                            .or_else(|| v["block"].as_str())
                            .or_else(|| v["currentAlbum"].as_str())
                            .unwrap_or("Unknown");
                        let listeners = v["listeners"].as_u64()
                            .or_else(|| v["listener_count"].as_u64())
                            .unwrap_or(0);
                        println!("  \u{1f3b5} Now Playing: \"{track}\" \u{2014} {album}");
                        println!("  \u{1f4fb} {} | {}", block,
                            chrono::Local::now().format("%I:%M %p"));
                        println!("  \u{1f465} {listeners} listeners");
                    } else {
                        println!("{body}");
                    }
                }
                Err(e) => {
                    eprintln!("  Radio not reachable: {e}");
                    eprintln!("  URL: {url}");
                    process::exit(1);
                }
            }
        }
        "now" => {
            let url = format!("{base}/api/state");
            match http_get(&url) {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        let (track, album) = pick_track(&v);
                        println!("  \u{1f3b5} \"{track}\" \u{2014} {album}");
                    } else {
                        println!("{body}");
                    }
                }
                Err(e) => {
                    eprintln!("  Radio not reachable: {e}");
                    process::exit(1);
                }
            }
        }
        "schedule" => {
            let url = format!("{base}/api/programming");
            match http_get(&url) {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        println!("  \u{1f4fb} Kannaka Radio \u{2014} 24/7 Programming Schedule");
                        println!("  {}", "\u{2500}".repeat(50));
                        if let Some(blocks) = v.as_array().or_else(|| v["blocks"].as_array()).or_else(|| v["schedule"].as_array()) {
                            for block in blocks {
                                // /api/programming returns getStatus(), whose
                                // schedule entries are {start, end, label, mood,
                                // albums} — `label`, not `name`/`block`, so every
                                // row printed "?". Older keys kept. (#591)
                                let name = block_label(block);
                                let time = block["time"].as_str()
                                    .or_else(|| block["start"].as_str())
                                    .unwrap_or("");
                                // No `description` in the live shape either; `mood`
                                // is the closest human-readable field. (#591)
                                let desc = block_description(block);
                                if desc.is_empty() {
                                    println!("  {time:>8}  {name}");
                                } else {
                                    println!("  {time:>8}  {name} \u{2014} {desc}");
                                }
                            }
                        } else {
                            println!("{}", serde_json::to_string_pretty(&v).unwrap_or(body));
                        }
                    } else {
                        println!("{body}");
                    }
                }
                Err(e) => {
                    eprintln!("  Radio not reachable: {e}");
                    process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("Usage: kannaka radio <status|now|schedule>");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Market commands
// ---------------------------------------------------------------------------

pub(crate) fn handle_market(cfg: &KannakaConfig, args: &[String]) {
    let sub = args.get(1).map(String::as_str).unwrap_or("list");
    // GhostSignals lives at `cfg.ghostsignals.hub_url` — falling back to
    // `cfg.constellation.radio_url` only when hub_url is empty (legacy
    // single-host configs). Pre-fix every market command hit radio_url
    // and operators couldn't split GhostSignals onto a different host
    // even though the config schema said they could. (#86)
    let base = if cfg.ghostsignals.hub_url.is_empty() {
        &cfg.constellation.radio_url
    } else {
        &cfg.ghostsignals.hub_url
    };
    let token = &cfg.ghostsignals.token;

    // buy works with EITHER a KAX identity token (labs-tier, attributed) or the
    // legacy GhostSignals token (play tier). create/portfolio keep the legacy
    // requirement for now.
    // buy proceeds even with nothing configured: ensure_fresh_kax_token can
    // federate a KAX identity from a stored SpaceChild session on the fly.
    if token.is_empty() && matches!(sub, "create" | "portfolio") {
        eprintln!("  GhostSignals token not configured.");
        eprintln!("  Run 'kannaka init' to register with GhostSignals.");
        process::exit(1);
    }

    match sub {
        "auth" => {
            let jwt = match args.get(2) {
                Some(j) => j.trim().to_string(),
                None => {
                    eprintln!("Usage: kannaka market auth <kax-identity-jwt>");
                    eprintln!("  Mint one at kax.ninja-portal.com (Bots page → Identity Token).");
                    process::exit(1);
                }
            };
            let claims = match jwt_claims(&jwt) {
                Some(c) if c["sub"].is_string() && c["kind"].is_string() => c,
                _ => {
                    eprintln!("  That does not look like a KAX identity token (expected a JWT with sub + kind claims).");
                    process::exit(1);
                }
            };
            persist_kax_token(&jwt);
            let kind = claims["kind"].as_str().unwrap_or("?");
            let sub_c = claims["sub"].as_str().unwrap_or("?");
            let exp = claims["exp"].as_i64().unwrap_or(0);
            let mins = (exp - now_epoch()).max(0) / 60;
            println!("  \u{2713} KAX identity stored.");
            if kind == "agent" {
                println!("  Principal: kax:agent:{}", claims["bot_id"].as_str().unwrap_or("?"));
            } else {
                println!("  Principal: kax:{kind}:{sub_c}");
            }
            println!("  Token expires in ~{mins} min — the CLI auto-refreshes it before market calls.");
        }
        "link" => {
            // Force a SpaceChild -> KAX federation exchange right now.
            match exchange_spacechild_for_kax(cfg) {
                Some(tok) => {
                    if let Some(c) = jwt_claims(&tok) {
                        let kind = c["kind"].as_str().unwrap_or("?");
                        println!("  Principal: kax:{}:{}", kind, c["sub"].as_str().unwrap_or("?"));
                    }
                    println!("  Federated token stored; market commands will self-refresh it.");
                }
                None => {
                    eprintln!("  No usable SpaceChild session. Run: kannaka identity login");
                    process::exit(1);
                }
            }
        }
        "whoami" => {
            let tok = cfg.ghostsignals.kax_token.trim();
            if tok.is_empty() {
                println!("  No KAX identity configured. Run: kannaka market auth <jwt>");
                return;
            }
            match jwt_claims(tok) {
                Some(c) => {
                    let kind = c["kind"].as_str().unwrap_or("?");
                    let principal = if kind == "agent" {
                        format!("kax:agent:{}", c["bot_id"].as_str().unwrap_or("?"))
                    } else {
                        format!("kax:{}:{}", kind, c["sub"].as_str().unwrap_or("?"))
                    };
                    let now = now_epoch();
                    let exp = c["exp"].as_i64().unwrap_or(0);
                    let oat = c["oat"].as_i64().or_else(|| c["iat"].as_i64()).unwrap_or(now);
                    println!("  Principal:      {principal}");
                    println!("  Token expires:  {} min (auto-refreshed before market calls)", (exp - now).max(0) / 60);
                    println!("  Lineage age:    {} day(s) (server refuses refresh past its max lifetime)", (now - oat).max(0) / 86400);
                    if let Some(scopes) = c["scopes"].as_array() {
                        let s: Vec<&str> = scopes.iter().filter_map(|x| x.as_str()).collect();
                        println!("  Scopes:         {}", s.join(", "));
                    }
                }
                None => println!("  Stored KAX token is unreadable — re-run: kannaka market auth <jwt>"),
            }
        }
        "list" => {
            let url = format!("{base}/api/markets");
            match http_get(&url) {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        let markets = v.as_array()
                            .or_else(|| v["markets"].as_array());
                        if let Some(markets) = markets {
                            let total = markets.len();
                            let display: Vec<_> = markets.iter().take(10).collect();
                            println!("  \u{1f4ca} Active Prediction Markets ({} of {})", display.len(), total);
                            println!();
                            println!("  {:<14} {:<44} {:>6} {:>6}",
                                "ID", "Question", "Price", "Vol");
                            println!("  {}", "\u{2500}".repeat(74));
                            for m in &display {
                                let id = m["id"].as_str()
                                    .or_else(|| m["market_id"].as_str())
                                    .unwrap_or("?");
                                let q = m["question"].as_str()
                                    .or_else(|| m["title"].as_str())
                                    .unwrap_or("?");
                                let price = m["price"].as_f64()
                                    .or_else(|| m["last_price"].as_f64())
                                    .unwrap_or(0.0);
                                let vol = m["volume"].as_u64().unwrap_or(0);
                                let q_trunc = if q.len() > 42 {
                                    let mut end = 42;
                                    while end > 0 && !q.is_char_boundary(end) { end -= 1; }
                                    format!("{}...", &q[..end])
                                } else {
                                    q.to_string()
                                };
                                println!("  {id:<14} {q_trunc:<44} {price:>5.2} {vol:>6}");
                            }
                        } else {
                            println!("{}", serde_json::to_string_pretty(&v).unwrap_or(body));
                        }
                    } else {
                        println!("{body}");
                    }
                }
                Err(e) => {
                    eprintln!("  GhostSignals not reachable: {e}");
                    process::exit(1);
                }
            }
        }
        "view" => {
            let market_id = match args.get(2) {
                Some(id) => id,
                None => {
                    eprintln!("Usage: kannaka market view <market-id>");
                    process::exit(1);
                }
            };
            let url = format!("{base}/api/markets/{market_id}");
            match http_get(&url) {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        // GhostSignals wraps the payload: {ok, market: {...}}.
                        // Reading the top level found nothing, so every field
                        // silently rendered as "?" / 0. Fall back to `v` itself
                        // so an unwrapped shape still works. (#594)
                        let m = market_payload(&v);
                        let q = m["question"].as_str()
                            .or_else(|| m["title"].as_str())
                            .unwrap_or("?");
                        let price = m["price"].as_f64()
                            .or_else(|| m["last_price"].as_f64())
                            .unwrap_or(0.0);
                        let vol = m["volume"].as_u64().unwrap_or(0);
                        let created = m["created_at"].as_str().unwrap_or("?");
                        let resolved = m["resolved"].as_bool().unwrap_or(false);

                        println!("  \u{1f4ca} Market: {market_id}");
                        println!("  {}", "\u{2500}".repeat(50));
                        println!("  Question: {q}");
                        println!("  Price:    {price:.2}");
                        println!("  Volume:   {vol}");
                        println!("  Created:  {created}");
                        println!("  Resolved: {}", if resolved { "Yes" } else { "No" });

                        if let Some(outcomes) = v["outcomes"].as_array() {
                            println!();
                            println!("  Outcomes:");
                            for o in outcomes {
                                let name = o["name"].as_str().unwrap_or("?");
                                let p = o["price"].as_f64().unwrap_or(0.0);
                                println!("    {name}: {p:.2}");
                            }
                        }
                    } else {
                        println!("{body}");
                    }
                }
                Err(e) => {
                    eprintln!("  GhostSignals not reachable: {e}");
                    process::exit(1);
                }
            }
        }
        "buy" => {
            let market_id = match args.get(2) {
                Some(id) => id,
                None => {
                    eprintln!("Usage: kannaka market buy <market-id> <outcome> <shares>");
                    process::exit(1);
                }
            };
            let outcome = match args.get(3) {
                Some(o) => o,
                None => {
                    eprintln!("Usage: kannaka market buy <market-id> <outcome> <shares>");
                    process::exit(1);
                }
            };
            // Strict-parse the quantity — a typo like "1O" used to silently
            // buy 1 share. Absent quantity keeps the historical default of 1.
            let shares: u64 = match args.get(4) {
                None => 1,
                Some(s) => match s.parse() {
                    Ok(n) => n,
                    Err(_) => {
                        eprintln!("  market buy: <shares> expects a whole number, got: {s}");
                        eprintln!("  Usage: kannaka market buy <market-id> <outcome> <shares>");
                        process::exit(1);
                    }
                },
            };

            // The hub takes the outcome as an INTEGER index; accept a bare
            // index or a label (yes/no) resolved against the market.
            let (outcome_idx, outcome_label) = match resolve_outcome_index(base, market_id, outcome) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("  {e}");
                    process::exit(1);
                }
            };

            // Prefer the KAX identity token (labs-tier: the hub verifies it and
            // derives the trader from the claims); fall back to the legacy
            // GhostSignals token for play-tier markets.
            let kax = ensure_fresh_kax_token(cfg);
            let bearer: &str = match &kax {
                Some(t) => t.as_str(),
                None if !token.is_empty() => token.as_str(),
                None => {
                    // ensure_fresh_kax_token already printed the re-mint help.
                    process::exit(1);
                }
            };

            let url = format!("{base}/api/markets/{market_id}/trade");
            // trader_id rides along for play-tier markets; on labs-tier the hub
            // overwrites it with the KAX-derived principal.
            let body = serde_json::json!({
                "outcome": outcome_idx,
                "shares": shares,
                "trader_id": cfg.agent.id,
            }).to_string();
            match http_post_json_with_token(&url, &body, bearer) {
                Ok(resp) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        if v["ok"].as_bool() == Some(false) {
                            eprintln!("  Trade rejected: {}", v["error"].as_str().unwrap_or("unknown error"));
                            process::exit(1);
                        }
                        let cost = v["cost"].as_f64().unwrap_or(0.0);
                        println!("  \u{2713} Bought {shares} share(s) of '{outcome_label}' on {market_id}");
                        println!("  Cost: {cost:.4} credits");
                        if let Some(prices) = v["prices"].as_array() {
                            let p: Vec<String> = prices.iter()
                                .filter_map(|x| x.as_f64())
                                .map(|x| format!("{x:.2}"))
                                .collect();
                            println!("  New prices: [{}]", p.join(", "));
                        }
                        if v["cost_minor"].is_string() {
                            println!("  Ledger: debit posted on the KAX credit ledger (labs-tier).");
                        }
                    } else {
                        println!("{resp}");
                    }
                }
                Err(e) => {
                    eprintln!("  Trade failed: {e}");
                    if e.contains("401") || e.contains("403") {
                        // Two credentials reach this route (kannaka-radio#304):
                        // a KAX identity token for labs tier, and the row's own
                        // hub bearer for play tier. Naming only the first sent
                        // operators to re-mint a JWT when the real problem was a
                        // stale `ghostsignals.token`.
                        eprintln!("  (labs-tier markets need a KAX identity token: kannaka market auth <jwt>)");
                        eprintln!("  (play-tier needs THIS row's hub bearer in ghostsignals.token; once a row holds one,");
                        eprintln!("   naming the trader is no longer enough. If the stored token is not the row's, only");
                        eprintln!("   an operator can clear it: POST /api/agents/{}/bearer/reset, then `kannaka init`)", cfg.agent.id);
                    } else if e.contains("409") {
                        eprintln!("  (insufficient credits, or you proposed this market — proposers can't trade their own markets)");
                    }
                    process::exit(1);
                }
            }
        }
        "propose" => {
            let statement = match args.get(2) {
                Some(s) => s.clone(),
                None => {
                    eprintln!("Usage: kannaka market propose \"<falsifiable claim>\" --by YYYY-MM-DD [--category <topic>]");
                    eprintln!("  Files a market proposal to Kannaka Labs, attributed to your KAX identity.");
                    eprintln!("  The settle-by date is required; a future date auto-opens it into a funded market.");
                    eprintln!("  You cannot trade a market you proposed (anti-self-dealing).");
                    process::exit(1);
                }
            };
            const PROPOSE_USAGE: &str =
                "Usage: kannaka market propose \"<claim>\" --by YYYY-MM-DD [--category <topic>]";
            let mut settles_by: Option<String> = None;
            let mut category: Option<String> = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--by" | "--settles-by" | "--due" => {
                        settles_by = Some(super::parse_flag_value::<String>(args, i, "--by", PROPOSE_USAGE));
                        i += 2;
                    }
                    "--category" | "--cat" => {
                        category = Some(super::parse_flag_value::<String>(args, i, "--category", PROPOSE_USAGE));
                        i += 2;
                    }
                    other => {
                        if other.starts_with("--") {
                            eprintln!("[market propose] ignoring unknown flag: {other}");
                        }
                        i += 1;
                    }
                }
            }
            if settles_by.is_none() {
                eprintln!("  market propose: a settle-by date is required (--by YYYY-MM-DD).");
                eprintln!("  {PROPOSE_USAGE}");
                process::exit(1);
            }
            // Proof-forward (multichannel-ingress-design.md): the observatory door
            // verifies OUR KAX identity and derives the canonical principal
            // (kax:agent:<bot> / kax:user:<sub>) — the SAME id we trade as, which
            // is what makes the anti-self-dealing guard fire on a market we propose.
            // Refresh the token first so a long-lived shell still authenticates.
            let kax = match ensure_fresh_kax_token(cfg) {
                Some(t) => t,
                None => {
                    eprintln!("  Proposing needs a KAX identity. Run: kannaka market auth <jwt>");
                    process::exit(1);
                }
            };
            let url = format!("{}/api/predictions/propose", cfg.constellation.observatory_url);
            let body = serde_json::json!({
                "statement": statement,
                "settlesBy": settles_by,
                "category": category,
            })
            .to_string();
            match http_post_json_with_token(&url, &body, &kax) {
                Ok(resp) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        let p = &v["prediction"];
                        let num = p["number"]
                            .as_i64()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        let status = p["status"].as_str().unwrap_or("proposed");
                        if v["duplicate"].as_bool() == Some(true) {
                            println!("  \u{2713} That claim is already an active market (prediction \u{2116}{num}).");
                        } else {
                            println!("  \u{2713} Proposed prediction \u{2116}{num} \u{2014} status: {status}");
                            println!("  \"{statement}\"");
                            if let Some(sb) = &settles_by {
                                println!("  Settles by: {sb}");
                            }
                            println!(
                                "  You can't trade this one (anti-self-dealing); watch it at {}",
                                cfg.constellation.observatory_url
                            );
                        }
                    } else {
                        println!("{resp}");
                    }
                }
                Err(e) => {
                    eprintln!("  Proposal failed: {e}");
                    if e.contains("401") {
                        eprintln!("  (proposing needs a KAX identity token: kannaka market auth <jwt>)");
                    }
                    process::exit(1);
                }
            }
        }
        "create" => {
            let question = match args.get(2) {
                Some(q) => q.clone(),
                None => {
                    eprintln!("Usage: kannaka market create \"question\" [--ttl 3600]");
                    process::exit(1);
                }
            };
            const CREATE_USAGE: &str = "Usage: kannaka market create \"question\" [--ttl 3600]";
            let mut ttl: u64 = 3600;
            let mut i = 3;
            while i < args.len() {
                if args[i] == "--ttl" {
                    ttl = super::parse_flag_value(args, i, "--ttl", CREATE_USAGE);
                    i += 2;
                } else {
                    if args[i].starts_with("--") {
                        eprintln!("[market create] ignoring unknown flag: {}", args[i]);
                    }
                    i += 1;
                }
            }
            let url = format!("{base}/api/markets");
            let body = serde_json::json!({
                "question": question,
                "ttl_seconds": ttl,
            }).to_string();
            match http_post_json_with_token(&url, &body, token) {
                Ok(resp) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        let id = v["id"].as_str()
                            .or_else(|| v["market_id"].as_str())
                            .unwrap_or("?");
                        println!("  \u{2713} Market created: {id}");
                        println!("  Question: {question}");
                        println!("  TTL: {ttl} seconds");
                    } else {
                        println!("{resp}");
                    }
                }
                Err(e) => {
                    eprintln!("  Market creation failed: {e}");
                    process::exit(1);
                }
            }
        }
        "portfolio" => {
            let url = format!("{}/api/agents/{}/portfolio",
                base, cfg.agent.id);
            match http_get_with_token(&url, token) {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        let capital = v["capital"].as_f64()
                            .or_else(|| v["balance"].as_f64())
                            .unwrap_or(0.0);
                        let reputation = v["reputation"].as_f64().unwrap_or(0.0);

                        println!("  \u{1f4b0} Portfolio for {}", cfg.agent.id);
                        println!("  {}", "\u{2500}".repeat(40));
                        println!("  Capital:    {capital:.2} ghost coins");
                        println!("  Reputation: {reputation:.2}");

                        if let Some(positions) = v["positions"].as_array() {
                            if !positions.is_empty() {
                                println!();
                                println!("  Positions:");
                                for p in positions {
                                    let mid = p["market_id"].as_str().unwrap_or("?");
                                    let outcome = p["outcome"].as_str().unwrap_or("?");
                                    let shares = p["shares"].as_u64().unwrap_or(0);
                                    println!("    {mid} | {outcome} | {shares} shares");
                                }
                            }
                        }
                    } else {
                        println!("{body}");
                    }
                }
                Err(e) => {
                    eprintln!("  GhostSignals not reachable: {e}");
                    process::exit(1);
                }
            }
        }
        "leaderboard" => {
            // GhostSignals serves this at /api/leaderboard. /api/agents/leaderboard
            // does not exist on kannaka-radio, so this 404'd every time. (#593)
            let url = format!("{base}/api/leaderboard");
            match http_get(&url) {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        // The live response is {ok, traders, count} — `traders`
                        // was missing from this chain, so even against the right
                        // route the list came back empty. Older keys kept. (#593)
                        let agents = leaderboard_rows(&v);
                        if let Some(agents) = agents {
                            println!("  \u{1f3c6} GhostSignals Leaderboard");
                            println!("  {}", "\u{2500}".repeat(50));
                            println!("  {:<4} {:<20} {:>10} {:>10}",
                                "#", "Agent", "Capital", "Rep");
                            for (i, a) in agents.iter().take(20).enumerate() {
                                let name = a["agent_id"].as_str()
                                    .or_else(|| a["display_name"].as_str())
                                    .unwrap_or("?");
                                let capital = a["capital"].as_f64()
                                    .or_else(|| a["balance"].as_f64())
                                    .unwrap_or(0.0);
                                let rep = a["reputation"].as_f64().unwrap_or(0.0);
                                println!("  {:<4} {:<20} {:>9.2} {:>9.2}",
                                    i + 1, name, capital, rep);
                            }
                        } else {
                            println!("{}", serde_json::to_string_pretty(&v).unwrap_or(body));
                        }
                    } else {
                        println!("{body}");
                    }
                }
                Err(e) => {
                    eprintln!("  GhostSignals not reachable: {e}");
                    process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("Usage: kannaka market <list|view|buy|propose|create|portfolio|leaderboard|auth|link|whoami>");
            eprintln!("  propose \"<claim>\" --by YYYY-MM-DD [--category <topic>]");
            eprintln!("               file a market proposal to Kannaka Labs (attributed to your KAX identity)");
            eprintln!("  auth <jwt>   store a KAX identity token (labs-tier trading; auto-refreshed)");
            eprintln!("  link         federate a KAX identity from your SpaceChild login (kannaka identity login)");
            eprintln!("  whoami       show the stored KAX principal + token/lineage status");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Constellation command
// ---------------------------------------------------------------------------

pub(crate) fn handle_constellation(cfg: &KannakaConfig) {
    let obs_url = format!("{}/api/constellation", cfg.constellation.observatory_url);
    println!("  \u{1f310} Kannaka Constellation Status");
    println!("  {}", "\u{2500}".repeat(60));

    match http_get(&obs_url) {
        Ok(body) => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                // Try to render structured constellation data
                if let Some(services) = v.as_array()
                    .or_else(|| v["services"].as_array())
                    .or_else(|| v["apps"].as_array())
                {
                    for svc in services {
                        let name = svc["name"].as_str().unwrap_or("?");
                        let url = svc["url"].as_str().unwrap_or("");
                        let status = svc["status"].as_str().unwrap_or("unknown");
                        let detail = svc["detail"].as_str()
                            .or_else(|| svc["info"].as_str())
                            .unwrap_or("");
                        let mark = if status == "up" || status == "ok" || status == "connected" {
                            "\u{2713}"
                        } else {
                            "\u{2717}"
                        };
                        if detail.is_empty() {
                            println!("  {mark} {name:<16} {url:<34}");
                        } else {
                            println!("  {mark} {name:<16} {url:<34} {detail}");
                        }
                    }
                } else {
                    // Flat JSON — just print it
                    println!("{}", serde_json::to_string_pretty(&v).unwrap_or(body));
                }
            } else {
                println!("{body}");
            }
        }
        Err(_) => {
            // Observatory unavailable — build a local status from what we can check
            // Radio
            let radio_url = format!("{}/api/state", cfg.constellation.radio_url);
            let radio_ok = http_get(&radio_url).is_ok();
            println!("  {} {:<16} {:<34}",
                if radio_ok { "\u{2713}" } else { "\u{2717}" },
                "Radio",
                cfg.constellation.radio_url);

            // Observatory
            println!("  \u{2717} {:<16} {:<34} not reachable",
                "Observatory",
                cfg.constellation.observatory_url);

            // Memory (local)
            let data_dir = config::KannakaConfig::data_dir();
            let hrm_path = data_dir.join("kannaka.hrm");
            if hrm_path.exists() {
                println!("  \u{2713} {:<16} {:<34}", "Memory", "local HRM");
            } else {
                println!("  \u{2717} {:<16} {:<34}", "Memory", "no HRM file");
            }

            // GhostSignals — honor cfg.ghostsignals.hub_url first. (#86)
            let gs_base = if cfg.ghostsignals.hub_url.is_empty() {
                &cfg.constellation.radio_url
            } else {
                &cfg.ghostsignals.hub_url
            };
            let gs_url = format!("{gs_base}/api/markets");
            let gs_ok = http_get(&gs_url).is_ok();
            println!("  {} {:<16} {:<34}",
                if gs_ok { "\u{2713}" } else { "\u{2717}" },
                "GhostSignals",
                if gs_ok { "markets available" } else { "not reachable" });

            // Kannaktopus
            let ktopus = check_kannaktopus_installed();
            println!("  {} {:<16} {:<34}",
                if ktopus { "\u{2713}" } else { "\u{2717}" },
                "Kannaktopus",
                if ktopus { "installed" } else { "not installed" });
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // The payloads below are the shapes kannaka-radio actually emits, taken
    // from server/routes.js — not invented. That is the point: these tests
    // fail if the client and the live API drift apart again.

    #[test]
    fn market_view_reads_the_wrapped_payload() {
        // GET /api/markets/:id → sendJson(200, { ok: true, market: m })
        let live = json!({
            "ok": true,
            "market": { "question": "Will X ship?", "price": 0.62, "volume": 41 }
        });
        let m = market_payload(&live);
        assert_eq!(m["question"].as_str(), Some("Will X ship?"));
        assert_eq!(m["price"].as_f64(), Some(0.62));
        assert_eq!(m["volume"].as_u64(), Some(41));
    }

    #[test]
    fn market_view_still_reads_an_unwrapped_payload() {
        let flat = json!({ "question": "Direct", "price": 0.5 });
        assert_eq!(market_payload(&flat)["question"].as_str(), Some("Direct"));
    }

    #[test]
    fn leaderboard_reads_the_traders_key() {
        // GET /api/leaderboard → sendJson(200, { ok: true, traders, count })
        let live = json!({ "ok": true, "traders": [{ "agent_id": "kannaka" }], "count": 1 });
        let rows = leaderboard_rows(&live).expect("traders should be found");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["agent_id"].as_str(), Some("kannaka"));
    }

    #[test]
    fn leaderboard_still_reads_the_older_shapes() {
        for v in [
            json!([{ "agent_id": "a" }]),
            json!({ "agents": [{ "agent_id": "a" }] }),
            json!({ "leaderboard": [{ "agent_id": "a" }] }),
        ] {
            assert_eq!(leaderboard_rows(&v).map(|r| r.len()), Some(1));
        }
    }

    #[test]
    fn leaderboard_absent_is_none_not_empty() {
        // Distinguishable from "present but empty", so the caller can say
        // "no data" rather than printing an empty table as if it were the
        // answer.
        assert!(leaderboard_rows(&json!({ "ok": false })).is_none());
        assert_eq!(leaderboard_rows(&json!({ "traders": [] })).map(|r| r.len()), Some(0));
    }

    #[test]
    fn schedule_block_uses_the_live_field_names() {
        // getStatus().schedule entries: {start, end, label, mood, albums}
        let block = json!({
            "start": "06:00", "end": "09:00",
            "label": "Dawn Chorus", "mood": "ambient", "albums": ["a"]
        });
        assert_eq!(block_label(&block), "Dawn Chorus");
        assert_eq!(block_description(&block), "ambient");
    }

    #[test]
    fn schedule_block_falls_back_to_older_names() {
        assert_eq!(block_label(&json!({ "name": "N" })), "N");
        assert_eq!(block_label(&json!({ "block": "B" })), "B");
        assert_eq!(block_description(&json!({ "description": "D" })), "D");
    }

    #[test]
    fn schedule_block_unknown_shape_is_marked_not_blank() {
        // "?" is deliberate: a blank name would read as a nameless block
        // rather than as missing data.
        assert_eq!(block_label(&json!({ "unexpected": 1 })), "?");
        assert_eq!(block_description(&json!({ "unexpected": 1 })), "");
    }
}
