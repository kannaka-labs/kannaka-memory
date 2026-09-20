//! `kannaka orchestrate`, `kannaka config`, `kannaka search`, `kannaka export`,
//! `kannaka import` — ops-utility handlers.
//!
//! Extracted from `bin/kannaka.rs` in v0.3.31 following the pattern
//! documented in `handlers/substrate.rs`.

use std::process;

use super::{check_kannaktopus_installed, flag_value, parse_flag_value, warn_if_readonly, KannakaConfig};

pub(crate) fn handle_orchestrate(args: &[String]) {
    if !check_kannaktopus_installed() {
        eprintln!("  Kannaktopus is not installed.");
        eprintln!("  Install it with: npm install -g kannaktopus");
        process::exit(1);
    }

    let sub = args.get(1).map(String::as_str).unwrap_or("status");

    match sub {
        "run" => {
            let task = match args.get(2) {
                Some(t) => t,
                None => {
                    eprintln!("Usage: kannaka orchestrate run \"task description\"");
                    process::exit(1);
                }
            };
            let status = std::process::Command::new("kannaktopus")
                .args(["run", task])
                .status();
            match status {
                Ok(s) if !s.success() => {
                    process::exit(s.code().unwrap_or(1));
                }
                Err(e) => {
                    eprintln!("  Failed to run kannaktopus: {e}");
                    process::exit(1);
                }
                _ => {}
            }
        }
        "status" => {
            let status = std::process::Command::new("kannaktopus")
                .arg("status")
                .status();
            match status {
                Ok(s) if !s.success() => {
                    process::exit(s.code().unwrap_or(1));
                }
                Err(e) => {
                    eprintln!("  Failed to run kannaktopus: {e}");
                    process::exit(1);
                }
                _ => {}
            }
        }
        "agents" => {
            let status = std::process::Command::new("kannaktopus")
                .arg("agents")
                .status();
            match status {
                Ok(s) if !s.success() => {
                    process::exit(s.code().unwrap_or(1));
                }
                Err(e) => {
                    eprintln!("  Failed to run kannaktopus: {e}");
                    process::exit(1);
                }
                _ => {}
            }
        }
        _ => {
            eprintln!("Usage: kannaka orchestrate <run|status|agents>");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Config commands
// ---------------------------------------------------------------------------

pub(crate) fn handle_config(cfg: &KannakaConfig, args: &[String]) {
    let sub = args.get(1).map(String::as_str).unwrap_or("show");

    match sub {
        "show" => {
            // Print config with redacted API keys
            let mut display = cfg.clone();
            if display.llm.api_key.len() > 8 {
                display.llm.api_key = format!("{}...", &display.llm.api_key[..8]);
            } else if !display.llm.api_key.is_empty() {
                display.llm.api_key = "***".to_string();
            }
            if display.ghostsignals.token.len() > 8 {
                display.ghostsignals.token = format!("{}...", &display.ghostsignals.token[..8]);
            } else if !display.ghostsignals.token.is_empty() {
                display.ghostsignals.token = "***".to_string();
            }
            if display.ghostsignals.kax_token.len() > 8 {
                display.ghostsignals.kax_token = format!("{}...", &display.ghostsignals.kax_token[..8]);
            } else if !display.ghostsignals.kax_token.is_empty() {
                display.ghostsignals.kax_token = "***".to_string();
            }
            match toml::to_string_pretty(&display) {
                Ok(text) => println!("{text}"),
                Err(e) => {
                    eprintln!("Error serializing config: {e}");
                    process::exit(1);
                }
            }
        }
        "set" => {
            let key = match args.get(2) {
                Some(k) => k,
                None => {
                    eprintln!("Usage: kannaka config set <key> <value>");
                    eprintln!();
                    eprintln!("Keys: agent.id, agent.display_name, llm.provider, llm.model,");
                    eprintln!("      llm.api_key, llm.base_url, swarm.enabled, swarm.nats_url, swarm.role,");
                    eprintln!("      ghostsignals.enabled, ghostsignals.token, ghostsignals.hub_url,");
                    eprintln!("      ghostsignals.kax_url, ghostsignals.kax_token,");
                    eprintln!("      constellation.radio_url, constellation.observatory_url,");
                    eprintln!("      hrm.path, hrm.wavefront_dim, updates.auto_check,");
                    eprintln!("      triage.enabled, triage.redundancy, triage.min_amplitude,");
                    eprintln!("      triage.min_age_hours, triage.max_evict, triage.xi_trigger");
                    eprintln!();
                    eprintln!("Booleans accept: true/false, 1/0, yes/no, on/off (case-insensitive).");
                    process::exit(1);
                }
            };
            let value = match args.get(3) {
                Some(v) => v,
                None => {
                    eprintln!("Usage: kannaka config set <key> <value>");
                    process::exit(1);
                }
            };

            // Parse booleans with a normalized set so users don't silently
            // get `false` for "True", "1", "yes", "TRUE", etc. (#96)
            let parse_bool = |v: &str| -> Result<bool, ()> {
                match v.to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on"  => Ok(true),
                    "false" | "0" | "no" | "off" => Ok(false),
                    _ => Err(()),
                }
            };
            // Critical: re-read the file WITHOUT env overrides so writing
            // one key doesn't silently persist `KANNAKA_AGENT_ID` /
            // `KANNAKA_NATS_URL` / etc. that only exist in the current
            // shell (#99). The caller's `cfg` was loaded through
            // KannakaConfig::load() which applies env precedence, so
            // saving that value back to disk would leak the env into
            // the file.
            let mut new_cfg = KannakaConfig::load_unmodified();
            match key.as_str() {
                "agent.id" => new_cfg.agent.id = value.clone(),
                "agent.display_name" => new_cfg.agent.display_name = value.clone(),
                "llm.provider" => new_cfg.llm.provider = value.clone(),
                "llm.model" => new_cfg.llm.model = value.clone(),
                "llm.api_key" => new_cfg.llm.api_key = value.clone(),
                "llm.base_url" => new_cfg.llm.base_url = value.clone(),
                "swarm.enabled" => {
                    match parse_bool(value) {
                        Ok(b) => new_cfg.swarm.enabled = b,
                        Err(()) => {
                            eprintln!("swarm.enabled expects true/false/1/0/yes/no/on/off, got: {value}");
                            process::exit(1);
                        }
                    }
                }
                "swarm.nats_url" => new_cfg.swarm.nats_url = value.clone(),
                // #112: swarm.role was a settable-looking but unsettable field.
                // It is read at swarm-connect time (announced to the constellation,
                // see config.rs swarm connect log), so make it a real knob.
                "swarm.role" => {
                    let v = value.trim();
                    if v.is_empty() {
                        eprintln!("swarm.role must be a non-empty role label (e.g. queen, worker, witness)");
                        process::exit(1);
                    }
                    new_cfg.swarm.role = v.to_string();
                }
                "ghostsignals.enabled" => {
                    match parse_bool(value) {
                        Ok(b) => new_cfg.ghostsignals.enabled = b,
                        Err(()) => {
                            eprintln!("ghostsignals.enabled expects true/false/1/0/yes/no/on/off, got: {value}");
                            process::exit(1);
                        }
                    }
                }
                "ghostsignals.token"   => new_cfg.ghostsignals.token = value.clone(),
                "ghostsignals.hub_url" => new_cfg.ghostsignals.hub_url = value.clone(),
                "ghostsignals.kax_url" => new_cfg.ghostsignals.kax_url = value.clone(),
                "ghostsignals.kax_token" => new_cfg.ghostsignals.kax_token = value.clone(),
                "constellation.radio_url" => new_cfg.constellation.radio_url = value.clone(),
                "constellation.observatory_url" => new_cfg.constellation.observatory_url = value.clone(),
                "updates.auto_check" => {
                    match parse_bool(value) {
                        Ok(b) => new_cfg.updates.auto_check = b,
                        Err(()) => {
                            eprintln!("updates.auto_check expects true/false/1/0/yes/no/on/off, got: {value}");
                            process::exit(1);
                        }
                    }
                }
                // #94: hrm.path and hrm.wavefront_dim are now writable.
                // hrm.wavefront_dim is parsed as an integer; #93 still
                // tracks whether the runtime actually honors changes here.
                "hrm.path" => new_cfg.hrm.path = value.clone(),
                "hrm.wavefront_dim" => {
                    match value.parse::<u32>() {
                        Ok(n) if n > 0 => new_cfg.hrm.wavefront_dim = n,
                        _ => {
                            eprintln!("hrm.wavefront_dim expects a positive integer, got: {value}");
                            process::exit(1);
                        }
                    }
                }
                // ADR-0031 Phase 3 — per-agent triage policy.
                "triage.enabled" => {
                    match parse_bool(value) {
                        Ok(b) => new_cfg.triage.enabled = b,
                        Err(()) => {
                            eprintln!("triage.enabled expects true/false/1/0/yes/no/on/off, got: {value}");
                            process::exit(1);
                        }
                    }
                }
                "triage.redundancy" => match value.parse::<f32>() {
                    Ok(n) if (0.0..=1.0).contains(&n) => new_cfg.triage.redundancy = n,
                    _ => { eprintln!("triage.redundancy expects a float in [0,1], got: {value}"); process::exit(1); }
                },
                "triage.min_amplitude" => match value.parse::<f32>() {
                    Ok(n) if n >= 0.0 => new_cfg.triage.min_amplitude = n,
                    _ => { eprintln!("triage.min_amplitude expects a non-negative float, got: {value}"); process::exit(1); }
                },
                "triage.min_age_hours" => match value.parse::<i64>() {
                    Ok(n) if n >= 0 => new_cfg.triage.min_age_hours = n,
                    _ => { eprintln!("triage.min_age_hours expects a non-negative integer, got: {value}"); process::exit(1); }
                },
                "triage.max_evict" => match value.parse::<usize>() {
                    Ok(n) => new_cfg.triage.max_evict = n,
                    _ => { eprintln!("triage.max_evict expects a non-negative integer, got: {value}"); process::exit(1); }
                },
                "triage.xi_trigger" => match value.parse::<f32>() {
                    Ok(n) if (0.0..=1.0).contains(&n) => new_cfg.triage.xi_trigger = n,
                    _ => { eprintln!("triage.xi_trigger expects a float in [0,1] (0 disables auto-trigger), got: {value}"); process::exit(1); }
                },
                other => {
                    eprintln!("Unknown config key: {other}");
                    process::exit(1);
                }
            }
            match new_cfg.save() {
                Ok(()) => println!("  \u{2713} Set {key} = {value}"),
                Err(e) => {
                    eprintln!("Error saving config: {e}");
                    process::exit(1);
                }
            }
        }
        "path" => {
            println!("{}", KannakaConfig::config_path().display());
        }
        _ => {
            eprintln!("Usage: kannaka config <show|set|path>");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Search command
// ---------------------------------------------------------------------------

pub(crate) fn handle_search(sys: &kannaka_memory::openclaw::KannakaMemorySystem, args: &[String]) {
    const USAGE: &str = "Usage: kannaka search \"query\" [--limit N] [--json]";
    if args.len() < 2 {
        eprintln!("{USAGE}");
        eprintln!();
        eprintln!("Literal text search over memory content. For semantic/");
        eprintln!("resonance recall use `kannaka recall` instead.");
        process::exit(1);
    }
    let mut limit = 20usize;
    let mut json_out = false;
    let mut query_parts = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" | "--top-k" => {
                limit = parse_flag_value(args, i, args[i].as_str(), USAGE);
                i += 2;
            }
            "--json" => {
                json_out = true;
                i += 1;
            }
            // Accepted everywhere for consistency; search itself never
            // touches NATS.
            "--nats-url" => { let _ = flag_value(args, i, "--nats-url", USAGE); i += 2; }
            other if other.starts_with("--") => {
                // A typo'd flag must NOT silently become part of the query.
                eprintln!("search: unknown flag: {other}");
                eprintln!("{USAGE}");
                process::exit(2);
            }
            _ => {
                query_parts.push(args[i].as_str());
                i += 1;
            }
        }
    }
    let query = query_parts.join(" ");
    match sys.search(&query, limit) {
        Ok(results) => {
            if json_out {
                let arr: Vec<serde_json::Value> = results.iter().map(|r| serde_json::json!({
                    "id":            r.id,
                    "content":       r.content,
                    "score":         r.score,
                    "match_type":    r.match_type,
                    "matched_terms": r.matched_terms,
                    "age_hours":     r.age_hours,
                    "layer":         r.layer,
                })).collect();
                println!("{}", serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into()));
                return;
            }
            if results.is_empty() {
                println!("  No results for \"{query}\"");
                return;
            }
            println!("  \u{1f50d} Search: \"{}\" ({} results)", query, results.len());
            println!("  {}", "\u{2500}".repeat(70));
            for (i, r) in results.iter().enumerate() {
                let preview = if r.content.len() > 70 {
                    let mut end = 70;
                    while end > 0 && !r.content.is_char_boundary(end) { end -= 1; }
                    format!("{}...", &r.content[..end])
                } else {
                    r.content.clone()
                };
                println!("  {:>3}. [{}] score={:.0} {}", i + 1, r.match_type, r.score, preview);
                println!("       id={} age={:.0}h matched={:?}",
                    r.id, r.age_hours, r.matched_terms);
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Export command
// ---------------------------------------------------------------------------

pub(crate) fn handle_export(sys: &mut kannaka_memory::openclaw::KannakaMemorySystem, args: &[String]) {
    const USAGE: &str = "Usage: kannaka export [--output FILE] [--format json]";
    let mut output_path: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            // flag_value errors on a trailing `--output` — pre-fix it was
            // silently dropped and the full HRM JSON went to stdout.
            "--output" | "-o" => {
                output_path = Some(flag_value(args, i, args[i].as_str(), USAGE).to_string());
                i += 2;
            }
            "--format" => {
                // Only JSON is supported, but accept the flag
                let _ = flag_value(args, i, "--format", USAGE);
                i += 2;
            }
            other => {
                if other.starts_with("--") {
                    eprintln!("[export] ignoring unknown flag: {other}");
                }
                i += 1;
            }
        }
    }

    let all_mems = sys.engine.store.all_memories()
        .unwrap_or_else(|e| { eprintln!("Error: {e}"); process::exit(1); });

    let output: Vec<serde_json::Value> = all_mems.iter().map(|m| {
        serde_json::json!({
            "id": m.id.to_string(),
            "content": m.content,
            "amplitude": m.amplitude,
            "frequency": m.frequency,
            "phase": m.phase,
            "decay_rate": m.decay_rate,
            "created_at": m.created_at.to_rfc3339(),
            "layer_depth": m.layer_depth,
            "hallucinated": m.hallucinated,
            "parents": m.parents,
            "vector": m.vector,
            "xi_signature": m.xi_signature,
            "geometry": m.geometry,
            "connections": m.connections.iter().map(|c| {
                serde_json::json!({
                    "target_id": c.target_id.to_string(),
                    "strength": c.strength,
                    "span": c.span
                })
            }).collect::<Vec<_>>()
        })
    }).collect();

    let json_str = serde_json::to_string(&output).unwrap();

    if let Some(path) = output_path {
        match std::fs::write(&path, &json_str) {
            Ok(()) => {
                println!("  \u{2713} Exported {} memories to {}", all_mems.len(), path);
            }
            Err(e) => {
                eprintln!("Error writing {path}: {e}");
                process::exit(1);
            }
        }
    } else {
        println!("{json_str}");
    }
}

// ---------------------------------------------------------------------------
// Import command
// ---------------------------------------------------------------------------

/// Outcome counters from a JSON memory import.
pub(crate) struct ImportSummary {
    pub imported: u32,
    pub skipped: u32,
    pub errors: u32,
    pub total: usize,
}

/// Lossless JSON import shared by `kannaka import` and `kannaka import-json`.
/// Preserves the full wave state written by `export` (amplitude, frequency,
/// phase, decay_rate, created_at, layer_depth, hallucinated, parents,
/// vector, xi_signature). Pre-fix `kannaka import` re-absorbed content +
/// amplitude only, so an export→import round-trip silently rebuilt every
/// memory's wave state.
pub(crate) fn import_memories_from_file(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    path: &str,
) -> ImportSummary {
    let file_data = std::fs::read_to_string(path)
        .unwrap_or_else(|e| { eprintln!("Failed to read {path}: {e}"); process::exit(1); });
    let memories: Vec<serde_json::Value> = serde_json::from_str(&file_data)
        .unwrap_or_else(|e| { eprintln!("Failed to parse JSON: {e}"); process::exit(1); });

    let existing_ids: std::collections::HashSet<uuid::Uuid> = sys.engine.store.all_memories()
        .unwrap_or_default().iter().map(|m| m.id).collect();

    let mut imported = 0u32;
    let mut skipped = 0u32;
    let mut errors = 0u32;

    for val in &memories {
        let id_str = val["id"].as_str().unwrap_or("");
        let id = match uuid::Uuid::parse_str(id_str) {
            Ok(id) => id,
            Err(_) => { errors += 1; continue; }
        };

        if existing_ids.contains(&id) {
            skipped += 1;
            continue;
        }

        let content = val["content"].as_str().unwrap_or("").to_string();
        if content.is_empty() { skipped += 1; continue; }

        let amplitude = val["amplitude"].as_f64().unwrap_or(0.5) as f32;
        let frequency = val["frequency"].as_f64().unwrap_or(1.0) as f32;
        let phase = val["phase"].as_f64().unwrap_or(0.0) as f32;
        let decay_rate = val["decay_rate"].as_f64().unwrap_or(0.001) as f32;
        let created_at = val["created_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);
        let hallucinated = val["hallucinated"].as_bool().unwrap_or(false);

        // Applied AFTER insert, never through it. `HrmStore::insert` drops
        // `modality` on purpose — see its #630 / ADR-0051 note: carrying it
        // across the wire boundary is only safe once `absorb_gate` sanitizes
        // those fields, or a peer gains ungated control of `tier`/`expires_at`.
        // `import-json` reads a local file the operator chose, so it opts in
        // here explicitly rather than loosening the shared insert path. (#553)
        let want_modality: kannaka_memory::medium::types::Modality =
            serde_json::from_value(val["modality"].clone()).unwrap_or_default();

        // Reconstruct vector from JSON array if present, otherwise re-encode
        let vector: Option<Vec<f32>> = val["vector"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect()
        });

        // #949: a record with no vector used to go through `absorb`, which
        // mints a FRESH id and discarded the `id` parsed above — the very id
        // the duplicate check a few lines up tests against. So the skip could
        // never fire on this path, a `--slim` export (which deliberately omits
        // vectors) could not be round-tripped without rewriting every id, and
        // importing the same slim file twice produced two full copies while
        // reporting `Skipped: 0`.
        //
        // Encoding with the store's OWN pipeline instead gives a
        // store-dimensioned vector, so the record can take the same
        // identity-preserving `insert` path as a record that carried one.
        //
        // ⚠ Behaviour change: `absorb` also ran ADR-0049 facet decomposition,
        // and `insert` does not. A slim import therefore no longer mints facets
        // inline — run `kannaka facets backfill --apply` afterwards, which is
        // the supported migration for exactly this shape of corpus. That is the
        // same treatment a full (vectored) export already got, so the two
        // import paths now agree rather than differing silently.
        let vector = match vector {
            Some(v) if !v.is_empty() => v,
            _ => {
                let encoded = sys
                    .engine
                    .store
                    .as_any_mut()
                    .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                    .map(|hrm| hrm.encode_text(&content));
                match encoded {
                    Some(Ok(v)) => v,
                    Some(Err(e)) => {
                        if errors < 5 {
                            eprintln!("  Error encoding {id_str}: {e}");
                        }
                        errors += 1;
                        continue;
                    }
                    // Not an HrmStore (legacy flat backend): fall back to
                    // absorb. The id is NOT preserved on that path — it is
                    // reported rather than hidden.
                    None => match sys.engine.store.absorb(&content, amplitude, None) {
                        Ok(new_id) => {
                            if want_modality
                                != kannaka_memory::medium::types::Modality::default()
                            {
                                if let Some(hrm) = sys
                                    .engine
                                    .store
                                    .as_any_mut()
                                    .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                                {
                                    hrm.set_modality(&new_id, want_modality);
                                }
                            }
                            eprintln!(
                                "  Note: {id_str} re-absorbed on a non-HRM store; its id was not preserved"
                            );
                            imported += 1;
                            continue;
                        }
                        Err(e) => {
                            if errors < 5 {
                                eprintln!("  Error absorbing {id_str}: {e}");
                            }
                            errors += 1;
                            continue;
                        }
                    },
                }
            }
        };

        let xi_sig: Vec<f32> = val["xi_signature"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect()
            })
            .unwrap_or_default();

        let content_clone = content.clone();
        let mut mem = kannaka_memory::memory::HyperMemory::new(vector, content);
        mem.id = id;
        mem.amplitude = amplitude;
        mem.frequency = frequency;
        mem.phase = phase;
        mem.decay_rate = decay_rate;
        mem.created_at = created_at;
        mem.layer_depth = val["layer_depth"].as_u64().unwrap_or(0) as u8;
        mem.hallucinated = hallucinated;
        // Restore NCS modality when the export carried it. Absent or
        // unrecognised ⇒ leave the `Modality::default()` (Unknown) that
        // HyperMemory::new already set: older exports predate the field, and
        // must keep importing rather than failing. (#553)
        mem.modality = serde_json::from_value(val["modality"].clone()).unwrap_or_default();
        mem.parents = val["parents"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        mem.xi_signature = xi_sig;

        // #949: the dimension-mismatch retry below must keep this record's
        // identity too, so hold a populated copy rather than rebuilding a bare
        // one from content alone.
        let retry = mem.clone();

        match sys.engine.store.insert(mem) {
            Ok(new_id) => {
                if want_modality != kannaka_memory::medium::types::Modality::default() {
                    if let Some(hrm) = sys
                                .engine
                                .store
                                .as_any_mut()
                                .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                            {
                                hrm.set_modality(&new_id, want_modality);
                            }
                }
                imported += 1
            }
            Err(e) => {
                // A vector in the JSON that does not match this store's
                // dimensions. Re-encode the content with the store's own
                // pipeline and insert the SAME record — #949: the old code
                // called `absorb` here, which minted a fresh id and dropped the
                // one being imported, so a cross-dimension restore silently
                // rewrote every id it touched.
                let err_str = format!("{e}");
                if err_str.contains("dimension mismatch") {
                    let re_encoded = sys
                        .engine
                        .store
                        .as_any_mut()
                        .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                        .map(|hrm| hrm.encode_text(&content_clone));
                    let outcome = match re_encoded {
                        Some(Ok(v)) => {
                            let mut again = retry;
                            again.vector = v;
                            sys.engine.store.insert(again)
                        }
                        Some(Err(e2)) => Err(e2),
                        // Legacy flat backend: absorb is all there is, and it
                        // does not preserve the id. Say so rather than hide it.
                        None => {
                            eprintln!(
                                "  Note: {id_str} re-absorbed on a non-HRM store; its id was not preserved"
                            );
                            sys.engine.store.absorb(&content_clone, amplitude, None)
                        }
                    };
                    match outcome {
                        Ok(new_id) => {
                            if want_modality
                                != kannaka_memory::medium::types::Modality::default()
                            {
                                if let Some(hrm) = sys
                                    .engine
                                    .store
                                    .as_any_mut()
                                    .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                                {
                                    hrm.set_modality(&new_id, want_modality);
                                }
                            }
                            imported += 1;
                        }
                        Err(e2) => {
                            if errors < 5 {
                                eprintln!("  Error re-encoding {id_str}: {e2}");
                            }
                            errors += 1;
                        }
                    }
                } else {
                    if errors < 5 {
                        eprintln!("  Error importing {id_str}: {e}");
                    }
                    errors += 1;
                }
            }
        }
    }

    // Save
    if imported > 0 {
        if let Err(e) = sys.save() {
            eprintln!("Failed to save: {e}");
            process::exit(1);
        }
        // #730: refresh Observatory's cached counts ONCE for the whole import.
        // This path absorbs straight into the store rather than going through
        // `remember`, so it needs its own call — and doing it here, not
        // per-memory, keeps a 10k-row import to a single cache write.
        sys.refresh_status_cache_counts();
    }

    ImportSummary { imported, skipped, errors, total: memories.len() }
}

pub(crate) fn handle_import(sys: &mut kannaka_memory::openclaw::KannakaMemorySystem, args: &[String]) {
    let path = match args.get(1) {
        Some(p) => p,
        None => {
            eprintln!("Usage: kannaka import <file.json>");
            process::exit(1);
        }
    };
    warn_if_readonly("import");
    let s = import_memories_from_file(sys, path);

    println!("  \u{2713} Import complete");
    println!("    Imported: {}", s.imported);
    println!("    Skipped:  {} (duplicates)", s.skipped);
    println!("    Errors:   {}", s.errors);
    println!("    Total:    {} in file", s.total);
}

#[cfg(test)]
mod import_id_tests {
    use super::*;

    /// #949: a record with no `vector` — the shape `export-json --slim` writes —
    /// must keep the id it carries. The old path called `absorb`, which minted a
    /// fresh id and threw away the one the duplicate check tests against, so
    /// the skip could never fire: importing the same slim file twice produced
    /// two full copies while reporting `Skipped: 0`.
    #[test]
    fn slim_import_preserves_ids_and_is_idempotent() {
        let dir = std::env::temp_dir()
            .join(format!("kannaka_import949_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let id = uuid::Uuid::parse_str("3f2b1c40-9a55-4d21-8e73-6c0af1d2b8e9").unwrap();
        // Deliberately NO "vector" key — this is what --slim omits.
        let slim = serde_json::json!([{
            "id": id.to_string(),
            "content": "the harbor beacon channel moved to twentyseven last spring",
            "amplitude": 0.7,
        }]);
        let json_path = dir.join("slim.json");
        std::fs::write(&json_path, serde_json::to_string(&slim).unwrap()).unwrap();
        let json_path = json_path.to_str().unwrap().to_string();

        let mut sys =
            kannaka_memory::openclaw::KannakaMemorySystem::init(dir.clone()).unwrap();

        let first = import_memories_from_file(&mut sys, &json_path);
        assert_eq!(first.imported, 1, "first import must take the record");
        assert_eq!(first.errors, 0, "no errors expected");

        let ids: Vec<uuid::Uuid> = sys
            .engine
            .store
            .all_memories()
            .unwrap()
            .iter()
            .map(|m| m.id)
            .collect();
        assert!(
            ids.contains(&id),
            "the imported id must be the one from the file, got {ids:?} (#949)"
        );
        let count_after_first = sys.engine.store.count();

        // Second import of the same file: the duplicate check can only fire if
        // the first import actually wrote that id.
        let second = import_memories_from_file(&mut sys, &json_path);
        assert_eq!(second.skipped, 1, "re-import must skip the duplicate (#949)");
        assert_eq!(second.imported, 0, "re-import must add nothing (#949)");
        assert_eq!(
            sys.engine.store.count(),
            count_after_first,
            "re-import must not grow the store (#949)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
