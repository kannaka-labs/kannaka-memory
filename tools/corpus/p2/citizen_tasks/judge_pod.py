#!/usr/bin/env python3
"""A short-lived GPU judge: one qBraid BMA running ollama with the two local judges (qwen2.5:14b and
gemma-4-26B-A4B-it). Provision -> cutoff FIRST (verified) -> ssh -> user-space ollama -> pull ->
write the ssh alias to ~/kb2/judge-pod.alias -> wait for ~/kb2/judge-pod.STOP (or the cutoff) ->
TERMINATE. Always terminates in finally. Run from debain2's ~/qbraid-venv."""
import subprocess, sys, time, warnings
from pathlib import Path
warnings.filterwarnings("ignore")
from qbraid_core.services.compute import ComputeClient

PROFILE, MAX_MIN, IDLE_MIN = sys.argv[1] if len(sys.argv) > 1 else "gpu-rtx-4090", 100, 45
K = Path.home() / "kb2"
STOP, ALIAS = K / "judge-pod.STOP", K / "judge-pod.alias"
STOP.unlink(missing_ok=True); ALIAS.unlink(missing_ok=True)


def log(m):
    print(f"[judge-pod {time.strftime('%H:%M:%S')}] {m}", flush=True)


def ssh(alias, cmd, timeout=3600, check=False):
    return subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20", "-o", "ServerAliveInterval=30", alias, cmd],
                          capture_output=True, text=True, timeout=timeout, check=check)


c = ComputeClient()
cred0 = float(c.get_credits_balance().get("qbraidCredits") or 0)
log(f"credits before {cred0:.1f}")
iid = None
t0 = time.time()
try:
    inst = c.provision_bma_instance(PROFILE)
    iid = inst.instance_id
    log(f"instance {iid}")
    try:
        c.update_bma_cutoff(iid, auto_stop_idle_minutes=IDLE_MIN, max_session_minutes=MAX_MIN)
    except Exception as e:
        log(f"cutoff raised {type(e).__name__}; verifying")
    chk = c.get_bma_instance(iid)
    if int(chk.max_session_minutes or 0) != MAX_MIN:
        raise SystemExit(f"cutoff NOT applied ({chk.max_session_minutes}); terminating")
    log(f"cutoff verified {chk.max_session_minutes} min / idle {chk.auto_stop_idle_minutes}")
    inst = c.wait_for_bma_instance(iid, timeout=1800)
    if "running" not in str(inst.status).lower():
        raise SystemExit(f"not running: {inst.status}")
    t0 = time.time()
    cfg = c.configure_ssh_for_instance(iid)
    alias = cfg.get("alias") or c.bma_ssh_alias(iid)
    for _ in range(30):
        if ssh(alias, "echo up", timeout=60).returncode == 0:
            break
        time.sleep(10)
    boot = (
        "set -e; cd ~; nvidia-smi --query-gpu=name,memory.total --format=csv,noheader; "
        "python3 -m pip install -q zstandard; "
        "curl -sL -o ollama.tar.zst https://github.com/ollama/ollama/releases/latest/download/ollama-linux-amd64.tar.zst; "
        "mkdir -p ~/ollama && python3 -c \"import zstandard,tarfile; f=open('ollama.tar.zst','rb'); "
        "r=zstandard.ZstdDecompressor().stream_reader(f); tarfile.open(fileobj=r,mode='r|').extractall('/home/jovyan/ollama')\"; "
        "rm -f ollama.tar.zst; "
        "(OLLAMA_HOST=127.0.0.1:11434 OLLAMA_MAX_LOADED_MODELS=2 OLLAMA_KEEP_ALIVE=2h setsid -f ~/ollama/bin/ollama serve > ~/ollama.log 2>&1 < /dev/null); "
        "sleep 5; ~/ollama/bin/ollama pull qwen2.5:14b > /dev/null 2>&1; "
        "~/ollama/bin/ollama pull hf.co/unsloth/gemma-4-26B-A4B-it-GGUF:UD-Q4_K_M > /dev/null 2>&1; "
        "~/ollama/bin/ollama list"
    )
    r = ssh(alias, boot, timeout=3000)
    log(f"bootstrap rc={r.returncode}: {r.stdout.strip()[-600:]} {r.stderr.strip()[-300:]}")
    if r.returncode != 0:
        raise SystemExit("bootstrap failed")
    ALIAS.write_text(alias)
    log(f"READY alias written; waiting for {STOP}")
    while not STOP.exists() and time.time() - t0 < (MAX_MIN - 3) * 60:
        time.sleep(20)
    log("stop requested" if STOP.exists() else "cutoff near; winding down")
finally:
    if iid:
        try:
            c.terminate_bma_instance(iid)
            log(f"instance {iid} TERMINATED")
        except Exception as e:
            log(f"TERMINATE FAILED {e}; terminate by hand")
        log(f"session {(time.time() - t0) / 60:.1f} min")
        try:
            cred1 = float(c.get_credits_balance().get("qbraidCredits") or 0)
            log(f"credits {cred0:.1f} -> {cred1:.1f} (spent {cred0 - cred1:.1f})")
        except Exception:
            pass
    ALIAS.unlink(missing_ok=True)
