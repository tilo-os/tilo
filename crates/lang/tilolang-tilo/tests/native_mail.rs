//! A mailbox the native Mail app wrote, opened by the tilolang Mail: its migration carries every
//! draft it was writing and every message still waiting in its outbox over once, each a draft
//! from the same account, to the same people, with the same subject and body. What a draft held
//! that the tilolang Mail has no place for is kept on the draft in words, never dropped. The
//! native app's documents are read, never written.
//!
//! The native mailbox is written here in the layout the native app stored it in: the record's
//! fields at the document's root, the composer's draft a record within it, the other drafts and
//! the outbox collections of items (an order key `k` and the fields `v`), and each outbox entry's
//! message the JSON the native app kept it as.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ScalarValue};
use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::{Replica, target};

const MAIL_APP: &str = "so.tilo.mail";
const MAIL: (&str, &str) = (
    include_str!("../../../../apps/tl/mail/app.tl"),
    include_str!("../../../../apps/tl/mail/app.ids"),
);

fn put(doc: &mut AutoCommit, at: &ObjId, key: &str, value: &str) {
    doc.put(at, key, value).unwrap();
}

/// One item of a native collection: its order key and its fields.
fn item(doc: &mut AutoCommit, collection: &ObjId, id: &str, order: &str) -> ObjId {
    let entry = doc.put_object(collection, id, ObjType::Map).unwrap();
    doc.put(&entry, "k", order).unwrap();
    doc.put_object(&entry, "v", ObjType::Map).unwrap()
}

/// The native mailbox: a composer draft with a Bcc, an attachment and notes; an empty draft and a
/// reply in its drafts; a message waiting in the outbox and one that already went.
fn native_mailbox() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "tilo:version", ScalarValue::Int(3)).unwrap();

    let draft = doc.put_object(ROOT, "draft", ObjType::Map).unwrap();
    let sender = doc.put_object(&draft, "sender", ObjType::Map).unwrap();
    put(&mut doc, &sender, "account", "work");
    put(&mut doc, &sender, "email", "ada@example.com");
    put(&mut doc, &draft, "to", "board@example.com");
    put(&mut doc, &draft, "cc", "");
    put(&mut doc, &draft, "bcc", "auditor@example.com");
    put(&mut doc, &draft, "subject", "Quarterly numbers");
    put(&mut doc, &draft, "body", "The numbers are attached.");
    put(&mut doc, &draft, "notes", "Check the totals first");
    let attachments = doc
        .put_object(&draft, "attachments", ObjType::List)
        .unwrap();
    let plan = doc.insert_object(&attachments, 0, ObjType::Map).unwrap();
    put(&mut doc, &plan, "content_id", "c1");
    put(&mut doc, &plan, "filename", "numbers.pdf");
    put(&mut doc, &plan, "mime", "application/pdf");

    let drafts = doc.put_object(ROOT, "drafts", ObjType::Map).unwrap();
    let empty = item(&mut doc, &drafts, "d-empty", "a0");
    for field in ["to", "cc", "bcc", "subject", "body", "notes"] {
        put(&mut doc, &empty, field, "");
    }
    let reply = item(&mut doc, &drafts, "d-reply", "a1");
    let sender = doc.put_object(&reply, "sender", ObjType::Map).unwrap();
    put(&mut doc, &sender, "account", "home");
    put(&mut doc, &sender, "email", "ada@home.example");
    put(&mut doc, &reply, "to", "grace@example.com");
    put(&mut doc, &reply, "cc", "linus@example.com");
    put(&mut doc, &reply, "bcc", "");
    put(&mut doc, &reply, "subject", "Re: Lunch");
    put(&mut doc, &reply, "body", "Thursday works.");
    put(&mut doc, &reply, "in_reply_to", "<lunch@example.com>");
    put(&mut doc, &reply, "notes", "");

    let outbox = doc.put_object(ROOT, "outbox", ObjType::Map).unwrap();
    let waiting = item(&mut doc, &outbox, "o-waiting", "a0");
    let message = serde_json::json!({
        "id": "o-waiting",
        "account": "work",
        "draft": {
            "sender": null,
            "to": "team@example.com",
            "cc": "",
            "bcc": "",
            "subject": "Offsite",
            "body": "See you at nine.",
            "html_body": null,
            "attachments": [],
        },
        "in_reply_to": "<offsite@example.com>",
        "not_before": 1,
    });
    put(&mut doc, &waiting, "value", &message.to_string());
    doc.put_object(&waiting, "compensates", ObjType::List)
        .unwrap();
    let gone = item(&mut doc, &outbox, "o-gone", "a1");
    let sent = serde_json::json!({
        "id": "o-gone",
        "account": "work",
        "draft": { "to": "old@example.com", "cc": "", "bcc": "", "subject": "Sent already",
                   "body": "Gone.", "html_body": null, "attachments": [] },
        "in_reply_to": null,
    });
    put(&mut doc, &gone, "value", &sent.to_string());
    let settled = doc.put_object(&gone, "settled", ObjType::Map).unwrap();
    put(&mut doc, &settled, "outcome", "done");
    doc
}

