#!/usr/bin/env node
/**
 * Postinstall: fetch the platform-matched native `kannaka` binary from the
 * GitHub release that matches this package's version, verify its published
 * sha256, and drop it next to the launcher. No native binary is shipped inside
 * the npm tarball — it is downloaded here so one small package serves every
 * platform.
 *
 * Env:
 *   KANNAKA_SKIP_DOWNLOAD=1   skip the download (CI / offline / source builds)
 */
"use strict";

const fs = require("fs");
const path = require("path");
const https = require("https");
const crypto = require("crypto");

const pkg = require("./package.json");
const VERSION = pkg.version;
const REPO = "kannaka-labs/kannaka-memory";

const OS_MAP = { linux: "linux", darwin: "macos", win32: "windows" };
const ARCH_MAP = { x64: "x86_64", arm64: "aarch64" };

function target() {
  const os = OS_MAP[process.platform];
  const arch = ARCH_MAP[process.arch];
  if (!os || !arch) {
    throw new Error(
      `unsupported platform ${process.platform}/${process.arch}. Prebuilt ` +
        `kannaka binaries exist for linux/macos/windows on x86_64/aarch64. ` +
        `Build from source: https://github.com/${REPO}`,
    );
  }
  const ext = process.platform === "win32" ? ".exe" : "";
  return { os, arch, ext };
}

/** GET a URL, following GitHub's redirect to the asset CDN, resolving to a Buffer. */
function get(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error("too many redirects"));
    https
      .get(url, { headers: { "User-Agent": "kannaka-npm-installer" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          resolve(get(res.headers.location, redirects + 1));
          return;
        }
        if (res.statusCode !== 200) {
          res.resume();
          reject(new Error(`HTTP ${res.statusCode} for ${url}`));
          return;
        }
        const chunks = [];
        let done = false;
        const settle = () => { if (!done) { done = true; resolve(Buffer.concat(chunks)); } };
        const fail = (e) => { if (!done) { done = true; reject(e); } };
        res.on("data", (c) => chunks.push(c));
        res.on("end", settle);
        res.on("close", settle);
        res.on("error", fail);
      })
      .on("error", reject);
  });
}

async function main() {
  if (process.env.KANNAKA_SKIP_DOWNLOAD === "1") {
    console.log("kannaka: KANNAKA_SKIP_DOWNLOAD=1 — skipping binary download.");
    return;
  }

  const { os, arch, ext } = target();
  const asset = `kannaka-${os}-${arch}${ext}`;
  const base = `https://github.com/${REPO}/releases/download/v${VERSION}`;
  const binDir = path.join(__dirname, "bin");
  fs.mkdirSync(binDir, { recursive: true });
  const dest = path.join(binDir, `kannaka-bin${ext}`);

  console.log(`kannaka: downloading ${asset} (v${VERSION})…`);
  const bin = await get(`${base}/${asset}`);

  // Verify against the published <asset>.sha256 (format: "<hex>  <name>" or bare hex).
  try {
    const shaText = (await get(`${base}/${asset}.sha256`)).toString("utf8").trim();
    const expected = shaText.split(/\s+/)[0].toLowerCase();
    const actual = crypto.createHash("sha256").update(bin).digest("hex");
    if (expected && expected !== actual) {
      throw new Error(`sha256 mismatch for ${asset}: expected ${expected}, got ${actual}`);
    }
    console.log("kannaka: sha256 verified.");
  } catch (e) {
    if (String(e && e.message).includes("mismatch")) throw e;
    console.warn(`kannaka: could not verify sha256 (${e && e.message}); proceeding.`);
  }

  fs.writeFileSync(dest, bin);
  if (process.platform !== "win32") fs.chmodSync(dest, 0o755);
  console.log(`kannaka: installed ${dest}`);
}

main().catch((e) => {
  console.error(`kannaka: install failed: ${e && e.message}`);
  console.error(
    "You can retry with network access, or install the binary directly:\n" +
      "  curl -sSf https://install.ninja-portal.com/kannaka | sh",
  );
  process.exit(1);
});
