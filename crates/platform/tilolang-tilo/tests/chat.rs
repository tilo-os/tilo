//! Chat: two people posting live in one channel of one workspace, each seeing the other's
//! messages, inline replies quoting what they answer, and threads branched off a message with
//! their replies counted under it. A workspace holding the conversations of the Chat before
//! channels opens with each conversation a channel and every message still saying what it said.

use automerge::AutoCommit;
use tilolang_runtime::rich::RichValue;
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

/// What each row of the transcript says: its rich text.
fn said(view: &mut ViewInstance) -> Vec<String> {
    rows(view)
        .iter()
        .map(|row| {
            let mut out = Vec::new();
            find_all(row, "ui.RichText", &mut out);
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
        r.host.set_phrases(|id, args| format!("{id} {args:?}"));
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
    // new text, which both see.
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

    // The same room, opened by the Chat of channels.
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
