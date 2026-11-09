//! Chat: two people posting live in one channel of one workspace, each seeing the other's
//! messages, inline replies quoting what they answer, and threads branched off a message with
//! their replies counted under it. A workspace holding the conversations of the Chat before
//! channels opens with each conversation a channel and every message still saying what it said.

use automerge::AutoCommit;
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::store::{Store as _, VersionKind};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::{InvokeOutcome, Principal, UiNode, Value, ViewInstance};

use crate::fixtures::Replica;

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const APP: &str = "so.tilo.lang.chat";

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
        .invoke_action(&Principal::new(&r.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

fn rich(text: &str) -> Value {
    Value::Rich(RichValue::from_text(text))
}

fn target(value: &Value) -> ResourceRef {
    match value {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

/// Every text a node and its children show, rich text as written.
fn texts(node: &UiNode, out: &mut Vec<String>) {
    out.extend(node.props.values().filter_map(|value| match value {
        Value::Text(text) => Some(text.clone()),
        Value::Rich(rich) => Some(rich.written_text()),
        _ => None,
    }));
    for child in &node.children {
        texts(child, out);
    }
}

fn find<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
    if node.kind == kind {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, kind))
}

fn find_all<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        find_all(child, kind, out);
    }
}

/// The transcript's rows as the list builds them: each row's node.
fn rows(view: &mut ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let list = find(view.tree(), "ui.VirtualList")
        .expect("a transcript")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| view.row(&list, i).unwrap().cloned())
        .collect()
}

/// Whether the transcript's "New messages" divider stands over each row. The list is told the
/// item the person stopped reading before (`unseen_from`, off `first_unseen`), and the toolkit
/// stands the divider over that item's row as it builds, so the view's tree names the item and
/// the row that presents it; a row of that unseen part carries no mark of its own.
fn dividers(view: &mut ViewInstance) -> Vec<bool> {
    view.poll_patches().unwrap();
    let tree = view.tree().clone();
    let list = list_nodes(&tree).into_iter().next().expect("a transcript");
    let Some(Value::Ref(unseen)) = list.props.get("unseen_from") else {
        return vec![false; rows(view).len()];
    };
    rows(view)
        .iter()
        .map(|row| row_shows_item(row, &unseen.target))
        .collect()
}

/// Every `ui.VirtualList` under `node`, outermost first.
fn list_nodes(node: &UiNode) -> Vec<&UiNode> {
    let mut out = Vec::new();
    if node.kind == "ui.VirtualList" {
        out.push(node);
    }
    for child in &node.children {
        out.extend(list_nodes(child));
    }
    out
}

/// Whether `row` presents `at`: its `ui.ListItem` names it (`item:`), which is the item the
/// list compares against its `unseen_from` to stand the divider over the row.
fn row_shows_item(row: &UiNode, at: &ResourceRef) -> bool {
    find(row, "ui.ListItem")
        .and_then(|item| item.props.get("item"))
        .is_some_and(|shown| match shown {
            Value::Ref(shown) => shown.target.id_string() == at.id_string(),
            _ => false,
        })
}

/// What each row of the transcript says: its rich text.
fn said(view: &mut ViewInstance) -> Vec<String> {
    rows(view)
        .iter()
        .map(|row| {
            let mut out = Vec::new();
            find_all(row, "ui.BlockEditor", &mut out);
            let mut text = Vec::new();
            for node in out {
                texts(node, &mut text);
            }
            text.join(" ")
        })
        .collect()
}

/// The key of the button in `node` labelled `label`.
fn button(node: &UiNode, label: &str) -> Option<String> {
    let mut all = Vec::new();
    find_all(node, "ui.Button", &mut all);
    all.iter()
        .find(|b| b.props.get("label") == Some(&Value::text(label)))
        .map(|b| b.key.clone())
}

/// The key of the menu item in `node` labelled `label`.
fn menu_item(node: &UiNode, label: &str) -> String {
    let mut all = Vec::new();
    find_all(node, "ui.MenuItem", &mut all);
    all.iter()
        .find(|item| item.props.get("label") == Some(&Value::text(label)))
        .map_or_else(
            || panic!("no menu item {label}: {:?}", all.len()),
            |item| item.key.clone(),
        )
}

/// Type `text` into the composer of `view` and press its send button, as a person does.
fn post(r: &mut Replica, view: &ViewInstance, text: &str) {
    let input = find(view.tree(), "ui.RichInput")
        .expect("the composer")
        .key
        .clone();
    view.edit(&input, &rich(text)).unwrap();
    let send = button(view.tree(), "Send").expect("a send button");
    let outcome = view.press(&send).unwrap();
    assert!(
        matches!(outcome, InvokeOutcome::Done(_)),
        "sent: {outcome:?}"
    );
    r.commit();
}

