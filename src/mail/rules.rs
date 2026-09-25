//! Threads and open-loop states: deterministic rules only (ADR-0064 §3, P0).
//!
//! A thread's state is computed from its MailRefs, the server's flags, and
//! closure records — never recalled. Rule order, first match wins:
//!
//! 1. **closure**: a `mail close` record whose horizon covers the thread's
//!    newest person-facing message → `done`. A newer message re-opens it.
//! 2. **ignored**: no person-facing message at all. A message is not
//!    person-facing when it is *internal* (every participant is one of our
//!    addresses or an internal domain) or *automated* (List-Id,
//!    List-Unsubscribe, Auto-Submitted ≠ no, Precedence bulk/list/junk,
//!    Feedback-ID, a VERP/bounce Return-Path, a noreply-style sender; or, for
//!    our own mail, Auto-Submitted or a tool sending under another display name).
//! 3. the newest person-facing message is **ours** → `waiting_on_them` when our
//!    own words ask a question, else `done` (we answered, informed, or thanked).
//! 4. it is **theirs** → `done` when the server says `\Answered`, when it is an
//!    FYI, or when it is a short question-free acknowledgement of one of our
//!    messages; otherwise `needs_reply`.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::store::Closure;
use super::{MailRef, Role};

/// Longest own-text (chars) a reply may have and still count as a bare
/// acknowledgement ("Test good", "Thanks!").
pub const ACK_MAX_CHARS: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum State {
    NeedsReply,
    WaitingOnThem,
    Done,
    Ignored,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::NeedsReply => "needs_reply",
            State::WaitingOnThem => "waiting_on_them",
            State::Done => "done",
            State::Ignored => "ignored",
        }
    }
    pub fn is_open(&self) -> bool {
        matches!(self, State::NeedsReply | State::WaitingOnThem)
    }
}

/// Who "we" are, for the rules.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Every address of this agent (all accounts), lowercased.
    pub self_addresses: HashSet<String>,
    /// Domains whose mail is agent-to-agent traffic (e.g. `ninja-portal.com`).
    pub internal_domains: Vec<String>,
    /// account name → the display name this agent sends under.
    pub display_names: HashMap<String, String>,
}

impl Policy {
    pub fn is_self(&self, email: &str) -> bool {
        self.self_addresses.contains(&email.to_ascii_lowercase())
    }
    fn is_internal_addr(&self, email: &str) -> bool {
        if self.is_self(email) {
            return true;
        }
        let dom = email.rsplit('@').next().unwrap_or("").to_ascii_lowercase();
        self.internal_domains.iter().any(|d| d.eq_ignore_ascii_case(&dom))
    }
    pub fn is_ours(&self, m: &MailRef) -> bool {
        self.is_self(&m.from.email)
    }
    /// Every participant is us or an internal domain.
    pub fn is_internal(&self, m: &MailRef) -> bool {
        self.is_internal_addr(&m.from.email)
            && m.to.iter().chain(m.cc.iter()).all(|a| self.is_internal_addr(&a.email))
    }
    /// Why a message is automated, if it is.
    pub fn automated_reason(&self, m: &MailRef) -> Option<&'static str> {
        let a = &m.auto;
        let auto_sub = a
            .auto_submitted
            .as_deref()
            .map(|v| !v.trim().eq_ignore_ascii_case("no") && !v.trim().is_empty())
            .unwrap_or(false);
        if self.is_ours(m) {
            if auto_sub {
                return Some("our auto-submitted mail");
            }
            if let Some(ours) = self.display_names.get(&m.account) {
                if !m.from.name.is_empty() && !m.from.name.eq_ignore_ascii_case(ours) {
                    return Some("sent by a tool under another display name");
                }
            }
            return None;
        }
        if a.list_id.is_some() {
            return Some("List-Id");
        }
        if a.list_unsubscribe {
            return Some("List-Unsubscribe");
        }
        if auto_sub {
            return Some("Auto-Submitted");
        }
        if let Some(p) = a.precedence.as_deref() {
            let p = p.trim().to_ascii_lowercase();
            if matches!(p.as_str(), "bulk" | "list" | "junk" | "auto_reply") {
                return Some("Precedence");
            }
        }
        if a.feedback_id {
            return Some("Feedback-ID");
        }
        if let Some(rp) = a.return_path.as_deref() {
            let rp = rp.trim().trim_matches(|c| c == '<' || c == '>').to_ascii_lowercase();
            if rp.is_empty() {
                return Some("null Return-Path (bounce)");
            }
            let local = rp.split('@').next().unwrap_or("");
            if local.contains("bounce") || (local.contains('+') && local.contains('=')) {
                return Some("VERP Return-Path");
            }
        }
        let local = m.from.email.split('@').next().unwrap_or("");
        const NOREPLY: &[&str] = &[
            "noreply", "no-reply", "no_reply", "donotreply", "do-not-reply", "do_not_reply",
            "mailer-daemon", "postmaster", "notification", "notifications", "notify", "welcome",
            "bounce", "bounces",
        ];
        if NOREPLY.iter().any(|n| local == *n || (n.contains("reply") && local.starts_with(n))) {
            return Some("noreply-style sender");
        }
        None
    }
    pub fn is_person_facing(&self, m: &MailRef) -> bool {
        !self.is_internal(m) && self.automated_reason(m).is_none()
    }
}

