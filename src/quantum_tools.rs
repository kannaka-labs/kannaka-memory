//! Quantum tools for the `kannaka agent` harness — give the coding agent real
//! quantum capabilities by shelling out to the `kannaka-quantum` bridge
//! (https://github.com/kannaka-labs/kannaka-quantum), which runs circuits on
//! qBraid backends (free simulator by default; real QPUs when the account has
//! credits).
//!
//! The bridge is invoked as `python -m kannaka_quantum <cmd>` and prints a
//! single JSON object to stdout. Set `KANNAKA_QUANTUM_PYTHON` if `python`
//! isn't the right interpreter on PATH.

use crate::agent::Tool;
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Quantum jobs can sit in a device queue; give them headroom.
const BRIDGE_TIMEOUT_SECS: u64 = 300;

fn python_bin() -> String {
    std::env::var("KANNAKA_QUANTUM_PYTHON").unwrap_or_else(|_| "python".to_string())
}

/// True for the tools handled here (vs coding / memory tools).
pub fn is_quantum_tool(name: &str) -> bool {
    matches!(
        name,
        "quantum_devices" | "quantum_run" | "quantum_recall" | "quantum_random"
    )
}

/// The quantum toolset exposed to the model. Names/descriptions are `'static`
/// so the prompt cache stays stable across turns.
pub fn quantum_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "quantum_devices",
            description: "List available quantum devices across providers (qBraid + OpenQuantum) \
                          with status, qubit counts, and cost. The free 'qbraid:qbraid:sim:qir-sv' \
                          simulator (30 qubits) needs no credits; 'openquantum:*' entries are real \
                          QPUs (IonQ/Rigetti/IQM/AQT) that spend Spark credits (no free simulator).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "online": { "type": "boolean", "description": "Only ONLINE devices.", "default": false }
                }
            }),
        },
        Tool {
            name: "quantum_run",
            description: "Execute an OpenQASM 3 circuit on a backend and return measurement counts. \
                          Write standard OpenQASM 3.0 (include \"stdgates.inc\"; declare qubit[]/bit[]; \
                          apply gates; measure). Defaults to the free qBraid simulator. To run on a \
                          real QPU set device='openquantum:<backend>' (e.g. openquantum:iqm:garnet) \
                          AND allow_spend=true — it spends real Spark credits (a max_credits cap, \
                          default 1.0 credit, guards against an accidental large run).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "qasm3": { "type": "string", "description": "OpenQASM 3.0 source." },
                    "shots": { "type": "integer", "minimum": 1, "default": 100 },
                    "device": { "type": "string", "description": "Device id; default free qBraid simulator. Use 'openquantum:<backend>' for a real QPU." },
                    "allow_spend": { "type": "boolean", "default": false, "description": "Required to run on OpenQuantum (spends Spark credits)." },
                    "max_credits": { "type": "number", "description": "Credit ceiling for an OpenQuantum run (1 credit = $2; default 1.0)." }
                },
                "required": ["qasm3"]
            }),
        },
        Tool {
            name: "quantum_recall",
            description: "Run Kannaka's resonance recall AS a quantum circuit: amplitude-encode \
                          candidate memory resonances into a quantum state and amplitude-amplify \
                          toward the strongest — the quantum analogue of 'attention as gravity'. \
                          Returns the measured distribution over candidates and the top pick. \
                          Defaults to the free qBraid simulator; for the real-hardware artifact set \
                          device='openquantum:iqm:garnet' + allow_spend=true and keep shots LOW \
                          (recall defaults to 1024 shots — the max_credits cap, default 1.0, blocks \
                          a budget-draining run on a paid QPU).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "amplitudes": { "type": "array", "items": { "type": "number" }, "description": "Resonance scores for each candidate memory." },
                    "labels": { "type": "array", "items": { "type": "string" }, "description": "Optional labels aligned with amplitudes." },
                    "shots": { "type": "integer", "minimum": 1, "default": 1024 },
                    "amplify": { "type": "boolean", "default": true },
                    "device": { "type": "string", "description": "Device id; default free qBraid simulator. Use 'openquantum:<backend>' for a real QPU." },
                    "allow_spend": { "type": "boolean", "default": false, "description": "Required to run on OpenQuantum (spends Spark credits)." },
                    "max_credits": { "type": "number", "description": "Credit ceiling for an OpenQuantum run (default 1.0)." }
                },
                "required": ["amplitudes"]
            }),
        },
        Tool {
            name: "quantum_random",
            description: "Generate true quantum random bits from measurement collapse (not a PRNG). \
                          Returns the bitstring, its integer value, and a float in [0,1). Defaults to \
                          the free qBraid simulator; set device='openquantum:<backend>' + \
                          allow_spend=true to draw entropy from a real QPU (spends Spark credits).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "n_bits": { "type": "integer", "minimum": 1, "maximum": 256, "default": 8 },
                    "device": { "type": "string", "description": "Device id; default free qBraid simulator." },
                    "allow_spend": { "type": "boolean", "default": false, "description": "Required to run on OpenQuantum (spends Spark credits)." }
                }
            }),
        },
    ]
}