#[test]
fn two_people_post_live_in_one_channel_with_replies_and_a_thread() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let team = call(&ada, "new_category", vec![all, Value::text("Team")]);
    let design = call(&ada, "add_channel", vec![team, Value::text("design")]);
    ada.commit();
    let mut ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    // Phrases say their id and arguments, so a count shows in what a chip says.
    for r in [&ada, &ben] {
        r.host.set_words(EchoWords);
    }

    // The sidebar lists the category's channel under the category's heading.
    let sidebar = ada.view(APP);
    let mut shown = Vec::new();
    texts(sidebar.tree(), &mut shown);
    assert!(shown.contains(&"design".to_owned()), "{shown:?}");
    let section = find(sidebar.tree(), "ui.NavSection").expect("the category's section");
    assert_eq!(section.props.get("title"), Some(&Value::text("Team")));
    let mut under = Vec::new();
    texts(section, &mut under);
    assert!(under.contains(&"design".to_owned()), "{under:?}");

    let mut ada_design = ada.host.open_resource(&target(&design), None).unwrap();
    let mut ben_design = ben.host.open_resource(&target(&design), None).unwrap();

    // Each types in the composer and sends; once synced, both see both, in order.
    post(&mut ada, &ada_design, "Is the build on the test machines?");
    assert_eq!(
        said(&mut ada_design),
        ["Is the build on the test machines?"]
    );
    ada.sync_with(&mut ben);
    post(&mut ben, &ben_design, "Since this morning.");
    ada.sync_with(&mut ben);
    let want = ["Is the build on the test machines?", "Since this morning."];
    assert_eq!(said(&mut ada_design), want);
    assert_eq!(said(&mut ben_design), want);

    // The composer is each person's own: it is empty again once sent, and nothing Ben typed
    // reached Ada's.
    let input = find(ada_design.tree(), "ui.RichInput").unwrap().key.clone();
    assert!(matches!(ada_design.bound_value(&input), Some(Value::Rich(r)) if r.is_empty()));

    // Ben answers Ada's message inline: Reply on her row puts it over his composer, and his
    // message quotes it in the channel, for both.
    let first = rows(&mut ben_design).remove(0);
    let reply = button(&first, "Reply").expect("Reply on a message");
    ben_design.press(&reply).unwrap();
    ben_design.poll_patches().unwrap();
    let banner = find(ben_design.tree(), "ui.Quote").expect("the reply banner");
    assert_eq!(
        banner.props.get("excerpt"),
        Some(&Value::text("Is the build on the test machines?"))
    );
    post(&mut ben, &ben_design, "On all three.");
    ben_design.poll_patches().unwrap();
    assert!(
        find(ben_design.tree(), "ui.Quote").is_none(),
        "the banner goes once the reply is sent"
    );
    ada.sync_with(&mut ben);
    for view in [&mut ada_design, &mut ben_design] {
        let rows = rows(view);
        assert_eq!(rows.len(), 3);
        let quote = find(&rows[2], "ui.Quote").expect("the reply quotes what it answers");
        assert_eq!(
            quote.props.get("excerpt"),
            Some(&Value::text("Is the build on the test machines?"))
        );
        assert_eq!(
            quote.props.get("author"),
            Some(&Value::Principal(Principal::new("ada")))
        );
    }

    // Ada starts a thread off Ben's first message; Ben answers in it. The thread is a side
    // conversation: the channel keeps its three messages, and the message shows the thread's
    // reply count, for both.
    let messages = rows(&mut ada_design);
    let start = button(&messages[1], "Start thread").expect("Start thread on a message");
    ada_design.press(&start).unwrap();
    ada.commit();
    ada.sync_with(&mut ben);
    let row = rows(&mut ben_design).remove(1);
    let chip = find(&row, "ui.Chip").expect("the thread's chip");
    let opened = ben_design
        .event_action(&chip.key, "press")
        .expect("the chip opens the thread");
    let thread = opened.args[0].clone();
    let mut ben_thread = ben.host.open_resource(&target(&thread), None).unwrap();
    post(&mut ben, &ben_thread, "Shall I post the numbers here?");
    assert_eq!(said(&mut ben_thread), ["Shall I post the numbers here?"]);
    ada.sync_with(&mut ben);
    for view in [&mut ada_design, &mut ben_design] {
        assert_eq!(
            said(view).len(),
            3,
            "a thread's replies stay out of the channel"
        );
        let row = &rows(view)[1];
        let chip = find(row, "ui.Chip").expect("the thread's chip");
        let label = format!("{:?}", chip.props.get("label"));
        assert!(
            label.contains("tilolang-thread-replies") && label.contains(r#"\"1\""#),
            "the chip counts the reply: {label}"
        );
    }
}

/// Who reacted with each emoji, as the row's reactions show it.
fn reactions(row: &UiNode) -> Vec<(String, Vec<Principal>)> {
    let Some(node) = find(row, "ui.Reactions") else {
        return Vec::new();
    };
    let Some(Value::Map(entries)) = node.props.get("reactions") else {
        panic!("reactions as a map: {:?}", node.props);
    };
    entries
        .iter()
        .map(|(emoji, who)| {
            let Value::List(who) = who else {
                panic!("who reacted, as a set: {who:?}");
            };
            let who = who
                .iter()
                .map(|p| match p {
                    Value::Principal(p) => p.clone(),
                    other => panic!("a principal: {other:?}"),
                })
                .collect();
            (emoji.as_text().unwrap_or_default().to_owned(), who)
        })
        .collect()
}

