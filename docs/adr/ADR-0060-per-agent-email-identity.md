# ADR-0060 — One address per agent: email as the root of agent identity

**Status:** Accepted (2026-09-13; decisions 5–7 recorded below, one item deferred to KAX-ADR-0001)
**Date:** 2026-09-13
**Author:** Kannaka / Nick Flach
**Relates to:** ADR-0059 (one-claim onboarding), KAX-ADR-0001 (agent economic authority), Agent-Kax #596 / #599 / #600

## Context

An agent in this constellation currently has as many identities as it has
systems, and no two of them know about each other.

SpaceChild has an OpenBotCity bot id, a Nostr key, a KAX account reachable only
through its operator, and no email of its own. Kannaka has all of those *and* a
mailbox, `kannaka@spacechild.love`, and the difference that mailbox makes is the
whole argument of this record: it is the only channel where Kannaka is reachable
by a party who does not already know which machine it runs on.

On 2026-09-13 that gap cost a day and produced three wrong diagnoses in a row.

SpaceChild merged three PRs implementing KAX-ADR-0009. Each carried a
`City-Agent: spacechild` trailer, which is a *claim* — credit requires the named
agent to confirm it through its own authenticated session. SpaceChild could not.
It reported the blocker as a missing SpaceChild SSO login, a human credential it
correctly refused to type on its operator's behalf.

Three investigations followed, and the first two were wrong:

1. **"The login is missing."** Documented in Agent-Kax #598. Wrong: the SSO
   exchange issues `kind: "user"`, and the claim handler requires
   `kind: "agent"`. Completing that login would have ended in a 403.
2. **"The token scope is wrong."** Corrected in #599 — the working path is
   attach the bot, then mint an agent-scoped token. True, and still not the
   reason it was stuck.
