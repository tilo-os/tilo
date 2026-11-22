//! Mentioning an agent in any app's rich text asks it: the system makes one ask per mention,
//! made from the item the text is in, by who wrote it, through the agent package's actions,
//! with no code in the app. Edits, undo and redo of the same text ask nothing new; removing the
//! mention withdraws a waiting ask, and putting it back asks again.

use automerge::AutoCommit;
use collaboration::history_source::verbs::HistoryEdit;
use serde_json::Value as Json;
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_runtime::rich::{Reference, RichValue};
use tilolang_runtime::store::Store as _;
use tilolang_runtime::{ResourceRef, Value};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::runtime::Actor;
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::{Replica, field_id, target};
use crate::legibility::call;

const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);
const AGENT: (&str, &str) = (
    include_str!("../../../../apps/tl/agent/app.tl"),
    include_str!("../../../../apps/tl/agent/app.ids"),
);
const NOTES_APP: &str = "so.tilo.notes";
const CHAT_APP: &str = "so.tilo.lang.chat";
const AGENT_APP: &str = "so.tilo.lang.agent";

fn helper() -> MemberId {
    MemberId::agent("helper")
}

/// Ada's workspace with Notes, Chat and the agents' asks, the helper agent a member of it: it
/// reads everything when `reads`, and nothing otherwise, as the workspace's grants say.
fn workspace(reads: bool) -> Replica {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[DISCUSSION, NOTES, AGENT, CHAT]);
    ada.host.set_clock(|| "2026-11-21T09:00:00Z".to_owned());
    let grants: std::rc::Rc<tilolang_runtime::members::Reads> =
        std::rc::Rc::new(move |who: &Actor, _: &ResourceRef| {
            reads || who.member_id().is_some_and(|member| !member.is_agent())
        });
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_action_policy::Role::Owner),
            tilolang_runtime::members::Member::agent("helper"),
        ],
        Some(grants),
    );
    ada.commit();
    ada
}

/// One ask as the agent package keeps it.
#[derive(Debug)]
struct Asked {
    agent: Actor,
    said: Option<String>,
    mention: String,
    state: String,
    who: Option<Actor>,
    from: Option<String>,
}

fn asks(r: &Replica) -> Vec<Asked> {
    let root = r.host.root_of(AGENT_APP).unwrap();
    let Value::List(items) = r.host.runtime().read(&root, "asks").unwrap() else {
        panic!("the asks are a list");
    };
    let meta = r.host.package(AGENT_APP).unwrap().meta;
    items
        .iter()
        .map(|item| {
            let at = target(item);
            let read = |field: &str| r.host.runtime().read(&at, field).unwrap();
            let Value::Actor(agent) = read("agent") else {
                panic!("an agent")
            };
            let said = match read("said") {
                Value::Opt(Some(said)) => match *said {
                    Value::Rich(rich) => Some(rich.written_text()),
                    other => panic!("rich text: {other:?}"),
                },
                _ => None,
            };
            let Value::Text(mention) = read("mention") else {
                panic!("a mention")
            };
            let Value::Enum(state) = read("state") else {
                panic!("a state")
            };
            let state = meta
                .enums
                .iter()
                .flat_map(|e| e.variants.iter())
                .find(|v| v.id == state.variant)
                .map_or_else(|| state.variant.clone(), |v| v.name.clone());
            let made = r.host.store().versions(&at).unwrap();
            let first = made.first().expect("the ask was made");
            Asked {
                agent,
                said,
                mention,
                state,
                who: first.who.clone(),
                from: first.from.as_ref().map(ResourceRef::id_string),
            }
        })
        .collect()
}

/// Commit everything the runtime committed, the asks its mentions made included.
fn settle(r: &mut Replica) {
    while r.commit() > 0 {}
}

fn rich_of(r: &Replica, item: &ResourceRef, field: &str) -> RichValue {
    match r.host.runtime().read(item, field).unwrap() {
        Value::Rich(rich) => rich,
        other => panic!("{field} is rich text: {other:?}"),
    }
}

/// Ada edits the note's body with the rich edit `action`, as her editor does: one step of hers.
fn edit_body(r: &mut Replica, note: &ResourceRef, action: &tilo_document::RichTextAction) {
    let meta = r.host.package(NOTES_APP).unwrap().meta;
    let path = vec![
        Segment::Item(Id::from_raw(note.item.as_deref().unwrap())),
        Segment::Field(field_id(&meta, "Note", "body")),
    ];
    let rich = serde_json::to_value(action).unwrap();
    r.apply(&DynAction {
        app: NOTES_APP.to_owned(),
        kind: format!("tl.{NOTES_APP}.Note"),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        // The person at the window writes it, as every edit a window plans is theirs.
        authorship: Authorship {
            invoker: Some(ada_person()),
            ..Authorship::default()
        },
        edits: vec![DynEdit::Edit {
            id: note.doc.clone(),
            edit: Edit::new(path, EditOp::Rich { rich }),
        }],
    });
    settle(r);
}

/// Where `said` begins in the note's first paragraph, which holds it: the block and the offset.
fn find(r: &Replica, note: &ResourceRef, said: &str) -> (String, usize) {
    let body = rich_of(r, note, "body");
    body.blocks
        .iter()
        .find_map(|block| {
            block.text.find(said).map(|byte| {
                let before = block.text.get(..byte).unwrap_or_default();
                (block.id.clone(), before.chars().count())
            })
        })
        .unwrap_or_else(|| panic!("the body says {said}: {:?}", body.written_text()))
}

fn type_after(r: &mut Replica, note: &ResourceRef, after: &str, typed: &str) {
    let (block, at) = find(r, note, after);
    edit_body(
        r,
        note,
        &tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block,
            at: at.saturating_add(after.chars().count()),
            del: 0,
            ins: typed.to_owned(),
        },
    );
}