#[test]
fn two_people_reacting_at_once_both_stay_and_a_second_press_takes_yours_back() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let m = call(
        &ada,
        "send",
        vec![design.clone(), rich("Shots are up."), Value::Opt(None)],
    );
    ada.commit();
    let mut ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    let mut ada_design = ada.host.open_resource(&target(&design), None).unwrap();
    let mut ben_design = ben.host.open_resource(&target(&design), None).unwrap();
    assert!(
        reactions(&rows(&mut ada_design)[0]).is_empty(),
        "no reactions yet"
    );

    // Both react with the same emoji before either hears of the other's.
    call(&ada, "react", vec![m.clone(), Value::text("👍")]);
    ada.commit();
    call(&ben, "react", vec![m.clone(), Value::text("👍")]);
    call(&ben, "react", vec![m.clone(), Value::text("🎉")]);
    ben.commit();
    ada.sync_with(&mut ben);
    let both = vec![
        (
            "👍".to_owned(),
            vec![Principal::new("ada"), Principal::new("ben")],
        ),
        ("🎉".to_owned(), vec![Principal::new("ben")]),
    ];
    for view in [&mut ada_design, &mut ben_design] {
        let mut shown = reactions(&rows(view)[0]);
        shown.sort();
        let mut want = both.clone();
        want.sort();
        assert_eq!(shown, want, "neither reaction is lost");
        let node = find(&rows(view)[0], "ui.Reactions").cloned().unwrap();
        assert!(node.events.contains_key("toggle"), "a pill toggles");
    }

    // Ada's second press takes hers back; Ben's stays.
    call(&ada, "react", vec![m, Value::text("👍")]);
    ada.commit();
    ada.sync_with(&mut ben);
    for view in [&mut ada_design, &mut ben_design] {
        let shown = reactions(&rows(view)[0]);
        assert!(
            shown.contains(&("👍".to_owned(), vec![Principal::new("ben")])),
            "{shown:?}"
        );
    }
}

/// Every text `row` shows.
fn shown_in(row: &UiNode) -> Vec<String> {
    let mut out = Vec::new();
    texts(row, &mut out);
    out
}

#[test]
fn enter_in_the_composer_sends_and_empties_it() {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let mut view = ada.host.open_resource(&target(&design), None).unwrap();
    let input = find(view.tree(), "ui.RichInput")
        .expect("the composer")
        .clone();
    assert!(
        input.events.contains_key("submit"),
        "the composer sends on Enter"
    );
    view.edit(&input.key, &rich("Is the build out?")).unwrap();
    let outcome = view.fire(&input.key, "submit").unwrap();
    assert!(
        matches!(outcome, InvokeOutcome::Done(_)),
        "sent: {outcome:?}"
    );
    ada.commit();
    assert_eq!(said(&mut view), ["Is the build out?"]);
    assert!(
        matches!(view.bound_value(&input.key), Some(Value::Rich(r)) if r.is_empty()),
        "the composer is empty again"
    );
}

#[test]
fn its_author_edits_a_message_in_place_and_anyone_pins_it() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let mut ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    let mut ada_design = ada.host.open_resource(&target(&design), None).unwrap();
    let mut ben_design = ben.host.open_resource(&target(&design), None).unwrap();
    post(&mut ada, &ada_design, "The bulid is on the test machines.");

    // Edit on Ada's message puts an editor holding what it says in its place; Save keeps the
    // new text, which both see, marked as edited.
    let row = rows(&mut ada_design).remove(0);
    let edit = button(&row, "Edit").expect("Edit on a message");
    ada_design.press(&edit).unwrap();
    let row = rows(&mut ada_design).remove(0);
    let editor = find(&row, "ui.RichInput")
        .expect("the editor in the message's place")
        .key
        .clone();
    assert!(
        matches!(ada_design.bound_value(&editor), Some(Value::Rich(r)) if r.written_text() == "The bulid is on the test machines."),
        "the editor starts from what the message says"
    );
    ada_design
        .edit(&editor, &rich("The build is on the test machines."))
        .unwrap();
    let save = button(&row, "Save").expect("Save beside the editor");
    let outcome = ada_design.press(&save).unwrap();
    assert!(
        matches!(outcome, InvokeOutcome::Done(_)),
        "saved: {outcome:?}"
    );
    ada.commit();
    ada.sync_with(&mut ben);
    for view in [&mut ada_design, &mut ben_design] {
        assert_eq!(said(view), ["The build is on the test machines."]);
        let row = rows(view).remove(0);
        assert!(find(&row, "ui.RichInput").is_none(), "the editor is gone");
        assert!(
            shown_in(&row).contains(&"edited".to_owned()),
            "marked as edited: {:?}",
            shown_in(&row)
        );
    }

    // Only its author edits a message: Ben has no Edit on Ada's, and editing it as Ben is
    // refused with the reason.
    let row = rows(&mut ben_design).remove(0);
    assert!(button(&row, "Edit").is_none(), "no Edit on someone else's");
    let message = ben_design
        .event_action(&button(&row, "Pin").expect("Pin"), "press")
        .expect("Pin runs on the message")
        .args[0]
        .clone();
    let meta = ben.host.package(APP).unwrap().meta;
    let edit_id = meta
        .actions
        .iter()
        .find(|a| a.name == "edit")
        .unwrap()
        .id
        .clone();
    let refused = ben.host.runtime().invoke_action(
        &Principal::new("ben"),
        (edit_id.as_str(), vec![message, rich("Not mine to say.")]),
    );
    assert!(
        !matches!(refused, Ok(InvokeOutcome::Done(_))),
        "Ben's edit of Ada's message is refused: {refused:?}"
    );
    assert_eq!(
        said(&mut ben_design),
        ["The build is on the test machines."]
    );

    // Ben pins it; both see it pinned.
    let row = rows(&mut ben_design).remove(0);
    let pin = button(&row, "Pin").expect("Pin on a message");
    ben_design.press(&pin).unwrap();
    ben.commit();
    ada.sync_with(&mut ben);
    for view in [&mut ada_design, &mut ben_design] {
        let row = rows(view).remove(0);
        assert!(
            shown_in(&row).contains(&"Pinned".to_owned()),
            "{:?}",
            shown_in(&row)
        );
        assert!(
            shown_in(&row).contains(&"edited".to_owned()),
            "a pin after the edit keeps it marked as edited"
        );
        let mut labels = Vec::new();
        find_all(&row, "ui.Button", &mut labels);
        let labels: Vec<_> = labels
            .iter()
            .map(|b| format!("{:?}", b.props.get("label")))
            .collect();
        assert!(
            button(&row, "Unpin").is_some(),
            "Pin turns into Unpin: {labels:?}"
        );
    }
}

