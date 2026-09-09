# Fresh install, update, uninstall — binary implementation plan (kannaka-memory)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the kannaka binary the two verbs the spec promises, `kannaka uninstall` and a `kannaka update` that refreshes the whole install, make this repo's old installers forward to the canonical one, and make the npm postinstall write the same receipt.

**Architecture:** Two new library modules. `install_receipt` owns the receipt file (`install.json`: types, load, atomic write, rotation) and is the only code that writes it from the binary. `uninstall` builds a plan from the receipt or, without one, from the spec's fallback table, with the identity check injected so tests never spawn a process; execution is separate from planning so `--dry-run` is the plan printed. `kannaka update` gains one call into a new `update_components` module that reads the receipt, fetches the signed constellation manifest, and refreshes every non-`kannaka` component with the existing safe swap. The forwarders and the npm change are small and self-contained.

**Tech Stack:** Rust 2021 (`clap` 4 builder API as in `src/cli.rs`, `serde`/`serde_json`, `ureq` 2, `sha2`, `ed25519-dalek` 2, `dirs` 5, `tempfile` 3 in dev-deps), POSIX sh, PowerShell 5.1, Node 18+ (`node --test`).

**Spec:** `kannaka-labs/kannaka-plugin` PR #19, `docs/superpowers/specs/2026-09-09-fresh-install-update-uninstall-design.md` **revision 2**. Sections referenced as §N. The installer half of the same spec is planned in kannaka-plugin (`docs/superpowers/plans/2026-09-09-fresh-install-installer.md`, PR #21); the receipt shape below is the one that plan writes.

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

