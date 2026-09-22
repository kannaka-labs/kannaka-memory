//! Atomic file writes — ONE implementation (#777).
//!
//! The write-to-temp-then-rename pattern existed as five private copies, and
//! the copies had already diverged in ways that mattered:
//!
//!   - `hrm_store`'s copy used `std::fs::write` with NO `sync_all()`, so a
//!     crash between OS-level write completion and disk flush could silently
//!     lose sidecar state (links.json, reactivation) even though the rename
//!     appeared to succeed;
//!   - the same copy named its temp file `path.with_extension("tmp.<pid>")`,
//!     which for a dotless basename (`links`) REPLACES the name rather than
//!     creating a sibling — every other copy used a UUID-named sibling, which
//!     is always correct;
//!   - return types disagreed (`Result` vs silent `()`), so callers could not
//!     be moved between copies without behavior change.
//!
//! "Matching X" comments pointing between the copies were the tell: a comment
//! that says two functions must stay in sync is a defect locator. Now they
//! cannot drift, because there is one function to drift from.

use std::path::Path;

/// Write `bytes` to `path` atomically: UUID-named temp sibling in the same
/// directory (same-filesystem rename is an atomic swap) → `write_all` →
/// `sync_all` → rename over the target. A reader always sees either the old
/// or the new file whole; a crash never leaves a truncated target.
///
/// The temp file is removed on every failure path.
pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_write_bytes_mode(path, bytes, None)
}

/// The UUID-named temp sibling for `path`: always in the SAME directory, so
/// the final rename is a same-filesystem atomic swap and never a copy.
pub(crate) fn temp_sibling(path: &Path) -> std::path::PathBuf {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    dir.join(format!(".kannaka-tmp-{}", uuid::Uuid::new_v4()))
}

/// `atomic_write_bytes` with an optional unix permission mode. On unix the
/// temp file is CREATED with that mode (`OpenOptions::mode`, `create_new`),
/// so a secret written as `Some(0o600)` is never world-readable for even an
/// instant — there is no create-then-chmod window — and the rename carries
/// the mode onto the target (ADR-0059 §1: "0600 from creation"). coding_tools
/// writes agent-visible files as `Some(0o644)`. On non-unix the mode is
/// ignored.
pub(crate) fn atomic_write_bytes_mode(
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
) -> Result<(), String> {
    let tmp = write_temp_sibling(path, bytes, mode)?;
    commit_temp_sibling(&tmp, path)
}

/// Stage `bytes` in the temp sibling for `path` and return the temp path:
/// `create_new` with `mode` on unix, `write_all`, `sync_all`, then re-assert
/// `mode`. The file is removed on every failure path; on success the caller
/// owns it and must either `commit_temp_sibling` or delete it.
///
/// Split out from `atomic_write_bytes_mode` (#933) so the mode is OBSERVABLE
/// before the rename. Statting the final file proves only that the mode is
/// right once the write has finished — the create-then-chmod implementation
/// this replaced passes that assertion unchanged, world-readable window and
/// all. The claim being made is "0600 from creation", and the only place it
/// can be checked is here.
pub(crate) fn write_temp_sibling(
    path: &Path,
    bytes: &[u8],
    #[cfg_attr(not(unix), allow(unused_variables))] mode: Option<u32>,
) -> Result<std::path::PathBuf, String> {
    let tmp = temp_sibling(path);
    let dir = tmp.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(m) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(m);
        }
        let mut f = opts.open(&tmp).map_err(|e| format!("temp create: {e}"))?;
        if let Err(e) = f.write_all(bytes) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write: {e}"));
        }
        if let Err(e) = f.sync_all() {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("sync: {e}"));
        }
    }
    // The umask can only CLEAR bits from the creation mode, so re-assert the
    // exact mode once the file exists (a 0o644 request under umask 077 would
    // otherwise land as 0o600). The file was never wider than `mode`.
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(m)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("chmod: {e}"));
        }
    }
    Ok(tmp)
}

