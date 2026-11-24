# Documents the native apps wrote

Each `*.hex` file here is what a native (Rust) app's own model code wrote before the tilolang
app of the same id replaced it: one document a line, its document id, a tab, and the saved
Automerge document as hex. The `native_*` tests open them with the tilolang app and prove its
`migration action` carries what the native app showed. They are never written by hand.

| Fixture | Written by | Native app's last commit |
| --- | --- | --- |
| `rust_notes.hex` | `apps/notes/tests/rust_notebook_fixture.rs` | `8ec58a62bb0` |
| `rust_chat.hex` | `apps/chat/tests/native_chat_fixture.rs` | `9fb3a1ad143` |
| `rust_skills.hex` | `apps/chat-model/tests/native_skills_fixture.rs` | `0ae397b1f5a` |
| `rust_tube.hex` | `apps/tube-model/tests/native_tube_fixture.rs` | `0ae397b1f5a` |
| `rust_mail.hex` | `apps/mail/tests/native_mail_fixture.rs` | `0ae397b1f5a` |
| `rust_discord.hex` | `apps/discord/tests/native_discord_fixture.rs` | `9156778ba7b` |

`0ae397b1f5a` is the parent of the commit that deleted the Rust Mail; the Tube model, the Chat
skills model and the resource layer they store through did not change from there to their own
deletions. Their generators are below. To write a fixture again, check that commit out in a
worktree of its own, add the generator, and run it there with the build it had (that
commit's `t.toml` names `starts` today's t no longer plans; empty it there first):

`rust_discord.hex` is written by `apps/discord/tests/native_discord_fixture.rs`, which is in the
tree beside the native app while it runs; it moves here with the generators below when that app
is deleted.

```sh
git worktree add --detach ../native-apps 0ae397b1f5a
# add the generator as the file the table names, then, in that worktree:
t exec --no-build --fetch 'crates/lang/tilolang-tilo/tests/legacy/rust_*.hex' -- \
    cargo test --locked -p chat-model --test native_skills_fixture -- --ignored
```

(`-p tube-model --test native_tube_fixture` and `-p mail --test native_mail_fixture` likewise.)
Each generator writes its fixture into this directory of that worktree; copy it here.

## Generators

### `apps/chat-model/tests/native_skills_fixture.rs`