/// Chat as it was before channels: conversations of messages made of parts.
const BEFORE: &str = include_str!("chat_before_channels/app.tl");
const BEFORE_IDS: &str = include_str!("chat_before_channels/app.ids");

#[test]
fn the_conversations_of_chat_before_channels_open_as_channels_saying_what_they_said() {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(BEFORE, BEFORE_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let plans = call(&ada, "start", vec![all, Value::text("Plans")]);
    ada.commit();
    call(
        &ada,
        "post",
        vec![
            plans.clone(),
            Value::text("Walk on Saturday?"),
            Value::text(""),
        ],
    );
    ada.commit();
    call(
        &ada,
        "post",
        vec![
            plans.clone(),
            Value::text("In minutes:"),
            Value::text("5 * 15"),
        ],
    );
    ada.commit();

    // The same workspace, opened by the Chat of channels.
    let (mut later, _) = Replica::new("ada", ada.doc.fork(), &[(CHAT, CHAT_IDS)]);
    later.host.sync_from(&later.doc);
    let sidebar = later.view(APP);
    let mut shown = Vec::new();
    texts(sidebar.tree(), &mut shown);
    assert!(
        shown.contains(&"Plans".to_owned()),
        "the conversation is a channel: {shown:?}"
    );

    let mut channel = later.host.open_resource(&target(&plans), None).unwrap();
    let rows = rows(&mut channel);
    assert_eq!(rows.len(), 2, "every message is still there");
    let mut first = Vec::new();
    texts(&rows[0], &mut first);
    assert!(first.contains(&"Walk on Saturday?".to_owned()), "{first:?}");
    let mut second = Vec::new();
    texts(&rows[1], &mut second);
    for part in ["In minutes:", "5 * 15"] {
        assert!(second.contains(&part.to_owned()), "{part} in {second:?}");
    }

    // And it takes new messages as any channel does.
    post(&mut later, &channel, "Count me in.");
    assert_eq!(
        said(&mut channel).last().map(String::as_str),
        Some("Count me in.")
    );
}

/// A message is deleted from its channel by one of the destructive actions offered on it: the
/// person who wrote it removes their own, and it is gone for everyone, its removal a version
/// the record keeps (so undo brings it back). The thread it started goes with it, and no
/// message of another channel is touched.
#[test]
fn a_deleted_message_is_gone_for_everyone_and_leaves_a_version_of_the_removal() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(
        &ada,
        "new_channel",
        vec![all.clone(), Value::text("design")],
    );
    let other = call(&ada, "new_channel", vec![all, Value::text("random")]);
    ada.commit();
    let kept = call(
        &ada,
        "send",
        vec![design.clone(), rich("This stays."), Value::none()],
    );
    let doomed = call(
        &ada,
        "send",
        vec![design.clone(), rich("This goes."), Value::none()],
    );
    call(&ada, "send", vec![other, rich("Not here."), Value::none()]);
    ada.commit();
    let mut ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);

    // A holder's action on a message is in the message's menu (`std.transcript` puts every
    // action offered on an item there), as a destructive one.
    let mut ada_design = ada.host.open_resource(&target(&design), None).unwrap();
    let rows = rows(&mut ada_design);
    let labels: Vec<_> = {
        let mut all = Vec::new();
        find_all(&rows[1], "ui.MenuItem", &mut all);
        all.iter()
            .map(|i| {
                (
                    i.props.get("label").cloned(),
                    i.props.get("destructive").cloned(),
                )
            })
            .collect()
    };
    assert!(
        labels
            .iter()
            .any(|(label, destructive)| label == &Some(Value::text("Delete"))
                && destructive == &Some(Value::Int(1))),
        "Delete is a destructive item of the message's menu: {labels:?}"
    );
    let offered = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .actions
        .iter()
        .any(|a| a.name == "delete" && a.role.as_deref() == Some("destructive"));
    assert!(offered, "delete is an action of the app, destructive");
    let delete = ada_design
        .event_action(&menu_item(&rows[1], "Delete"), "choose")
        .expect("Delete runs on the message");
    let outcome = ada.host.runtime().invoke_action(
        &Principal::new("ada"),
        (delete.action.as_str(), delete.args.clone()),
    );
    assert!(
        matches!(outcome, Ok(InvokeOutcome::Done(_))),
        "deleted: {outcome:?}"
    );
    ada.commit();

    // Gone here, and for the other person once they sync; the message beside it stays.
    assert_eq!(said(&mut ada_design), ["This stays."]);
    ben.commit();
    ben.host.sync_from(&ada.doc);
    let mut ben_design = ben.host.open_resource(&target(&design), None).unwrap();
    assert_eq!(said(&mut ben_design), ["This stays."]);
    assert_eq!(
        ada.host
            .store()
            .versions(&target(&doomed))
            .unwrap()
            .last()
            .map(|v| v.kind),
        Some(VersionKind::Removed),
        "the removal is a version of the message"
    );
    assert!(
        !ada.host
            .store()
            .versions(&target(&kept))
            .unwrap()
            .is_empty(),
        "the kept message keeps its history"
    );
}