/// Attempts made at the final rename, and the pause between them (#934 P4).
const RENAME_ATTEMPTS: usize = 3;
const RENAME_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// Is this rename failure worth another try?
///
/// On Windows `std::fs::rename` is `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING`, which fails with access-denied while ANOTHER
/// process holds the target open without `FILE_SHARE_DELETE` — something
/// antivirus scanners and non-Rust readers do routinely and briefly. Before
/// #933 the plain `std::fs::write` silently succeeded; failing loudly is the
/// right direction, but it introduced a new transient failure:
///
///     failed to write ...: rename: Access is denied. (os error 5)
///
/// Only `PermissionDenied` retries. `NotFound`, `CrossesDevices` and the rest
/// are permanent — retrying them would just delay an honest error. A file that
/// is genuinely held still fails, 100 ms later than before.
fn rename_is_retryable(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}

/// fsync the directory that now holds the renamed entry (#934 P2).
///
/// `sync_all()` on the temp file durably lands its DATA, but the directory
/// entry the rename creates is metadata: without this, a power cut can leave
/// the old name pointing at the old contents. Unix only — Windows has no
/// directory handle to flush this way.
///
/// Best-effort: the data is already durable and the rename already succeeded,
/// so a failure here is weaker than what the caller had before #933, not
/// stronger. Refusing a completed write over it would be the worse trade.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Ok(f) = std::fs::File::open(dir) {
            let _ = f.sync_all();
        }
    }
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) {}