1. **`kannaka` itself keeps updating from the latest GitHub release** (the existing, tested `self_update` path with the sha256 sidecar). The manifest governs the *siblings*. Spec §7 rev 2 says the same; the engine joins the pinned set once kannaka-library publishes a manifest per release.
2. **`kannaka update` rewrites the receipt in place, without rotation.** Rotation is for installs; an update every few days would otherwise fill all three slots with near-identical receipts and lose the install that mattered.
3. **Without a receipt, `kannaka update` refreshes the siblings it finds beside itself** (today's behaviour, plus `kannaka-hdl`) and then writes a first receipt with `installer = "kannaka update@<VERSION>"`, so the next uninstall has something to read.
4. **`--purge` needs consent** (§6 rev 2): on a terminal it asks for the word `purge`; with no terminal and no `--yes` it prints the question and exits 3. **`--purge` moves the data dir aside** to `<data dir>.removed-<UTC timestamp>` and prints the path; `--delete-data` deletes it. Only a data dir under `$HOME` that is not `$HOME` itself is touched at all; anything else is printed.
5. **Rc blocks are removed between the `# kannaka…` sentinel and the `# /kannaka` closer** (§5 rev 2), plus the one blank line the installer wrote before the sentinel. A legacy block with no closer loses the sentinel and only the lines the installer is known to have written (`RC_KNOWN_LINES`); any other line stays and is reported.
6. **Manifest signature verification in Rust** uses `ed25519-dalek` (already a dependency) over the raw manifest bytes, with the public key parsed from the PEM `manifest.pub` (SubjectPublicKeyInfo: the key is the last 32 bytes of the DER). A signature that fails means the manifest is ignored and the siblings are left as they are.
7. **The Windows self-rename is unit-tested on Windows, not in CI.** kannaka-memory CI is ubuntu-only and its sibling-checkout matrix is not worth duplicating for one rename. The test is `#[cfg(windows)]`, the implementer runs it on this Windows box, and the review package quotes the run.
8. **The uninstall fallback is the three installer directories** (`~/.local/bin`, `~/.cargo/bin`, `%LOCALAPPDATA%\Programs\kannaka`), §6 rev 2. A kannaka anywhere else on `PATH` is named with the package manager's own command and never touched.
9. **The Windows user PATH is edited with `reg add`, never `setx`** (setx truncates at 1024 characters). Only an entry the receipt's `path_edits` lists is removed.
10. **Registrations are reversed in order**: `claude-statusline` (run the plugin's `statusline/setup.sh off`, which restores the previous `statusLine`) before `claude-plugin`, before `claude-marketplace`, so the setup script is still on disk when it is needed.
11. **The npm postinstall merges** its one `files` entry into the existing receipt and writes atomically; it never rotates and never overwrites the other fields (§2 rev 2).

## File map

| file | responsibility |
|---|---|
| `scripts/install.sh`, `scripts/install.ps1` | forwarders to the canonical installer; a failed fetch is a failure |
| `scripts/tests/install-forwarder.test.sh` | runs the POSIX forwarder with a stubbed `curl`, asserts the canonical URL, argument and environment pass-through, failure on a failed fetch |
| `src/install_receipt.rs` | `Receipt` and entry types (incl. `extras`, `path_edits`, `declined`), `receipt_path`, `load_from`, `write_atomic`, `write_rotated` (complete → lock → rotate → rename), `sha256_hex` |
| `tests/fixtures/receipt-from-install-sh.json` | the receipt kannaka-plugin's `install.sh` produced in its test; parsed here so both repos test the same document |
| `src/uninstall.rs` | `banner_component`, `Options`, `Plan`, `DataAction`, `plan`, `fallback_candidates`, `elsewhere_on_path`, `strip_rc_block`, `purge_print_only`, `execute`, `render` |
| `src/update_components.rs` | `Manifest` loading + signature check, `swap_in`, `record_refresh`, `refresh_with` (testable core), `refresh_all` |
| `src/cli.rs` | `uninstall` subcommand and `handle_uninstall`; `update` long_about updated |
| `src/config.rs` | `self_update` calls `update_components::refresh_all`; `windows_swap_binary` and `platform_triple` become `pub(crate)` |
| `src/lib.rs` | `pub mod install_receipt; pub mod uninstall; pub mod update_components;` |
| `packaging/npm/install.js`, `packaging/npm/receipt.js`, `packaging/npm/receipt.test.js` | npm postinstall merges into the receipt |
| `.github/workflows/ci.yml` | run the forwarder test, the npm test, and the old-owner grep guard |
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
- Create: `tests/fixtures/receipt-from-install-sh.json` — copy `tests/fixtures/receipt-from-install-sh.json` from the kannaka-plugin checkout (produced by its lifecycle test, plan A Task 6). If that file does not exist yet, create it with the content in Step 1 below; it has the same shape.
- Modify: `src/lib.rs` (add `pub mod install_receipt;` after `pub mod config;` at line 88)

**Interfaces:**
- Produces:

```rust
pub const SCHEMA: u32 = 1;
pub const FILE_NAME: &str = "install.json";
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)] pub struct Receipt { … }   // every list field #[serde(default)]
pub struct FileEntry { pub path: PathBuf, pub sha256: String, pub component: String, pub version: String }
pub struct Extra { pub path: PathBuf, pub kind: String }
pub struct RcEdit { pub file: PathBuf, pub sentinel: String }
pub struct PathEdit { pub scope: String, pub entry: String }
pub struct ConfigEdit { pub file: PathBuf, pub sections: Vec<String> }
#[serde(untagged)] pub enum Credential { File { file: PathBuf }, UserEnv { kind: String, names: Vec<String> } }
pub struct Registration { pub kind: String, #[serde(default)] pub name: String }
pub struct Removed { pub path: String, pub component: String, pub reason: String }
pub struct Declined { pub path: String, pub reason: String }
pub fn receipt_path() -> PathBuf                                   // KannakaConfig::data_dir().join(FILE_NAME)
pub fn load_from(path: &Path) -> Result<Option<Receipt>, String>   // Ok(None) when absent
pub fn write_atomic(path: &Path, r: &Receipt) -> Result<(), String>   // tmp + rename, no rotation
pub fn write_rotated(path: &Path, r: &Receipt) -> Result<(), String>  // complete tmp, then .2->.3, .1->.2, cur->.1, then rename
pub fn sha256_hex(bytes: &[u8]) -> String
```

- [ ] **Step 1: The fixture and the failing tests**

If the plugin's fixture is not available, create `tests/fixtures/receipt-from-install-sh.json`:

```json
{
  "schema": 1,
  "installed_at": "2026-09-09T10:00:00Z",
  "installer": "kannaka-labs/kannaka-plugin/install/install.sh@2",
  "manifest": "latest",
  "platform": "linux-x86_64",
  "files": [
    {"path": "/tmp/tmp.abc/home/.local/bin/kannaka", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "component": "kannaka", "version": "9.9.9"},
    {"path": "/tmp/tmp.abc/home/.local/bin/kannaka-tui", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "component": "kannaka-tui", "version": "9.9.9"},
    {"path": "/tmp/tmp.abc/home/.local/bin/kannaka-hdl", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "component": "kannaka-hdl", "version": "9.9.9"}
  ],
  "extras": [
  ],
  "rc_edits": [
    {"file": "/tmp/tmp.abc/home/.bashrc", "sentinel": "# kannaka swarm credentials"}
  ],
  "path_edits": [],
  "config_edits": [
  ],
  "credentials": [
  ],
  "registrations": [
    {"kind": "claude-marketplace", "name": "kannaka-labs/kannaka-plugin"},
    {"kind": "claude-plugin", "name": "kannaka@kannaka"}
  ],
  "removed": [
    {"path": "/tmp/tmp.abc/home/.local/bin/kannaka", "component": "kannaka", "reason": "replaced"},
    {"path": "/tmp/tmp.abc/home/.cargo/bin/kannaka", "component": "kannaka", "reason": "cargo install era"},
    {"path": "/tmp/tmp.abc/home/shadow/kannaka", "component": "kannaka", "reason": "earlier on PATH than /tmp/tmp.abc/home/.local/bin"},
    {"path": "brew:kannaka", "component": "kannaka", "reason": "brew formula"}
  ],
  "declined": [
    {"path": "/tmp/tmp.abc/home/.cargo/bin/kannaka-hdl", "reason": "not kannaka"},
    {"path": "/tmp/tmp.abc/outside-bin/kannaka", "reason": "outside your home; it is earlier on PATH than /tmp/tmp.abc/home/.local/bin and will shadow the new kannaka"}
  ],
  "previous": []
}
```

Tests, at the bottom of the new module inside `#[cfg(test)] mod tests`:

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
            extras: vec![Extra { path: "/h/Desktop/Link Kannaka.command".into(), kind: "launcher".into() }],
            rc_edits: vec![RcEdit { file: "/h/.bashrc".into(), sentinel: "# kannaka".into() }],
            path_edits: vec![],
            config_edits: vec![],
            credentials: vec![Credential::File { file: "/h/.kannaka-nats.env".into() }],
            registrations: vec![Registration { kind: "claude-plugin".into(), name: "kannaka@kannaka".into() }, Registration { kind: "claude-statusline".into(), name: String::new() }],
            removed: vec![],
            declined: vec![],
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
    fn a_receipt_the_real_installer_wrote_parses() {
        // The document install.sh produced in kannaka-plugin's lifecycle test:
        // the two repos are held to the same file.
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/receipt-from-install-sh.json")).unwrap();
        let r: Receipt = serde_json::from_str(&text).unwrap();
        assert_eq!(r.schema, 1);
        assert!(r.installer.starts_with("kannaka-labs/kannaka-plugin/install/install.sh@"));
        assert!(r.files.iter().any(|f| f.component == "kannaka"));
        assert!(r.removed.iter().any(|x| x.reason == "cargo install era"));
        assert!(r.declined.iter().any(|x| x.reason == "not kannaka"));
        assert!(r.registrations.iter().any(|x| x.kind == "claude-plugin"));
    }

    #[test]
    fn missing_optional_lists_default_to_empty() {
        let text = r#"{"schema":1,"installed_at":"t","installer":"i","manifest":"m","platform":"p","files":[]}"#;
        let r: Receipt = serde_json::from_str(text).unwrap();
        assert!(r.removed.is_empty() && r.previous.is_empty() && r.extras.is_empty() && r.path_edits.is_empty() && r.declined.is_empty());
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
        assert!(!dir.path().join("install.json.lock").exists());
    }

    #[test]
    fn write_rotated_refuses_a_fresh_lock_and_ignores_a_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        let lock = dir.path().join("install.json.lock");
        std::fs::create_dir(&lock).unwrap();
        assert!(write_rotated(&p, &sample()).is_err());
        assert!(!p.exists());
        // age the lock past ten minutes
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(11 * 60);
        filetime_set(&lock, old);
        write_rotated(&p, &sample()).unwrap();
        assert!(p.exists() && !lock.exists());
    }

    fn filetime_set(p: &Path, t: std::time::SystemTime) {
        let f = std::fs::File::open(p).unwrap();
        f.set_modified(t).unwrap();
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
//! npm postinstall (merge, never rotate), and by this crate (`update_components`).
//! One shape, one file, one writer at a time.
//!
//! Spec: kannaka-plugin/docs/superpowers/specs/2026-09-09-fresh-install-update-uninstall-design.md §3 (rev 2).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCHEMA: u32 = 1;
pub const FILE_NAME: &str = "install.json";
const LOCK_STALE_SECS: u64 = 10 * 60;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Receipt {
    pub schema: u32,
    pub installed_at: String,
    pub installer: String,
    pub manifest: String,
    pub platform: String,
    #[serde(default)] pub files: Vec<FileEntry>,
    #[serde(default)] pub extras: Vec<Extra>,
    #[serde(default)] pub rc_edits: Vec<RcEdit>,
    #[serde(default)] pub path_edits: Vec<PathEdit>,
    #[serde(default)] pub config_edits: Vec<ConfigEdit>,
    #[serde(default)] pub credentials: Vec<Credential>,
    #[serde(default)] pub registrations: Vec<Registration>,
    #[serde(default)] pub removed: Vec<Removed>,
    #[serde(default)] pub declined: Vec<Declined>,
    #[serde(default)] pub previous: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileEntry { pub path: PathBuf, pub sha256: String, pub component: String, pub version: String }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Extra { pub path: PathBuf, pub kind: String }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RcEdit { pub file: PathBuf, pub sentinel: String }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PathEdit { pub scope: String, pub entry: String }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ConfigEdit { pub file: PathBuf, pub sections: Vec<String> }

/// POSIX installs record the credentials FILE; Windows installs record the
/// user-environment variable NAMES (there is no file). Both must load.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Credential {
    File { file: PathBuf },
    UserEnv { kind: String, names: Vec<String> },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Registration { pub kind: String, #[serde(default)] pub name: String }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Removed { pub path: String, pub component: String, pub reason: String }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Declined { pub path: String, pub reason: String }

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

fn write_tmp(path: &Path, r: &Receipt) -> Result<PathBuf, String> {
    let dir = path.parent().ok_or("receipt path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!("{FILE_NAME}.tmp.{}", std::process::id()));
    let text = serde_json::to_string_pretty(r).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    Ok(tmp)
}

/// Write `r` to `path` atomically: a temp file in the same directory, then
/// a rename. No rotation — what `kannaka update` and npm use.
pub fn write_atomic(path: &Path, r: &Receipt) -> Result<(), String> {
    let tmp = write_tmp(path, r)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot move receipt into place: {e}")
    })
}

/// Take the writer lock (a directory: mkdir is atomic everywhere). A lock
/// older than ten minutes belongs to a crashed writer and is replaced.
fn take_lock(path: &Path) -> Result<PathBuf, String> {
    let lock = PathBuf::from(format!("{}.lock", path.display()));
    if let Err(e) = std::fs::create_dir(&lock) {
        if e.kind() != std::io::ErrorKind::AlreadyExists { return Err(format!("cannot create {}: {e}", lock.display())); }
        let age = std::fs::metadata(&lock).and_then(|m| m.modified()).ok()
            .and_then(|t| std::time::SystemTime::now().duration_since(t).ok());
        match age {
            Some(a) if a.as_secs() > LOCK_STALE_SECS => {
                let _ = std::fs::remove_dir_all(&lock);
                std::fs::create_dir(&lock).map_err(|e| format!("cannot re-create {}: {e}", lock.display()))?;
            }
            _ => return Err(format!("another writer holds {} (younger than ten minutes); not writing the receipt", lock.display())),
        }
    }
    Ok(lock)
}

/// Complete the temp file, take the lock, rotate `install.json` -> `.1` -> `.2`
/// -> `.3` (the oldest is dropped), set `previous`, rename into place. A crash
/// at any point leaves either the old receipt or the new one. What an INSTALL
/// does; see write_atomic for an update.
pub fn write_rotated(path: &Path, r: &Receipt) -> Result<(), String> {
    let mut r = r.clone();
    r.previous.clear();
    let tmp = write_tmp(path, &r)?;
    let lock = match take_lock(path) { Ok(l) => l, Err(e) => { let _ = std::fs::remove_file(&tmp); return Err(e); } };
    let s = path.to_string_lossy().to_string();
    let n = |k: u32| PathBuf::from(format!("{s}.{k}"));
    let result = (|| {
        if n(2).exists() { std::fs::rename(n(2), n(3)).map_err(|e| e.to_string())?; }
        if n(1).exists() { std::fs::rename(n(1), n(2)).map_err(|e| e.to_string())?; }
        if path.exists() { std::fs::rename(path, n(1)).map_err(|e| e.to_string())?; }
        r.previous = (1..=3).filter(|k| n(*k).exists()).map(|k| format!("{FILE_NAME}.{k}")).collect();
        let text = serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| format!("cannot move receipt into place: {e}"))
    })();
    let _ = std::fs::remove_dir_all(&lock);
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result
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
Expected: 9 passed.

- [ ] **Step 5: Commit**

```bash
git add src/install_receipt.rs src/lib.rs tests/fixtures/receipt-from-install-sh.json
git commit -m "install_receipt: the install receipt as a type; complete-rotate-rename under a lock; the installer's own receipt as a fixture"
```

---

### Task 3: The uninstall planner and executor

**Files:**
- Create: `src/uninstall.rs`
- Modify: `src/lib.rs` (`pub mod uninstall;` after `install_receipt`)

**Interfaces:**
- Consumes: `install_receipt::{Receipt, Credential, Registration, Extra, PathEdit}`
- Produces:

```rust
pub const COMPONENTS: [&str; 3];
pub const SYSTEMD_UNITS: [&str; 3];   // kannaka-attention.service, kannaka-eye.service, kannaka-hive-bridge.service
pub const WINDOWS_TASK: &str;         // KannakaSeedBeacon
pub const RC_OPEN: [&str; 2];         // "# kannaka", "# kannaka swarm credentials"
pub const RC_CLOSE: &str;             // "# /kannaka"
pub const LAUNCHER_MARK: &str;        // "Links your Constellation Pass"
pub type Banner = dyn Fn(&Path) -> Option<String>;
pub fn banner_component(path: &Path) -> Option<String>;     // spawns `--version`, 5 s cap, only under $HOME
pub struct Options { pub purge: bool, pub delete_data: bool, pub dry_run: bool }
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub unlink: Vec<PathBuf>,                 // proven ours
    pub declined: Vec<(PathBuf, String)>,     // never touched, always reported
    pub extras: Vec<PathBuf>,                 // launcher(s) whose content still carries LAUNCHER_MARK — purge only
    pub rc_blocks: Vec<(PathBuf, String)>,    // purge only
    pub path_edits: Vec<String>,              // Windows user-PATH entries to drop — purge only
    pub data_dir: Option<(PathBuf, DataAction)>, // purge only; MoveAside(<target>) or Delete
    pub remove_files: Vec<PathBuf>,           // credential files, the receipt and its rotations
    pub user_env: Vec<String>,                // purge only
    pub registrations: Vec<Registration>,     // purge only; claude-statusline first, then plugin, then marketplace
    pub print_only: Vec<String>,              // commands printed, never run
    pub self_path: Option<PathBuf>,           // removed last
}
#[derive(Debug, Clone, PartialEq)] pub enum DataAction { MoveAside(PathBuf), Delete }
pub fn plan(receipt: Option<&Receipt>, opts: &Options, home: &Path, data_dir: &Path, path_env: &str, current_exe: &Path, banner: &Banner, now: &str) -> Plan;
pub fn fallback_candidates(home: &Path) -> Vec<PathBuf>;                 // ~/.local/bin, ~/.cargo/bin, %LOCALAPPDATA%\Programs\kannaka only
pub fn elsewhere_on_path(home: &Path, path_env: &str) -> Vec<PathBuf>;   // kannaka* in other PATH dirs: declined with a hint, never touched
pub fn strip_rc_block(text: &str, sentinel: &str) -> (String, Vec<String>);  // (new text, lines it declined to remove)
pub fn purge_print_only(home: &Path, data_dir: &Path) -> Vec<String>;
pub struct Report { pub removed: Vec<PathBuf>, pub parked: Vec<PathBuf>, pub still_present: Vec<PathBuf>, pub declined: Vec<(PathBuf, String)>, pub moved_aside: Option<PathBuf> }
pub fn execute(plan: &Plan, run: &dyn Fn(&str, &[&str]) -> bool) -> Report;
pub fn render(plan: &Plan) -> String;
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
        fn new() -> Self { let fx = Fx { dir: tempfile::tempdir().unwrap(), banners: HashMap::new() }; std::fs::create_dir_all(fx.home()).unwrap(); fx }
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
                .filter(|s| COMPONENTS.contains(&s.as_str())))
        }
        fn receipt(&self, files: &[PathBuf]) -> Receipt {
            Receipt {
                schema: SCHEMA, installed_at: "t".into(), installer: "i".into(), manifest: "m".into(), platform: "p".into(),
                files: files.iter().map(|p| FileEntry { path: p.clone(), sha256: "0".repeat(64), component: p.file_name().unwrap().to_string_lossy().trim_end_matches(".exe").to_string(), version: "1".into() }).collect(),
                extras: vec![Extra { path: self.home().join("Desktop/Link Kannaka.command"), kind: "launcher".into() }],
                rc_edits: vec![RcEdit { file: self.home().join(".bashrc"), sentinel: "# kannaka".into() }],
                path_edits: vec![],
                config_edits: vec![],
                credentials: vec![Credential::File { file: self.home().join(".kannaka-nats.env") }],
                registrations: vec![
                    Registration { kind: "claude-plugin".into(), name: "kannaka@kannaka".into() },
                    Registration { kind: "claude-statusline".into(), name: String::new() },
                ],
                removed: vec![], declined: vec![], previous: vec![],
            }
        }
    }
    fn no_purge() -> Options { Options { purge: false, delete_data: false, dry_run: false } }
    fn purge() -> Options { Options { purge: true, delete_data: false, dry_run: false } }
    fn noop_run(_: &str, _: &[&str]) -> bool { true }
    const NOW: &str = "20260909T120000Z";

    #[test]
    fn receipt_plan_is_exactly_the_receipt_and_keeps_the_preserve_set() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "0.17.0");
        let t = fx.ours(".local/bin/kannaka-tui", "kannaka-tui", "0.5.9");
        std::fs::create_dir_all(fx.data()).unwrap();
        std::fs::write(fx.data().join("node_key.ed25519"), b"IDENTITY").unwrap();
        std::fs::write(fx.home().join(".bashrc"), "x\n\n# kannaka\nexport PATH=1\n# /kannaka\n").unwrap();
        std::fs::write(fx.home().join(".kannaka-nats.env"), "u").unwrap();
        let r = fx.receipt(&[k.clone(), t.clone()]);
        let p = plan(Some(&r), &no_purge(), &fx.home(), &fx.data(), "", &k, &*fx.banner(), NOW);
        assert_eq!(p.unlink, vec![t.clone()]);            // kannaka itself is self_path, not unlink
        assert_eq!(p.self_path, Some(k.clone()));
        assert!(p.rc_blocks.is_empty() && p.data_dir.is_none() && p.user_env.is_empty() && p.registrations.is_empty() && p.extras.is_empty(), "{p:?}");
        assert_eq!(p.remove_files, vec![fx.data().join("install.json")]);   // only the receipt leaves the data dir
        let rep = execute(&p, &noop_run);
        assert!(!t.exists() && !k.exists());
        assert!(rep.still_present.is_empty());
        assert_eq!(std::fs::read(fx.data().join("node_key.ed25519")).unwrap(), b"IDENTITY");
        assert_eq!(std::fs::read_to_string(fx.home().join(".bashrc")).unwrap(), "x\n\n# kannaka\nexport PATH=1\n# /kannaka\n");
        assert!(fx.home().join(".kannaka-nats.env").exists());
    }

    #[test]
    fn a_receipt_path_that_is_not_kannaka_is_declined_not_deleted() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        let x = fx.impostor(".local/bin/kannaka-tui");
        let r = fx.receipt(&[k.clone(), x.clone()]);
        let p = plan(Some(&r), &no_purge(), &fx.home(), &fx.data(), "", &k, &*fx.banner(), NOW);
        assert!(p.unlink.is_empty());
        assert_eq!(p.declined, vec![(x.clone(), "not kannaka".to_string())]);
        execute(&p, &noop_run);
        assert!(x.exists());
    }

    #[test]
    fn purge_moves_the_data_dir_aside_and_reverses_everything_else() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        std::fs::create_dir_all(fx.data()).unwrap();
        std::fs::write(fx.data().join("kannaka.hrm"), b"HRM").unwrap();
        std::fs::write(fx.home().join(".bashrc"), "x\n\n# kannaka\nexport PATH=1\n# /kannaka\n\n# other\n").unwrap();
        std::fs::write(fx.home().join(".kannaka-nats.env"), "u").unwrap();
        std::fs::create_dir_all(fx.home().join("Desktop")).unwrap();
        std::fs::write(fx.home().join("Desktop/Link Kannaka.command"), "#!/bin/sh\n# Links your Constellation Pass to this machine.\n").unwrap();
        let r = fx.receipt(&[k.clone()]);
        let p = plan(Some(&r), &purge(), &fx.home(), &fx.data(), "", &k, &*fx.banner(), NOW);
        let aside = fx.home().join(format!(".kannaka.removed-{NOW}"));
        assert_eq!(p.data_dir, Some((fx.data(), DataAction::MoveAside(aside.clone()))));
        assert_eq!(p.rc_blocks, vec![(fx.home().join(".bashrc"), "# kannaka".to_string())]);
        assert!(p.remove_files.contains(&fx.home().join(".kannaka-nats.env")));
        assert_eq!(p.extras, vec![fx.home().join("Desktop/Link Kannaka.command")]);
        assert_eq!(p.registrations.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(), vec!["claude-statusline", "claude-plugin"]);
        let ran = std::cell::RefCell::new(Vec::<String>::new());
        let run = |cmd: &str, args: &[&str]| { ran.borrow_mut().push(format!("{cmd} {}", args.join(" "))); true };
        let rep = execute(&p, &run);
        assert!(!fx.data().exists() && aside.join("kannaka.hrm").exists(), "data dir moved aside, not deleted");
        assert_eq!(rep.moved_aside, Some(aside));
        assert_eq!(std::fs::read_to_string(fx.home().join(".bashrc")).unwrap(), "x\n\n# other\n");
        assert!(!fx.home().join(".kannaka-nats.env").exists());
        assert!(!fx.home().join("Desktop/Link Kannaka.command").exists());
        let ran = ran.borrow();
        assert!(ran.iter().any(|c| c.ends_with("off")), "statusline off not run: {ran:?}");
        assert!(ran.iter().any(|c| c == "claude plugin uninstall kannaka@kannaka"), "{ran:?}");
        assert!(rep.still_present.is_empty());
    }

    #[test]
    fn purge_with_delete_data_deletes() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        std::fs::create_dir_all(fx.data()).unwrap();
        let r = fx.receipt(&[k.clone()]);
        let p = plan(Some(&r), &Options { purge: true, delete_data: true, dry_run: false }, &fx.home(), &fx.data(), "", &k, &*fx.banner(), NOW);
        assert_eq!(p.data_dir, Some((fx.data(), DataAction::Delete)));
        execute(&p, &noop_run);
        assert!(!fx.data().exists());
        assert!(std::fs::read_dir(fx.home()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".kannaka.removed")));
    }

    #[test]
    fn purge_never_touches_a_data_dir_outside_home_or_home_itself() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        let outside = fx.dir.path().join("srv-data"); std::fs::create_dir_all(&outside).unwrap();
        let r = fx.receipt(&[k.clone()]);
        let p = plan(Some(&r), &purge(), &fx.home(), &outside, "", &k, &*fx.banner(), NOW);
        assert!(p.data_dir.is_none(), "{p:?}");
        assert!(p.print_only.iter().any(|l| l.contains(&outside.display().to_string())), "{p:?}");
        execute(&p, &noop_run);
        assert!(outside.exists());
        let p = plan(Some(&r), &purge(), &fx.home(), &fx.home(), "", &k, &*fx.banner(), NOW);
        assert!(p.data_dir.is_none(), "KANNAKA_DATA_DIR=$HOME must never be purged: {p:?}");
        assert!(p.print_only.iter().any(|l| l.contains("is your home directory")), "{p:?}");
    }

    #[test]
    fn purge_prints_rather_than_runs_system_commands() {
        let fx = Fx::new();
        let outside = fx.dir.path().join("srv-data");
        let lines = purge_print_only(&fx.home(), &outside);
        // units are listed only when present on this machine; the task and the outside dir always
        for u in SYSTEMD_UNITS { if Path::new("/etc/systemd/system").join(u).exists() { assert!(lines.iter().any(|l| l.contains(u)), "{lines:?}"); } }
        if cfg!(windows) { assert!(lines.iter().any(|l| l.contains(WINDOWS_TASK)), "{lines:?}"); }
        assert!(lines.iter().any(|l| l.contains(&outside.display().to_string())), "{lines:?}");
    }

    #[test]
    fn no_receipt_falls_back_to_the_three_installer_dirs_and_names_the_rest() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        let c = fx.ours(".cargo/bin/kannaka", "kannaka", "0.9");
        let s = fx.ours("shadow/kannaka-tui", "kannaka-tui", "0.4");     // on PATH but NOT an installer dir
        let x = fx.impostor(".cargo/bin/kannaka-hdl");
        std::fs::write(fx.home().join(".local/bin/kannaka.bak-7"), b"stale").unwrap();
        let sep = if cfg!(windows) { ";" } else { ":" };
        let path_env = format!("{}{sep}{}", fx.home().join("shadow").display(), fx.home().join(".local/bin").display());
        let p = plan(None, &no_purge(), &fx.home(), &fx.data(), &path_env, &k, &*fx.banner(), NOW);
        let mut unlink = p.unlink.clone(); unlink.sort();
        let mut want = vec![c.clone(), fx.home().join(".local/bin/kannaka.bak-7")]; want.sort();
        assert_eq!(unlink, want);
        assert_eq!(p.self_path, Some(k));
        assert!(p.declined.contains(&(x, "not kannaka".to_string())), "{:?}", p.declined);
        let shadow = p.declined.iter().find(|(pp, _)| *pp == s).expect("shadow copy named");
        assert!(shadow.1.contains("not an installer directory"), "{shadow:?}");
        assert!(s.exists());
    }

    #[test]
    fn exit_is_nonzero_when_something_survives() {
        let mut fx = Fx::new();
        let k = fx.ours(".local/bin/kannaka", "kannaka", "1");
        let t = fx.ours(".local/bin/kannaka-tui", "kannaka-tui", "1");
        let r = fx.receipt(&[k.clone(), t.clone()]);
        let p = plan(Some(&r), &no_purge(), &fx.home(), &fx.data(), "", &k, &*fx.banner(), NOW);
        // make the tui undeletable: turn it into a non-empty directory
        std::fs::remove_file(&t).unwrap(); std::fs::create_dir(&t).unwrap(); std::fs::write(t.join("x"), b"").unwrap();
        let rep = execute(&p, &noop_run);
        assert_eq!(rep.still_present, vec![t]);
    }

    #[test]
    fn strip_rc_block_removes_between_markers_and_keeps_user_lines() {
        // closed block: everything between the markers goes, the blank line before it too
        assert_eq!(strip_rc_block("a\n\n# kannaka\nl1\nl2\n# /kannaka\nb\n", "# kannaka"), ("a\nb\n".to_string(), vec![]));
        // legacy block without a closer: only the sentinel and the line the installer wrote
        let legacy = "a\n\n# kannaka\ncase \":$PATH:\" in *\":$HOME/.local/bin:\"*) ;; *) export PATH=\"$HOME/.local/bin:$PATH\" ;; esac\nexport EDITOR=vim\n";
        let (out, kept) = strip_rc_block(legacy, "# kannaka");
        assert_eq!(out, "a\n\nexport EDITOR=vim\n");
        assert!(kept.is_empty());
        // a legacy block whose next line is not ours: it is kept and reported
        let (out, kept) = strip_rc_block("a\n# kannaka\nalias g=git\n", "# kannaka");
        assert_eq!(out, "a\nalias g=git\n");
        assert_eq!(kept, vec!["alias g=git"]);
        // the sentinel must match the whole line
        assert_eq!(strip_rc_block("a\n# kannaka swarm credentials\nl1\n", "# kannaka").0, "a\n# kannaka swarm credentials\nl1\n");
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
        let p = plan(None, &no_purge(), &fx.home(), &fx.data(), "", &k, &*accept_all, NOW);
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
        assert_eq!(rep.parked.len(), 1);
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
//! fall back to the three directories our installers write into. Either way
//! a file is removed only when its --version banner says it is one of ours,
//! and nothing outside $HOME is ever run or removed. Planning is separate from
//! execution so --dry-run is the plan, printed.
//!
//! Spec: kannaka-plugin/docs/superpowers/specs/2026-09-09-fresh-install-update-uninstall-design.md §4-§6 (rev 2).

use crate::install_receipt::{self as receipt, Credential, Receipt, Registration};
use std::path::{Path, PathBuf};

pub const COMPONENTS: [&str; 3] = ["kannaka", "kannaka-tui", "kannaka-hdl"];
pub const SYSTEMD_UNITS: [&str; 3] = ["kannaka-attention.service", "kannaka-eye.service", "kannaka-hive-bridge.service"];
pub const WINDOWS_TASK: &str = "KannakaSeedBeacon";
pub const RC_OPEN: [&str; 2] = ["# kannaka", "# kannaka swarm credentials"];
pub const RC_CLOSE: &str = "# /kannaka";
pub const LAUNCHER_MARK: &str = "Links your Constellation Pass";
const RC_FILES: [&str; 4] = [".bashrc", ".zshrc", ".bash_profile", ".profile"];
/// The exact lines the installers have ever written under a sentinel, for
/// legacy blocks that have no closing marker.
const RC_KNOWN_LINES: [&str; 2] = [
    r#"case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) export PATH="$HOME/.local/bin:$PATH" ;; esac"#,
    r#"[ -f "$HOME/.kannaka-nats.env" ] && . "$HOME/.kannaka-nats.env""#,
];

pub type Banner = dyn Fn(&Path) -> Option<String>;

fn under(path: &Path, dir: &Path) -> bool {
    let (Ok(p), Ok(d)) = (std::fs::canonicalize(path), std::fs::canonicalize(dir)) else { return path.starts_with(dir) };
    p.starts_with(&d)
}

/// The real identity check: run `<path> --version`, wait at most five
/// seconds, take the first word of the first line. Only under $HOME.
pub fn banner_component(path: &Path) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let home = dirs::home_dir()?;
    if !path.is_file() || !under(path, &home) { return None; }
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
    let ver = words.next()?.trim_start_matches('v');
    if COMPONENTS.contains(&name) && ver.chars().next().is_some_and(|c| c.is_ascii_digit()) { Some(name.to_string()) } else { None }
}

pub struct Options { pub purge: bool, pub delete_data: bool, pub dry_run: bool }

#[derive(Debug, Clone, PartialEq)]
pub enum DataAction { MoveAside(PathBuf), Delete }

#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub unlink: Vec<PathBuf>,
    pub declined: Vec<(PathBuf, String)>,
    pub extras: Vec<PathBuf>,
    pub rc_blocks: Vec<(PathBuf, String)>,
    pub path_edits: Vec<String>,
    pub data_dir: Option<(PathBuf, DataAction)>,
    pub remove_files: Vec<PathBuf>,
    pub user_env: Vec<String>,
    pub registrations: Vec<Registration>,
    pub print_only: Vec<String>,
    pub self_path: Option<PathBuf>,
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) { (Ok(x), Ok(y)) => x == y, _ => a == b }
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

fn installer_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![home.join(".local").join("bin"), home.join(".cargo").join("bin")];
    if cfg!(windows) {
        if let Ok(lap) = std::env::var("LOCALAPPDATA") { dirs.push(PathBuf::from(lap).join("Programs").join("kannaka")); }
    }
    dirs
}

/// The narrow fallback (§6): only the directories our installers write into.
pub fn fallback_candidates(home: &Path) -> Vec<PathBuf> {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let mut out = Vec::new();
    for d in installer_dirs(home) {
        for c in COMPONENTS {
            let p = d.join(format!("{c}{ext}"));
            if p.exists() && !out.contains(&p) { out.push(p); }
        }
    }
    out
}

/// A kannaka in any OTHER PATH directory is named, never touched: it belongs
/// to a package manager or to an admin.
pub fn elsewhere_on_path(home: &Path, path_env: &str) -> Vec<PathBuf> {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let sep = if cfg!(windows) { ';' } else { ':' };
    let ours = installer_dirs(home);
    let mut out = Vec::new();
    for d in path_env.split(sep).filter(|d| !d.is_empty()).map(PathBuf::from) {
        if ours.iter().any(|o| same_file(o, &d)) { continue; }
        for c in COMPONENTS {
            let p = d.join(format!("{c}{ext}"));
            if p.exists() && !out.contains(&p) { out.push(p); }
        }
    }
    out
}

fn hint_for(path: &Path) -> String {
    let s = path.display().to_string();
    if s.contains("/opt/homebrew/") || s.contains("/usr/local/Cellar/") || s.contains("/home/linuxbrew/") { return "brew uninstall kannaka".into(); }
    if s.contains("node_modules") || s.contains("/lib/node") { return "npm rm -g kannaka".into(); }
    "remove it with the tool that installed it".into()
}

pub fn plan(receipt_: Option<&Receipt>, opts: &Options, home: &Path, data_dir: &Path, path_env: &str, current_exe: &Path, banner: &Banner, now: &str) -> Plan {
    let mut out = Plan::default();
    match receipt_ {
        Some(r) => for f in &r.files { consider(&f.path, current_exe, banner, &mut out); },
        None => for p in fallback_candidates(home) { consider(&p, current_exe, banner, &mut out); },
    }
    for p in elsewhere_on_path(home, path_env) {
        if out.unlink.contains(&p) || out.self_path.as_deref() == Some(&p) { continue; }
        out.declined.push((p.clone(), format!("not an installer directory; left alone ({})", hint_for(&p))));
    }
    // The receipt and its rotations always go: they describe an install that
    // no longer exists once this runs.
    let rp = data_dir.join(receipt::FILE_NAME);
    out.remove_files.push(rp.clone());
    for k in [".1", ".2", ".3"] {
        let p = PathBuf::from(format!("{}{k}", rp.display()));
        if p.exists() { out.remove_files.push(p); }
    }
    if opts.purge {
        // The data dir: only under $HOME, never $HOME itself; otherwise printed.
        let is_home = same_file(data_dir, home);
        if data_dir.starts_with(home) && !is_home && data_dir.exists() {
            let aside = PathBuf::from(format!("{}.removed-{now}", data_dir.display()));
            out.data_dir = Some((data_dir.to_path_buf(), if opts.delete_data { DataAction::Delete } else { DataAction::MoveAside(aside) }));
        }
        match receipt_ {
            Some(r) => {
                for e in &r.rc_edits { if e.file.exists() { out.rc_blocks.push((e.file.clone(), e.sentinel.clone())); } }
                for c in &r.credentials {
                    match c {
                        Credential::File { file } => if file.exists() { out.remove_files.push(file.clone()); },
                        Credential::UserEnv { names, .. } => out.user_env.extend(names.iter().cloned()),
                    }
                }
                for x in &r.extras {
                    if x.kind == "launcher" && std::fs::read_to_string(&x.path).map(|t| t.contains(LAUNCHER_MARK)).unwrap_or(false) { out.extras.push(x.path.clone()); }
                }
                out.path_edits = r.path_edits.iter().filter(|p| p.scope == "user").map(|p| p.entry.clone()).collect();
                // statusline off BEFORE the plugin goes, so setup.sh is still there to run
                let mut regs: Vec<Registration> = r.registrations.iter().filter(|x| x.kind == "claude-statusline").cloned().collect();
                regs.extend(r.registrations.iter().filter(|x| x.kind == "claude-plugin").cloned());
                regs.extend(r.registrations.iter().filter(|x| x.kind == "claude-marketplace").cloned());
                out.registrations = regs;
            }
            None => {
                for rc in RC_FILES {
                    let f = home.join(rc);
                    let Ok(text) = std::fs::read_to_string(&f) else { continue };
                    for s in RC_OPEN { if text.lines().any(|l| l.trim_end() == s) { out.rc_blocks.push((f.clone(), s.to_string())); } }
                }
                let creds = home.join(".kannaka-nats.env");
                if creds.exists() { out.remove_files.push(creds); }
                if cfg!(windows) { out.user_env = vec!["NATS_USER".into(), "NATS_PASSWORD".into()]; }
                out.registrations = vec![Registration { kind: "claude-statusline".into(), name: String::new() }, Registration { kind: "claude-plugin".into(), name: "kannaka@kannaka".into() }];
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
        if Path::new("/etc/systemd/system").join(u).exists() { v.push(format!("sudo systemctl disable --now {u} && sudo rm /etc/systemd/system/{u}")); }
    }
    if cfg!(windows) { v.push(format!("schtasks /Delete /TN {WINDOWS_TASK} /F     (only if the seed beacon task was installed)")); }
    if cfg!(target_os = "macos") && Path::new("/var/db/receipts").exists() { v.push("sudo pkgutil --forget com.kannaka.pkg     (only if the .pkg was used)".into()); }
    if same_file(data_dir, home) {
        v.push(format!("KANNAKA_DATA_DIR={} is your home directory; nothing there is removed", data_dir.display()));
    } else if !data_dir.starts_with(home) {
        v.push(format!("rm -rf {}     (KANNAKA_DATA_DIR is outside your home; not touched)", data_dir.display()));
    }
    v
}

/// Remove an rc block. Closed block: sentinel through `# /kannaka`, plus one
/// blank line before the sentinel (the installer wrote it). Legacy block
/// without a closer: the sentinel and the lines the installer is known to
/// have written; anything else stays and is returned as declined.
pub fn strip_rc_block(text: &str, sentinel: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let bare = |l: &str| l.trim_end_matches(['\n', '\r']).to_string();
    let Some(start) = lines.iter().position(|l| bare(l) == sentinel) else { return (text.to_string(), vec![]) };
    let closed = lines[start + 1..].iter().position(|l| bare(l) == RC_CLOSE).map(|i| start + 1 + i);
    let mut out: Vec<&str> = Vec::new();
    let mut declined = Vec::new();
    // drop one blank line immediately before the sentinel
    let keep_before = if start > 0 && bare(lines[start - 1]).trim().is_empty() { start - 1 } else { start };
    out.extend_from_slice(&lines[..keep_before]);
    match closed {
        Some(end) => out.extend_from_slice(&lines[end + 1..]),
        None => {
            let mut i = start + 1;
            while i < lines.len() {
                let b = bare(lines[i]);
                if b.trim().is_empty() { break; }
                if !RC_KNOWN_LINES.contains(&b.as_str()) { declined.push(b); out.push(lines[i]); }
                i += 1;
            }
            out.extend_from_slice(&lines[i..]);
        }
    }
    (out.concat(), declined)
}

pub struct Report { pub removed: Vec<PathBuf>, pub parked: Vec<PathBuf>, pub still_present: Vec<PathBuf>, pub declined: Vec<(PathBuf, String)>, pub moved_aside: Option<PathBuf> }

/// Remove a file; on Windows a locked (running) exe is parked instead.
fn remove_or_park(p: &Path, rep: &mut Report) {
    if p.is_dir() {
        if std::fs::remove_dir_all(p).is_ok() && !p.exists() { rep.removed.push(p.to_path_buf()); } else { rep.still_present.push(p.to_path_buf()); }
        return;
    }
    if std::fs::remove_file(p).is_ok() && !p.exists() { rep.removed.push(p.to_path_buf()); return; }
    #[cfg(windows)]
    {
        let bak = PathBuf::from(format!("{}.bak-{}", p.display(), std::process::id()));
        if std::fs::rename(p, &bak).is_ok() { rep.parked.push(bak); return; }
    }
    if p.exists() { rep.still_present.push(p.to_path_buf()); }
}

fn statusline_setup(home: &Path) -> Option<PathBuf> {
    let cache = home.join(".claude").join("plugins").join("cache").join("kannaka");
    let mut found: Vec<PathBuf> = walk(&cache).into_iter().filter(|p| p.file_name().is_some_and(|n| n == "setup.sh") && p.parent().is_some_and(|d| d.file_name().is_some_and(|n| n == "statusline"))).collect();
    found.sort();
    found.pop()
}
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) { for e in rd.flatten() { let p = e.path(); if p.is_dir() { out.extend(walk(&p)); } else { out.push(p); } } }
    out
}

/// Carry out the plan. `run` executes an external command and returns
/// whether it succeeded; injected so tests never spawn anything.
pub fn execute(plan: &Plan, run: &dyn Fn(&str, &[&str]) -> bool) -> Report {
    let mut rep = Report { removed: vec![], parked: vec![], still_present: vec![], declined: plan.declined.clone(), moved_aside: None };
    for p in &plan.unlink { remove_or_park(p, &mut rep); }
    for (f, s) in &plan.rc_blocks {
        if let Ok(text) = std::fs::read_to_string(f) {
            let (new, kept) = strip_rc_block(&text, s);
            if new != text { let _ = std::fs::write(f, new); }
            for k in kept { rep.declined.push((f.clone(), format!("kept a line you added under {s}: {k}"))); }
        }
    }
    for p in &plan.extras { if p.exists() { remove_or_park(p, &mut rep); } }
    for p in &plan.remove_files { if p.exists() { remove_or_park(p, &mut rep); } }
    for name in &plan.user_env {
        #[cfg(windows)] { let _ = run("reg", &["delete", "HKCU\\Environment", "/v", name, "/f"]); }
        #[cfg(not(windows))] { let _ = name; }
    }
    #[cfg(windows)]
    for entry in &plan.path_edits { user_path_remove(entry, run); }
    #[cfg(not(windows))]
    { let _ = &plan.path_edits; }
    let home = dirs::home_dir().unwrap_or_default();
    for r in &plan.registrations {
        match r.kind.as_str() {
            "claude-statusline" => { if let Some(s) = statusline_setup(&home) { let _ = run("bash", &[&s.display().to_string(), "off"]); } }
            "claude-plugin" => { let _ = run("claude", &["plugin", "uninstall", &r.name]); }
            "claude-marketplace" => { let _ = run("claude", &["plugin", "marketplace", "remove", "kannaka"]); }
            _ => {}
        }
    }
    if let Some((d, action)) = &plan.data_dir {
        if d.exists() {
            match action {
                DataAction::MoveAside(aside) => { if std::fs::rename(d, aside).is_ok() { rep.moved_aside = Some(aside.clone()); rep.removed.push(d.clone()); } else { rep.still_present.push(d.clone()); } }
                DataAction::Delete => { if std::fs::remove_dir_all(d).is_ok() && !d.exists() { rep.removed.push(d.clone()); } else { rep.still_present.push(d.clone()); } }
            }
        }
    }
    // Self, last.
    if let Some(me) = &plan.self_path {
        #[cfg(windows)]
        {
            let bak = PathBuf::from(format!("{}.bak-{}", me.display(), std::process::id()));
            if std::fs::rename(me, &bak).is_ok() { rep.parked.push(bak); } else { rep.still_present.push(me.clone()); }
        }
        #[cfg(not(windows))]
        { if std::fs::remove_file(me).is_ok() { rep.removed.push(me.clone()); } else { rep.still_present.push(me.clone()); } }
    }
    rep
}

/// Drop one entry from the user PATH in the registry. `reg` rather than setx:
/// setx truncates at 1024 characters and would eat the rest of the PATH.
#[cfg(windows)]
fn user_path_remove(entry: &str, run: &dyn Fn(&str, &[&str]) -> bool) {
    let out = std::process::Command::new("reg").args(["query", "HKCU\\Environment", "/v", "Path"]).output();
    let Ok(out) = out else { return };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(line) = text.lines().find(|l| l.trim_start().starts_with("Path")) else { return };
    let mut parts = line.split_whitespace();
    let (_name, kind) = (parts.next(), parts.next().unwrap_or("REG_EXPAND_SZ"));
    let value = line.splitn(3, char::is_whitespace).nth(2).map(|s| s.trim()).unwrap_or("");
    let value = value.trim_start_matches(|c: char| c == 'R' || c == 'E' || c == 'G' || c == '_' || c == 'S' || c == 'Z' || c == 'X' || c == 'P' || c == 'A' || c == 'N' || c == 'D' || c.is_whitespace());
    let keep: Vec<&str> = value.split(';').filter(|e| !e.is_empty() && e.trim_end_matches('\\') != entry.trim_end_matches('\\')).collect();
    if keep.len() == value.split(';').filter(|e| !e.is_empty()).count() { return; }
    let new = keep.join(";");
    let _ = run("reg", &["add", "HKCU\\Environment", "/v", "Path", "/t", kind, "/d", &new, "/f"]);
}

pub fn render(plan: &Plan) -> String {
    let mut s = String::new();
    for p in &plan.unlink { s.push_str(&format!("remove   {}\n", p.display())); }
    for p in &plan.extras { s.push_str(&format!("remove   {}  (launcher)\n", p.display())); }
    for p in &plan.remove_files { s.push_str(&format!("remove   {}\n", p.display())); }
    for (f, sent) in &plan.rc_blocks { s.push_str(&format!("edit     {}  (drop the '{sent}' block)\n", f.display())); }
    for e in &plan.path_edits { s.push_str(&format!("unset    user PATH entry {e}\n")); }
    for n in &plan.user_env { s.push_str(&format!("unset    user environment {n}\n")); }
    for r in &plan.registrations {
        match r.kind.as_str() {
            "claude-statusline" => s.push_str("run      statusline setup.sh off  (restores your previous statusLine)\n"),
            _ => s.push_str(&format!("run      claude plugin uninstall {}\n", r.name)),
        }
    }
    if let Some((d, a)) = &plan.data_dir {
        match a {
            DataAction::MoveAside(x) => s.push_str(&format!("move     {}  ->  {}  (identity, memory, snapshots kept there; --delete-data to delete)\n", d.display(), x.display())),
            DataAction::Delete => s.push_str(&format!("DELETE   {}  (everything: identity, memory, snapshots)\n", d.display())),
        }
    }
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
git commit -m "uninstall: plan from the receipt (or the three installer dirs) by identity; purge moves the data dir aside; execute; report survivors"
```

---

### Task 4: `kannaka uninstall` on the command line

**Files:**
- Modify: `src/cli.rs` — subcommand after the `update` block (line 134), dispatch after `if name == "update"` (line 594), `handle_uninstall` after `handle_update`
- Modify: `src/bin/kannaka.rs` line ~1047 (the comment listing what is NOT in the fast path: add `uninstall`)

**Interfaces:**
- Consumes: `uninstall::{plan, execute, render, banner_component, Options}`, `install_receipt::{receipt_path, load_from}`
- Produces: `kannaka uninstall [--purge] [--delete-data] [--dry-run] [--yes]`; exit 0 clean, 1 when something meant to be removed survived, 2 on a receipt that cannot be read, 3 when the purge was not confirmed (a refused prompt, or no terminal and no `--yes`).

- [ ] **Step 1: Write the failing test** (in `src/cli.rs`'s `#[cfg(test)]` module; create one at the bottom if absent)

```rust
    #[test]
    fn uninstall_parses_its_four_flags() {
        let m = build_cli().get_matches_from(["kannaka", "uninstall", "--purge", "--delete-data", "--dry-run", "--yes"]);
        let (name, sub) = m.subcommand().unwrap();
        assert_eq!(name, "uninstall");
        assert!(sub.get_flag("purge") && sub.get_flag("delete-data") && sub.get_flag("dry-run") && sub.get_flag("yes"));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib cli::tests::uninstall_parses_its_four_flags`
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
                     Without a receipt, looks in the three directories the installers write\n\
                     into and removes only what identifies itself as kannaka; a kannaka\n\
                     anywhere else is named with the command that removes it.\n\n\
                     Flags:\n  \
                     --purge        also move the data dir aside (identity key, memory,\n                 \
                                    snapshots), remove the shell rc blocks, the swarm\n                 \
                                    credentials, the launcher and the Claude registrations.\n                 \
                                    System units and scheduled tasks are printed, never run.\n  \
                     --delete-data  with --purge: delete the data dir instead of moving it aside\n  \
                     --dry-run      print the plan, change nothing\n  \
                     --yes          do not ask before --purge (required when stdin is not a terminal)",
                )
                .arg(Arg::new("purge").long("purge").action(ArgAction::SetTrue).help("Also move the data dir aside and remove rc blocks, credentials, launcher and registrations"))
                .arg(Arg::new("delete-data").long("delete-data").action(ArgAction::SetTrue).help("With --purge: delete the data dir rather than moving it aside"))
                .arg(Arg::new("dry-run").long("dry-run").action(ArgAction::SetTrue).help("Print the plan and change nothing"))
                .arg(Arg::new("yes").long("yes").action(ArgAction::SetTrue).help("Do not ask before --purge")),
        )
