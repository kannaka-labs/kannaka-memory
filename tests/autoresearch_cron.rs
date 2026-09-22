//! Guards for `research/autoresearch-cron.sh` (#939).
//!
//! The nightly OODA cron aborted 126 nights out of 126 between 2026-05-09 and
//! 2026-09-12, invoking `cargo run --release --bin research` with stderr sent
//! to `/dev/null`.
//!
//! #939 and the first fix both blamed a cold rebuild that would not fit under
//! `MemoryMax=2200M`. That was wrong: it never reached a compile. `cargo` is at
//! `~/.cargo/bin/cargo`, on PATH only via the shell profile, and a
//! `systemd-run` scope inherits `PATH=/sbin:/bin:/usr/sbin:/usr/bin`. It exited
//! 127 — "failed to run command 'cargo': No such file or directory" — which
//! gave empty stdout, no fitness line, and a discarded explanation.
//!
//! The properties asserted here, by running the real script against a scratch
//! tree with a stubbed `cargo` and a stubbed research binary:
//!
//! 1. it runs the PREBUILT binary, and never compiles implicitly before the
//!    baseline;
//! 2. a missing or stale binary aborts loudly and says so, rather than
//!    silently triggering a build inside the cron's memory scope;
//! 3. a failed run reports the binary's stderr instead of discarding it;
//! 4. cargo is resolved by path, so a scope's stripped PATH cannot hide it,
//!    and a genuinely absent cargo is named rather than failing blank;
//! 5. a build failure is reported with cargo's OWN exit status — not `tail`'s,
//!    and not the 0 that a false `if` with no else branch returns;
//! 6. it refuses to run as root, which is how `sudo systemd-run` left 176
//!    working-tree files and 991 git objects owned by uid 0.

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
    //
    // GENERATED FROM THE ROTATION ITSELF. Hand-written, this fixture drifted:
    // it still held the pre-2026-09-22 knobs, so on a day whose slot had
    // changed the `sed` matched nothing, the script exited 0 on "param edit
    // did not take", and the tests downstream of the hypothesis stage passed
    // by never reaching it.
    let mut params = String::from("fn experiment_params() -> Params {\n    Params {\n");
    for (name, from) in rotation_knobs() {
        params.push_str(&format!("        {name}: {from},\n"));
    }
    params.push_str("    }\n}\n");
    fs::write(s.repo().join("src/bin/research.rs"), params).unwrap();
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
    // `cargo test` exports CARGO (and often CARGO_HOME) into the test process.
    // The cron has neither, and leaving them set would let find_cargo succeed
    // through a door production does not have.
    cmd.env_remove("CARGO").env_remove("CARGO_HOME");
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

/// A research stub whose fitness moves between runs, the way a knob that
/// really is wired through behaves.
fn varying_research(counter: &Path) -> String {
    format!(
        "#!/bin/sh\nn=$(cat {c} 2>/dev/null || echo 0)\nn=$((n+1))\necho $n > {c}\necho \"fitness:              0.10000$n\"\n",
        c = counter.display()
    )
}
const FAILING_RESEARCH: &str =
    "#!/bin/sh\necho 'ld: cannot open shared object file' >&2\nexit 101\n";