/// One thread of one account.
#[derive(Debug, Clone)]
pub struct Thread<'a> {
    pub account: String,
    /// Root reference: the earliest message's first `References` id, else its
    /// `In-Reply-To`, else its own `Message-ID`.
    pub key: String,
    /// Short stable id, `t:` + 10 hex of blake3(account, key).
    pub id: String,
    /// Oldest first.
    pub messages: Vec<&'a MailRef>,
}

pub fn thread_id(account: &str, key: &str) -> String {
    let h = blake3::hash(format!("{account}\n{key}").as_bytes()).to_hex();
    format!("t:{}", &h[..10])
}

fn sort_key(m: &MailRef) -> (DateTime<Utc>, u8, u32) {
    (
        m.date.unwrap_or(DateTime::<Utc>::MIN_UTC),
        if m.role == Role::Inbox { 0 } else { 1 },
        m.uid.unwrap_or(0),
    )
}

/// Group refs into threads per account by Message-ID / In-Reply-To / References.
pub fn build_threads(refs: &[MailRef]) -> Vec<Thread<'_>> {
    let mut parent: HashMap<String, String> = HashMap::new();
    fn find(p: &mut HashMap<String, String>, x: &str) -> String {
        let mut cur = x.to_string();
        loop {
            let next = p.entry(cur.clone()).or_insert_with(|| cur.clone()).clone();
            if next == cur {
                break;
            }
            let grand = p.get(&next).cloned().unwrap_or_else(|| next.clone());
            p.insert(cur.clone(), grand.clone());
            cur = next;
        }
        cur
    }
    fn union(p: &mut HashMap<String, String>, a: &str, b: &str) {
        let (ra, rb) = (find(p, a), find(p, b));
        if ra != rb {
            p.insert(ra, rb);
        }
    }
    let node = |i: usize, m: &MailRef| format!("{}\n#{}", m.account, i);
    let idn = |m: &MailRef, id: &str| format!("{}\n{}", m.account, id);
    for (i, m) in refs.iter().enumerate() {
        let me = node(i, m);
        find(&mut parent, &me);
        let ids = std::iter::once(&m.message_id)
            .chain(m.in_reply_to.iter())
            .chain(m.references.iter())
            .filter(|s| !s.is_empty());
        for id in ids {
            union(&mut parent, &me, &idn(m, id));
        }
    }
    let mut groups: BTreeMap<String, Vec<&MailRef>> = BTreeMap::new();
    for (i, m) in refs.iter().enumerate() {
        let r = find(&mut parent, &node(i, m));
        groups.entry(r).or_default().push(m);
    }
    let mut out: Vec<Thread> = groups
        .into_values()
        .map(|mut msgs| {
            msgs.sort_by_key(|m| sort_key(m));
            let first = msgs[0];
            let key = first
                .references
                .first()
                .or(first.in_reply_to.first())
                .cloned()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    if first.message_id.is_empty() {
                        first.server_id.clone()
                    } else {
                        first.message_id.clone()
                    }
                });
            Thread { account: first.account.clone(), id: thread_id(&first.account, &key), key, messages: msgs }
        })
        .collect();
    out.sort_by_key(|t| t.messages.last().map(|m| sort_key(m)));
    out
}