```

After the `if name == "update" { … }` block (line 594) add:

```rust
    if name == "uninstall" {
        return handle_uninstall(
            sub_matches.get_flag("purge"),
            sub_matches.get_flag("delete-data"),
            sub_matches.get_flag("dry-run"),
            sub_matches.get_flag("yes"),
        );
    }
```

After `handle_update` add:

```rust
/// `kannaka uninstall`. Exit codes: 0 clean, 1 something meant to be
/// removed is still there, 2 unreadable receipt, 3 purge not confirmed.
fn handle_uninstall(purge: bool, delete_data: bool, dry_run: bool, yes: bool) -> Dispatch {
    use crate::{install_receipt, uninstall};
    use std::io::IsTerminal;
    let data_dir = crate::config::KannakaConfig::data_dir();
    let rpath = install_receipt::receipt_path();
    let receipt = match install_receipt::load_from(&rpath) {
        Ok(r) => r,
        Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
    };
    if receipt.is_none() {
        eprintln!("No install receipt at {} — this install predates receipts; looking in the installer directories.", rpath.display());
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let current_exe = std::env::current_exe().unwrap_or_default();
    let path_env = std::env::var("PATH").unwrap_or_default();
    let now = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let opts = uninstall::Options { purge, delete_data, dry_run };
    let plan = uninstall::plan(receipt.as_ref(), &opts, &home, &data_dir, &path_env, &current_exe, &uninstall::banner_component, &now);
    print!("{}", uninstall::render(&plan));
    if dry_run { println!("(dry run — nothing changed)"); return Dispatch::Handled; }
    if purge && !yes {
        let what = if delete_data { "DELETES" } else { "moves aside" };
        let question = format!("This {what} {} including your identity key and memory. Type 'purge' to continue: ", data_dir.display());
        if std::io::stdin().is_terminal() {
            eprint!("{question}");
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            if line.trim() != "purge" { eprintln!("not confirmed; nothing changed"); std::process::exit(3); }
        } else {
            eprintln!("{question}");
            eprintln!("stdin is not a terminal; pass --yes to confirm. Nothing changed.");
            std::process::exit(3);
        }
    }
    let run = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).status().map(|s| s.success()).unwrap_or(false);
    let report = uninstall::execute(&plan, &run);
    for p in &report.removed { println!("removed  {}", p.display()); }
    for p in &report.parked { println!("parked   {}  (was running; swept on the next kannaka run)", p.display()); }
    for (p, why) in &report.declined { println!("kept     {}  ({why})", p.display()); }
    if let Some(a) = &report.moved_aside { println!("your data is at {}  (delete it yourself when you are sure)", a.display()); }
    if !report.still_present.is_empty() {
        for p in &report.still_present { eprintln!("STILL PRESENT  {}", p.display()); }
        eprintln!("uninstall incomplete: {} item(s) could not be removed", report.still_present.len());
        std::process::exit(1);
    }
    println!("kannaka uninstalled.{}", if purge { "" } else { " Your data in the data dir and your shell rc were kept (use --purge to remove them)." });
    Dispatch::Handled
}
```

In `src/bin/kannaka.rs` extend the comment at line ~1047 so it reads `completions`, `update` and `uninstall` are intentionally NOT here.

- [ ] **Step 4: Run the tests and a real dry run**

Run: `cargo test --lib cli:: && cargo run -q -- uninstall --dry-run && echo | cargo run -q -- uninstall --purge; echo "exit=$?"`
Expected: tests pass; the dry run prints a plan for this box (no receipt yet → the three installer dirs) and ends with `(dry run — nothing changed)`; the piped `--purge` without `--yes` prints the question and exits 3 with nothing changed (`kannaka --version` still works).

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/bin/kannaka.rs
git commit -m "cli: kannaka uninstall [--purge] [--delete-data] [--dry-run] [--yes]; no terminal means no consent"
```

