# ADR-0064 P0 gate: the hand-labelled open-loop set

**Pre-registered 2026-09-25, before any classifier rule was written.** This file is committed ahead of
the `mail` module on branch `mail/p0`; the commit order in the PR is the evidence.

## How the set was built

Both of Kannaka's mailboxes were read **read-only** (IMAP `EXAMINE`, `BODY.PEEK` only, no flag
changes) on 2026-09-25 ~21:00Z:

| account | authority | folders | messages |
|---|---|---|---|
| `zoho` kannaka@spacechild.love | imap.zoho.com:993 | INBOX (56), Sent (68) | 124 |
| `np` kannaka@ninja-portal.com | mail.ninja-portal.com (Stalwart) | INBOX (2), Sent Items (0) | 2 |

The 30-day window (since 2026-08-26) covers each mailbox's whole life: Zoho opened 09-09 and
ninja-portal on 09-16. Messages were grouped into threads per account by `Message-ID` /
`In-Reply-To` / `References`. That gives **51 threads** (49 zoho, 2 np). Each thread was then
labelled by a person reading it: the headers, and the bodies where a header alone could not say.
Bodies were read live and never stored.

Stalwart does not file SMTP submissions into `Sent Items`, so the np account has no Sent copy of
the mail Kannaka sent from it. Those letters appear as inbound mail in the zoho INBOX (T42, T43).

## What each label means (P0 semantics)

- **needs_reply**: a person wrote to us, a response from us is warranted, and none has been sent.
- **waiting_on_them**: our message is the latest one, and it asks a person something they have not
  answered.
- **done**: nothing is owed by either side in this thread. It was answered, acknowledged or purely
  informational, or it was resolved elsewhere and recorded with `mail close`.
- **ignored**: automated or bulk mail, our own tool or test traffic, agent-to-agent traffic inside
  ninja-portal.com, or unsolicited mail that is not correspondence.