/// Execute a quantum tool by shelling out to the bridge. Returns
/// `(result_text, is_error)`.
pub fn dispatch_quantum_tool(name: &str, input: &Value) -> (String, bool) {
    match name {
        "quantum_devices" => {
            let mut args = vec!["devices".to_string()];
            if input.get("online").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("--online".to_string());
            }
            run_bridge(&args, None)
        }
        "quantum_run" => {
            let qasm = input.get("qasm3").and_then(|v| v.as_str()).unwrap_or("");
            if qasm.trim().is_empty() {
                return ("quantum_run requires a non-empty qasm3".into(), true);
            }
            let shots = input.get("shots").and_then(|v| v.as_u64()).unwrap_or(100).to_string();
            let mut args = vec!["run".to_string(), "--shots".to_string(), shots];
            push_device_arg(&mut args, input);
            push_spend_args(&mut args, input);
            // QASM goes over stdin — robust against length + shell quoting.
            run_bridge(&args, Some(qasm))
        }
        "quantum_recall" => {
            let amps = match input.get("amplitudes") {
                Some(v) if v.is_array() => v.to_string(),
                _ => return ("quantum_recall requires an amplitudes array".into(), true),
            };
            let mut args = vec!["recall".to_string(), "--amplitudes".to_string(), amps];
            if let Some(l) = input.get("labels") {
                if l.is_array() {
                    args.push("--labels".to_string());
                    args.push(l.to_string());
                }
            }
            if let Some(s) = input.get("shots").and_then(|v| v.as_u64()) {
                args.push("--shots".to_string());
                args.push(s.to_string());
            }
            if input.get("amplify").and_then(|v| v.as_bool()) == Some(false) {
                args.push("--no-amplify".to_string());
            }
            push_device_arg(&mut args, input);
            push_spend_args(&mut args, input);
            run_bridge(&args, None)
        }
        "quantum_random" => {
            let bits = input.get("n_bits").and_then(|v| v.as_u64()).unwrap_or(8).to_string();
            let mut args = vec!["qrng".to_string(), "--bits".to_string(), bits];
            push_device_arg(&mut args, input);
            push_spend_args(&mut args, input);
            run_bridge(&args, None)
        }
        other => (format!("unknown quantum tool: {other}"), true),
    }
}

/// Append `--device <id>` from a tool input (skips empty/missing).
fn push_device_arg(args: &mut Vec<String>, input: &Value) {
    if let Some(d) = input.get("device").and_then(|v| v.as_str()) {
        if !d.is_empty() {
            args.push("--device".to_string());
            args.push(d.to_string());
        }
    }
}

/// Append the OpenQuantum spend-guard flags (`--allow-spend`, `--max-credits`)
/// from a tool input. No-ops on the free qBraid simulator.
fn push_spend_args(args: &mut Vec<String>, input: &Value) {
    if input.get("allow_spend").and_then(|v| v.as_bool()) == Some(true) {
        args.push("--allow-spend".to_string());
    }
    if let Some(mc) = input.get("max_credits").and_then(|v| v.as_f64()) {
        args.push("--max-credits".to_string());
        args.push(mc.to_string());
    }
}

fn run_bridge(args: &[String], stdin_data: Option<&str>) -> (String, bool) {
    let py = python_bin();
    let mut full: Vec<String> = vec!["-m".to_string(), "kannaka_quantum".to_string()];
    full.extend_from_slice(args);

    let mut child = match Command::new(&py)
        .args(&full)
        .env("KANNAKA_QUIET", "1")
        .stdin(if stdin_data.is_some() { Stdio::piped() } else { Stdio::null() })
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

    if let Some(data) = stdin_data {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(data.as_bytes());
            let _ = si.flush();
        } // dropped here → stdin closed
    }

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
        return (
            "quantum job timed out (the device may be queued — try the free simulator)".into(),
            true,
        );
    }
    let body = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if body.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.trim().lines().last().unwrap_or("no output");
        return (format!("quantum bridge produced no result: {last}"), true);
    }
    // The bridge prints one JSON object; an {"error": ...} means failure.
    let is_error = serde_json::from_str::<Value>(&body)
        .map(|v| v.get("error").is_some())
        .unwrap_or(false);
    (body, is_error)
}