---

### Task 5: `kannaka update` refreshes every sibling from the manifest, and CI guards the owner

**Files:**
- Create: `src/update_components.rs`
- Modify: `src/config.rs` — `fn windows_swap_binary` → `pub(crate) fn` (line 935), `fn platform_triple` → `pub(crate) fn` (line 1567); in `self_update` replace both `update_sibling_tui(&agent, &body, tag, &current_exe, remote_version);` calls (lines 1037 and 1154) with `crate::update_components::refresh_all(&agent, &current_exe);`
- Modify: `src/lib.rs` (`pub mod update_components;`), `src/cli.rs` `update` long_about (mention kannaka-hdl and the receipt)
- Modify: `.github/workflows/ci.yml` (old-owner grep guard)

**Interfaces:**
- Consumes: `install_receipt::{Receipt, FileEntry, load_from, write_atomic, receipt_path, sha256_hex}`, `config::{windows_swap_binary, platform_triple}`, `uninstall::banner_component`
- Produces:

```rust
pub const MANIFEST_URL: &str = "https://ninja-portal.com/constellation.json";
pub const MANIFEST_FALLBACK: &str = "https://github.com/kannaka-labs/kannaka-library/releases/download/library/constellation.json";
pub const MANIFEST_PUB_URL: &str = "https://github.com/kannaka-labs/kannaka-library/releases/download/library/manifest.pub";
pub struct Pin { pub version: String, pub url: String, pub sha256: String }
pub struct Manifest { pub generated: String, pub signed: bool, pins: HashMap<String, Pin> }
impl Manifest { pub fn parse(json: &[u8], target: &str) -> Result<Manifest, String>; pub fn pin(&self, component: &str) -> Option<&Pin>; }
pub fn verify_signature(manifest_bytes: &[u8], sig_b64: &str, pub_pem: &str) -> Result<(), String>;
pub fn ed25519_pub_from_pem(pem: &str) -> Result<[u8; 32], String>;
pub fn swap_in(target: &Path, bytes: &[u8]) -> Result<(), String>;
pub fn record_refresh(r: &mut Receipt, path: &Path, bytes: &[u8], version: &str);
pub fn refresh_with(receipt_path: &Path, current_exe: &Path, manifest: Option<&Manifest>, fetch: &dyn Fn(&str) -> Result<Vec<u8>, String>, local_version: &dyn Fn(&Path) -> Option<String>) -> Result<(), String>;   // testable core
pub fn refresh_all(agent: &ureq::Agent, current_exe: &Path);   // never fails the caller; prints what it did
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
    fn refresh_touches_only_the_listed_siblings_and_the_receipt() {
        use crate::install_receipt::*;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data"); std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("node_key.ed25519"), b"IDENTITY").unwrap();
        std::fs::write(data.join("kannaka.hrm"), b"HRM").unwrap();
        let bin = dir.path().join("bin"); std::fs::create_dir_all(&bin).unwrap();
        let k = bin.join("kannaka"); let t = bin.join("kannaka-tui"); let h = bin.join("kannaka-hdl");
        std::fs::write(&k, b"engine").unwrap(); std::fs::write(&t, b"old-tui").unwrap(); std::fs::write(&h, b"old-hdl").unwrap();
        let rp = data.join(FILE_NAME);
        let r = Receipt { schema: SCHEMA, installer: "i".into(), files: vec![
            FileEntry { path: k.clone(), sha256: sha256_hex(b"engine"), component: "kannaka".into(), version: "0.17.0".into() },
            FileEntry { path: t.clone(), sha256: sha256_hex(b"old-tui"), component: "kannaka-tui".into(), version: "0.5.0".into() },
            FileEntry { path: h.clone(), sha256: sha256_hex(b"old-hdl"), component: "kannaka-hdl".into(), version: "0.11.0".into() },
        ], ..Default::default() };
        write_atomic(&rp, &r).unwrap();
        let manifest = Manifest::parse(br#"{"schema":"kannaka-constellation/1","generated":"g","components":[
          {"id":"kannaka-tui","release":{"version":"v0.5.9"},"assets":[{"url":"https://example/t","sha256":"SHA_T","target":"T"}]},
          {"id":"kannaka-hdl","release":{"version":"v0.11.0"},"assets":[{"url":"https://example/h","sha256":"SHA_H","target":"T"}]}]}"#
            .to_vec().as_slice(), "T").unwrap();
        let new_tui = b"new-tui".to_vec();
        let manifest = { let mut m = manifest; m.pins.get_mut("kannaka-tui").unwrap().sha256 = sha256_hex(&new_tui); m };
        let fetch = |url: &str| -> Result<Vec<u8>, String> { if url == "https://example/t" { Ok(new_tui.clone()) } else { Err(format!("unexpected fetch {url}")) } };
        let local_version = |p: &Path| -> Option<String> { if p == t { Some("0.5.0".into()) } else if p == h { Some("0.11.0".into()) } else { None } };
        refresh_with(&rp, &k, Some(&manifest), &fetch, &local_version).unwrap();
        assert_eq!(std::fs::read(&t).unwrap(), b"new-tui", "tui refreshed to the pin");
        assert_eq!(std::fs::read(&h).unwrap(), b"old-hdl", "hdl already at the pin: untouched, never fetched");
        assert_eq!(std::fs::read(&k).unwrap(), b"engine", "the engine is not the manifest's business here");
        let after = load_from(&rp).unwrap().unwrap();
        let tui = after.files.iter().find(|f| f.component == "kannaka-tui").unwrap();
        assert_eq!((tui.version.as_str(), tui.sha256.as_str()), ("0.5.9", sha256_hex(b"new-tui").as_str()));
        assert_eq!(std::fs::read(data.join("node_key.ed25519")).unwrap(), b"IDENTITY");
        assert_eq!(std::fs::read(data.join("kannaka.hrm")).unwrap(), b"HRM");
        assert!(!data.join("install.json.1").exists(), "update rewrites in place, never rotates");
    }

    #[test]
    fn a_sha_mismatch_against_the_manifest_leaves_the_sibling_alone() {
        use crate::install_receipt::*;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data"); std::fs::create_dir_all(&data).unwrap();
        let t = dir.path().join("kannaka-tui"); std::fs::write(&t, b"old-tui").unwrap();
        let k = dir.path().join("kannaka"); std::fs::write(&k, b"engine").unwrap();
        let rp = data.join(FILE_NAME);
        write_atomic(&rp, &Receipt { schema: SCHEMA, files: vec![FileEntry { path: t.clone(), sha256: "x".into(), component: "kannaka-tui".into(), version: "0.5.0".into() }], ..Default::default() }).unwrap();
        let manifest = Manifest::parse(br#"{"schema":"kannaka-constellation/1","generated":"g","components":[{"id":"kannaka-tui","release":{"version":"v0.5.9"},"assets":[{"url":"https://example/t","sha256":"0000","target":"T"}]}]}"#, "T").unwrap();
        let fetch = |_: &str| -> Result<Vec<u8>, String> { Ok(b"evil".to_vec()) };
        refresh_with(&rp, &k, Some(&manifest), &fetch, &|_| Some("0.5.0".into())).unwrap();
        assert_eq!(std::fs::read(&t).unwrap(), b"old-tui");
        assert_eq!(load_from(&rp).unwrap().unwrap().files[0].version, "0.5.0");
    }

    #[test]
    fn no_receipt_synthesizes_one_from_the_siblings_beside_the_engine() {
        use crate::install_receipt::*;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let k = dir.path().join(if cfg!(windows) { "kannaka.exe" } else { "kannaka" }); std::fs::write(&k, b"engine").unwrap();
        let t = dir.path().join(if cfg!(windows) { "kannaka-tui.exe" } else { "kannaka-tui" }); std::fs::write(&t, b"tui").unwrap();
        let rp = data.join(FILE_NAME);
        refresh_with(&rp, &k, None, &|u| Err(format!("no network: {u}")), &|_| Some("1.0.0".into())).unwrap();
        let r = load_from(&rp).unwrap().unwrap();
        assert!(r.installer.starts_with("kannaka update@"));
        assert_eq!(r.files.iter().map(|f| f.component.as_str()).collect::<Vec<_>>(), vec!["kannaka", "kannaka-tui"]);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib update_components::`
