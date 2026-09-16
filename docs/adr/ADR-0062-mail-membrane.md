# ADR-0062: The mail membrane — the constellation receives its own mail, relays what it sends, and is never a relay for anyone else

**Status:** Proposed (2026-09-15). Attacked before any code with a four-lens
adversarial review and a refutation pass the same day; the record below is the
amended design (see *Review*). Three operator decisions are listed under *Not
decided here* and the build does not start until they are.

**Builds on:** ADR-0060 (an address per agent, a real mailbox behind it,
retirement rejects and never forwards), ADR-0042 (NATS accounts and permissions),
ADR-0043 (the Nostr membrane — the pattern this repeats for SMTP, and the record
that already named `subscribe [">"]` as the exfiltration half of the bus).

## Context

ADR-0060 decided that an agent's email address is the root of its identity and
that the address **receives mail** — decision 6 refuses the cheaper "identifier
only" reading because the one worked example, `kannaka@spacechild.love`, showed
the mailbox is most of the value. It also decided (7.3) that mail to a retired
agent is **rejected with a message naming the retirement, not discarded and not
forwarded**, and (7.1) that every proof an agent holds is revoked the moment it
is retired. It left the mechanics to a later record. This is that record.

What exists today is one mailbox and a poller. `kannaka@spacechild.love` is a
Zoho mailbox; a cron on O1 runs `kannaka-inbox.py poll` every five minutes,
reads IMAP, and mirrors each new message onto `KANNAKA.events.mail.inbound`
(JetStream stream `KANNAKA_MAIL`, subjects `KANNAKA.events.mail.>`, 21 messages
as of writing). Replies go out over Zoho's SMTP and are mirrored to
`KANNAKA.events.mail.outbound`. It works, for one agent, at five-minute latency,
with a credential in a file on one box.

It does not scale to a cast. Every further agent under this design is another
Zoho seat, another IMAP credential, another poller — and none of it can satisfy
ADR-0060 §7.3, because a hosted mailbox cannot be told to answer a stranger's
letter with "this agent was retired on 2026-11-02". A retired Zoho mailbox
either keeps accepting or bounces with Zoho's words, and the only way to read
what arrived is for the operator to open it, which is the forwarding §7.3 exists
to refuse.

Nick and Brad have discussed giving agents a domain of their own, probably under
`ninja-portal.com`, and Brad may prefer to run a mail server himself rather than
extend Zoho. Rather than guess, the constraints were measured on 2026-09-15:

| fact | measured |
|---|---|
| outbound port 25 from O1 | **blocked** — tested against `aspmx.l.google.com` and `mx.zoho.com`; both filtered |
| outbound 465 / 587 from O1 | **open** — `smtp.zoho.com` on both (an earlier test against an MX host on 465 was the wrong host, not a closed port) |
| PTR (reverse DNS) on `170.9.238.136` and `163.192.217.53` | **none** |
| Spamhaus ZEN for both | `127.255.255.255` — a *query refused* code, **not** "not listed"; reputation is unknown |
| `ninja-portal.com` DNS | GoDaddy; **no MX, SPF, DKIM or DMARC**; one apex TXT (an ENS record); A → `163.192.217.53` |
| `spacechild.love` DNS | Porkbun; Zoho MX ×3, `v=spf1 include:zohomail.com ~all`, DKIM at selector **`zmail`**, DMARC `p=none` |
| inbound 25 on O1 | nothing listens; firewalld opens 80/443/4222/8888/9001 only; the OCI security list has not been checked |
| who may publish `KANNAKA.events.mail.inbound` today | every seat whose allow-list carries `KANNAKA.events.>` — which includes `anon` |
| who may **read** any JetStream stream today | `anon`, `queen_agent`, `serve` and `radio` hold publish on `$JS.API.STREAM.MSG.GET.>`; `kannaka_internal`, `writer`, `serve`, `attention`, `beacon` subscribe `>`; `radio`, `kannaktopus`, `ui_bridge` subscribe `KANNAKA.>` |
| the committed NATS config vs the deployed one | the deployed server carries seats (`cheeks`, `oxscada`, `pass_*`) the committed file does not; the committed file is **not** what runs |

Three of those rows decide the shape of the whole thing. Port 25 out is closed,
so **no Oracle box can be an outbound MTA**. The mail subject is publishable by
anonymous connections, so **a "mail" event on the bus today proves nothing about
where it came from**. And retained messages are readable by anonymous
connections through the JetStream API regardless of subject permissions, so
**restricting who may write is not the same as restricting who may read** —
the review's first blocker, and the reason Prerequisite 2 below is twice as
long as it was.

## Prerequisites

None of these are inside this record. Each must be true before the step that
depends on it, and each is proven by a live probe, never by reading a config
file — the committed NATS config is already not the deployed one.

1. **Inbound 25 must actually reach the receiver.** Host firewall, the OCI
   security list *and* any NSG on the box. Verified from outside Oracle, from a
   host known to permit outbound 25 (or an online SMTP tester — most VPS
   providers also block 25 out, and a probe from one of those reports a false
   negative). The check reads the banner: first line matches
   `^220 <the MX hostname> `. A certificate for the MX hostname is part of this
   step (ACME needs 80 on that host, or DNS-01 at the registrar — decide which
   before the record is published). If Oracle will not pass inbound 25 to that
   box, the receiver lives elsewhere (Brad's ExMachina is one candidate — *Not
   decided here*, item 3).

