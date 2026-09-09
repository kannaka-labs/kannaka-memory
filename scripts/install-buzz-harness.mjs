#!/usr/bin/env node
/**
 * install-buzz-harness — register `kannaka-acp` as a Buzz ACP harness.
 *
 * ADR-0052 Tier 1 ("the tune"): Kannaka capability enters the Buzz desktop
 * client through upstream's BYOH seam (block/buzz#2773) as a JSON definition
 * dropped into `<app-data>/custom_harnesses/<id>.json`. No fork, no patch,
 * no merge surface — this is configuration, not code.
 *
 *   node scripts/install-buzz-harness.mjs [options]
 *
 *   --identifier <id>   Tauri bundle identifier, which selects the app-data
 *                       directory. Defaults to stock Buzz. A Kannaka-branded
 *                       build (ADR-0052 Tier 2) uses its own identifier so it
 *                       installs alongside stock Buzz — point this at that one
 *                       to register the harness in the Kannaka build instead.
 *   --command <path>    Absolute path to the kannaka-acp binary.
 *   --buzz-cli <path>   Absolute path to the bundled `buzz` CLI.
 *   --top-k <n>         Recall breadth passed to kannaka-acp (default 5).
 *   --dry-run           Print the resolved plan; write nothing.
 *   --force             Overwrite an existing definition without prompting.
 *
 * Verified against desktop/src-tauri/src/managed_agents/custom_harnesses.rs
 * at block/buzz main (2026-07-29).
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync, renameSync } from "node:fs";
import { homedir, platform, tmpdir } from "node:os";
import { join, isAbsolute } from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, "..");

// Stock Buzz. A Kannaka build overrides this via --identifier.
const DEFAULT_IDENTIFIER = "xyz.block.buzz.app";
const HARNESS_ID = "kannaka";

/**
 * IDs reserved for the compiled-in catalog (tier-1 runtimes + PRESET_HARNESSES).
 * A colliding definition is rejected by the loader, so we fail early and say why.
 */
const RESERVED_IDS = new Set([
  "amp", "buzz-agent", "claude", "codex", "cursor", "goose",
  "grok", "hermes", "kimi", "omp", "openclaw", "opencode",
]);

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(`--${name}`);
const opt = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i === -1 || i === argv.length - 1 ? fallback : argv[i + 1];
};
const die = (msg) => { console.error(`error: ${msg}`); process.exit(1); };

/**
 * Tauri v2 `app_data_dir()` = platform data dir + bundle identifier. This must
 * track `app.path().app_data_dir()` in agent_discovery.rs — note it is the DATA
 * dir, not the config dir, which differ on Linux (~/.local/share vs ~/.config).
 */
function appDataDir(identifier) {
  const home = homedir();
  switch (platform()) {
    case "win32": {
      const roaming = process.env.APPDATA || join(home, "AppData", "Roaming");
      return join(roaming, identifier);
    }
    case "darwin":
      return join(home, "Library", "Application Support", identifier);
    default: {
      const xdg = process.env.XDG_DATA_HOME || join(home, ".local", "share");
      return join(xdg, identifier);
    }
  }
}