Expected: compile error.

- [ ] **Step 3: Implement**

`src/update_components.rs`:

```rust
//! `kannaka update` beyond the engine: refresh every sibling the receipt
//! lists (kannaka-tui, kannaka-hdl) to the version the signed constellation
//! manifest pins, verifying each download against the manifest's sha256,
//! with the same safe swap the engine uses, then rewrite the receipt in
//! place. The engine itself still follows its release channel (spec §7 rev 2).
//!
//! Spec: kannaka-plugin/docs/superpowers/specs/2026-09-09-fresh-install-update-uninstall-design.md §7.

use crate::install_receipt::{self as receipt, FileEntry, Receipt};
use std::collections::HashMap;
use std::path::Path;

pub const MANIFEST_URL: &str = "https://ninja-portal.com/constellation.json";
pub const MANIFEST_FALLBACK: &str = "https://github.com/kannaka-labs/kannaka-library/releases/download/library/constellation.json";
pub const MANIFEST_PUB_URL: &str = "https://github.com/kannaka-labs/kannaka-library/releases/download/library/manifest.pub";

pub struct Pin { pub version: String, pub url: String, pub sha256: String }

pub struct Manifest { pub generated: String, pub signed: bool, pub(crate) pins: HashMap<String, Pin> }

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

fn fetch_url(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let resp = agent.get(url).set("User-Agent", "kannaka-update").call().map_err(|e| format!("{url}: {e}"))?;
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Load the manifest, signature-checked when the signature and key can be
/// fetched; a manifest whose signature FAILS is refused (None). Missing
/// signature material means unsigned, still used: the per-asset sha256 is
/// the real guarantee, as in install.sh.
fn load_manifest(agent: &ureq::Agent, target: &str) -> Option<Manifest> {
    let (bytes, from) = match fetch_url(agent, MANIFEST_URL) { Ok(b) => (b, MANIFEST_URL), Err(_) => (fetch_url(agent, MANIFEST_FALLBACK).ok()?, MANIFEST_FALLBACK) };
    let mut m = match Manifest::parse(&bytes, target) { Ok(m) => m, Err(e) => { eprintln!("Note: {e}; siblings left as they are."); return None; } };
    if let (Ok(sig), Ok(pubk)) = (fetch_url(agent, &format!("{from}.sig")), fetch_url(agent, MANIFEST_PUB_URL)) {
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

/// The testable core: refresh every non-engine sibling the receipt lists
/// (or, with no receipt, the siblings beside `current_exe`, then write a
/// first receipt). `fetch` and `local_version` are injected.
pub fn refresh_with(receipt_path: &Path, current_exe: &Path, manifest: Option<&Manifest>,
                    fetch: &dyn Fn(&str) -> Result<Vec<u8>, String>, local_version: &dyn Fn(&Path) -> Option<String>) -> Result<(), String> {
    let (os, arch, ext) = crate::config::platform_triple();
    let target = format!("{os}-{arch}");
    let mut r = match receipt::load_from(receipt_path)? {
        Some(r) => r,
        None => synthesize_receipt(current_exe, ext, &target, local_version),
    };
    let synthesized = r.installer.starts_with("kannaka update@");
    let mut changed = false;
    for entry in r.files.clone() {
        if entry.component == "kannaka" { continue; }
        let Some(m) = manifest else { eprintln!("Note: no manifest — {} left as is.", entry.component); continue; };
        let Some(pin) = m.pin(&entry.component) else { eprintln!("Note: manifest has no {} for {target}.", entry.component); continue; };
        if local_version(&entry.path).as_deref() == Some(pin.version.as_str()) { eprintln!("{} already at v{}.", entry.component, pin.version); continue; }
        eprintln!("Downloading {} v{} (pinned)...", entry.component, pin.version);
        let bytes = match fetch(&pin.url) { Ok(b) => b, Err(e) => { eprintln!("Note: {e}"); continue } };
        let got = receipt::sha256_hex(&bytes);
        if got != pin.sha256 { eprintln!("Note: {} sha256 mismatch against the manifest (want {} got {got}) — skipped.", entry.component, pin.sha256); continue; }
        match swap_in(&entry.path, &bytes) {
            Ok(()) => { record_refresh(&mut r, &entry.path, &bytes, &pin.version); changed = true; eprintln!("{} updated to v{}.", entry.component, pin.version); }
            Err(e) => eprintln!("Note: {} not replaced: {e}", entry.component),
        }
    }
    // The engine's own entry: self_update already swapped it; record what is there now.
    if let Some(me) = r.files.iter_mut().find(|f| f.component == "kannaka") {
        if let Ok(bytes) = std::fs::read(&me.path) {
            let h = receipt::sha256_hex(&bytes);
            if h != me.sha256 { me.sha256 = h; me.version = crate::config::VERSION.to_string(); changed = true; }
        }
    }
    if changed || synthesized { receipt::write_atomic(receipt_path, &r)?; }
    Ok(())
}

/// Never fails the caller: every problem is a printed note.
pub fn refresh_all(agent: &ureq::Agent, current_exe: &Path) {
    let (os, arch, _) = crate::config::platform_triple();
    let manifest = load_manifest(agent, &format!("{os}-{arch}"));
    let fetch = |url: &str| fetch_url(agent, url);
    if let Err(e) = refresh_with(&receipt::receipt_path(), current_exe, manifest.as_ref(), &fetch, &local_version) {
        eprintln!("Note: {e}; siblings not refreshed.");
    }
}

fn local_version(path: &Path) -> Option<String> {
    crate::uninstall::banner_component(path)?;   // identity first: never run a stranger
    let out = std::process::Command::new(path).arg("--version").output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next()?.split_whitespace().nth(1).map(|s| s.trim_start_matches('v').to_string())
}

/// An install older than receipts: the engine plus whichever siblings sit
/// beside it.
fn synthesize_receipt(current_exe: &Path, ext: &str, target: &str, local_version: &dyn Fn(&Path) -> Option<String>) -> Receipt {
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

In `src/config.rs`: make `windows_swap_binary` and `platform_triple` `pub(crate)`; replace the two `update_sibling_tui(...)` calls in `self_update` with `crate::update_components::refresh_all(&agent, &current_exe);` (in the "already up to date" branch `current_exe` comes from the existing `if let Ok(current_exe) = std::env::current_exe()`); leave `update_sibling_tui` in place for `bootstrap_install_tui`, which still uses it. Add `pub mod update_components;` to `src/lib.rs`. In `src/cli.rs` the `update` long_about's "Also updates the kannaka-tui sibling binary…" sentence becomes "Then refreshes every sibling the install receipt lists (kannaka-tui, kannaka-hdl) to the version the signed constellation manifest pins, verifying each against the manifest's sha256, and rewrites the receipt."

CI guard, in `.github/workflows/ci.yml` `check` job after the `Test` step:

```yaml
      - name: no old-owner strings in src/ (the update URLs must point at kannaka-labs)
        working-directory: kannaka-memory
        run: |
          if grep -rni 'nickflach' src/ packaging/ scripts/; then echo "old owner found above"; exit 1; fi
          echo "clean"
