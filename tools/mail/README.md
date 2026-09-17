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

Not yet: per-agent durable pull consumers (`mail-<slug>`) and the consumer-side seats
(subscribe `KANNAKA.mail.<slug>.>` only), the outbound half (`.sent`/`.bounce`, needs
the relay provider), attachment blobs.