/// The computed state of one thread.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadStatus {
    pub id: String,
    pub account: String,
    pub key: String,
    pub state: State,
    /// Which rule decided, in words.
    pub reason: String,
    pub subject: String,
    /// The message the state is about (newest person-facing, else newest).
    pub anchor: String,
    pub last_at: Option<DateTime<Utc>>,
    pub age_days: Option<f64>,
    /// The other side: the newest person-facing message's sender (theirs) or recipients (ours).
    pub counterpart: Vec<String>,
    pub messages: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_note: Option<String>,
    /// Every message's anchor, oldest first.
    pub members: Vec<String>,
}

/// Does `c` still cover a thread whose newest relevant message is at `horizon`?
pub fn closure_covers(c: &Closure, horizon: Option<DateTime<Utc>>) -> bool {
    match (c.covers_through, horizon) {
        (Some(through), Some(h)) => h <= through,
        _ => true,
    }
}

pub fn classify(t: &Thread, policy: &Policy, closures: &[Closure], now: DateTime<Utc>) -> ThreadStatus {
    let person: Vec<&MailRef> = t.messages.iter().copied().filter(|m| policy.is_person_facing(m)).collect();
    let newest = *t.messages.last().expect("a thread has at least one message");
    let focus = person.last().copied().unwrap_or(newest);
    let mk = |state: State, reason: String, note: Option<String>| {
        let counterpart = if policy.is_ours(focus) {
            focus.to.iter().chain(focus.cc.iter()).filter(|a| !policy.is_self(&a.email)).map(|a| a.email.clone()).collect()
        } else {
            vec![focus.from.email.clone()]
        };
        ThreadStatus {
            id: t.id.clone(),
            account: t.account.clone(),
            key: t.key.clone(),
            state,
            reason,
            subject: t.messages[0].subject.clone(),
            anchor: focus.anchor(),
            last_at: focus.date,
            age_days: focus.date.map(|d| ((now - d).num_minutes() as f64 / 1440.0 * 10.0).round() / 10.0),
            counterpart,
            messages: t.messages.len(),
            closure_note: note,
            members: t.messages.iter().map(|m| m.anchor()).collect(),
        }
    };

    // 1. closure (the operator's word), unless something newer arrived.
    let closure = closures
        .iter()
        .filter(|c| c.account == t.account && (c.thread_key == t.key || c.thread_id == t.id))
        .last();
    if let Some(c) = closure {
        if closure_covers(c, focus.date) {
            return mk(State::Done, format!("closed: {}", c.note), Some(c.note.clone()));
        }
    }

    // 2. nothing person-facing.
    let Some(last) = person.last().copied() else {
        let why = if t.messages.iter().all(|m| policy.is_internal(m)) {
            "internal traffic only".to_string()
        } else {
            let r = t.messages.iter().find_map(|m| policy.automated_reason(m)).unwrap_or("automated");
            format!("automated ({r})")
        };
        return mk(State::Ignored, why, None);
    };

    // 3. our message is the newest.
    if policy.is_ours(last) {
        return if last.own.question {
            mk(State::WaitingOnThem, "our last message asks and is unanswered".into(), None)
        } else {
            mk(State::Done, "our last message answers or informs".into(), None)
        };
    }

    // 4. theirs is the newest.
    if last.has_flag("\\Answered") {
        return mk(State::Done, "\\Answered on the server".into(), None);
    }
    if last.own.fyi {
        return mk(State::Done, "FYI".into(), None);
    }
    let ours: HashSet<&str> = t.messages.iter().filter(|m| policy.is_ours(m)).map(|m| m.message_id.as_str()).collect();
    let replies_to_us = last.in_reply_to.iter().any(|id| ours.contains(id.as_str()));
    if replies_to_us && !last.own.question && last.own.chars <= ACK_MAX_CHARS {
        return mk(State::Done, "acknowledgement of our message".into(), None);
    }
    mk(State::NeedsReply, "their last message is unanswered".into(), None)
}