```

- [ ] **Step 4: Run the tests and one real update**

Run: `cargo test --lib update_components:: && cargo test --lib && grep -rni nickflach src/ packaging/ scripts/ || echo clean && cargo run -q -- update`
Expected: unit tests pass; the grep is clean; the real `update` on this box prints `Manifest loaded (signed, generated …)` (or `unsigned` if the PEM layout surprises us — report it as a finding), refreshes or reports `already at` for kannaka-tui, and writes a receipt at `~/.kannaka/install.json` with `installer: "kannaka update@0.17.0"` because this box predates receipts.

- [ ] **Step 5: Commit**

```bash
git add src/update_components.rs src/config.rs src/lib.rs src/cli.rs .github/workflows/ci.yml
git commit -m "update: refresh every receipt sibling from the signed manifest, rewrite the receipt in place; CI guards the owner"
```

---

### Task 6: The npm postinstall merges into the receipt

**Files:**
- Create: `packaging/npm/receipt.js`, `packaging/npm/receipt.test.js`
- Modify: `packaging/npm/install.js` (after line 101 `console.log(\`kannaka: installed ${dest}\`)`), `packaging/npm/package.json` (`"files"` must include `receipt.js`; check with `grep -n '"files"' packaging/npm/package.json`)
- Modify: `.github/workflows/ci.yml` (node test step)

