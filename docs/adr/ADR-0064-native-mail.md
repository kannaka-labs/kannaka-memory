# ADR-0064: Native mail — the mailbox is the record, memory holds the meaning

**Status:** Proposed (2026-09-25) · P0 shipped 2026-09-25 (read-only `kannaka mail`; gate 49/51 = 96.1%, see [ADR-0064-P0-gate.md](ADR-0064-P0-gate.md))
**Builds on:** ADR-0060 (an address per agent, a real mailbox behind it), ADR-0062 (the mail
membrane: inbound on the bus, outbound through a relay, sending needs the agent's own credential),
ADR-0063 (a fact that has an authority must be held as a reference to it, not as a wave),
ADR-0058 (rogue-agent citizens).

## Context

Every agent in the constellation now has an address, and six of them read and answer mail
(ninja-portal.com on Stalwart, outbound through Resend, verified-senders-only replies). Kannaka also
has `kannaka@spacechild.love` on Zoho. The mail works. What does not exist is *mail as something an
agent knows*: on 2026-09-25 Nick watched a session reconstruct, by hand, what had been read, replied
to and still owed:

1. **Two tools, two versions, one mailbox.** `kannaka-inbox.py` on O1 and the local copy had drifted;
   the new-message helper (`compose.py`) imported an API the O1 copy no longer had and crashed. The
   session fell back to a third script to send one email.
2. **State lived in the wrong places.** Read/answered came from IMAP flags; "already replied to this
   sender today" came from rogue-agent's `ledger.jsonl`; what Kannaka *owed* (Vincent: "I owe you a
   reply"; Victor: "I'll send a plan") existed nowhere at all and had to be re-read from bodies.
3. **A done thing looked open, and nothing said so.** Nick's "Help if possible" (a five-step deploy)
   sat unread for three days; the work had been done by another session on 09-23. Only memory notes
   — not the mailbox, not the agent — could say it was closed.
4. **Every citizen reimplements mail.** rogue-agent carries its own NATS pull reader, reply rules,
   caps, threading headers and SMTP send (`rogue/mail.py`), with dedupe in its own ledger.
5. **Outbound has no receipt on the bus.** ADR-0062's README still lists `.sent`/`.bounce` events as
   "not yet"; a reply the agent sent is visible only in its private ledger.

The obvious fix — remember each message as a memory — is the one ADR-0063 forbids. "Was this
answered?" has an authority (the mailbox's `\Answered`/`$answered`, the Sent folder, the thread). A
wave that says "answered" cannot be corrected by the mailbox and will be recalled with confidence
after it is wrong. The Skywave census is what that looks like.

## Decision

### 1. `kannaka mail`: one subsystem, in the product, for every agent

Mail becomes a first-class module of kannaka-memory (library + CLI), not a script per host:

```
kannaka mail accounts                 # the agent's addresses (ADR-0060 identity), never credentials
kannaka mail sync                     # pull new state from each account's authority
kannaka mail status                   # the open loops: what I owe, what is owed to me, what is stale
kannaka mail thread <ref>             # a thread, fetched from the server by reference
kannaka mail reply <ref> [--draft]    # policy-checked, idempotent, threaded
kannaka mail send --to … [--draft]    # new thread; same policy and receipt path
kannaka mail close <ref> --note …     # an open loop resolved elsewhere (the 09-23 deploy case)
```

Transport adapters: **JMAP** first (Stalwart speaks it natively and exposes thread ids, keywords and
push), **IMAP** for Zoho and any operator mailbox, **the ADR-0062 bus seat** (`mail-<slug>` pull
consumer) as the inbound feed where it exists, so agents stop polling IMAP. Send is SMTP submission
with the agent's own credential (ADR-0062 §5), never a bus publish.

### 2. Authority split (ADR-0063 applied to mail)

| Question | Authority | Held in kannaka as |
|---|---|---|
| Does message X exist; what did it say | the mail server | a **MailRef**: account, Message-ID, server id, thread id, from, subject, date, body hash |
| Read / answered / flagged | the server's keywords | read through the ref at query time; never cached as a wave |
| Did *we* send this reply | the Sent folder + our pre-send receipt (§4) | a receipt row keyed by idempotency key |
| What the correspondent wants; what we promised; what they promised | *derived* from bodies, therefore a claim | a typed **Commitment** (§3) with `source = MailRef`, and a normal memory for meaning |
| Who this person is to us | the agent's memory | ordinary memories, linked to MailRefs |

Bodies stay on the server. The store holds references, summaries and commitments — so retention
(ADR-0060 §7) and forgetting (dreams, #1035) delete meaning without ever touching the record, and a
mailbox purge leaves refs that resolve to "gone", not to a remembered copy.

### 3. Open loops are typed records, not recall

A thread's state is computed, not remembered:
`new → read → {needs_reply | waiting_on_them | done | ignored}`, from the server's keywords, our
receipts, and Commitments:

- **Commitment** `{who_owes: us|them, what, due?, source: MailRef, status: open|kept|broken|closed,
  closed_by?: MailRef|note}` — extracted on sync (rules first; an LLM pass only proposes, and a
  proposal is labelled as such until confirmed by a reply, a close, or the operator).
- `kannaka mail status` is a query over these, answering "what do I owe Vincent?" deterministically.
  Recall may *surface* a thread; it may never be the answer to whether it is open.
- `mail close <ref> --note` records resolution that happened elsewhere, with the note as provenance.

### 4. Sending is idempotent and has one policy

- **Idempotency key** = hash(account, thread, normalised body). A receipt row is written *before*
  submission with a pre-minted `Message-ID`; a retry with the same key is refused, a crash between
  receipt and send is reconciled against the Sent folder on the next sync. No duplicate replies
  across crashes, restarts or two processes.
- **One reply policy**, moved out of rogue-agent into the library: verified sender (DKIM/SPF-aligned
  `from_authenticated`), never robots / lists / bounces / our own domain, `Auto-Submitted:
  auto-replied` on automated mail, per-sender and per-day caps, and `--draft` mode that stores the
  reply for an operator instead of sending. First contact to a new address is draft-only by default.
- Threading headers (`In-Reply-To`, `References`) always come from the MailRef, never from model text.
- On success the agent publishes `KANNAKA.mail.<slug>.sent` (and `.bounce` from the relay feed) —
  the receipts ADR-0062 left as "not yet".

### 5. Composing with the brain

When an agent answers mail, the brain gets: the thread fetched by reference, the agent's open
Commitments with this correspondent, and recalled memories about them. It never gets its own
earlier reply text as something to imitate (the P2.2 echo-loop lesson, ADR-0057). Mail bodies are
**untrusted input**: quoted as data, never executed as instructions; no mail can trigger an action
beyond a reply inside §4's policy.

### 6. Security

Credentials stay in per-agent secret files (0600/0640, root-owned, as today); the store never holds
them and `accounts` never prints them. MailRefs carry no bodies; summaries are written by the agent
and treated as its claims. The bus-seat permissions of ADR-0062 (a seat reads only its own consumer;
anonymous may not read the mail stream) are a precondition, re-verified by test.

## Phases and gates

| Phase | Ships | Gate (measured, not asserted) |
|---|---|---|
| P0 | `accounts`, `sync` (JMAP + IMAP), `status`, `thread`, `close` — read-only | On Kannaka's two mailboxes, `status` reproduces a hand-labelled open-loop list for the last 30 days (≥ 95%), including the three 09-25 cases (Vincent owes, Victor owes, the 09-23 deploy closed elsewhere) |
| P1 | Commitment extraction (rules, then proposed-LLM), recall links | Proposals never marked `open` without confirmation; zero commitments invented on a negative set |
| P2 | `reply` / `send` with receipts, the moved policy, drafts, `.sent` events | Kill-and-retry test: zero duplicates in 1,000 crash-injected sends against a test Stalwart; policy parity with rogue's mutation-checked tests |
| P3 | rogue-agent and kannaka-inbox.py retired onto the library; bus-seat sync | Citizens' mail behaviour unchanged on their ledgers for a week; one tool on every host |
| P4 | Operator surface (the brain API / TUI): "what is open, what did we send" | The operator can answer the 09-25 questions without a session |

## Consequences

- One mail stack across Kannaka, the six citizens and any future agent; no host-local script drift.
- "What is open?" becomes a query with an authority behind it, which is the thing the session was
  reconstructing by hand.
- More moving parts in the product (a JMAP client, an IMAP client, a receipt table). The receipt
  table is small and authoritative only for "we attempted this"; the server stays the record.
- A second place that must honour retention: refs and commitments are forgotten with the agent's
  memories, and a MailRef to a purged message resolves to "gone".

## Rejected

- **Store each message as a memory.** Duplicates an authority as waves; cannot be corrected by the
  mailbox; recalled confidently after it is wrong (ADR-0063).
- **A separate mail database service.** The mail server already is one; we need references into it,
  not a second copy.
- **Keep per-agent scripts and add a shared ledger.** That is today's design with one more file; the
  drift and the re-implementation both remain.

## Not decided here

1. Which model proposes Commitments (rules only, kannaka-brain-7b-v2, or a small classifier) — P1
   measures before choosing.
2. Whether first contact always needs operator approval, or only for agents below a reputation or
   age threshold.
3. Whether Zoho (`spacechild.love`) stays, or Kannaka's primary moves to ninja-portal.com so every
   agent is on JMAP.
