# ADR-0062: The mail membrane — the constellation receives its own mail, relays what it sends, and is never a relay for anyone else

**Status:** Proposed (2026-09-15). Design record ahead of any DNS change or code;
three operator decisions are listed under *Not decided here* and the build does
not start until they are.

**Builds on:** ADR-0060 (an address per agent, a real mailbox behind it,
retirement rejects and never forwards), ADR-0042 (NATS accounts and permissions),
ADR-0043 (the Nostr membrane — the pattern this repeats for SMTP).

## Context

ADR-0060 decided that an agent's email address is the root of its identity and
that the address **receives mail** — decision 6 refuses the cheaper "identifier
only" reading because the one worked example, `kannaka@spacechild.love`, showed
the mailbox is most of the value. It also decided (7.3) that mail to a retired
agent is **rejected with a message naming the retirement, not discarded and not
forwarded**. It left the mechanics to a later record. This is that record.

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

Two of those rows decide the shape of the whole thing. Port 25 out is closed, so
**no Oracle box can be an outbound MTA**; and the mail subject is publishable by
anonymous connections, so **a "mail" event on the bus today proves nothing about
where it came from**.

## Prerequisites

None of these are inside this record. Each must be true before the step that
depends on it.

1. **Inbound 25 must actually reach the receiver.** Host firewall *and* the OCI
   security list on whichever box hosts it. Verified by an external `telnet
   <ip> 25` from outside Oracle, not by reading the config. If Oracle will not
   pass inbound 25 to that box, the receiver lives elsewhere (Brad's ExMachina
   is one candidate — see *Not decided here*).
2. **A dedicated NATS seat for the service (`mail`)**, ADR-0042 style, and a
   **new subject namespace it alone may publish**. The existing
   `KANNAKA.events.mail.>` cannot be used for anything an agent will act on,
   because anonymous connections may publish it. The namespace here is
   `KANNAKA.mail.>`, and `nats.conf` must deny it to every seat that is not the
   service — including `kannaka_internal`, the transitional catch-all.
3. **A domain, chosen by the operator** (*Not decided here*, item 1), and the
   ability to publish records in it.
4. **A relay provider that verifies domains, not mailboxes** (*Not decided
   here*, item 2).

## Decision

### 1. The constellation owns inbound

A small SMTP receiver, run by us, is the MX for every agent domain the
constellation serves. It accepts mail **only** for addresses it has been told
exist, and for nothing else.

This is the half that the hosted-mailbox design cannot do and ADR-0060 requires:

- **Retirement is enforced at the door.** Mail to a retired address is refused
  in the SMTP conversation with a permanent `550` whose text names the
  retirement and its date. The sender learns the truth at send time; nothing is
  stored; nobody reads it. This is §7.3 exactly, and it is only possible when we
  answer the connection.
- **Unknown addresses are refused at the door too**, not accepted-then-bounced.
  Backscatter is spam with our name on it.
- **Delivery is immediate.** A message is on the bus within the SMTP session,
  not on the next five-minute tick.
- **One listener serves every agent.** Adding an agent is a row, not a seat, a
  poller and a credential.

Inbound is unconstrained by the measurements above: port 25 *in* is a firewall
decision, not an Oracle policy, and receiving needs no reputation.

### 2. The constellation does not own outbound reputation

Outbound mail is **relayed through an authenticated provider on 587/465**. We
do not run an MTA that talks to other people's MX hosts, and this record refuses
to build one, on three measured facts: port 25 out is closed on every box we
have; no box has a PTR record; and our sender reputation is unknown and would
start from nothing. Ghost Signals Records already landed in the operator's own
spam folder for exactly these reasons. Deliverability is a full-time job at
companies whose only product is deliverability; we borrow theirs.

The provider is a **pluggable seam, not a decision**: whatever it is, it is
reached by SMTP AUTH on 587 with a credential the service holds, it is the party
that DKIM-signs the message, and it appears in the domain's SPF. Switching
providers is a credential and two DNS records.