/** Locate an executable on PATH without a shell. */
function which(bin) {
  const probe = platform() === "win32" ? "where" : "which";
  try {
    const out = execFileSync(probe, [bin], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
    const first = out.split(/\r?\n/).find((l) => l.trim());
    return first ? first.trim() : null;
  } catch {
    return null;
  }
}

/**
 * Resolve kannaka-acp. Prefer an INSTALLED binary (PATH, i.e. `cargo install`
 * into ~/.cargo/bin) over this repo's `target/release` build.
 *
 * The registration is durable but the dev artifact is not — `cargo clean`
 * deletes `target/release`, which would leave a harness definition pointing at
 * a missing binary. The repo build is a development fallback only; pass
 * --command to override either way.
 *
 * The definition must carry an ABSOLUTE path: Buzz inherits the PATH of
 * whatever launched it, and a PATH edit does not reach an already-running
 * process, so a bare command name resolves inconsistently.
 */
function resolveAgent() {
  const onPath = which("kannaka-acp");
  if (onPath) return onPath;
  const exe = platform() === "win32" ? "kannaka-acp.exe" : "kannaka-acp";
  const local = join(REPO, "target", "release", exe);
  if (existsSync(local)) return local;
  return null;
}

/**
 * Resolve the `buzz` CLI. Buzz Desktop bundles it, and kannaka-acp needs it to
 * post replies into channels — buzz-acp itself never publishes agent text.
 * Absent, the agent streams only, which is correct for the desktop gallery
 * (it renders agent_message_chunk itself, so posting would double the answer).
 */
function resolveBuzzCli() {
  if (platform() === "win32") {
    const local = process.env.LOCALAPPDATA || join(homedir(), "AppData", "Local");
    const bundled = join(local, "Buzz", "buzz.exe");
    if (existsSync(bundled)) return bundled;
  }
  return which("buzz");
}

/** Mirror of validate_harness_definition in custom_harnesses.rs. */
function validate(def) {
  if (!/^[a-z0-9_][a-z0-9_-]*$/.test(def.id)) {
    die(`id ${JSON.stringify(def.id)} does not match [a-z0-9_][a-z0-9_-]*`);
  }
  if (RESERVED_IDS.has(def.id.toLowerCase())) {
    die(`id ${JSON.stringify(def.id)} is reserved for a built-in harness`);
  }
  if (!def.command.trim()) die("command must not be empty");
  if (!def.label.trim()) die("label must not be empty");
  // args ride the comma-delimited BUZZ_ACP_AGENT_ARGS transport (clap
  // value_delimiter = ','), so a literal comma silently splits into two args.
  const bad = def.args.find((a) => a.includes(","));
  if (bad !== undefined) die(`arg ${JSON.stringify(bad)} contains a comma, which would split at spawn`);
  if (!isAbsolute(def.command)) {
    console.warn(`warning: command ${def.command} is not absolute; Buzz may not resolve it`);
  }
}

const identifier = opt("identifier", DEFAULT_IDENTIFIER);
const topK = opt("top-k", "5");
if (!/^\d+$/.test(topK)) die(`--top-k must be a positive integer, got ${JSON.stringify(topK)}`);

const command = opt("command", resolveAgent());
if (!command) {
  die(
    "could not find kannaka-acp.\n" +
    "  install it:  cargo install --path . --bin kannaka-acp\n" +
    "  or pass:     --command <absolute-path>",
  );
}
if (!existsSync(command)) die(`kannaka-acp not found at ${command}`);

const buzzCli = opt("buzz-cli", resolveBuzzCli());

// `env` is applied first and LOSES to Buzz-injected vars on conflict.
// BUZZ_CLI is not one of the reserved BUZZ_* keys, so it survives.
const env = {};
if (buzzCli) env.BUZZ_CLI = buzzCli;

const definition = {
  id: HARNESS_ID,
  label: "Kannaka",
  command,
  args: ["--top-k", topK],
  env,
  installInstructionsUrl: "https://github.com/kannaka-labs/kannaka-plugin",
  installHint: "cargo install --path . --bin kannaka-acp, then re-run scripts/install-buzz-harness.mjs",
};

validate(definition);

const dir = join(appDataDir(identifier), "custom_harnesses");
const target = join(dir, `${HARNESS_ID}.json`);
const body = `${JSON.stringify(definition, null, 2)}\n`;

console.log(`identifier : ${identifier}`);
console.log(`app data   : ${appDataDir(identifier)}`);
console.log(`command    : ${command}`);
console.log(`buzz CLI   : ${buzzCli ?? "(not found — agent will stream only, no channel replies)"}`);
console.log(`target     : ${target}`);

if (flag("dry-run")) {
  console.log(`\n--- ${HARNESS_ID}.json (dry run, nothing written) ---\n${body}`);
  process.exit(0);
}

if (!existsSync(appDataDir(identifier))) {
  console.warn(
    `\nwarning: ${appDataDir(identifier)} does not exist.\n` +
    "  Buzz creates it on first launch. Launch the app once, then re-run.\n" +
    "  Writing anyway — the directory will be created.",
  );
}

if (existsSync(target) && !flag("force")) {
  const current = readFileSync(target, "utf8");
  if (current === body) {
    console.log("\nalready up to date; nothing to do.");
    process.exit(0);
  }
  die(`${target} already exists and differs. Re-run with --force to overwrite.`);
}

mkdirSync(dir, { recursive: true });
// Write via a temp file in the destination dir, then rename, so a crash can
// never leave a half-written definition the loader would warn on and skip.
const tmp = join(dir, `.${HARNESS_ID}.json.tmp`);
writeFileSync(tmp, body, { mode: 0o600 });
renameSync(tmp, target);

console.log(`\nwrote ${target}`);
console.log(
  "\nThe harness list is a cached React Query (staleTime 60s) with no rescan button.\n" +
  "Revisit Agents after a minute, or restart Buzz — startup re-warms the registry.",
);