/// What someone else says in a channel is unseen to you until you open it: the sidebar's place
/// for the channel carries how many of its messages you have not seen and stands out, the
/// transcript stands its "New messages" divider where the unseen part starts, and opening the
/// channel takes the divider away. A message of your own is never unseen to you, and nothing
/// outside the channel is counted.
#[test]
fn a_channels_new_messages_are_unseen_in_the_sidebar_until_the_person_opens_it() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(
        &ada,
        "new_channel",
        vec![all.clone(), Value::text("design")],
    );
    let random = call(&ada, "new_channel", vec![all, Value::text("random")]);
    ada.commit();
    let mut ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    call(
        &ben,
        "send",
        vec![design.clone(), rich("The build is up."), Value::none()],
    );
    call(
        &ben,
        "send",
        vec![design.clone(), rich("On all three."), Value::none()],
    );
    call(
        &ben,
        "send",
        vec![random, rich("Coffee is fixed."), Value::none()],
    );
    ben.commit();
    ben.host.sync_from(&ben.doc);
    ada.sync_with(&mut ben);

    // Ada's sidebar says two are new in design, one in random, and the places stand out.
    let sidebar = ada.view(APP);
    let place = |key: &str| {
        let mut all = Vec::new();
        find_all(sidebar.tree(), "ui.NavItem", &mut all);
        all.into_iter()
            .find(|item| item.props.get("key") == Some(&Value::text(key)))
            .unwrap_or_else(|| panic!("no place {key}"))
    };
    assert_eq!(
        place("/design").props.get("live"),
        Some(&Value::Int(2)),
        "two new in design"
    );
    assert_eq!(
        place("/design").props.get("unread"),
        Some(&Value::Bool(true))
    );
    assert_eq!(place("/random").props.get("live"), Some(&Value::Int(1)));

    // Ben's messages are new, and the transcript says so once, with its divider where their
    // part starts: a row of that part carries no mark of its own.
    let mut channel = ada.host.open_resource(&target(&design), None).unwrap();
    let marked = |view: &mut ViewInstance| -> Vec<bool> {
        rows(view)
            .iter()
            .map(|row| find(row, "ui.ChangeMark").is_some())
            .collect()
    };
    assert_eq!(marked(&mut channel), [false, false]);
    assert_eq!(
        dividers(&mut channel),
        [true, false],
        "the divider stands over the first message Ada had not seen"
    );
    post(&mut ada, &channel, "Thanks.");
    assert_eq!(marked(&mut channel), [false, false, false]);

    // Opening the channel marks what it showed seen, as the window does on the way out: the
    // divider goes, and so does the badge; random's is its own.
    ada.host
        .runtime()
        .mark_seen("ada", &target(&design))
        .unwrap();
    ada.commit();
    let mut reopened = ada.host.open_resource(&target(&design), None).unwrap();
    assert_eq!(
        dividers(&mut reopened),
        [false, false, false],
        "opening the channel saw it"
    );
    let sidebar = ada.view(APP);
    let mut all = Vec::new();
    find_all(sidebar.tree(), "ui.NavItem", &mut all);
    let place = |all: &mut Vec<&UiNode>, key: &str| {
        all.iter()
            .find(|item| item.props.get("key") == Some(&Value::text(key)))
            .map(|item| {
                (
                    item.props.get("live").cloned(),
                    item.props.get("unread").cloned(),
                )
            })
            .expect("the place")
    };
    assert_ne!(
        place(&mut all, "/design").0,
        Some(Value::Int(2)),
        "design's badge cleared"
    );
    assert_ne!(place(&mut all, "/design").1, Some(Value::Bool(true)));
    assert_eq!(
        place(&mut all, "/random").0,
        Some(Value::Int(1)),
        "random's is its own"
    );
}

