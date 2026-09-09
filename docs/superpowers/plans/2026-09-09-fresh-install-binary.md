# Fresh install, update, uninstall — binary implementation plan (kannaka-memory)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the kannaka binary the two verbs the spec promises, `kannaka uninstall` and a `kannaka update` that refreshes the whole install, make this repo's old installers forward to the canonical one, and make the npm postinstall write the same receipt.

**Architecture:** Two new library modules. `install_receipt` owns the receipt file (`install.json`: types, load, atomic write, rotation) and is the only code that writes it from the binary. `uninstall` builds a plan from the receipt or, without one, from the spec's fallback table, with the identity check injected so tests never spawn a process; execution is separate from planning so `--dry-run` is the plan printed. `kannaka update` gains one call into a new `update_components` module that reads the receipt, fetches the signed constellation manifest, and refreshes every non-`kannaka` component with the existing safe swap. The forwarders and the npm change are small and self-contained.

**Tech Stack:** Rust 2021 (`clap` 4 builder API as in `src/cli.rs`, `serde`/`serde_json`, `ureq` 2, `sha2`, `ed25519-dalek` 2, `dirs` 5, `tempfile` 3 in dev-deps), POSIX sh, PowerShell 5.1, Node 18+ (`node --test`).

**Spec:** `kannaka-labs/kannaka-plugin` PR #19, `docs/superpowers/specs/2026-09-09-fresh-install-update-uninstall-design.md`. Sections referenced as §N. The installer half of the same spec is planned in kannaka-plugin (`docs/superpowers/plans/2026-09-09-fresh-install-installer.md`, PR #21); the receipt shape below is the one that plan writes.

## Global Constraints

- **Never delete by path, only by identity** (§4). A path from the receipt is still identity-checked before removal; a file that does not answer `--version` with a kannaka banner is declined and named. The two exceptions, because they are not executables, are the stale swap leftovers (`<name>.bak-*`, `<name>.old`, `<name>.new`) beside a recognised binary, and the receipt's own rotated copies.
- **The preserve set** (§5): `kannaka uninstall` without `--purge` never touches the data dir (except the receipt), rc blocks, `~/.kannaka-nats.env`, Windows user-env credentials, services or scheduled tasks. `--purge` removes the data dir, rc blocks, credentials and registrations, and **prints rather than runs** anything under `/etc/systemd`, a data dir outside `$HOME`, or a Windows scheduled task.
- **Exit non-zero if anything meant to be removed is still there** (§6).
- **The running binary is removed last** (§6): POSIX unlink; Windows rename to `kannaka.exe.bak-<pid>`.
- Receipt path: `KannakaConfig::data_dir().join("install.json")` (that helper honours `KANNAKA_DATA_DIR`). Rotated copies `install.json.1..3`.
- Receipt `credentials` entries come in two shapes and both must parse: `{"file": "..."}` (POSIX) and `{"kind": "user-env", "names": ["NATS_USER", "NATS_PASSWORD"]}` (Windows).
- Component names: `kannaka`, `kannaka-tui`, `kannaka-hdl`. Banner: first word of the first line of `--version`, second word starts with a digit.
- Exact names the purge message prints: systemd units `kannaka-attention.service`, `kannaka-eye.service`, `kannaka-hive-bridge.service`; Windows scheduled task `KannakaSeedBeacon`.
- No new network endpoints, no new secrets, no old-owner strings anywhere in new code.
- Every guard gets a mutation that is actually run (§9).
- Commit messages end with the trailer block the session uses (Co-Authored-By + Claude-Session).

## Rulings