/// Classify every thread.
pub fn status_all(refs: &[MailRef], policy: &Policy, closures: &[Closure], now: DateTime<Utc>) -> Vec<ThreadStatus> {
    build_threads(refs).iter().map(|t| classify(t, policy, closures, now)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::parse::OwnText;
    use crate::mail::{Addr, AutoHeaders};
    use chrono::TimeZone;

    const ME: &str = "kannaka@spacechild.love";

    fn policy() -> Policy {
        Policy {
            self_addresses: [ME, "kannaka@ninja-portal.com"].iter().map(|s| s.to_string()).collect(),
            internal_domains: vec!["ninja-portal.com".into()],
            display_names: [("zoho".to_string(), "Kannaka".to_string())].into_iter().collect(),
        }
    }
    fn at(day: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, day, h, 0, 0).unwrap()
    }
    fn now() -> DateTime<Utc> {
        at(25, 21)
    }
    fn addr(e: &str) -> Addr {
        Addr { name: String::new(), email: e.into() }
    }
    /// A message. `from == ME` makes it ours (and a Sent copy).
    fn msg(uid: u32, id: &str, reply_to: Option<&str>, from: &str, to: &str, when: DateTime<Utc>) -> MailRef {
        let ours = from == ME;
        MailRef {
            account: "zoho".into(),
            folder: if ours { "Sent".into() } else { "INBOX".into() },
            role: if ours { Role::Sent } else { Role::Inbox },
            server_id: format!("1:{uid}"),
            uid: Some(uid),
            server_thread: None,
            message_id: format!("<{id}>"),
            in_reply_to: reply_to.map(|r| vec![format!("<{r}>")]).unwrap_or_default(),
            references: reply_to.map(|r| vec![format!("<{r}>")]).unwrap_or_default(),
            from: Addr { name: if ours { "Kannaka".into() } else { String::new() }, email: from.into() },
            to: vec![addr(to)],
            cc: vec![],
            subject: format!("subject {id}"),
            date: Some(when),
            flags: vec!["\\Seen".into()],
            auto: AutoHeaders::default(),
            body_hash: String::new(),
            own: OwnText { chars: 400, question: false, fyi: false },
            synced_at: now(),
        }
    }
    fn state_of(refs: &[MailRef]) -> State {
        state_with(refs, &[])
    }
    fn state_with(refs: &[MailRef], closures: &[Closure]) -> State {
        let all = status_all(refs, &policy(), closures, now());
        assert_eq!(all.len(), 1, "expected one thread, got {all:?}");
        all[0].state
    }

    #[test]
    fn unanswered_person_mail_needs_reply() {
        assert_eq!(state_of(&[msg(1, "a", None, "nick@x.com", ME, at(22, 3))]), State::NeedsReply);
    }

    #[test]
    fn our_reply_in_sent_answers_it() {
        let a = msg(1, "a", None, "nick@x.com", ME, at(22, 3));
        let b = msg(2, "b", Some("a"), ME, "nick@x.com", at(22, 5));
        assert_eq!(state_of(&[a, b]), State::Done);
    }

    #[test]
    fn our_question_is_waiting_on_them() {
        let mut q = msg(68, "q", None, ME, "vincent@getinference.com", at(25, 20));
        q.own.question = true;
        assert_eq!(state_of(&[q]), State::WaitingOnThem);
    }

    #[test]
    fn answered_flag_is_an_authority() {
        let mut a = msg(1, "a", None, "nick@x.com", ME, at(22, 3));
        a.flags.push("\\Answered".into());
        assert_eq!(state_of(&[a]), State::Done);
    }

    #[test]
    fn automated_senders_are_ignored() {
        let base = || msg(1, "a", None, "lina@corp.com", ME, at(20, 3));
        let mut cases: Vec<(&str, MailRef)> = Vec::new();
        let mut m = base(); m.auto.list_id = Some("<x.list>".into()); cases.push(("list-id", m));
        let mut m = base(); m.auto.list_unsubscribe = true; cases.push(("list-unsub", m));
        let mut m = base(); m.auto.auto_submitted = Some("auto-replied".into()); cases.push(("auto-submitted", m));
        let mut m = base(); m.auto.precedence = Some("Bulk".into()); cases.push(("precedence", m));
        let mut m = base(); m.auto.feedback_id = true; cases.push(("feedback-id", m));
        let mut m = base(); m.auto.return_path = Some("<bounces+5-c1=kannaka=spacechild.love@em.x.com>".into()); cases.push(("verp", m));
        let mut m = base(); m.from.email = "no-reply@github.com".into(); cases.push(("noreply", m));
        let mut m = base(); m.from.email = "notifications@youspot.com".into(); cases.push(("notifications", m));
        for (name, m) in cases {
            assert_eq!(state_of(&[m]), State::Ignored, "{name} should be ignored");
        }
    }

    #[test]
    fn auto_submitted_no_is_a_person() {
        let mut m = msg(1, "a", None, "lina@corp.com", ME, at(20, 3));
        m.auto.auto_submitted = Some("no".into());
        m.auto.return_path = Some("<lina@corp.com>".into());
        assert_eq!(state_of(&[m]), State::NeedsReply);
    }

    #[test]
    fn internal_and_tool_traffic_is_ignored() {
        // our own test to an agent inside ninja-portal.com, and its auto-reply
        let mut q = msg(65, "q", None, ME, "rogue@ninja-portal.com", at(24, 18));
        q.own.question = true;
        let r = msg(53, "r", Some("q"), "rogue@ninja-portal.com", ME, at(24, 19));
        assert_eq!(state_of(&[q, r]), State::Ignored);
        // a notifier sending from our address under another display name
        let mut n = msg(9, "n", None, ME, "nick@x.com", at(10, 16));
        n.from.name = "Ghost Signals Records".into();
        assert_eq!(state_of(&[n]), State::Ignored);
    }

    #[test]
    fn an_auto_reply_does_not_hide_our_open_question() {
        let mut q = msg(1, "q", None, ME, "sean@raicollab.org", at(20, 3));
        q.own.question = true;
        let mut ooo = msg(2, "o", Some("q"), "sean@raicollab.org", ME, at(20, 4));
        ooo.auto.auto_submitted = Some("auto-replied".into());
        assert_eq!(state_of(&[q, ooo]), State::WaitingOnThem);
    }

    #[test]
    fn fyi_and_short_acknowledgement_are_done() {
        let mut f = msg(48, "f", None, "nick@spacechild.love", ME, at(22, 20));
        f.own = OwnText { chars: 5, question: false, fyi: true };
        assert_eq!(state_of(&[f]), State::Done);

        let ours = msg(2, "o", None, ME, "nick@x.com", at(10, 2));
        let mut ack = msg(4, "k", Some("o"), "nick@x.com", ME, at(10, 3));
        ack.own = OwnText { chars: 9, question: false, fyi: false };
        assert_eq!(state_of(&[ours.clone(), ack.clone()]), State::Done);

        // the same short text that does NOT reply to us is not an acknowledgement
        let mut cold = msg(37, "c", None, "nick@x.com", ME, at(20, 20));
        cold.own = OwnText { chars: 0, question: false, fyi: false };
        assert_eq!(state_of(&[cold]), State::NeedsReply);

        // a short reply that asks is not an acknowledgement
        ack.own.question = true;
        assert_eq!(state_of(&[ours, ack]), State::NeedsReply);
    }

    #[test]
    fn closure_wins_until_something_newer_arrives() {
        let help = msg(49, "h", None, "nflach78@gmail.com", ME, at(23, 3));
        let t = build_threads(std::slice::from_ref(&help));
        let c = Closure {
            thread_id: t[0].id.clone(),
            thread_key: t[0].key.clone(),
            account: "zoho".into(),
            note: "deployed 09-23 (kax-scada-desk-run2)".into(),
            closed_at: now(),
            covers_through: help.date,
        };
        assert_eq!(state_with(std::slice::from_ref(&help), &[]), State::NeedsReply);
        assert_eq!(state_with(std::slice::from_ref(&help), std::slice::from_ref(&c)), State::Done);
        // closure beats waiting_on_them too
        let mut q = msg(50, "q2", Some("h"), ME, "nflach78@gmail.com", at(23, 4));
        q.own.question = true;
        let mut c2 = c.clone();
        c2.covers_through = q.date;
        assert_eq!(state_with(&[help.clone(), q.clone()], &[c2]), State::Done);
        // a newer person message re-opens it
        let again = msg(51, "h2", Some("h"), "nflach78@gmail.com", ME, at(24, 9));
        assert_eq!(state_with(&[help, again], &[c]), State::NeedsReply);
    }

    #[test]
    fn closure_for_another_account_does_not_apply() {
        let help = msg(49, "h", None, "nflach78@gmail.com", ME, at(23, 3));
        let t = build_threads(std::slice::from_ref(&help));
        let c = Closure {
            thread_id: t[0].id.clone(),
            thread_key: t[0].key.clone(),
            account: "np".into(),
            note: "x".into(),
            closed_at: now(),
            covers_through: help.date,
        };
        assert_eq!(state_with(&[help], &[c]), State::NeedsReply);
    }

    #[test]
    fn threads_join_by_references_and_split_by_account() {
        let a = msg(1, "a", None, "brad@yahoo.com", ME, at(15, 16));
        let b = msg(2, "b", Some("a"), ME, "brad@yahoo.com", at(15, 22));
        let mut c = msg(3, "c", Some("b"), "brad@yahoo.com", ME, at(16, 19));
        c.references = vec!["<a>".into(), "<b>".into()];
        let mut other = msg(4, "a2", None, "brad@yahoo.com", ME, at(16, 20));
        other.account = "np".into();
        other.references = vec!["<a>".into()];
        let all = [a, b, c, other];
        let threads = build_threads(&all);
        assert_eq!(threads.len(), 2);
        let zoho = threads.iter().find(|t| t.account == "zoho").unwrap();
        assert_eq!(zoho.messages.len(), 3);
        assert_eq!(zoho.key, "<a>");
        assert_ne!(zoho.id, threads.iter().find(|t| t.account == "np").unwrap().id);
    }

    #[test]
    fn newest_message_decides_by_date_not_folder_order() {
        // Sent copy dated before the inbound reply even though it syncs later.
        let mut ours = msg(65, "q", None, ME, "sean@raicollab.org", at(24, 18));
        ours.own.question = true;
        let theirs = msg(53, "r", Some("q"), "sean@raicollab.org", ME, at(24, 19));
        assert_eq!(state_of(&[theirs, ours]), State::NeedsReply);
    }
}
