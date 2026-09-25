//! `sync`: pull headers + flags from each account's authority into MailRefs,
//! and `thread`'s live fetch by reference. Read-only against every server.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use super::config::{Account, Transport};
use super::imap::ImapSession;
use super::jmap::{bracket, keyword_to_flag, JmapSession};
use super::parse::{self, header, OwnText};
use super::{Addr, AutoHeaders, MailError, MailRef, Result, Role};

/// Counts for one account's sync.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub account: String,
    pub folders: Vec<(String, usize)>,
    pub fetched_bodies: usize,
    pub reused: usize,
}

fn known_key(r: &MailRef) -> (String, String, String) {
    (r.account.clone(), r.folder.clone(), r.server_id.clone())
}

/// Build a MailRef from a raw RFC 5322 message (IMAP `BODY.PEEK[]`).
#[allow(clippy::too_many_arguments)]
pub fn mailref_from_raw(
    account: &str,
    folder: &str,
    role: Role,
    server_id: String,
    uid: Option<u32>,
    flags: Vec<String>,
    internaldate: Option<DateTime<Utc>>,
    raw: &[u8],
    now: DateTime<Utc>,
) -> MailRef {
    let (hb, body) = parse::split_message(raw);
    let hs = parse::parse_headers(hb);
    let get = |n: &str| header(&hs, n).map(|s| s.to_string());
    let from = get("From").map(|f| parse::parse_addrs(&f)).and_then(|mut v| if v.is_empty() { None } else { Some(v.remove(0)) }).unwrap_or_default();
    let text = parse::best_text(&hs, body).unwrap_or_default();
    MailRef {
        account: account.to_string(),
        folder: folder.to_string(),
        role,
        server_id,
        uid,
        server_thread: None,
        message_id: get("Message-ID").map(|m| parse::parse_ids(&m).into_iter().next().unwrap_or(m)).unwrap_or_default(),
        in_reply_to: get("In-Reply-To").map(|v| parse::parse_ids(&v)).unwrap_or_default(),
        references: get("References").map(|v| parse::parse_ids(&v)).unwrap_or_default(),
        from,
        to: get("To").map(|v| parse::parse_addrs(&v)).unwrap_or_default(),
        cc: get("Cc").map(|v| parse::parse_addrs(&v)).unwrap_or_default(),
        subject: get("Subject").map(|s| parse::decode_words(&s)).unwrap_or_default(),
        date: get("Date").and_then(|d| parse::parse_date(&d)).or(internaldate),
        flags,
        auto: AutoHeaders {
            list_id: get("List-Id"),
            list_unsubscribe: get("List-Unsubscribe").is_some(),
            auto_submitted: get("Auto-Submitted"),
            precedence: get("Precedence"),
            feedback_id: get("Feedback-ID").is_some(),
            return_path: get("Return-Path"),
        },
        body_hash: parse::body_hash(body),
        own: parse::own_text_of(&text),
        synced_at: now,
    }
}

fn find_sent_folder(list: &[(String, String)]) -> Option<String> {
    list.iter()
        .find(|(attrs, _)| attrs.split_whitespace().any(|a| a.eq_ignore_ascii_case("\\Sent")))
        .map(|(_, n)| n.clone())
        .or_else(|| {
            ["Sent", "Sent Items", "Sent Messages", "INBOX.Sent"]
                .iter()
                .find(|c| list.iter().any(|(_, n)| n == *c))
                .map(|s| s.to_string())
        })
}