1. **`kannaka` itself keeps updating from the latest GitHub release** (the existing, tested `self_update` path with the sha256 sidecar). The manifest governs the *siblings*. Pinning the engine itself to the manifest is a follow-up once kannaka-library publishes a manifest per release; today the manifest lags the release by design (it pinned 0.16.1 while 0.16.2 was current).
2. **`kannaka update` rewrites the receipt in place, without rotation.** Rotation is for installs; an update every few days would otherwise fill all three slots with near-identical receipts and lose the install that mattered.
3. **Without a receipt, `kannaka update` refreshes the siblings it finds beside itself** (today's behaviour, plus `kannaka-hdl`) and then writes a first receipt with `installer = "kannaka update@<VERSION>"`, so the next uninstall has something to read.
4. **`--purge` asks once when stdin is a terminal** (`This removes <data dir> including your identity key and memory. Type 'purge' to continue:`); `--yes` skips it; a non-tty stdin proceeds (scriptable, like every other verb). The spec does not mention a prompt; deleting a node key without one is the kind of thing a person should have to type.
5. **The rc-block removal rule**: delete the sentinel line (`# kannaka` or `# kannaka swarm credentials`) and every following line up to the next blank line or end of file. Both blocks the installer writes are exactly one sentinel plus one line; the rule tolerates a user having added a line inside the block.
6. **Manifest signature verification in Rust** uses `ed25519-dalek` (already a dependency) over the raw manifest bytes, with the public key parsed from the PEM `manifest.pub` (SubjectPublicKeyInfo: the key is the last 32 bytes of the DER). A signature that fails means the manifest is ignored and the siblings fall back to their repos' latest releases, exactly as `install.sh` does.
7. **The Windows self-rename is unit-tested on Windows, not in CI.** kannaka-memory CI is ubuntu-only and its sibling-checkout matrix is not worth duplicating for one rename. The test is `#[cfg(windows)]`, the implementer runs it on this Windows box, and the review package quotes the run.

## File map

| file | responsibility |
|---|---|
| `scripts/install.sh`, `scripts/install.ps1` | forwarders to the canonical installer; header stops advertising a hostname that does not exist |
| `scripts/tests/install-forwarder.test.sh` | runs the POSIX forwarder with a stubbed `curl`, asserts the canonical URL and argument pass-through |
| `src/install_receipt.rs` | `Receipt` and entry types, `receipt_path`, `load_from`, `write_atomic`, `write_rotated`, `installed_components` |
| `src/uninstall.rs` | `banner_component`, `Options`, `Plan`, `plan`, `execute`, `fallback_candidates`, `strip_rc_block`, `purge_print_only` |
| `src/update_components.rs` | `Manifest` loading + signature check, `refresh_all(agent)` |
| `src/cli.rs` | `uninstall` subcommand and `handle_uninstall`; `update` long_about updated |
| `src/config.rs` | `self_update` calls `update_components::refresh_all`; `windows_swap_binary` and `platform_triple` become `pub(crate)` |
| `src/lib.rs` | `pub mod install_receipt; pub mod uninstall; pub mod update_components;` |
| `packaging/npm/install.js`, `packaging/npm/receipt.js`, `packaging/npm/receipt.test.js` | npm postinstall writes the receipt |
| `.github/workflows/ci.yml` | run the forwarder test and the npm test |
| `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md` | 0.17.0 |

---

### Task 1: Forwarders (§2, rollout step 1)

**Files:**
- Rewrite: `scripts/install.sh`, `scripts/install.ps1`
- Create: `scripts/tests/install-forwarder.test.sh`
- Modify: `.github/workflows/ci.yml` (one step in the `check` job)

**Interfaces:**
- Produces: `scripts/install.sh` execs `sh -s -- "$@"` on the fetched canonical script; `scripts/install.ps1` invokes the canonical script block with `@args`. Environment (`KANNAKA_*`) passes through untouched.

- [ ] **Step 1: Write the failing test**

Create `scripts/tests/install-forwarder.test.sh`:

```bash
#!/usr/bin/env bash
# The old installer at scripts/install.sh is a FORWARDER: it fetches the
# canonical kannaka-plugin installer and execs it with the same arguments.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
FWD="$HERE/../install.sh"
fails=0
work="$(mktemp -d)"; mkdir -p "$work/bin"
# curl stub: record the URL, serve a script that records its own argv and env
cat > "$work/bin/curl" <<'EOF'
#!/bin/sh
url=""; for a in "$@"; do case "$a" in -*) ;; *) url="$a" ;; esac; done
echo "$url" >> "$STUB/url"
printf '#!/bin/sh\necho "argv: $*" >> "$STUB/argv"\necho "env: ${KANNAKA_MANIFEST:-unset}" >> "$STUB/argv"\n'
EOF
chmod +x "$work/bin/curl"
STUB="$work" KANNAKA_MANIFEST=from-env PATH="$work/bin:$PATH" sh "$FWD" --claim --brain local > "$work/out" 2>&1
rc=$?
[ "$rc" -eq 0 ] && echo "  ok   exit 0" || { echo "  FAIL exit $rc"; fails=$((fails+1)); }
grep -qx 'https://raw.githubusercontent.com/kannaka-labs/kannaka-plugin/master/install/install.sh' "$work/url" \
  && echo "  ok   fetches the canonical installer" || { echo "  FAIL wrong url: $(cat "$work/url")"; fails=$((fails+1)); }
grep -qx 'argv: --claim --brain local' "$work/argv" && echo "  ok   arguments pass through" || { echo "  FAIL argv: $(cat "$work/argv" 2>/dev/null)"; fails=$((fails+1)); }
grep -qx 'env: from-env' "$work/argv" && echo "  ok   environment passes through" || { echo "  FAIL env"; fails=$((fails+1)); }
grep -q 'kannaka-labs/kannaka-plugin' "$work/out" && echo "  ok   says where it went" || { echo "  FAIL silent forward"; fails=$((fails+1)); }
# a failed fetch must not exec an empty script and report success
cat > "$work/bin/curl" <<'EOF'
#!/bin/sh
exit 22
EOF
STUB="$work" PATH="$work/bin:$PATH" sh "$FWD" > "$work/out2" 2>&1; rc=$?
[ "$rc" -ne 0 ] && echo "  ok   failed fetch is a failure" || { echo "  FAIL failed fetch exited 0"; fails=$((fails+1)); }
rm -rf "$work"
[ "$fails" -eq 0 ] && echo "install-forwarder.test.sh: all cases passed" || { echo "install-forwarder.test.sh: $fails failed"; exit 1; }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `bash scripts/tests/install-forwarder.test.sh`
Expected: "fetches the canonical installer" FAILS (the old script downloads a release asset instead).

- [ ] **Step 3: Replace both scripts**

`scripts/install.sh` becomes, in full:

```sh
#!/bin/sh
# Kannaka installer — FORWARDER.
#
# This file used to be an installer of its own. The one installer for the
# constellation lives in kannaka-labs/kannaka-plugin (binary-first, signed
# manifest, receipt, fresh-install sweep). This script fetches it and runs it
# with the same arguments, so every one-liner ever published keeps working:
#
#   curl -fsSL https://raw.githubusercontent.com/kannaka-labs/kannaka-memory/master/scripts/install.sh | sh
#
# Arguments and KANNAKA_* environment pass straight through. See the canonical
# script for the flags (--claim, --brain, --keep-others, ...).
set -eu
CANONICAL="https://raw.githubusercontent.com/kannaka-labs/kannaka-plugin/master/install/install.sh"
printf '\033[36m▸\033[0m %s\n' "Forwarding to the constellation installer at kannaka-labs/kannaka-plugin…"
script=$(curl -fsSL --max-time 30 "$CANONICAL") || {
  printf '\033[33m!\033[0m %s\n' "Could not fetch $CANONICAL — check your connection and retry." >&2
  exit 1
}
[ -n "$script" ] || { printf '\033[33m!\033[0m %s\n' "The installer came back empty; refusing to run nothing." >&2; exit 1; }
printf '%s\n' "$script" | sh -s -- "$@"
```

`scripts/install.ps1` becomes, in full:

```powershell
# Kannaka installer — FORWARDER (Windows).
#
# The one installer lives in kannaka-labs/kannaka-plugin. This script fetches it
# and invokes it with the same parameters, so the old one-liner keeps working:
#
#   irm https://raw.githubusercontent.com/kannaka-labs/kannaka-memory/master/scripts/install.ps1 | iex
#
# To pass parameters, build a script block (iex cannot forward them):
#   & ([scriptblock]::Create((irm <this url>))) -Claim -Brain local
$ErrorActionPreference = "Stop"
$canonical = "https://raw.githubusercontent.com/kannaka-labs/kannaka-plugin/master/install/install.ps1"
Write-Host "▸ Forwarding to the constellation installer at kannaka-labs/kannaka-plugin…" -ForegroundColor Cyan
try { $script = (Invoke-WebRequest -Uri $canonical -UseBasicParsing -TimeoutSec 30).Content } catch {
  Write-Host "! Could not fetch $canonical — check your connection and retry. ($_)" -ForegroundColor Yellow; exit 1
}
if (-not $script) { Write-Host "! The installer came back empty; refusing to run nothing." -ForegroundColor Yellow; exit 1 }
& ([scriptblock]::Create($script)) @args
```

- [ ] **Step 4: Run the test and the syntax checks**

Run: `bash scripts/tests/install-forwarder.test.sh && sh -n scripts/install.sh`
Expected: all `ok`, "all cases passed".

- [ ] **Step 5: CI and commit**

In `.github/workflows/ci.yml`, in the `check` job after the `Test` step, add:

```yaml
      - name: install forwarder — scripts/install.sh hands off to kannaka-plugin
        working-directory: kannaka-memory
        run: bash scripts/tests/install-forwarder.test.sh
```

Also update `README.md` and `docs/QUICKSTART.md` wherever they show `scripts/install.sh`: keep the one-liner (it still works) and add the sentence "This forwards to the constellation installer in kannaka-labs/kannaka-plugin." — `grep -n 'scripts/install' README.md docs/QUICKSTART.md` lists the spots.

```bash
git add scripts/install.sh scripts/install.ps1 scripts/tests/install-forwarder.test.sh .github/workflows/ci.yml README.md docs/QUICKSTART.md
git commit -m "scripts/install: forward to the canonical kannaka-plugin installer"
```

---

### Task 2: The receipt module

**Files:**
- Create: `src/install_receipt.rs`
- Modify: `src/lib.rs` (add `pub mod install_receipt;` after `pub mod config;` at line 88)

**Interfaces:**
- Produces:

```rust
pub const SCHEMA: u32 = 1;
pub const FILE_NAME: &str = "install.json";
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)] pub struct Receipt { … }
#[derive(…)] pub struct FileEntry { pub path: PathBuf, pub sha256: String, pub component: String, pub version: String }
#[derive(…)] pub struct RcEdit { pub file: PathBuf, pub sentinel: String }
#[derive(…)] pub struct ConfigEdit { pub file: PathBuf, pub sections: Vec<String> }
#[derive(…)] #[serde(untagged)] pub enum Credential { File { file: PathBuf }, UserEnv { kind: String, names: Vec<String> } }
#[derive(…)] pub struct Registration { pub kind: String, pub name: String }
#[derive(…)] pub struct Removed { pub path: String, pub component: String, pub reason: String }
pub fn receipt_path() -> PathBuf                                   // KannakaConfig::data_dir().join(FILE_NAME)
pub fn load_from(path: &Path) -> Result<Option<Receipt>, String>   // Ok(None) when absent
pub fn write_atomic(path: &Path, r: &Receipt) -> Result<(), String>   // tmp + rename, no rotation
pub fn write_rotated(path: &Path, r: &Receipt) -> Result<(), String>  // .2->.3, .1->.2, cur->.1, sets r.previous
pub fn sha256_hex(bytes: &[u8]) -> String
```

- [ ] **Step 1: Write the failing tests** (at the bottom of the new file, inside `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Receipt {
        Receipt {
            schema: SCHEMA,
            installed_at: "2026-09-09T04:00:00Z".into(),
            installer: "kannaka-labs/kannaka-plugin/install/install.sh@2".into(),
            manifest: "latest".into(),
            platform: "linux-x86_64".into(),
            files: vec![FileEntry { path: "/h/.local/bin/kannaka".into(), sha256: "ab".repeat(32), component: "kannaka".into(), version: "0.17.0".into() }],
            rc_edits: vec![RcEdit { file: "/h/.bashrc".into(), sentinel: "# kannaka".into() }],
            config_edits: vec![],
            credentials: vec![Credential::File { file: "/h/.kannaka-nats.env".into() }],
            registrations: vec![Registration { kind: "claude-plugin".into(), name: "kannaka@kannaka".into() }],
            removed: vec![],
            previous: vec![],
        }
    }

    #[test]
    fn round_trips_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        write_atomic(&p, &sample()).unwrap();
        assert_eq!(load_from(&p).unwrap(), Some(sample()));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "temp file left behind");
    }

    #[test]
    fn absent_receipt_is_none_not_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_from(&dir.path().join(FILE_NAME)).unwrap(), None);
    }

    #[test]
    fn both_credential_shapes_parse() {
        let posix = r#"{"file": "/h/.kannaka-nats.env"}"#;
        let win = r#"{"kind": "user-env", "names": ["NATS_USER", "NATS_PASSWORD"]}"#;
        assert!(matches!(serde_json::from_str::<Credential>(posix).unwrap(), Credential::File { .. }));
        assert!(matches!(serde_json::from_str::<Credential>(win).unwrap(), Credential::UserEnv { .. }));
    }

    #[test]
    fn the_installers_receipt_parses_with_removed_and_previous() {
        // Shape written by kannaka-plugin's install.sh (its plan, Task 3).
        let text = r#"{
  "schema": 1, "installed_at": "2026-09-09T04:00:00Z",
  "installer": "kannaka-labs/kannaka-plugin/install/install.sh@2", "manifest": "library@2026-09-08T00:00:00Z",
  "platform": "linux-x86_64",
  "files": [{"path": "/h/.local/bin/kannaka", "sha256": "0000000000000000000000000000000000000000000000000000000000000000", "component": "kannaka", "version": "0.16.2"}],
  "rc_edits": [], "config_edits": [], "credentials": [], "registrations": [],
  "removed": [{"path": "/h/.cargo/bin/kannaka", "component": "kannaka", "reason": "cargo install era"}],
  "previous": ["install.json.1"]
}"#;
        let r: Receipt = serde_json::from_str(text).unwrap();
        assert_eq!(r.removed[0].reason, "cargo install era");
        assert_eq!(r.previous, vec!["install.json.1"]);
    }

    #[test]
    fn missing_optional_lists_default_to_empty() {
        // An older receipt without "removed"/"previous" must still load.
        let text = r#"{"schema":1,"installed_at":"t","installer":"i","manifest":"m","platform":"p","files":[],"rc_edits":[],"config_edits":[],"credentials":[],"registrations":[]}"#;
        let r: Receipt = serde_json::from_str(text).unwrap();
        assert!(r.removed.is_empty() && r.previous.is_empty());
    }

    #[test]
    fn write_rotated_keeps_three_and_lists_them() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        for i in 0..5 {
            let mut r = sample();
            r.installed_at = format!("t{i}");
            write_rotated(&p, &r).unwrap();
        }
        let cur = load_from(&p).unwrap().unwrap();
        assert_eq!(cur.installed_at, "t4");
        assert_eq!(cur.previous, vec!["install.json.1", "install.json.2", "install.json.3"]);
        assert_eq!(load_from(&dir.path().join("install.json.1")).unwrap().unwrap().installed_at, "t3");
        assert_eq!(load_from(&dir.path().join("install.json.3")).unwrap().unwrap().installed_at, "t1");
        assert!(!dir.path().join("install.json.4").exists());
    }

    #[test]
    fn write_atomic_does_not_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        write_atomic(&p, &sample()).unwrap();
        write_atomic(&p, &sample()).unwrap();
        assert!(!dir.path().join("install.json.1").exists());
    }

    #[test]
    fn sha256_hex_matches_sha256sum() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib install_receipt::`
Expected: compile error (module does not exist).

- [ ] **Step 3: Implement**

`src/install_receipt.rs`:

```rust
//! The install receipt: what the installer wrote on this machine, so that
//! `kannaka uninstall` can reverse exactly that and `kannaka update` can
//! refresh exactly that. Written by the installers in kannaka-plugin, by the
//! npm postinstall, and by this crate (`update_components`, and `uninstall`
//! when it removes its own receipt). One shape, one file, one writer at a time.
//!
//! Spec: kannaka-plugin/docs/superpowers/specs/2026-09-09-fresh-install-update-uninstall-design.md §3.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCHEMA: u32 = 1;
pub const FILE_NAME: &str = "install.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Receipt {
    pub schema: u32,
    pub installed_at: String,
    pub installer: String,
    pub manifest: String,
    pub platform: String,
    pub files: Vec<FileEntry>,
    pub rc_edits: Vec<RcEdit>,
    pub config_edits: Vec<ConfigEdit>,
    pub credentials: Vec<Credential>,
    pub registrations: Vec<Registration>,
    #[serde(default)]
    pub removed: Vec<Removed>,
    #[serde(default)]
    pub previous: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub sha256: String,
    pub component: String,
    pub version: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RcEdit {
    pub file: PathBuf,
    pub sentinel: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ConfigEdit {
    pub file: PathBuf,
    pub sections: Vec<String>,
}

/// POSIX installs record the credentials FILE; Windows installs record the
/// user-environment variable NAMES (there is no file). Both must load.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Credential {
    File { file: PathBuf },
    UserEnv { kind: String, names: Vec<String> },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Registration {
    pub kind: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Removed {
    pub path: String,
    pub component: String,
    pub reason: String,
}

/// `<data dir>/install.json`, honouring `KANNAKA_DATA_DIR`.
pub fn receipt_path() -> PathBuf {
    crate::config::KannakaConfig::data_dir().join(FILE_NAME)
}

/// `Ok(None)` when there is no receipt (an install older than the receipt).
pub fn load_from(path: &Path) -> Result<Option<Receipt>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{} is not a valid install receipt: {e}", path.display()))
}

/// Write `r` to `path` atomically: a temp file in the same directory, then
/// a rename. No rotation — this is what `kannaka update` uses.
pub fn write_atomic(path: &Path, r: &Receipt) -> Result<(), String> {
    let dir = path.parent().ok_or("receipt path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!("{FILE_NAME}.tmp.{}", std::process::id()));
    let text = serde_json::to_string_pretty(r).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot move receipt into place: {e}")
    })
}