/// Every text the window of the Chat before channels shows, in order: that Chat's messages were
/// plain text parts, not rich text, so its rows carry them among the row's other texts.
fn sent(view: &mut ViewInstance) -> Vec<String> {
    view.poll_patches().unwrap();
    let mut out = Vec::new();
    texts(view.tree(), &mut out);
    for row in rows(view) {
        let mut row_texts = Vec::new();
        texts(&row, &mut row_texts);
        out.extend(row_texts);
    }
    out
}

/// Whether one of the window's texts has `said` in it.
fn says(view: &mut ViewInstance, said: &str) -> bool {
    sent(view).iter().any(|text| text.contains(said))
}

/// Two packages loaded here at once, of the two versions of Chat this repository keeps: the
/// current one and the one before channels, as a workspace upgraded from the old build has
/// them — and as every test of this suite has them, since it loads both in one process. A
/// record of one version is read and changed by its own types, never by the other's; the type
/// `Message` both declare is the same id with different fields, which is what takes the wrong
/// schema.
#[test]
fn a_room_held_by_one_version_of_chat_is_read_and_changed_by_that_version_with_the_other_loaded() {
    // The current Chat loaded first, then the Chat before channels: each a package with its
    // own ids lock, as an upgraded workspace has them.
    let (mut current, _) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let channels = Value::resource(&current.host.root_of(APP).unwrap());
    current.commit();
    let channel = call(
        &current,
        "new_channel",
        vec![channels, Value::text("design")],
    );
    current.commit();
    call(
        &current,
        "send",
        vec![channel.clone(), rich("Shipped."), Value::Opt(None)],
    );
    current.commit();
    let mut seen = current.host.open_resource(&target(&channel), None).unwrap();
    assert_eq!(said(&mut seen), ["Shipped."]);

    // The other version of the app, over the same app id, in this process: the Chat before
    // channels, which shares the ids of `Message` and its pieces.
    let (mut older, _) = Replica::new("ada", AutoCommit::new(), &[(BEFORE, BEFORE_IDS)]);
    let conversations = Value::resource(&older.host.root_of(APP).unwrap());
    older.commit();
    let plans = call(&older, "start", vec![conversations, Value::text("Plans")]);
    older.commit();
    call(
        &older,
        "post",
        vec![
            plans.clone(),
            Value::text("Walk on Saturday?"),
            Value::text(""),
        ],
    );
    older.commit();
    let mut thread = older.host.open_resource(&target(&plans), None).unwrap();
    assert!(
        says(&mut thread, "Walk on Saturday?"),
        "{:?}",
        sent(&mut thread)
    );

    // The current Chat's channel still reads its message, and still takes one written by the
    // current version's action: the older version's schema, loaded second, never takes over how
    // the current version's records read.
    let mut seen_again = current.host.open_resource(&target(&channel), None).unwrap();
    assert_eq!(
        said(&mut seen_again),
        ["Shipped."],
        "the channel still reads its message with the older Chat loaded too"
    );
    post(&mut current, &seen_again, "Yes.");
    assert_eq!(said(&mut seen_again), ["Shipped.", "Yes."]);

    // The older version's conversation reads and takes a message by that version's types, with
    // the current Chat loaded first: its own `send` writes the plain-text `Part` its schema has.
    let conversation = Value::resource(&target(&plans));
    call(
        &older,
        "send",
        vec![conversation, Value::text("Count me in.")],
    );
    older.commit();
    thread = older.host.open_resource(&target(&plans), None).unwrap();
    assert!(says(&mut thread, "Count me in."), "{:?}", sent(&mut thread));
    assert!(
        says(&mut thread, "Walk on Saturday?"),
        "{:?}",
        sent(&mut thread)
    );
}

/// A conversation's first unread message is where its "New messages" divider stands: the
/// transcript reads `first_unseen(c)`, a fact of the channel's messages and the person's seen
/// state, and the list places the divider over that message's row. Once the person reads to the
/// end nothing is marked, and a message that arrives afterwards is marked alone.
#[test]
fn the_transcript_marks_the_first_message_the_person_had_not_seen() {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let mut ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    call(
        &ben,
        "send",
        vec![design.clone(), rich("One."), Value::none()],
    );
    call(
        &ben,
        "send",
        vec![design.clone(), rich("Two."), Value::none()],
    );
    ben.commit();
    ben.host.sync_from(&ben.doc);
    ada.sync_with(&mut ben);

    // The rows the list shows, each with whether the "New messages" divider stands over it: the
    // list is told the item reading stopped before (`unseen_from`) and draws its divider over
    // that item's row.
    let marked = dividers;

    // Ada's view stands the divider over the first of the two, and not over the second.
    let mut view = ada.host.open_resource(&target(&design), None).unwrap();
    assert_eq!(marked(&mut view), [true, false], "over the first");

    // She reads: opening the channel marks what it showed seen, and nothing is left unread.
    ada.host
        .runtime()
        .mark_seen("ada", &target(&design))
        .unwrap();
    ada.commit();
    let mut reopened = ada.host.open_resource(&target(&design), None).unwrap();
    assert_eq!(marked(&mut reopened), [false, false], "nothing unseen");

    // A message from Ben afterwards is what is new, alone.
    ben.host.sync_from(&ada.doc);
    call(&ben, "send", vec![design, rich("Any time."), Value::none()]);
    ben.commit();
    ben.host.sync_from(&ben.doc);
    ada.sync_with(&mut ben);
    assert_eq!(
        marked(&mut reopened),
        [false, false, true],
        "his answer is what is new"
    );
}