**Interfaces:**
- Produces: `mergeReceipt({ dataDir, file, installer, platform })` → loads an existing receipt if present, replaces or adds the one `files` entry with the same path, keeps every other field, writes atomically, **never rotates**; with no receipt, creates one with empty lists. Returns the receipt path.

- [ ] **Step 1: Write the failing test**

`packaging/npm/receipt.test.js`:

```js
"use strict";
const test = require("node:test");
const assert = require("node:assert");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { mergeReceipt, receiptDir } = require("./receipt");

const entry = { path: "/n/bin/kannaka-bin", sha256: "ab".repeat(32), component: "kannaka", version: "0.17.0" };

test("with no receipt, writes one with the installers' shape", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "kr-"));
  const p = mergeReceipt({ dataDir: dir, installer: "npm:kannaka@0.17.0", platform: "linux-x86_64", file: entry });
  const r = JSON.parse(fs.readFileSync(p, "utf8"));
  assert.strictEqual(r.schema, 1);
  assert.deepStrictEqual(Object.keys(r), ["schema", "installed_at", "installer", "manifest", "platform", "files", "extras", "rc_edits", "path_edits", "config_edits", "credentials", "registrations", "removed", "declined", "previous"]);
  assert.deepStrictEqual(r.files, [entry]);
  assert.ok(!fs.existsSync(path.join(dir, "install.json.1")));
});

test("merges into an existing receipt and rotates nothing", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "kr-"));
  const existing = { schema: 1, installed_at: "t", installer: "kannaka-labs/kannaka-plugin/install/install.sh@2", manifest: "latest", platform: "linux-x86_64",
    files: [{ path: "/h/.local/bin/kannaka", sha256: "cd".repeat(32), component: "kannaka", version: "0.17.0" }],
    extras: [], rc_edits: [{ file: "/h/.bashrc", sentinel: "# kannaka" }], path_edits: [], config_edits: [],
    credentials: [{ file: "/h/.kannaka-nats.env" }], registrations: [{ kind: "claude-plugin", name: "kannaka@kannaka" }], removed: [], declined: [], previous: ["install.json.1"] };
  fs.writeFileSync(path.join(dir, "install.json"), JSON.stringify(existing));
  mergeReceipt({ dataDir: dir, installer: "npm:kannaka@0.17.0", platform: "linux-x86_64", file: entry });
  const r = JSON.parse(fs.readFileSync(path.join(dir, "install.json"), "utf8"));
  assert.strictEqual(r.installer, existing.installer, "the machine's install record keeps its author");
  assert.deepStrictEqual(r.rc_edits, existing.rc_edits);
  assert.deepStrictEqual(r.credentials, existing.credentials);
  assert.deepStrictEqual(r.registrations, existing.registrations);
  assert.deepStrictEqual(r.previous, existing.previous);
  assert.strictEqual(r.files.length, 2);
  assert.ok(!fs.existsSync(path.join(dir, "install.json.1")), "npm must never rotate");
  // a second npm install of the same package replaces its own entry, not duplicates it
  mergeReceipt({ dataDir: dir, installer: "npm:kannaka@0.17.1", platform: "linux-x86_64", file: { ...entry, version: "0.17.1" } });
  const r2 = JSON.parse(fs.readFileSync(path.join(dir, "install.json"), "utf8"));
  assert.strictEqual(r2.files.length, 2);
  assert.strictEqual(r2.files.find((f) => f.path === entry.path).version, "0.17.1");
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
 * The install receipt (~/.kannaka/install.json): what was installed on this
 * machine, so `kannaka uninstall` can reverse exactly it. The npm postinstall
 * MERGES its one binary into whatever receipt exists — it never rotates, so a
 * project's `npm install` cannot push the machine's real install record off
 * the end — and writes atomically.
 */
const fs = require("fs");
const os = require("os");
const path = require("path");

function receiptDir() {
  return process.env.KANNAKA_DATA_DIR || path.join(os.homedir(), ".kannaka");
}

function emptyReceipt(installer, platform) {
  return {
    schema: 1,
    installed_at: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    installer, manifest: "latest", platform,
    files: [], extras: [], rc_edits: [], path_edits: [], config_edits: [], credentials: [], registrations: [],
    removed: [], declined: [], previous: [],
  };
}

function mergeReceipt({ dataDir, installer, platform, file }) {
  fs.mkdirSync(dataDir, { recursive: true });
  const p = path.join(dataDir, "install.json");
  let doc = null;
  try { doc = JSON.parse(fs.readFileSync(p, "utf8")); } catch (e) { doc = null; }
  if (!doc || doc.schema !== 1) doc = emptyReceipt(installer, platform);
  for (const k of ["files", "extras", "rc_edits", "path_edits", "config_edits", "credentials", "registrations", "removed", "declined", "previous"]) {
    if (!Array.isArray(doc[k])) doc[k] = [];
  }
  doc.files = doc.files.filter((f) => f.path !== file.path);
  doc.files.push(file);
  const tmp = `${p}.tmp.${process.pid}`;
  fs.writeFileSync(tmp, JSON.stringify(doc, null, 2) + "\n");
  fs.renameSync(tmp, p);
  return p;
}

module.exports = { mergeReceipt, receiptDir };
```