/// Rotate `install.json` -> `.1` -> `.2` -> `.3` (the oldest is dropped),
/// set `r.previous` to the rotated names that exist, then write atomically.
/// This is what an INSTALL does; see write_atomic for an update.
pub fn write_rotated(path: &Path, r: &Receipt) -> Result<(), String> {
    let s = path.to_string_lossy().to_string();
    let n = |k: u32| PathBuf::from(format!("{s}.{k}"));
    if n(2).exists() { std::fs::rename(n(2), n(3)).map_err(|e| e.to_string())?; }
    if n(1).exists() { std::fs::rename(n(1), n(2)).map_err(|e| e.to_string())?; }
    if path.exists() { std::fs::rename(path, n(1)).map_err(|e| e.to_string())?; }
    let mut r = r.clone();
    r.previous = (1..=3).filter(|k| n(*k).exists()).map(|k| format!("{FILE_NAME}.{k}")).collect();
    write_atomic(path, &r)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
```

Add `pub mod install_receipt;` to `src/lib.rs` after line 88.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib install_receipt::`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add src/install_receipt.rs src/lib.rs
git commit -m "install_receipt: the install receipt as a type, loaded and written atomically"
```

---

### Task 3: The uninstall planner and executor

**Files:**
- Create: `src/uninstall.rs`
- Modify: `src/lib.rs` (`pub mod uninstall;` after `install_receipt`)

**Interfaces:**
- Consumes: `install_receipt::{Receipt, Credential, load_from}`
- Produces:

```rust
pub type Banner = dyn Fn(&Path) -> Option<String>;          // returns the component name, or None
pub fn banner_component(path: &Path) -> Option<String>;     // the real one: spawns `--version`, 5 s cap
pub struct Options { pub purge: bool, pub dry_run: bool }
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub unlink: Vec<PathBuf>,               // files proven ours (receipt files, fallback finds, stale leftovers)
    pub declined: Vec<(PathBuf, String)>,   // exists but did not identify — never touched, always reported
    pub rc_blocks: Vec<(PathBuf, String)>,  // (file, sentinel) — purge only
    pub remove_dirs: Vec<PathBuf>,          // the data dir — purge only
    pub remove_files: Vec<PathBuf>,         // credential files, the receipt and its rotations
    pub user_env: Vec<String>,              // Windows credential names — purge only
    pub registrations: Vec<install_receipt::Registration>, // purge only
    pub print_only: Vec<String>,            // system units / scheduled tasks: commands printed, never run
    pub self_path: Option<PathBuf>,         // the running binary, removed last
}
pub fn plan(receipt: Option<&Receipt>, opts: &Options, home: &Path, data_dir: &Path, path_env: &str, current_exe: &Path, banner: &Banner) -> Plan;
pub fn fallback_candidates(home: &Path, path_env: &str) -> Vec<PathBuf>;   // §4 table, POSIX and Windows rows
pub fn strip_rc_block(text: &str, sentinel: &str) -> String;
pub fn purge_print_only(home: &Path, data_dir: &Path) -> Vec<String>;
pub struct Report { pub removed: Vec<PathBuf>, pub still_present: Vec<PathBuf>, pub declined: Vec<(PathBuf, String)> }
pub fn execute(plan: &Plan, run: &dyn Fn(&str, &[&str]) -> bool) -> Report;   // `run` executes a registration command; injected
pub fn render(plan: &Plan) -> String;    // what --dry-run prints
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_receipt::*;
    use std::collections::HashMap;

    struct Fx { dir: tempfile::TempDir, banners: HashMap<PathBuf, String> }
    impl Fx {
        fn new() -> Self { Fx { dir: tempfile::tempdir().unwrap(), banners: HashMap::new() } }
        fn home(&self) -> PathBuf { self.dir.path().join("home") }
        fn data(&self) -> PathBuf { self.home().join(".kannaka") }
        fn ours(&mut self, rel: &str, comp: &str, ver: &str) -> PathBuf {
            let p = self.home().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"bin").unwrap();
            self.banners.insert(p.clone(), format!("{comp} {ver} (stub)"));
            p
        }
        fn impostor(&mut self, rel: &str) -> PathBuf {
            let p = self.home().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"nope").unwrap();
            self.banners.insert(p.clone(), "definitely-not-kannaka 9.9".into());
            p
        }
        fn banner(&self) -> Box<Banner> {
            let b = self.banners.clone();
            Box::new(move |p: &Path| b.get(p).and_then(|l| l.split_whitespace().next()).map(|s| s.to_string())
                .filter(|s| ["kannaka", "kannaka-tui", "kannaka-hdl"].contains(&s.as_str())))
        }
        fn receipt(&self, files: &[PathBuf]) -> Receipt {
            Receipt {
                schema: SCHEMA, installed_at: "t".into(), installer: "i".into(), manifest: "m".into(), platform: "p".into(),
                files: files.iter().map(|p| FileEntry { path: p.clone(), sha256: "0".repeat(64), component: p.file_name().unwrap().to_string_lossy().trim_end_matches(".exe").to_string(), version: "1".into() }).collect(),
                rc_edits: vec![RcEdit { file: self.home().join(".bashrc"), sentinel: "# kannaka".into() }],
                config_edits: vec![],
                credentials: vec![Credential::File { file: self.home().join(".kannaka-nats.env") }],
                registrations: vec![Registration { kind: "claude-plugin".into(), name: "kannaka@kannaka".into() }],
                removed: vec![], previous: vec![],
            }
        }
    }
    fn no_purge() -> Options { Options { purge: false, dry_run: false } }
    fn purge() -> Options { Options { purge: true, dry_run: false } }
    fn noop_run(_: &str, _: &[&str]) -> bool { true }

    #[test]
    fn receipt_plan_is_exactly_the_receipt_and_keeps_the_preserve_set() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "0.17.0");
        let t = fx.ours(".local/bin/kannaka-tui", "kannaka-tui", "0.5.9");
        std::fs::create_dir_all(fx.data()).unwrap();
        std::fs::write(fx.data().join("node_key.ed25519"), b"IDENTITY").unwrap();
        std::fs::write(fx.home().join(".bashrc"), "x\n\n# kannaka\nexport PATH=1\n").unwrap();
        std::fs::write(fx.home().join(".kannaka-nats.env"), "u").unwrap();
        let r = fx.receipt(&[k.clone(), t.clone()]);
        let p = plan(Some(&r), &no_purge(), &fx.home(), &fx.data(), "", &k, &*fx.banner());
        assert_eq!(p.unlink, vec![t.clone()]);            // kannaka itself is self_path, not unlink
        assert_eq!(p.self_path, Some(k.clone()));
        assert!(p.rc_blocks.is_empty() && p.remove_dirs.is_empty() && p.user_env.is_empty() && p.registrations.is_empty(), "{p:?}");
        assert_eq!(p.remove_files, vec![fx.data().join("install.json")]);   // only the receipt leaves the data dir
        let rep = execute(&p, &noop_run);
        assert!(!t.exists() && !k.exists());
        assert!(rep.still_present.is_empty());
        assert_eq!(std::fs::read(fx.data().join("node_key.ed25519")).unwrap(), b"IDENTITY");
        assert_eq!(std::fs::read_to_string(fx.home().join(".bashrc")).unwrap(), "x\n\n# kannaka\nexport PATH=1\n");
        assert!(fx.home().join(".kannaka-nats.env").exists());
    }

    #[test]
    fn a_receipt_path_that_is_not_kannaka_is_declined_not_deleted() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        let x = fx.impostor(".local/bin/kannaka-tui");
        let r = fx.receipt(&[k.clone(), x.clone()]);
        let p = plan(Some(&r), &no_purge(), &fx.home(), &fx.data(), "", &k, &*fx.banner());
        assert!(p.unlink.is_empty());
        assert_eq!(p.declined, vec![(x.clone(), "not kannaka".to_string())]);
        execute(&p, &noop_run);
        assert!(x.exists());
    }

    #[test]
    fn purge_widens_to_data_dir_rc_blocks_credentials_and_registrations() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        std::fs::create_dir_all(fx.data()).unwrap();
        std::fs::write(fx.data().join("kannaka.hrm"), b"HRM").unwrap();
        std::fs::write(fx.home().join(".bashrc"), "x\n\n# kannaka\nexport PATH=1\n\n# other\n").unwrap();
        std::fs::write(fx.home().join(".kannaka-nats.env"), "u").unwrap();
        let r = fx.receipt(&[k.clone()]);
        let p = plan(Some(&r), &purge(), &fx.home(), &fx.data(), "", &k, &*fx.banner());
        assert_eq!(p.remove_dirs, vec![fx.data()]);
        assert_eq!(p.rc_blocks, vec![(fx.home().join(".bashrc"), "# kannaka".to_string())]);
        assert!(p.remove_files.contains(&fx.home().join(".kannaka-nats.env")));
        assert_eq!(p.registrations.len(), 1);
        // execute takes `&dyn Fn`, so collect through a RefCell
        let ran = std::cell::RefCell::new(Vec::<String>::new());
        let run = |cmd: &str, args: &[&str]| { ran.borrow_mut().push(format!("{cmd} {}", args.join(" "))); true };
        let rep = execute(&p, &run);
        assert!(!fx.data().exists());
        assert_eq!(std::fs::read_to_string(fx.home().join(".bashrc")).unwrap(), "x\n\n# other\n");
        assert!(!fx.home().join(".kannaka-nats.env").exists());
        assert_eq!(ran.borrow().as_slice(), ["claude plugin uninstall kannaka@kannaka"]);
        assert!(rep.still_present.is_empty());
    }

    #[test]
    fn purge_prints_rather_than_runs_system_commands() {
        let fx = Fx::new();
        let outside = fx.dir.path().join("srv-data");          // a data dir outside $HOME
        let lines = purge_print_only(&fx.home(), &outside);
        assert!(lines.iter().any(|l| l.contains("kannaka-attention.service")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("kannaka-eye.service")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("kannaka-hive-bridge.service")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("KannakaSeedBeacon")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains(&outside.display().to_string())), "{lines:?}");
    }

    #[test]
    fn no_receipt_falls_back_to_the_table_with_the_same_identity_check() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        let c = fx.ours(".cargo/bin/kannaka", "kannaka", "0.9");
        let s = fx.ours("shadow/kannaka-tui", "kannaka-tui", "0.4");
        let x = fx.impostor(".cargo/bin/kannaka-hdl");
        std::fs::write(fx.home().join(".local/bin/kannaka.bak-7"), b"stale").unwrap();
        let path_env = format!("{}:{}", fx.home().join("shadow").display(), fx.home().join(".local/bin").display());
        let p = plan(None, &no_purge(), &fx.home(), &fx.data(), &path_env, &k, &*fx.banner());
        let mut unlink = p.unlink.clone(); unlink.sort();
        let mut want = vec![c.clone(), s.clone(), fx.home().join(".local/bin/kannaka.bak-7")]; want.sort();
        assert_eq!(unlink, want);
        assert_eq!(p.self_path, Some(k));
        assert_eq!(p.declined, vec![(x, "not kannaka".to_string())]);
    }

    #[test]
    fn exit_is_nonzero_when_something_survives() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        let t = fx.ours(".local/bin/kannaka-tui", "kannaka-tui", "1");
        let r = fx.receipt(&[k.clone(), t.clone()]);
        let p = plan(Some(&r), &no_purge(), &fx.home(), &fx.data(), "", &k, &*fx.banner());
        // make the tui undeletable: turn it into a non-empty directory
        std::fs::remove_file(&t).unwrap(); std::fs::create_dir(&t).unwrap(); std::fs::write(t.join("x"), b"").unwrap();
        let rep = execute(&p, &noop_run);
        assert_eq!(rep.still_present, vec![t]);
    }

    #[test]
    fn strip_rc_block_removes_sentinel_through_next_blank_line() {
        assert_eq!(strip_rc_block("a\n\n# kannaka\nl1\nl2\n\nb\n", "# kannaka"), "a\n\nb\n");
        assert_eq!(strip_rc_block("a\n# kannaka\nl1", "# kannaka"), "a\n");
        assert_eq!(strip_rc_block("a\n# kannaka swarm credentials\nl1\n", "# kannaka"), "a\n# kannaka swarm credentials\nl1\n", "sentinel must match the whole line");
    }

    #[test]
    fn mutation_guard_removed_deletes_the_impostor() {
        // Proves the fixture reaches the guard: a banner that accepts everything
        // must put the impostor in `unlink`. If this test ever passes with the
        // real guard, the fixture is broken, not the guard.
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        let x = fx.impostor(".cargo/bin/kannaka-hdl");
        let accept_all: Box<Banner> = Box::new(|_| Some("kannaka".to_string()));
        let p = plan(None, &no_purge(), &fx.home(), &fx.data(), "", &k, &*accept_all);
        assert!(p.unlink.contains(&x), "mutant did not reach the impostor: {p:?}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_self_removal_parks_the_running_exe() {
        // Run on a Windows box (Ruling 7). Copies the test binary into a temp
        // dir, holds it open (a stand-in for a running image: Windows allows a
        // running exe to be RENAMED but not deleted, which is why self_path is
        // always renamed on Windows), and asks execute() to remove it as
        // self_path: it must end up as .exe.bak-<pid>.
        let dir = tempfile::tempdir().unwrap();
        let me = dir.path().join("kannaka.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &me).unwrap();
        let lock = std::fs::File::open(&me).unwrap();
        let p = Plan { self_path: Some(me.clone()), ..Default::default() };
        let rep = execute(&p, &noop_run);
        drop(lock);
        assert!(!me.exists(), "exe still at its path");
        let parked: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("kannaka.exe.bak-")).collect();
        assert_eq!(parked.len(), 1);
        assert!(rep.still_present.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib uninstall::`
Expected: compile error (module does not exist).

- [ ] **Step 3: Implement**

`src/uninstall.rs`:

```rust
//! `kannaka uninstall`: read the receipt and reverse it. Without a receipt,
//! fall back to the spec's table of places a kannaka can live. Either way
//! a file is removed only when its --version banner says it is one of ours.
//! Planning is separate from execution so --dry-run is the plan, printed.
//!
//! Spec: kannaka-plugin/docs/superpowers/specs/2026-09-09-fresh-install-update-uninstall-design.md §4-§6.

use crate::install_receipt::{self as receipt, Credential, Receipt, Registration};
use std::path::{Path, PathBuf};

pub const COMPONENTS: [&str; 3] = ["kannaka", "kannaka-tui", "kannaka-hdl"];
pub const SYSTEMD_UNITS: [&str; 3] = ["kannaka-attention.service", "kannaka-eye.service", "kannaka-hive-bridge.service"];
pub const WINDOWS_TASK: &str = "KannakaSeedBeacon";
const RC_FILES: [&str; 4] = [".bashrc", ".zshrc", ".bash_profile", ".profile"];
const RC_SENTINELS: [&str; 2] = ["# kannaka", "# kannaka swarm credentials"];

pub type Banner = dyn Fn(&Path) -> Option<String>;

/// The real identity check: run `<path> --version`, wait at most five
/// seconds, take the first word of the first line. Anything else is None.
pub fn banner_component(path: &Path) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    if !path.is_file() { return None; }
    let mut child = Command::new(path).arg("--version")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
            _ => { let _ = child.kill(); let _ = child.wait(); return None; }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let line = out.lines().next()?;
    let mut words = line.split_whitespace();
    let name = words.next()?;
    let ver = words.next()?;
    if COMPONENTS.contains(&name) && ver.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        Some(name.to_string())
    } else {
        None
    }
}

pub struct Options { pub purge: bool, pub dry_run: bool }

#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub unlink: Vec<PathBuf>,
    pub declined: Vec<(PathBuf, String)>,
    pub rc_blocks: Vec<(PathBuf, String)>,
    pub remove_dirs: Vec<PathBuf>,
    pub remove_files: Vec<PathBuf>,
    pub user_env: Vec<String>,
    pub registrations: Vec<Registration>,
    pub print_only: Vec<String>,
    pub self_path: Option<PathBuf>,
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Stale swap leftovers beside a binary that IS ours: <name>.bak-*, .old, .new.
fn stale_beside(bin: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (bin.parent(), bin.file_name().map(|n| n.to_string_lossy().to_string())) else { return vec![] };
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    entries.flatten().map(|e| e.path()).filter(|p| {
        let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        n.starts_with(&format!("{name}.bak-")) || n == format!("{name}.old") || n == format!("{name}.new")
    }).collect()
}

/// Consider one candidate: ours -> unlink (or self), impostor -> declined.
fn consider(path: &Path, current_exe: &Path, banner: &Banner, out: &mut Plan) {
    if !path.exists() { return; }
    match banner(path) {
        Some(_) => {
            if same_file(path, current_exe) { out.self_path = Some(path.to_path_buf()); }
            else if !out.unlink.contains(&path.to_path_buf()) { out.unlink.push(path.to_path_buf()); }
            for s in stale_beside(path) { if !out.unlink.contains(&s) { out.unlink.push(s); } }
        }
        None => out.declined.push((path.to_path_buf(), "not kannaka".to_string())),
    }
}

/// §4's table, both platforms' rows, minus the package managers (those are
/// the installer's business: an uninstall reverses what THIS binary's
/// receipt says, and a brew/npm install has its own uninstall).
pub fn fallback_candidates(home: &Path, path_env: &str) -> Vec<PathBuf> {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let mut dirs: Vec<PathBuf> = vec![home.join(".local").join("bin"), home.join(".cargo").join("bin")];
    if cfg!(windows) {
        if let Ok(lap) = std::env::var("LOCALAPPDATA") { dirs.push(PathBuf::from(lap).join("Programs").join("kannaka")); }
    }
    let sep = if cfg!(windows) { ';' } else { ':' };
    for d in path_env.split(sep).filter(|d| !d.is_empty()) { dirs.push(PathBuf::from(d)); }
    let mut out = Vec::new();
    for d in dirs {
        for c in COMPONENTS {
            let p = d.join(format!("{c}{ext}"));
            if p.exists() && !out.contains(&p) { out.push(p); }
        }
    }
    out
}

pub fn plan(receipt_: Option<&Receipt>, opts: &Options, home: &Path, data_dir: &Path, path_env: &str, current_exe: &Path, banner: &Banner) -> Plan {
    let mut out = Plan::default();
    match receipt_ {
        Some(r) => for f in &r.files { consider(&f.path, current_exe, banner, &mut out); },
        None => for p in fallback_candidates(home, path_env) { consider(&p, current_exe, banner, &mut out); },
    }
    // The receipt and its rotations always go: they describe an install that
    // no longer exists once this runs.
    let rp = data_dir.join(receipt::FILE_NAME);
    for k in ["", ".1", ".2", ".3"] {
        let p = PathBuf::from(format!("{}{k}", rp.display()));
        if p.exists() || k.is_empty() { out.remove_files.push(p); }
    }
    if opts.purge {
        out.remove_dirs.push(data_dir.to_path_buf());
        match receipt_ {
            Some(r) => {
                for e in &r.rc_edits { if e.file.exists() { out.rc_blocks.push((e.file.clone(), e.sentinel.clone())); } }
                for c in &r.credentials {
                    match c {
                        Credential::File { file } => if file.exists() { out.remove_files.push(file.clone()); },
                        Credential::UserEnv { names, .. } => out.user_env.extend(names.iter().cloned()),
                    }
                }
                out.registrations = r.registrations.clone();
            }
            None => {
                for rc in RC_FILES {
                    let f = home.join(rc);
                    let Ok(text) = std::fs::read_to_string(&f) else { continue };
                    for s in RC_SENTINELS { if text.lines().any(|l| l.trim_end() == s) { out.rc_blocks.push((f.clone(), s.to_string())); } }
                }
                let creds = home.join(".kannaka-nats.env");
                if creds.exists() { out.remove_files.push(creds); }
                if cfg!(windows) { out.user_env = vec!["NATS_USER".into(), "NATS_PASSWORD".into()]; }
                out.registrations = vec![Registration { kind: "claude-plugin".into(), name: "kannaka@kannaka".into() }];
            }
        }
        out.print_only = purge_print_only(home, data_dir);
    }
    out
}

/// Things --purge will not do for you, with the exact command each one takes.
pub fn purge_print_only(home: &Path, data_dir: &Path) -> Vec<String> {
    let mut v = Vec::new();
    for u in SYSTEMD_UNITS {
        if Path::new("/etc/systemd/system").join(u).exists() {
            v.push(format!("sudo systemctl disable --now {u} && sudo rm /etc/systemd/system/{u}"));
        }
    }
    if cfg!(windows) {
        v.push(format!("schtasks /Delete /TN {WINDOWS_TASK} /F     (only if the seed beacon task was installed)"));
    }
    if !data_dir.starts_with(home) {
        v.push(format!("rm -rf {}     (KANNAKA_DATA_DIR is outside your home; not touched)", data_dir.display()));
    }
    v
}

/// Remove the sentinel line and every following line up to the next blank
/// line (or EOF). The sentinel must match a whole line.
pub fn strip_rc_block(text: &str, sentinel: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if !skipping && bare == sentinel { skipping = true; continue; }
        if skipping {
            // the blank line that ends the block goes with it: the installer
            // wrote a leading blank line before the sentinel, so this keeps
            // the file's spacing as it was before the install
            if bare.trim().is_empty() { skipping = false; }
            continue;
        }
        out.push_str(line);
    }
    out
}

