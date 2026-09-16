# Rolling a kannaka release across the fleet

Two scripts, committed on 2026-09-16 after the v0.16.3 and v0.16.4 rolls both lost
theirs to a session scratchpad and v0.16.6 had to reconstruct one from the notes.

- `roll-node.sh vX.Y.Z [--stage-only]` — any node except O1. Downloads the release
  binary for the node's arch, verifies it against the `.sha256` sidecar (after
  asserting both are non-empty — an empty sidecar passes vacuously), finds the units
  whose `/proc/<MainPID>/exe` **is** the binary (a name match caught unrelated units
  on skywave), refuses up front if those units are active and `sudo -n` fails,
  replaces by move-aside, restarts one at a time, and verifies each process is on
  the new inode — failing on `*.previous`. `--stage-only` replaces the binary and
  prints the restarts owed, for a box like groundwave where sudo wants a password:
  `kill <MainPID>` as the unit's user and `Restart=always` re-execs on the new file.
- `roll-o1.sh vX.Y.Z` — O1 only. The generic filter would restart `kannaka-radio`
  mid-show, and O1 has a third binary at `~/kannaka-memory/target/release/kannaka`
  that five units execute directly. Refuses while a show is on air, replaces all
  three by move-aside with `restorecon`, fast-forwards the checkout so tree and
  binary agree, and restarts only its six units. Never `cargo build` on O1: it is
  one core, and anything built there lands under those five units at their next
  restart.

Order that has worked: one canary per architecture (debain2 for x86_64, O3 for
aarch64 — SELinux Enforcing, so the label check matters), then the rest, O1 last.
Verify the fleet in `presence_roster` afterwards, not on disk. The release workflow
verdict reads `failure` on every release because of the `brew-bump` PAT (#937);
read the per-job outcomes and count the 20 assets instead.