/// A direct message is a conversation in the same model as a channel: it stands in the sidebar
/// with everyone the workspace has, pressing a person opens the conversation with them (the
/// one already open, not a new empty one), and it takes messages as a channel does.
#[test]
fn a_direct_message_opens_with_a_person_and_takes_their_messages() {
    // A workspace with Ben in it: everyone the workspace has, from its own membership.
    let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace-dm", "ada");
    policy
        .members
        .insert("ben".to_owned(), tilo_action_policy::Role::Editor);
    let (mut ada, loaded) = Replica::standing(
        "ada",
        AutoCommit::new(),
        &[(CHAT, CHAT_IDS)],
        Some(tilo_action_policy::Standing::person(policy, "ada")),
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let mut ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.sync_from(&ben.doc);
    // The workspace's membership, as the host hands it to the runtime.
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", "owner"),
            tilolang_runtime::members::Member::person("ben", "editor"),
        ],
        None,
    );

    // The sidebar offers Ben under Direct messages: everyone in the workspace, from its
    // membership rather than a copy Chat keeps.
    let sidebar = ada.view(APP);
    let section = {
        let mut seen = Vec::new();
        find_all(sidebar.tree(), "ui.NavSection", &mut seen);
        seen.into_iter()
            .find(|s| s.props.get("title") == Some(&Value::text("Direct messages")))
            .expect("the direct messages section")
            .clone()
    };
    let mut people = Vec::new();
    texts(&section, &mut people);
    assert!(people.contains(&"ben".to_owned()), "{people:?}");

    // Pressing them opens the conversation with Ben, once: a person has one.
    let person = {
        let mut items = Vec::new();
        find_all(&section, "ui.NavItem", &mut items);
        items
            .into_iter()
            .find(|item| item.props.get("key") == Some(&Value::text("/person/ben")))
            .expect("Ben's place")
            .clone()
    };
    // The place's event runs the action the app declared for it.
    assert!(
        sidebar.event_action(&person.key, "select").is_some(),
        "the place opens a conversation"
    );
    assert!(
        ada.host
            .package(APP)
            .unwrap()
            .meta
            .actions
            .iter()
            .any(|a| a.name == "message_person"),
        "the action is declared"
    );
    let dm = call(
        &ada,
        "message_person",
        vec![all.clone(), Value::Principal(Principal::new("ben"))],
    );
    let again = call(
        &ada,
        "message_person",
        vec![all, Value::Principal(Principal::new("ben"))],
    );
    assert_eq!(
        target(&dm).id_string(),
        target(&again).id_string(),
        "a person has one conversation"
    );
    // It takes a message from Ben, which Ada reads as any conversation's: an item created by
    // `insert` is held in its holder's record, and the store's index of records learns of it
    // on the next scan, as a `commit` and a sync does.
    // Ada's own sync first: the workspace's host scans the records, so the new conversation is
    // one the store can find.
    ada.commit();
    ben.host.sync_from(&ada.doc);
    ben.commit();
    ben.host.sync_from(&ben.doc);
    ada.sync_with(&mut ben);
    let _ = ada.host.wait_index();
    call(
        &ben,
        "send",
        vec![dm.clone(), rich("Still on for Saturday?"), Value::none()],
    );
    ben.commit();
    ben.host.sync_from(&ben.doc);
    ada.sync_with(&mut ben);
    let mut conversation = ada.host.open_resource(&target(&dm), None).unwrap();
    assert_eq!(said(&mut conversation), ["Still on for Saturday?"]);
}

/// A conversation says who is typing in it from the system's presence: the lenses on the
/// channel a person has open carry whether that person is composing a message there, never
/// what it says. The line goes when they stop, and a lens that is only open (not composing)
/// says nothing.
#[test]
fn a_conversation_says_who_is_typing_from_presence() {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    ada.host.sync_from(&ada.doc);
    let mut view = ada.host.open_resource(&target(&design), None).unwrap();
    let channel = target(&design).id_string();

    // Nobody at the channel but Ada: no one is typing.
    let typing = |view: &mut ViewInstance| -> Vec<String> {
        view.poll_patches().unwrap();
        let mut found = Vec::new();
        texts(view.tree(), &mut found);
        found
            .into_iter()
            .filter(|text| text.contains("is typing") || text.contains("are typing"))
            .collect()
    };
    assert!(typing(&mut view).is_empty(), "no one typing yet");

    // Ben has the channel open and is composing: the conversation says so, in his name. The
    // runtime's lenses are what `lenses(c)` reads; a view that read them runs again.
    let ben = Principal::new("ben");
    ada.host
        .runtime()
        .set_lenses(&channel, &[(ben.clone(), false, true)]);

    assert_eq!(
        typing(&mut view),
        ["ben is typing…"],
        "presence says Ben is composing"
    );

    // He stops: the line goes. Open but not composing says nothing either.
    ada.host
        .runtime()
        .set_lenses(&channel, &[(ben, false, false)]);
    assert!(typing(&mut view).is_empty(), "he stopped");
}