pub struct Report { pub removed: Vec<PathBuf>, pub still_present: Vec<PathBuf>, pub declined: Vec<(PathBuf, String)> }

fn remove_path(p: &Path) -> bool {
    if p.is_dir() { std::fs::remove_dir_all(p).is_ok() } else { std::fs::remove_file(p).is_ok() }
}

/// Carry out the plan. `run` executes an external command (claude) and
/// returns whether it succeeded; injected so tests never spawn anything.
pub fn execute(plan: &Plan, run: &dyn Fn(&str, &[&str]) -> bool) -> Report {
    let mut removed = Vec::new();
    let mut still = Vec::new();
    for p in &plan.unlink {
        if remove_path(p) && !p.exists() { removed.push(p.clone()); } else if p.exists() { still.push(p.clone()); }
    }
    for (f, s) in &plan.rc_blocks {
        if let Ok(text) = std::fs::read_to_string(f) {
            let new = strip_rc_block(&text, s);
            if new != text { let _ = std::fs::write(f, new); }
        }
    }
    for p in &plan.remove_files {
        if !p.exists() { continue; }
        if remove_path(p) && !p.exists() { removed.push(p.clone()); } else { still.push(p.clone()); }
    }
    for name in &plan.user_env {
        #[cfg(windows)]
        { let _ = run("reg", &["delete", "HKCU\\Environment", "/v", name, "/f"]); }
        #[cfg(not(windows))]
        { let _ = name; }
    }
    for r in &plan.registrations {
        match r.kind.as_str() {
            "claude-plugin" => { let _ = run("claude", &["plugin", "uninstall", &r.name]); }
            "claude-marketplace" => { let _ = run("claude", &["plugin", "marketplace", "remove", "kannaka"]); }
            _ => {}
        }
    }
    for d in &plan.remove_dirs {
        if !d.exists() { continue; }
        if remove_path(d) && !d.exists() { removed.push(d.clone()); } else { still.push(d.clone()); }
    }
    // Self, last.
    if let Some(me) = &plan.self_path {
        #[cfg(windows)]
        {
            let bak = me.with_extension(format!("exe.bak-{}", std::process::id()));
            if std::fs::rename(me, &bak).is_ok() { removed.push(me.clone()); } else { still.push(me.clone()); }
        }
        #[cfg(not(windows))]
        {
            if std::fs::remove_file(me).is_ok() { removed.push(me.clone()); } else { still.push(me.clone()); }
        }
    }
    Report { removed, still_present: still, declined: plan.declined.clone() }
}

