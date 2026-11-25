//! Chat's smaller conversation affordances, through its published actions and views: a
//! category's heading is kept by the category itself, a thread's chip says who replied last and
//! when and whether the thread was set aside, and a conversation's transcript follows its end.

use crate::fixtures::{Replica, Workspace, target};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value, ViewInstance};

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const APP: &str = "so.tilo.chat";

fn person(name: &str) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name))
}

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&person(&r.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

/// Every text a node and its children show, rich text as written.
fn texts(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            Value::Rich(rich) => Some(rich.written_text()),
            _ => None,
        })
        .collect()
}

/// The transcript's rows as the list builds them.
fn rows(view: &mut ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let list = view
        .tree()
        .first_of_kind("ui.VirtualList")
        .expect("a transcript")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| view.row(&list, i).unwrap().cloned())
        .collect()
}

/// The key of the button in `node` labelled `label`.
fn button(node: &UiNode, label: &str) -> Option<String> {
    node.of_kind("ui.Button")
        .iter()
        .find(|b| b.props.get("label") == Some(&Value::text(label)))
        .map(|b| b.key.clone())
}

/// Open `at` the way the system does: its type's `opened action` as the person, then its window.
fn open(r: &Replica, at: &tilolang_runtime::ResourceRef) -> ViewInstance {
    if let Some(action) = r.host.runtime().opened_action(&at.ty) {
        match r
            .host
            .runtime()
            .invoke_action(
                &person(&r.name),
                (action.as_str(), vec![Value::resource(at)]),
            )
            .unwrap()
        {
            InvokeOutcome::Done(_) => {}
            other => panic!("opening {at:?}: {other:?}"),
        }
    }
    r.host.open_resource(at, None).unwrap()
}

/// Type `text` into the composer of `view` and press its send button, as a person does.
fn post(r: &Replica, view: &ViewInstance, text: &str) {
    let input = view
        .tree()
        .first_of_kind("ui.RichInput")
        .expect("the composer")
        .key
        .clone();
    view.edit(&input, &Value::Rich(RichValue::from_text(text)))
        .unwrap();
    let send = button(view.tree(), "Send").expect("a send button");
    let outcome = view.press(&send).unwrap();
    assert!(
        matches!(outcome, InvokeOutcome::Done(_)),
        "sent: {outcome:?}"
    );
    r.commit();
}

/// Words that say a phrase's id, so what a chip says names the phrase; a declaration's words
/// stay the source's.
struct EchoWords;

impl tilolang_runtime::Words for EchoWords {
    fn add(&self, _catalogues: &[(String, String)]) {}

    fn add_used(&self, _catalogues: &[(String, String)]) {}

    fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
        if key.starts_with("d-") {
            source.to_owned()
        } else {
            format!("{key} {args:?}")
        }
    }
}

/// A workspace with Ada's channel `design` under the category `Team`.
fn workspace() -> (
    Replica,
    Vec<tilolang_tilo::host::Package>,
    Value,
    Value,
    Value,
) {
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let team = call(&ada, "new_category", vec![all.clone(), Value::text("Team")]);
    let design = call(
        &ada,
        "new_channel",
        vec![
            all.clone(),
            Value::text("design"),
            Value::text(""),
            Value::some(team.clone()),
        ],
    );
    ada.commit();
    (ada, loaded, all, team, design)
}

/// A category's heading names the category it is, so the window keeps what the person did to it
/// (folding it away) by the category, not by its name or where it stands: renamed and moved, the
/// heading still names the same category.
#[test]
fn a_category_heading_is_kept_by_the_category_across_a_rename_and_a_move() {
    let (ada, _, all, team, _) = workspace();
    // Ada owns the workspace: she arranges its categories.
    ada.host.runtime().set_members(
        vec![tilolang_runtime::members::Member::person(
            "ada",
            tilo_actions::policy::Role::Owner,
        )],
        None,
    );
    let ops = call(&ada, "new_category", vec![all.clone(), Value::text("Ops")]);
    call(
        &ada,
        "new_channel",
        vec![
            all.clone(),
            Value::text("deploys"),
            Value::text(""),
            Value::some(ops),
        ],
    );
    ada.commit();
    let heading = |title: &str| -> Value {
        let sidebar = ada.view(APP);
        sidebar
            .tree()
            .of_kind("ui.NavSection")
            .into_iter()
            .find(|s| s.props.get("title") == Some(&Value::text(title)))
            .unwrap_or_else(|| panic!("the heading {title}"))
            .props
            .get("item")
            .cloned()
            .expect("the heading names its category")
    };
    let before = heading("Team");
    assert_eq!(target(&before), target(&team));
    call(
        &ada,
        "rename_category",
        vec![team.clone(), Value::text("Product")],
    );
    let sidebar = ada.view(APP);
    let menu = sidebar
        .tree()
        .of_kind("ui.NavSection")
        .into_iter()
        .find(|s| s.props.get("title") == Some(&Value::text("Product")))
        .expect("the renamed heading")
        .clone();
    let down = menu
        .of_kind("ui.MenuItem")
        .into_iter()
        .find(|item| item.props.get("label") == Some(&Value::text("Move down")))
        .expect("Move down on the heading")
        .key
        .clone();
    let moved = sidebar.fire(&down, "choose").unwrap();
    assert!(matches!(moved, InvokeOutcome::Done(_)), "{moved:?}");
    ada.commit();
    let sidebar = ada.view(APP);
    let titles: Vec<Value> = sidebar
        .tree()
        .of_kind("ui.NavSection")
        .into_iter()
        .filter_map(|s| s.props.get("title").cloned())
        .collect();
    assert_eq!(
        titles[..2],
        [Value::text("Ops"), Value::text("Product")],
        "the category moved below the other"
    );
    assert_eq!(
        target(&heading("Product")),
        target(&team),
        "renamed and moved, the heading is the same category's"
    );
}

