//! The chat this app writes, saved for the tilolang Chat to convert once it opens a workspace
//! this app wrote: the documents of a chat made with this app's own actions, its record and
//! every part of its messages, as a workspace keeps them.
//!
//! The fixture is `crates/lang/tilolang-tilo/tests/legacy/rust_chat.hex`, one document a
//! line: its document id, a tab, the saved Automerge document as hex. Write it again with
//!
//! ```text
//! t exec --fetch crates/lang/tilolang-tilo/tests/legacy/rust_chat.hex -- \
//!     cargo test --locked -p chat --test it native_chat_fixture -- --ignored
//! ```

use std::fmt::Write as _;

use chat::chat::{Chat, Quote, chat as verbs};
use chat::demo::{Workspace, start};
use chat::messages::{NewMessage, chat_messages};
use chat::parts::{self, ChatMessages};
use ui::resource::DocumentId;
use ui::tilo_resource::{Id, Invoke, MemberId};

/// Where the fixture is written, from this crate.
const FIXTURE: &str = "../../crates/lang/tilolang-tilo/tests/legacy/rust_chat.hex";

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// A message to post under the fixed id `id`, so the test reading the fixture names it.
fn message(id: &str, channel: &str, text: &str, parent: Option<&str>) -> NewMessage {
    NewMessage {
        id: Some(Id::from_raw(id)),
        channel: Id::from_raw(channel),
        text: text.into(),
        parent: parent.map(Id::from_raw),
        quoted: None,
        body: None,
        attachments: Vec::new(),
        mentions: None,
        written: None,
        origin: None,
    }
}

fn post(workspace: &mut Workspace, by: &MemberId, at: i64, message: NewMessage) {
    workspace
        .apply_part_at(
            by,
            at,
            ChatMessages::POST.with(chat_messages::Post { message }),
        )
        .unwrap();
}

fn react(workspace: &mut Workspace, by: &MemberId, at: i64, message: &str, emoji: &str) {
    let edit = ChatMessages::paths()
        .messages
        .item(&Id::from_raw(message))
        .reactions
        .react(emoji);
    workspace
        .apply_part_at(by, at, Invoke::edit(&edit).unwrap())
        .unwrap();
}

#[test]
#[ignore = "writes the fixture the tilolang Chat converts; run it by name to write it again"]
fn native_chat_fixture() {
    let ada = MemberId::person("u_ada");
    let leo = MemberId::person("u_leo");
    let mia = MemberId::person("u_mia");
    let mut workspace = Workspace::default();
    // The workspace's first channels, #chat and #voice, as the first one to open it makes them.
    start(&mut workspace.chat, "u_ada").unwrap();
    channels(&mut workspace);
    conversation(&mut workspace, &ada, &leo, &mia);
    direct_message(&mut workspace, &ada, &leo);
    write_fixture(&mut workspace);
}

/// The categories and channels the fixture's chat holds, one channel archived.
fn channels(workspace: &mut Workspace) {
    let apply = |workspace: &mut Workspace, invoke| workspace.apply_as("u_ada", invoke).unwrap();
    apply(
        workspace,
        Chat::CREATE_CATEGORY.with(verbs::CreateCategory {
            name: "Team".parse().unwrap(),
        }),
    );
    apply(
        workspace,
        Chat::CREATE_CATEGORY.with(verbs::CreateCategory {
            name: "Social".parse().unwrap(),
        }),
    );
    let categories = workspace.read().categories;
    let category = |name: &str| {
        categories
            .iter()
            .find(|(_, c)| c.name == name)
            .map(|(id, _)| id.clone())
            .unwrap()
    };
    let (team, social) = (category("Team"), category("Social"));
    for (id, name, category, topic) in [
        ("c_general", "general", None, Some("Anything, really")),
        (
            "c_design",
            "design",
            Some(team.clone()),
            Some("Reviews and critique"),
        ),
        ("c_release", "release", Some(team), None),
        ("c_weekend", "weekend", Some(social), None),
        (
            "c_plans",
            "old-plans",
            None,
            Some("What we planned last year"),
        ),
    ] {
        apply(
            workspace,
            Chat::CREATE_CHANNEL_WITH_ID.with(verbs::CreateChannelWithId {
                id: id.into(),
                name: name.parse().unwrap(),
                category,
                kind: None,
                topic: topic.map(str::to_owned),
                members: None,
            }),
        );
    }
    apply(
        workspace,
        Chat::ARCHIVE_CHANNEL.with(verbs::ArchiveChannel {
            channel: Id::from_raw("c_plans"),
            archived: true,
        }),
    );
}