/// With no channels yet, the workspace offers one way to make the first: the empty state's
/// "New channel" button. The window's bar names the same action, and steps aside for the call
/// to action the page draws itself (the host's header rules), so the person is offered one
/// button; once a channel exists the empty state is gone and the bar keeps its primary.
#[test]
fn an_empty_workspace_offers_one_new_channel_button() {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(CHAT, CHAT_IDS)]);
    ada.commit();

    let labelled = |view: &ViewInstance, label: &str| {
        let mut buttons = Vec::new();
        find_all(view.tree(), "ui.Button", &mut buttons);
        buttons
            .iter()
            .filter(|b| b.props.get("label") == Some(&Value::text(label)))
            .count()
    };
    let sidebar = ada.view(APP);
    let empty = find(sidebar.tree(), "ui.EmptyState").expect("the empty state");
    assert_eq!(
        empty.props.get("action"),
        Some(&Value::text("New channel")),
        "the empty state is where the first channel is made"
    );
    // The bar's primary comes from the declaration the view no longer reads back; the empty
    // state's action is the page's own. The host drops the bar's while both would stand
    // (`tilo_ui_host::shell::header_rules`, its own test), leaving one button on the page.
    assert!(
        labelled(&sidebar, "New channel") >= 1,
        "the action is offered: the empty state's, and the bar's until the host composes it"
    );

    // A channel made, the sidebar's "no channels" empty state goes and the bar is the place
    // to make another.
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    let _ = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let sidebar = ada.view(APP);
    assert!(
        find(sidebar.tree(), "ui.EmptyState")
            .is_none_or(|state| state.props.get("action") != Some(&Value::text("New channel"))),
        "no empty state offering New channel once a channel exists"
    );
    assert_eq!(
        labelled(&sidebar, "New channel"),
        1,
        "the bar keeps its primary once the empty state is gone"
    );
}

/// A place that stands for a person reads as them: the sidebar's direct messages carry the
/// person as `person`, so the list draws their avatar and presence, and the conversation with
/// yourself says so rather than showing your own name. What a view reads of the viewer
/// (`workspace.me`) is the person the window is open for, each replica its own.
#[test]
fn a_places_person_is_a_row_of_them_and_your_own_conversation_is_your_notes() {
    let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace-dm", "ada");
    policy
        .members
        .insert("ben".to_owned(), tilo_action_policy::Role::Editor);
    let (mut ada, _loaded) = Replica::standing(
        "ada",
        AutoCommit::new(),
        &[(CHAT, CHAT_IDS)],
        Some(tilo_action_policy::Standing::person(policy, "ada")),
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", "owner"),
            tilolang_runtime::members::Member::person("ben", "editor"),
        ],
        None,
    );

    // Everyone else in the workspace is a place that stands for them.
    let sidebar = ada.view(APP);
    let section = {
        let mut seen = Vec::new();
        find_all(sidebar.tree(), "ui.NavSection", &mut seen);
        seen.into_iter()
            .find(|s| s.props.get("title") == Some(&Value::text("Direct messages")))
            .expect("the direct messages section")
            .clone()
    };
    let mut places = Vec::new();
    find_all(&section, "ui.NavItem", &mut places);
    let ben = places
        .iter()
        .find(|item| item.props.get("key") == Some(&Value::text("/person/ben")))
        .copied()
        .expect("Ben's place");
    assert_eq!(
        ben.props.get("person"),
        Some(&Value::Principal(Principal::new("ben"))),
        "the place stands for Ben"
    );
    assert_eq!(
        ben.props.get("icon"),
        None,
        "a person's place shows their avatar, in the icon's place"
    );
    assert!(
        places
            .iter()
            .all(|item| item.props.get("key") != Some(&Value::text("/person/ada"))),
        "your own place is your notes, not a message to yourself"
    );

    // The conversation with yourself is your notes, and says so.
    let notes = call(
        &ada,
        "message_person",
        vec![all, Value::Principal(Principal::new("ada"))],
    );
    ada.commit();
    let sidebar = ada.view(APP);
    let notes_place = {
        let mut items = Vec::new();
        find_all(sidebar.tree(), "ui.NavItem", &mut items);
        items
            .iter()
            .find(|item| item.props.get("label") == Some(&Value::text("Notes to self")))
            .copied()
            .expect("the notes to self place")
    };
    assert!(
        matches!(
            notes_place.props.get("item"),
            Some(Value::Ref(reference)) if reference.target.id_string() == target(&notes).id_string()
        ),
        "the place opens your notes, which is that conversation: {:?}",
        notes_place.props.get("item")
    );
    assert_eq!(
        notes_place.props.get("person"),
        None,
        "your own place shows the notes glyph, not your avatar"
    );
}

/// Words that say a phrase's id and arguments, so a count shows in what a chip says; a
/// declaration's words stay the source's.
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