pub fn render(plan: &Plan) -> String {
    let mut s = String::new();
    for p in &plan.unlink { s.push_str(&format!("remove   {}\n", p.display())); }
    for p in &plan.remove_files { s.push_str(&format!("remove   {}\n", p.display())); }
    for (f, sent) in &plan.rc_blocks { s.push_str(&format!("edit     {}  (drop the '{sent}' block)\n", f.display())); }
    for n in &plan.user_env { s.push_str(&format!("unset    user environment {n}\n")); }
    for r in &plan.registrations { s.push_str(&format!("run      claude plugin uninstall {}\n", r.name)); }
    for d in &plan.remove_dirs { s.push_str(&format!("remove   {}  (everything: identity, memory, snapshots)\n", d.display())); }
    if let Some(me) = &plan.self_path { s.push_str(&format!("remove   {}  (this binary, last)\n", me.display())); }
    for (p, why) in &plan.declined { s.push_str(&format!("keep     {}  ({why})\n", p.display())); }
    if !plan.print_only.is_empty() {
        s.push_str("not done for you (run yourself if you mean it):\n");
        for l in &plan.print_only { s.push_str(&format!("    {l}\n")); }
    }
    s
}
```

Add `pub mod uninstall;` to `src/lib.rs` after `install_receipt`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib uninstall::` (on this Windows box the `windows_self_removal_parks_the_running_exe` case runs too; on Linux it is skipped)
Expected: all pass. Quote the Windows run in the task report.

- [ ] **Step 5: Commit**

```bash
git add src/uninstall.rs src/lib.rs
git commit -m "uninstall: plan from the receipt (or the fallback table) by identity, execute, report survivors"
```

---

### Task 4: `kannaka uninstall` on the command line

**Files:**
- Modify: `src/cli.rs` — subcommand after the `update` block (line 134), dispatch after `if name == "update"` (line 594), `handle_uninstall` after `handle_update`
- Modify: `src/bin/kannaka.rs` line ~1047 (the comment listing what is NOT in the fast path: add `uninstall`)

**Interfaces:**
- Consumes: `uninstall::{plan, execute, render, banner_component, Options}`, `install_receipt::{receipt_path, load_from}`
- Produces: `kannaka uninstall [--purge] [--dry-run] [--yes]`; exit 0 clean, 1 when something meant to be removed survived, 2 on a receipt that cannot be read, 3 when the purge confirmation was refused.