/// The knobs the nightly rotation sweeps, as `(name, from_value)`.
fn rotation_knobs() -> Vec<(String, String)> {
    let script = fs::read_to_string(SCRIPT).expect("autoresearch-cron.sh");
    let mut out = Vec::new();
    for line in script.lines() {
        let t = line.trim_start();
        if !t.starts_with(|c: char| c.is_ascii_digit()) || !t.contains("PARAM=") {
            continue;
        }
        let name = t
            .split("PARAM=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .map(str::to_string);
        let from = t
            .split("FROM=")
            .nth(1)
            .and_then(|s| s.split(';').next())
            .map(|s| s.trim().to_string());
        if let (Some(n), Some(f)) = (name, from) {
            out.push((n, f));
        }
    }
    out
}

/// The cross-file invariant that would have caught the dead knobs (#939
/// follow-up).
///
/// The cron edits `experiment_params()` and then measures `--level $LEVEL`.
/// But each level clones those params and overwrites some of them:
///
///     l4_params.chain_carry_strength = 0.7    (research.rs:1477)
///     l5_params.chain_carry_strength = 0.7    (research.rs:3436)
///     l5_params.dream_gravity        = 0.35   (research.rs:3460)
///
/// A rotation slot holding an overwritten knob can never move the fitness. It
/// burns a full cycle and two builds, then writes a confident `revert` for a
/// knob that was never tried — a negative result nobody measured. Two of the
/// seven slots were in that state, and it showed on 2026-09-22 as ten runs
/// across both arms all returning exactly 0.202656.
#[test]
fn no_rotation_knob_is_overwritten_by_the_level_it_is_measured_at() {
    let src = fs::read_to_string("src/bin/research.rs").expect("research.rs");
    let knobs = rotation_knobs();
    assert!(
        knobs.len() >= 7,
        "control failed: parsed {} rotation knobs, so the assertions below would be near-vacuous",
        knobs.len()
    );

    let mut dead = Vec::new();
    for (name, _) in &knobs {
        for level in ["l4", "l5"] {
            if src.contains(&format!("{level}_params.{name} =")) {
                dead.push(format!("{name} (overwritten by {level}_params)"));
            }
        }
    }
    assert!(
        dead.is_empty(),
        "these rotation knobs cannot move the fitness they are measured against:\n    {}",
        dead.join("\n    ")
    );
}

/// The other way a slot goes quietly dead: the `FROM` value drifts out of step
/// with `experiment_params()`, the `sed` matches nothing, and the script exits
/// 0 with "param edit did not take" — a skipped cycle that looks like a run.
#[test]
fn every_rotation_knob_matches_its_current_value_in_experiment_params() {
    let src = fs::read_to_string("src/bin/research.rs").expect("research.rs");
    let knobs = rotation_knobs();
    assert!(!knobs.is_empty(), "control failed: no rotation knobs parsed");

    let mut stale = Vec::new();
    for (name, from) in &knobs {
        // The same shape the script's `sed` anchors on: `<indent><name>: <from>,`
        if !src.contains(&format!("{name}: {from},")) {
            stale.push(format!("{name}: expected `{name}: {from},` in experiment_params()"));
        }
    }
    assert!(
        stale.is_empty(),
        "the rotation would sed nothing and skip the cycle for:\n    {}",
        stale.join("\n    ")
    );
}

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

/// Move the cargo stub off PATH and into a scratch `$HOME/.cargo/bin`, the way
/// rustup installs it. Returns the HOME to run with.
fn hide_cargo_in_home(s: &Scratch) -> PathBuf {
    let home = s.root.join("home");
    fs::create_dir_all(home.join(".cargo/bin")).unwrap();
    fs::rename(s.bin().join("cargo"), home.join(".cargo/bin/cargo")).unwrap();
    home
}

/// PATH with nothing of ours on it — enough to run the script, no cargo.
fn bare_path(s: &Scratch) -> String {
    format!("{}:/usr/bin:/bin", s.bin().display())
}

#[test]
fn cargo_is_found_by_path_when_a_scope_strips_it_from_path() {
    // THE 126-night failure. A systemd-run scope inherits
    // PATH=/sbin:/bin:/usr/sbin:/usr/bin, and rustup puts cargo in
    // ~/.cargo/bin, which only a login profile adds. `cargo run` exited 127
    // with "No such file or directory" and `2>/dev/null` ate the sentence.
    let s = scratch(GOOD_RESEARCH, 0);
    make_binary_stale(&s);
    let home = hide_cargo_in_home(&s);

    let code = run(
        &s,
        &[
            ("OODA_ALLOW_BUILD", "1"),
            ("PATH", &bare_path(&s)),
            ("HOME", &home.display().to_string()),
        ],
    );
    let log = s.log();

    assert!(
        !log.contains("CANNOT BUILD"),
        "cargo under $HOME/.cargo/bin must be found even when PATH lacks it:\n{log}"
    );
    assert!(
        s.cargo_calls().contains("build --release --bin research"),
        "and must actually be invoked; cargo saw: {:?}",
        s.cargo_calls()
    );
    assert_eq!(code, 0, "the cycle should then complete\n{log}");
}

#[test]
fn a_genuinely_absent_cargo_says_so_instead_of_failing_blank() {
    let s = scratch(GOOD_RESEARCH, 0);
    make_binary_stale(&s);
    fs::remove_file(s.bin().join("cargo")).unwrap();
    let empty_home = s.root.join("empty-home");
    fs::create_dir_all(&empty_home).unwrap();

    let code = run(
        &s,
        &[
            ("OODA_ALLOW_BUILD", "1"),
            ("PATH", &bare_path(&s)),
            ("HOME", &empty_home.display().to_string()),
        ],
    );
    let log = s.log();

    assert_eq!(code, 1, "a missing cargo must fail the run\n{log}");
    assert!(
        log.contains("CANNOT BUILD") && log.contains("not on PATH"),
        "the failure must name the missing tool, not print nothing:\n{log}"
    );
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
    // The first cut of this test stopped at the line above, and the message it
    // accepted said "BUILD FAILED (exit 0)" every time: `local rc=$?` sat after
    // an `if` whose false branch returns 0. Assert the number, or the report is
    // free to be wrong about the one fact it exists to carry.
    assert!(
        log.contains("BUILD FAILED (exit 101)"),
        "the reported status must be cargo's own (101 here), not the `if`'s:\n{log}"
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

/// Two arms that are bit-identical, run for run, are evidence about the
/// WIRING, not the parameter — so the cycle must say so rather than file a
/// `revert` that reads as a measured null.
#[test]
fn identical_arms_are_reported_as_a_possibly_inert_knob() {
    let s = scratch(GOOD_RESEARCH, 0); // constant fitness on every run

    let code = run(&s, &[]);
    let log = s.log();

    assert!(
        log.contains("INERT KNOB?"),
        "every run returning the same value must be flagged, not filed as a null:\n{log}"
    );
    assert!(
        log.contains("may not reach"),
        "the flag must say what it suspects and how to check it:\n{log}"
    );
    assert_eq!(code, 0, "the cycle still completes and still reverts\n{log}");
}

/// The control. A knob that genuinely moves the fitness must NOT be flagged,
/// or the warning is noise on every cycle and will be ignored by the time it
/// matters.
#[test]
fn a_knob_that_moves_the_fitness_is_not_flagged_inert() {
    let dir = std::env::temp_dir().join(format!(
        "autoresearch-ctr-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let counter = dir.join("ctr");
    let s = scratch(&varying_research(&counter), 0);

    let code = run(&s, &[]);
    let log = s.log();
    let _ = fs::remove_dir_all(&dir);

    assert!(
        !log.contains("INERT KNOB?"),
        "a fitness that changes between runs must not be flagged inert:\n{log}"
    );
    assert!(
        log.contains("baseline avg="),
        "control failed: the cycle must actually have run for the absence above to mean anything:\n{log}"
    );
    assert_eq!(code, 0, "the cycle completes\n{log}");
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