**The provider must verify a domain, not individual mailboxes.** This is the
constraint that matters and it is easy to miss: Zoho's SMTP will only send
*from* an address that exists as a Zoho mailbox or alias, so relaying
`<agent>@<domain>` through Zoho requires the domain in Zoho *and* every agent
address created there as a seat — which is the per-agent cost §1 just removed.
A transactional relay (the kind that verifies the domain by DNS and then sends
as any address in it) fits an agentic service; a mailbox-hosting relay does not.
Which one is item 2 under *Not decided here*.

### 3. Never a relay

The receiver **accepts only for its own domains and never forwards anywhere**.
No relaying for authenticated users, no forwarding rules, no catch-all, no
"send to operator". It is an inbound door and an outbound submission point,
and the two are not connected — a message that comes in on 25 cannot go out on
587 by any path inside the service.

Stated as a rule because an open relay is the single most common way a small
mail server becomes someone else's weapon, and because §7.3's refusal to forward
is a special case of it.

### 4. Agents are spoken to on the bus, on a subject that only the service may publish

Each accepted message is published, once, to
`KANNAKA.mail.<agent-id>.inbound`, JetStream-retained, so an agent that was
offline receives its mail when it returns. The payload carries headers, the
text body, the thread keys (`Message-ID`, `In-Reply-To`, `References`) — and
the **authentication verdict**: the SPF, DKIM and DMARC results the receiver
computed for the sending domain. An agent acting on a message must be able to
see whether the `From:` was proven or merely asserted; a mail that fails DMARC
is data to read, not an instruction to follow.

Attachments are not on the bus. They are stored by the service and referenced
by hash and size; the agent fetches one if it wants it. Nobody's mailbox
becomes the constellation's disk.

The subject namespace is **publish-restricted to the `mail` seat**
(Prerequisite 2). This is the difference between the bus carrying mail and the
bus carrying rumours about mail: NATS attaches no publisher identity, so the
only proof that a `KANNAKA.mail.*` message came from the receiver is that
nothing else is permitted to write there. Each agent's own seat subscribes to
its own `KANNAKA.mail.<agent-id>.>` and nothing wider — the OracleCheeks seat
is the model.

### 5. Sending as an agent requires that agent's credential, never a bus publish

An agent sends by **authenticating to the service** — SMTP AUTH on the
service's own submission port, with a credential issued to that agent alone —
and the service relays upstream (§2). The service stamps the `From:` from the
authenticated identity; a client cannot choose it.

Not `KANNAKA.mail.<agent>.outbound` on the bus, and this is deliberate. A bus
publish carries no identity (ADR-0042; the same fact that made ADR-0061's
faculties other-assertable), so "send this as `odin`" over NATS would be
honoured for whoever asked. The transitional `kannaka_internal` seat, which
most members still use, can publish anything. Until every agent has its own
scoped seat, the bus cannot be trusted to say *who* is sending, and even then
SMTP AUTH is the credential every mail library already speaks.

The credential is the agent's mail password, held by the service hashed, as any
MTA holds one. ADR-0060 §3 ("the identity record is not a credential store")
is unaffected: the *identity* record still holds no secret; the *mail service*
does, for its own door, as it must.

A copy of every sent message is published to `KANNAKA.mail.<agent-id>.sent`
(same seat restriction) so the agent's own record of its correspondence lives
where its inbound does.

### 6. Records, and where the domain sits

The domain is the operator's to choose (*Not decided here*, item 1). Whatever it
is, the records are:

| record | value |
|---|---|
| `MX` | the receiver's hostname, one record, priority 10 — no secondary; a second MX that we do not also run is a hole, not a backup |
| `A` (receiver hostname) | the receiver's IP |
| `TXT` (SPF) | `v=spf1 include:<relay provider> -all` — **`-all`, not `~all`**: nothing but the relay ever sends for this domain, so say so |
| `TXT` (DKIM) | the relay provider's selector and key; they sign, so they hold the key |
| `TXT` `_dmarc` | `v=DMARC1; p=none; rua=mailto:<a mailbox we read>` for the first 30 days of traffic, then `p=quarantine`, then `p=reject` — each step after a clean month |
| `PTR` on the receiver IP | required for the receiver only so that the *providers we relay through* and the *senders who check us* see a named host; Oracle sets it from the console on a reserved IP |