```rust
//! The skills this model writes, saved for the tilolang Chat to carry over.

use std::fmt::Write as _;

use chat_model::skills::{Skills, skills};
use tilo_resource::{Id, NonBlank, Replica};

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn ids(replica: &Replica<Skills>) -> Vec<(Id, String)> {
    replica
        .state()
        .unwrap()
        .skills
        .iter()
        .map(|(id, skill)| (id.clone(), skill.name.clone()))
        .collect()
}

fn skill_named(replica: &Replica<Skills>, name: &str) -> Id {
    ids(replica)
        .into_iter()
        .find(|(_, named)| named == name)
        .map(|(id, _)| id)
        .unwrap()
}

#[test]
#[ignore = "writes the fixture the tilolang Chat carries; run it by name to write it again"]
fn native_skills_fixture() {
    let mut ada = Replica::<Skills>::new().as_author("u_ada");
    // An agent writes the first skill for Ada, with a model and a form.
    let mut agent = ada.fork().as_agent_for("assistant", "u_ada");
    agent.set_now_ms(1_700_000_000_000);
    agent
        .apply(Skills::WRITE_SKILL.with(skills::WriteSkill {
            name: NonBlank::parse("Weekly report").unwrap(),
            work: NonBlank::parse("Answering a request that asks for a weekly report").unwrap(),
            how: "Read the numbers from the sheet.\nSay what moved first.".into(),
            form: Some("Three short paragraphs".into()),
            model: Some("Anthropic/claude-research".into()),
        }))
        .unwrap();
    ada.sync_with(&mut agent).unwrap();
    // Ada writes the second one herself, with no fixed form and the model of her choice.
    ada.set_now_ms(1_700_000_060_000);
    ada.apply(Skills::WRITE_SKILL.with(skills::WriteSkill {
        name: NonBlank::parse("Brief").unwrap(),
        work: NonBlank::parse("Answering a request for a short brief").unwrap(),
        how: "Background first, then the decision.".into(),
        form: None,
        model: None,
    }))
    .unwrap();
    let weekly = skill_named(&ada, "Weekly report");
    let brief = skill_named(&ada, "Brief");
    // Examples: accepted and rejected, with and without words; one said again moves to the end,
    // one is taken out again.
    for (at, skill, text, accepted, note) in [
        (1_700_000_100_000_i64, &weekly, "Revenue rose 4 %, churn fell.", true, None),
        (
            1_700_000_200_000,
            &weekly,
            "It was a week of things.",
            false,
            Some("Too vague"),
        ),
        (1_700_000_300_000, &weekly, "Drop this one.", false, None),
        (
            1_700_000_400_000,
            &weekly,
            "Revenue rose 4 %, churn fell.",
            true,
            Some("Exactly this"),
        ),
        (1_700_000_500_000, &brief, "Context: the move. Decision: Tuesday.", true, None),
    ] {
        ada.set_now_ms(at);
        ada.apply(Skills::KEEP_EXAMPLE.with(skills::KeepExample {
            skill: skill.clone(),
            text: text.into(),
            accepted,
            note: note.map(str::to_owned),
        }))
        .unwrap();
    }
    let dropped = ada
        .state()
        .unwrap()
        .skills
        .get(&weekly)
        .unwrap()
        .examples
        .iter()
        .find(|example| example.text == "Drop this one.")
        .unwrap()
        .id
        .clone();
    ada.set_now_ms(1_700_000_600_000);
    ada.apply(Skills::DROP_EXAMPLE.with(skills::DropExample {
        skill: weekly.clone(),
        example: dropped,
    }))
    .unwrap();
    // A skill written and forgotten again leaves nothing behind.
    ada.apply(Skills::WRITE_SKILL.with(skills::WriteSkill {
        name: NonBlank::parse("Old habit").unwrap(),
        work: NonBlank::parse("Nothing any more").unwrap(),
        how: "Nothing.".into(),
        form: None,
        model: None,
    }))
    .unwrap();
    let old = skill_named(&ada, "Old habit");
    ada.apply(Skills::FORGET_SKILL.with(skills::ForgetSkill { skill: old }))
        .unwrap();

    let out = format!("chat.skills\t{}\n", hex(&ada.save()));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/lang/tilolang-tilo/tests/legacy/rust_skills.hex");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, out).unwrap();
}
```

### `apps/tube-model/tests/native_tube_fixture.rs`

