# kannaka-mail — the mail membrane (ADR-0062, first slice)

`membrane.py` runs on the mail host (Stalwart on ExMachina, `mail.ninja-portal.com`)
as the `kannaka-mail` system user and turns every agent's inbound mail into a bus
event: one JetStream message per accepted message per recipient on
`KANNAKA.mail.<slug>.inbound`, stream `KANNAKA_MAIL_V2`, payload as ADR-0062 §4
(verdict first — `auth`, `from_authenticated` — body last, `untrusted: true`
always, `+tag` extracted, attachments as references).

How it differs from the ADR's receiver: the ADR publishes before it answers `250`
and says `451` when it cannot. A poller cannot refuse mail that Stalwart already
accepted, so this is at-least-once with a content-derived `Nats-Msg-Id`
(`sha256(slug | envelope_from | sha256(raw))`; the stream's 7-day duplicate
window makes a retry idempotent) and a per-mailbox IMAP UID watermark that only
advances after the PubAck. Nothing is marked `\Seen` — that is the agent's own
tool's business.

Deploy (ExMachina, 2026-09-17): `/opt/kannaka-mail/{membrane.py,venv}` (nats-py),
unit `kannaka-mail-membrane.service`, `EnvironmentFile=/etc/kannaka-secrets/mail-membrane.env`
(`NATS_USER=mail-membrane`, publish-only seat on `KANNAKA.mail.*.inbound`),
agent credentials as `/etc/kannaka-secrets/agents/<slug>.secret` (`user:password`,
root:kannaka-mail 0640), state `/var/lib/kannaka-mail/state.json`. Stream created on
the bus with `nats stream add KANNAKA_MAIL_V2 --subjects 'KANNAKA.mail.>' --storage file
--discard new --max-age 90d --max-msgs-per-subject 2000 --max-bytes 1GB --max-msg-size 1MB
--dupe-window 168h --deny-delete --deny-purge`.

Proven 03:16Z: a message sent from Zoho to `kannaka+bus@ninja-portal.com` was on the
bus 10 s later with `from_authenticated: true` (dkim=pass d=spacechild.love, spf=pass on
MAIL FROM), `tag: "bus"`, `untrusted: true`.

## Reading: one seat per agent (`mail-seat.py`)

⚠ **2026-09-24: the mail stream was readable anonymously.** `anon` held
`$JS.API.STREAM.MSG.GET.>`, which covers `KANNAKA_MAIL_V2`; an anonymous client fetched
message 1. Core subscribe on `KANNAKA.mail.>` was already denied, but a direct fetch by
sequence is not a subscription. Fixed on all three servers: every seat except
`kannaka_internal` and `mail-membrane` now carries a publish deny on MSG.GET, DIRECT.GET and
the consumer API for this stream, and a subscribe deny on `KANNAKA.mail.>`. **A seat added
to the config later does not inherit that deny** — scope it narrower or add it.

An agent reads its own mail only through its own seat:

    # on O1, with the operator seat in the environment
    set -a; . ~/.kannaka-nats.env; set +a
    python3 tools/mail/mail-seat.py <slug> [--dry-run] [--rotate]

This creates the durable pull consumer `mail-<slug>` (filter `KANNAKA.mail.<slug>.>`,
DeliverAll, explicit ack, so mail that arrived earlier is waiting), adds a NATS user
`mail-<slug>` whose only JetStream rights are MSG.NEXT, INFO and ACK on that one consumer,
validates and hot-reloads the config (backup kept), and writes the seat's credentials to
`/etc/kannaka-secrets/mail-seats/<slug>.env` (root, 0600) for delivery to the agent's host.
The agent binds to the existing consumer:

    js.pull_subscribe("KANNAKA.mail.<slug>.>", durable="mail-<slug>", stream="KANNAKA_MAIL_V2")

Proven 2026-09-24 with `mail-kannaka`: its own two messages pulled; `mail-rogue`'s consumer
and a direct stream fetch both refused with a permissions violation.

Seats minted 2026-09-24 for the six OBC citizens (kannaka, rogue, the-archivist, ghost-signal,
gossipghost-01, and 0xscada-qe once Nick gave it a mailbox the same day), delivered to each instance's `mail-seat.env` on
debain2, and read by rogue-agent's `read_mail()` (rogue-agent PR #11).

## A new mailbox (`new-mailbox.py`)

    sudo new-mailbox <slug>        # on ExMachina (installed at /usr/local/sbin/new-mailbox)

creates the Stalwart `User` account (domain `b`, description `agent`; `emailAddress` is server-set
from name + domain and is refused if sent), and writes `address:password` to
`/etc/stalwart/agents/<slug>.secret` (0600) and `/etc/kannaka-secrets/agents/<slug>.secret`
(root:kannaka-mail 0640), where the membrane reads it every round -- no membrane restart.
Then `mail-seat.py <slug>` on O1 for the read seat, and deliver both credential files to the
agent's host. Issuing an address creates an agent identity (ADR-0060): Nick decides who gets one.

## Sending: Resend relay (`stalwart-enable-resend.sh`)

Oracle blocks outbound :25, so Stalwart cannot deliver to a remote MX. Outbound goes through
Resend (domain ninja-portal.com, us-east-1). DNS in the zone: `resend._domainkey` TXT (DKIM),
`send` MX + SPF TXT (Resend's return path, so the apex `v=spf1 mx -all` is untouched), `rsend`
CNAME. The apex publishes DMARC `p=reject`: nothing relayed delivers until DKIM verifies.

    printf '%s' 're_…' | sudo stalwart-enable-resend     # on ExMachina; --status, --disable

stores the key in `/etc/stalwart/resend.key` (root:stalwart 0640), creates the Relay route
`resend` (smtp.resend.com:465, implicit TLS, user `resend`, `authSecret` File), sets the
outbound strategy `is_local_domain(rcpt_domain)` → `'local'` else `'resend'`, and **restarts
Stalwart** — a strategy changed through the admin API is not used by the running queue (a letter
queued after the change still went straight to the recipient's MX). v0.16 notes:
`is_local_domain` takes one argument; a route's `name` is read-only on update.

`stalwart-jmap.py <method> '<json>'` is the admin helper (root, reads `/etc/stalwart/admin.secret`,
masks secrets). To learn an object's schema without creating anything, send a create with an
extra bogus property: the server rejects the patch and names only what it could not accept.

Proven 2026-09-24 16:28:04Z: Kannaka (kannaka@ninja-portal.com) → kannaka@spacechild.love,
delivered through smtp.resend.com in 1 s, Resend `last_event: delivered`; local delivery
re-checked after the switch.

Not yet: `.sent`/`.bounce` events on the bus, agents composing and sending mail themselves
(the citizens only read), attachment blobs.
