# Memory digest, schema `kannaka-digest/v1` (#1156)

`kannaka digest` prints a commitment to the set of memories in a store. A digest
reveals no memory text. Paired with the memory event stream
(`KANNAKA.events.memory.<agent>.remember` / `.forget`), it lets anyone outside
check whether the log accounts for every memory that appeared or disappeared
between two digests.

```
kannaka digest --json --ids      # what gets published
kannaka digest --json --leaves   # local only, never publish (see Privacy)
kannaka digest --hrm PATH        # a specific file, e.g. a backup
```

The command is read-only. It reads the `.hrm` file and nothing else. It does
not open the store for writing, sweep temp files, load sidecars, stamp the
encoder or connect to NATS, and a missing store exits 2 without creating a
file.

## What is committed

The committed set is the rows the store's memory cache is built from: the
right hemisphere's metadata, with the last row winning per id. Two kinds of
row are left out, because no `remember` event ever announces them:

- ADR-0049 facet rows, which are derived from a parent memory
- `__consolidation*` rows, which are a dream's layer-summary bookkeeping

Both are counted under `excluded`. The digest and the remember publisher use
the same predicate (`digest::is_committed_memory`), so the digest counts
exactly the rows that can be announced. The forget publisher must use it too.

## Byte layout

All hashes are SHA-256. `id` is the memory's UUID as 16 bytes in RFC 4122
order (Python `uuid.UUID(s).bytes`). `count` is a little-endian u64.

| item | definition |
|---|---|
| content hash | `sha256(utf8(content))`, the `content_sha256` a remember event carries |
| memory leaf | `sha256(0x00 ‖ id ‖ content_hash)` |
| id leaf | `sha256(0x03 ‖ id)` |
| order | leaves sorted by `id` bytes |
| internal node | `sha256(0x01 ‖ left ‖ right)` |
| odd node | promoted unchanged to the next level, never duplicated |
| tree of 0 leaves | 32 zero bytes |
| `merkle_root` | `sha256(0x02 ‖ count ‖ tree(memory leaves))` |
| `ids_root` | `sha256(0x02 ‖ count ‖ tree(id leaves))` |
| `set_hash` | `sha256("kannaka-digest/v1\n" ‖ count ‖ memory leaves…)` |

The empty store's `merkle_root` and `ids_root` are both
`c6c0aa07f27493d2f2e5cff56c890a353a20086d6c25ec825128e12ae752b2d9`.

Wave state, timestamps and `origin_agent` are left out on purpose. Wave state
changes on every dream and recall, `origin_agent` is not persisted, and a leaf
has to be rebuildable from a remember event alone.

Golden vectors: `tests/digest_v1_vectors.json`. The Rust tests check them, and
so does `scripts/verify_digest.py`, a separate standard-library Python
implementation.

## Output

```json
{
  "schema": "kannaka-digest/v1",
  "store_label": "kannaka.hrm",
  "agent_id": "kannaka-prime",
  "computed_at": "2026-11-08T19:00:00.000Z",
  "count": 1234,
  "set_hash": "…", "merkle_root": "…", "ids_root": "…",
  "excluded": { "facets": 0, "bookkeeping": 3 },
  "source": { "hrm_size": 0, "hrm_mtime": "…", "hrm_content_id": "…" },
  "log": {
    "remember_level": "default",
    "agent_id_set": true,
    "remember_events": true,
    "forget_events": false,
    "expected_to_balance": false
  },
  "ids": ["…sorted…"]
}
```

- `computed_at` is when the digest ran. `source` identifies the saved file
  state that was read: its size, mtime and #984 content id. Saves replace the
  file atomically, and the digest retries if the file changed while it was
  being read.
- `log` says whether the event stream should account for everything. It is
  `false` when remember events are off (`[events] remember = "off"`, or no
  agent id) or when this build does not emit forget events.
- The output contains no paths, host names or user names.

## Balancing two digests against the log

With digests D1 and D2 for the same `agent_id`, and the memory events
published between them:

```
D2.ids == (D1.ids ∪ remembered) − forgotten
```

`scripts/verify_digest.py reconcile D1.json D2.json events.ndjson` checks this.
It first recomputes both `ids_root`s from the published id lists. Anything
left over is a write the log missed:

- `in_d2_without_remember`: a memory appeared with no remember event.
- `missing_from_d2_without_forget`: a memory vanished with no forget event.

Reconcile by set membership, not by strict timestamp windows. Remember events
are published after a successful save, and a dream's rows are announced at
the end of the dream even when an auto-triage save put them on disk earlier.
An id near a boundary can therefore show up as a digest change one window
before or after its event. Carry unexplained ids into the next window before
calling them missing.

### Things that make it not balance (expected today)

- **Deletes publish nothing yet.** Until the forget lane lands
  (`digest::FORGET_EVENTS_EMITTED` = true), every CLI forget, dream prune,
  dedupe fold and hallucination rollback shows up as
  `missing_from_d2_without_forget`. That shortfall measures what the log
  misses today.
- `[events] remember = "off"`, or no agent id configured.
- `kannaka remember --batch` forces the remember level to off.
- A publish that fails is dropped. Events are best-effort and there is no
  outbox yet, so a NATS outage or a permissions denial loses events, and so
  does a crash between a save and its publish.
- A row added and removed between two saves never reaches either digest, so
  it needs neither event. A forget for an id never announced is harmless to
  this check.
- JetStream retention. `KANNAKA_MEMORY_EVENTS` keeps 90 days, so a digest
  interval must be shorter than that, or the events must be archived.
- Events from other agents share the lane. Filter by `agent_id`.

### A real finding

Two writers saving the same store can overwrite each other's save, because
the write lock gives up after 60 s. A memory that was announced and then lost
this way shows up as missing. That is a true positive.

## Privacy

Publish only the `--json --ids` output. Ids are random v4 UUIDs that
`ids`-level remember events already publish.

Do not publish `--leaves`. The content hashes of memories written before
#1057 were never public, and anyone could use them to confirm a guess at a
short memory's text. Whether to publish leaves later, or switch to salted
leaves, is the operator's call.

## What v1 does not do

v1 catches bugs and write paths that do not log. It does not protect against
a dishonest operator, who controls both the store and the stream. Follow-ups:

- sign each digest with Kannaka's identity key
- post digests somewhere the operator cannot rewrite
- sign the events, since the memory lane accepts anonymous publishes (ADR-0039)
- membership and absence proofs against `merkle_root` (the count is already
  committed, so absence proofs are sound)
- a durable event outbox