- [ ] **Step 1: Write the failing test** (in `src/cli.rs`'s existing `#[cfg(test)]` module, or create one at the bottom if absent)

```rust
    #[test]
    fn uninstall_parses_its_three_flags() {
        let m = build_cli().get_matches_from(["kannaka", "uninstall", "--purge", "--dry-run", "--yes"]);
        let (name, sub) = m.subcommand().unwrap();
        assert_eq!(name, "uninstall");
        assert!(sub.get_flag("purge") && sub.get_flag("dry-run") && sub.get_flag("yes"));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib cli::tests::uninstall_parses_its_three_flags`
Expected: FAIL (clap: unrecognized subcommand).

- [ ] **Step 3: Implement**

After the `update` subcommand's closing `)` (line 134) add:

```rust
        .subcommand(
            Command::new("uninstall")
                .about("Remove this install (reads the install receipt; data and shell state kept unless --purge)")
                .long_about(
                    "Reverse the install recorded in <data dir>/install.json: the binaries it\n\
                     wrote (each checked to be kannaka before removal), then this binary last.\n\
                     Without a receipt, looks in the places a kannaka can live and removes\n\
                     only what identifies itself as one.\n\n\
                     Flags:\n  \
                     --purge    also remove the data dir (identity key, memory, snapshots),\n             \
                                the shell rc blocks, the swarm credentials and the Claude\n             \
                                plugin registration. System units and scheduled tasks are\n             \
                                printed, never run.\n  \
                     --dry-run  print the plan, change nothing\n  \
                     --yes      skip the --purge confirmation",
                )
                .arg(Arg::new("purge").long("purge").action(ArgAction::SetTrue).help("Also remove the data dir, rc blocks, credentials and registrations"))
                .arg(Arg::new("dry-run").long("dry-run").action(ArgAction::SetTrue).help("Print the plan and change nothing"))
                .arg(Arg::new("yes").long("yes").action(ArgAction::SetTrue).help("Do not ask before --purge")),
        )
```

After the `if name == "update" { … }` block (line 594) add:

```rust
    if name == "uninstall" {
        return handle_uninstall(
            sub_matches.get_flag("purge"),
            sub_matches.get_flag("dry-run"),
            sub_matches.get_flag("yes"),
        );
    }
```

After `handle_update` add:

```rust
/// `kannaka uninstall`. Exit codes: 0 clean, 1 something meant to be
/// removed is still there, 2 unreadable receipt, 3 purge not confirmed.
fn handle_uninstall(purge: bool, dry_run: bool, yes: bool) -> Dispatch {
    use crate::{install_receipt, uninstall};
    use std::io::IsTerminal;
    let data_dir = crate::config::KannakaConfig::data_dir();
    let rpath = install_receipt::receipt_path();
    let receipt = match install_receipt::load_from(&rpath) {
        Ok(r) => r,
        Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
    };
    if receipt.is_none() {
        eprintln!("No install receipt at {} — this install predates receipts; using the fallback table.", rpath.display());
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let current_exe = std::env::current_exe().unwrap_or_default();
    let path_env = std::env::var("PATH").unwrap_or_default();
    let opts = uninstall::Options { purge, dry_run };
    let plan = uninstall::plan(receipt.as_ref(), &opts, &home, &data_dir, &path_env, &current_exe, &uninstall::banner_component);
    print!("{}", uninstall::render(&plan));
    if dry_run { println!("(dry run — nothing changed)"); return Dispatch::Handled; }
    if purge && !yes && std::io::stdin().is_terminal() {
        eprint!("This removes {} including your identity key and memory. Type 'purge' to continue: ", data_dir.display());
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        if line.trim() != "purge" { eprintln!("not confirmed; nothing changed"); std::process::exit(3); }
    }
    let run = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).status().map(|s| s.success()).unwrap_or(false);
    let report = uninstall::execute(&plan, &run);
    for p in &report.removed { println!("removed  {}", p.display()); }
    for (p, why) in &report.declined { println!("kept     {}  ({why})", p.display()); }
    if !report.still_present.is_empty() {
        for p in &report.still_present { eprintln!("STILL PRESENT  {}", p.display()); }
        eprintln!("uninstall incomplete: {} item(s) could not be removed", report.still_present.len());
        std::process::exit(1);
    }
    println!("kannaka uninstalled.{}", if purge { "" } else { " Your data in the data dir and your shell rc were kept (use --purge to remove them)." });
    Dispatch::Handled
}
```

`Dispatch::Handled` is returned only for the paths that print; every exit path uses `std::process::exit` as `handle_update` does. In `src/bin/kannaka.rs` extend the comment at line ~1047 so it reads `completions`, `update` and `uninstall` are intentionally NOT here.

- [ ] **Step 4: Run the tests and a real dry run**

Run: `cargo test --lib cli:: && cargo run -q -- uninstall --dry-run`
Expected: tests pass; the dry run prints a plan for this box (no receipt yet → fallback table) and ends with `(dry run — nothing changed)`; nothing on disk changed (`kannaka --version` still works).

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/bin/kannaka.rs
git commit -m "cli: kannaka uninstall [--purge] [--dry-run] [--yes]"
```

---

### Task 5: `kannaka update` refreshes every component from the manifest

**Files:**
- Create: `src/update_components.rs`
- Modify: `src/config.rs` — `fn windows_swap_binary` → `pub(crate) fn` (line 935), `fn platform_triple` → `pub(crate) fn` (line 1567); in `self_update` replace both `update_sibling_tui(&agent, &body, tag, &current_exe, remote_version);` calls (lines ~1041 and ~1155) with `crate::update_components::refresh_all(&agent, &current_exe);`
- Modify: `src/lib.rs` (`pub mod update_components;`), `src/cli.rs` `update` long_about (mention kannaka-hdl and the receipt)

**Interfaces:**
- Consumes: `install_receipt::{Receipt, FileEntry, load_from, write_atomic, receipt_path, sha256_hex}`, `config::{windows_swap_binary, platform_triple, fetch_and_verify_sha256, VerifyError}`, `uninstall::banner_component`
- Produces:

```rust
pub const MANIFEST_URL: &str = "https://ninja-portal.com/constellation.json";
pub const MANIFEST_FALLBACK: &str = "https://github.com/kannaka-labs/kannaka-library/releases/download/library/constellation.json";
pub const MANIFEST_PUB_URL: &str = "https://github.com/kannaka-labs/kannaka-library/releases/download/library/manifest.pub";
pub struct Pin { pub version: String, pub url: String, pub sha256: String }
pub struct Manifest { pub generated: String, pub signed: bool, pins: HashMap<String, Pin> }   // keyed by component id, for THIS platform target
impl Manifest { pub fn parse(json: &[u8], target: &str) -> Result<Manifest, String>; pub fn pin(&self, component: &str) -> Option<&Pin>; }
pub fn verify_signature(manifest_bytes: &[u8], sig_b64: &str, pub_pem: &str) -> Result<(), String>;
pub fn ed25519_pub_from_pem(pem: &str) -> Result<[u8; 32], String>;
pub fn refresh_all(agent: &ureq::Agent, current_exe: &Path);   // never fails the caller; prints what it did
pub fn swap_in(target: &Path, bytes: &[u8]) -> Result<(), String>;   // unix: .new + rename; windows: windows_swap_binary
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"schema":"kannaka-constellation/1","generated":"2026-09-08T00:00:00Z","components":[
      {"id":"kannaka","kind":"binary","release":{"version":"v0.16.1"},"assets":[
        {"name":"kannaka-linux-x86_64","url":"https://example/k","sha256":"aa","target":"linux-x86_64"}]},
      {"id":"kannaka-tui","kind":"binary","release":{"version":"v0.5.9"},"assets":[
        {"name":"kannaka-tui-linux-x86_64","url":"https://example/t","sha256":"bb","target":"linux-x86_64"},
        {"name":"kannaka-tui-windows-x86_64.exe","url":"https://example/tw","sha256":"cc","target":"windows-x86_64"}]},
      {"id":"kannaka-brain","kind":"model"}]}"#;

    #[test]
    fn parses_pins_for_one_target_only() {
        let m = Manifest::parse(SAMPLE.as_bytes(), "linux-x86_64").unwrap();
        assert_eq!(m.generated, "2026-09-08T00:00:00Z");
        assert_eq!(m.pin("kannaka-tui").unwrap().version, "0.5.9");
        assert_eq!(m.pin("kannaka-tui").unwrap().sha256, "bb");
        assert!(m.pin("kannaka-hdl").is_none());
        assert!(m.pin("kannaka-brain").is_none(), "a model has no binary pin");
        let w = Manifest::parse(SAMPLE.as_bytes(), "windows-x86_64").unwrap();
        assert_eq!(w.pin("kannaka-tui").unwrap().url, "https://example/tw");
    }

    #[test]
    fn rejects_a_document_that_is_not_the_manifest() {
        assert!(Manifest::parse(br#"{"schema":"something-else"}"#, "linux-x86_64").is_err());
        assert!(Manifest::parse(b"<html>", "linux-x86_64").is_err());
    }

    #[test]
    fn signature_round_trip_with_a_fresh_key() {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        // SubjectPublicKeyInfo DER for Ed25519 is a fixed 12-byte prefix + the 32-byte key.
        let mut der = vec![0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
        der.extend_from_slice(vk.as_bytes());
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n", b64(&der));
        assert_eq!(ed25519_pub_from_pem(&pem).unwrap(), *vk.as_bytes());
        let sig = sk.sign(SAMPLE.as_bytes());
        assert!(verify_signature(SAMPLE.as_bytes(), &b64(&sig.to_bytes()), &pem).is_ok());
        let mut tampered = SAMPLE.as_bytes().to_vec(); tampered[10] ^= 1;
        assert!(verify_signature(&tampered, &b64(&sig.to_bytes()), &pem).is_err());
    }

    fn b64(b: &[u8]) -> String {
        // minimal base64 for the test; the implementation has its own decoder
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in b.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            s.push(T[(n >> 18 & 63) as usize] as char); s.push(T[(n >> 12 & 63) as usize] as char);
            s.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
            s.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
        }
        s
    }

    #[test]
    fn swap_in_replaces_the_file_and_leaves_no_new_behind() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join(if cfg!(windows) { "kannaka-tui.exe" } else { "kannaka-tui" });
        std::fs::write(&t, b"old").unwrap();
        swap_in(&t, b"new").unwrap();
        assert_eq!(std::fs::read(&t).unwrap(), b"new");
        assert!(!dir.path().join("kannaka-tui.new").exists());
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; assert_ne!(std::fs::metadata(&t).unwrap().permissions().mode() & 0o111, 0); }
    }

    #[test]
    fn receipt_entry_is_rewritten_after_a_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("kannaka-tui");
        std::fs::write(&t, b"old").unwrap();
        let mut r = crate::install_receipt::Receipt { files: vec![crate::install_receipt::FileEntry { path: t.clone(), sha256: "x".into(), component: "kannaka-tui".into(), version: "0.5.0".into() }], ..Default::default() };
        record_refresh(&mut r, &t, b"new", "0.5.9");
        assert_eq!(r.files[0].version, "0.5.9");
        assert_eq!(r.files[0].sha256, crate::install_receipt::sha256_hex(b"new"));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib update_components::`
Expected: compile error.

- [ ] **Step 3: Implement**

`src/update_components.rs`:

```rust
//! `kannaka update` beyond the engine: refresh every component the receipt
//! lists (kannaka-tui, kannaka-hdl) to the version the signed constellation
//! manifest pins, with the same safe swap and sha256 check the engine uses,
//! then rewrite the receipt. The engine itself still follows its latest
//! release (Ruling 1 of the binary plan); this module is for the siblings.
//!
//! Spec: kannaka-plugin/docs/superpowers/specs/2026-09-09-fresh-install-update-uninstall-design.md §7.

use crate::install_receipt::{self as receipt, FileEntry, Receipt};
use std::collections::HashMap;
use std::path::Path;

pub const MANIFEST_URL: &str = "https://ninja-portal.com/constellation.json";
pub const MANIFEST_FALLBACK: &str = "https://github.com/kannaka-labs/kannaka-library/releases/download/library/constellation.json";
pub const MANIFEST_PUB_URL: &str = "https://github.com/kannaka-labs/kannaka-library/releases/download/library/manifest.pub";

pub struct Pin { pub version: String, pub url: String, pub sha256: String }

pub struct Manifest { pub generated: String, pub signed: bool, pins: HashMap<String, Pin> }

