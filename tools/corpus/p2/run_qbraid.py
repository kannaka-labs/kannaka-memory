#!/usr/bin/env python3
"""ADR-0057 P2 — run train_lora.py on a qBraid GPU, gated the way the ADR says.

Every qBraid GPU profile is an on-demand BMA instance. This script:
  1. refuses to spend unless --allow-spend is given, and only on a whitelisted
     profile (single-GPU; multi-GPU needs Nick)
  2. provisions the instance, sets the cutoff BEFORE any work
     (max_session_minutes, auto_stop_idle_minutes), waits for `running`
  3. configures a per-instance SSH alias and ships the SFT data + trainer
  4. bootstraps the pod (pip, llama.cpp), launches training under nohup,
     tails the log until train.manifest.json appears (or the cutoff kills it)
  5. fetches adapter/ manifest/ samples/ (and gguf/ if made) to --fetch-to
  6. stops (default) or --terminate the instance. Disk is kept on stop.

  python run_qbraid.py --profile gpu-rtx-4090 --data ~/.kannaka-corpus/sft \
      --base Qwen/Qwen2.5-1.5B-Instruct --max-steps 30 --max-minutes 45 --allow-spend   # smoke
  python run_qbraid.py --profile gpu-a100-sxm --data ~/.kannaka-corpus/sft \
      --base Qwen/Qwen2.5-14B-Instruct --qlora --epochs 2 --merge --gguf q4_K_M \
      --max-minutes 240 --allow-spend                                            # the real one

Nothing here contacts anyone but qBraid. Outputs land outside any repo.
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ALLOWED = {"gpu-rtx-4090": 0.87, "gpu-l40s": 2.28, "gpu-a100-sxm": 2.49, "gpu-h100-sxm": 5.37}  # $/h, single GPU only
# Measured 2026-09-05: a $0.87/h profile bills 1.45 credits/min => 1 qBraid credit = $0.01.
CREDIT_USD = 0.01
REMOTE = "~/kannaka-p2"


try:  # Windows consoles default to cp1252; never let a log line kill a paid run
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass


def log(msg):
    print(f"[qbraid {time.strftime('%H:%M:%S')}] {msg}", flush=True)


# On Windows the SDK writes ssh config entries with backslash paths and a python
# ProxyCommand; Git-Bash's MSYS ssh mangles those. Use the native OpenSSH.
_WIN_SSH = Path("C:/WINDOWS/System32/OpenSSH")
SSH = str(_WIN_SSH / "ssh.exe") if (_WIN_SSH / "ssh.exe").exists() else "ssh"
SCP = str(_WIN_SSH / "scp.exe") if (_WIN_SSH / "scp.exe").exists() else "scp"


def sh(cmd, check=True, capture=False, **kw):
    if cmd and cmd[0] in ("ssh", "scp"):
        cmd = [SSH if cmd[0] == "ssh" else SCP] + list(cmd[1:])
    if capture:
        return subprocess.run(cmd, check=check, capture_output=True, text=True, **kw)
    return subprocess.run(cmd, check=check, **kw)


def ssh(alias, remote_cmd, check=True, capture=False, timeout=None):
    return sh(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20", "-o", "ServerAliveInterval=30",
               alias, remote_cmd], check=check, capture=capture, timeout=timeout)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--profile", required=True, choices=sorted(ALLOWED))
    ap.add_argument("--data", required=True, help="dir with train.jsonl/holdout.jsonl")
    ap.add_argument("--base", default="Qwen/Qwen2.5-14B-Instruct")
    ap.add_argument("--max-minutes", type=int, default=240, help="hard session cutoff (ADR-0057: 4 h)")
    ap.add_argument("--idle-minutes", type=int, default=15)
    ap.add_argument("--provision-timeout", type=int, default=1800, help="seconds to wait for the BMA to boot (GPU boots have taken 10+ min)")
    ap.add_argument("--allow-spend", action="store_true", help="REQUIRED to provision anything")
    ap.add_argument("--fetch-to", default=str(Path.home() / ".kannaka-corpus" / "runs"))
    ap.add_argument("--terminate", action="store_true", help="delete the instance + disk when done (default: stop)")
    ap.add_argument("--instance", default=None, help="reuse an existing (stopped) instance id")
    ap.add_argument("--dry-run", action="store_true", help="print the plan and the cost ceiling; provision nothing")
    ap.add_argument("train_args", nargs=argparse.REMAINDER, help="passed to train_lora.py after --")
    ap.add_argument("--job", default=None,
                    help="pod-side bash script run INSTEAD of train_lora.py (shipped next to it; --data is shipped "
                         "recursively; BASE is exported; the wait ends at out/JOB_DONE). E.g. code_arms.sh")
    ap.add_argument("--job-env", action="append", default=[], help="KEY=VALUE exported to the job (repeatable)")
    ap.add_argument("--ship", action="append", default=[], help="extra file shipped next to the trainer (repeatable)")
    a = ap.parse_args(argv)
    train_args = [x for x in a.train_args if x != "--"]

    rate = ALLOWED[a.profile]
    ceiling = rate * a.max_minutes / 60
    run_name = f"{a.profile}-{time.strftime('%Y%m%d-%H%M')}"
    log(f"plan: {a.profile} @ ${rate}/h, cutoff {a.max_minutes} min -> ceiling ${ceiling:.2f}; base={a.base}; run={run_name}")
    if a.dry_run:
        return 0
    if not a.allow_spend:
        log("refusing: --allow-spend not given (ADR-0057 spend gate)")
        return 2

    import warnings
    warnings.filterwarnings("ignore")
    from qbraid_core.services.compute import ComputeClient
    c = ComputeClient()
    bal = c.get_credits_balance()
    credits = float(bal.get("qbraidCredits") or 0)
    # the Standard plan carries included compute hours (400/month, 2026-09-05); a run
    # that fits in the remaining plan hours is allowed regardless of the credit balance
    plan_hours = 0.0
    try:
        ch = (c.get_usage(days=30).model_dump().get("compute_hours") or {})
        plan_hours = float(ch.get("remaining") or 0)
        log(f"plan compute hours: {ch.get('used')} used / {ch.get('quota')} quota, {plan_hours:.1f} remaining (renews {str(ch.get('renewalDate'))[:10]})")
    except Exception as e:
        log(f"plan hours unknown ({type(e).__name__})")
    log(f"credits before: {credits:.1f} (~ ${credits * CREDIT_USD:.2f})")
    # Measured 2026-09-05: BMA instances bill CREDITS, not the Standard plan's compute
    # hours (compute_hours.used stayed 0 across four GPU sessions). Until a session
    # type that draws plan hours is proven, the plan figure is informational and the
    # credit ceiling is the gate.
    if ceiling > credits * CREDIT_USD:
        log(f"refusing: ceiling ${ceiling:.2f} exceeds the credit balance ~ ${credits * CREDIT_USD:.2f} (BMA bills credits; plan hours do not apply)")
        return 4

    iid = None
    started = time.time()
    try:
        # 1-2. provision + cutoff first (inside try: the finally ALWAYS stops what we started)
        if a.instance:
            inst = c.get_bma_instance(a.instance)
            iid = inst.instance_id
            if "stopped" in str(inst.status).lower():
                c.start_bma_instance(iid)
        else:
            inst = c.provision_bma_instance(a.profile)
            iid = inst.instance_id
        log(f"instance {iid} status={inst.status}")
        try:
            c.update_bma_cutoff(iid, auto_stop_idle_minutes=a.idle_minutes, max_session_minutes=a.max_minutes)
        except Exception as e:
            # the SDK can fail to PARSE a successful cutoff response; trust the server, verify below
            log(f"cutoff call raised {type(e).__name__}; verifying")
        chk = c.get_bma_instance(iid)
        if int(chk.max_session_minutes or 0) != a.max_minutes:
            log(f"cutoff NOT applied (max_session_minutes={chk.max_session_minutes}); terminating rather than run ungated")
            c.terminate_bma_instance(iid)
            iid = None
            return 3
        log(f"cutoff verified: max {chk.max_session_minutes} min, idle {chk.auto_stop_idle_minutes} min")
        inst = c.wait_for_bma_instance(iid, timeout=a.provision_timeout)
        if "running" not in str(inst.status).lower():
            log(f"instance did not reach running (status={inst.status}, last_error={inst.last_error}); winding down")
            return 5
        log(f"instance running: {inst.url}")
        started = time.time()

        # 3. ssh + ship
        cfg = c.configure_ssh_for_instance(iid)
        alias = cfg.get("alias") or c.bma_ssh_alias(iid)
        log(f"ssh alias: {alias}")
        for _ in range(30):
            if ssh(alias, "echo up", check=False, capture=True).returncode == 0:
                break
            time.sleep(10)
        ssh(alias, f"mkdir -p {REMOTE}/data {REMOTE}/out")
        sh(["scp", "-q", "-o", "BatchMode=yes", str(HERE / "train_lora.py"), f"{alias}:{REMOTE}/"])
        for extra in list(a.ship) + ([a.job] if a.job else []):
            sh(["scp", "-q", "-o", "BatchMode=yes", str(extra), f"{alias}:{REMOTE}/"])
        if a.job:  # a job owns its data layout: ship the whole directory
            sh(["scp", "-q", "-r", "-o", "BatchMode=yes", str(Path(a.data)) + "/.", f"{alias}:{REMOTE}/data/"])
        else:
            for f in ("train.jsonl", "holdout.jsonl"):
                sh(["scp", "-q", "-o", "BatchMode=yes", str(Path(a.data) / f), f"{alias}:{REMOTE}/data/"])
        log("shipped trainer + data" + (f" + job {Path(a.job).name}" if a.job else ""))

        # 4. bootstrap + launch
        # With --merge --gguf the pod also merges + quantizes (train_lora.py). A 14B merge
        # needs ~3x the model in free disk, which debain2's 112 GB disk does not have (the
        # 2026-09-06 weekly died there); only the q4 (9 GB) comes back over scp.
        want_gguf = "--gguf" in train_args
        boot = (
            "cd " + REMOTE + " && { "
            # CUDA torch FIRST (some images ship torch+cpu: the A100 one did), then the rest
            "(python3 -c 'import torch,sys; sys.exit(0 if torch.cuda.is_available() else 1)' 2>/dev/null || "
            "python3 -m pip install -q --force-reinstall --no-deps torch --index-url https://download.pytorch.org/whl/cu128) && "
            "python3 -m pip install -q 'transformers>=4.45' 'peft>=0.13' 'datasets>=3' 'accelerate>=1' 'trl>=0.12' bitsandbytes sentencepiece protobuf"
            + (" && ([ -d ~/llama.cpp ] || git clone -q --depth 1 https://github.com/ggml-org/llama.cpp ~/llama.cpp) && "
               # NOT llama.cpp's requirements file: it pins a CPU torch and undoes the CUDA install above.
               # convert_hf_to_gguf.py puts its own gguf-py on sys.path; sentencepiece is already installed.
               "(command -v cmake >/dev/null || python3 -m pip install -q cmake) && "
               "(cd ~/llama.cpp && cmake -B build -DGGML_CUDA=OFF -DLLAMA_CURL=OFF >/dev/null && cmake --build build --target llama-quantize -j >/dev/null)"
               if want_gguf else "")
            + " && nvidia-smi --query-gpu=name,memory.total --format=csv,noheader && python3 -c 'import torch;print(\"torch\",torch.__version__,\"cuda\",torch.cuda.is_available())'; "
            "} > bootstrap.log 2>&1; rc=$?; tail -n 6 bootstrap.log; exit $rc"
        )
        r = ssh(alias, boot, check=False, capture=True, timeout=2400)
        tail_lines = r.stdout.strip().splitlines()[-6:]
        log("bootstrap rc=%d: %s" % (r.returncode, " | ".join(tail_lines)))
        if r.returncode != 0 or not any("cuda True" in l for l in tail_lines):
            log("bootstrap failed or no CUDA torch; refusing to train — winding down (bootstrap.log fetched)")
            dest = Path(a.fetch_to) / run_name
            dest.mkdir(parents=True, exist_ok=True)
            sh(["scp", "-q", "-o", "BatchMode=yes", f"{alias}:{REMOTE}/bootstrap.log", str(dest)], check=False)
            return 6
        if a.job:
            jname = Path(a.job).name
            env = " ".join(f"{k}={shlex.quote(v)}" for k, v in (x.split("=", 1) for x in a.job_env))
            # setsid -f: fully detached, so this ssh returns at once. `nohup … &` kept the
            # session open until the job exited (the whole 90-minute code-arms run, 2026-09-06).
            launch = (f"cd {REMOTE} && setsid -f env BASE={shlex.quote(a.base)} {env} bash {jname} "
                      f"> train.log 2>&1 < /dev/null; echo launched")
            proc_pat, done_file = jname, "out/JOB_DONE"
        else:
            targs = " ".join(shlex.quote(x) for x in train_args)
            launch = (f"cd {REMOTE} && setsid -f python3 train_lora.py --base {shlex.quote(a.base)} --data data --out out "
                      f"{targs} > train.log 2>&1 < /dev/null; echo launched")
            proc_pat, done_file = "train_lora.py", "out/train.manifest.json"
        # pgrep -f would match the poll's own `bash -c "... pgrep -f X ..."` command line and never
        # report __DEAD__; bracket the first character so the literal poll text cannot match itself.
        proc_re = "[" + proc_pat[0] + "]" + proc_pat[1:]
        pid = ssh(alias, launch, capture=True).stdout.strip()
        log(f"{'job' if a.job else 'training'} {pid}; tailing train.log (cutoff {a.max_minutes} min)")

        # tail until manifest or cutoff. The poll must exit 0 whenever ssh worked:
        # a bare `test -f` at the end returned 1 while the manifest was still
        # missing and the first A100 attempt misread that as a lost connection.
        last = 0
        lost = 0
        while True:
            poll = (f"tail -c +{last + 1} {REMOTE}/train.log | head -c 20000; "
                    f"if [ -f {REMOTE}/{done_file} ]; then echo __DONE__; "
                    f"elif ! pgrep -f {shlex.quote(proc_re)} >/dev/null; then echo __DEAD__; fi; true")
            r = ssh(alias, poll, check=False, capture=True, timeout=90)
            if r.returncode != 0:
                lost += 1
                log(f"ssh poll failed rc={r.returncode} ({lost}/5)")
                if lost >= 5:
                    log("ssh lost for good (cutoff hit?)")
                    break
                time.sleep(20)
                continue
            lost = 0
            chunk = r.stdout
            dead = chunk.endswith("__DEAD__\n")
            if dead:
                chunk = chunk[: -len("__DEAD__\n")]
            done = chunk.endswith("__DONE__\n")
            if done:
                chunk = chunk[: -len("__DONE__\n")]
            if chunk:
                sys.stdout.write(chunk)
                sys.stdout.flush()
                last += len(chunk.encode())
            if done:
                log(f"{done_file} present")
                break
            if dead:
                log("training process exited WITHOUT a manifest — see train.log above")
                break
            if time.time() - started > a.max_minutes * 60 + 120:
                log("past cutoff; stopping wait")
                break
            time.sleep(20)

        # 5. fetch
        dest = Path(a.fetch_to) / run_name
        dest.mkdir(parents=True, exist_ok=True)
        items = ("out/train.manifest.json", "out/samples.json", "train.log", "bootstrap.log", "out/adapter", "out/gguf")
        if a.job:  # a job's outputs live in named subdirs (code_arms.sh: real/, scr/, eval*/, arms.json)
            items = ("train.log", "bootstrap.log", "out/arms.json", "out/eval_stage1", "out/eval", "out/real", "out/scr")
        for item in items:
            sh(["scp", "-q", "-r", "-o", "BatchMode=yes", f"{alias}:{REMOTE}/{item}", str(dest)], check=False)
        log(f"fetched to {dest}: {sorted(p.name for p in dest.iterdir())}")
    finally:
        # 6. stop or terminate — always (also on a crash before/while provisioning)
        if iid is not None:
            _wind_down(c, a, iid, rate, started, credits)
    return 0


def _wind_down(c, a, iid, rate, started, credits):
    if True:
        try:
            if a.terminate:
                c.terminate_bma_instance(iid)
                log(f"instance {iid} TERMINATED")
            else:
                c.stop_bma_instance(iid)
                log(f"instance {iid} stopped (disk kept; --instance {iid} to reuse)")
        except Exception as e:
            log(f"STOP FAILED: {e} — stop it by hand: ComputeClient().stop_bma_instance('{iid}')")
        mins = (time.time() - started) / 60
        try:
            bal2 = c.get_credits_balance()
            c2 = float(bal2.get("qbraidCredits") or 0)
            log(f"session {mins:.1f} min ~ ${rate * mins / 60:.2f}; credits {credits:.1f} -> {c2:.1f} (spent {credits - c2:.1f} ~ ${(credits - c2) * CREDIT_USD:.2f})")
        except Exception:
            log(f"session {mins:.1f} min ~ ${rate * mins / 60:.2f}")


if __name__ == "__main__":
    raise SystemExit(main())
