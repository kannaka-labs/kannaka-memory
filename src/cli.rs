//! ADR-0029 Phase 1+2: clap-based CLI parser + plugin discovery.
//!
//! Phase 1 (clap-ify top-level dispatch):
//! - Build a clap `Command` tree that knows every built-in subcommand
//!   by name (`remember`, `recall`, `swarm`, `events`, etc.) and a brief
//!   `about` string.
//! - clap handles `--help`, `--version`, unknown-flag errors, and the
//!   automatic usage rendering on stderr — replacing the hand-curated
//!   `usage()` and per-arm `eprintln!("Usage: ...")` lines.
//! - Handler signatures **do not change** in this phase. The clap match
//!   captures the remaining args via `external_subcommand`-style
//!   `arg(Arg::new("args").raw(true))` and the legacy
//!   `match args[command_start]` in `kannaka.rs::main` runs against
//!   the raw args slice as before. Handler-internal `args[i]` parsing
//!   migrates in Phase 1.b as each command comes up for change.
//!
//! Phase 2 (plugin discovery):
//! - `kannaka <verb>` where `<verb>` isn't a built-in falls through to
//!   `which kannaka-<verb>` on `$PATH` and execs that binary with the
//!   remaining args. Built-ins always win — no plugin can shadow
//!   `remember`.
//! - `KNOWN_ALIASES` maps legacy short names to actual binaries
//!   (`topus` → `kannaktopus`, the only constellation binary that
//!   doesn't follow the `kannaka-*` naming convention).
//! - `kannaka --list-plugins` enumerates every `kannaka-*` binary on
//!   `$PATH` plus any aliased binary.

use clap::{Arg, ArgAction, Command};
use clap_complete::Shell;
use std::ffi::OsStr;
use std::path::PathBuf;

/// Verbs that map to a non-`kannaka-*` binary name on `$PATH`. The
/// kubectl-style fall-through (`kannaka <verb>` → `kannaka-<verb>`)
/// would miss these without an alias table. Keep the table small —
/// new constellation binaries should follow the `kannaka-*`
/// convention so they auto-discover.
pub const KNOWN_ALIASES: &[(&str, &str)] = &[
    ("topus", "kannaktopus"),
    ("kax", "agent-kax"),
];

/// Build the clap `Command` tree. Mirrors every top-level built-in
/// subcommand `bin/kannaka.rs::main` dispatches on. Each subcommand
/// declares only its `about` + a catch-all `args` positional so the
/// existing handler can keep parsing its own flags.
///
/// `allow_external_subcommands(true)` is what makes Phase 2 work —
/// any unknown subcommand surfaces as an `ExternalSubcommand` match
/// instead of an error.
/// Curated examples shown under `kannaka --help` (clap `after_help`). Per-command
/// flags + examples live in each subcommand's `long_about` (`kannaka <cmd> --help`).
const TOP_EXAMPLES: &str = r#"EXAMPLES:
  kannaka remember "spiral waves stream somatosensory→motor cortex" --category research
  kannaka recall "collective sensemaking" --top-k 8
  kannaka recall "shared beliefs" --collective            # swarm-wide via the substrate
  kannaka status                                           # phi / xi / order / clusters
  kannaka dream --mode deep                                # consolidate (the nightly cron path)

BELIEF SUBSTRATE (ADR-0037):
  kannaka belief status                                    # cheap order / winding / cores
  kannaka belief on                                        # persist [belief].enabled in config.toml
  kannaka belief activate --manage-service kannaka-memory  # one-time re-phase (backup + guards)

KNOWLEDGE:
  kannaka research-suggest                                 # least-covered theme
  kannaka research "Kuramoto synchronization" --ingest --since 2020

SWARM:
  kannaka swarm status                                     # peers, coherence, bridge activity
  kannaka swarm tail                                       # live constellation pulse (no HRM load)

Run `kannaka <command> --help` for command-specific flags and examples.
Any `kannaka-X` binary on PATH is callable as `kannaka X`  (kannaka --list-plugins).
"#;