fn sync_imap(acct: &Account, since: DateTime<Utc>, known: &HashMap<(String, String, String), MailRef>, now: DateTime<Utc>) -> Result<(Vec<MailRef>, SyncReport)> {
    let creds = acct.credentials()?;
    let host = acct.host.as_deref().unwrap_or_default();
    let mut s = ImapSession::connect(host, acct.port.unwrap_or(993))?;
    s.login(&creds)?;
    drop(creds);
    let list = s.list()?;
    let mut folders = vec![("INBOX".to_string(), Role::Inbox)];
    match find_sent_folder(&list) {
        Some(f) => folders.push((f, Role::Sent)),
        None => eprintln!("mail: {}: no Sent folder found; replies will look unanswered", acct.name),
    }
    let mut out = Vec::new();
    let mut rep = SyncReport { account: acct.name.clone(), ..Default::default() };
    for (folder, role) in folders {
        let validity = s.examine(&folder)?;
        let uids = s.uid_search_since(since)?;
        let metas = s.uid_fetch(&uids, "(UID FLAGS INTERNALDATE)")?;
        let mut need: Vec<u32> = Vec::new();
        for m in &metas {
            let sid = format!("{validity}:{}", m.uid);
            match known.get(&(acct.name.clone(), folder.clone(), sid.clone())) {
                Some(prev) => {
                    let mut r = prev.clone();
                    r.flags = m.flags.clone();
                    r.synced_at = now;
                    out.push(r);
                    rep.reused += 1;
                }
                None => need.push(m.uid),
            }
        }
        for chunk in need.chunks(20) {
            for f in s.uid_fetch(chunk, "(UID FLAGS INTERNALDATE BODY.PEEK[])")? {
                let raw = f.data.unwrap_or_default();
                out.push(mailref_from_raw(&acct.name, &folder, role, format!("{validity}:{}", f.uid), Some(f.uid), f.flags, f.internaldate, &raw, now));
                rep.fetched_bodies += 1;
            }
        }
        rep.folders.push((folder, metas.len()));
    }
    s.logout();
    Ok((out, rep))
}

const JMAP_PROPS: &[&str] = &[
    "id", "threadId", "mailboxIds", "keywords", "messageId", "inReplyTo", "references", "from", "to", "cc",
    "subject", "sentAt", "receivedAt", "textBody", "bodyValues",
    "header:List-Id:asText", "header:List-Unsubscribe:asText", "header:Auto-Submitted:asText",
    "header:Precedence:asText", "header:Feedback-ID:asText", "header:Return-Path:asText",
];

fn jaddrs(v: &Value) -> Vec<Addr> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Addr { name: x["name"].as_str().unwrap_or("").to_string(), email: x["email"].as_str()?.to_ascii_lowercase() })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn jids(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(bracket)).collect()).unwrap_or_default()
}