/// The message a thread started from says who answered in it last, the start of what they said
/// and when; the person who started it sets it aside from the thread's own page, the chip says it
/// is archived, and a reply in it picks it up again.
#[test]
fn a_thread_chip_shows_the_last_reply_and_an_archived_thread_reopens_on_a_reply() {
    let (ada, loaded, _, _, design) = workspace();
    let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    for r in [&ada, &ben] {
        r.host.set_words(EchoWords);
    }
    let asked = call(
        &ada,
        "send",
        vec![
            design.clone(),
            Value::Rich(RichValue::from_text("Is the build out?")),
        ],
    );
    let thread = call(&ada, "start_thread", vec![design.clone(), asked]);
    ada.commit();
    ada.sync_with(&ben);
    let ben_thread = open(&ben, &target(&thread));
    post(
        &ben,
        &ben_thread,
        "Since this morning, on all three machines.",
    );
    ada.sync_with(&ben);

    let mut channel = open(&ada, &target(&design));
    let row = rows(&mut channel).remove(0);
    let faces: Vec<Option<Value>> = row
        .of_kind("ui.Avatar")
        .iter()
        .map(|a| a.props.get("who").cloned())
        .collect();
    assert!(
        faces.contains(&Some(Value::Actor(person("ben")))),
        "the chip shows who replied last: {faces:?}"
    );
    let shown = texts(&row);
    assert!(
        shown.iter().any(|t| t.starts_with("Since this morning")),
        "the chip quotes the start of the last reply: {shown:?}"
    );
    assert!(
        row.first_of_kind("ui.Time").is_some(),
        "the chip says when the last reply came"
    );
    assert!(
        !shown.iter().any(|t| t.contains("tilolang-thread-archived")),
        "an open thread says nothing of being archived: {shown:?}"
    );

    // Ada, who started it, sets the thread aside from its page.
    let mut ada_thread = open(&ada, &target(&thread));
    ada_thread.poll_patches().unwrap();
    let archive = button(ada_thread.tree(), "Archive thread").expect("Archive on the thread");
    let outcome = ada_thread.press(&archive).unwrap();
    assert!(matches!(outcome, InvokeOutcome::Done(_)), "{outcome:?}");
    ada.commit();
    ada_thread.poll_patches().unwrap();
    let page = texts(ada_thread.tree());
    assert!(
        page.contains(&"This thread is archived. A reply reopens it.".to_owned()),
        "the thread's page says it was set aside: {page:?}"
    );
    assert!(
        button(ada_thread.tree(), "Reopen thread").is_some(),
        "and offers to reopen it"
    );
    let row = rows(&mut channel).remove(0);
    assert!(
        texts(&row)
            .iter()
            .any(|t| t.contains("tilolang-thread-archived")),
        "the chip says the thread is archived: {:?}",
        texts(&row)
    );

    // Ben answers in it, and the thread is open again for both.
    ada.sync_with(&ben);
    let ben_thread = open(&ben, &target(&thread));
    post(&ben, &ben_thread, "One more thing.");
    ada.sync_with(&ben);
    let row = rows(&mut channel).remove(0);
    let shown = texts(&row);
    assert!(
        !shown.iter().any(|t| t.contains("tilolang-thread-archived")),
        "a reply reopened the thread: {shown:?}"
    );
    assert!(
        shown.iter().any(|t| t.starts_with("One more thing")),
        "the chip now quotes the newest reply: {shown:?}"
    );
}

/// A conversation's transcript follows its end: the list keeps the newest message in view as
/// messages arrive, and while the person reads back it offers the way to the latest.
#[test]
fn a_channel_transcript_follows_its_end_and_offers_the_way_back_to_the_latest() {
    let (ada, _, _, _, design) = workspace();
    let channel = open(&ada, &target(&design));
    let list = channel
        .tree()
        .first_of_kind("ui.VirtualList")
        .expect("a transcript");
    assert!(
        matches!(list.props.get("follow_end"), Some(Value::Bool(true))),
        "the transcript follows its end: {:?}",
        list.props.get("follow_end")
    );
}