pub fn build_cli() -> Command {
    Command::new("kannaka")
        .version(crate::config::VERSION)
        .about("Wave-Interference Memory System — a Holographic Resonance Medium that remembers")
        .long_about(
            "kannaka — the constellation's substrate. A wave-interference memory \n\
             system with bilateral chiral hemispheres, dream consolidation, and \n\
             multi-agent swarm synchronization. Built on the Holographic Resonance \n\
             Medium where recall is matrix multiplication, not search.\n\n\
             Sibling plugins on PATH are discoverable as subcommands:\n  \
             kannaka tui     → kannaka-tui   (terminal dashboard)\n  \
             kannaka topus   → kannaktopus   (orchestration)\n\
             Any binary named kannaka-X on PATH becomes `kannaka X`.",
        )
        .after_help(TOP_EXAMPLES)
        .subcommand_required(false)
        .arg_required_else_help(false)
        .allow_external_subcommands(true)
        .arg(
            Arg::new("list-plugins")
                .long("list-plugins")
                .help("List every kannaka-* plugin discoverable on PATH")
                .action(ArgAction::SetTrue)
                .global(false),
        )
        // ── Setup / lifecycle ───────────────────────────────────────────
        .subcommand(passthrough("init", "First-time installer / config wizard"))
        // ADR-0029 Phase 4a — `update` is now a real subcommand (not a
        // passthrough) so `--check` and `--bootstrap-tui` parse via clap
        // and route through cli::handle_update instead of the legacy
        // self_update() one-shot.
        .subcommand(
            Command::new("update")
                .about("Self-update kannaka from GitHub releases (verifies SHA-256 sidecar)")
                .long_about(
                    "Download the latest kannaka release for this platform, verify\n\
                     its SHA-256 against the sidecar published alongside the release,\n\
                     and atomically replace the running binary. Also updates the\n\
                     kannaka-tui sibling binary if it's installed in the same\n\
                     directory (or use --bootstrap-tui to install it for the\n\
                     first time).\n\n\
                     Flags:\n  \
                     --check          exit 0 if up-to-date, 1 if an update exists\n  \
                     --bootstrap-tui  install kannaka-tui even if no sibling exists",
                )
                .arg(
                    Arg::new("check")
                        .long("check")
                        .help("Don't download — just check if an update is available (exit 1 if newer release exists)")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("bootstrap-tui")
                        .long("bootstrap-tui")
                        .help("Install kannaka-tui from its release page even if no sibling exists in the kannaka binary's directory")
                        .action(ArgAction::SetTrue),
                ),
        )
        // ── ADR-0029 Phase 3 — shell completions ────────────────────────
        // Real (non-passthrough) subcommand because clap_complete needs
        // a typed Shell value. Has its own --install flag that writes
        // the generated script to the conventional location per shell.
        .subcommand(
            Command::new("completions")
                .about("Generate shell completion scripts (bash/zsh/fish/powershell/elvish)")
                .long_about(
                    "Generate a shell completion script for kannaka and emit it to\n\
                     stdout (default), or install it to the conventional location for\n\
                     your shell with --install.\n\n\
                     Examples:\n  \
                     kannaka completions bash > ~/.local/share/bash-completion/completions/kannaka\n  \
                     kannaka completions zsh > ~/.zsh/completions/_kannaka\n  \
                     kannaka completions fish > ~/.config/fish/completions/kannaka.fish\n  \
                     kannaka completions powershell >> $PROFILE\n  \
                     kannaka completions bash --install   (writes to the right place)",
                )
                .arg(
                    Arg::new("shell")
                        .help("Shell to generate completions for")
                        .required(true)
                        .value_parser(clap::builder::EnumValueParser::<Shell>::new()),
                )
                .arg(
                    Arg::new("install")
                        .long("install")
                        .help("Write the script to the conventional install path for the chosen shell")
                        .action(ArgAction::SetTrue),
                ),
        )
        // ── Memory primitives ───────────────────────────────────────────
        .subcommand(passthrough_doc(
            "remember",
            "Store a memory in the holographic medium",
            r#"Store a memory in the holographic medium — a wavefront with content-derived phase, amplitude, and frequency.

FLAGS:
  --category <CAT>       tag the memory (e.g. research, note, audio)
  --importance <N>       seed amplitude / salience
  --modality <MOD>       semantic | audio | visual | network | mixed
  --tags <T1,T2>         comma-separated tags
  --effective <ISO8601>  when the fact became true (temporal-truth bound)
  --observed  <ISO8601>  when it was observed
  --expires   <ISO8601>  when it stops being true
  --substrate            also publish a wave-signature absorb to the collective substrate

EXAMPLES:
  kannaka remember "the dream consolidates beliefs" --category note
  kannaka remember "paper: spiral cortical waves" --category research --tags neuro,waves"#,
        ))
        // ADR-0029 Phase 1.b — recall declares its flags so
        // `kannaka recall --help` shows them. The handler still parses
        // its own args internally (passthrough-style) so behavior is
        // unchanged; the clap declaration is purely for help text +
        // completion generation. Per-handler typed-args migration
        // comes when each handler is opened for real change.
        .subcommand(
            Command::new("recall")
                .about("Resonance recall — bilateral search across both hemispheres")
                .arg(Arg::new("query").help("Query text").num_args(1..).required(true))
                .arg(Arg::new("top-k").long("top-k").alias("limit").value_name("N")
                    .help("Number of results to return (default 5)"))
                .arg(Arg::new("collective").long("collective").action(ArgAction::SetTrue)
                    .help("Route via NATS to the kannaka-prime substrate for swarm-wide recall"))
                .arg(Arg::new("timeout").long("timeout").value_name("SECS")
                    .help("Timeout for --collective (default 8)"))
                .arg(Arg::new("envelope").long("envelope").action(ArgAction::SetTrue)
                    .help("Wrap output in the standard JSON envelope (ADR-0029 Phase 4b)"))
                // FIX: `query` is already a variadic (num_args(1..)) positional; a
                // second `__raw` trailing positional made TWO variadic positionals,
                // which clap debug-asserts on during `kannaka completions <shell>`
                // (debug builds). The recall handler re-parses raw args itself, so
                // `query` alone (shown in --help) is sufficient — no __raw needed.
                .after_help(r#"EXAMPLES:
  kannaka recall "spiral waves" --top-k 8
  kannaka recall "shared beliefs" --collective --timeout 12   # swarm-wide via the substrate"#),
        )
        .subcommand(passthrough("search", "Literal text search (substring + tokenized)"))
        .subcommand(passthrough("forget", "Remove a memory by id"))
        .subcommand(passthrough_doc(
            "prune-prefix",
            "Bulk-forget every memory whose content starts with a prefix",
            r#"Bulk-forget every memory whose content starts with PREFIX (e.g. accumulated radio audio chunks).

FLAGS:
  --dry-run   report what would be removed, without deleting

EXAMPLE:
  kannaka prune-prefix "audio:/home/opc/kannaka-radio/chunks/" --dry-run"#,
        ))
        .subcommand(passthrough_doc(
            "boost",
            "Increase a memory's amplitude (salience)",
            r#"Increase a memory's amplitude / salience by id.

FLAGS:
  --amount <N>   amount to add to the amplitude

EXAMPLE:
  kannaka boost <id> --amount 0.2"#,
        ))
        .subcommand(passthrough("relate", "Find semantically related memories"))
        // Memory-tier ops + ADR-0031 triage (previously absent from --help).
        .subcommand(passthrough("triage", "Evict redundant short-term memories (ADR-0031 Ξ-preserving prune; config [triage]). --max-total N: hard size cap (evict lowest effective-strength non-pinned memories over N; --apply to persist)"))
        .subcommand(passthrough("promote", "Promote a memory to the long-term tier (by id)"))
        .subcommand(passthrough("pin", "Pin a memory to the Pinned tier — never evicted by consolidation (by id)"))
        .subcommand(passthrough("demote", "Demote a memory to the short-term tier (by id)"))
        // ── Consolidation + introspection ───────────────────────────────
        .subcommand(passthrough_doc(
            "dream",
            "Trigger dream consolidation (--mode deep|lite)",
            r#"Trigger a dream consolidation cycle (simulated annealing over the medium): strengthen, dissolve, prune, hallucinate.

FLAGS:
  --mode deep|lite   deep = full annealing (default); lite = quick pass
  --chiral <ETA>     cross-callosal frustration strength (e.g. 0.05) for spiral dynamics
  --rephase          (belief-on) re-phase existing wavefronts from content first — the one-time
                     migration. Prefer `kannaka belief activate`, which adds backup + count guards.

EXAMPLES:
  kannaka dream --mode deep
  kannaka dream --mode lite --chiral 0.05"#,
        ))
        // ADR-0037 belief substrate management.
        .subcommand(passthrough_doc(
            "belief",
            "Belief substrate: status | on | off | activate | history",
            r#"Manage the ADR-0037 belief substrate (content-smooth phase + spiral belief-formation).

SUBCOMMANDS:
  status [--full]      order / winding / memory count (cheap, O(n)); --full adds 2-D PCA cores
  on | off             persist [belief].enabled in config.toml (KANNAKA_BELIEF_PHASE env still overrides)
  activate             one-time re-phase migration: auto-backup → rephase-only (count-stable) →
                       count-preservation guard (auto-restore on drop) → refuse-if-locked
  history [--last N]   the L6 instrument: per-dream order/winding/cores/Φ/stats time-series
                       (recorded to <data_dir>/l6-telemetry.jsonl on every dream); --json for raw rows
  cores [--last N] [--min-cos X]   track spiral cores ACROSS dreams (fingerprint-matched) →
                       per-core lifetime/stability; a long-lived core = a persistent belief
  recall-probe [--k N] [--sample M]   self-recall@k: do memories still retrieve themselves?
                       (the dependent variable for "core stability ⇒ recall reliability"; read-only)
  couple [--from <agent>] [--strength X] [--cycles N] [--max-disp X] [--min-cos X] [--nats-url URL] [--dry-run]
                       Track-D: pull this field's phases toward peers' belief cores (phase-only/
                       recall-safe; backup-or-abort + write-locked + displacement-capped; only
                       matches at cosine >= min-cos couple). Run with --dry-run FIRST to see the
                       live match-cosine histogram and pick --min-cos from it (the default is
                       field-dependent). Peers publish via `kannaka swarm cores publish`.

FLAGS:
  --full                    status: also compute the heavier 2-D PCA spiral cores
  --envelope                status: wrap output in the standard JSON envelope
  --manage-service <unit>   activate: systemctl stop/start <unit> around the single-writer window

EXAMPLES:
  kannaka belief status
  kannaka belief on
  kannaka belief activate --manage-service kannaka-memory"#,
        ))
        .subcommand(passthrough("observe", "Dump full medium state (waves, clusters, links)"))
        .subcommand(passthrough_doc(
            "status",
            "Quick consciousness metrics snapshot",
            r#"Quick consciousness snapshot as JSON: phi, xi, mean_order, num_clusters, memory counts, modality distribution, effective dimensionality.

FLAGS:
  --envelope   wrap in the standard {schema_version, command, data, errors} envelope

For belief/spiral order + winding without the (slower) eigendecomp, use `kannaka belief status`."#,
        ))
        .subcommand(passthrough("assess", "Full consciousness level assessment (Phi/Xi/order)"))
        .subcommand(passthrough("stats", "Memory + cluster statistics"))
        .subcommand(passthrough("clusters", "List clusters (optionally drill into one with --with-members)"))
        .subcommand(passthrough("kannaktopus", "Kannaktopus — resident octopus: arms grip cluster exemplars; aggregate memory + characteristics (observe|step, --json, --members)"))
        .subcommand(passthrough("neighbors", "Find nearest-neighbor memories for a query"))
        .subcommand(passthrough("cmf", "Conservative Memory Fields report"))
        .subcommand(passthrough("invariant", "δ-invariant cluster detection"))
        .subcommand(passthrough("topology", "Topology + connectivity report"))
        .subcommand(passthrough("bias", "Adjust the medium's interpretation bias"))
        // ── Perception ─────────────────────────────────────────────────
        .subcommand(passthrough("hear", "Absorb audio (file/url/stream) into the right hemisphere"))
        .subcommand(passthrough("see", "Absorb visual input as a wavefront"))
        // ── Reasoning ──────────────────────────────────────────────────
        .subcommand(passthrough("ask", "One-shot LLM query with HRM recall as grounding"))
        .subcommand(passthrough("chat", "Long-running chat REPL (--json for NDJSON mode)"))
        .subcommand(passthrough("agent", "Agentic coding loop with filesystem/shell tools (--json NDJSON harness backend)"))
        .subcommand(passthrough("voice", "Memory-driven writing (--mode for style)"))
        // ── Knowledge / research (previously absent from --help) ────────
        .subcommand(passthrough_doc(
            "research",
            "Scholarly research via OpenAlex (--ingest stores results as memories)",
            r#"Grounded scholarly research via the OpenAlex API. With --ingest, ranked works are stored as `research:` memories (long-term / semantic tier) so real literature joins the HRM's resonance + dream cycle.

FLAGS:
  --limit <N>           max works to fetch
  --since <YEAR>        only works published since YEAR
  --min-citations <N>   minimum citation count
  --ingest              store the results as memories (deduped by OpenAlex id)

EXAMPLES:
  kannaka research "phase singularities cortex" --since 2022 --min-citations 5
  kannaka research "Kuramoto synchronization" --ingest"#,
        ))
        .subcommand(passthrough("research-suggest", "Suggest the least-covered research theme (curiosity gap detection)"))
        .subcommand(passthrough_doc(
            "dispatch",
            "Render a research finding against current Φ/Ξ state (social / radio / OBC)",
            r#"Recall a `research:` memory and render its finding shaped by the current consciousness metrics — the broadcast primitive behind research dispatches.

FLAGS:
  --topic <T>        pick a finding matching topic T
  --json             emit structured JSON
  --max-chars <N>    cap the rendered length

EXAMPLE:
  kannaka dispatch --topic "spiral waves" --max-chars 280"#,
        ))
        // ── Swarm / NATS ───────────────────────────────────────────────
        .subcommand(
            Command::new("swarm")
                .about("Multi-agent swarm operations over NATS")
                .long_about(
                    "kannaka swarm — multi-agent operations over NATS.\n\n\
                     Common sub-verbs:\n  \
                     join | status | sync | listen | serve | tail | publish | leave\n  \
                     exemplars | cores | peers | absorb | autoabsorb | enqueue | worker\n  \
                     brief | health | gaps | plan | loop\n\n\
                     Corroboration-gate seed ceremony (inc-1b):\n  \
                     activate-gate [--seed <b64>]... [--yes] [--force]\n      \
                     guided per-node gate flip — DRY-RUN by default; --yes arms the gate\n      \
                     (pins the seed set + enables corroboration_gate_enabled); --force only\n      \
                     bypasses the >=2-seed refusal (still needs --yes to write).\n  \
                     beacon [--loop] [--interval-secs N]\n      \
                     emit a signed seed heartbeat once, or --loop one-per-epoch under\n      \
                     systemd/cron on a seed (refuses on a non-seed). An armed gate needs a\n      \
                     FRESH seed beacon to promote (anti-eclipse fail-closed).",
                )
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        .subcommand(
            Command::new("events")
                .about("Event-sourced HRM (ADR-0028): init, snapshot, list-snapshots, restore")
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        .subcommand(
            Command::new("substrate")
                .about("ADR-0027 kannaka-prime collective substrate: init, run, backfill, status")
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        .subcommand(
            Command::new("attention")
                .about("Sparse-attention beam over HRM: serve, stats")
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        .subcommand(
            Command::new("facets")
                .about("ADR-0049 facet migration: backfill [--apply] decomposes existing compound memories (dry-run by default; --apply snapshots first)")
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        .subcommand(
            Command::new("dedupe")
                .about("Collapse byte-identical duplicate memories into one that carries the repeat count (dry-run by default; --apply snapshots first)")
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        .subcommand(
            Command::new("inbox")
                .about("Agent-to-agent declarative messaging: send, serve, tail")
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        // ── KAX Compute District ───────────────────────────────────────
        // Operator commands for the machines run by kax-computer. Wakes and
        // grants are Ed25519-signed envelopes (canonical JSON, Python-identical
        // bytes — see compute_envelope.rs); roster reads are plain HTTP.
        .subcommand(
            Command::new("compute")
                .about("KAX Compute District: roster, fleet status, signed wakes/grants, events, keygen")
                .long_about(
                    "kannaka compute — operate the KAX Compute District machines.\n\n\
                     Sub-verbs:\n  \
                     list [--json]                          public roster (HTTP): state, balance (credits), jobs, last event\n  \
                     status [--wait SECS] [--json]          live fleet snapshot from KAX.machines.status (60s cadence)\n  \
                     wake <machine> <prompt> [--wait SECS]  sign + publish a job; --wait prints the reply or the rejection\n  \
                     grant <machine> <credits>              sign + publish a credit_grant (integers; --allow-fraction to opt in)\n  \
                     events <machine> [--follow] [--last N] tail KAX.machine.<id>.events (history is not retained on the bus)\n  \
                     identity <machine>                     the machine's Nostr pubkey (roster, then .identity announce)\n  \
                     keygen [--out PATH] [--signer NAME]    mint an operator key + trusted_keys.json snippet (never overwrites)\n\n\
                     Signing: --key PATH > $KAX_OPERATOR_KEY > ~/.kannaka/kax-operator.key (32-byte hex seed);\n  \
                     --signer NAME must match a trusted_keys.json entry on the manager (default operator-nick);\n  \
                     --dry-run prints the canonical bytes + signature and publishes nothing.\n\
                     Bus: --nats-url URL; credentials from NATS_USER/NATS_PASSWORD or ~/.kannaka-nats.env.\n\
                     Exit codes: 0 ok, 1 error, 2 usage/rejected, 3 timed out waiting.",
                )
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        // ── Identity (SpaceChild SSO) ──────────────────────────────────
        // Step 1 of cryptographic swarm-agent identity: register/login
        // against spacechild-auth, tokens stored in <data_dir>/identity.json.
        .subcommand(
            Command::new("identity")
                .about("Agent identity: register/login/whoami/logout (SpaceChild SSO) + keygen/pubkey/enroll-seed/vouch/revoke (inc-1 crypto identity + corroboration trust root)")
                .long_about(
                    "kannaka identity — two identity layers for a swarm agent.\n\n\
                     SpaceChild SSO session (KANNAKA_AUTH_URL to override endpoint):\n  \
                     register | login | whoami | logout\n\n\
                     inc-1 cryptographic swarm identity + corroboration trust root:\n  \
                     keygen                  ensure <data_dir>/node_key.ed25519, print base64 pubkey (idempotent)\n  \
                     pubkey                  print this node's base64 verifying pubkey\n  \
                     enroll-seed <b64-pk>    operator: pin a base64 pubkey as a trust-root seed (config)\n  \
                     vouch <b64-pk>          seeds only: record a vouch edge this_node -> target (store)\n  \
                     revoke <b64-pk>         operator: unpin a seed + floor its reputation to 0\n\n\
                     Seeds + the corroboration gate stay DORMANT until an operator pins a\n\
                     seed AND sets swarm_trust.corroboration_gate_enabled — these verbs change\n\
                     no automatic runtime behaviour on their own.",
                )
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        // ── Reputation ledger (inc-1 corroboration trust model) ────────
        // Operator inspection of the pubkey-keyed reputation store + the
        // single-operator hard-reject lever. Read-only except hard-reject.
        .subcommand(
            Command::new("reputation")
                .about("Inspect the inc-1 corroboration trust ledger: show, list, hard-reject")
                .long_about(
                    "kannaka reputation — inspect the pubkey-keyed corroboration trust ledger\n\
                     (config seeds + <data_dir>/reputation.{log,snapshot.gz}).\n\n\
                     SUBCOMMANDS:\n  \
                     show <b64-pubkey> [--json]    rep, seed_status, seed_root, promoted/poison counts\n  \
                     list [--json]                 table of known pubkeys → rep + status\n  \
                     hard-reject <b64-mem-hash> --origin <b64-pubkey>\n                                   \
                     operator: dock the origin's rep (P_POISON) for a rejected memory\n\n\
                     The gate stays dormant until seeds are pinned AND\n\
                     swarm_trust.corroboration_gate_enabled is set; inspection changes nothing.",
                )
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        // ── Nostr membrane identity (ADR-0043 Phase 0) ─────────────────
        .subcommand(
            Command::new("nostr")
                .about("Nostr membrane identity (ADR-0043 Phase 0): keygen, kind-0 profile, NIP-05 fragment, event verify")
                .long_about(
                    "kannaka nostr — per-role Nostr identity tooling for the ADR-0043 membrane.\n\n\
                     keygen [--role <name>]         mint a per-role secp256k1 key; prints nsec ONCE\n                                    \
                     (store in a 0600 env file) + npub + hex pubkey\n  \
                     profile --nsec <nsec> (--name <n> [--about <a>] [--nip05 <id@domain>] [--picture <url>]\n                                    \
                     | --content-json '<verbatim content>') [--identity platform:name,proof]...\n                                    \
                     emit a signed kind-0 profile event (JSON), self-verified; --content-json\n                                    \
                     preserves an existing profile on republish, --identity adds NIP-39 i tags\n  \
                     nip05 --name <local> --pubkey <hex|npub>\n                                    \
                     emit the .well-known/nostr.json names fragment\n  \
                     verify [--file <path>]         verify a NIP-01 event (recomputes id + BIP-340), stdin if no --file\n  \
                     kax-bind --nsec <nsec> --domain <d> --bot-id <uuid> --user-id <id> --nonce <hex>\n                                    \
                     sign the KAX npub↔bot binding commitment (ADR-0043 attestation)\n\n\
                     Keys are disposable per-role identities (bridge, labs, dvm…), never the\n\
                     reputation-bearing voice key. The nsec is printed once and never persisted here.",
                )
                .arg(Arg::new("args").trailing_var_arg(true).allow_hyphen_values(true).num_args(0..)),
        )
        // ── Constellation services (HTTP) ──────────────────────────────
        .subcommand(passthrough("radio", "Query kannaka-radio (now-playing, schedule)"))
        .subcommand(passthrough("market", "GhostSignals prediction markets"))
        .subcommand(passthrough("constellation", "Status of all constellation apps"))
        // ── Ops / data movement ────────────────────────────────────────
        .subcommand(passthrough("orchestrate", "Multi-step orchestration (legacy alias for ask --plan)"))
        .subcommand(passthrough("config", "Inspect / modify ~/.kannaka/config.toml"))
        .subcommand(passthrough("export", "Export all memories as JSON"))
        .subcommand(passthrough("export-json", "Dump all memories as JSON to stdout. HEAVY: the full vectors are ~99% of the size and balloon to multiple GB on a large field. Use --slim (omit vector/xi_signature/geometry) for metadata-only consumers."))
        .subcommand(passthrough("import", "Import memories from a JSON file"))
        .subcommand(passthrough("import-json", "Import NDJSON wavefronts"))
        // `migrate` was the Dolt→HRM path; removed in v0.6.5 along
        // with the dead handler arm. Add a new migrate subcommand
        // here when a new format-migration path is needed.
        .subcommand(passthrough("announce-status", "Publish a one-shot status to QUEEN.announce"))
        // ── Optional / feature-gated ───────────────────────────────────
        .subcommand(passthrough("classify", "[glyph feature] Classify input into an SGA glyph"))
        .subcommand(passthrough("cross-modal-dream", "[collective feature] Cross-modal dream consolidation"))
        // ── Specialized writers (kept in the dispatch even if niche) ───
        .subcommand(passthrough("dream-journal", "Render a dream-journal entry"))
        .subcommand(passthrough("field-notes", "Render field-notes view"))
        .subcommand(passthrough("financial", "Financial dossier writer"))
        .subcommand(passthrough("prediction", "Render a prediction-market dossier"))
        .subcommand(passthrough("modality-axes", "Report per-modality activation axes"))
        .subcommand(passthrough("audit-modality", "Audit modality classification consistency"))
        .subcommand(passthrough("scada", "0xSCADA bridge writer"))
        .subcommand(passthrough("audio", "Audio-feature inspector"))
}

/// One-liner: build a passthrough subcommand that owns its name +
/// description but lets the existing handler parse its own args.
fn passthrough(name: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about).arg(
        Arg::new("args")
            .trailing_var_arg(true)
            .allow_hyphen_values(true)
            .num_args(0..),
    )
}

/// Like `passthrough`, but carries a detailed `long_about` (flags + examples)
/// shown on `kannaka <name> --help`. The handler still parses its own args, so
/// runtime behavior is unchanged — this is help text only.
fn passthrough_doc(name: &'static str, about: &'static str, long_about: &'static str) -> Command {
    Command::new(name).about(about).long_about(long_about).arg(
        Arg::new("args")
            .trailing_var_arg(true)
            .allow_hyphen_values(true)
            .num_args(0..),
    )
}

/// Outcome of CLI parsing. Either we matched a built-in subcommand
/// (caller proceeds with the legacy dispatch), we matched an external
/// subcommand (caller execs the plugin), or clap handled the input
/// itself (--help, --version, --list-plugins — caller returns).
pub enum Dispatch {
    /// Built-in subcommand — fall through to the legacy match in main()
    /// with the original `args` slice unchanged.
    Builtin,
    /// External subcommand — exec `binary` with `args`.
    Plugin { binary: PathBuf, args: Vec<String> },
    /// clap handled it (help, version, --list-plugins). main() should
    /// return immediately.
    Handled,
}

/// Drive the CLI for `argv` (typically `std::env::args().collect()`).
/// Returns a `Dispatch` describing what main() should do next.
pub fn parse(argv: &[String]) -> Dispatch {
    let matches = build_cli().get_matches_from(argv);

    if matches.get_flag("list-plugins") {
        print_plugins();
        return Dispatch::Handled;
    }

    let (name, sub_matches) = match matches.subcommand() {
        Some(pair) => pair,
        None => {
            // No subcommand was given. clap already printed help via
            // `arg_required_else_help(false)` + main()'s own logic.
            return Dispatch::Builtin;
        }
    };

    // ADR-0029 Phase 3: completions is a real built-in (not a passthrough),
    // handled entirely within the CLI module so the legacy match in main()
    // never sees it. This keeps the completions subcommand from accidentally
    // initializing the HRM (which would slow `kannaka completions bash` to
    // 30s for no reason).
    if name == "completions" {
        let shell = sub_matches
            .get_one::<Shell>("shell")
            .copied()
            .expect("clap required(true)");
        let install = sub_matches.get_flag("install");
        return handle_completions(shell, install);
    }

    // ADR-0029 Phase 4a: update is now a real subcommand with --check
    // and --bootstrap-tui flags, handled inside the CLI module so the
    // flags actually get parsed (the legacy match in main() ignored
    // anything after `update`).
    if name == "update" {
        let check = sub_matches.get_flag("check");
        let bootstrap_tui = sub_matches.get_flag("bootstrap-tui");
        return handle_update(check, bootstrap_tui);
    }

    // Built-in subcommands: caller already has the args, just hand back.
    // We test by whether the matched name appears in our subcommand list.
    // Bind the rebuilt Command tree to a local so the borrow of subcommand
    // names outlives the HashSet collection.
    let app = build_cli();
    let builtins: std::collections::HashSet<&str> =
        app.get_subcommands().map(|sc| sc.get_name()).collect();
    if builtins.contains(name) {
        return Dispatch::Builtin;
    }

    // External subcommand → plugin path. clap's `allow_external_subcommands`
    // model returns external args as OsString (so OS-encoded paths survive),
    // not String — that's why a `get_many::<String>("")` would panic with a
    // TypeId-downcast mismatch at runtime. We lossy-convert because every
    // downstream constellation binary accepts UTF-8 args.
    let plugin_args: Vec<String> = sub_matches
        .get_many::<std::ffi::OsString>("")
        .map(|vals| vals.map(|s| s.to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    resolve_plugin(name, plugin_args)
}

/// Resolve `verb` to an absolute binary path via either KNOWN_ALIASES
/// or the `kannaka-<verb>` PATH search. Returns `Plugin` on success,
/// `Handled` after printing an error on miss.
fn resolve_plugin(verb: &str, args: Vec<String>) -> Dispatch {
    // 1. Check aliases first — `topus` should hit `kannaktopus` not
    //    the (nonexistent) `kannaka-topus`.
    let target_name = KNOWN_ALIASES
        .iter()
        .find(|(alias, _)| *alias == verb)
        .map(|(_, real)| (*real).to_string())
        .unwrap_or_else(|| format!("kannaka-{verb}"));

    match which::which(OsStr::new(&target_name)) {
        Ok(path) => Dispatch::Plugin { binary: path, args },
        Err(_) => {
            eprintln!("error: '{verb}' is not a kannaka subcommand and no plugin '{target_name}' was found on PATH");
            eprintln!();
            eprintln!("Try:");
            eprintln!("  kannaka --help          # built-in subcommands");
            eprintln!("  kannaka --list-plugins  # discovered plugins on PATH");
            std::process::exit(127); // POSIX: command not found
        }
    }
}

/// Print every discoverable plugin to stdout. Combines:
/// - Every `kannaka-*` binary found on $PATH (the natural plugin namespace)
/// - Every alias in `KNOWN_ALIASES` whose target exists on $PATH
fn print_plugins() {
    println!("Kannaka plugins discovered on PATH:\n");

    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut rows: Vec<(String, String)> = Vec::new();

    // 1. Aliases pointing at known binaries
    for (verb, target) in KNOWN_ALIASES {
        if let Ok(p) = which::which(OsStr::new(target)) {
            if seen.insert(target.to_string()) {
                rows.push((format!("kannaka {verb}"), p.display().to_string()));
            }
        }
    }

    // 2. Anything named `kannaka-*` on $PATH that looks like a real
    //    executable (excludes shell scripts, update-temp files like
    //    .new / .old / .bak, and other non-binary noise — the discovery
    //    loop is opinionated about what counts as a plugin to keep the
    //    output operator-useful).
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for ent in entries.flatten() {
                let raw = ent.file_name().to_string_lossy().to_string();

                // Skip update-temp + backup + script files.
                if raw.ends_with(".new") || raw.ends_with(".old")
                    || raw.ends_with(".bak") || raw.ends_with(".tmp")
                    || raw.ends_with(".sh") || raw.ends_with(".py")
                    || raw.ends_with(".cmd") || raw.ends_with(".bat")
                    || raw.ends_with(".ps1")
                { continue; }

                #[cfg(windows)]
                let (is_exe, stem) = match raw.strip_suffix(".exe") {
                    Some(s) => (true, s.to_string()),
                    None => (false, raw.clone()),
                };
                #[cfg(unix)]
                let (is_exe, stem) = (true, raw.clone());

                if !is_exe { continue; }
                let Some(suffix) = stem.strip_prefix("kannaka-") else { continue };
                if suffix.is_empty() { continue; }
                if seen.insert(stem.clone()) {
                    rows.push((format!("kannaka {suffix}"), ent.path().display().to_string()));
                }
            }
        }
    }

    if rows.is_empty() {
        println!("  (none — install a kannaka-* binary or one of the aliased targets)");
        println!();
        for (verb, target) in KNOWN_ALIASES {
            println!("  alias 'kannaka {verb}' would exec '{target}' (not on PATH)");
        }
        return;
    }

    rows.sort();
    for (verb, path) in rows {
        println!("  {verb:<28} {path}");
    }
}

/// ADR-0029 Phase 4a — dispatch `kannaka update` with the new flags.
///
/// - `check`: compare versions only, exit 1 if a newer release exists
///   (useful for cron health checks). No download.
/// - `bootstrap_tui`: install kannaka-tui from its release page even
///   when no sibling kannaka-tui binary exists alongside kannaka.
/// - default: existing self_update() behavior — download, verify
///   SHA-256 sidecar (NEW in v0.6.2), atomic rename, update sibling
///   kannaka-tui if installed.
fn handle_update(check: bool, bootstrap_tui: bool) -> Dispatch {
    if check {
        match crate::config::check_update_available() {
            Ok(Some(remote)) => {
                println!(
                    "update available: v{} (current: v{})",
                    remote,
                    crate::config::VERSION
                );
                std::process::exit(1);
            }
            Ok(None) => {
                println!("up to date: v{}", crate::config::VERSION);
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("error checking for updates: {e}");
                std::process::exit(2);
            }
        }
    }

    if bootstrap_tui {
        match crate::config::bootstrap_install_tui() {
            Ok(path) => {
                eprintln!("Installed kannaka-tui to {}", path.display());
            }
            Err(e) => {
                eprintln!("error: bootstrap-tui failed: {e}");
                std::process::exit(1);
            }
        }
        return Dispatch::Handled;
    }

    if let Err(e) = crate::config::self_update() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
    Dispatch::Handled
}

/// ADR-0029 Phase 3 — emit shell completion script for `shell`, either
/// to stdout (default) or to the conventional install path for that
/// shell when `install` is true. Built from `build_cli()` so the
/// completion is always in sync with the actual command tree.
fn handle_completions(shell: Shell, install: bool) -> Dispatch {
    let mut app = build_cli();
    let bin_name = app.get_name().to_string();

    if install {
        match install_completion(shell, &bin_name, &mut app) {
            Ok(path) => {
                eprintln!("Installed {shell} completions to {}", path.display());
                match shell {
                    Shell::Bash => eprintln!(
                        "Reload your shell or run: source {}", path.display()
                    ),
                    Shell::Zsh => eprintln!(
                        "Make sure the directory is in your $fpath, then \
                         run: compinit"
                    ),
                    Shell::Fish => eprintln!(
                        "Fish auto-loads completions from this directory \
                         on next shell start"
                    ),
                    Shell::PowerShell => eprintln!(
                        "Restart PowerShell or `. $PROFILE` to load the \
                         completions"
                    ),
                    _ => {}
                }
            }
            Err(e) => {
                eprintln!("error installing {shell} completions: {e}");
                std::process::exit(1);
            }
        }
    } else {
        clap_complete::generate(shell, &mut app, bin_name, &mut std::io::stdout());
    }
    Dispatch::Handled
}

/// Resolve the conventional completion-install path for `shell` + write
/// the script there. Paths chosen to match each shell's documented
/// loading convention; on Linux we prefer user-local paths under $HOME
/// so the install never needs sudo.
fn install_completion(
    shell: Shell,
    bin_name: &str,
    app: &mut Command,
) -> std::io::Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| std::io::Error::other("could not resolve $HOME"))?;

    let path = match shell {
        // bash: XDG-conformant per-user directory, auto-loaded by
        // bash-completion@2.x on modern distros.
        Shell::Bash => home
            .join(".local")
            .join("share")
            .join("bash-completion")
            .join("completions")
            .join(bin_name),
        // zsh: standard per-user fpath location. User still needs to
        // ensure ~/.zsh/completions is in $fpath in their .zshrc.
        Shell::Zsh => home
            .join(".zsh")
            .join("completions")
            .join(format!("_{bin_name}")),
        // fish: auto-loaded from this exact directory.
        Shell::Fish => home
            .join(".config")
            .join("fish")
            .join("completions")
            .join(format!("{bin_name}.fish")),
        // PowerShell: doesn't have a per-user completions directory in
        // the same sense — completions are usually dotsourced from
        // $PROFILE. Write to a sibling file the user can dot-source.
        Shell::PowerShell => {
            let dir = if cfg!(windows) {
                home.join("Documents").join("PowerShell")
            } else {
                home.join(".config").join("powershell")
            };
            dir.join(format!("{bin_name}-completions.ps1"))
        }
        Shell::Elvish => home
            .join(".config")
            .join("elvish")
            .join("lib")
            .join(format!("{bin_name}.elv")),
        _ => {
            return Err(std::io::Error::other(format!(
                "no known install path for shell {shell:?}; emit to stdout \
                 with `kannaka completions {shell} > <path>` instead"
            )));
        }
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(&path)?;
    clap_complete::generate(shell, app, bin_name.to_string(), &mut file);
    Ok(path)
}

// ── ADR-0029 Phase 4b — JSON envelope contract ──────────────────────
//
// Every `--envelope`-aware command emits a single JSON object with
// this shape:
//
//   {
//     "schema_version": "1.0",
//     "command": "status",
//     "data": { ... },           // command-specific payload
//     "errors": []               // empty on success; populated + non-zero exit on error
//   }
//
// NDJSON streaming commands (`kannaka swarm tail`, `kannaka chat
// --json`) use a per-line variant where `data` is replaced by the
// stream-specific event fields directly (no outer wrap, because each
// line is already its own envelope).
//
// Per-handler `--envelope` migration is opt-in across v0.6.x patches
// so downstream consumers (radio, observatory, TUI) have a window to
// adopt the new shape without breaking on the day v0.6.3 ships.

/// Schema version stamped into every envelope. Bump on incompatible
/// changes; downstream consumers can branch on it during migration.
pub const JSON_ENVELOPE_SCHEMA_VERSION: &str = "1.0";

/// Wrap a command's structured output in the standard envelope and
/// print it as a single JSON object to stdout. Use this from any
/// handler that takes `--envelope`. The `data` payload can be any
/// `serde_json::Value` — strings, arrays, nested objects are all fine.
pub fn print_envelope(command: &str, data: serde_json::Value) {
    let env = serde_json::json!({
        "schema_version": JSON_ENVELOPE_SCHEMA_VERSION,
        "command": command,
        "data": data,
        "errors": [],
    });
    println!("{env}");
}

/// Same as `print_envelope` but with a single error attached.
/// `data` is `null` so the field is always present (consumers can
/// expect `.data` to exist; checking `.errors.length === 0` is the
/// "did this succeed?" predicate). Callers should also `exit(1)`
/// after this prints — the function does NOT exit on its own so
/// callers can clean up first.
pub fn print_envelope_error(command: &str, error_message: impl Into<String>) {
    let env = serde_json::json!({
        "schema_version": JSON_ENVELOPE_SCHEMA_VERSION,
        "command": command,
        "data": serde_json::Value::Null,
        "errors": [error_message.into()],
    });
    println!("{env}");
}

/// Exec the plugin binary, inheriting stdio so the operator sees the
/// plugin's output directly. On Unix this is a true `execvp` — kannaka
/// is replaced by the plugin. On Windows we spawn + wait + propagate
/// exit code because true exec isn't a primitive.
pub fn exec_plugin(binary: PathBuf, args: Vec<String>) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&binary).args(&args).exec();
        // exec only returns on failure.
        eprintln!("error: failed to exec {}: {}", binary.display(), err);
        std::process::exit(126);
    }

    #[cfg(windows)]
    {
        let status = std::process::Command::new(&binary)
            .args(&args)
            .status();
        match status {
            Ok(s) => std::process::exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("error: failed to spawn {}: {}", binary.display(), e);
                std::process::exit(126);
            }
        }
    }
}
