//! `kannaka attention serve` — kannaka-attention beam consumer.
//!
//! Subscribes to KANNAKA.attention.eye, treats each glyph event as a gravity
//! pull on the attention beam. For every event we run a top-K recall on the
//! medium using a synthetic query string built from the glyph's signature
//! (dominant_class + fold_sequence + fano_signature); the resulting memory
//! ids are pushed onto the beam via AttentionBeam::observe. Later callers
//! can use Medium::recall_against_ids(beam.candidates(), q) for O(K) recall.
//!
//! Extracted from `bin/kannaka.rs` in v0.3.28 following the pattern
//! documented in `handlers/substrate.rs`.

use std::process;

use super::{resolve_nats_url, KannakaConfig};
#[cfg(feature = "nats")]
use super::{flag_value, parse_flag_value};
#[cfg(feature = "nats")]
use kannaka_memory::nats::SubEvent;

#[cfg(feature = "nats")]
pub(crate) fn handle_attention_serve(
    sys: &mut kannaka_memory::openclaw::KannakaMemorySystem,
    cfg: &KannakaConfig,
    args: &[String],
) {
    use kannaka_attention::{AttentionBeam, BeamConfig, ObservationEvent};
    use kannaka_attention::landmarks::Landmark;
    use kannaka_attention::salience::RecencyWeightedGate;
    use std::time::{Duration, Instant};

    const USAGE: &str = "Usage: kannaka attention serve [--top-k N] [--subject SUBJ] [--nats-url URL]";

    // Single-writer policy: attention serve is a long-running READER (its
    // recalls mutate observation state in RAM only). Enforce read-only here
    // instead of trusting the operator to export KANNAKA_READONLY.
    std::env::set_var("KANNAKA_READONLY", "1");
    if let Some(hrm) = sys
        .engine
        .store
        .as_any_mut()
        .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
    {
        hrm.set_readonly(true);
    }
    eprintln!("[attention serve] read-only mode enforced (single-writer policy)");

    let mut top_k: usize = 3;
    let mut subject: String = "KANNAKA.attention.eye".to_string();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--top-k" => {
                top_k = parse_flag_value(args, i, "--top-k", USAGE); i += 2;
            }
            "--subject" => {
                subject = flag_value(args, i, "--subject", USAGE).to_string(); i += 2;
            }
            "--nats-url" => { let _ = flag_value(args, i, "--nats-url", USAGE); i += 2; }
            other => {
                if other.starts_with("--") {
                    eprintln!("[attention serve] ignoring unknown flag: {other}");
                }
                i += 1;
            }
        }
    }

    let nats_url = resolve_nats_url(args, 0, &cfg.swarm.nats_url);
    let transport = match kannaka_memory::nats::SwarmTransport::connect(&nats_url) {
        Ok(t) => t,
        Err(e) => {
            // NATS is the sole input to attention-as-gravity: no NATS => no eye
            // glyphs => the beam never warms and gravity never fires. Make the
            // degradation loud, then exit so the supervisor (Restart=always in
            // kannaka-attention.service) retries rather than us busy-spinning a
            // dead socket. The landmark/ear subscriptions below are best-effort
            // and already WARN on their own if NATS is only partially up.
            eprintln!(
                "[attention serve] FATAL: NATS unavailable at {nats_url} ({e}) — \
                 attention-as-gravity OFFLINE (no eye glyphs will be consumed); \
                 exiting for supervisor restart"
            );
            process::exit(1);
        }
    };

    eprintln!("[attention serve] subject={subject} top_k={top_k}");
    super::warn_if_hash_adopted(sys);
    eprintln!("[attention serve] landmarks=KANNAKA.exemplar.>  (best-effort)");
    eprintln!("[attention serve] nats={nats_url} press Ctrl+C to stop");

    let mut sub = match transport.subscribe(&subject) {
        Ok(s) => s,
        Err(e) => { eprintln!("[attention serve] subscribe failed: {e}"); process::exit(1); }
    };
    let _ = sub.set_timeout(Some(Duration::from_secs(2)));

    // Second transport for the exemplar landmark subscription. Each NATS
    // subscription owns the connection's TCP read half so a second subject
    // needs a second connection. Best-effort.
    let lm_transport = kannaka_memory::nats::SwarmTransport::connect(&nats_url).ok();
    let mut lm_sub = lm_transport.as_ref().and_then(|t| t.subscribe("KANNAKA.exemplar.>").ok());
    if let Some(ref mut s) = lm_sub {
        let _ = s.set_timeout(Some(Duration::from_secs(2)));
    } else {
        eprintln!("[attention serve] WARN: no landmark subscription — running eye-only");
    }

    // Third transport for ear events from kannaka-radio's track-change hook.
    let ear_transport = kannaka_memory::nats::SwarmTransport::connect(&nats_url).ok();
    let mut ear_sub = ear_transport.as_ref().and_then(|t| t.subscribe("KANNAKA.attention.ear").ok());
    if let Some(ref mut s) = ear_sub {
        let _ = s.set_timeout(Some(Duration::from_secs(2)));
        eprintln!("[attention serve] ear=KANNAKA.attention.ear (radio track-change)");
    } else {
        eprintln!("[attention serve] WARN: no ear subscription — running eye+landmarks only");
    }

    let mut beam = AttentionBeam::with_config(BeamConfig::default());
    // Install the default recency-weighted salience gate. Landmarks whose
    // exemplar id is in the recency ring get a 2.0× boost; in lookback (not
    // recency), 1.3×; cold landmarks fall back to their intrinsic weight.
    beam.set_gate(Box::new(RecencyWeightedGate::default()));
    eprintln!("[attention serve] salience gate: {}", beam.gate_name());

    // Degradation visibility: glyph-gravity ships OFF by default (gain 0.0), so
    // announce the state once at startup — otherwise a quiet beam is
    // indistinguishable from a disabled gravity loop. (#14)
    #[cfg(feature = "glyph")]
    {
        let gain = std::env::var("KANNAKA_GLYPH_GRAVITY")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0);
        if gain > 0.0 {
            eprintln!("[attention serve] glyph-gravity ENABLED (KANNAKA_GLYPH_GRAVITY={gain}) — same-Fano-line beam pull active (recall-side ranking boost removed in #837: measured net-negative)");
        } else {
            eprintln!("[attention serve] glyph-gravity DISABLED (KANNAKA_GLYPH_GRAVITY unset/0) — beam warms but the same-line pull is inert");
        }
    }

    // Write an initial empty beam dump so the observatory can render
    // "warming up" instead of "daemon offline" while we wait for the
    // first observation.
    let dump_path_init = std::env::var("KANNAKA_ATTENTION_BEAM_FILE")
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "C:\\Users\\Public\\kannaka-attention-beam.json".to_string()
            } else {
                "/tmp/kannaka-attention-beam.json".to_string()
            }
        });
    let _ = std::fs::write(&dump_path_init, serde_json::json!({
        "schema_version": 1,
        "ts": chrono::Utc::now().to_rfc3339(),
        "stats": {"recency_len":0,"lookback_len":0,"landmarks_len":0,"beam_size":0,"observations":0u64},
        "candidates": [],
        "note": "warming up — no observations yet",
    }).to_string());
    let mut last_stats_at = Instant::now();
    // One-shot guard so the "store is not HrmStore" degradation warns once, not
    // per event. (#14)
    #[cfg(feature = "glyph")]
    let mut warned_no_hrm = false;
    let stats_interval = Duration::from_secs(60);

    loop {
        // ── Landmark subscription: each exemplar event upserts a landmark
        // keyed by cluster id. Best-effort: on connection close, degrade to
        // eye-only (mirrors the startup behavior) instead of spinning.
        let mut lm_closed = false;
        if let Some(ref mut lm) = lm_sub {
            match lm.next_event() {
                SubEvent::Timeout => {}
                SubEvent::Closed => {
                    eprintln!("[attention serve] WARN: landmark subscription closed — continuing without landmarks");
                    lm_closed = true;
                }
                SubEvent::Msg(lmmsg) => {
                    if let Ok(payload) = std::str::from_utf8(&lmmsg.payload) {
                        if let Ok(env) = serde_json::from_str::<serde_json::Value>(payload) {
                            let exemplar_id = env.get("exemplar_id").and_then(|v| v.as_str())
                                .and_then(|s| uuid::Uuid::parse_str(s).ok());
                            let cluster_id = env.get("cluster_id").and_then(|v| v.as_u64());
                            let theme = env.get("theme").and_then(|v| v.as_str())
                                .or_else(|| env.get("semantic_summary").and_then(|v| v.as_str()))
                                .unwrap_or("?");
                            let agent = env.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
                            if let (Some(id), Some(cid)) = (exemplar_id, cluster_id) {
                                let label = format!("{agent}/{cid}");
                                let weight = env.get("amplitude").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                                beam.upsert_landmark(Landmark {
                                    id, cluster_label: label.clone(), weight,
                                });
                                eprintln!("[attention serve] landmark + {label} theme=\"{theme}\"");
                            }
                        }
                    }
                }
            }
        }
        if lm_closed {
            lm_sub = None;
        }

        // Poll the ear subscription. Each track-change event from radio
        // becomes an ear observation: build a query from track title +
        // album, recall the closest memories, push them into the beam
        // with source="ear:right". The two senses converge on the same
        // beam, so a song that thematically rhymes with what an eye-
        // glyph already pulled in will reinforce the same neighborhood.
        let mut ear_closed = false;
        if let Some(ref mut es) = ear_sub {
            match es.next_event() {
                SubEvent::Timeout => {}
                SubEvent::Closed => {
                    eprintln!("[attention serve] WARN: ear subscription closed — continuing without ear");
                    ear_closed = true;
                }
                SubEvent::Msg(emsg) => {
                    if let Ok(payload) = std::str::from_utf8(&emsg.payload) {
                        if let Ok(env) = serde_json::from_str::<serde_json::Value>(payload) {
                            let title = env.get("track").and_then(|t| t.get("title")).and_then(|v| v.as_str()).unwrap_or("");
                            let album = env.get("track").and_then(|t| t.get("album")).and_then(|v| v.as_str()).unwrap_or("");
                            let commercial = env.get("track").and_then(|t| t.get("commercial")).and_then(|v| v.as_bool()).unwrap_or(false);
                            if !title.is_empty() && !commercial {
                                let perc = env.get("perception").cloned().unwrap_or(serde_json::json!({}));
                                let tempo = perc.get("tempo_bpm").and_then(|v| v.as_f64()).map(|v| format!(" tempo={v:.0}")).unwrap_or_default();
                                let query = format!("ear track \"{title}\" album=\"{album}\"{tempo}");
                                let beam_cands = beam.candidates();
                                let results = if beam_cands.len() >= 8 {
                                    sys.recall_with_beam(&beam_cands, &query, top_k).unwrap_or_default()
                                } else {
                                    sys.recall(&query, top_k).unwrap_or_default()
                                };
                                for r in &results {
                                    let ev = ObservationEvent::now(r.id, "ear:right".to_string());
                                    beam.observe(&ev);
                                }
                            }
                        }
                    }
                }
            }
        }
        if ear_closed {
            ear_sub = None;
        }

        let msg = match sub.next_event() {
            SubEvent::Msg(m) => m,
            SubEvent::Timeout => continue,
            // Pre-fix a closed eye socket spun this loop at 100% CPU
            // forever. Exit nonzero so systemd Restart=on-failure works.
            SubEvent::Closed => {
                eprintln!("[attention serve] subscription {subject} closed — exiting for restart");
                process::exit(1);
            }
        };
        let payload = match std::str::from_utf8(&msg.payload) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Parse the eye envelope.
        let env: serde_json::Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(e) => { eprintln!("[attention serve] bad payload: {e}"); continue; }
        };
        let hemisphere = env.get("hemisphere").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        let glyph = match env.get("glyph") {
            Some(g) => g,
            None => continue,
        };
        let dom = glyph.get("dominant_class").and_then(|v| v.as_u64()).unwrap_or(0);
        let fano = glyph.get("fano_signature").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_u64()).take(8).map(|x| x.to_string()).collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        let fold = glyph.get("fold_sequence").and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_u64()).take(8).map(|x| x.to_string()).collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        let query = format!("glyph dom={dom} fano=[{fano}] fold=[{fold}]");

        // Two-stage warmup: cold beam falls back to full recall; warm beam
        // (≥ 8 candidates) runs recall against the beam only — O(K) end to end.
        const BEAM_WARM_THRESHOLD: usize = 8;
        let beam_cands = beam.candidates();
        let results = if beam_cands.len() >= BEAM_WARM_THRESHOLD {
            match sys.recall_with_beam(&beam_cands, &query, top_k) {
                Ok(r) => r,
                Err(_) => continue,
            }
        } else {
            match sys.recall(&query, top_k) {
                Ok(r) => r,
                Err(_) => continue,
            }
        };

        let source = format!("eye:{hemisphere}");
        for r in &results {
            let ev = ObservationEvent::now(r.id, source.clone());
            beam.observe(&ev);
        }

        // ── Glyph-gravity pull ────────────────────────────────────────────
        // The eye saw a glyph on a dominant Fano line. Pull the SAME-line
        // memories into the beam so this perception's whole neighborhood is
        // "in attention" for O(K) recall — folded information acting as gravity.
        // Gated by KANNAKA_GLYPH_GRAVITY. This pull only ADDS candidates to
        // the beam; the recall-side same-line ranking boost that the same var
        // used to enable was removed in #837 (measured to degrade recall).
        #[cfg(feature = "glyph")]
        {
            let gravity_on = std::env::var("KANNAKA_GLYPH_GRAVITY")
                .ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) > 0.0;
            if gravity_on {
                // Dominant Fano line = argmax of the eye glyph's 7-line energy
                // signature. Shared seam so the integration test reads the same
                // line off an eye envelope without NATS (glyph_bridge).
                let line = kannaka_memory::glyph_bridge::event_dominant_fano_line(glyph);
                if let Some(l) = line {
                    let pull_limit = top_k.saturating_mul(8).max(16);
                    if let Some(hrm) = sys
                        .engine
                        .store
                        .as_any_mut()
                        .downcast_mut::<kannaka_memory::hrm_store::HrmStore>()
                    {
                        let ids = hrm.ids_by_fano_line(l, pull_limit);
                        let n = ids.len();
                        for id in ids {
                            beam.observe(&ObservationEvent::now(id, "eye:gravity".to_string()));
                        }
                        if n > 0 {
                            eprintln!("[attention serve] glyph-gravity: line {l} pulled {n} same-line memories into beam");
                        }
                    } else if !warned_no_hrm {
                        // Degradation visibility: gravity is ON but the backing
                        // store can't serve a same-line well, so the beam pull
                        // silently no-ops. Say so once. (#14)
                        warned_no_hrm = true;
                        eprintln!("[attention serve] WARN: glyph-gravity ON but backing store is not HrmStore — same-line beam pull unavailable");
                    }
                }
            }
        }

        // Periodic stats line so an operator can watch the beam shape up.
        if last_stats_at.elapsed() >= stats_interval {
            let stats = beam.stats();
            eprintln!(
                "[attention serve] beam={} recency={} lookback={} landmarks={} obs={}",
                stats.beam_size, stats.recency_len, stats.lookback_len,
                stats.landmarks_len, stats.observations
            );
            last_stats_at = Instant::now();
        }

        // Dump beam state to a small JSON file the observatory can poll.
        let dump_path = std::env::var("KANNAKA_ATTENTION_BEAM_FILE")
            .unwrap_or_else(|_| {
                if cfg!(windows) {
                    "C:\\Users\\Public\\kannaka-attention-beam.json".to_string()
                } else {
                    "/tmp/kannaka-attention-beam.json".to_string()
                }
            });
        let stats = beam.stats();
        let cands = beam.candidates();
        let dump = serde_json::json!({
            "schema_version": 1,
            "ts": chrono::Utc::now().to_rfc3339(),
            "stats": {
                "recency_len": stats.recency_len,
                "lookback_len": stats.lookback_len,
                "landmarks_len": stats.landmarks_len,
                "beam_size": stats.beam_size,
                "observations": stats.observations,
            },
            "candidates": cands.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
        });
        let _ = std::fs::write(&dump_path, dump.to_string());
    }
    // The serve loop is intentionally infinite — Ctrl+C is the only exit.
}

#[cfg(not(feature = "nats"))]
pub(crate) fn handle_attention_serve(_: &mut kannaka_memory::openclaw::KannakaMemorySystem, _: &KannakaConfig, _: &[String]) {
    eprintln!("attention serve requires the 'nats' feature");
    process::exit(1);
}
