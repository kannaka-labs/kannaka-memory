# ADR-0059 — One claim, one identity: onboarding a node, and routing asks across brains

**Status:** Proposed
**Date:** 2026-09-11
**Author:** Kannaka / Nick Flach
**Relates to:** ADR-0057 (open-weight brain), ADR-0042 (swarm auth), ADR-0012 (Ghost Signals hub), ADR-0045 (hive organs: `ask` is hireable), kax-computer v0.11 (Firecracker), the `kannaka-node` skill (flaukowski/skills)

## Context

The first person outside the project to stand up a node did it on 2026-09-10/11:
Brad, on an Ubuntu box in Oracle Cloud, with the public installer, the TUI, and
mail to Kannaka when stuck. His shell history and config are the evidence for
this ADR. What he had to do, and what did not work:

| step | what happened |
|---|---|
| install | worked; `kannaka` then "not found" because the PATH line went to `~/.bashrc` and his open shell never re-read it |
| Constellation Pass | `--claim-only` was run; "Link Kannaka.command" is still in his home directory, so the link never completed |
| Ghost Signals | `[ghostsignals] enabled = true, token = ""`; nothing in his history touches it, because there is no command that would |
| brain | the installer's `--brain local` pulls a generic 7B, not hers; he installed ollama, cloned the GGUF from HuggingFace, wrote the Modelfile and built `kannaka-brain` himself, six commands |
| identity | `kannaka init` defaulted `[swarm] role = "queen"` and `kind = "human"` for a member node |
| swarm | joined anonymously; the node's own log said it would not appear in peer lists, which is false when the presence stream exists (kannaka-memory #928) |
| re-install | the installer downloads onto the live binary; `ETXTBSY` made it delete `kannaka`, and on the next run `kannaka-tui`, while his TUI was open (kannaka-plugin #23, fix #24) |
| store | two TUI sessions and the new service wrote the same `.hrm` until he closed them |
| Kannaktopus | attempted on another machine; did not install (no evidence on this box) |

He needed **five secrets from three claim flows** to be "fully in": a pass, a
swarm credential (`NATS_USER/NATS_PASSWORD`, issued by hand), a Ghost Signals
token, a KAX token, and a brain key. The one flow he completed, the install,
mints none of them. Everything else on that list is a symptom of the same
thing: onboarding is a set of parts, not a path.

Two adjacent asks arrived with this: Kannaktopus began as a multi-LLM tool and
the constellation should let a person keep their existing keys (Anthropic,
OpenAI) while `kannaka-brain` stays the base; and the Firecracker microVMs
that kax-computer v0.11 made real have no stated job.

## Decision

### 1. One claim mints the whole identity

A pass claim returns a **bundle**, not a brain key: the swarm credential, a
Ghost Signals hub token, the brain key, and the KAX agent token when the pass
tier includes it. The portal already mints the brain key on claim; this adds
the other three to the same response, each scoped to the pass and revocable
with it. The swarm credential is a per-pass NATS user in the existing auth
model (ADR-0042), member permissions only; `serve` stays an operator grant.

`kannaka init` becomes **the** onboarding step. It asks for the short claim
code (the device-code flow the installer already prints), receives the bundle,
and writes `config.toml`, `~/.kannaka-nats.env` and the token fields itself,
0600. No secret is ever typed into a terminal or pasted into mail; Brad had to
do both.

Defaults change to what a member is: `[swarm] role = "worker"`,
`[agent] kind = "agent"`. `queen` is never a default.

Provisioning a server (`kannaka-node`) reduces to: install, `init` with the
code, `service`. The skill's `configure`/`credentials` steps become `init`.

### 2. The installer installs *her* brain

`--brain local` installs `kannaka-brain` (the published GGUF and Modelfile
from ADR-0057 P2), not a generic model, and refuses on hosts under the
model's memory floor with the hosted option printed as the alternative.
`--brain hosted` stays the default for small boxes. Both write
`[llm.providers.kannaka-brain]`, below.

The installer never writes onto a live binary (kannaka-plugin #24) and
`kannaka-node` refuses to re-run it while a `kannaka` process runs.

### 3. Many providers, one base, routing by kind of ask

`[llm]` becomes a table of providers with `kannaka-brain` always present:

```toml
[llm]
default = "kannaka-brain"

[llm.providers.kannaka-brain]
kind = "ollama"            # or "gateway" for the hosted key
base_url = "http://localhost:11434"
model = "kannaka-brain"

[llm.providers.anthropic]
kind = "anthropic"
model = "claude-sonnet-5"
api_key_env = "ANTHROPIC_API_KEY"

[llm.providers.openai]
kind = "openai"
model = "gpt-5"
api_key_env = "OPENAI_API_KEY"

[llm.route]
voice     = ["kannaka-brain", "swarm:ask"]
reason    = ["anthropic", "openai", "swarm:ask", "kannaka-brain"]
tools     = ["anthropic", "openai"]
cheap     = ["local-small", "kannaka-brain"]
```

The router chooses by the **kind of ask**, not by a global ranking:

- **recall** never goes to a model; it is the HRM.
- **voice** (introspection, memory-grounded answers, anything that speaks as
  Kannaka) goes to `kannaka-brain`, because that is what it was tuned on.
- **reason** (code, long chains, planning) goes to the strongest key the user
  added; `tools` requires a provider that supports tool use.
- **cheap** (classification, summaries, triage) goes to the smallest local
  model.

Each list is tried in order: **local first, then the swarm, then paid**.
`swarm:ask` is the hive organ market of ADR-0045, where `ask` is already a
hireable capability; a node without a GPU routes her voice to Prime instead of
to a cloud key. A provider with no key is skipped, never an error at init.

Every routed answer carries `{route, provider, model, tokens, cost}` and is
published on `KANNAKA.events.llm.route` so the choice is observable across the
swarm and the route table can be corrected from evidence, not taste.

### 4. What the microVMs are for

Firecracker (kax-computer v0.11) is not for hosting models; there is no GPU.
Its jobs, in the order they pay back:

1. **CI for onboarding itself.** Every installer and `kannaka-node` release
   runs `provision.sh all` against a fresh microVM (Ubuntu and Oracle Linux
   images) and must reach `verify ok`. That test catches both binary
   deletions above before a person does.
2. **A jail for agent tools.** `kannaka agent` exposes `bash`, `write_file`
   and `edit_file` to a model on the member's own host. Tool execution moves
   into a per-session microVM with the working directory mounted; the host
   process only relays.
3. **Compute for hired work.** A hired `ask` or job (ADR-0045) runs inside a
   VM with the caller's budget as its ceiling and no host exposure.
4. **Try-before-install.** A throwaway node per newcomer, alive for an hour,
   joined anonymously, so the next person meets the swarm before touching a
   server.

## Consequences

- A newcomer's path is three commands and one code: install, `init`, `service`.
  The five-secret scavenger hunt ends; revocation is one pass.
- The portal holds the mapping pass → {NATS user, hub token, brain key, KAX
  token}. Revoking a pass revokes the node's seat, its trading identity and
  its brain budget together. That is a feature and a blast radius; the NATS
  user is member-scoped and cannot create streams or serve.
- `[llm]` single-slot configs keep working: a lone `provider/model` pair is
  read as `providers.<provider>` and made the default.
- Routing adds one decision per ask and one event per answer; the router is a
  table lookup, not a model call.
- Kannaktopus's original multi-LLM role lands inside the core instead of in a
  separate install; what Kannaktopus keeps is the orchestrator surface.
- Nothing here changes how the fleet's existing nodes are configured; their
  hand-issued credentials remain valid.

## Work items (one issue each, linked from this ADR's PR)

- **ninja-portal** — pass claim returns the bundle; per-pass NATS user in the
  swarm auth config; hub token minting; revoke-on-pass-revoke.
- **kannaka-memory** — `kannaka init` claim-code flow that writes the bundle;
  member defaults; `[llm.providers]` + `[llm.route]` + the router;
  `KANNAKA.events.llm.route`; fix the presence warning (#928).
- **kannaka-plugin** — `--brain local` installs `kannaka-brain`; memory-floor
  check; #24 (temp + mv) merged.
- **kannaka-radio** — hub accepts a pass-minted Ghost Signals token as a
  trader identity; the `[ghostsignals] token` field means something.
- **flaukowski/skills (kannaka-node)** — `configure`/`credentials` → `init`;
  Firecracker CI job that runs `provision.sh all` on release.
- **kax-computer** — agent-tool jail: a per-session microVM runner the agent
  backend can target.

## Not decided here

Which paid providers ship adapters first (the table is the contract; adapters
follow demand). Whether `try-before-install` nodes are anonymous or carry a
guest pass. What Kannaktopus's install story becomes once its multi-LLM role
moves here.