impl Manifest {
    /// Parse constellation.json, keeping only the binary pins for `target`
    /// (e.g. "linux-x86_64"). A document without the schema line is refused:
    /// a wrong file must not quietly de-pin every component.
    pub fn parse(json: &[u8], target: &str) -> Result<Manifest, String> {
        let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| format!("manifest is not JSON: {e}"))?;
        if v["schema"].as_str() != Some("kannaka-constellation/1") { return Err("not a constellation manifest".into()); }
        let mut pins = HashMap::new();
        for c in v["components"].as_array().cloned().unwrap_or_default() {
            let (Some(id), Some(version)) = (c["id"].as_str(), c["release"]["version"].as_str()) else { continue };
            let Some(asset) = c["assets"].as_array().and_then(|a| a.iter().find(|x| x["target"].as_str() == Some(target))) else { continue };
            let (Some(url), Some(sha)) = (asset["url"].as_str(), asset["sha256"].as_str()) else { continue };
            pins.insert(id.to_string(), Pin { version: version.trim_start_matches('v').to_string(), url: url.to_string(), sha256: sha.to_lowercase() });
        }
        Ok(Manifest { generated: v["generated"].as_str().unwrap_or("unknown").to_string(), signed: false, pins })
    }
    pub fn pin(&self, component: &str) -> Option<&Pin> { self.pins.get(component) }
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buf = 0u32; let mut bits = 0;
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' { break; }
        let v = match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'+' => 62, b'/' => 63, _ => return Err("bad base64".into()) } as u32;
        buf = buf << 6 | v; bits += 6;
        if bits >= 8 { bits -= 8; out.push((buf >> bits) as u8 & 0xff); }
    }
    Ok(out)
}

/// The key is the last 32 bytes of the SubjectPublicKeyInfo DER inside the PEM.
pub fn ed25519_pub_from_pem(pem: &str) -> Result<[u8; 32], String> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = b64_decode(&body)?;
    if der.len() < 32 { return Err("public key too short".into()); }
    let mut k = [0u8; 32];
    k.copy_from_slice(&der[der.len() - 32..]);
    Ok(k)
}

pub fn verify_signature(manifest_bytes: &[u8], sig_b64: &str, pub_pem: &str) -> Result<(), String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let vk = VerifyingKey::from_bytes(&ed25519_pub_from_pem(pub_pem)?).map_err(|e| e.to_string())?;
    let sig_bytes = b64_decode(sig_b64)?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|e| e.to_string())?;
    vk.verify(manifest_bytes, &sig).map_err(|_| "manifest signature did not verify".to_string())
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let resp = agent.get(url).set("User-Agent", "kannaka-update").call().map_err(|e| format!("{url}: {e}"))?;
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Load the manifest, signature-checked when the signature and key can be
/// fetched; a manifest whose signature FAILS is refused (None). Missing
/// signature material means unsigned, which is still used (the per-asset
/// sha256 is the real guarantee, as in install.sh).
fn load_manifest(agent: &ureq::Agent, target: &str) -> Option<Manifest> {
    let (bytes, from) = match fetch(agent, MANIFEST_URL) { Ok(b) => (b, MANIFEST_URL), Err(_) => (fetch(agent, MANIFEST_FALLBACK).ok()?, MANIFEST_FALLBACK) };
    let mut m = match Manifest::parse(&bytes, target) { Ok(m) => m, Err(e) => { eprintln!("Note: {e}; refreshing siblings from their latest releases instead."); return None; } };
    if let (Ok(sig), Ok(pubk)) = (fetch(agent, &format!("{from}.sig")), fetch(agent, MANIFEST_PUB_URL)) {
        match verify_signature(&bytes, &String::from_utf8_lossy(&sig), &String::from_utf8_lossy(&pubk)) {
            Ok(()) => m.signed = true,
            Err(e) => { eprintln!("Warning: {e} — ignoring the manifest."); return None; }
        }
    }
    eprintln!("Manifest loaded ({}, generated {}).", if m.signed { "signed" } else { "unsigned" }, m.generated);
    Some(m)
}

/// Replace `target` with `bytes` safely: a `.new` beside it, then rename
/// (Unix) or the Windows swap that parks the running image.
pub fn swap_in(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = target.with_extension("new");
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, target).map_err(|e| { let _ = std::fs::remove_file(&tmp); format!("replace {}: {e}", target.display()) })?;
    }
    #[cfg(windows)]
    {
        crate::config::windows_swap_binary(target, &tmp).map_err(|e| { let _ = std::fs::remove_file(&tmp); e })?;
    }
    Ok(())
}

pub fn record_refresh(r: &mut Receipt, path: &Path, bytes: &[u8], version: &str) {
    if let Some(f) = r.files.iter_mut().find(|f| f.path == path) {
        f.sha256 = receipt::sha256_hex(bytes);
        f.version = version.to_string();
    }
}

/// Refresh every sibling the receipt lists. Without a receipt, refresh the
/// siblings found beside `current_exe` and write a first receipt. Never
/// fails the caller: every problem is a printed note.
pub fn refresh_all(agent: &ureq::Agent, current_exe: &Path) {
    let (os, arch, ext) = crate::config::platform_triple();
    let target = format!("{os}-{arch}");
    let rpath = receipt::receipt_path();
    let mut r = match receipt::load_from(&rpath) {
        Ok(Some(r)) => r,
        Ok(None) => synthesize_receipt(current_exe, ext, &target),
        Err(e) => { eprintln!("Note: {e}; not refreshing siblings."); return; }
    };
    let manifest = load_manifest(agent, &target);
    let mut changed = false;
    for entry in r.files.clone() {
        if entry.component == "kannaka" { continue; }
        let Some(m) = &manifest else {
            eprintln!("Note: no manifest — {} left as is (re-run when ninja-portal.com is reachable).", entry.component);
            continue;
        };
        let Some(pin) = m.pin(&entry.component) else { eprintln!("Note: manifest has no {} for {target}.", entry.component); continue };
        let local = crate::uninstall::banner_component(&entry.path).and_then(|_| local_version(&entry.path));
        if local.as_deref() == Some(pin.version.as_str()) { eprintln!("{} already at v{}.", entry.component, pin.version); continue; }
        eprintln!("Downloading {} v{} (pinned)...", entry.component, pin.version);
        let bytes = match fetch(agent, &pin.url) { Ok(b) => b, Err(e) => { eprintln!("Note: {e}"); continue } };
        let got = receipt::sha256_hex(&bytes);
        if got != pin.sha256 { eprintln!("Note: {} sha256 mismatch against the manifest (want {} got {got}) — skipped.", entry.component, pin.sha256); continue; }
        match swap_in(&entry.path, &bytes) {
            Ok(()) => { record_refresh(&mut r, &entry.path, &bytes, &pin.version); changed = true; eprintln!("{} updated to v{}.", entry.component, pin.version); }
            Err(e) => eprintln!("Note: {} not replaced: {e}", entry.component),
        }
    }
    // The engine's own entry: self_update already swapped it; record what is there now.
    if let Some(me) = r.files.iter_mut().find(|f| f.component == "kannaka") {
        if let Ok(bytes) = std::fs::read(&me.path) { let h = receipt::sha256_hex(&bytes); if h != me.sha256 { me.sha256 = h; me.version = crate::config::VERSION.to_string(); changed = true; } }
    }
    if changed || r.installer.starts_with("kannaka update@") {
        if let Err(e) = receipt::write_atomic(&rpath, &r) { eprintln!("Note: receipt not updated: {e}"); }
    }
}

fn local_version(path: &Path) -> Option<String> {
    let out = std::process::Command::new(path).arg("--version").output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next()?.split_whitespace().nth(1).map(|s| s.to_string())
}

/// An install older than receipts: the engine plus whichever siblings sit
/// beside it. Written after the first refresh so the next uninstall has
/// something to read.
fn synthesize_receipt(current_exe: &Path, ext: &str, target: &str) -> Receipt {
    let dir = current_exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut files = Vec::new();
    for c in crate::uninstall::COMPONENTS {
        let p = dir.join(format!("{c}{ext}"));
        if !p.exists() { continue; }
        let bytes = std::fs::read(&p).unwrap_or_default();
        files.push(FileEntry { path: p.clone(), sha256: receipt::sha256_hex(&bytes), component: c.to_string(), version: local_version(&p).unwrap_or_else(|| "unknown".into()) });
    }
    Receipt {
        schema: receipt::SCHEMA,
        installed_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        installer: format!("kannaka update@{}", crate::config::VERSION),
        manifest: "latest".into(),
        platform: target.to_string(),
        files, ..Default::default()
    }
}
```

In `src/config.rs`: make `windows_swap_binary` and `platform_triple` `pub(crate)`; replace the two `update_sibling_tui(...)` calls in `self_update` with `crate::update_components::refresh_all(&agent, &current_exe);` (in the "already up to date" branch `current_exe` comes from the existing `if let Ok(current_exe) = std::env::current_exe()`); leave `update_sibling_tui` in place for `bootstrap_install_tui`, which still uses it, and add `#[allow(dead_code)]` if the compiler complains. Add `pub mod update_components;` to `src/lib.rs`. In `src/cli.rs` the `update` long_about's "Also updates the kannaka-tui sibling binary…" sentence becomes "Then refreshes every component the install receipt lists (kannaka-tui, kannaka-hdl) to the version the signed constellation manifest pins, and rewrites the receipt."

- [ ] **Step 4: Run the tests and one real update**

Run: `cargo test --lib update_components:: && cargo test --lib && cargo run -q -- update`
Expected: unit tests pass; the real `update` on this box prints `Manifest loaded (signed, generated …)` (or `unsigned` if LibreSSL-style PEM parsing surprises us — that is a finding, report it), refreshes or reports `already at` for kannaka-tui, and writes a receipt at `~/.kannaka/install.json` with `installer: "kannaka update@0.17.0"` because this box predates receipts.

- [ ] **Step 5: Commit**

```bash
git add src/update_components.rs src/config.rs src/lib.rs src/cli.rs
git commit -m "update: refresh every receipt component from the signed manifest, rewrite the receipt"
```

---

### Task 6: The npm postinstall writes the receipt

**Files:**
- Create: `packaging/npm/receipt.js`, `packaging/npm/receipt.test.js`
- Modify: `packaging/npm/install.js` (after line 101 `console.log(\`kannaka: installed ${dest}\`)`), `packaging/npm/package.json` (`"files"` must include `receipt.js`; check with `grep -n '"files"' packaging/npm/package.json`)
- Modify: `.github/workflows/ci.yml` (node test step)

**Interfaces:**
- Produces: `writeReceipt({ dataDir, files, installer, platform })` → rotates `install.json` three deep and writes atomically; returns the receipt path.

- [ ] **Step 1: Write the failing test**

`packaging/npm/receipt.test.js`:

