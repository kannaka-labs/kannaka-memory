//! qBraid Lab / infrastructure tools for the `kannaka agent` harness.
//!
//! Where [`crate::quantum_tools`] runs *circuits* on qBraid, these tools manage
//! the *platform* around them — credits, environments, compute profiles, the
//! Lab server, and on-demand instances — by shelling out to the same
//! `kannaka-quantum` bridge (its `lab-*` subcommands, which wrap `qbraid_core`).
//!
//! Three safety tiers, classified here and gated by the agent handler:
//! - **read-only** ([`is_lab_readonly_tool`]) — visibility ops; never spend,
//!   never mutate → auto-allowed like the quantum tools.
//! - **free mutations** — env/package/kernel lifecycle + stop/down; cost no
//!   credits but change state → routed through human approval.
//! - **paid** ([`is_lab_paid_tool`]) — start/provision/resume compute; bill
//!   credits *per wall-clock minute* → require `allow_spend` + `max_credits`
//!   in the call AND human approval (the bridge refuses a paid run otherwise).

use crate::agent::Tool;
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Compute provisioning + `--wait` polling can take minutes; give it headroom.
const BRIDGE_TIMEOUT_SECS: u64 = 660;

fn python_bin() -> String {
    std::env::var("KANNAKA_QUANTUM_PYTHON").unwrap_or_else(|_| "python".to_string())
}

/// True for any tool handled here.
pub fn is_lab_tool(name: &str) -> bool {
    matches!(
        name,
        "lab_credits"
            | "lab_list_envs"
            | "lab_env_info"
            | "lab_list_profiles"
            | "lab_compute_status"
            | "lab_compute_usage"
            | "lab_list_instances"
            | "lab_list_kernels"
            | "lab_create_env"
            | "lab_delete_env"
            | "lab_pip_install"
            | "lab_pip_freeze"
            | "lab_add_kernel"
            | "lab_remove_kernel"
            | "lab_compute_up"
            | "lab_compute_down"
            | "lab_provision_instance"
            | "lab_start_instance"
            | "lab_stop_instance"
            | "lab_ssh_configure"
            | "lab_agent_launch"
            | "lab_agent_list"
            | "lab_agent_read"
            | "lab_agent_send"
            | "lab_agent_setup"
            | "lab_exec"
            | "lab_qos_boot"
            | "lab_watch"
            | "lab_qos_watch"
            | "lab_qos_swarm_bridge"
    )
}

/// Read-only / $0 visibility tools — safe to auto-allow.
pub fn is_lab_readonly_tool(name: &str) -> bool {
    matches!(
        name,
        "lab_credits"
            | "lab_list_envs"
            | "lab_env_info"
            | "lab_list_profiles"
            | "lab_compute_status"
            | "lab_compute_usage"
            | "lab_list_instances"
            | "lab_list_kernels"
            | "lab_pip_freeze"
            | "lab_agent_list"
            | "lab_agent_read"
    )
}

/// Tools that spend qBraid credits (per-minute compute) — must never be
/// auto-approved, and the bridge requires `allow_spend` + a `max_credits` cap.
pub fn is_lab_paid_tool(name: &str) -> bool {
    matches!(
        name,
        "lab_compute_up" | "lab_provision_instance" | "lab_start_instance"
    )
}