/// Ada marks `name` in the note as a mention of `member` (`None` takes the mention away), as the
/// composer does when she picks them from its suggestions.
fn mark(r: &mut Replica, note: &ResourceRef, name: &str, member: Option<&MemberId>) {
    let (block, start) = find(r, note, name);
    edit_body(
        r,
        note,
        &tilo_document::RichTextAction::SetMark {
            region: tilo_document::action::RegionKind::Body,
            spans: vec![tilo_document::action::Span {
                block,
                start,
                end: start.saturating_add(name.chars().count()),
            }],
            name: tilolang_runtime::rich::MENTION.to_owned(),
            value: member.map(|member| {
                tilo_document::action::MarkVal::Str(Reference::Member(member.clone()).mark_value())
            }),
            also_clear: Vec::new(),
        },
    );
}

fn history(r: &mut Replica, edit: &HistoryEdit) {
    r.history(edit);
    settle(r);
}

fn undo(r: &mut Replica) {
    history(
        r,
        &HistoryEdit::Undo {
            steps: None,
            invocation: None,
            by: None,
            to: None,
            step: None,
        },
    );
}

fn redo(r: &mut Replica) {
    history(r, &HistoryEdit::Redo { steps: None });
}

/// A note of Ada's saying `body`.
fn note(ada: &mut Replica, body: &str) -> ResourceRef {
    let root = Value::resource(&ada.host.root_of(NOTES_APP).unwrap());
    let note = target(&call(ada, "ada", NOTES_APP, "new_note", vec![root]));
    settle(ada);
    let blocks: Vec<tilo_document::ClipBlock> = tilo_document::blocks_of_plain(body)
        .iter()
        .map(tilo_document::ClipBlock::from_view)
        .collect();
    edit_body(
        ada,
        &note,
        &tilo_document::RichTextAction::SyncClips {
            blocks,
            replace: None,
            at: 0,
        },
    );
    note
}

fn ada_person() -> Actor {
    MemberId::person("ada").into()
}

#[test]
fn mentioning_an_agent_in_a_note_asks_it_once_however_often_the_paragraph_is_edited() {
    let mut ada = workspace(true);
    let note = note(&mut ada, "Could Helper sum this up");
    assert!(asks(&ada).is_empty(), "nobody is mentioned yet");
    mark(&mut ada, &note, "Helper", Some(&helper()));
    type_after(&mut ada, &note, "sum this up", " please");
    type_after(&mut ada, &note, "please", ", by Friday");

    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "{asked:#?}");
    let ask = &asked[0];
    assert_eq!(ask.agent, Actor::from(helper()));
    assert_eq!(ask.mention, Reference::Member(helper()).mark_value());
    assert_eq!(ask.state, "waiting");
    assert_eq!(ask.who.as_ref(), Some(&ada_person()), "Ada asked");
    assert_eq!(ask.from.as_deref(), Some(note.id_string().as_str()));
    assert_eq!(
        ask.said.as_deref(),
        Some("Could Helper sum this up"),
        "what was asked is the text as it read when asked"
    );
}

#[test]
fn removing_the_mention_withdraws_the_ask_and_undo_asks_again() {
    let mut ada = workspace(true);
    let note = note(&mut ada, "Helper, a summary please");
    mark(&mut ada, &note, "Helper", Some(&helper()));
    assert_eq!(asks(&ada).len(), 1);

    mark(&mut ada, &note, "Helper", None);
    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "{asked:#?}");
    assert_eq!(asked[0].state, "withdrawn");

    undo(&mut ada);
    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "still the one ask: {asked:#?}");
    assert_eq!(asked[0].state, "waiting", "the mention is back");

    redo(&mut ada);
    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "{asked:#?}");
    assert_eq!(asked[0].state, "withdrawn", "the mention is gone again");
}

#[test]
fn a_chat_message_mentioning_an_agent_asks_it_about_that_message() {
    let mut ada = workspace(true);
    let all = Value::resource(&ada.host.root_of(CHAT_APP).unwrap());
    let design = call(
        &ada,
        "ada",
        CHAT_APP,
        "new_channel",
        vec![all, Value::text("design")],
    );
    settle(&mut ada);
    let mut body = RichValue::from_text("Can ");
    body.push_reference("@Helper", &Reference::Member(helper()));
    body.push_text(" review the shots?");
    let message = call(
        &ada,
        "ada",
        CHAT_APP,
        "send",
        vec![design, Value::Rich(body), Value::Opt(None)],
    );
    settle(&mut ada);

    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "{asked:#?}");
    assert_eq!(
        asked[0].from.as_deref(),
        Some(target(&message).id_string().as_str()),
        "made from the message, not its conversation"
    );
    assert_eq!(asked[0].who.as_ref(), Some(&ada_person()));
    assert_eq!(
        asked[0].said.as_deref(),
        Some("Can @Helper review the shots?")
    );
}

#[test]
fn an_agent_that_may_not_read_the_note_is_asked_with_nothing_of_it() {
    let mut ada = workspace(false);
    let note = note(&mut ada, "Helper, the salaries are in here");
    mark(&mut ada, &note, "Helper", Some(&helper()));
    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "{asked:#?}");
    assert_eq!(asked[0].said, None, "nothing of the note is copied");
    assert_eq!(asked[0].state, "cannot_read");
}

#[test]
fn mentioning_a_person_asks_nobody() {
    let mut ada = workspace(true);
    let note = note(&mut ada, "Grace, have a look");
    mark(&mut ada, &note, "Grace", Some(&MemberId::person("grace")));
    assert!(asks(&ada).is_empty(), "{:#?}", asks(&ada));
}
