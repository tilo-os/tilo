//! A mailbox the native Mail app wrote, opened by the tilolang Mail: its migration carries every
//! draft it was writing and every message still waiting in its outbox over once, each a draft
//! from the same account and identity, to the same people (the blind ones too), with the same
//! subject, body and invitation, answering what it answered. What a draft held that the
//! tilolang Mail has no place for (its attachments, its private notes) is kept on it in words,
//! never dropped. Each draft is made as whoever wrote it there, as the native mailbox's history
//! names them, and read by them alone until it is sent. The native app's documents are read, never
//! written.
//!
//! `legacy/rust_mail.hex` is what the native Mail's own actions wrote: Ada's composer draft
//! (a chosen sender, a blind copy, a body being written, an attachment, an invitation and notes),
//! a reply and an empty draft of hers, a draft Leo wrote in the same mailbox, one an agent drafted
//! for Ada, and two messages she sent, one waiting in the outbox and one that went.

use automerge::AutoCommit;
use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::{MAIL_SET, Replica, Workspace, target};

const MAIL_APP: &str = "so.tilo.mail";

/// The invitation the composer's draft carries, as the native app kept it.
const INVITATION: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Tilo//Mail//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:review-1@tilo.example\r\nDTSTAMP:20260629T070000Z\r\nDTSTART:20260702T090000Z\r\nDTEND:20260702T100000Z\r\nSUMMARY:Design review\r\nORGANIZER:mailto:ada@work.example\r\nATTENDEE:mailto:mia@work.example\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

fn native_documents() -> Vec<(String, AutoCommit)> {
    crate::fixtures::native_documents(include_str!("legacy/rust_mail.hex"))
}

fn text(r: &Replica, at: &ResourceRef, field: &str) -> String {
    match r.host.runtime().read(at, field).unwrap() {
        Value::Text(text) => text,
        other => panic!("{field} reads {other:?}"),
    }
}

/// A draft as the test reads it: from, the identity it goes as, to, cc, bcc, subject, body, what
/// it answers, its invitation, and what was set aside.
type Row = [String; 10];

/// Each draft of the mailbox `r` holds, by subject.
fn rows(r: &Replica, mail: &ResourceRef) -> Vec<Row> {
    let Value::List(items) = r.host.runtime().read(mail, "drafts").unwrap() else {
        panic!("the drafts read as a list");
    };
    let mut out: Vec<Row> = items
        .iter()
        .map(|item| {
            let draft = target(item);
            [
                "from",
                "from_address",
                "to",
                "cc",
                "bcc",
                "subject",
                "body",
                "in_reply_to",
                "invitation",
                "set_aside",
            ]
            .map(|field| text(r, &draft, field))
        })
        .collect();
    out.sort_by(|a, b| a[5].cmp(&b[5]));
    out
}

fn row(fields: [&str; 10]) -> Row {
    fields.map(str::to_owned)
}

/// What Ada's native mailbox showed her as drafts: the composer's, her reply, the one her agent
/// drafted for her, and the message still waiting to go, each a draft again; not the empty one,
/// nor the message that went.
fn adas() -> Vec<Row> {
    vec![
        row([
            "ada@work.example",
            "",
            "team@work.example",
            "",
            "",
            "Offsite",
            "See you at nine.",
            "<offsite-1@work.example>",
            "",
            "",
        ]),
        row([
            "work",
            "ada@work.example",
            "board@work.example",
            "",
            "auditor@work.example",
            "Quarterly numbers",
            "The numbers are attached.\n\nThe totals moved.",
            "",
            INVITATION,
            "Attachments not carried over: numbers.pdf",
        ]),
        row([
            "home",
            "ada@home.example",
            "grace@hopper.example",
            "linus@home.example",
            "",
            "Re: Lunch",
            "Thursday works.",
            "<lunch-1@hopper.example>",
            "",
            "Notes: Ask about the venue",
        ]),
        row([
            "",
            "",
            "mia@work.example",
            "",
            "",
            "Your launch numbers",
            "Mia, the launch is down to a second and a half.",
            "",
            "",
            "Notes: Drafted from the release channel",
        ]),
    ]
}