/// The conversation in #design: what was said, reacted to and pinned.
fn conversation(workspace: &mut Workspace, ada: &MemberId, leo: &MemberId, mia: &MemberId) {
    // 2026-06-29, from 07:02 UTC.
    let at = |minute: i64| 1_782_716_520_000_i64.saturating_add(minute.saturating_mul(60_000));
    post(
        workspace,
        mia,
        at(0),
        message(
            "m-build",
            "c_design",
            "The new onboarding build is on the test machines.",
            None,
        ),
    );
    post(
        workspace,
        mia,
        at(1),
        message(
            "m-launch",
            "c_design",
            "First launch is down to a second and a half.",
            None,
        ),
    );
    react(workspace, leo, at(3), "m-launch", "🎉");
    react(workspace, ada, at(4), "m-launch", "🎉");
    react(workspace, ada, at(4), "m-launch", "🚀");
    workspace
        .apply_part_at(
            ada,
            at(4),
            ChatMessages::PIN.with(chat_messages::Pin {
                message: Id::from_raw("m-launch"),
                pinned: true,
            }),
        )
        .unwrap();
    post(
        workspace,
        leo,
        at(18),
        message(
            "m-ask",
            "c_design",
            "Can we move the account step to the end?",
            None,
        ),
    );
    post(
        workspace,
        ada,
        at(29),
        message(
            "m-thread-1",
            "c_design",
            "Sign-in first keeps sync simple, but people bounce there.",
            Some("m-ask"),
        ),
    );
    let mut quoting = message(
        "m-thread-2",
        "c_design",
        "Then let them in first.",
        Some("m-ask"),
    );
    quoting.quoted = Some(Quote {
        text: "the account step".into(),
        start: 12,
        end: 28,
    });
    post(workspace, leo, at(38), quoting);
}

/// What #general and #old-plans hold, and the direct conversation between two people.
fn direct_message(workspace: &mut Workspace, ada: &MemberId, leo: &MemberId) {
    let at = |minute: i64| 1_782_716_520_000_i64.saturating_add(minute.saturating_mul(60_000));
    post(
        workspace,
        leo,
        at(88),
        message(
            "m-coffee",
            "c_general",
            "Coffee machine on the third floor is fixed.",
            None,
        ),
    );
    post(
        workspace,
        ada,
        at(90),
        message("m-plans", "c_plans", "Ship it by spring.", None),
    );
    let direct = Chat::direct_id([ada.clone(), leo.clone()]);
    post(
        workspace,
        leo,
        at(120),
        message(
            "m-walk",
            direct.as_str(),
            "Are we still on for the walk on Saturday?",
            None,
        ),
    );
    post(
        workspace,
        ada,
        at(121),
        message("m-walk-yes", direct.as_str(), "Yes, at ten.", None),
    );
}

/// Writes the workspace's documents, one a line, to the fixture file.
fn write_fixture(workspace: &mut Workspace) {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}\t{}",
        DocumentId::workspace_default().as_str(),
        hex(&workspace.chat.save())
    );
    for index in workspace.part_numbers() {
        let saved = workspace.part_bytes(index).expect("the part is held");
        let _ = writeln!(out, "{}\t{}", parts::part(index).as_str(), hex(&saved));
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    std::fs::write(&path, out).unwrap();
}