/// The lab toolset exposed to the model. Names/descriptions are `'static` so the
/// prompt cache stays stable across turns.
pub fn lab_tools() -> Vec<Tool> {
    vec![
        // --- Phase 1: read-only / $0 ------------------------------------- //
        Tool {
            name: "lab_credits",
            description: "Show the qBraid credit balance (1 credit = $0.01) and its USD value. Free, read-only.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        Tool {
            name: "lab_list_envs",
            description: "List qBraid environments available to the account (name, slug, description, tags). Free, read-only.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "page": { "type": "integer", "minimum": 1, "default": 1 },
                    "limit": { "type": "integer", "minimum": 1, "default": 20 }
                }
            }),
        },
        Tool {
            name: "lab_env_info",
            description: "Get full metadata for one qBraid environment by its slug. Free, read-only.",
            input_schema: json!({
                "type": "object",
                "properties": { "slug": { "type": "string" } },
                "required": ["slug"]
            }),
        },
        Tool {
            name: "lab_list_profiles",
            description: "List qBraid compute profiles (instance types) with live per-minute credit cost, USD/min, GPU \
                          flag, plan, and capacity. Call this BEFORE starting paid compute to choose a profile and see \
                          its burn rate. Free, read-only.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "gpu_only": { "type": "boolean", "default": false, "description": "Only GPU profiles." },
                    "available_only": { "type": "boolean", "default": false, "description": "Only profiles ready (with capacity)." },
                    "plan": { "type": "string", "description": "Filter by plan (e.g. Free, Standard, Pro)." },
                    "limit": { "type": "integer", "minimum": 1 }
                }
            }),
        },
        Tool {
            name: "lab_compute_status",
            description: "Report the qBraid Lab server status: whether it's running, on which profile, and its URL. Free, read-only.",
            input_schema: json!({
                "type": "object",
                "properties": { "cluster": { "type": "string", "description": "Cluster id (default 'lab')." } }
            }),
        },
        Tool {
            name: "lab_compute_usage",
            description: "Show qBraid compute usage and per-session credit rates / totals charged (optionally for the last N days). Free, read-only.",
            input_schema: json!({
                "type": "object",
                "properties": { "days": { "type": "integer", "minimum": 1 } }
            }),
        },
        Tool {
            name: "lab_list_instances",
            description: "List on-demand (BMA) qBraid compute instances with status, URL, and running/stopped credits-per-minute. Free, read-only.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        Tool {
            name: "lab_list_kernels",
            description: "List Jupyter kernels installed locally (only meaningful when running inside qBraid Lab / on a provisioned instance). Free, read-only.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        // --- Phase 2: free mutations (env / package / kernel) ------------ //
        Tool {
            name: "lab_create_env",
            description: "Create a qBraid environment. Any 'packages' listed install during the (async, cloud-side) build — \
                          this is the way to install packages into a qBraid environment when not running inside Lab. \
                          Free (no credits).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "python_version": { "type": "string", "description": "e.g. 3.11" },
                    "packages": {
                        "description": "Object {name: version} or array of package names to install during the build.",
                        "oneOf": [ { "type": "object" }, { "type": "array", "items": { "type": "string" } } ]
                    },
                    "kernel_name": { "type": "string" },
                    "visibility": { "type": "string", "default": "private" },
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "lab_delete_env",
            description: "Delete a qBraid environment by slug. Free (no credits).",
            input_schema: json!({
                "type": "object",
                "properties": { "slug": { "type": "string" } },
                "required": ["slug"]
            }),
        },
        Tool {
            name: "lab_pip_install",
            description: "pip-install packages into a LOCALLY-installed environment (keyed by its local env_id, NOT the cloud \
                          slug). Only works inside qBraid Lab / on a provisioned instance. Free.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "env_id": { "type": "string", "description": "Local registry env id (not the cloud slug)." },
                    "packages": { "type": "array", "items": { "type": "string" } },
                    "upgrade_pip": { "type": "boolean", "default": false }
                },
                "required": ["env_id", "packages"]
            }),
        },
        Tool {
            name: "lab_pip_freeze",
            description: "List packages installed in a locally-installed environment (in-Lab only). Free, read-only.",
            input_schema: json!({
                "type": "object",
                "properties": { "env_id": { "type": "string" } },
                "required": ["env_id"]
            }),
        },
        Tool {
            name: "lab_add_kernel",
            description: "Register a Jupyter kernel for a (locally-installed) environment (in-Lab only). Free.",
            input_schema: json!({
                "type": "object",
                "properties": { "environment": { "type": "string" } },
                "required": ["environment"]
            }),
        },
        Tool {
            name: "lab_remove_kernel",
            description: "Remove a Jupyter kernel for an environment (in-Lab only). Free.",
            input_schema: json!({
                "type": "object",
                "properties": { "environment": { "type": "string" } },
                "required": ["environment"]
            }),
        },
        // --- Phase 3: paid compute provisioning -------------------------- //
        Tool {
            name: "lab_compute_up",
            description: "Start the qBraid Lab server on a compute profile. PAID — bills qBraid credits per wall-clock minute \
                          until stopped. Requires allow_spend=true and a max_credits ceiling (the balance you accept to \
                          risk). Call lab_list_profiles first for the burn rate; stop with lab_compute_down.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string", "description": "Profile slug from lab_list_profiles." },
                    "allow_spend": { "type": "boolean", "default": false, "description": "Required true — paid compute." },
                    "max_credits": { "type": "number", "description": "Committed credit ceiling (default 60 ≈ $0.60)." },
                    "cluster": { "type": "string" },
                    "wait": { "type": "boolean", "default": false, "description": "Block until the server is ready." },
                    "timeout": { "type": "number", "description": "Seconds to wait when wait=true." }
                },
                "required": ["profile", "allow_spend", "max_credits"]
            }),
        },
        Tool {
            name: "lab_compute_down",
            description: "Stop the running qBraid Lab server (preserves disk, stops per-minute billing). Free.",
            input_schema: json!({
                "type": "object",
                "properties": { "cluster": { "type": "string" } }
            }),
        },
        Tool {
            name: "lab_provision_instance",
            description: "Provision (launch) a new on-demand qBraid compute instance on a profile. PAID — per-minute credits. \
                          Requires allow_spend=true + max_credits. Pause it later with lab_stop_instance.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile": { "type": "string", "description": "Profile slug from lab_list_profiles." },
                    "allow_spend": { "type": "boolean", "default": false },
                    "max_credits": { "type": "number" },
                    "wait": { "type": "boolean", "default": false },
                    "timeout": { "type": "number" }
                },
                "required": ["profile", "allow_spend", "max_credits"]
            }),
        },
        Tool {
            name: "lab_start_instance",
            description: "Resume a stopped on-demand instance by instance_id. PAID — per-minute credits. Requires allow_spend=true + max_credits.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "instance_id": { "type": "string" },
                    "allow_spend": { "type": "boolean", "default": false },
                    "max_credits": { "type": "number" }
                },
                "required": ["instance_id", "allow_spend", "max_credits"]
            }),
        },
        Tool {
            name: "lab_stop_instance",
            description: "Stop (pause) an on-demand instance by instance_id — preserves disk, stops running billing (a stopped \
                          instance still bills a small disk rate; terminate-to-delete is via the web UI by design). Free to call.",
            input_schema: json!({
                "type": "object",
                "properties": { "instance_id": { "type": "string" } },
                "required": ["instance_id"]
            }),
        },
        // --- Phase 4: remote agents (run a coding agent on an instance) --- //
        Tool {
            name: "lab_ssh_configure",
            description: "Configure local SSH access to a running on-demand instance and return its stable SSH alias. \
                          Run this once before any lab_agent_* tool. Free.",
            input_schema: json!({
                "type": "object",
                "properties": { "instance_id": { "type": "string" } },
                "required": ["instance_id"]
            }),
        },
        Tool {
            name: "lab_agent_setup",
            description: "Prepare a remote instance so a launched claude agent runs AUTONOMOUSLY: injects your Anthropic \
                          API key (resolved locally from ~/.kannaka config or ANTHROPIC_API_KEY — NOT passed here), \
                          pre-accepts onboarding, and pins a valid model. Run once after lab_ssh_configure, before \
                          lab_agent_launch. Uploads the key to the instance (stored 0600).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string", "description": "From lab_ssh_configure." },
                    "provider": { "type": "string", "default": "anthropic", "description": "Currently 'anthropic' (claude)." },
                    "model": { "type": "string", "default": "claude-sonnet-4-6", "description": "Model to pin (the image default may be unavailable)." }
                },
                "required": ["ssh_alias"]
            }),
        },
        Tool {
            name: "lab_agent_launch",
            description: "Launch a coding agent (claude / codex / opencode) ON a remote provisioned instance over SSH — \
                          have Kannaka drive another agent on cloud compute. Run lab_ssh_configure + lab_agent_setup \
                          first (else claude stops at its login prompt). qBraid's launch does NOT auto-submit \
                          instructions — after launching, drive the agent with lab_agent_send. Instance must be running.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string", "description": "From lab_ssh_configure." },
                    "tool": { "type": "string", "default": "claude", "description": "claude | codex | opencode" },
                    "instructions": { "type": "string", "description": "Initial task/prompt for the remote agent." },
                    "cwd": { "type": "string" },
                    "name": { "type": "string" },
                    "agent_type": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["ssh_alias"]
            }),
        },
        Tool {
            name: "lab_agent_list",
            description: "List coding agents running on a remote instance. Read-only.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string" },
                    "include_stopped": { "type": "boolean", "default": false }
                },
                "required": ["ssh_alias"]
            }),
        },
        Tool {
            name: "lab_agent_read",
            description: "Read recent terminal output from a remote agent session. Read-only.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string" },
                    "session_id": { "type": "string" },
                    "lines": { "type": "integer", "minimum": 1, "default": 50 }
                },
                "required": ["ssh_alias", "session_id"]
            }),
        },
        Tool {
            name: "lab_agent_send",
            description: "Send a prompt / keystrokes to a remote agent session (drive it). Mutating.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string" },
                    "session_id": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["ssh_alias", "session_id", "text"]
            }),
        },
        // --- Phase 5: remote shell + QuantumOS boot ----------------------- //
        Tool {
            name: "lab_exec",
            description: "Run a shell command on a provisioned instance over its SSH alias and return rc + capped \
                          stdout/stderr (a non-zero exit comes back as data, not an error). Costs nothing beyond \
                          the instance's own per-minute bill. Requires lab_ssh_configure first.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string", "description": "From lab_ssh_configure." },
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "default": 90 }
                },
                "required": ["ssh_alias", "command"]
            }),
        },
        Tool {
            name: "lab_qos_boot",
            description: "Boot QuantumOS in QEMU on a provisioned instance: installs missing deps, clones/updates \
                          the repo, builds the kernel, and boots it in a detached tmux session (serial console). \
                          Returns the boot tail with a 'booted' flag keyed on the kernel's 'QuantumOS ready' line. \
                          Idempotent — an already-running session is reported, not clobbered (pass fresh=true to \
                          rebuild+reboot). Watch it live with lab_watch. Requires lab_ssh_configure + an active lease.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string", "description": "From lab_ssh_configure." },
                    "repo": { "type": "string", "description": "Clone URL (default: flaukowski/QuantumOS)." },
                    "ref": { "type": "string", "description": "Branch / tag / sha to check out." },
                    "session": { "type": "string", "default": "qos", "description": "tmux session name on the instance." },
                    "fresh": { "type": "boolean", "default": false, "description": "Kill an existing session and reboot from a rebuilt kernel." },
                    "timeout_secs": { "type": "integer", "minimum": 30, "default": 540 },
                    "qseed": { "type": "string", "description": "Kernel boot entropy (qseed= cmdline): 'reservoir' draws 64 raw QPU bits from the local quantum-entropy reservoir (provenance chain included, errors if empty); or pass <=16 hex digits. The kernel echoes the accepted seed; qseed_confirmed reports the round-trip." },
                    "graphical": { "type": "boolean", "default": false, "description": "Boot with a VGA framebuffer over VNC (paused, -S) + install noVNC, instead of the serial console — so the wave-interference splash is watchable in a browser via lab_qos_watch." },
                    "network": { "type": "boolean", "default": false, "description": "Boot with an rtl8139 NIC on QEMU user-mode networking (SLIRP, rootless, NAT to the instance's real internet) so QuantumOS runs its full stack: the boot self-test does ARP/DHCP/ICMP/DNS and the ring-3 shell gains a working nslookup/udping/http (the TCP client fetches real pages). Default is NIC-less." },
                    "quiet": { "type": "boolean", "default": false, "description": "Silence the demo kernel's steady-state console chatter (the timer-tick heartbeat + paradoxd/ghostd narration) for a clean interactive qsh prompt. Pairs with network for a usable networked shell." },
                    "web_port": { "type": "integer", "default": 6080, "description": "websockify web port for --graphical." },
                    "monitor_port": { "type": "integer", "default": 4444, "description": "QEMU TCP monitor port for --graphical (used by lab_qos_watch to resume)." }
                },
                "required": ["ssh_alias"]
            }),
        },
        Tool {
            name: "lab_watch",
            description: "Open a LOCAL terminal window attached to a tmux session on the instance (e.g. the \
                          QuantumOS serial console from lab_qos_boot) so the user can watch the machine live. \
                          Spawns a window on the user's desktop; the session survives the window closing. \
                          If no window can be opened, returns the manual attach command instead.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string", "description": "From lab_ssh_configure." },
                    "session": { "type": "string", "default": "qos", "description": "tmux session to attach." }
                },
                "required": ["ssh_alias"]
            }),
        },
        Tool {
            name: "lab_qos_watch",
            description: "Open the GRAPHICAL QuantumOS watch for a lab_qos_boot(graphical=true) VM: an SSH -L tunnel \
                          to the instance's noVNC, the browser at vnc_lite.html, and a monitor 'cont' to resume the \
                          paused (-S) VM so the wave-interference splash animates from frame 1. The graphical analog \
                          of lab_watch (a browser window instead of a terminal). Runs locally on the user's desktop.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string", "description": "From lab_ssh_configure." },
                    "session": { "type": "string", "default": "qos" },
                    "web_port": { "type": "integer", "default": 6080, "description": "websockify web port (from lab_qos_boot)." },
                    "monitor_port": { "type": "integer", "default": 4444, "description": "QEMU TCP monitor port (from lab_qos_boot)." }
                },
                "required": ["ssh_alias"]
            }),
        },
        Tool {
            name: "lab_qos_swarm_bridge",
            description: "Join a booted QuantumOS instance to the NATS swarm under its OWN Lamport-signed \
                          boot identity. QuantumOS's ring-3 swarm service emits a signed attestation on COM2 \
                          as it boots; this tails that COM2 log (from lab_qos_boot's com2_log), verifies the \
                          attestation, and ONLY if it verifies joins the swarm as qos-<qseed> — refusing to \
                          join on a bad attestation, so peer-presence is backed by a genuine signed boot. \
                          Runs locally on the user's desktop against the local kannaka + ~/.kannaka-nats.env. \
                          Returns the joined agent-id and a background join_pid holding presence; confirm with \
                          `kannaka swarm peers`. Graphical flow only (needs COM2, which the graphical boot wires).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ssh_alias": { "type": "string", "description": "From lab_ssh_configure." },
                    "session": { "type": "string", "default": "qos", "description": "Boot session id (from lab_qos_boot) — selects the COM2 log." },
                    "qseed": { "type": "string", "description": "The qseed hex from lab_qos_boot's return; cross-checked against the attestation and used to derive the agent-id. Omit to skip the cross-check and derive the id from the attestation." },
                    "agent_id": { "type": "string", "description": "Override the derived qos-<qseed> agent-id." }
                },
                "required": ["ssh_alias"]
            }),
        },
    ]
}