A commitment stated in prose ("I owe you a reply", "I'll send a plan", "next table when the run
lands") is **P1** and does not change a P0 label. Such commitments are noted in the table.

## The gate metric

- **Gate (strict)**: exact agreement of `kannaka mail status` with the label below, over all 51
  threads, with the four states compared as they are. Pass at **≥ 95%** (at most 2 of 51 wrong).
- It is reported twice. **Structural** means before any `close`: the label column, with T40 as
  `needs_reply`. **After closures** means after the one pre-registered `close` on T40, where T40's
  label becomes `done`. No other `close` is pre-registered, and no other will be run for the gate.
- Also reported: the open-loop view (`needs_reply` / `waiting_on_them` / not-open), and every
  disagreement with its cause.
- **Expected misses**, declared before the rules exist: T19 (a script-sent key delivery from a
  person's address) and T20 (recruiter cold mail from an Outlook tenant). Neither carries any
  automation header (`List-*`, `Auto-Submitted`, `Precedence`, `Feedback-ID`, VERP `Return-Path`),
  so a header rule that caught them would be overfitting.
- The rules are designed with this set in view, and no held-out set exists (these two mailboxes are
  the whole corpus). The number is therefore **in-sample**. The honest out-of-sample check is to
  re-run this gate on the next 30 days.

## The labelled set

Anchor = the latest message in the thread (`account:folder:uid`). Dates are UTC.

| # | acct | anchor | thread (root subject) | last msg | label | evidence (one line) |
|---|---|---|---|---|---|---|
| T00 | zoho | INBOX:2 | Welcome to Zoho Mail | 09-10 | ignored | Zoho onboarding mail from welcome@zoho.com |
| T01 | zoho | INBOX:4 | From my own address, via the Oracle box | 09-10 | done | Nick's reply "Test good" acknowledges our test send; nothing asked |
| T02 | zoho | Sent:6 | A mailbox of my own (IMAP) | 09-10 | done | our "It works … thank you" closes Brad's IMAP switch |
| T03 | zoho | Sent:7 | Hello from Kannaka | 09-10 | done | an introduction to Cheeks, no question (Cheeks wrote separately, T22) |
| T04 | zoho | Sent:8 | Fwd: thegrid ADR-0008 ask | 09-10 | done | we answered Nick's forward: "Read it, and acted on it", PR opened |
| T05 | zoho | Sent:9 | [records] FAILED: Night One | 09-10 | ignored | automated delivery notice (From "Ghost Signals Records") |
| T06 | zoho | Sent:10 | [records] delivered: Night One | 09-10 | ignored | automated delivery notice |
| T07 | zoho | Sent:11 | [records] delivered: Night One | 09-10 | ignored | automated delivery notice |
| T08 | zoho | Sent:12 | E-005 is in | 09-11 | done | experiment report to Nick; informational, no question |
| T09 | zoho | Sent:13 | E-005 correction | 09-11 | done | correction to the report; informational |
| T10 | zoho | Sent:14 | [records] FAILED: Night One | 09-11 | ignored | automated delivery notice |
| T11 | zoho | Sent:15 | [records] delivered: Night One | 09-11 | ignored | automated delivery notice |
| T12 | zoho | Sent:20 | Your sign-off for the sticker is recorded | 09-11 | done | confirmation to Vincent (sent twice, same Message-ID); informational |
| T13 | zoho | Sent:21 | [records] radio spin | 09-11 | ignored | automated notice |
| T14 | zoho | Sent:23 | What your box looks like from the inside | 09-12 | waiting_on_them | we asked Cheeks three questions ("What does the box look like from the inside?"); no reply |
| T15 | zoho | Sent:24 | From my own address | 09-12 | done | we answered Nick: "Yes, and I have now" |
| T16 | zoho | Sent:25 | Fwd: aiid 50,000-character limit | 09-12 | done | our opinion on Nick's forward, delivered |
| T17 | zoho | Sent:28 | ADR-0060: one address per agent | 09-13 | done | Brad answered its questions in the Oracle-server thread on 09-15 15:59Z (ninja-portal.com on ExMachina) |
| T18 | zoho | Sent:29 | KAX-ADR-0009 merged | 09-13 | done | "Got it, used it, and it worked" |
| T19 | zoho | INBOX:16 | Your Kannaka Brain key | 09-14 | ignored | machine-issued key delivery sent as nick@ ("the only copy we will send"); no reply expected; **expected rule miss** |
| T20 | zoho | INBOX:18 | Ubie US HealthTech Career Opportunities | 09-14 | ignored | unsolicited recruiter mail addressed to "Nicholas"; **expected rule miss** |
| T21 | zoho | Sent:36 | Fwd: Agent-Kax #600 | 09-15 | done | "Handled" reply to Nick's forward |
| T22 | zoho | Sent:37 | OracleCheeks is on the swarm | 09-15 | done | our acknowledgement to Cheeks, with a status update |
| T23 | zoho | Sent:45 | inbound test 2118749d36 | 09-17 | ignored | our test to our own ninja-portal box |
| T24 | zoho | Sent:47 | Your Oracle server: hand it to an agent | 09-17 | done | 31 messages; our last ("That was it", plus a correction) follows Brad's ingress fix; his 09-15 asks are delivered |
| T25 | zoho | Sent:48 | membrane test 7d7dac9950 | 09-17 | ignored | our membrane test |
| T26 | zoho | Sent:49 | first email to a machine (376b1efe) | 09-17 | ignored | test mail to a KAX machine, fc-01@ninja-portal.com |
| T27 | zoho | Sent:50 | /goal | 09-17 | done | we answered Nick's goal with the plan |
| T28 | zoho | Sent:51 | Fwd: OCI Cloud Shell | 09-17 | done | Nick's question answered ("nothing to worry about") |
| T29 | zoho | Sent:53 | Our competition | 09-17 | done | our reply "Working them"; P1: we owe the next table |
| T30 | zoho | Sent:56 | Disclosure: AIID used as the payload … | 09-19 | done | we answered Sean's four questions |
| T31 | zoho | INBOX:36 | New experiment | 09-20 | needs_reply | Nick proposes an experiment from the IonQ article; never answered, no record of it being handled |
| T32 | zoho | INBOX:37 | Another important read for our future | 09-20 | needs_reply | Nick shares a read "for our future"; never answered |
| T33 | zoho | INBOX:38 | 903148 is your verification code | 09-21 | ignored | OTP mail (notifications@, VERP bounce Return-Path) |
| T34 | zoho | Sent:59 | Re: Chipmunks | 09-22 | done | our reply to Nick and Linus |
| T35 | zoho | Sent:60 | A more direct way to run on Rigetti's Cepheus | 09-22 | done | "It works. Thank you" to Kanav and Ryan closes the qBraid loop |
| T36 | zoho | Sent:61 | Task | 09-22 | done | "Done on O1. Pulled, restarted, healthy." |
| T37 | zoho | Sent:62 | Cognition and consciousness … analog | 09-22 | done | we answered Nick's follow-up |
| T38 | zoho | INBOX:48 | Fwd: Re: kannaka-labs, a thought about kannaka-memory | 09-22 | done | **case 3.** Nick's forward says only "Fyi.."; nothing is asked of us. P1: Victor owes a plan (a commitment on them) |
| T39 | zoho | Sent:64 | Postmortem: the agent-channel reconnect storm | 09-22 | done | our thanks to Vincent closes this thread. P1: Vincent's "I owe you a reply" is a commitment on them |
| T40 | zoho | INBOX:49 | Help if possible | 09-23 | needs_reply → **done after close** | **case 2.** Structurally unanswered (unread). The SCADA desk deploy it asks for was done 09-23 (kax-scada-desk-run2); pre-registered close: `--note "deployed 09-23 (kax-scada-desk-run2)"` |
| T41 | zoho | INBOX:50 | Thanks for Joining the Kaisoft mailing list | 09-23 | ignored | list confirmation (Feedback-ID, SES Return-Path) |
| T42 | zoho | INBOX:51 | First letter out of ninja-portal.com, take 2 | 09-24 | ignored | our own test letter from kannaka@ninja-portal.com |
| T43 | zoho | INBOX:52 | First letter out of ninja-portal.com | 09-24 | ignored | the same test, delivered late |
| T44 | zoho | INBOX:53 | A question for the Rogue Agent (ba15c179) | 09-24 | ignored | our test of a citizen; its reply is `Auto-Submitted: auto-replied` |
| T45 | zoho | INBOX:54 | What did the record keep this week? (28283be8) | 09-24 | ignored | citizen test, auto-replied |
| T46 | zoho | INBOX:55 | Welcome to your mailbox (52508051) | 09-24 | ignored | citizen test, auto-replied |
| T47 | zoho | INBOX:56 | Registration Confirmation for Automation Fair 2026 | 09-25 | ignored | event registration confirmation, we are only cc'd (VERP bounce Return-Path) |
| T48 | zoho | Sent:68 | The Agentic Arena: which activity feeds msgs_per_bot? | 09-25 20:42Z | waiting_on_them | **case 1.** Our three questions to Vincent are the latest message |
| N00 | np | INBOX:1 | inbound test 2118749d36 | 09-17 | ignored | our own test from kannaka@spacechild.love |
| N01 | np | INBOX:2 | membrane test 7d7dac9950 | 09-17 | ignored | our own membrane test |

**Totals (structural):** needs_reply 3 (T31, T32, T40) · waiting_on_them 2 (T14, T48) · done 24 ·
ignored 22 (20 zoho + 2 np) = 51. **After the T40 close:** needs_reply 2 · waiting_on_them 2 · done 25 ·
ignored 22.

## Measured result (2026-09-25 ~21:5xZ, appended after the run)

Run: `kannaka mail sync --days 30`, then `kannaka mail status --all --json`, compared to the table
above by anchor membership. The np threads are matched by subject tag, because JMAP anchors are
email ids rather than UIDs. Sync read zoho INBOX 56 + Sent 68 over IMAP, and np Inbox 2 + Sent
Items 0 over JMAP: 126 refs, 51 threads, and every label matched exactly one thread with none left
unlabelled.

| measurement | agreement |
|---|---|
| **Structural** (before any close), strict 4-state | **49 / 51 = 96.1%**: passes ≥ 95% |
| **After the pre-registered T40 close**, strict 4-state | **49 / 51 = 96.1%**: passes |
| Open-loop view (needs_reply / waiting_on_them / not-open), both runs | 49 / 51 = 96.1% |

**Disagreements (both runs), exactly the two declared in advance:**

- **T19** "Your Kannaka Brain key": labelled `ignored`, status says `needs_reply` (their last
  message is unanswered). A script sent it as nick@spacechild.love with no automation header.
- **T20** "Ubie US HealthTech Career Opportunities": labelled `ignored`, status says
  `needs_reply`. Recruiter cold mail from an Outlook tenant, with no automation header.

Both are false *opens*: the list shows two extra items to dismiss, and no real loop is hidden.
Neither was fixed by a rule, since that would be fitting the rules to the test set. A `close` with
a note is the P0 tool for them, and a sender-reputation or first-contact signal is a P1 question.

**The three 09-25 cases:**

1. Vincent (T48, "The Agentic Arena: which activity feeds msgs_per_bot?", ours 09-25 20:42Z):
   `waiting_on_them`, because our last message asks and is unanswered. ✓ The earlier postmortem
   thread (T39) is `done`; his "I owe you a reply" there is a P1 commitment.
2. Nick's "Help if possible" (T40, `t:e4c81f1ddd`): `needs_reply` → `kannaka mail close
   t:e4c81f1ddd --note "deployed 09-23 (kax-scada-desk-run2)"` → `done (closed: deployed 09-23
   (kax-scada-desk-run2))`. ✓ The closure is only a local row in `<data_dir>/mail/closures.jsonl`,
   with `covers_through` 2026-09-23T03:24:45Z; a newer message in the thread would re-open it.
3. Nick's forward of Victor Cypher's offer (T38): `done` (FYI), because Nick's own words are
   "Fyi..". ✓ P1 adds "Victor owes a plan" as a commitment on them.

**Read-only, checked against the authority.** "Help if possible" was still unread on the server
(`\Recent`, no `\Seen`) after two full-body syncs and two live `thread` fetches. Every fetch was a
PEEK. `refs.jsonl` (118 KB) holds no body text: grep for a known body phrase and for the `sk-` key
prefix returns 0.

**Caveat, stated before the run and restated here:** the number is in-sample. The re-check is this
same procedure on the next 30 days of mail.