fn native_documents() -> Vec<(String, AutoCommit)> {
    vec![(String::new(), native_mailbox())]
}

fn text(ada: &Replica, at: &ResourceRef, field: &str) -> String {
    match ada.host.runtime().read(at, field).unwrap() {
        Value::Text(text) => text,
        other => panic!("{field} reads {other:?}"),
    }
}

/// Each draft of the mailbox: from, to, cc, subject, body, what it answers, what was set aside.
fn drafts(ada: &Replica, mail: &ResourceRef) -> Vec<[String; 7]> {
    let Value::List(items) = ada.host.runtime().read(mail, "drafts").unwrap() else {
        panic!("the drafts read as a list");
    };
    let mut out: Vec<[String; 7]> = items
        .iter()
        .map(target)
        .map(|d| {
            [
                "from",
                "to",
                "cc",
                "subject",
                "body",
                "in_reply_to",
                "set_aside",
            ]
            .map(|field| text(ada, &d, field))
        })
        .collect();
    out.sort_by(|a, b| a[3].cmp(&b[3]));
    out
}

fn row(fields: [&str; 7]) -> [String; 7] {
    fields.map(str::to_owned)
}

fn assert_carried(ada: &Replica, mail: &ResourceRef) {
    assert_eq!(
        drafts(ada, mail),
        [
            row([
                "work",
                "team@example.com",
                "",
                "Offsite",
                "See you at nine.",
                "<offsite@example.com>",
                "",
            ]),
            row([
                "work",
                "board@example.com",
                "",
                "Quarterly numbers",
                "The numbers are attached.",
                "",
                "Bcc: auditor@example.com\nAttachments not carried over: numbers.pdf\n\
                 Notes: Check the totals first",
            ]),
            row([
                "home",
                "grace@example.com",
                "linus@example.com",
                "Re: Lunch",
                "Thursday works.",
                "<lunch@example.com>",
                "",
            ]),
        ],
        "the composer's draft, the reply and the waiting message, each a draft; the empty draft \
         and the message that went are not"
    );
}

#[test]
fn a_native_mailbox_is_carried_over_once() {
    let documents = native_documents();
    let before: Vec<_> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    let (ada, _) = Replica::new("ada", crate::fixtures::Workspace::new(), &[MAIL]);
    ada.host.runtime().set_earlier(MAIL_APP, &documents);
    let mail = ada.host.root_of(MAIL_APP).expect("the mailbox");
    ada.commit();
    assert!(
        drafts(&ada, &mail).is_empty(),
        "nothing before the migration"
    );

    assert_eq!(ada.host.migrate(), 1, "the mailbox's migration runs");
    assert_eq!(ada.commit(), 1, "as one change");
    assert_carried(&ada, &mail);

    assert_eq!(ada.host.migrate(), 0, "it runs once");
    assert_eq!(ada.commit(), 0);
    let grace = ada.fork("grace", &[MAIL]);
    grace.host.runtime().set_earlier(MAIL_APP, &documents);
    assert_eq!(
        grace.host.migrate(),
        0,
        "another start with the same documents finds it done"
    );
    assert_carried(&grace, &mail);

    let after: Vec<_> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    assert_eq!(before, after, "the native app's documents are not written");
}

#[test]
fn a_mailbox_opened_before_the_native_documents_arrived_receives_them_once_they_do() {
    let (ada, _) = Replica::new("ada", crate::fixtures::Workspace::new(), &[MAIL]);
    let mail = ada.host.root_of(MAIL_APP).expect("the mailbox");
    ada.commit();
    assert_eq!(ada.host.migrate(), 0, "nothing to carry has arrived");
    assert_eq!(ada.commit(), 0, "and nothing is recorded");

    ada.host
        .runtime()
        .set_earlier(MAIL_APP, &native_documents());
    assert_eq!(ada.host.migrate(), 1, "the carrying waited for them");
    assert_eq!(ada.commit(), 1);
    assert_carried(&ada, &mail);
    assert_eq!(ada.host.migrate(), 0, "and runs only once");
}
