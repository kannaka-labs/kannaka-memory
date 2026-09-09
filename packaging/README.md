# Packaging

Distribution wrappers for the `kannaka` CLI. Each fetches the platform-matched
binary from the GitHub release (assets `kannaka-{linux,macos,windows}-{x86_64,aarch64}`)
— nothing here rebuilds the binary.

| Path | Channel | Status |
|------|---------|--------|
| [`npm/`](npm/) | `npx kannaka` / `npm i -g kannaka` | verified locally (download + sha256 + launch → 0.11.1) |
| [`docker/Dockerfile`](docker/Dockerfile) | container image | written; build-verify pending a running Docker daemon |

The existing one-line installers live in [`../scripts/install.sh`](../scripts/install.sh)
and [`../scripts/install.ps1`](../scripts/install.ps1).

## npm

The `npm/` package is a launcher + a `postinstall` that downloads the release
binary for the host platform, verifies its `sha256`, and drops it beside the
launcher. Keep `npm/package.json` `version` equal to the kannaka release tag it
should install (`vX.Y.Z`).

**Publish (owner action — outward/irreversible, so left for a human):**

```sh
cd packaging/npm
# ensure version == the kannaka release you want it to install
npm publish --access public      # needs `npm login` as the package owner
```

To automate: add an `npm-publish` job to `.github/workflows/release.yml` that,
on a `v*` tag, sets `npm/package.json` version to the tag and runs
`npm publish` with an `NPM_TOKEN` repo secret.

## Docker

```sh
docker build -t kannaka --build-arg VERSION=0.11.1 -f packaging/docker/Dockerfile .
docker run --rm kannaka --version
docker run --rm -v kannaka-data:/data kannaka remember "hello" --importance 0.8
```

Multi-arch + push to GHCR:

```sh
docker buildx build --platform linux/amd64,linux/arm64 \
  -t ghcr.io/kannaka-labs/kannaka:0.11.1 -t ghcr.io/kannaka-labs/kannaka:latest \
  --build-arg VERSION=0.11.1 -f packaging/docker/Dockerfile --push .
```

## Homebrew

Tap: [`kannaka-labs/homebrew-kannaka`](https://github.com/kannaka-labs/homebrew-kannaka)

```sh
brew install kannaka-labs/kannaka/kannaka
```

Binary formula (macOS + Linux, arm64 + x86_64) with sha256 digests pinned from
the release sidecars. The `brew-bump` job in
[`publish-channels.yml`](../.github/workflows/publish-channels.yml) regenerates
the formula on every release — it needs a `TAP_PUSH_TOKEN` repo secret (PAT
with contents:write on the tap) and skips cleanly without one.