```rust
//! The library and watch history this model writes, saved for the tilolang YouTube to carry over.

use std::fmt::Write as _;

use tilo_resource::{Invoke, NonBlank, Replica};
use tube_model::{Library, WatchHistory};

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[test]
#[ignore = "writes the fixture the tilolang YouTube carries; run it by name to write it again"]
fn native_tube_fixture() {
    let mut library = Replica::<Library>::new().as_author("u_ada");
    let edit = |library: &mut Replica<Library>, edit| {
        library.apply(Invoke::edit(&edit).unwrap()).unwrap();
    };
    edit(&mut library, Library::subscribe("UC_lexfridman", true));
    edit(&mut library, Library::subscribe("UC_veritasium", true));
    edit(&mut library, Library::subscribe("UC_gone", true));
    edit(&mut library, Library::subscribe("UC_gone", false));
    edit(&mut library, Library::like("v_rockets", true));
    edit(&mut library, Library::like("v_cells", true));
    edit(&mut library, Library::enqueue("v_cells", true));
    edit(&mut library, Library::enqueue("v_bridges", true));
    edit(&mut library, Library::like_comment("Ugx_rockets_1", true));
    library
        .apply(
            Library::REPLY_TO_COMMENT.with(tube_model::library::library::ReplyToComment {
                comment: NonBlank::parse("Ugx_rockets_1").unwrap(),
                text: NonBlank::parse("The second stage lands too now.").unwrap(),
            }),
        )
        .unwrap();
    library
        .apply(
            Library::REPLY_TO_COMMENT.with(tube_model::library::library::ReplyToComment {
                comment: NonBlank::parse("Ugx_cells_9").unwrap(),
                text: NonBlank::parse("Mitochondria, of course.").unwrap(),
            }),
        )
        .unwrap();

    let mut history = Replica::<WatchHistory>::new().as_author("u_ada");
    for (video, at) in [
        ("v_rockets", 1_782_716_520_000_u64),
        ("v_cells", 1_782_716_820_000),
        ("v_bridges", 1_782_717_120_000),
        ("v_forgotten", 1_782_717_420_000),
    ] {
        history
            .apply(WatchHistory::RECORD.with((video.to_owned(), at)))
            .unwrap();
    }
    // Left halfway through one, watched to the end of another.
    history
        .apply(WatchHistory::PROGRESS.with(("v_cells".to_owned(), 300_000, 600_000)))
        .unwrap();
    history
        .apply(WatchHistory::PROGRESS.with(("v_rockets".to_owned(), 599_000, 600_000)))
        .unwrap();
    history
        .apply(WatchHistory::FORGET.with("v_forgotten".to_owned()))
        .unwrap();

    let out = format!(
        "tube.library\t{}\ntube.watch-history\t{}\n",
        hex(&library.save()),
        hex(&history.save())
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/lang/tilolang-tilo/tests/legacy/rust_tube.hex");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, out).unwrap();
}
```

### `apps/mail/tests/native_mail_fixture.rs`