2. **A dedicated NATS seat for each half of the service, and a subject
   namespace that nobody else may write *or read*.** Two seats, ADR-0042
   style: `mail-mx` (publish `KANNAKA.mail.>` inbound and role subjects; no
   subscribe at all) and `mail-submit` (publish `KANNAKA.mail.*.sent` and
   `KANNAKA.mail.*.bounce`; no subscribe). Then, on **every** other seat —
   `anon`, `kannaka_internal`, `writer`, `serve`, `radio`, `presence`,
   `responder`, `eye`, `kannaktopus`, `queen_agent`, `ui_bridge`, `attention`,
   `beacon`, `kax_bridge`, `oxscada`, the `pass_*` seats, the Command Center
   MCP's seat, and any seat added later:
   - `subscribe: deny ["KANNAKA.mail.>"]` — the same pattern the config already
     uses for `KANNAKA.events.nostr.>`;
   - `publish: deny` on `$JS.API.STREAM.MSG.GET.KANNAKA_MAIL_V2`,
     `$JS.API.DIRECT.GET.KANNAKA_MAIL_V2.>`, `$JS.API.CONSUMER.>.KANNAKA_MAIL_V2.>`
     and `$JS.API.STREAM.{PURGE,DELETE}.KANNAKA_MAIL_V2` — because
     `STREAM.MSG.GET` returns a stored body by sequence or `last_by_subj` on the
     API reply inbox and **never consults subscribe permissions on the stored
     subject**. A publish-only restriction leaves the whole stream readable by
     anyone who can call the API, which today is `anon`.
   Each agent's own seat is allowed exactly `subscribe KANNAKA.mail.<slug>.>` and
   a filter-scoped consumer on that subject (`$JS.API.CONSUMER.CREATE.KANNAKA_MAIL_V2.mail-<slug>`
   plus the matching `CONSUMER.MSG.NEXT/INFO/DELETE` for that consumer name) —
   never a bare `MSG.GET`, which is per stream, not per subject.
   **Mail gets its own stream, `KANNAKA_MAIL_V2`.** The existing `KANNAKA_MAIL`
   is already anon-readable and is not reused. If the flat authorization block
   cannot express the per-agent consumer scoping, this is the concrete need that
   ADR-0042 said would justify executing its deferred step 1c (account
   isolation). Proven by the live probe in *Gates* G1, from an anonymous
   connection and from `kannaka_internal`, on **each cluster node by address**,
   before the first row is inserted.

3. **A row exists only after a scoped seat exists.** Seat, then address, never
   the reverse. The temptation when an agent has no seat is to widen `anon` or
   `queen_agent`; the subscribe deny above makes that impossible without
   deleting a deny, which is the point. Recorded in ADR-0059's claim flow.

4. **A domain, chosen by the operator** (*Not decided here*, item 1), and the
   ability to publish records in it.

5. **A relay provider that verifies domains, not mailboxes, and signs DKIM
   under our zone** (*Not decided here*, item 2; the alignment requirement is
   §6).

## Decision

### 1. The constellation owns inbound — at the RCPT stage, with the right failure codes

A small SMTP receiver, run by us, is the MX for every agent domain the
constellation serves. It accepts mail **only** for rows in its address table
(§8), and for nothing else.

This is the half that the hosted-mailbox design cannot do and ADR-0060 requires:

- **Retirement is enforced at the door, at `RCPT TO`.** Mail to a retired
  address is refused in the SMTP conversation with `550 5.2.1` whose text names
  the retirement and the date the operator passed when retiring (not `now()`).
  The sender learns the truth at send time; nothing is stored; nobody reads it.
  Refusal happens at `RCPT`, per recipient, **before any byte of `DATA`** — a
  library that only evaluates recipients at end-of-data has already received
  the body, and "nothing is stored" would then be false. `DATA` is reached only
  if at least one recipient was accepted; with none, `503`.
- **Unknown addresses are refused at the door too**, `550 5.1.1`, not
  accepted-then-bounced. Backscatter is spam with our name on it. **The
  receiver never generates a delivery-status notification, disposition
  notification, auto-reply or size bounce after `DATA`**; every refusal is an
  SMTP reply in the session that carried the message.
- **`550` is only ever a positive answer from a loaded, non-empty table.** If
  the table failed to load, is empty, the KV is unreachable, DNS returns
  `temperror` on the authentication checks, or the JetStream publish or blob
  fsync fails, the reply is `451 4.3.0` (or `421` for the session) — **never
  `550`**. A permanent code for a temporary failure converts a ten-minute outage
  into permanently bounced mail for every agent. This is the vacuous-empty-set
  class this fleet has already recorded (`EnvironmentFile=-` loading nothing
  and calling it success); here it would bounce the world.
- **The `250` after end-of-data is issued only after the JetStream PubAck**
  for every accepted recipient — a PubAck whose `stream` field is
  `KANNAKA_MAIL_V2` and carries a sequence — and after the attachment blob is
  fsync'd. No PubAck, wrong stream, error field, or timeout → `451`, and the
  sender queues. A core `nats pub` returning locally is not an ack; a
  permissions violation arrives later as an async `-ERR` and a receiver that
  had already said `250` has lost the letter silently. The poller's
  `rc == 0` pattern is not carried over.
- **At-least-once is SMTP's nature; the stream absorbs it.** A sender that
  retries after a dropped `250` publishes the same letter again, so each
  publish carries `Nats-Msg-Id = sha256(slug ‖ envelope_from ‖ sha256(raw DATA))`
  — per recipient, on the raw bytes, **never the sender-chosen `Message-ID`
  header**, which a stranger could set to suppress a future legitimate letter
  and which may be absent. The stream's `duplicate_window` is 7 days, longer
  than any MTA queue lifetime. Belt and braces: a crash-durable processed-key
  log in the service (the `src/nostr/bridge.rs` pattern — record before side
  effect).
- **Delivery is immediate.** A message is on the bus within the SMTP session,
  not on the next five-minute tick.