In `packaging/npm/install.js`, add near the top `const { mergeReceipt, receiptDir } = require("./receipt");` and after line 101 (`console.log(\`kannaka: installed ${dest}\`)`):

```js
  // The receipt: this is an install like any other, so `kannaka uninstall`
  // can find and reverse it. Merged, never rotated; written last.
  const receipt = mergeReceipt({
    dataDir: receiptDir(),
    installer: `npm:kannaka@${VERSION}`,
    platform: `${os}-${arch}`,
    file: { path: dest, sha256: crypto.createHash("sha256").update(bin).digest("hex"), component: "kannaka", version: VERSION },
  });
  console.log(`kannaka: receipt ${receipt}`);
```

Also fix the failure hint at line 108 of `install.js`: replace `curl -sSf https://install.ninja-portal.com/kannaka | sh` with `curl -fsSL https://raw.githubusercontent.com/kannaka-labs/kannaka-plugin/master/install/install.sh | sh` (the hostname serves nothing yet; §8 is Nick's decision and until then the hint must be a working address).

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
      - name: npm postinstall — receipt merge
        working-directory: kannaka-memory
        run: node --test packaging/npm/receipt.test.js && node --check packaging/npm/install.js
```

```bash
git add packaging/npm/receipt.js packaging/npm/receipt.test.js packaging/npm/install.js packaging/npm/package.json .github/workflows/ci.yml
git commit -m "npm: postinstall merges its binary into the install receipt (never rotates); failure hint points at a real address"
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

- `kannaka uninstall [--purge] [--delete-data] [--dry-run] [--yes]`. Reads the
  install receipt (`<data dir>/install.json`, written by the constellation
  installer) and reverses exactly it, removing each binary only after its
  `--version` banner proves it is kannaka, and this binary last. Without a
  receipt it looks in the three directories the installers write into and
  applies the same identity check; a kannaka anywhere else is named with the
  command that removes it and left alone. Data, shell rc blocks and credentials
  are kept unless `--purge`, which moves the data dir aside (`--delete-data`
  deletes it), asks for the word `purge` on a terminal and refuses without
  `--yes` when there is none. System units and scheduled tasks are printed,
  never run. Exits non-zero if anything meant to be removed is still there.

### Changed

- `kannaka update` now refreshes every sibling the receipt lists —
  `kannaka-tui` and `kannaka-hdl`, not only the TUI — to the version the signed
  constellation manifest pins, verifying each download against the manifest's
  sha256, and rewrites the receipt in place. An install older than receipts
  gets one. The engine itself keeps following its release channel.
- `scripts/install.sh` and `scripts/install.ps1` are forwarders to the one
  installer in `kannaka-labs/kannaka-plugin`; every published one-liner keeps
  working. The npm postinstall merges its binary into the same receipt.
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

**Spec coverage (rev 2).** §2 forwarders → Task 1; npm merges, never rotates → Task 6. §3 receipt with `extras`, `path_edits`, `declined`, both credential shapes, complete-rotate-rename under a lock → Task 2; the installer's own receipt as a fixture → Task 2 (from plan A Task 6). §4 identity check bounded to `$HOME` with a five-second cap and a leading-`v` version → Task 3 `banner_component`. §5 preserve set → Task 3's first test byte-checks the identity key and rc; `--purge` widening, data dir moved aside, outside-`$HOME` and `$HOME`-itself refusals, print-only list with the exact unit and task names → Task 3. §6 all flags, exit codes, consent on a terminal and refusal without one, self last with the Windows rename, parked files reported as parked, statusline off before plugin uninstall, launcher and Windows PATH entry reversed, legacy rc blocks → Tasks 3, 4. §7 siblings from the manifest, receipt rewritten in place, engine on its release channel, CI grep for the old owner, data dir byte-identical around a refresh → Task 5. §8 → Nick; the npm hint stops advertising the dead hostname meanwhile. §9: receipt round trip (install half in plan A; `--dry-run` = plan; uninstall leaves the preserve set; `--purge` leaves the moved-aside dir and prints the system commands) → Task 3; no-receipt fallback → Task 3; Windows self-rename → Task 3 (`#[cfg(windows)]`, run on this box per Ruling 7); mutation → Task 3; cross-repo fixture → Task 2. §10 order: Task 1 is step 1, Tasks 2-7 are step 3.

**Placeholders.** None.

**Type consistency.** `Banner = dyn Fn(&Path) -> Option<String>` is what `plan` takes and what `banner_component` is (`&uninstall::banner_component` coerces). `execute`'s `run: &dyn Fn(&str, &[&str]) -> bool` matches the closure in `handle_uninstall`. `Plan.data_dir: Option<(PathBuf, DataAction)>` is what the tests compare. `Receipt` fields used in `update_components` (`files`, `installer`) and in `uninstall` (`extras`, `path_edits`, `registrations`) exist in Task 2. `refresh_with`'s injected `fetch`/`local_version` are the two things `refresh_all` supplies from the network and the process table. `windows_swap_binary(target, new_file) -> Result<PathBuf, String>` is called with `(target, &tmp)` and its `Ok` is discarded.

**Known gaps, stated.** (1) `banner_component` reads stdout only after the child exits; a component that prints more than the pipe buffer before exiting would block — none does (one line). (2) The manifest PEM parser assumes the fixed Ed25519 SPKI layout; a key in any other encoding reads as "unsigned", never as "signed". (3) `user_path_remove` parses `reg query` output by whitespace; a PATH containing a value with the literal text `REG_` at the start is not a case Windows produces. (4) `hint_for` recognises brew and npm prefixes by path fragments; anything else gets the generic "the tool that installed it".
