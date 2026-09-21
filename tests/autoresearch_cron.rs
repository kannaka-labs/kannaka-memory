//! Guards for `research/autoresearch-cron.sh` (#939).
//!
//! The nightly OODA cron aborted 126 nights out of 126 between 2026-05-09 and
//! 2026-09-12. It invoked `cargo run --release --bin research` — which compiles
//! first — inside a `MemoryMax=2200M` scope on a one-core box under a 600 s
//! timeout, with the build's stderr sent to `/dev/null`. The research binary
//! itself was fine; run directly on O1 it finishes in under four minutes.
//!
//! Three properties keep that from recurring, and all three are asserted here
//! by running the real script against a scratch tree with a stubbed `cargo`
//! and a stubbed research binary:
//!
//! 1. it runs the PREBUILT binary, and never compiles implicitly before the
//!    baseline;
//! 2. a missing or stale binary aborts loudly and says so, rather than
//!    silently triggering a build inside the cron's memory scope;
//! 3. a failed run reports the binary's stderr instead of discarding it, and a
//!    failed build is detected from cargo's exit status rather than from a
//!    pipeline whose last command is `tail`.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "research/autoresearch-cron.sh";

struct Scratch {
    root: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Scratch {
    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }
    fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }
    fn cargo_calls(&self) -> String {
        fs::read_to_string(self.root.join("cargo-calls.txt")).unwrap_or_default()
    }
    /// Everything the run appended to today's autoresearch log.
    fn log(&self) -> String {
        let dir = self.logs();
        let mut out = String::new();
        if let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("autoresearch-") && name.ends_with(".log") && !name.contains("build") {
                    out.push_str(&fs::read_to_string(e.path()).unwrap_or_default());
                }
            }
        }
        out
    }
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A scratch tree that looks enough like the repo for the script to act on.
///
/// `research_body` is the stub the script should execute instead of the real
/// research binary; `cargo_exit` is what the stubbed `cargo` returns.
fn scratch(research_body: &str, cargo_exit: i32) -> Scratch {
    let root = std::env::temp_dir().join(format!(
        "autoresearch-cron-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let s = Scratch { root: root.clone() };
    fs::create_dir_all(s.repo().join("src/bin")).unwrap();
    fs::create_dir_all(s.repo().join("target/release")).unwrap();
    fs::create_dir_all(s.repo().join("research")).unwrap();
    fs::create_dir_all(s.repo().join("experiments")).unwrap();
    fs::create_dir_all(s.logs()).unwrap();
    fs::create_dir_all(s.bin()).unwrap();

    // Every knob the nightly rotation can pick, at its FROM value, so the test
    // does not depend on today's day-of-year.
    fs::write(
        s.repo().join("src/bin/research.rs"),
        "fn experiment_params() -> Params {\n    Params {\n        \
         kuramoto_steps: 20,\n        kuramoto_threshold: 0.35,\n        \
         prune_threshold: 0.095,\n        constructive_boost: 0.45,\n        \
         destructive_penalty: 0.35,\n        chain_carry_strength: 0.5,\n        \
         dream_gravity: 0.0,\n    }\n}\n",
    )
    .unwrap();
    fs::write(s.repo().join("Cargo.toml"), "[package]\nname = \"scratch\"\n").unwrap();
    fs::write(s.repo().join("Cargo.lock"), "# scratch\n").unwrap();
    fs::write(s.repo().join("experiments/ooda-state.json"), "{\"level\": 4}\n").unwrap();

    write_exec(&s.repo().join("target/release/research"), research_body);

    write_exec(
        &s.bin().join("cargo"),
        &format!(
            "#!/bin/sh\necho \"$@\" >> \"{}/cargo-calls.txt\"\nexit {}\n",
            root.display(),
            cargo_exit
        ),
    );

    // A real repo, so the script's branch/commit/checkout steps behave.
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(s.repo())
            .output()
            .expect("git");
    };
    git(&["init", "-q", "-b", "master"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "scratch"]);

    // The binary must be newer than the sources unless a test wants it stale.
    touch_binary_now(&s);
    s
}

fn touch_binary_now(s: &Scratch) {
    // `find -newer` compares mtimes, so make the binary the newest thing here.
    Command::new("sh")
        .arg("-c")
        .arg("sleep 1; touch target/release/research")
        .current_dir(s.repo())
        .output()
        .expect("touch");
}

fn make_binary_stale(s: &Scratch) {
    Command::new("sh")
        .arg("-c")
        .arg("touch -d '2020-01-01' target/release/research")
        .current_dir(s.repo())
        .output()
        .expect("touch");
}

fn run(s: &Scratch, extra_env: &[(&str, &str)]) -> i32 {
    let script = fs::canonicalize(SCRIPT).expect("autoresearch-cron.sh");
    let path = format!(
        "{}:{}",
        s.bin().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg(&script)
        .env("PATH", path)
        .env("AUTORESEARCH_REPO", s.repo())
        .env("AUTORESEARCH_LOG_DIR", s.logs())
        .env("OODA_RUNS", "1")
        .env("OODA_MAX_RUNTIME", "120")
        .env("OODA_RUN_TIMEOUT", "30")
        .env("OODA_BUILD_TIMEOUT", "30")
        .env("OODA_LEVEL", "4");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("run cron").status.code().unwrap_or(-1)
}

/// Put a stub `id` on PATH so the root-refusal branch can be exercised without
/// a root runner. The script resolves `id` on PATH for exactly this reason.
fn stub_id_as_root(s: &Scratch) {
    write_exec(
        &s.bin().join("id"),
        "#!/bin/sh\nif [ \"$1\" = \"-u\" ]; then echo 0; else /usr/bin/id \"$@\"; fi\n",
    );
}

const GOOD_RESEARCH: &str = "#!/bin/sh\necho 'fitness:              0.130145'\n";
const FAILING_RESEARCH: &str =
    "#!/bin/sh\necho 'ld: cannot open shared object file' >&2\nexit 101\n";

#[test]
fn running_as_root_is_refused_before_the_checkout_is_touched() {
    // O1's crontab reached this script through `sudo systemd-run --scope`, so
    // it ran as uid 0 and its `git reset --hard` rewrote an opc-owned checkout
    // as root — 176 working-tree files and 991 objects under .git by the time
    // a plain `git pull` started failing with "Permission denied".
    let s = scratch(GOOD_RESEARCH, 0);
    stub_id_as_root(&s);

    let code = run(&s, &[]);
    let log = s.log();

    assert_eq!(code, 1, "running as root must abort\n{log}");
    assert!(
        log.contains("REFUSING to run as root"),
        "the refusal must name the problem:\n{log}"
    );
    assert!(
        log.contains("--uid="),
        "and must name the fix, since the memory cap is why sudo was there:\n{log}"
    );
    assert!(
        !log.contains("--- baseline ---"),
        "it must refuse BEFORE doing any work on the checkout:\n{log}"
    );
}

#[test]
fn an_explicitly_root_owned_checkout_may_opt_in() {
    // A root-owned checkout is a legitimate configuration; the guard is about
    // the accident, not the arrangement. Without this, the refusal would be
    // untestable in the direction that matters — that it can be turned off.
    let s = scratch(GOOD_RESEARCH, 0);
    stub_id_as_root(&s);

    let code = run(&s, &[("OODA_ALLOW_ROOT", "1")]);
    let log = s.log();

    assert!(
        !log.contains("REFUSING to run as root"),
        "OODA_ALLOW_ROOT=1 must lift the refusal:\n{log}"
    );
    assert_eq!(code, 0, "and the cycle should then run normally\n{log}");
}

#[test]
fn a_stale_binary_aborts_loudly_instead_of_building_inside_the_cron() {
    let s = scratch(GOOD_RESEARCH, 0);
    make_binary_stale(&s);

    let code = run(&s, &[]);
    let log = s.log();

    assert_eq!(code, 1, "a stale binary must abort, not proceed\n{log}");
    assert!(
        log.contains("ABORTING") && log.contains("older than its sources"),
        "the abort must say WHY, naming the staleness:\n{log}"
    );
    assert!(
        log.contains("cargo build --release --bin research"),
        "the abort must tell the operator how to fix it:\n{log}"
    );
    assert_eq!(
        s.cargo_calls(),
        "",
        "the whole point of #939: the cron must not compile on its own"
    );
}

#[test]
fn a_missing_binary_aborts_and_names_the_missing_path() {
    let s = scratch(GOOD_RESEARCH, 0);
    fs::remove_file(s.repo().join("target/release/research")).unwrap();

    let code = run(&s, &[]);
    let log = s.log();

    assert_eq!(code, 1, "a missing binary must abort\n{log}");
    assert!(
        log.contains("is missing"),
        "the abort must say the binary is missing:\n{log}"
    );
    assert_eq!(s.cargo_calls(), "", "still no implicit build");
}

#[test]
fn an_opt_in_build_is_budgeted_and_its_failure_is_detected() {
    // `if ! cargo build ... | tail -5` tested tail's exit status, so a failed
    // build read as a successful one. This asserts the status comes from cargo.
    let s = scratch(GOOD_RESEARCH, 101);
    make_binary_stale(&s);

    let code = run(&s, &[("OODA_ALLOW_BUILD", "1")]);
    let log = s.log();

    assert_eq!(code, 1, "a failed build must fail the run\n{log}");
    assert!(
        log.contains("BUILD FAILED"),
        "a failing cargo must be reported as a failure:\n{log}"
    );
    assert!(
        s.cargo_calls().contains("build --release --bin research"),
        "the opt-in path must actually build; cargo saw: {:?}",
        s.cargo_calls()
    );
}

#[test]
fn a_failed_run_reports_the_stderr_the_old_script_discarded() {
    let s = scratch(FAILING_RESEARCH, 0);

    let code = run(&s, &[]);
    let log = s.log();

    assert_eq!(code, 1, "no usable baseline must still abort\n{log}");
    assert!(
        log.contains("cannot open shared object file"),
        "the binary's own stderr is the diagnosis #939 asked for:\n{log}"
    );
    assert!(
        log.contains("all baseline runs failed"),
        "and the abort itself is unchanged:\n{log}"
    );
}

#[test]
fn a_fresh_binary_runs_the_baseline_without_compiling() {
    let s = scratch(GOOD_RESEARCH, 0);

    let code = run(&s, &[]);
    let log = s.log();

    assert!(
        log.contains("baseline avg=.130145") || log.contains("baseline avg=0.130145"),
        "the prebuilt binary's fitness must be used:\n{log}"
    );
    let calls = s.cargo_calls();
    assert!(
        !calls.contains("run "),
        "the baseline must never `cargo run`; cargo saw: {calls:?}"
    );
    // The hypothesis stage edits source, so exactly one build is expected —
    // after the baseline, not before it.
    assert_eq!(
        calls.matches("build --release --bin research").count(),
        1,
        "one deliberate build for the param change, no more; cargo saw: {calls:?}"
    );
    assert_eq!(code, 0, "a complete cycle should exit clean\n{log}");
}