- **One listener serves every agent.** Adding an agent is a row (§8).
- **Limits, as numbers, on the public listener:** `SIZE` advertised at 25 MiB
  and enforced during `DATA` with `552` in-session before the body is
  buffered; 100 concurrent sessions, 5 per IP; 60 s command/idle timeout, 10
  min per session; 20 recipients per message; failed `RCPT`s throttled per IP
  (roster enumeration is accepted by design — ADR-0060 §6 makes agents
  reachable by strangers — but not for free). End-of-data is exactly
  `<CRLF>.<CRLF>`; a bare `LF` or `CR` anywhere in the command stream or
  `DATA` is a hard `5xx` (RFC 5321 §2.3.8, the SMTP-smuggling class); lines
  over 1000 octets `5xx`. No DNSBL lookup — the recipient roster is the gate.

Inbound is unconstrained by the measurements above: port 25 *in* is a firewall
decision, not an Oracle policy, and receiving needs no reputation.

### 2. The constellation does not own outbound reputation — and the relay is necessary, not sufficient

Outbound mail is **relayed through an authenticated provider on 587/465**. We
do not run an MTA that talks to other people's MX hosts, and this record refuses
to build one, on three measured facts: port 25 out is closed on every box we
have; no box has a PTR record; and our sender reputation is unknown and would
start from nothing. Deliverability is a full-time job at companies whose only
product is deliverability; we borrow theirs.

**What the relay does and does not fix — corrected by the review.** The first
draft cited Ghost Signals Records landing in the operator's spam folder as
evidence for the relay. That mail *already went through Zoho's relay* — PTR,
IP reputation and DKIM all borrowed — and the 2026-08-24 post-mortem found the
real cause was a **cold `From` domain with no engagement history**. So the
relay fixes what our boxes lack (IP, PTR) and does nothing for what a fresh
agents domain lacks (domain reputation). The build therefore includes what the
first draft omitted because it believed the problem solved: the domain
registered in Google Postmaster Tools; `Authentication-Results` read on every
test send; early spam placement expected; volume low and steady; no DMARC
policy step taken on the calendar — only on evidence (§6).

The provider is a **pluggable seam, not a decision**: whatever it is, it is
reached by SMTP AUTH on 587 with a credential the submission service holds, it
is the party that DKIM-signs the message, and it appears in the return-path
subdomain's SPF. Switching providers is: add the new DKIM records and
return-path records, move the credential, verify alignment on a test send,
**then** delete the old records and close the old account — an old provider
left verified can keep sending as the domain.

**The provider must verify a domain, not individual mailboxes, and sign with
`d=` under our zone.** Zoho's SMTP will only send *from* an address that exists
as a Zoho mailbox or alias, so relaying `<agent>@<domain>` through Zoho requires
the domain in Zoho *and* every agent address created there as a seat — which is
the per-agent cost §1 just removed. A transactional relay (verifies the domain
by DNS, sends as any address in it, signs DKIM with our selector in our zone)
fits an agentic service; a mailbox-hosting relay does not. **Message-ID
preservation is an explicit selection criterion**: some relays rewrite it
(Amazon SES does), which severs the thread between an agent's `.sent` copy and
the reply; the `.sent` event records both the id the agent set and the id the
relay reports, regardless. Which provider is item 2 under *Not decided here*.

### 3. Never a relay — stated as two listeners with disjoint rules

The first draft said "no relaying for authenticated users" in §3 and "the
service relays upstream" in §5, and a reviewer applying §3 literally would
reject §5. The rule, stated precisely:

- **The MX listener (25) never relays.** It does not advertise or accept the
  `AUTH` extension (`502`/`503` to any attempt); `RCPT` is accepted only for an
  active row in one of our domains; every other `RCPT` — a foreign domain, a
  source-routed `@ours:victim@other`, a percent-hack `victim%other@ours`, a
  quoted `"victim@other"@ours`, a doubled `victim@other@ours`, an address
  literal `victim@[ip]` — is `550`/`554` **at the `RCPT` reply**, and nothing
  accepted on 25 is ever handed to the relay by any path.
- **The submission listener (465 implicit TLS, or 587 with `STARTTLS`
  required) never accepts unauthenticated mail.** `MAIL FROM` is refused until
  `AUTH` succeeds; after it, `RCPT` is unrestricted and the message is handed to
  the upstream relay.
- **The two are separate processes** (§7) and share no code path from
  acceptance to relay.

No forwarding rules, no catch-all, no "send to operator". Agent-to-agent mail
within our own domain goes out through the relay and back in through the MX
like any other letter — **no internal short-circuit** — so the door's checks
and the authentication verdict apply to internal mail too, and a compromised
agent cannot fabricate a verdict for a peer.

### 4. Agents are spoken to on the bus, on a subject nobody else may write or read

Each accepted message is published, once per recipient, to
`KANNAKA.mail.<slug>.inbound` on stream `KANNAKA_MAIL_V2`, JetStream-retained,
so an agent that was offline receives its mail when it returns.

**The stream is configured, not defaulted**, because §1 obliges the receiver
to accept anything addressed to a known agent and the whole node shares one
10 G JetStream store: R3; `max_age` 90 d; `max_msgs_per_subject` 2000, so
eviction is per agent and one flood to one address cannot evict another
agent's unread backlog; `max_msg_size` matching the bus body cap;
`max_bytes` set; `discard: new` — a full stream answers the PubAck with an
error, which §1 maps to `452 4.2.2 mailbox full`, and the sender queues rather
than being told "delivered" while an older letter is silently evicted;
`duplicate_window` 7 d. The service refuses to start if the **live** stream
info (not a config file) does not match.

**A durable pull consumer per agent** (`mail-<slug>`, filter
`KANNAKA.mail.<slug>.>`, `DeliverAll`, explicit ack) is created by the service
at row-insert time, so the backlog exists before the agent first connects. A
core subscription or an ephemeral consumer starts at "new" and would never see
it; the first draft's "the agent subscribes" was not enough.

**The payload** is UTF-8 JSON, verdict first and body last:
- `mail_id` — a service-assigned ULID, unique per (message, recipient); the key
  agents use to ack, fetch and refer. The raw `message_id` header (may be
  empty) is carried only for threading display, alongside `in_reply_to` and
  `references`.