/// Execute a lab tool by shelling out to the bridge's matching `lab-*`
/// subcommand. Returns `(result_text, is_error)`.
pub fn dispatch_lab_tool(name: &str, input: &Value) -> (String, bool) {
    // Enforce the paid-compute contract at the harness, not only in the bridge:
    // the bridge has an env-var opt-in path, so the Rust side must independently
    // require an explicit allow_spend=true + a positive max_credits ceiling.
    if is_lab_paid_tool(name) {
        if input.get("allow_spend").and_then(|v| v.as_bool()) != Some(true) {
            return (
                format!(
                    "{name} is a PAID per-minute compute action and requires allow_spend=true. \
                     Call lab_credits + lab_list_profiles first, then retry with allow_spend=true \
                     and a max_credits ceiling."
                ),
                true,
            );
        }
        if let Err(e) = req_num(input, "max_credits") {
            return (e, true);
        }
    }
    let mut args: Vec<String> = Vec::new();
    match name {
        "lab_credits" => args.push("lab-credits".into()),
        "lab_list_envs" => {
            args.push("lab-list-envs".into());
            push_int(&mut args, "--page", input, "page");
            push_int(&mut args, "--limit", input, "limit");
        }
        "lab_env_info" => {
            let slug = match req_str(input, "slug") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-env-info".into());
            args.push("--slug".into());
            args.push(slug);
        }
        "lab_list_profiles" => {
            args.push("lab-list-profiles".into());
            push_flag(&mut args, "--gpu-only", input, "gpu_only");
            push_flag(&mut args, "--available-only", input, "available_only");
            push_str_opt(&mut args, "--plan", input, "plan");
            push_int(&mut args, "--limit", input, "limit");
        }
        "lab_compute_status" => {
            args.push("lab-compute-status".into());
            push_str_opt(&mut args, "--cluster", input, "cluster");
        }
        "lab_compute_usage" => {
            args.push("lab-compute-usage".into());
            push_int(&mut args, "--days", input, "days");
        }
        "lab_list_instances" => args.push("lab-list-instances".into()),
        "lab_list_kernels" => args.push("lab-list-kernels".into()),
        "lab_create_env" => {
            let n = match req_str(input, "name") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-create-env".into());
            args.push("--name".into());
            args.push(n);
            push_str_opt(&mut args, "--description", input, "description");
            push_str_opt(&mut args, "--python-version", input, "python_version");
            push_json_opt(&mut args, "--packages", input, "packages");
            push_str_opt(&mut args, "--kernel-name", input, "kernel_name");
            push_str_opt(&mut args, "--visibility", input, "visibility");
            push_json_opt(&mut args, "--tags", input, "tags");
        }
        "lab_delete_env" => {
            let slug = match req_str(input, "slug") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-delete-env".into());
            args.push("--slug".into());
            args.push(slug);
        }
        "lab_pip_install" => {
            let env_id = match req_str(input, "env_id") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            match input.get("packages") {
                Some(p) if p.is_array() => {
                    args.push("lab-pip-install".into());
                    args.push("--env-id".into());
                    args.push(env_id);
                    args.push("--packages".into());
                    args.push(p.to_string());
                    push_flag(&mut args, "--upgrade-pip", input, "upgrade_pip");
                }
                _ => return ("lab_pip_install requires a packages array".into(), true),
            }
        }
        "lab_pip_freeze" => {
            let env_id = match req_str(input, "env_id") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-pip-freeze".into());
            args.push("--env-id".into());
            args.push(env_id);
        }
        "lab_add_kernel" | "lab_remove_kernel" => {
            let env = match req_str(input, "environment") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push(if name == "lab_add_kernel" {
                "lab-add-kernel".into()
            } else {
                "lab-remove-kernel".into()
            });
            args.push("--environment".into());
            args.push(env);
        }
        "lab_compute_up" => {
            let profile = match req_str(input, "profile") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-compute-up".into());
            args.push("--profile".into());
            args.push(profile);
            push_str_opt(&mut args, "--cluster", input, "cluster");
            push_flag(&mut args, "--wait", input, "wait");
            push_num(&mut args, "--timeout", input, "timeout");
            push_spend_args(&mut args, input);
        }
        "lab_compute_down" => {
            args.push("lab-compute-down".into());
            push_str_opt(&mut args, "--cluster", input, "cluster");
        }
        "lab_provision_instance" => {
            let profile = match req_str(input, "profile") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-provision-instance".into());
            args.push("--profile".into());
            args.push(profile);
            push_flag(&mut args, "--wait", input, "wait");
            push_num(&mut args, "--timeout", input, "timeout");
            push_spend_args(&mut args, input);
        }
        "lab_start_instance" => {
            let id = match req_str(input, "instance_id") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-start-instance".into());
            args.push("--instance-id".into());
            args.push(id);
            push_spend_args(&mut args, input);
        }
        "lab_stop_instance" => {
            let id = match req_str(input, "instance_id") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-stop-instance".into());
            args.push("--instance-id".into());
            args.push(id);
        }
        "lab_ssh_configure" => {
            let id = match req_str(input, "instance_id") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-ssh-configure".into());
            args.push("--instance-id".into());
            args.push(id);
        }
        "lab_agent_launch" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-agent-launch".into());
            args.push("--ssh-alias".into());
            args.push(alias);
            push_str_opt(&mut args, "--tool", input, "tool");
            push_str_opt(&mut args, "--instructions", input, "instructions");
            push_str_opt(&mut args, "--cwd", input, "cwd");
            push_str_opt(&mut args, "--name", input, "name");
            push_str_opt(&mut args, "--agent-type", input, "agent_type");
            push_json_opt(&mut args, "--tags", input, "tags");
        }
        "lab_agent_list" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-agent-list".into());
            args.push("--ssh-alias".into());
            args.push(alias);
            push_flag(&mut args, "--include-stopped", input, "include_stopped");
        }
        "lab_agent_read" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            let sid = match req_str(input, "session_id") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-agent-read".into());
            args.push("--ssh-alias".into());
            args.push(alias);
            args.push("--session-id".into());
            args.push(sid);
            push_int(&mut args, "--lines", input, "lines");
        }
        "lab_agent_send" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            let sid = match req_str(input, "session_id") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            let text = match req_str(input, "text") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-agent-send".into());
            args.push("--ssh-alias".into());
            args.push(alias);
            args.push("--session-id".into());
            args.push(sid);
            args.push("--text".into());
            args.push(text);
        }
        "lab_agent_setup" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-agent-setup".into());
            args.push("--ssh-alias".into());
            args.push(alias);
            push_str_opt(&mut args, "--provider", input, "provider");
            push_str_opt(&mut args, "--model", input, "model");
        }
        "lab_exec" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            let cmd = match req_str(input, "command") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-exec".into());
            args.push("--ssh-alias".into());
            args.push(alias);
            args.push("--command".into());
            args.push(cmd);
            push_int(&mut args, "--timeout-secs", input, "timeout_secs");
        }
        "lab_qos_boot" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-qos-boot".into());
            args.push("--ssh-alias".into());
            args.push(alias);
            push_str_opt(&mut args, "--repo", input, "repo");
            push_str_opt(&mut args, "--ref", input, "ref");
            push_str_opt(&mut args, "--session", input, "session");
            push_flag(&mut args, "--fresh", input, "fresh");
            push_int(&mut args, "--timeout-secs", input, "timeout_secs");
            push_str_opt(&mut args, "--qseed", input, "qseed");
            push_flag(&mut args, "--graphical", input, "graphical");
            push_flag(&mut args, "--network", input, "network");
            push_flag(&mut args, "--quiet", input, "quiet");
            push_int(&mut args, "--web-port", input, "web_port");
            push_int(&mut args, "--monitor-port", input, "monitor_port");
        }
        // Graphical watch: the quantum CLI's lab-watch opens the SSH -L tunnel +
        // browser + resume locally (detached), so a bridge call is the right shape.
        "lab_qos_watch" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-watch".into());
            args.push("--ssh-alias".into());
            args.push(alias);
            push_str_opt(&mut args, "--session", input, "session");
            push_int(&mut args, "--web-port", input, "web_port");
            push_int(&mut args, "--monitor-port", input, "monitor_port");
        }
        "lab_qos_swarm_bridge" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            args.push("lab-qos-swarm-bridge".into());
            args.push("--ssh-alias".into());
            args.push(alias);
            push_str_opt(&mut args, "--session", input, "session");
            push_str_opt(&mut args, "--qseed", input, "qseed");
            push_str_opt(&mut args, "--agent-id", input, "agent_id");
        }
        // Local, not a bridge call: opens a window on the user's own desktop.
        "lab_watch" => {
            let alias = match req_str(input, "ssh_alias") {
                Ok(s) => s,
                Err(e) => return (e, true),
            };
            let session = input
                .get("session")
                .and_then(|v| v.as_str())
                .unwrap_or("qos")
                .to_string();
            return spawn_watch_window(&alias, &session);
        }
        other => return (format!("unknown lab tool: {other}"), true),
    }
    run_bridge(&args)
}