```js
"use strict";
const test = require("node:test");
const assert = require("node:assert");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { writeReceipt, receiptDir } = require("./receipt");

test("writes the receipt shape the installers write", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "kr-"));
  const p = writeReceipt({ dataDir: dir, installer: "npm:kannaka@0.17.0", platform: "linux-x86_64",
    files: [{ path: "/n/bin/kannaka-bin", sha256: "ab".repeat(32), component: "kannaka", version: "0.17.0" }] });
  const r = JSON.parse(fs.readFileSync(p, "utf8"));
  assert.strictEqual(r.schema, 1);
  assert.deepStrictEqual(Object.keys(r), ["schema", "installed_at", "installer", "manifest", "platform", "files", "rc_edits", "config_edits", "credentials", "registrations", "removed", "previous"]);
  assert.strictEqual(r.files[0].component, "kannaka");
  assert.deepStrictEqual(r.previous, []);
  assert.ok(!fs.existsSync(path.join(dir, "install.json.tmp")));
});

test("rotates three deep", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "kr-"));
  for (let i = 0; i < 5; i++) writeReceipt({ dataDir: dir, installer: `i${i}`, platform: "p", files: [] });
  const r = JSON.parse(fs.readFileSync(path.join(dir, "install.json"), "utf8"));
  assert.strictEqual(r.installer, "i4");
  assert.deepStrictEqual(r.previous, ["install.json.1", "install.json.2", "install.json.3"]);
  assert.strictEqual(JSON.parse(fs.readFileSync(path.join(dir, "install.json.3"), "utf8")).installer, "i1");
  assert.ok(!fs.existsSync(path.join(dir, "install.json.4")));
});

test("receiptDir honours KANNAKA_DATA_DIR", () => {
  process.env.KANNAKA_DATA_DIR = "/x/y";
  assert.strictEqual(receiptDir(), "/x/y");
  delete process.env.KANNAKA_DATA_DIR;
  assert.strictEqual(receiptDir(), path.join(os.homedir(), ".kannaka"));
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `node --test packaging/npm/receipt.test.js`
Expected: "Cannot find module './receipt'".

- [ ] **Step 3: Implement**

`packaging/npm/receipt.js`:

```js
"use strict";
/**
 * The install receipt (~/.kannaka/install.json): what this install wrote, so
 * `kannaka uninstall` can reverse exactly it. Same shape as the shell and
 * PowerShell installers write; rotated three deep; written last, atomically.
 */
const fs = require("fs");
const os = require("os");
const path = require("path");

function receiptDir() {
  return process.env.KANNAKA_DATA_DIR || path.join(os.homedir(), ".kannaka");
}

function writeReceipt({ dataDir, installer, platform, files, manifest = "latest", removed = [] }) {
  fs.mkdirSync(dataDir, { recursive: true });
  const p = path.join(dataDir, "install.json");
  // rotate: .2 -> .3, .1 -> .2, current -> .1
  for (const [from, to] of [[`${p}.2`, `${p}.3`], [`${p}.1`, `${p}.2`], [p, `${p}.1`]]) {
    if (fs.existsSync(from)) fs.renameSync(from, to);
  }
  const previous = [1, 2, 3].filter((n) => fs.existsSync(`${p}.${n}`)).map((n) => `install.json.${n}`);
  const doc = {
    schema: 1,
    installed_at: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    installer, manifest, platform, files,
    rc_edits: [], config_edits: [], credentials: [], registrations: [],
    removed, previous,
  };
  const tmp = `${p}.tmp.${process.pid}`;
  fs.writeFileSync(tmp, JSON.stringify(doc, null, 2) + "\n");
  fs.renameSync(tmp, p);
  return p;
}

module.exports = { writeReceipt, receiptDir };
```

In `packaging/npm/install.js`, add near the top `const { writeReceipt, receiptDir } = require("./receipt");` and after line 101 (`console.log(\`kannaka: installed ${dest}\`)`):

```js
  // The receipt: this is an install like any other, so `kannaka uninstall`
  // can find and reverse it. Written last.
  const receipt = writeReceipt({
    dataDir: receiptDir(),
    installer: `npm:kannaka@${VERSION}`,
    platform: `${os}-${arch}`,
    files: [{ path: dest, sha256: crypto.createHash("sha256").update(bin).digest("hex"), component: "kannaka", version: VERSION }],
  });
  console.log(`kannaka: receipt ${receipt}`);
```

Also fix the failure hint at line 108 of `install.js`: replace `curl -sSf https://install.ninja-portal.com/kannaka | sh` with `curl -fsSL https://raw.githubusercontent.com/kannaka-labs/kannaka-plugin/master/install/install.sh | sh` (the hostname does not exist; §8 is Nick's decision and until then the hint must be a working address).

Ensure `packaging/npm/package.json`'s `"files"` array lists `receipt.js` beside `install.js`.

- [ ] **Step 4: Run the tests**

Run: `node --test packaging/npm/receipt.test.js && node --check packaging/npm/install.js`
Expected: 3 passed; check silent.

- [ ] **Step 5: CI and commit**

In `.github/workflows/ci.yml` `check` job, after the forwarder step:

```yaml
      - uses: actions/setup-node@v4
        with:
          node-version: "20"
      - name: npm postinstall — receipt writer
        working-directory: kannaka-memory
        run: node --test packaging/npm/receipt.test.js && node --check packaging/npm/install.js
```

```bash
git add packaging/npm/receipt.js packaging/npm/receipt.test.js packaging/npm/install.js packaging/npm/package.json .github/workflows/ci.yml
git commit -m "npm: postinstall writes the install receipt; failure hint points at a real address"
```

---

### Task 7: Release 0.17.0

**Files:**
- Modify: `Cargo.toml` line 3 (`version = "0.17.0"`), `Cargo.lock` (the crate's own entry: `cargo update -p kannaka-memory --precise 0.17.0` is wrong for a path crate; run `cargo build` once and commit the lockfile diff, which only touches this package's version), `CHANGELOG.md` under `## [Unreleased]`

- [ ] **Step 1: CHANGELOG**

Insert after `## [Unreleased]`:

```markdown
## [0.17.0] — 2026-09-10

### Added — an install can be reversed

- `kannaka uninstall [--purge] [--dry-run] [--yes]`. Reads the install receipt
  (`<data dir>/install.json`, written by the constellation installer) and
  reverses exactly it, removing each binary only after its `--version` banner
  proves it is kannaka, and this binary last. Without a receipt it looks in the
  places a kannaka can live and applies the same identity check. Data, shell rc
  blocks and credentials are kept unless `--purge`; system units and scheduled
  tasks are printed, never run. Exits non-zero if anything meant to be removed
  is still there.

### Changed

- `kannaka update` now refreshes every component the receipt lists —
  `kannaka-tui` and `kannaka-hdl`, not only the TUI — to the version the signed
  constellation manifest pins, verifying each download against the manifest's
  sha256, and rewrites the receipt. An install older than receipts gets one.
- `scripts/install.sh` and `scripts/install.ps1` are forwarders to the one
  installer in `kannaka-labs/kannaka-plugin`; every published one-liner keeps
  working. The npm postinstall writes the same receipt.
```

- [ ] **Step 2: Version bump and lockfile**

```bash
sed -i 's/^version = "0.16.2"$/version = "0.17.0"/' Cargo.toml
cargo build -q 2>&1 | tail -2
git diff --stat Cargo.lock      # expect: one file, the kannaka-memory entry only
```

- [ ] **Step 3: Full test run**

Run: `cargo test --lib --bins --test nats_contract_conformance --test attention_gravity_e2e && bash scripts/tests/install-forwarder.test.sh && node --test packaging/npm/receipt.test.js`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "release: 0.17.0 — kannaka uninstall, whole-install update, forwarders, npm receipt"
```

The tag `v0.17.0` is pushed by the controller after the PR merges and CI is green (release.yml → publish-channels.yml). Docker and brew still depend on Nick's two pending items (ghcr package visibility, `TAP_PUSH_TOKEN` rotation).

---

## Self-review

**Spec coverage.** §2 forwarders → Task 1; npm as an install like any other → Task 6. §3 receipt → Task 2 (both credential shapes; `removed`/`previous` default). §4 fallback table with the identity check → Task 3 (`fallback_candidates`, `consider`); the brew/npm/marketplace rows are the installer's (plugin plan) — an uninstall reverses the receipt, and a brew or npm install has its own uninstall, stated in the module comment. §5 preserve set → Task 3's first test asserts it byte-for-byte for the identity key and rc; `--purge` widening and the print-only list with the exact unit and task names → Task 3. §6 all three forms, exit non-zero on survivors, self last with the Windows rename → Tasks 3, 4. §7 whole-install update, manifest, receipt rewrite → Task 5 (Ruling 1 keeps the engine on latest; stated). §8 → Nick; the npm hint stops advertising the dead hostname meanwhile. §9: receipt round trip (install half in the plugin plan; `--dry-run` = plan, uninstall leaves the preserve set, `--purge` leaves nothing and prints the system commands) → Task 3; no-receipt fallback → Task 3; Windows self-rename → Task 3 (`#[cfg(windows)]`, run on this box per Ruling 7); mutation → Task 3 (`mutation_guard_removed_deletes_the_impostor`). §10 order: Task 1 is step 1, Tasks 2-7 are step 3.

**Placeholders.** None. One deliberate trap is flagged inline (the duplicated `ran`/`run` lines in the purge test) so the implementer reads the `&dyn Fn` contract.

**Type consistency.** `Banner = dyn Fn(&Path) -> Option<String>` is what `plan` takes and what `banner_component` is (`&uninstall::banner_component` coerces). `execute`'s `run: &dyn Fn(&str, &[&str]) -> bool` matches the closure in `handle_uninstall`. `Receipt` fields used in `update_components` (`files`, `installer`) exist in Task 2. `windows_swap_binary(target, new_file) -> Result<PathBuf, String>` is called with `(target, &tmp)` and its `Ok` is discarded.

**Known gaps, stated.** (1) `banner_component` reads stdout only after the child exits; a component that prints more than the pipe buffer before exiting would block — none does (one line). (2) The manifest PEM parser assumes the fixed Ed25519 SPKI layout; a key in any other encoding reads as "unsigned", never as "signed". (3) `local_version` spawns the sibling without the 5-second cap that `banner_component` has; it runs only on a path that already passed `banner_component`.
