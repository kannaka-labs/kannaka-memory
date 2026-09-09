# kannaka

Wave-interference (Holographic Resonance Medium) memory for AI agents — the
native `kannaka` CLI, distributed over npm.

```sh
# one-off, no install
npx kannaka --version

# or install globally
npm install -g kannaka
kannaka remember "the grid hums at 72.83Hz" --importance 0.8
kannaka recall "what frequency" --top-k 5
```

## How it works

This package ships a tiny launcher. On install, its `postinstall` step
downloads the native `kannaka` binary for your platform from the matching
[GitHub release](https://github.com/kannaka-labs/kannaka-memory/releases),
verifies its published `sha256`, and places it next to the launcher. No native
binary is bundled in the tarball, so one small package serves every platform.

Supported platforms: **linux**, **macOS**, **windows** on **x86_64** and
**aarch64** (Linux builds are static musl, so they run on any distro).

## Environment

- `KANNAKA_SKIP_DOWNLOAD=1` — skip the postinstall download (offline / CI /
  source builds). The launcher then reports the binary is missing until you
  provide one.
- `KANNAKA_DATA_DIR` — where the HRM store lives (default `~/.kannaka`).

## Alternatives

- Direct install script: `curl -sSf https://install.ninja-portal.com/kannaka | sh`
- Docker: `docker run --rm ghcr.io/kannaka-labs/kannaka --version`
- Build from source: <https://github.com/kannaka-labs/kannaka-memory>

MIT licensed. The version of this package tracks the kannaka release it installs.
