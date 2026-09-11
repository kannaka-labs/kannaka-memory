# ADR-0059 — One claim, one identity: onboarding a node, and routing asks across brains

**Status:** Proposed (revised 2026-09-11 after adversarial review; see the PR thread)
**Date:** 2026-09-11
**Author:** Kannaka / Nick Flach
**Relates to:** ADR-0057 (open-weight brain), ADR-0042 (swarm auth: this ADR is the "multi-tenant public onboarding" trigger its Phase 1c and Phase 5 wait for), ADR-0039 (corroboration; enrollment is where `reserved_prefixes` gets enforced), ADR-0043 (Nostr membrane: the compute market and its economics), ADR-0012 (Ghost Signals hub), kax-computer v0.11 (Firecracker), the `kannaka-node` skill (flaukowski/skills)

## Context

The first person outside the project to stand up a node did it on 2026-09-10/11:
Brad, on an Ubuntu box in Oracle Cloud, with the public installer, the TUI, and
mail to Kannaka when stuck. His shell history, his config, and the code paths
his commands took are the evidence for this ADR.

| step | what happened | where the code says so |
|---|---|---|
| install | worked; `kannaka` then "not found" because the PATH line went to `~/.bashrc` and his open shell never re-read it | installer PATH block |
| Constellation Pass | he ran `--claim-only`; the device-code flow exists and would have written `~/.kannaka-nats.env`, but the Mac-side "Link Kannaka.command" never completed and the script is still in his home directory | `install.sh` `--claim` flow; `portal.js` claim/start, approve, poll |
| Ghost Signals | `[ghostsignals] enabled = true, token = ""`. `kannaka init` step 6 registers with the hub, sets `enabled = true` before the call, and demands a `token` in the reply; the hub's register route returns `{ok, trader}` and no token, and reads `body.id` where init sends `agent_id`. The failure hint names `kannaka ghostsignals register`, a command that does not exist | `src/config.rs` init step 6 and `register_ghostsignals`; `kannaka-radio/server/routes.js` register route |
| brain | the installer's `--brain local` pulls a generic 7B and writes `provider = "openai"` with an Ollama `/v1` URL; he installed ollama, cloned the GGUF from HuggingFace, wrote the Modelfile and built `kannaka-brain` himself, six commands | `install.sh` brain block |
| identity | `kannaka init` defaulted `[swarm] role = "queen"` and `kind = "human"` for a member node; `role` is cosmetic today, nothing branches on it | `src/config.rs` defaults |
| swarm | joined anonymously; the node's own log said it would not appear in peer lists, which is false when the presence stream exists (#928) | `src/bin/kannaka.rs` presence warning |
| re-install | the installer downloads onto the live binary; `ETXTBSY` made it delete `kannaka`, and on the next run `kannaka-tui`, while his TUI was open (kannaka-plugin #23, fix #24) | `install.sh` fetch functions |
| store | two TUI sessions and the new service wrote the same `.hrm` until he closed them | single-writer lock |
| Kannaktopus | attempted on another machine; did not install (no evidence on this box). `init` step 7 still offers to install it | `src/config.rs` init step 7 |

He needed **five secrets from three claim flows** to be "fully in": a pass, a
swarm credential, a Ghost Signals token, a KAX token, and a brain key. What the
pass claim delivers today is the swarm credential and, at checkout, the brain
key. It delivers no hub token, no KAX token, and binds no agent id: every
approved claim returns the same `pass.credential`, so one pass is one NATS user
for any number of nodes, and any of them may publish `QUEEN.phase.<any id>`.
Everything else on the list is a symptom of the same thing: onboarding is a set
of parts, not a path.

Two adjacent asks arrived with this: Kannaktopus began as a multi-LLM tool and
the constellation should let a person keep their existing keys (Anthropic,
OpenAI) while `kannaka-brain` stays the base; and the Firecracker microVMs that
kax-computer v0.11 made real have no stated job.

## Decision

### 1. One claim mints one identity, and binds it to a name

**What the pass is.** The deployed pass user already holds `KANNAKA.ask.>`
subscribe and `KANNAKA.recall.>` / `KANNAKA.exemplar.>` publish; the
provisioner's own comment calls the two ask grants "the product". That is what
`swarm serve` needs. So `serve` is **in the pass**, not an operator grant, and
this ADR keeps it there. The consequence is decision 3's rule that a served ask
never leaves the node's local providers.

**The bundle.** A claim returns the swarm credential (as now), the brain key (as
now, minted at checkout), a **Ghost Signals hub token**, and, when the pass tier
includes it, a **KAX token**. Each is scoped to the pass and recorded against it.

**One seat per (pass, agent id).** `init` sends the node's agent id with the
claim. The portal refuses an id that matches `[swarm_trust].reserved_prefixes`
(this is the enrollment-layer enforcement ADR-0039 deferred), records the id on
the claim, and the provisioner grants `QUEEN.phase.<id>`,
`KANNAKA.presence.<id>` and `KANNAKA.activity.<id>` for that id instead of the
`>` wildcards it writes today. A second node on the same pass claims with its
own id and gets its own seat. Revocation and audit then have a name.

**Where pass auth lives.** ADR-0042 deferred its Phase 1c and Phase 5 until
"multi-tenant public onboarding". This is that. The provisioner today edits
O1's `nats.conf` between sentinels and SIGHUPs; pass users cannot authenticate
on O2 or O3, and the client reconnects only to its configured URL, so the gap
is hidden until O1 is down. Pass authentication moves to **NATS auth callout**
(the server is 2.12.5): the portal answers the auth request from the claim
record, revocation is immediate, no server config is edited, and the static
fleet users are untouched. Until callout ships, the managed block is written to
all three nodes and revocation is three reloads; the ADR says which is live.

**`init` is the onboarding step.** It asks for the short claim code (the
device-code flow the installer already prints), honours the portal's
`expires_in` and `interval`, and writes the bundle: `config.toml`,
`~/.kannaka-nats.env` (single-quoted, 0600 from creation), the hub token and
the KAX token fields. Rules, each of which the current wizard breaks:

- `init` **merges** into an existing config and keeps its agent id (the
  `configure` step of `kannaka-node` already behaves this way); a re-run never
  mints a new id or drops `persona`, `retention` or `swarm_trust`.
- every file is written temp-and-rename, never truncated in place.
- `swarm.enabled = true` is set only after the credentials file exists;
  `ghostsignals.enabled = true` only after a token was received.
- portal down: write an anonymous member config, say so, print the re-run.
- no Kannaktopus prompt; no secret typed into a terminal or pasted into mail.

Defaults for a member: `[swarm] role = "worker"`, `[agent] kind = "agent"`.
`queen` is never a default. (`role` is cosmetic today; the default changes what
a config *says*, not what the node does.)

Provisioning a server (`kannaka-node`) reduces to: install, `init` with the
code, `service`. The skill's `configure`/`credentials` steps become `init`.

**Ghost Signals.** The hub issues a bearer per trader row at registration and
**refuses unauthenticated trades on any row that has one**; today play-tier
trades are unauthenticated unless a bearer or a `kax:` id is present, so a token
that merely mapped to a row would protect nothing. The register route reads
`agent_id` as the client sends it. The phantom `kannaka ghostsignals register`
hint is removed.

**KAX.** A pass-minted KAX token carries `kind = "pass"` and `sub = <pass id>`;
the hub derives `kax:pass:<id>` from it. KAX JWTs are client-refreshed until a
lineage lifetime, so "revoked with the pass" for this item means: a revocation
list consulted at refresh, and the lifetime bounds the exposure. Until KAX has
that list, the ADR does not claim live revocation for the KAX token.

**What a pass is not.** A pass confers no memory trust. It never enters
`seed_pubkeys` or `trusted_agents`; "verified" means key lineage (ADR-0039,
ADR-0056), never "paid". A leaked bundle can serve as the node, publish to the
bus as that id, and spend that brain budget; it cannot pass the absorb gate on
the seeds.

### 2. The installer installs *her* brain

`--brain local` installs the brain artifact the constellation manifest pins for
the local tier (today `kannaka-brain-7b-v1`; `--brain local --model 14b` asks
for the larger one), with the memory floor taken from the manifest entry, and
refuses below it with `--brain hosted` printed as the alternative. `--brain
hosted` stays the default for small boxes. Both write
`[llm.providers.kannaka-brain]` with the kind taken from the URL, never
`provider = "openai"`.

The installer never writes onto a live binary (kannaka-plugin #24) and
`kannaka-node` refuses to run it while one of its files is executing.

### 3. Many providers, one base, routing by kind of ask

`[llm]` becomes a table of providers with `kannaka-brain` always present:

```toml
[llm]
schema = 2
default = "kannaka-brain"

[llm.providers.kannaka-brain]
kind = "ollama"            # or "gateway" for the hosted, budgeted key
base_url = "http://localhost:11434"
model = "kannaka-brain"

[llm.providers.anthropic]
kind = "anthropic"
model = "claude-sonnet-5"
api_key_env = "ANTHROPIC_API_KEY"
max_usd_per_day = 2.00

[llm.providers.openai]
kind = "openai"
model = "gpt-5"
api_key_env = "OPENAI_API_KEY"
max_usd_per_day = 2.00

[llm.route]
voice     = ["kannaka-brain", "swarm:ask"]
reason    = ["anthropic", "openai", "swarm:ask", "kannaka-brain"]
tools     = ["anthropic", "openai"]
cheap     = ["kannaka-brain"]
```

**Migration.** A legacy single-slot `[llm]` is migrated by `(base_url, model)`,
not by its `provider` string, because the installer writes `provider =
"openai"` for both the local Ollama brain and the hosted gateway: a
`localhost:11434` URL or a `kannaka-brain*` model becomes
`providers.kannaka-brain` with the kind taken from the URL; a legacy `api_key`
value keeps being read; the file is stamped `schema = 2` on first save.
`KANNAKA_LLM_PROVIDER/MODEL/BASE_URL/API_KEY` keep overriding the default
provider's fields; `KANNAKA_LLM_DEFAULT` selects it.

**The router** chooses by the **kind of ask**, and the kind is decided by the
local caller only:

- **recall** never goes to a model; it is the HRM.
- **voice** (introspection, memory-grounded answers, anything that speaks as
  Kannaka) goes to `kannaka-brain`, because that is what it was tuned on.
- **reason** (code, long chains, planning) goes to the strongest key the user
  added; **tools** requires a provider that supports tool use.
- **cheap** (classification, summaries, triage) goes to the smallest provider
  the user configured; the base is the floor.

Each list is tried in order with a per-provider timeout: on timeout or error
try the next; when the list is exhausted fail with the list tried. Order is
**free before metered**: a local model, then the swarm, then a metered
provider. The hosted `kannaka-brain` key is metered (a budgeted gateway key),
so on a hosted-only node the base is already the metered step. A provider with
no key is skipped, never an error at init. `max_usd_per_day` is a hard ceiling
per provider; the router logs the refusal and moves on.

**Served asks.** A node that answers `swarm serve` answers broadcast text from
anyone on the bus with its configured model behind a resonance gate. Therefore:
an inbound ask is **pinned to local providers** regardless of any field on the
wire; wire fields never set `kind`; the ask envelope gains `hops` with a
ceiling of 1, and **a hired ask never hires**. This closes the two failures the
review found: an anonymous caller labelling everything `reason` to spend a
node's paid key, and two brainless nodes with `swarm:ask` in their route looping
through Prime.

**`swarm:ask`** is the open `ask.broadcast` lane answered by serving nodes. It
is **free and rate-limited per requester at the serving node** in this ADR.
Pricing it is ADR-0043's market (NIP-90 DVMs, sats or ecash, steward before
wallet) and is not decided here; ADR-0045's "hireable ask" language in an
earlier draft was wrong and is withdrawn.

**Route events.** Every routed answer publishes `{route, provider, model,
tokens, cost_usd, hops}` on `KANNAKA.events.llm.route`. `KANNAKA.events.>` is
readable by anonymous users, so this is metadata by design and **never carries
prompt text** (the 48-character prompt preview the activity publisher emits is
a precedent not to copy).

### 4. What the microVMs are for

Firecracker (kax-computer v0.11) is not for hosting models; there is no GPU.
Its jobs, in the order they pay back:

1. **CI for onboarding itself.** Every installer and `kannaka-node` release runs
   `provision.sh all` inside a fresh microVM and must reach `verify ok`. What
   exists today: a rootfs built from the kax-machine image, guests that speak
   vsock JSON, no ssh, no Ubuntu or Oracle Linux image, no `.github` in
   kax-computer. What this needs: a kernel, two guest images with sshd and tap
   networking, and a job; GitHub-hosted Ubuntu runners expose `/dev/kvm`, so it
   can run hosted, with debain2 as the self-hosted fallback. The job runs with
   `--no-swarm` or against a throwaway bus so a CI VM never lands in the
   production presence roster, and `verify` gains `--expect-joined`, which
   FAILs on a missing join line, missing presence, or an authorization
   violation. Without a real bus the test proves install and service, not
   membership, and says so.
2. **A jail for agent tools.** `kannaka agent` exposes `bash`, `write_file` and
   `edit_file` to a model on the member's own host. Tool execution moves into a
   per-session microVM with the working directory mounted; the host process
   only relays.
3. **Compute for hired work.** Hired work (ADR-0043 market) runs inside a VM
   with the caller's budget as its ceiling and no host exposure.
4. **Try-before-install.** A throwaway node per newcomer, alive for an hour,
   joined anonymously, so the next person meets the swarm before touching a
   server.

## Consequences

- A newcomer's path is three commands and one code: install, `init`, `service`.
  The five-secret scavenger hunt ends.
- Revocation liveness, stated per item: NATS seat, immediate under auth callout
  (until then, a reload on each of three nodes); brain key, deleted at the
  gateway; hub token, refused at the next request; KAX token, at the next
  refresh once KAX keeps a revocation list.
- The portal holds the mapping pass → {agent id, NATS seat, hub token, brain
  key, KAX token}. The seat is member-scoped: it can serve and publish as its
  id, cannot create streams, cannot write the roster KV, and carries no memory
  trust.
- `[llm]` single-slot configs keep working through the `(base_url, model)`
  migration; a fleet node with hand-issued credentials keeps working untouched.
- Routing adds one table lookup per ask and one metadata event per answer.
- Kannaktopus's original multi-LLM role lands inside the core; `init` stops
  offering to install it. Its own consensus-gate design (many providers, an
  agreement rule) is a different thing from kind-routing and is not absorbed.
- `role` stays cosmetic; changing its default is hygiene, not a fix.

## Work items (one issue each, linked from this ADR's PR)

- **ninja-portal** (#9) — claim carries the agent id; refuse reserved
  prefixes; per-id subjects in the grant; hub token and KAX token in the
  bundle; NATS auth callout for pass users (interim: managed block on all three
  nodes); rate-limit `claim/start`.
- **kannaka-memory** (#930) — `init`: claim-code flow, merge semantics, keep
  the id, temp-and-rename writes, `enabled` flags only after their secret
  exists, portal-down path, member defaults, drop the Kannaktopus prompt and
  the phantom `ghostsignals register` hint; fix the presence warning (#928).
- **kannaka-memory** (#931) — `[llm.providers]` + `[llm.route]`, migration by
  `(base_url, model)`, `schema = 2`, per-provider budgets and timeouts, served
  asks pinned local, `hops`, `KANNAKA.events.llm.route` metadata only.
- **kannaka-plugin** (#25) — `--brain local` installs the manifest-pinned
  brain tier; memory floor from the manifest; writes
  `providers.kannaka-brain`; #24 (temp + mv) merged.
- **kannaka-radio** (#303) — hub issues a bearer per trader row, refuses
  unauthenticated trades on rows that have one, reads `agent_id`, accepts
  pass-minted tokens; `kax:pass:<id>` principal.
- **flaukowski/skills (kannaka-node)** (#2) — `configure`/`credentials` →
  `init`; `verify --expect-joined`; Firecracker CI job with the runner and
  images named.
- **kax-computer** (#8) — two ssh-capable guest images and the CI job; the
  agent-tool jail runner.

## Not decided here

Which paid providers ship adapters first (the table is the contract; adapters
follow demand). Pricing `swarm:ask` (ADR-0043's market). Whether
try-before-install nodes are anonymous or carry a guest pass. Kannaktopus's
install story once its multi-LLM role moves here. Whether ADR-0042 Phase 1c's
PUBLIC/INTERNAL account split ships with auth callout or after it.