/// True when a string is safe to pass into a locally-spawned terminal command
/// line without quoting concerns (ssh aliases and tmux session names are
/// simple tokens; anything else is refused rather than escaped).
fn is_plain_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The ssh binary for the watch window. On Windows the alias config +
/// ProxyCommand written by lab_ssh_configure target native OpenSSH; an MSYS
/// ssh (e.g. Git's) can't parse the Windows-path Include, so prefer the
/// System32 binary explicitly over whatever shadows `ssh` on PATH.
fn ssh_bin() -> String {
    if cfg!(windows) {
        let native = r"C:\Windows\System32\OpenSSH\ssh.exe";
        if std::path::Path::new(native).exists() {
            return native.to_string();
        }
    }
    "ssh".to_string()
}

/// Open a LOCAL terminal window running `ssh -t <alias> tmux attach -t <session>`
/// so the user can watch a session (e.g. the QuantumOS serial console) live.
/// Returns `(result_text, is_error)` like every other dispatch path. The
/// manual attach command is always included so a failed spawn is
/// self-serviceable.
fn spawn_watch_window(alias: &str, session: &str) -> (String, bool) {
    if !is_plain_token(alias) || !is_plain_token(session) {
        return (
            format!("lab_watch: refusing non-plain ssh_alias/session ('{alias}' / '{session}')"),
            true,
        );
    }
    let ssh = ssh_bin();
    let attach = format!("{ssh} -t {alias} tmux attach -t {session}");
    let ssh_args = ["-t", alias, "tmux", "attach", "-t", session];

    let spawned: Result<String, String> = if cfg!(windows) {
        // Windows Terminal opens a clean new window; classic `start` via cmd
        // is the fallback on hosts without wt.
        let wt = Command::new("wt.exe")
            .arg("new-window")
            .arg("--")
            .arg(&ssh)
            .args(ssh_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match wt {
            Ok(_) => Ok("Windows Terminal".to_string()),
            Err(_) => Command::new("cmd")
                .args(["/C", "start", "kannaka lab watch"])
                .arg(&ssh)
                .args(ssh_args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map(|_| "console window".to_string())
                .map_err(|e| e.to_string()),
        }
    } else if cfg!(target_os = "macos") {
        Command::new("osascript")
            .args([
                "-e",
                &format!("tell application \"Terminal\" to do script \"{attach}\""),
            ])
            .args(["-e", "tell application \"Terminal\" to activate"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| "Terminal.app".to_string())
            .map_err(|e| e.to_string())
    } else {
        // Linux: honor $TERMINAL, then the common emulators. `-e` is the
        // widest-supported "run this command" flag.
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(t) = std::env::var("TERMINAL") {
            if !t.is_empty() {
                candidates.push(t);
            }
        }
        for c in ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"] {
            candidates.push(c.to_string());
        }
        let mut last_err = "no terminal emulator found".to_string();
        let mut opened = None;
        for term in candidates {
            let r = Command::new(&term)
                .arg("-e")
                .arg(&attach)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match r {
                Ok(_) => {
                    opened = Some(term);
                    break;
                }
                Err(e) => last_err = format!("{term}: {e}"),
            }
        }
        opened.ok_or(last_err)
    };

    match spawned {
        Ok(via) => (
            json!({
                "opened": true,
                "via": via,
                "ssh_alias": alias,
                "session": session,
                "attach": attach,
                "note": "window is a viewer only — the tmux session (and QEMU) survive closing it"
            })
            .to_string(),
            false,
        ),
        Err(e) => (
            json!({
                "opened": false,
                "error": format!("could not open a terminal window: {e}"),
                "attach": attach,
                "note": "run the attach command manually in any terminal"
            })
            .to_string(),
            true,
        ),
    }
}

fn req_str(input: &Value, key: &str) -> Result<String, String> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(format!("lab tool requires a non-empty '{key}'")),
    }
}