fn jtext(v: &Value, name: &str) -> Option<String> {
    v[format!("header:{name}:asText")].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The decoded text parts JMAP returned for this email (plain, or html stripped).
fn jmap_text(e: &Value) -> String {
    let mut out = String::new();
    for part in e["textBody"].as_array().cloned().unwrap_or_default() {
        let Some(pid) = part["partId"].as_str() else { continue };
        let val = e["bodyValues"][pid]["value"].as_str().unwrap_or("");
        if part["type"].as_str().unwrap_or("").eq_ignore_ascii_case("text/html") {
            out.push_str(&parse::strip_html(val));
        } else {
            out.push_str(val);
        }
    }
    out
}

pub fn mailref_from_jmap(account: &str, folder: &str, role: Role, e: &Value, now: DateTime<Utc>) -> MailRef {
    let text = jmap_text(e);
    let flags = e["keywords"].as_object().map(|k| k.keys().map(|s| keyword_to_flag(s)).collect()).unwrap_or_default();
    let date = e["sentAt"].as_str().or(e["receivedAt"].as_str()).and_then(|d| DateTime::parse_from_rfc3339(d).ok()).map(|d| d.with_timezone(&Utc));
    MailRef {
        account: account.to_string(),
        folder: folder.to_string(),
        role,
        server_id: e["id"].as_str().unwrap_or("").to_string(),
        uid: None,
        server_thread: e["threadId"].as_str().map(|s| s.to_string()),
        message_id: jids(&e["messageId"]).into_iter().next().unwrap_or_default(),
        in_reply_to: jids(&e["inReplyTo"]),
        references: jids(&e["references"]),
        from: jaddrs(&e["from"]).into_iter().next().unwrap_or_default(),
        to: jaddrs(&e["to"]),
        cc: jaddrs(&e["cc"]),
        subject: e["subject"].as_str().unwrap_or("").to_string(),
        date,
        flags,
        auto: AutoHeaders {
            list_id: jtext(e, "List-Id"),
            list_unsubscribe: jtext(e, "List-Unsubscribe").is_some(),
            auto_submitted: jtext(e, "Auto-Submitted"),
            precedence: jtext(e, "Precedence"),
            feedback_id: jtext(e, "Feedback-ID").is_some(),
            return_path: jtext(e, "Return-Path"),
        },
        body_hash: parse::body_hash(text.as_bytes()),
        own: if text.is_empty() { OwnText::default() } else { parse::own_text_of(&text) },
        synced_at: now,
    }
}

fn jmap_mailboxes(s: &JmapSession) -> Result<Vec<(String, String, Option<String>)>> {
    let r = s.call(vec![("Mailbox/get", json!({"accountId": s.account_id, "ids": null, "properties": ["id", "name", "role"]}))])?;
    Ok(r[0]["list"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| (m["id"].as_str().unwrap_or("").to_string(), m["name"].as_str().unwrap_or("").to_string(), m["role"].as_str().map(|s| s.to_string())))
        .collect())
}

fn sync_jmap(acct: &Account, since: DateTime<Utc>, now: DateTime<Utc>) -> Result<(Vec<MailRef>, SyncReport)> {
    let creds = acct.credentials()?;
    let s = JmapSession::connect(acct.url.as_deref().unwrap_or_default(), &creds)?;
    drop(creds);
    let boxes = jmap_mailboxes(&s)?;
    let mut out = Vec::new();
    let mut rep = SyncReport { account: acct.name.clone(), ..Default::default() };
    for (want, role) in [("inbox", Role::Inbox), ("sent", Role::Sent)] {
        let Some((id, name, _)) = boxes.iter().find(|(_, _, r)| r.as_deref() == Some(want)) else {
            eprintln!("mail: {}: no mailbox with role {want}", acct.name);
            continue;
        };
        let mut ids: Vec<String> = Vec::new();
        let mut position = 0usize;
        loop {
            let r = s.call(vec![(
                "Email/query",
                json!({
                    "accountId": s.account_id,
                    "filter": {"inMailbox": id, "after": since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)},
                    "sort": [{"property": "receivedAt", "isAscending": true}],
                    "position": position, "limit": 256,
                }),
            )])?;
            let page: Vec<String> = r[0]["ids"].as_array().cloned().unwrap_or_default().iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
            let n = page.len();
            ids.extend(page);
            if n < 256 {
                break;
            }
            position += n;
        }
        for chunk in ids.chunks(50) {
            let r = s.call(vec![(
                "Email/get",
                json!({"accountId": s.account_id, "ids": chunk, "properties": JMAP_PROPS, "fetchTextBodyValues": true, "maxBodyValueBytes": 262144}),
            )])?;
            for e in r[0]["list"].as_array().cloned().unwrap_or_default() {
                out.push(mailref_from_jmap(&acct.name, name, role, &e, now));
                rep.fetched_bodies += 1;
            }
        }
        rep.folders.push((name.clone(), ids.len()));
    }
    Ok((out, rep))
}