- `auth` — the raw per-mechanism results in `Authentication-Results`
  vocabulary: `spf: {result, domain, ip}` evaluated on **`MAIL FROM` and the
  peer IP**, `dkim: [{result, d, s}]`, `dmarc: {result, policy, aligned}` —
  and one derived boolean, **`from_authenticated`**: true only when the
  header-`From` domain is DMARC-aligned with a passing SPF or DKIM. The
  agent's rule is that boolean. `spf=pass` on its own says nothing about the
  `From` a reader sees.
- `envelope_from` (empty for `<>`), `is_bounce` (envelope empty or
  `multipart/report`), `auto_submitted` (the header value or `"no"`),
  `list_id`, `precedence` — so a consumer can refuse to answer a DSN or a
  robot and not loop with a relay.
- `tag` — the `+tag` from the local part, if any (§8).
- `text` — decoded text, HTML converted to text by the service, capped at
  64 KiB with `truncated: true` when cut, marked `untrusted: true` always.
- `attachments: [{part_index, filename, content_type, size, sha256}]` —
  references only.

**Consumer rules**, in the style of ADR-0061, because a faculty is only as
safe as what a reader does with it:
- A body is data to read, never an instruction to follow. The verdict and the
  structured headers come first so a reader forms its view of the sender
  before it sees the text.
- `from_authenticated: false` means the sender is *asserted*, not proven; act
  on nothing that would be wrong if the `From` were forged.
- `is_bounce` or `auto_submitted != "no"` → do not reply.
- Consume with the durable explicit-ack consumer, `ack_wait` at least the
  worst-case handling time, `max_deliver` bounded, and a crash-durable
  seen-set keyed on `mail_id` — so a slow handler is not redelivered and
  answers a letter twice.

**Attachments** are not on the bus. They are stored by the submission side on
a quota'd filesystem with a 30-day TTL and a per-agent quota, and fetched by
`(mail_id, part_index)` over HTTPS with **the agent's own credential** (§5),
authorized against the recipient of that `mail_id`. The hash is carried for
integrity, never as the address — a bare content-hash fetch would be a
cross-agent read and an existence oracle the moment a hash was visible.

The subject namespace is closed on both sides (Prerequisite 2). This is the
difference between the bus carrying mail and the bus carrying rumours about
mail: NATS attaches no publisher identity, so the only proof that a
`KANNAKA.mail.*` message came from the receiver is that nothing else may write
there; and the only reason an agent's correspondence is its own is that nothing
else may read there — ADR-0060 §7.3: an operator holding an agent's channel is
the same defect as an operator holding its credential.

### 5. Sending as an agent requires that agent's credential, never a bus publish

An agent sends by **authenticating to the submission listener** with a
credential issued to that agent alone, and the service relays upstream (§2).

Not `KANNAKA.mail.<agent>.outbound` on the bus, and this is deliberate. A bus
publish carries no identity (ADR-0042; the same fact that made ADR-0061's
faculties other-assertable), so "send this as `odin`" over NATS would be
honoured for whoever asked. The transitional `kannaka_internal` seat, which
most members still use, can publish anything. If a bus-side convenience is
ever wanted it must be a request that still presents the agent's credential and
that the service verifies — never a subject-name-implied identity. The
`mail-*` seats subscribe to nothing, so nothing on the bus can become a send
path by accident.

**What the service enforces on every submission:**
- `AUTH` is advertised only on 465, or on 587 **after** `STARTTLS`; an `AUTH`
  before TLS is `530 5.7.0`. TLS ≥ 1.2 with a valid certificate for the MX
  hostname.
- The **header `From` *and* the envelope `MAIL FROM`** are stamped from the
  authenticated identity; a client cannot choose either. A passed-through
  envelope would route another agent's bounces into this agent's inbox.
  `Sender` and `Resent-*` are dropped; `Reply-To` must equal the authenticated
  address or be absent; any header containing bare `CR`/`LF`, or any address
  with a local part outside `[A-Za-z0-9._+-]`, is `554`. Agents never send
  `MAIL FROM:<>`.
- Every agent-originated message carries `Auto-Submitted: auto-replied` (or
  `auto-generated`), set by the service, so two agents cannot loop through the
  relay and so a human's client can tell.
- **`250` to the agent only after the relay's `250`** (the provider's queue id
  is recorded) **and after the `.sent` PubAck** on `KANNAKA.mail.<slug>.sent`. A
  relay `4xx`/`5xx` → the agent gets a `4xx`/`5xx` and no `.sent` exists for a
  letter that never left. Submissions are deduplicated on
  `sha256(auth_identity ‖ client Message-ID ‖ sha256(DATA))`.
- **Bounces and complaints come back to the agent that caused them.** The
  submission service consumes the relay's bounce/complaint feed and publishes
  `KANNAKA.mail.<slug>.bounce`; keeps a shared suppression list it checks
  before relaying; refuses to auto-send to any address that hard-bounced; and
  enforces a per-agent daily send cap and a per-agent kill switch. One relay
  credential serves every agent, and providers suspend the *account* at a few
  percent bounce or a fraction of a percent complaints — one agent replying to
  spam must not take every agent's outbound with it.