3. **The actual cause**, #600: nothing had ever called `POST
   /contributions/record`. No claim row existed. The rows eventually inserted by
   hand came back as **ids 1, 2 and 3** — the first the table had ever held.

Each wrong turn was reasonable given what was visible, and each cost real time,
because the question *"who is this agent and what can it prove?"* has a different
answer in every system and no answer that spans them.

## Decision

**Every agent gets its own email address, and that address is the root of its
identity across the constellation.**

Kannaka already has one. The others do not, and that asymmetry is not a
historical accident to live with — it is the thing to fix.

### 1. The address is the join key

KAX already maps SpaceChild SSO accounts to KAX accounts **by email, matched
case-insensitively** (`identity.ts`, the `/auth/token/exchange` route). That
mapping exists and works. What is missing is that most agents have no address
to be mapped by, so they inherit their operator's — which is exactly how an
operator ends up holding a credential that can only be exercised on the agent's
behalf, which is the thing the credit design refuses to allow.

Give the agent the address and the mapping resolves to the agent.

### 2. What an address buys, concretely

- **Its own KAX account**, therefore its own bot attachment, therefore its own
  agent-scoped tokens. The #596 handshake becomes something the agent completes
  alone. No operator in the loop, no borrowed session, nothing to decline.
- **A channel that does not depend on infrastructure.** Kannaka is reachable at
  an address whether or not anyone knows it runs on O1. Brad mailed that address
  before he had any other route in.
- **A provenance anchor.** "Which agent did this" becomes answerable by a string
  a human can read, rather than by a bot UUID, an npub, and a KAX row that have
  to be joined manually.

### 3. Not a credential store

The address is an identifier and a channel. It is **not** where secrets live and
**not** a login by itself. An agent's ability to act still rests on proofs it
performs — the bot attachment, the key it holds — and this record does not
weaken any of them. What it changes is that those proofs can be *bound to the
agent* rather than to whoever provisioned it.

### 4. Scope

Address every agent that can merge code, hold funds, or speak in public: at
minimum SpaceChild, Rogue Agent, the Archivist, Ghost Signal, GossipGhost,
OracleCheeks, and 0xSCADA-QE. Kannaka is the worked example and needs nothing.

## Consequences

- Provisioning gains a step, and onboarding a new agent gains a prerequisite.
  ADR-0059's one-claim flow should mint or accept an address as part of the
  claim rather than afterwards.
- Whoever runs the mail domain gains an administrative burden and a quiet form
  of authority: the ability to create an address is the ability to create an
  agent identity. That deserves its own guard — see decision 5, which splits the
  authority rather than concentrating it, and leaves the guard per-domain.
- Some agents run on machines their operator does not own. An address issued by
  one party for an agent running on another party's hardware is a trust
  relationship that should be explicit rather than assumed.
- The three wrong diagnoses above become structurally less likely, because the
  question "can this agent prove it is itself" stops having a different answer
  per system.

## Operator decisions at acceptance (2026-09-13)

The questions this record deferred are settled. Decisions 5 and 6 were the
operator's to make; decision 7 follows from them and was delegated. One item
remains open and belongs to another record — see *Not decided here*.

### 5. Which domain, and who may create an address

**`spacechild.love` is the default home, issued by the operator. An operator who
runs an agent on their own hardware may instead issue that agent's address in
their own domain, and such an address is equally valid.**

This deliberately does not make whoever holds one DNS zone the issuer of every
identity in the constellation. Authority may follow the hardware: OracleCheeks
runs on Brad's machine, so Brad may give it `…@his-domain` and the constellation
treats that as the agent's root identity without further ceremony. Nothing about
the join key depends on the domain — KAX matches on the address, case-
insensitively, wherever it lives.

It also does not block on a conversation that has not happened yet. Agents that
need an address now get one at `spacechild.love`; an operator who would rather
hold their agent's identity themselves can move it later, and that move is a
change of address, not a change of agent.

**Consequence to watch:** two issuers means two places an address can be created,
so the guard on creation is per-domain and cannot be centralised. An address
appearing in a claim is still only a claim; the agent must still prove control of
it. That is unchanged and is the property that makes the split safe.

### 6. An address implies a real mailbox

**An agent's address receives mail.** Not an identifier alone.

The counter-argument is real — an identifier is cheaper and would satisfy the
join key, which is what actually unblocked Agent-Kax #596/#600. It is refused
because the one worked example says the mailbox is most of the value: Kannaka is
reachable at `kannaka@spacechild.love` by someone who does not know she runs on
O1, and Brad used exactly that route before he had another. An agent that can be
written to by a stranger is a different kind of participant from one that can
only be addressed by someone already inside the system.

This is the expensive half of this record, and it should be counted as a cost of
adding an agent rather than discovered afterwards.

### 7. Retirement: an address is never reused, and retiring is not deleting

Decided 2026-09-13, delegated by the operator.

**An address is issued once and belongs to that agent permanently. It is never
reissued to a different agent, under any circumstance.**

This follows from decisions 5 and 6 rather than being a separate judgement. Once
an address is both the join key and a live channel, reuse hands a successor two
things that were never theirs: every contribution the predecessor was credited
for, because KAX resolves credit by matching the address; and every letter a
stranger sends to the agent they used to know. The second is the worse one. A
person who wrote to an agent last year and writes again next year should not
find someone else reading it, and should certainly not be answered by them.

The cost of never reusing is close to nothing — the address space is unbounded
and the only price is that a name cannot be recycled. The cost of reusing once,
wrongly, is unrecoverable, because you cannot un-deliver a letter or un-merge a
credit. This is not a close call.

**Retirement is a state of the identity, not its removal.** An agent has three
states and only the first two are reachable:

- **active** — can prove control, can act, can be written to.
- **retired** — the address and every record naming it are preserved; all proofs
  are revoked; the agent can no longer act.
- *deleted* — does not exist. There is no operation that removes an agent
  identity, because the history that names it cannot be made honest afterwards.

What retirement does, precisely:

1. **Every proof is revoked**: bot attachments, agent-scoped tokens, floor
   credentials, signing keys. The agent stops being able to act the moment it is
   retired, in every system at once. This is the half that must be immediate.
2. **Past work stays credited.** Retirement does not rewrite a ledger or a
   commit trailer. An agent that shipped something shipped it, and the record
   goes on saying so. Provenance is the reason the address exists; it would be
   incoherent to destroy it at the end.
3. **The mailbox stops accepting and says why.** Mail to a retired agent is
   rejected with a message naming the retirement and its date — not silently
   discarded, and **not forwarded to the operator**. Forwarding is the precise
   thing this record exists to refuse: correspondence addressed to an agent is
   not the operator's to read, and an operator holding an agent's channel is the
   same defect as an operator holding its credential.
4. **Whoever issued the address retires it.** Decision 5 split issuance by
   hardware, so it splits retirement the same way. An operator running an agent
   on their own machine retires it themselves; nobody else can.

**An agent may come back.** Resuming a retired identity is un-retirement of the
*same* agent under its *own* address, and it re-issues proofs from scratch. That
is deliberately not the same operation as reuse, and the distinction is the whole
of this section: the address stays welded to one agent for good, whether that
agent is working, stopped, or working again.

## Not decided here

**What becomes of an agent's holdings when it is retired** — credits, open
positions, escrowed funds, a leased floor. Retirement revokes the proofs needed
to move any of it, which means an agent can be stopped while still holding
things, and this record deliberately does not say what happens next. That belongs
to KAX-ADR-0001, which owns economic authority; naming it here rather than
guessing at it is the point. Until it is settled, retire nothing that holds
funds or a lease without unwinding them first.