/// Sync every selected account; refs of accounts not synced are kept as they were.
pub fn sync_accounts(accounts: &[&Account], days: i64, existing: Vec<MailRef>, now: DateTime<Utc>) -> (Vec<MailRef>, Vec<Result<SyncReport>>) {
    let since = now - Duration::days(days);
    let known: HashMap<_, _> = existing.iter().map(|r| (known_key(r), r.clone())).collect();
    let mut keep: Vec<MailRef> = existing;
    let mut reports = Vec::new();
    for acct in accounts {
        let res = match acct.transport {
            Transport::Imap => sync_imap(acct, since, &known, now),
            Transport::Jmap => sync_jmap(acct, since, now),
        };
        match res {
            Ok((refs, rep)) => {
                // The authority is the record: this account's refs are replaced wholesale.
                keep.retain(|r| r.account != acct.name);
                keep.extend(refs);
                reports.push(Ok(rep));
            }
            Err(e) => reports.push(Err(MailError::Config(format!("{}: {e}", acct.name)))),
        }
    }
    (keep, reports)
}

/// One message as the server has it now (for `mail thread`).
#[derive(Debug)]
pub struct LiveMsg {
    pub anchor: String,
    /// `None` when the server no longer has it.
    pub present: Option<LiveHeaders>,
}

#[derive(Debug)]
pub struct LiveHeaders {
    pub from: String,
    pub to: String,
    pub cc: String,
    pub date: String,
    pub subject: String,
    pub flags: Vec<String>,
    /// Own words, only when asked for; never stored.
    pub own_text: Option<String>,
}