/// Require a strictly-positive numeric input (the committed credit ceiling).
fn req_num(input: &Value, key: &str) -> Result<f64, String> {
    match input.get(key).and_then(|v| v.as_f64()) {
        Some(n) if n > 0.0 => Ok(n),
        _ => Err(format!(
            "paid lab tool requires a positive numeric '{key}' (committed credit ceiling)"
        )),
    }
}

/// Append `--flag` if the named boolean input is true.
fn push_flag(args: &mut Vec<String>, flag: &str, input: &Value, key: &str) {
    if input.get(key).and_then(|v| v.as_bool()) == Some(true) {
        args.push(flag.to_string());
    }
}

/// Append `--opt <value>` for a non-empty string input.
fn push_str_opt(args: &mut Vec<String>, opt: &str, input: &Value, key: &str) {
    if let Some(s) = input.get(key).and_then(|v| v.as_str()) {
        if !s.is_empty() {
            args.push(opt.to_string());
            args.push(s.to_string());
        }
    }
}

/// Append `--opt <int>` for an integer input.
fn push_int(args: &mut Vec<String>, opt: &str, input: &Value, key: &str) {
    if let Some(n) = input.get(key).and_then(|v| v.as_i64()) {
        args.push(opt.to_string());
        args.push(n.to_string());
    }
}