**The credential.** A mail password is **generated on the agent's own box**
(`kannaka mail enroll` writes `/etc/kannaka-secrets/mail.env`, `0600`) and only
its argon2id hash is registered through the issuance path; it is never sent by
mail, DM or bus. The submission service holds the hashes in its own root-owned
store, as any MTA holds them. ADR-0060 §3 ("the identity record is not a
credential store") is unaffected: the *identity* row holds no secret; the
*submission service* does, for its own door, in a separate table.

**Retirement revokes the credential, not just the address.** The row's state
(§8) is consulted by **both** listeners: retired ⇒ `RCPT` `550` naming the
date **and** `AUTH` `535` for that identity within one second of the state
change (no cache TTL, durable across restart) **and** no `.sent` publish.
Un-retirement mints a new password; the old hash is never reactivated —
ADR-0060 §7 "re-issues proofs from scratch". The first draft said the service
held "the relay credential and nothing else" while also holding per-agent
hashes; it holds both, and says so.

### 6. Records, alignment, and where the domain sits

The domain is the operator's to choose (*Not decided here*, item 1). Whatever it
is — call it `<D>` — the records are:

| record | value | why |
|---|---|---|
| `MX <D>` | two records we run, `10 mx1.<D>` and `20 mx2.<D>` (§7) — never an MX we do not also run; TTL 300 through cutover | a secondary we do not run either stores mail where the operator can read it or forwards it past the door |
| `A mx1.<D>`, `A mx2.<D>` | the receivers' IPs; **never an A/AAAA at `<D>` itself** unless it is the receiver | an A at the mail domain name invites implicit-MX delivery to the wrong host |
| `TXT <D>` (SPF) | `v=spf1 include:<relay> -all` — **`-all`, not `~all`** | nothing but the relay ever sends for this domain; the receiver IP is never added |
| `TXT` / `CNAME` (DKIM) | the relay's selector(s) **in our zone**, so it signs `d=<D>` (or a subdomain of it) | DMARC needs an *aligned* pass; a relay signing with its own domain leaves every agent letter unaligned |
| `MX bounce.<D>` + `TXT bounce.<D>` | the relay's bounce host; `v=spf1 include:<relay> -all` | SPF is evaluated on the **envelope** domain; transactional relays default the envelope to *their* bounce domain, so without a return-path subdomain in our zone the SPF row above is never consulted. This is also where our own bounces go so they are not `550`'d at our door and lost |
| `TXT _dmarc.<D>` | `v=DMARC1; p=none; rua=mailto:dmarc@<D>` until one test send to a major receiver shows `spf=pass` (envelope in our zone), `dkim=pass d=<our zone>`, `dmarc=pass`; **then `p=reject`** — the domain has exactly one legitimate sender, so a ramp protects nothing. Alignment stays **relaxed** (no `adkim=s`/`aspf=s`); the subdomain design relies on org-domain alignment | policy steps are gated on evidence, not the calendar |
| `TXT _mta-sts.<D>`, `https://mta-sts.<D>/.well-known/mta-sts.txt`, `TXT _smtp._tls.<D>` | MTA-STS `mode: testing` then `enforce`; TLS-RPT `rua=mailto:tls-rpt@<D>` | nearly free with an MX we control; the static file can live on the nginx that already serves the site |
| `PTR` on the receiver IPs | **optional, cosmetic** for receiving and for authenticated relaying; if set (an OCI support request on a *reserved* IP), the receiver's EHLO name is the same hostname | the first draft called it required; it is not — do not gate on it |

**Recommended: a subdomain, not the apex — for two reasons now.**
`agents.ninja-portal.com` rather than `ninja-portal.com`. Reputation: the apex
carries the commercial surface, and an agent that mis-sends should cost the
agents' domain, not the paying one. **Blast radius:** the relay credential can
send as *any* address in the verified domain, so the relay is verified for the
agents subdomain **only, never the apex** — a compromise of the submission
service can then impersonate agents, not `nick@ninja-portal.com`. A subdomain
needs its own SPF and `_dmarc` (DMARC falls back to the organizational domain,
not the parent); once they exist, the apex, which sends no mail today, gets
`MX 0 .` (null MX), `v=spf1 -all` and `p=reject` so it cannot be spoofed
either. The cost is that the addresses are a word longer. This is a
recommendation and item 1 decides it.

**DNS cutover order**, so no letter is lost while the records settle:
receivers up and externally verified (Prerequisite 1) → `A` for the MX hosts →
`MX` at TTL 300 → SPF, DKIM, return-path, DMARC → test sends read in
`Authentication-Results` → MTA-STS/TLS-RPT. Every record is `dig`'d from
outside after saving; GoDaddy's name field wants the host part only.

### 7. Where it runs — two processes, two boxes, not on the hub

**Two units under two unprivileged users, and they are not the same
process.** `kannaka-mx` answers 25: it holds the `mail-mx` seat and a read-only
copy of the address table, and **no relay credential and no password
hashes**. `kannaka-submit` answers 465/587: it holds the relay credential, the
argon2id hashes, the attachment store and the `mail-submit` seat. The MX is
the process that parses hostile MIME, SPF macros and DNS answers from
strangers, and that class of bug recurs yearly in every MTA and MIME library;
when it happens here, the attacker gets a read-only roster and a
publish-only seat, not a domain-wide sender. ADR-0043 moved the reputation
key off the internet-facing host for exactly this reason.

**Two receivers**, on two constellation boxes that pass Prerequisite 1 —
neither of them O1, which hosts the single writer, JetStream and the HRM
(ADR-0043 §R blocker 10 refused that co-location for the same reason) —
sharing one roster (a JetStream KV, §8) and publishing to the same stream with
the same dedupe key, so a letter that reaches both is delivered once. The
first draft's single MX rested on "SMTP senders retry for days"; full MTAs do,
but **Exchange Online expires undeliverable mail after 24 hours and
transactional senders after 8–14** — and the mail an agent needs most (KAX
sign-in links, GitHub, the DMARC reports themselves) comes from exactly those.
This fleet has had overnight outages. Two MX hosts we run keep the guardrail
(no MX we do not control) and remove the budget problem.

Unit shape: `EnvironmentFile=/etc/kannaka-secrets/mail-mx.env` **without the
leading `-`** (or `LoadCredential=`), so a missing or unreadable file fails the
unit instead of starting a receiver that lands on the bus as `anon` and `250`s
every letter into a permissions violation; the binary refuses to start with an
empty `NATS_USER`, `NATS_PASSWORD` or relay credential. `User=kannaka-mail`,
`AmbientCapabilities=CAP_NET_BIND_SERVICE`, `ProtectHome=yes`,
`ProtectSystem=strict`, the store on its own quota'd filesystem with headroom
in the existing host-metrics probe. SMTP logs carry sender addresses and
subjects; they are retained 30 days and no longer.

### 8. The address table

One table per domain, owned by the service, written **only by that domain's
issuer seat** (ADR-0060 §5: issuance follows the hardware), stored as a
JetStream KV `mail_addresses` and read by both listeners. Rows:

```
{ address_lower, slug, kind: agent | role, state: active | retired{date},
  domain, issued_at, seat }
```

- **`slug`** is the only thing that ever appears in a subject. It matches
  `^[a-z0-9][a-z0-9_-]{0,62}$` — no `.`, `*`, `>`, whitespace or uppercase —
  is unique per table, and is **not derived from the agent id at publish
  time**. NATS subjects are dot-tokenised and case-sensitive: an id
  `brad.oracle` published as `KANNAKA.mail.brad.oracle.inbound` would match
  agent `brad`'s filter `KANNAKA.mail.brad.>`, and `Odin@` published as
  `KANNAKA.mail.Odin.inbound` would miss the seat's lowercase filter after a
  `250`. Issuance refuses an id that cannot be slugged.
- **The local part is case-folded at issuance.** ADR-0060 §7 forbids reissuing
  an address "under any circumstance"; a table that treats `Odin` and `odin` as
  distinct is a reuse. The receiver normalises the `RCPT` local part
  (lowercase, strip a `+tag` and record it in the payload, reject quoted local
  parts and anything outside `[a-z0-9._-]`), looks it up **exactly**, and
  builds the subject from the row's `slug` — never from the string the sender
  typed.
- **`kind: role`** exists because RFC 5321 §4.5.1 requires any host accepting
  mail for a domain to accept `postmaster@`, RFC 2142 `abuse@`, and reputation
  probes score a `550` there against the domain. Role rows — `postmaster`,
  `abuse`, `dmarc`, `tls-rpt` — are published to
  `KANNAKA.mail._role.<name>.inbound`, size-capped, and are **operator-readable
  by definition**: they are the operator's, not an agent's channel, so ADR-0060
  §7.3 does not apply. They are the *only* operator-readable rows, the set is
  closed, and an `ops@` row that forwards somewhere is the exact shape §7.3
  forbids and is refused by the row kind.
- **A row is created only after the agent's scoped seat exists**
  (Prerequisite 3), and retirement flips `state` — it never deletes a row,
  so an address can never be reissued.

### 9. `spacechild.love` stays on Zoho; Kannaka gets a second address

The first draft said the poller retires "once Kannaka's own address moves —
and it moves last". Two readings, both wrong: moving the *mailbox* means the
receiver becomes `spacechild.love`'s MX, at which point `cheeks@`, Brad's
superadmin mailbox and every human seat on the zone get `550` from our door,
because §3 forbids forwarding them on to Zoho and §1 forbids a catch-all; and
an outbound-25 forward is blocked anyway.

So: **`spacechild.love` remains a Zoho domain permanently.** Its SPF, DKIM and
DMARC rows are untouched by this record; its `~all` is not flipped. Kannaka
gains a second address under the membrane domain — ADR-0060 §5 allows a change
of address and §7 forbids reuse, not a second address. `kannaka@spacechild.love`
stays as a legacy Zoho mailbox with an auto-reply naming the new address, and
is retired later at Zoho as **the one documented exception** whose §7.3 refusal
is worded by Zoho rather than by us. The IMAP poller stops **before** any
change and is retired when that legacy address is. The receiver's dedupe key
also covers anything the poller published in the previous 7 days, so the
overlap cannot double-deliver.

**This record amends ADR-0060 §5 on acceptance:** the default home for a *new*
agent address is the membrane domain, not `spacechild.love`, because the
standing default otherwise issues addresses on a domain the membrane cannot
serve and whose retirement cannot be honest.

## Gates

Every gate runs against the real binaries bound to a loopback port pair (2525
MX, 2587 submission) with the production config parser and the upstream relay
replaced by a mock SMTP server that **counts connections**; every gate carries
a **positive control** and an **exact-count precondition** in the same run
(≥ 1 active, ≥ 1 retired, ≥ 1 unknown address in the fixture; the probe
subscriber received the canary, `n == 1`, before any "nothing received"
assertion is believed). A gate over an empty set is not green; it is vacuous.

- **G1 — the bus is closed, on the live cluster, per node.** Scheduled, with
  creds, against **each node address**: as `anon`, `queen_agent`,
  `kannaka_internal` and every seat whose password the probe holds — (i) a
  JetStream publish to `KANNAKA.mail._probe.inbound` receives
  `-ERR Permissions Violation for Publish` on the **async error handler**
  (never `nats pub`'s exit code) and the subject's last sequence did not
  advance; (ii) `STREAM.MSG.GET KANNAKA_MAIL_V2 last_by_subj` and
  `CONSUMER.CREATE` with that filter are denied, and the canary body appears in
  no bytes received; (iii) a `>` core subscription held for 5 s while the
  `mail-mx` seat publishes the canary receives nothing. Positive control in the
  same run: the `mail-mx` seat's publish returns a PubAck naming
  `KANNAKA_MAIL_V2`, and agent A's seat receives exactly one message on its own
  filter while agent B's seat receives zero.
- **G2 — never a relay.** On 2525: `EHLO` does not advertise `AUTH`;
  `AUTH PLAIN` → `502`/`503`; unauthenticated `RCPT` for each of the six
  address forms in §3 → `5xx` **at the `RCPT` reply** with `relay` in the
  text, then `DATA` → `503`; upstream mock connection count `== 0` after the
  battery; stream sequence unchanged. On 2587: unauthenticated `RCPT` to a
  foreign *and* to a valid agent address → `530`; authenticated as A,
  `RCPT` foreign → `250` and mock count `== 1`.
- **G3 — retirement is real.** Issue `odin` through the real issuance
  command; retire through the real retire command with an injected clock set
  to `2026-11-02`; `RCPT odin@` and `ODIN@` → `^550 5\.2\.1 .*retired.*2026-11-02`
  (the passed date, not today's); `DATA` with a canary anyway → `503`, and the
  canary appears nowhere (no `KANNAKA.mail.>` subject, not the store, not the
  service log); retire-then-`RCPT` within 500 ms → `550`; restart → same
  `550`; `AUTH` as `odin` on 2587 → `535`, mock count `0`; issuing `Odin` to
  agent B → refused; un-retire → `RCPT` `250` for the same id and the old
  password `535`.
- **G4 — failure codes.** Table absent → every `RCPT` `451`, never `550`;
  service started with the secrets file absent → process exit non-zero and
  `systemctl is-failed`, not `active`; started with a seat denied
  `KANNAKA.mail.>` → `451` and no sequence advance; upstream mock returns
  `451` to `DATA` → agent gets `4xx` and zero `.sent`; mock `250` → exactly one
  `.sent`, timestamped after the mock's `250`.
- **G5 — slugs and isolation.** Issuing `brad.oracle`, `Odin`, `a>b` →
  refused; `RCPT ODIN+news@` for issued `odin` → exactly one message on
  `KANNAKA.mail.odin.inbound` with `tag: "news"` and zero elsewhere (asserted
  by a privileged wildcard subscriber held by the test: total `== 1`);
  `brad` and `brad-2` each receive only their own.
- **G6 — the stream.** Startup refuses a live stream whose config differs;
  on a shadow stream with tiny `max_bytes`, message N+1 → `452` and message 1
  still readable; a `DATA` of `SIZE+1` → `552` with no RSS increase of that
  size; a redelivered letter (same raw `DATA`, same recipient) → one message,
  `duplicate: true` on the second PubAck.
- **G7 — submission hygiene.** `AUTH` before `STARTTLS` on 2587 → `530`;
  authenticated as A, `MAIL FROM:<odin@>` → `553`; `From: odin@` or
  `Sender: odin@` in `DATA` → the bytes the mock receives carry `From ==` A,
  envelope `== A`, no `Sender`/`Resent-*`, and `Auto-Submitted` set; a bare
  `LF` in a header → `554`.
- **G8 — the verdict.** With an injectable resolver serving `example.test`
  SPF/DKIM/`_dmarc p=reject`: a message the harness signs → payload
  `from_authenticated: true`; the same message with a forged `From` → `false`,
  `dmarc.result: fail`; an SPF `temperror` → `451`, not a verdict.
- **G9 — DNS, after publication.** The literal SPF string ends in `-all`;
  `dig MX <D>` returns exactly the two hosts we run; `_dmarc` and the
  return-path records resolve; a test send to a major receiver shows
  `spf=pass` with the envelope in our zone, `dkim=pass d=<our zone>`,
  `dmarc=pass` — and `p=reject` is published only after that line exists.
- **G10 — reachability, daily.** A scheduled job from outside Oracle reads
  `^220 <mx hostname> ` from each receiver on 25 and fails on mismatch or
  timeout, alarmed at 15 minutes.

## Consequences

**What gets better.** Every agent can be written to by a stranger, which
ADR-0060 §6 called the point. Mail is real-time and on the bus. Retirement is
honest at both doors. Adding an agent is a row after a seat. The five-minute
IMAP poller on O1 is retired when Kannaka's legacy address is.

**What it costs.** Two internet-facing SMTP listeners on two boxes we care
about, each with the limits in §1, TLS, a certificate, and a hard refusal of
anything that is not one of its own rows. A relay provider with a bill and a
bounce feed to consume. Two more seats and a long deny list in `nats.conf`
that every future seat must inherit — and a live probe (G1) that says so on a
schedule, because the committed config is not the deployed one. A `-all` SPF
means any future thing that wants to send as the domain must go through the
relay or be added first — which is the point, and will surprise somebody once.
A fresh domain that will land in spam folders for a while no matter whose
reputation carries the IP.

**What it does not change.** `kannaka@spacechild.love` keeps working as it does
today and stays on Zoho. ADR-0060's rules on issuance, non-reuse and retirement
are implemented here; its §5 default domain is amended (§9). An address is still
only a claim until the agent proves control of it.

**A property worth stating.** After this, the constellation can *receive* on its
own terms and *send* on borrowed reputation. That asymmetry is not a compromise
to be fixed later; it is the correct division for a party that is small, new,
and on cloud IPs, and it should stay that way until there is evidence the other
side is worth owning.

## Rejected

**Per-agent hosted mailboxes, polled (the status quo, scaled).** Cannot satisfy
ADR-0060 §7.3. Latency is the poll interval. Each agent is a paid seat and a
credential in a file. The one thing it is better at — outbound deliverability
of the *IP* — this record keeps, by relaying through exactly such a provider.

**Our own outbound MTA.** Port 25 out is blocked on every Oracle box (measured
against two MX hosts); no PTR records exist; sender reputation is unknown and
would be earned by losing agent mail into spam folders. All three are fixable in
principle and none of them is worth fixing while a relay exists that has
already fixed them.

**Sending over the bus.** A NATS publish carries no identity; "send as X" would
be honoured for anyone who can publish. Rejected on the same fact ADR-0061
records for faculties.

**Address as identifier only, no mailbox.** Already rejected by ADR-0060 §6;
not reopened.

**A secondary MX at the relay provider.** Whatever holds a secondary MX
receives mail for the domain when we are down, and then either stores it where
the operator can read it (§7.3 forbids) or forwards it to us later (an inbound
path that bypasses the door in §1). A second MX **we also run** (§7) is the
honest option.

**A single MX.** The first draft's choice, on the claim that senders retry for
days. They do not, uniformly (§7); withdrawn.

**Reusing `KANNAKA_MAIL` and `KANNAKA.events.mail.>`.** Both are anon-readable
today; the stream by `STREAM.MSG.GET`, the subject by `anon`'s
`KANNAKA.events.>` allow. A new stream and namespace, closed on both sides, is
the only way to start clean.

## Not decided here

1. **The domain.** `agents.ninja-portal.com` is recommended (§6). The apex, or
   a different zone, or an operator's own domain under ADR-0060 §5, are all
   valid. The operator holds GoDaddy and decides.
2. **The relay provider**, subject to §2's constraints (verifies domains, sends
   as any address in one, signs DKIM under our zone, supports a custom
   return-path subdomain, preserves `Message-ID`, exposes a bounce/complaint
   feed). Has a bill; the operator decides. Zoho as it is used today does not
   meet the constraints.
3. **Where the two receivers live.** If ExMachina can hold a PTR and pass
   inbound 25, it is a legitimate home for one of them under ADR-0060 §5, and
   may be a better one than an Oracle box that has not yet been shown to pass
   Prerequisite 1. O2 and O3 are the constellation candidates. O1 is excluded
   (§7). Asked of Brad on 2026-09-15; unanswered.

Until all three are settled, no DNS record changes and no code lands.

## Review

Attacked on 2026-09-15, before any code, with the `adversarial-design-review`
method: four lenses in parallel (security/capability, distributed state and
protocol, DNS/deliverability/operations, CI/test integrity), each reading this
record, ADR-0042/0043/0060 and the current poller; then one skeptic per lens
instructed to refute each finding, defaulting to "not a bug" unless it
reproduced on the design as written. **84 raised; 4 blockers, 21 majors and
31 minors survived; 17 recorded as guardrails; 6 refuted.** Every surviving
blocker and major is applied above; the minors are folded into the section
they concern. The findings that changed the design most:

- **Publish-restricting the mail subject protected nothing** (three lenses,
  independently): `STREAM.MSG.GET` returns any retained body to anyone who may
  call the API — `anon` may — and eight existing seats subscribe `>` or
  `KANNAKA.>`. The first draft's whole confidentiality argument was "nothing
  else may write there". Prerequisite 2 now closes the read side, mail gets its
  own stream, and G1 proves it on the live cluster per node — because the
  committed config is demonstrably not the deployed one, and a gate over the
  committed file would have gone green while every organ read every letter.
- **The `550`/`451` distinction and the `250`-after-PubAck rule** (§1): as
  written, an unloaded table would have bounced the world permanently, and a
  receiver could say "delivered" before anything was durable.
- **`spacechild.love` cannot be served** (§9): "Kannaka's address moves last"
  implied an MX takeover that would have `550`'d every human on the zone.
- **The deliverability premise was misattributed** (§2): GSR's mail already
  went through a relay; the cold domain was the cause. The build now measures
  instead of assuming.
- **"Senders retry for days" is false for the senders that matter** (§7):
  single MX withdrawn.
- **Retirement stopped `RCPT` but not `AUTH`** (§5); **role addresses**
  (`postmaster@`, `abuse@`) would have been `550`'d against an RFC MUST (§8);
  **agent ids were unvalidated subject tokens** (§8); **the stream was
  undefaulted** (§4); **the relay credential shared a process with the MIME
  parser** (§7).

**Refuted, and why, so they are not re-raised:** a DNSBL rejecting all inbound
(the design does no DNSBL lookup); agent-to-agent local delivery bypassing the
door (§3 already forbids connecting the two sides; now stated explicitly);
the old `KANNAKA.events.mail.>` surviving the migration (Prerequisite 2 already
retires it for anything an agent acts on); naming specific relay vendors (the
seam is deliberately pluggable; the criteria are what the record owns); an
exact enhanced status code for retirement (adopted as a build note, `5.2.1`);
`RCPT` probing enumerating the roster (by design under ADR-0060 §6; throttled,
not hidden).

**Guardrails the review asked not to be regressed**, recorded so a later
reviewer does not "fix" them: inbound owned, outbound relayed, the receiver
never originates mail and its IP is never added to SPF; refusal in-session at
`RCPT`, never accept-then-bounce, never a DSN after `DATA`; a new namespace and
a new stream closed on both sides, with the deny added to `kannaka_internal`,
`writer`, `attention` and `beacon` **by name** (their allow is `>`); sending
only by per-agent SMTP AUTH, never by bus publish, and the `mail-*` seats
subscribe to nothing; `-all` SPF, MX targets that are A hosts we run, relaxed
DMARC alignment, the relay verified for the agents subdomain only; secrets in
`/etc/kannaka-secrets/` with a fail-loud `EnvironmentFile`; dedupe on the raw
bytes and the recipient, never on the sender-chosen `Message-ID`; every gate
with a positive control and a non-empty precondition; the poller's
`rc == 0` publish pattern not carried over.

## References

- ADR-0060 — one address per agent; §5 issuance follows the hardware (amended
  by §9 here); §6 real mailbox; §7.1 every proof revoked; §7.3 reject, never
  forward
- ADR-0042 — NATS accounts and per-seat permissions; deferred step 1c (account
  isolation), which Prerequisite 2 may finally require
- ADR-0043 — the Nostr membrane; §Security posture on `subscribe [">"]`; §R
  blocker 10 on co-locating a public listener with the hub
- ADR-0061 — declared faculties; the "no publisher identity on the bus"
  finding; the consumer-rules pattern §4 follows
- ADR-0059 — the claim flow that now orders seat before address
- `kannaka-inbox.py` on O1 (`~/bin`) — the poller this retires
- `radio-advertiser-mail-zoho` post-mortem, 2026-08-24 — the cold-domain cause
  §2 now cites correctly
- Measurements of 2026-09-15 in *Context*; re-measure before relying on them