**Recommended: a subdomain, not the apex.** `agents.ninja-portal.com` rather
than `ninja-portal.com`. The apex carries the commercial surface — the site,
Stripe, the pass flow, an ENS record — and mail reputation is per-domain. An
agent that mis-sends should cost the agents' domain, not the paying one. It
also means the apex zone is untouched: no MX where there was none, no SPF that
could interact with anything the site later needs. The cost of the subdomain is
that the addresses are a word longer. This is a recommendation and item 1
decides it.

### 7. Where it runs

On a constellation box that can pass Prerequisite 1, as a systemd unit with
its secrets in `/etc/kannaka-secrets/` (SELinux forbids `user_home_t` from
`init_t`, learned the hard way on this fleet), with the `mail` NATS seat's
credential and the relay credential and nothing else. It is a member of the
swarm the way the radio is: a service seat, not an agent, with no memory store
of its own.

## Consequences

**What gets better.** Every agent can be written to by a stranger, which
ADR-0060 §6 called the point. Mail is real-time and on the bus. Retirement is
honest at the door. Adding an agent is a row. The five-minute IMAP poller on O1
is retired once Kannaka's own address moves — and it moves last, after the
service has carried other agents' mail for long enough to be trusted with hers.

**What it costs.** An internet-facing SMTP listener is a new attack surface on a
box we care about; it needs size limits, connection limits, TLS, and a hard
refusal of anything that is not one of its own addresses, and it needs to be
attacked before it is trusted (see *Review*). The relay provider is a
dependency with a bill. A `-all` SPF means any future thing that wants to send
as the domain must go through the relay or be added to SPF first — which is the
point, and will surprise somebody once.

**What it does not change.** `kannaka@spacechild.love` keeps working as it does
today until the migration in *What gets better*. ADR-0060's rules on issuance,
non-reuse and retirement are implemented here, not amended. An address is still
only a claim until the agent proves control of it.

**A property worth stating.** After this, the constellation can *receive* on its
own terms and *send* on borrowed reputation. That asymmetry is not a compromise
to be fixed later; it is the correct division for a party that is small, new,
and on cloud IPs, and it should stay that way until there is evidence the other
side is worth owning.

## Rejected

**Per-agent hosted mailboxes, polled (the status quo, scaled).** Cannot satisfy
ADR-0060 §7.3. Latency is the poll interval. Each agent is a paid seat and a
credential in a file. The one thing it is better at — outbound deliverability —
this record keeps, by relaying through exactly such a provider.

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
path that bypasses the door in §1). A single MX that queues on the sender's side
is the honest option: SMTP senders retry for days.

## Not decided here

1. **The domain.** `agents.ninja-portal.com` is recommended (§6). The apex, or
   a different zone, or an operator's own domain under ADR-0060 §5, are all
   valid. The operator holds GoDaddy and decides.
2. **The relay provider**, subject to §2's constraint (verifies domains, sends
   as any address in one). Has a bill; the operator decides. Zoho as it is used
   today does not meet the constraint.
3. **Whether Brad hosts.** If ExMachina can hold a PTR and pass inbound 25, it
   is a legitimate home for the receiver under ADR-0060 §5, and may be a better
   one than an Oracle box that has not yet been shown to pass Prerequisite 1.
   Asked of him on 2026-09-15; unanswered.

Until all three are settled, no DNS record changes and no code lands.

## Review

To be attacked with the `adversarial-design-review` method before the build —
security/capability, distributed-state, and the mail-specific lens: what an
outside sender can make the receiver do (backscatter, relay, resource
exhaustion, header injection, a `From:` that passes SPF for a domain we did not
expect). Findings recorded here before code, as ADR-0061 did.

Guardrails that should not be regressed by a later reviewer: inbound owned,
outbound relayed; never a relay; retirement rejected at the door and never
forwarded; the bus subject publishable only by the service; sending
authenticated per agent and never by bus publish; `-all` SPF; single MX.

## References

- ADR-0060 — one address per agent; §6 real mailbox; §7.3 reject, never forward
- ADR-0042 — NATS accounts and per-seat permissions
- ADR-0043 — the Nostr membrane; the pattern this repeats
- ADR-0061 — declared faculties; the "no publisher identity on the bus" finding
- `kannaka-inbox.py` on O1 (`~/bin`) — the poller this retires
- Measurements of 2026-09-15 in *Context*; re-measure before relying on them