/// Append `--opt <number>` for a numeric input.
fn push_num(args: &mut Vec<String>, opt: &str, input: &Value, key: &str) {
    if let Some(n) = input.get(key).and_then(|v| v.as_f64()) {
        args.push(opt.to_string());
        args.push(n.to_string());
    }
}

/// Append `--opt <json>` for an array/object input (the bridge parses it).
fn push_json_opt(args: &mut Vec<String>, opt: &str, input: &Value, key: &str) {
    if let Some(v) = input.get(key) {
        if v.is_array() || v.is_object() {
            args.push(opt.to_string());
            args.push(v.to_string());
        } else if let Some(s) = v.as_str() {
            // The bridge's _parse_json_arg also accepts a CSV/JSON string, so
            // forward a string form rather than silently dropping it.
            if !s.is_empty() {
                args.push(opt.to_string());
                args.push(s.to_string());
            }
        }
    }
}

/// Append `--allow-spend` / `--max-credits` from the tool input (paid tools).
fn push_spend_args(args: &mut Vec<String>, input: &Value) {
    if input.get("allow_spend").and_then(|v| v.as_bool()) == Some(true) {
        args.push("--allow-spend".to_string());
    }
    if let Some(mc) = input.get("max_credits").and_then(|v| v.as_f64()) {
        args.push("--max-credits".to_string());
        args.push(mc.to_string());
    }
}