/// Fetch the given refs of one account live from its authority.
pub fn fetch_live(acct: &Account, refs: &[&MailRef], with_bodies: bool) -> Result<Vec<LiveMsg>> {
    let creds = acct.credentials()?;
    let mut out = Vec::new();
    match acct.transport {
        Transport::Imap => {
            let mut s = ImapSession::connect(acct.host.as_deref().unwrap_or_default(), acct.port.unwrap_or(993))?;
            s.login(&creds)?;
            let mut by_folder: HashMap<&str, Vec<&MailRef>> = HashMap::new();
            for r in refs {
                by_folder.entry(r.folder.as_str()).or_default().push(r);
            }
            let item = if with_bodies { "(UID FLAGS BODY.PEEK[])" } else { "(UID FLAGS BODY.PEEK[HEADER.FIELDS (FROM TO CC DATE SUBJECT)])" };
            for (folder, rs) in by_folder {
                let validity = s.examine(folder)?;
                for r in rs {
                    let same_validity = r.server_id.split(':').next() == Some(validity.to_string().as_str());
                    let got = match (same_validity, r.uid) {
                        (true, Some(uid)) => s.uid_fetch(&[uid], item)?.into_iter().find(|f| f.uid == uid),
                        _ => None,
                    };
                    out.push(LiveMsg {
                        anchor: r.anchor(),
                        present: got.map(|f| {
                            let raw = f.data.unwrap_or_default();
                            let (hb, body) = parse::split_message(&raw);
                            let hs = parse::parse_headers(hb);
                            let g = |n: &str| header(&hs, n).map(parse::decode_words).unwrap_or_default();
                            LiveHeaders {
                                from: g("From"),
                                to: g("To"),
                                cc: g("Cc"),
                                date: g("Date"),
                                subject: g("Subject"),
                                flags: f.flags,
                                own_text: if with_bodies { Some(parse::own_text_string(&parse::best_text(&hs, body).unwrap_or_default())) } else { None },
                            }
                        }),
                    });
                }
            }
            s.logout();
        }
        Transport::Jmap => {
            let s = JmapSession::connect(acct.url.as_deref().unwrap_or_default(), &creds)?;
            let ids: Vec<&str> = refs.iter().map(|r| r.server_id.as_str()).collect();
            let r = s.call(vec![(
                "Email/get",
                json!({"accountId": s.account_id, "ids": ids, "properties": ["id", "keywords", "from", "to", "cc", "sentAt", "subject", "textBody", "bodyValues"], "fetchTextBodyValues": with_bodies}),
            )])?;
            let list = r[0]["list"].as_array().cloned().unwrap_or_default();
            let fmt = |v: &Value| jaddrs(v).iter().map(|a| if a.name.is_empty() { a.email.clone() } else { format!("{} <{}>", a.name, a.email) }).collect::<Vec<_>>().join(", ");
            for rf in refs {
                let e = list.iter().find(|e| e["id"].as_str() == Some(rf.server_id.as_str()));
                out.push(LiveMsg {
                    anchor: rf.anchor(),
                    present: e.map(|e| LiveHeaders {
                        from: fmt(&e["from"]),
                        to: fmt(&e["to"]),
                        cc: fmt(&e["cc"]),
                        date: e["sentAt"].as_str().unwrap_or("").to_string(),
                        subject: e["subject"].as_str().unwrap_or("").to_string(),
                        flags: e["keywords"].as_object().map(|k| k.keys().map(|s| keyword_to_flag(s)).collect()).unwrap_or_default(),
                        own_text: if with_bodies { Some(parse::own_text_string(&jmap_text(e))) } else { None },
                    }),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_message_becomes_a_ref_without_its_body() {
        let raw = b"From: Flaukowski <nflach78@gmail.com>\r\nTo: Kannaka <kannaka@spacechild.love>\r\nSubject: Help if possible\r\nDate: Tue, 22 Sep 2026 22:24:45 -0500\r\nMessage-ID: <CAAerEH@mail.gmail.com>\r\nReturn-Path: <nflach78@gmail.com>\r\n\r\nPlease deploy the desk. Secret token sk-THISMUSTNOTBESTORED\r\n";
        let r = mailref_from_raw("zoho", "INBOX", Role::Inbox, "7:49".into(), Some(49), vec![], None, raw, Utc::now());
        assert_eq!(r.message_id, "<CAAerEH@mail.gmail.com>");
        assert_eq!(r.from.email, "nflach78@gmail.com");
        assert_eq!(r.date.unwrap().to_rfc3339(), "2026-09-23T03:24:45+00:00");
        assert!(!r.own.question);
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("THISMUSTNOTBESTORED"), "a body leaked into the ref: {json}");
        assert!(!json.contains("deploy the desk"), "a body leaked into the ref");
        assert_eq!(r.anchor(), "zoho:INBOX:49");
    }

    #[test]
    fn jmap_email_becomes_a_ref() {
        let e = json!({
            "id": "Mabc", "threadId": "Tq", "keywords": {"$seen": true, "$answered": true},
            "messageId": ["x@y"], "inReplyTo": ["p@q"], "references": ["r@s", "p@q"],
            "from": [{"name": "Kannaka", "email": "Kannaka@spacechild.love"}],
            "to": [{"name": null, "email": "kannaka+bus@ninja-portal.com"}],
            "subject": "membrane test", "sentAt": "2026-09-16T20:16:19-07:00",
            "textBody": [{"partId": "1", "type": "text/plain"}],
            "bodyValues": {"1": {"value": "is this thing on?"}},
            "header:Auto-Submitted:asText": " auto-replied "
        });
        let r = mailref_from_jmap("np", "INBOX", Role::Inbox, &e, Utc::now());
        assert_eq!(r.message_id, "<x@y>");
        assert_eq!(r.references, vec!["<r@s>", "<p@q>"]);
        assert!(r.has_flag("\\Answered"));
        assert_eq!(r.from.email, "kannaka@spacechild.love");
        assert_eq!(r.date.unwrap().to_rfc3339(), "2026-09-17T03:16:19+00:00");
        assert!(r.own.question);
        assert_eq!(r.auto.auto_submitted.as_deref(), Some("auto-replied"));
        assert_eq!(r.anchor(), "np:INBOX:Mabc");
    }

    #[test]
    fn sent_folder_by_special_use_then_name() {
        let l = vec![("\\HasNoChildren".to_string(), "INBOX".to_string()), ("\\Sent".to_string(), "Sent Items".to_string())];
        assert_eq!(find_sent_folder(&l).as_deref(), Some("Sent Items"));
        let l2 = vec![("".to_string(), "Sent".to_string())];
        assert_eq!(find_sent_folder(&l2).as_deref(), Some("Sent"));
    }
}