```rust
//! The mailbox this app writes, saved for the tilolang Mail to carry over.

use std::fmt::Write as _;

use mail::actions::DraftData;
use mail::resource::{Mail, mailbox};
use mail_engine::model::{DraftAttachment, DraftSender};
use ui::tilo_resource::{Id, Invoke, Replica, Settled};

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn data(sender: Option<(&str, &str)>, to: &str, cc: &str, bcc: &str, subject: &str, body: &str) -> DraftData {
    DraftData {
        sender: sender.map(|(account, email)| DraftSender {
            account: account.into(),
            email: email.into(),
        }),
        to: to.into(),
        cc: cc.into(),
        bcc: bcc.into(),
        subject: subject.into(),
        body: body.into(),
        html_body: None,
        calendar_ics: None,
        attachments: Vec::new(),
    }
}

const INVITATION: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Tilo//Mail//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:review-1@tilo.example\r\nDTSTAMP:20260629T070000Z\r\nDTSTART:20260702T090000Z\r\nDTEND:20260702T100000Z\r\nSUMMARY:Design review\r\nORGANIZER:mailto:ada@work.example\r\nATTENDEE:mailto:mia@work.example\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[test]
#[ignore = "writes the fixture the tilolang Mail carries; run it by name to write it again"]
fn native_mail_fixture() {
    let mut ada = Replica::<Mail>::new().as_author("u_ada");
    ada.set_now_ms(1_782_716_520_000);
    // The composer's draft: a sender chosen, a blind copy, a body being written, an attachment,
    // an invitation and private notes.
    let mut composed = data(
        Some(("work", "ada@work.example")),
        "board@work.example",
        "",
        "auditor@work.example",
        "Quarterly numbers",
        "The numbers are attached.\n\nThe totals moved.",
    );
    composed.calendar_ics = Some(INVITATION.into());
    composed.attachments = vec![DraftAttachment {
        content_id: format!("mail-attachment:blake3:{}", "ab".repeat(32)),
        filename: "numbers.pdf".into(),
        mime: "application/pdf".into(),
        size: 48_213,
    }];
    ada.apply(Mail::START_DRAFT_BODY.with(mailbox::StartDraftBody {
        text: "The numbers are attached.\n\nThe totals moved.".into(),
        html: None,
        draft: None,
    }))
    .unwrap();
    ada.apply(Mail::SAVE_DRAFT.with(mailbox::SaveDraft {
        message: composed,
        draft: None,
        in_reply_to: None,
    }))
    .unwrap();

    // A reply Ada writes beside it, with notes, and an empty one she started.
    ada.apply(Mail::CREATE_DRAFT.with(mailbox::CreateDraft {
        id: "6f1c2a4e-0d3b-4c8e-9a51-7b2f3e4d5c60".into(),
        message: data(
            Some(("home", "ada@home.example")),
            "grace@hopper.example",
            "linus@home.example",
            "",
            "Re: Lunch",
            "Thursday works.",
        ),
        in_reply_to: Some("<lunch-1@hopper.example>".into()),
        notes: Some("Ask about the venue".into()),
    }))
    .unwrap();
    ada.apply(Mail::CREATE_DRAFT.with(mailbox::CreateDraft {
        id: "0b6e1d2c-3a4f-4e5d-8c7b-9a0f1e2d3c4b".into(),
        message: data(None, "", "", "", "", ""),
        in_reply_to: None,
        notes: None,
    }))
    .unwrap();

    // Leo, another member, writes a draft of his own in the same mailbox.
    let mut leo = ada.fork().as_author("u_leo");
    leo.set_now_ms(1_782_716_580_000);
    leo.apply(Mail::CREATE_DRAFT.with(mailbox::CreateDraft {
        id: "9d8c7b6a-5f4e-4d3c-8b2a-1f0e9d8c7b6a".into(),
        message: data(
            Some(("work", "leo@work.example")),
            "ops@work.example",
            "",
            "",
            "Rota",
            "I can take Friday.",
        ),
        in_reply_to: None,
        notes: None,
    }))
    .unwrap();
    ada.sync_with(&mut leo).unwrap();

    // An agent drafts a message for Ada to review.
    let mut agent = ada.fork().as_agent_for("assistant", "u_ada");
    agent.set_now_ms(1_782_716_640_000);
    agent
        .apply(Mail::DRAFT_MESSAGE.with(mailbox::DraftMessage {
            to: "mia@work.example".into(),
            subject: "Your launch numbers".into(),
            body: "Mia, the launch is down to a second and a half.".into(),
            cc: None,
            notes: Some("Drafted from the release channel".into()),
        }))
        .unwrap();
    ada.sync_with(&mut agent).unwrap();

    // One message is waiting in the outbox, one went already.
    ada.set_now_ms(1_782_716_700_000);
    ada.apply_confirmed(Mail::SEND_MESSAGE.with(mailbox::SendMessage {
        account: "ada@work.example".into(),
        message: data(
            None,
            "team@work.example",
            "",
            "",
            "Offsite",
            "See you at nine.",
        ),
        in_reply_to: Some("<offsite-1@work.example>".into()),
        draft: None,
    }))
    .unwrap();
    ada.apply_confirmed(Mail::SEND_MESSAGE.with(mailbox::SendMessage {
        account: "ada@work.example".into(),
        message: data(None, "old@work.example", "", "", "Sent already", "Gone."),
        in_reply_to: None,
        draft: None,
    }))
    .unwrap();
    let state = ada.state().unwrap();
    let gone: Vec<Id> = state
        .outbox
        .entries()
        .filter(|(_, entry)| entry.value.draft.subject == "Sent already")
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(state.outbox.entries().len(), 2, "both messages are in the outbox");
    assert_eq!(gone.len(), 1);
    for id in gone {
        ada.apply(Invoke::from(
            Mail::paths().outbox.settle(&id, &Settled::Done(())),
        ))
        .unwrap();
    }

    let out = format!("mail.mailbox\t{}\n", hex(&ada.save()));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/lang/tilolang-tilo/tests/legacy/rust_mail.hex");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, out).unwrap();
}
```