fn run_bridge(args: &[String]) -> (String, bool) {
    // NOTE: the pipes are drained only after the child exits (via
    // wait_with_output below). This is safe ONLY because every `lab-*`
    // subcommand prints a single small JSON object (cli.py does one
    // `print(json.dumps(...))`, no streaming) — well under the OS pipe buffer.
    // If a subcommand is ever made to stream large output, switch to draining
    // stdout/stderr on reader threads to avoid a pipe-buffer deadlock.
    let py = python_bin();
    let mut full: Vec<String> = vec!["-m".to_string(), "kannaka_quantum".to_string()];
    full.extend_from_slice(args);

    let mut child = match Command::new(&py)
        .args(&full)
        // Force Python UTF-8 mode: the remote-agent tools read a remote TUI
        // whose box-drawing chars crash qBraid's default-cp1252 subprocess
        // decoding on Windows. UTF-8 mode makes that decode correctly.
        .env("PYTHONUTF8", "1")
        .env("KANNAKA_QUIET", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                format!(
                    "quantum bridge unavailable: could not run '{py} -m kannaka_quantum' ({e}). \
                     Install it: pip install git+https://github.com/kannaka-labs/kannaka-quantum \
                     (set KANNAKA_QUANTUM_PYTHON if python isn't on PATH)."
                ),
                true,
            );
        }
    };
    let _ = child.stdin.take(); // explicitly close stdin

    let start = Instant::now();
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(BRIDGE_TIMEOUT_SECS) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return (format!("quantum bridge wait failed: {e}"), true),
        }
    };

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return (format!("quantum bridge output failed: {e}"), true),
    };
    if timed_out {
        return ("lab operation timed out (a compute action may still be in progress — check lab_compute_status)".into(), true);
    }
    let body = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if body.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.trim().lines().last().unwrap_or("no output");
        return (format!("quantum bridge produced no result: {last}"), true);
    }
    let is_error = serde_json::from_str::<Value>(&body)
        .map(|v| v.get("error").is_some())
        .unwrap_or(false);
    (body, is_error)
}