/// What Leo wrote in the same mailbox: his own draft, which only he reads.
fn leos() -> Vec<Row> {
    vec![row([
        "work",
        "leo@work.example",
        "ops@work.example",
        "",
        "",
        "Rota",
        "I can take Friday.",
        "",
        "",
        "",
    ])]
}

/// `name`'s mailbox with the native app's `documents` beside it.
fn opened(name: &str, doc: Workspace, documents: &[(String, AutoCommit)]) -> (Replica, ResourceRef) {
    let (r, _) = Replica::new(name, doc, MAIL_SET);
    r.host.runtime().set_earlier(MAIL_APP, documents);
    let mail = r.host.root_of(MAIL_APP).expect("the mailbox");
    r.commit();
    (r, mail)
}

/// A device of `name` that receives what `from` wrote, as sync hands it what its person reads,
/// with the native app's `documents` beside it as well.
fn device(from: &Replica, name: &str, documents: &[(String, AutoCommit)]) -> Replica {
    let (r, _) = Replica::new(name, Workspace::new(), MAIL_SET);
    from.send_to(&r);
    r.host.runtime().set_earlier(MAIL_APP, documents);
    r
}

#[test]
fn a_native_mailbox_is_carried_over_once_each_draft_its_authors() {
    let documents = native_documents();
    let before: Vec<_> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    let (ada, mail) = opened("u_ada", Workspace::new(), &documents);
    assert!(rows(&ada, &mail).is_empty(), "nothing before the migration");

    assert_eq!(ada.host.migrate(), 1, "the mailbox's migration runs");
    ada.commit();
    let migration = ada
        .host
        .package(MAIL_APP)
        .unwrap()
        .meta
        .migrations()
        .find(|m| m.name == "carry_native_mail")
        .unwrap()
        .id
        .clone();
    let versions = ada.host.runtime().versions(&mail).unwrap();
    assert_eq!(
        versions.iter().filter(|v| v.action == migration).count(),
        1,
        "one change of the mailbox carried them: {versions:?}"
    );
    assert_eq!(ada.host.migrate(), 0, "it runs once");
    assert_eq!(ada.commit(), 0);

    // Ada's other device, opening the same mailbox and the same native documents later, finds
    // it done and holds her drafts alone; Leo's holds his own and none of hers.
    let laptop = device(&ada, "u_ada", &documents);
    assert_eq!(laptop.host.migrate(), 0, "another device finds it done");
    assert_eq!(rows(&laptop, &mail), adas(), "Ada's drafts, each once");
    let leo = device(&ada, "u_leo", &documents);
    assert_eq!(leo.host.migrate(), 0, "and so does Leo's");
    assert_eq!(rows(&leo, &mail), leos(), "Leo's draft, his alone");

    let after: Vec<_> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    assert_eq!(before, after, "the native app's documents are not written");
}

#[test]
fn a_mailbox_opened_before_the_native_documents_arrived_receives_them_once_they_do() {
    let (ada, mail) = opened("u_ada", Workspace::new(), &[]);
    assert_eq!(ada.host.migrate(), 0, "nothing to carry has arrived");
    assert_eq!(ada.commit(), 0, "and nothing is recorded");

    let documents = native_documents();
    ada.host.runtime().set_earlier(MAIL_APP, &documents);
    assert_eq!(ada.host.migrate(), 1, "the carrying waited for them");
    ada.commit();
    assert_eq!(ada.host.migrate(), 0, "and runs only once");
    let laptop = device(&ada, "u_ada", &documents);
    assert_eq!(laptop.host.migrate(), 0);
    assert_eq!(rows(&laptop, &mail), adas());
}