/// Rename a staged temp sibling over its target. The temp file is removed if
/// the rename fails, so a failed write never leaves litter behind.
fn commit_temp_sibling(tmp: &Path, path: &Path) -> Result<(), String> {
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..RENAME_ATTEMPTS {
        match std::fs::rename(tmp, path) {
            Ok(()) => {
                sync_parent_dir(path);
                return Ok(());
            }
            Err(e) => {
                if attempt + 1 < RENAME_ATTEMPTS && rename_is_retryable(&e) {
                    std::thread::sleep(RENAME_RETRY_DELAY);
                    last = Some(e);
                    continue;
                }
                last = Some(e);
                break;
            }
        }
    }
    let _ = std::fs::remove_file(tmp);
    Err(format!(
        "rename: {}",
        last.map(|e| e.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    ))
}

/// Age past which an orphaned temp sibling is litter (#934 P3).
const TEMP_LITTER_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Remove `.kannaka-tmp-*` files in `dir` older than an hour.
///
/// Every temp sibling is a fresh `Uuid::new_v4()`, so a leftover can never
/// collide with the `create_new` of a later write — the "fails forever" case
/// does not exist. What does exist is that a hard kill between create and
/// rename leaves the file behind and nothing ever reclaims it, so `~/.kannaka`
/// accumulates them indefinitely.
///
/// The age floor is what makes this safe to call while other writes are in
/// flight: a temp file belonging to a live write is seconds old, never an
/// hour. A file whose age cannot be read is left alone — not knowing how old
/// something is, is not a reason to delete it.
pub(crate) fn sweep_temp_litter(dir: &Path) -> usize {
    sweep_temp_litter_older_than(dir, TEMP_LITTER_MAX_AGE)
}

/// The body of [`sweep_temp_litter`], with the age floor injectable.
///
/// Split out so the two rules can be tested independently without touching
/// the clock: `Duration::ZERO` exercises the NAME filter (everything matching
/// goes, everything else stays), and the real hour exercises the AGE filter
/// (nothing recent goes). Backdating an mtime would need a new dependency to
/// assert what two calls already prove.
fn sweep_temp_litter_older_than(dir: &Path, max_age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(".kannaka-tmp-") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue; // a future mtime: clock skew, not litter
        };
        if age >= max_age && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("kannaka-fsutil-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_and_overwrites_whole_files() {
        let d = temp_dir("roundtrip");
        let p = d.join("state.json");
        atomic_write_bytes(&p, b"first").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"first");
        atomic_write_bytes(&p, b"second, longer than first").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"second, longer than first");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The hrm_store copy's `with_extension("tmp.<pid>")` REPLACED a dotless
    /// basename instead of creating a sibling. The unified temp naming must
    /// handle `links` (no dot) exactly like `links.json`.
    #[test]
    fn dotless_basenames_are_safe() {
        let d = temp_dir("dotless");
        let p = d.join("links");
        atomic_write_bytes(&p, b"graph").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"graph");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// No stray temp files after a successful write — the rename consumed it.
    #[test]
    fn leaves_no_temp_litter() {
        let d = temp_dir("litter");
        atomic_write_bytes(&d.join("a.bin"), &[0u8; 128]).unwrap();
        let stray: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".kannaka-tmp-"))
            .collect();
        assert!(stray.is_empty(), "temp files left behind: {stray:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_parent_directories_are_created() {
        let d = temp_dir("mkdirs");
        let p = d.join("a").join("b").join("c.json");
        atomic_write_bytes(&p, b"deep").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"deep");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #930: the temp file must be a sibling of the target (same directory,
    /// hence same filesystem) — a temp in the system temp dir would turn the
    /// rename into a copy and lose atomicity.
    #[test]
    fn temp_file_is_a_sibling_of_the_target() {
        let d = temp_dir("sibling");
        let p = d.join("sub").join("config.toml");
        let t = temp_sibling(&p);
        assert_eq!(t.parent(), p.parent(), "temp must live next to the target: {t:?}");
        assert!(t.file_name().unwrap().to_string_lossy().starts_with(".kannaka-tmp-"));
        // Two calls never collide.
        assert_ne!(t, temp_sibling(&p));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #930: a write that fails at the rename must leave the existing target
    /// exactly as it was and remove its temp file. The failure is forced by
    /// making the target a directory: `rename(file, dir)` fails on every
    /// platform, after the temp file has been fully written and synced.
    #[test]
    fn failed_rename_leaves_the_old_target_intact() {
        let d = temp_dir("keepold");
        let target = d.join("config.toml");
        std::fs::create_dir_all(&target).unwrap();
        let marker = target.join("old-contents");
        std::fs::write(&marker, b"the previous file").unwrap();

        let err = atomic_write_bytes(&target, b"new contents").unwrap_err();
        assert!(err.starts_with("rename:"), "expected the rename to fail, got: {err}");

        assert!(target.is_dir(), "target must be untouched by a failed write");
        assert_eq!(std::fs::read(&marker).unwrap(), b"the previous file");
        let stray: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".kannaka-tmp-"))
            .collect();
        assert!(stray.is_empty(), "temp file must be removed on failure: {stray:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #930: a secret must be 0600 from the moment the temp file exists, not
    /// after a chmod that follows a world-readable create.
    ///
    /// #933: the assertion is on the TEMP file, before the rename. Statting
    /// only the final file tests the second half of this test's name and not
    /// the first: create-then-chmod, the implementation #930 removed, passes
    /// a final-file check unchanged. This is the one claim in the change that
    /// only CI can run, so it has to be the strong form.
    #[cfg(unix)]
    #[test]
    fn owner_only_mode_is_applied_at_creation_and_survives_the_rename() {
        use std::os::unix::fs::PermissionsExt;
        let d = temp_dir("mode600");
        let p = d.join("secret.env");

        let tmp = write_temp_sibling(&p, b"NATS_PASSWORD='x'\n", Some(0o600)).unwrap();
        let staged = std::fs::metadata(&tmp).unwrap().permissions().mode() & 0o777;
        assert_eq!(staged, 0o600, "temp file before the rename: got {staged:o}");
        commit_temp_sibling(&tmp, &p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "after the rename: got {mode:o}");

        // Overwriting an older world-readable file re-tightens it — and the
        // replacement is owner-only before it ever takes the target's name.
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let tmp = write_temp_sibling(&p, b"NATS_PASSWORD='y'\n", Some(0o600)).unwrap();
        let staged = std::fs::metadata(&tmp).unwrap().permissions().mode() & 0o777;
        assert_eq!(staged, 0o600, "replacement temp file: got {staged:o}");
        commit_temp_sibling(&tmp, &p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
        assert_eq!(std::fs::read(&p).unwrap(), b"NATS_PASSWORD='y'\n");

        let _ = std::fs::remove_dir_all(&d);
    }

    /// The wrapper still leaves no staged file behind: `write_temp_sibling`
    /// hands ownership to `commit_temp_sibling`, which consumes it.
    #[cfg(unix)]
    #[test]
    fn the_mode_wrapper_commits_and_leaves_no_temp() {
        use std::os::unix::fs::PermissionsExt;
        let d = temp_dir("modewrap");
        let p = d.join("secret.env");
        atomic_write_bytes_mode(&p, b"x", Some(0o600)).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let stray: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".kannaka-tmp-"))
            .collect();
        assert!(stray.is_empty(), "temp files left behind: {stray:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #934 P3: an orphaned temp file is reclaimed once it is old enough.
    ///
    /// The age floor is the whole safety argument, so both sides are asserted:
    /// a temp file belonging to a write happening RIGHT NOW is seconds old and
    /// must survive, or this sweep would delete other processes work.
    #[test]
    fn sweep_removes_old_temp_litter_and_spares_everything_else() {
        let d = temp_dir("sweep");
        let old = d.join(".kannaka-tmp-11111111-1111-1111-1111-111111111111");
        let fresh = d.join(".kannaka-tmp-22222222-2222-2222-2222-222222222222");
        let real = d.join("kannaka.hrm");
        let dotfile = d.join(".encoder");
        for f in [&old, &fresh, &real, &dotfile] {
            std::fs::write(f, b"x").unwrap();
        }

        // AGE rule: with the real hour-long floor, files created just now are
        // writes in flight and must all survive. This is the assertion that
        // stops the sweep deleting another process's work.
        assert_eq!(
            sweep_temp_litter(&d),
            0,
            "nothing recent may be swept — a live write's temp file is seconds old"
        );
        assert!(old.exists() && fresh.exists(), "no recent temp file may be removed");

        // NAME rule: drop the age floor and only `.kannaka-tmp-*` goes.
        assert_eq!(
            sweep_temp_litter_older_than(&d, std::time::Duration::ZERO),
            2,
            "both temp siblings are litter once old enough"
        );
        assert!(!old.exists() && !fresh.exists(), "aged temp litter must be reclaimed");
        assert!(real.exists(), "a real store file must never be touched");
        assert!(dotfile.exists(), "an unrelated dotfile must never be touched");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A sweep of a directory that does not exist is a no-op, not an error.
    #[test]
    fn sweep_of_a_missing_directory_is_harmless() {
        assert_eq!(sweep_temp_litter(Path::new("definitely-not-a-real-dir-934")), 0);
    }

    /// #934 P4: only a transient failure is retried. Retrying a permanent one
    /// would turn an honest error into a slow honest error, and retrying
    /// everything would mask a real bug.
    #[test]
    fn only_permission_denied_is_retried() {
        use std::io::ErrorKind;
        assert!(rename_is_retryable(&std::io::Error::from(ErrorKind::PermissionDenied)));
        for kind in [ErrorKind::NotFound, ErrorKind::AlreadyExists, ErrorKind::InvalidInput] {
            assert!(
                !rename_is_retryable(&std::io::Error::from(kind)),
                "{kind:?} is permanent; retrying it only delays the error"
            );
        }
    }

    /// The retry must be bounded, or a permanently-held file hangs the writer.
    #[test]
    fn the_rename_retry_is_bounded_and_short() {
        assert_eq!(RENAME_ATTEMPTS, 3);
        assert!(
            RENAME_ATTEMPTS * RENAME_RETRY_DELAY.as_millis() as usize <= 500,
            "a held file must fail promptly, not hang the writer"
        );
    }

    /// A write whose rename cannot succeed still reports the REAL error after
    /// the retries, not a placeholder.
    #[test]
    fn an_unretryable_rename_failure_keeps_its_message() {
        let d = temp_dir("badrename");
        let target = d.join("sub").join("deep").join("target");
        let tmp = write_temp_sibling(&d.join("target"), b"x", None).unwrap();
        // Renaming into a directory that does not exist is NotFound: permanent.
        let err = commit_temp_sibling(&tmp, &target).unwrap_err();
        assert!(err.starts_with("rename: "), "unexpected error: {err}");
        assert!(!err.contains("unknown"), "the real error must survive the retry loop: {err}");
        assert!(!tmp.exists(), "a failed rename must not leave litter");
        let _ = std::fs::remove_dir_all(&d);
    }
}
