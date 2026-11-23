//! Chat: two people posting live in one channel of one workspace, each seeing the other's
//! messages, inline replies quoting what they answer, and threads branched off a message with
//! their replies counted under it. A workspace holding the conversations of the Chat before
//! channels opens with each conversation a channel and every message still saying what it said.

use crate::fixtures::Workspace;
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::store::{Store as _, VersionKind};
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value, ViewInstance};

use crate::fixtures::{Replica, target};

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
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(&r.name)),
            (id.as_str(), args),
        )
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

fn rich(text: &str) -> Value {
    Value::Rich(RichValue::from_text(text))
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

/// The transcript's rows as the list builds them, each with its menu: each row's node.
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
        .filter_map(|i| {
            // The menu of each row, asked for as the window asks when the person opens it.
            let key = view.row_key(&list, i);
            view.menu_row(&list, key.as_deref());
            view.poll_patches().unwrap();
            view.row(&list, i).unwrap().cloned()
        })
        .collect()
}

/// The item the transcript is told the person's unread part starts at (`unseen_from`): the list
/// draws its "New messages" divider over the row that shows it. `None` when nothing is unseen.
fn unseen_from(view: &mut ViewInstance) -> Option<String> {
    rows(view);
    let list = view
        .tree()
        .first_of_kind("ui.VirtualList")
        .expect("a transcript");
    match list.props.get("unseen_from") {
        Some(Value::Ref(unseen)) => unseen.target.item.clone(),
        _ => None,
    }
}

/// The item a resource value names.
fn item_of(value: &Value) -> Option<String> {
    match value {
        Value::Ref(r) => r.target.item.clone(),
        _ => None,
    }
}

/// What each row of the transcript says: its rich text.
fn said(view: &mut ViewInstance) -> Vec<String> {
    rows(view)
        .iter()
        .map(|row| {
            let text: Vec<String> = row
                .of_kind("ui.BlockEditor")
                .into_iter()
                .flat_map(texts)
                .collect();
            text.join(" ")
        })
        .collect()
}

/// The key of the button in `node` labelled `label`.
fn button(node: &UiNode, label: &str) -> Option<String> {
    let all = node.of_kind("ui.Button");
    all.iter()
        .find(|b| b.props.get("label") == Some(&Value::text(label)))
        .map(|b| b.key.clone())
}

/// The key of the menu item in `node` labelled `label`.
fn menu_item(node: &UiNode, label: &str) -> String {
    let all = node.of_kind("ui.MenuItem");
    all.iter()
        .find(|item| item.props.get("label") == Some(&Value::text(label)))
        .map_or_else(
            || panic!("no menu item {label}: {:?}", all.len()),
            |item| item.key.clone(),
        )
}

/// Type `text` into the composer of `view` and press its send button, as a person does.
fn post(r: &Replica, view: &ViewInstance, text: &str) {
    let input = view
        .tree()
        .first_of_kind("ui.RichInput")
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
#[allow(
    clippy::too_many_lines,
    reason = "one conversation: posts, a reply and a thread, on both devices"
)]
fn two_people_post_live_in_one_channel_with_replies_and_a_thread() {
    let (ada, loaded) = Replica::new(
        "ada",
        Workspace::new(),
        &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let team = call(&ada, "new_category", vec![all, Value::text("Team")]);
    let design = call(&ada, "add_channel", vec![team, Value::text("design")]);
    ada.commit();
    let ben = ada.fork("ben", &[crate::fixtures::AGENT, (CHAT, &loaded[1].ids)]);
    // Phrases say their id and arguments, so a count shows in what a chip says.
    for r in [&ada, &ben] {
        r.host.set_words(EchoWords);
    }

    // The sidebar lists the category's channel under the category's heading.
    let sidebar = ada.view(APP);
    let shown = texts(sidebar.tree());
    assert!(shown.contains(&"design".to_owned()), "{shown:?}");
    let section = sidebar.tree().first_of_kind("ui.NavSection").unwrap();
    assert_eq!(section.props.get("title"), Some(&Value::text("Team")));
    let under = texts(section);
    assert!(under.contains(&"design".to_owned()), "{under:?}");

    let mut ada_design = ada.host.open_resource(&target(&design), None).unwrap();
    let mut ben_design = ben.host.open_resource(&target(&design), None).unwrap();

    // Each types in the composer and sends; once synced, both see both, in order.
    post(&ada, &ada_design, "Is the build on the test machines?");
    assert_eq!(
        said(&mut ada_design),
        ["Is the build on the test machines?"]
    );
    ada.sync_with(&ben);
    post(&ben, &ben_design, "Since this morning.");
    ada.sync_with(&ben);
    let want = ["Is the build on the test machines?", "Since this morning."];
    assert_eq!(said(&mut ada_design), want);
    assert_eq!(said(&mut ben_design), want);

    // The composer is each person's own: it is empty again once sent, and nothing Ben typed
    // reached Ada's.
    let input = ada_design.tree().first_of_kind("ui.RichInput").unwrap();
    assert!(
        matches!(ada_design.bound_value(&input.key.clone()), Some(Value::Rich(r)) if r.is_empty())
    );

    // Ben answers Ada's message inline: Reply on her row puts it over his composer, and his
    // message quotes it in the channel, for both.
    let first = rows(&mut ben_design).remove(0);
    let reply = button(&first, "Reply").expect("Reply on a message");
    ben_design.press(&reply).unwrap();
    ben_design.poll_patches().unwrap();
    let banner = ben_design.tree().first_of_kind("ui.Quote").unwrap();
    assert_eq!(
        banner.props.get("excerpt"),
        Some(&Value::text("Is the build on the test machines?"))
    );
    post(&ben, &ben_design, "On all three.");
    ben_design.poll_patches().unwrap();
    assert!(
        ben_design.tree().first_of_kind("ui.Quote").is_none(),
        "the banner goes once the reply is sent"
    );
    ada.sync_with(&ben);
    for view in [&mut ada_design, &mut ben_design] {
        let rows = rows(view);
        assert_eq!(rows.len(), 3);
        let quote = rows[2]
            .first_of_kind("ui.Quote")
            .expect("the reply quotes what it answers");
        assert_eq!(
            quote.props.get("excerpt"),
            Some(&Value::text("Is the build on the test machines?"))
        );
        assert_eq!(
            quote.props.get("author"),
            Some(&Value::Actor(Actor::from(
                tilolang_tilo::runtime::ids::MemberId::person("ada")
            )))
        );
    }

    // Ada starts a thread off Ben's first message; Ben answers in it. The thread is a side
    // conversation: the channel keeps its three messages, and the message shows the thread's
    // reply count, for both.
    let messages = rows(&mut ada_design);
    let start = button(&messages[1], "Start thread").expect("Start thread on a message");
    ada_design.press(&start).unwrap();
    ada.commit();
    ada.sync_with(&ben);
    let row = rows(&mut ben_design).remove(1);
    let chip = row.first_of_kind("ui.Chip").expect("the thread's chip");
    let opened = ben_design
        .event_action(&chip.key, "press")
        .expect("the chip opens the thread");
    let thread = opened.args[0].clone();
    let mut ben_thread = ben.host.open_resource(&target(&thread), None).unwrap();
    post(&ben, &ben_thread, "Shall I post the numbers here?");
    assert_eq!(said(&mut ben_thread), ["Shall I post the numbers here?"]);
    ada.sync_with(&ben);
    for view in [&mut ada_design, &mut ben_design] {
        assert_eq!(
            said(view).len(),
            3,
            "a thread's replies stay out of the channel"
        );
        let row = &rows(view)[1];
        let chip = row.first_of_kind("ui.Chip").expect("the thread's chip");
        let label = format!("{:?}", chip.props.get("label"));
        assert!(
            label.contains("tilolang-thread-replies") && label.contains(r#"\"1\""#),
            "the chip counts the reply: {label}"
        );
    }
}

/// Who reacted with each emoji, as the row's reactions show it.
fn reactions(row: &UiNode) -> Vec<(String, Vec<Actor>)> {
    let Some(node) = row.first_of_kind("ui.Reactions") else {
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
                    Value::Actor(p) => p.clone(),
                    other => panic!("an actor: {other:?}"),
                })
                .collect();
            (emoji.as_text().unwrap_or_default().to_owned(), who)
        })
        .collect()
}

#[test]
fn two_people_reacting_at_once_both_stay_and_a_second_press_takes_yours_back() {
    let (ada, loaded) = Replica::new(
        "ada",
        Workspace::new(),
        &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
    );
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
    let ben = ada.fork("ben", &[crate::fixtures::AGENT, (CHAT, &loaded[1].ids)]);
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
    ada.sync_with(&ben);
    let both = vec![
        (
            "👍".to_owned(),
            vec![
                Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben")),
            ],
        ),
        (
            "🎉".to_owned(),
            vec![Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
                "ben",
            ))],
        ),
    ];
    for view in [&mut ada_design, &mut ben_design] {
        let mut shown = reactions(&rows(view)[0]);
        shown.sort();
        let mut want = both.clone();
        want.sort();
        assert_eq!(shown, want, "neither reaction is lost");
        let node = (rows(view)[0])
            .first_of_kind("ui.Reactions")
            .cloned()
            .unwrap();
        assert!(node.events.contains_key("toggle"), "a pill toggles");
    }

    // Ada's second press takes hers back; Ben's stays.
    call(&ada, "react", vec![m, Value::text("👍")]);
    ada.commit();
    ada.sync_with(&ben);
    for view in [&mut ada_design, &mut ben_design] {
        let shown = reactions(&rows(view)[0]);
        assert!(
            shown.contains(&(
                "👍".to_owned(),
                vec![Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
                    "ben"
                ))]
            )),
            "{shown:?}"
        );
    }
}

tilo_test_env::scenario! {
    fn a_message_is_sent_edited_and_deleted;

    phase enter_in_the_composer_sends_and_empties_it;
    {
        let (ada, _) = Replica::new("ada", Workspace::new(), &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let mut view = ada.host.open_resource(&target(&design), None).unwrap();
        let input = view.tree().first_of_kind("ui.RichInput")
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

    phase its_author_edits_a_message_in_place_and_anyone_pins_it;
    {
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let ben = ada.fork("ben", &[crate::fixtures::AGENT, (CHAT, &loaded[1].ids)]);
        let mut ada_design = ada.host.open_resource(&target(&design), None).unwrap();
        let mut ben_design = ben.host.open_resource(&target(&design), None).unwrap();
        post(&ada, &ada_design, "The bulid is on the test machines.");

        // Edit on Ada's message puts an editor holding what it says in its place; Save keeps the
        // new text, which both see, marked as edited.
        let row = rows(&mut ada_design).remove(0);
        let edit = button(&row, "Edit").expect("Edit on a message");
        ada_design.press(&edit).unwrap();
        let row = rows(&mut ada_design).remove(0);
        let editor = row.first_of_kind("ui.RichInput")
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
        ada.sync_with(&ben);
        for view in [&mut ada_design, &mut ben_design] {
            assert_eq!(said(view), ["The build is on the test machines."]);
            let row = rows(view).remove(0);
            assert!(row.first_of_kind("ui.RichInput").is_none(), "the editor is gone");
            assert!(
                texts(&row).contains(&"edited".to_owned()),
                "marked as edited: {:?}",
                texts(&row)
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
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben")),
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
        ada.sync_with(&ben);
        for view in [&mut ada_design, &mut ben_design] {
            let row = rows(view).remove(0);
            assert!(
                texts(&row).contains(&"Pinned".to_owned()),
                "{:?}",
                texts(&row)
            );
            assert!(
                texts(&row).contains(&"edited".to_owned()),
                "a pin after the edit keeps it marked as edited"
            );
            let labels = row.of_kind("ui.Button");
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

    phase a_deleted_message_is_gone_for_everyone_and_leaves_a_version_of_the_removal;
    // A message is deleted from its channel by one of the destructive actions offered on it: the
    // person who wrote it removes their own, and it is gone for everyone, its removal a version
    // the document keeps (so undo brings it back). The thread it started goes with it, and no
    // message of another channel is touched.
    {
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)]);
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
        let ben = ada.fork("ben", &[crate::fixtures::AGENT, (CHAT, &loaded[1].ids)]);

        // A holder's action on a message is in the message's menu (`std.transcript` puts every
        // action offered on an item there), as a destructive one.
        let mut ada_design = ada.host.open_resource(&target(&design), None).unwrap();
        let rows = rows(&mut ada_design);
        let labels: Vec<_> = {
            let all = rows[1].of_kind("ui.MenuItem");
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
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
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
        ada.sync_with(&ben);
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
}

/// Chat as it was before channels: conversations of messages made of parts.
const BEFORE: &str = include_str!("chat_before_channels/app.tl");
const BEFORE_IDS: &str = include_str!("chat_before_channels/app.ids");

#[test]
fn the_conversations_of_chat_before_channels_open_as_channels_saying_what_they_said() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(BEFORE, BEFORE_IDS)]);
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
    let (later, _) = Replica::new(
        "ada",
        ada.doc.fork(),
        &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
    );
    let sidebar = later.view(APP);
    let shown = texts(sidebar.tree());
    assert!(
        shown.contains(&"Plans".to_owned()),
        "the conversation is a channel: {shown:?}"
    );

    let mut channel = later.host.open_resource(&target(&plans), None).unwrap();
    let rows = rows(&mut channel);
    assert_eq!(rows.len(), 2, "every message is still there");
    let first = texts(&rows[0]);
    assert!(first.contains(&"Walk on Saturday?".to_owned()), "{first:?}");
    let second = texts(&rows[1]);
    for part in ["In minutes:", "5 * 15"] {
        assert!(second.contains(&part.to_owned()), "{part} in {second:?}");
    }

    // And it takes new messages as any channel does.
    post(&later, &channel, "Count me in.");
    assert_eq!(
        said(&mut channel).last().map(String::as_str),
        Some("Count me in.")
    );
}

tilo_test_env::scenario! {
    fn what_a_person_has_not_seen_is_marked_and_counted;

    phase a_channels_new_messages_are_unseen_in_the_sidebar_until_the_person_opens_it;
    // What someone else says in a channel is unseen to you until you open it: the sidebar's place
    // for the channel carries how many of its messages you have not seen and stands out, the
    // transcript stands its "New messages" divider where the unseen part starts, and opening the
    // channel takes the divider away. A message of your own is never unseen to you, and nothing
    // outside the channel is counted.
    {
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(
            &ada,
            "new_channel",
            vec![all.clone(), Value::text("design")],
        );
        let random = call(&ada, "new_channel", vec![all, Value::text("random")]);
        ada.commit();
        let ben = ada.fork("ben", &[crate::fixtures::AGENT, (CHAT, &loaded[1].ids)]);
        let first = call(
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
        ada.sync_with(&ben);

        // Ada's sidebar says two are new in design, one in random, and the places stand out.
        let sidebar = ada.view(APP);
        let place = |key: &str| {
            let all = sidebar.tree().of_kind("ui.NavItem");
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
                .map(|row| row.first_of_kind("ui.ChangeMark").is_some())
                .collect()
        };
        assert_eq!(marked(&mut channel), [false, false]);
        assert_eq!(
            unseen_from(&mut channel),
            item_of(&first),
            "the divider stands over the first message Ada had not seen"
        );
        post(&ada, &channel, "Thanks.");
        assert_eq!(marked(&mut channel), [false, false, false]);

        // Opening the channel marks what it showed seen, as the window does on the way out: the
        // divider goes, and so does the badge; random's is its own.
        ada.host
            .runtime()
            .mark_seen(&tilolang_tilo::personal::actor("ada"), &target(&design))
            .unwrap();
        ada.commit();
        let mut reopened = ada.host.open_resource(&target(&design), None).unwrap();
        assert_eq!(
            unseen_from(&mut reopened),
            None,
            "opening the channel saw it, and no divider stands"
        );
        let sidebar = ada.view(APP);
        let mut all = sidebar.tree().of_kind("ui.NavItem");
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

    phase a_channels_row_and_its_header_count_the_same_messages;
    // A channel's row in the sidebar and the header of its window count one thing: the messages of
    // it the person has not read. A thread branched off a message is not a message the transcript
    // shows and is not counted beside them, and reading the channel clears the row and the header
    // alike — one number, read from one place.
    {
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let ben = ada.fork("ben", &[crate::fixtures::AGENT, (CHAT, &loaded[1].ids)]);
        let first = call(
            &ben,
            "send",
            vec![design.clone(), rich("The build is up."), Value::none()],
        );
        call(
            &ben,
            "start_thread",
            vec![design.clone(), first, Value::text("Which machines")],
        );
        call(
            &ben,
            "send",
            vec![design.clone(), rich("On all three."), Value::none()],
        );
        call(
            &ben,
            "send",
            vec![design.clone(), rich("And the old one."), Value::none()],
        );
        ben.commit();
        ada.sync_with(&ben);

        // Three messages are new: the thread started from the first is not one of them, and both
        // places say the same number.
        let badge = badge_of(&mut ada.view(APP), "/design");
        let mut channel = ada.host.open_resource(&target(&design), None).unwrap();
        let header = header_count(&mut channel);
        assert_eq!(badge, Some(3), "three messages wait in design");
        assert_eq!(header, badge, "the row and the header say one number");

        // Ada reads to the end: the row says nothing new and the header marks nothing.
        ada.host
            .runtime()
            .mark_seen(&tilolang_tilo::personal::actor("ada"), &target(&design))
            .unwrap();
        ada.commit();
        assert_eq!(badge_of(&mut ada.view(APP), "/design"), None, "read");
        let mut read = ada.host.open_resource(&target(&design), None).unwrap();
        assert_eq!(header_count(&mut read), None, "and the header agrees");
    }

    phase a_readers_face_stands_where_their_read_mark_ends;
    // Who has read to a line is each member's own read mark: the transcript asks `seen_by(c, m)`
    // and shows that person's face on the one line their reading ends at. A reader's mark moves
    // their face down the conversation, and the person looking never sees their own.
    {
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let ben = ada.fork("ben", &[crate::fixtures::AGENT, (CHAT, &loaded[1].ids)]);
        let first = call(
            &ben,
            "send",
            vec![design.clone(), rich("One."), Value::none()],
        );
        let second = call(
            &ben,
            "send",
            vec![design.clone(), rich("Two."), Value::none()],
        );
        ben.commit();
        ada.sync_with(&ben);

        // Ben read to the second message: his mark is the one that made it.
        let read_to = tilolang_runtime::seen::SeenMark::of(
            &ben.host
                .store()
                .versions(&target(&second))
                .unwrap()
                .last()
                .cloned()
                .expect("the second message has a version"),
        );
        let runtime = ada.host.runtime();
        // The workspace's membership reaches her window, and his mark of the channel with it.
        runtime.set_members(
            vec![
                tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Editor),
                tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            ],
            None,
        );
        runtime.set_seen(&tilolang_tilo::personal::actor("ben"), &target(&design).id_string(), Some(read_to));

        // Ada's window says he read to the second line, and names him nowhere else.
        let seen_by = |at: &Value| {
            runtime
                .seen_by(&tilolang_tilo::personal::actor("ada"), &target(&design), &target(at))
                .unwrap()
                .into_iter()
                .map(|who| who.id().to_owned())
                .collect::<Vec<_>>()
        };
        assert!(seen_by(&first).is_empty(), "he read past the first");
        assert_eq!(
            seen_by(&second),
            ["ben".to_owned()],
            "he read to the second"
        );
        // Her own reading is no receipt: the marks she reads are everyone's but hers.
        let hers = runtime.seen(&tilolang_tilo::personal::actor("ben"), &target(&design).id_string());
        runtime.set_seen(&tilolang_tilo::personal::actor("ada"), &target(&design).id_string(), hers);
        assert_eq!(seen_by(&second), ["ben".to_owned()], "her own face aside");
        ada.commit();
    }
}

/// The number a place in the sidebar shows, on the row `key` names.
fn badge_of(view: &mut ViewInstance, key: &str) -> Option<i64> {
    view.poll_patches().unwrap();
    let places = view.tree().of_kind("ui.NavItem");
    let place = places
        .into_iter()
        .find(|item| item.props.get("key") == Some(&Value::text(key)))
        .unwrap_or_else(|| panic!("no place {key}"));
    match place.props.get("live") {
        Some(Value::Int(count)) => Some(*count),
        _ => None,
    }
}

/// The number a window's header says is new of what it shows, if it marks anything.
fn header_count(view: &mut ViewInstance) -> Option<i64> {
    view.poll_patches().unwrap();
    let marks = view.tree().of_kind("ui.ChangeMark");
    let mark = marks
        .into_iter()
        .find(|mark| mark.props.get("window") == Some(&Value::Bool(true)))?;
    match mark.props.get("count") {
        Some(Value::Int(count)) => Some(*count),
        other => panic!("a window's mark counts what is new: {other:?}"),
    }
}

/// Every text the window of the Chat before channels shows, in order: that Chat's messages were
/// plain text parts, not rich text, so its rows carry them among the row's other texts.
fn sent(view: &mut ViewInstance) -> Vec<String> {
    view.poll_patches().unwrap();
    let mut out = texts(view.tree());
    for row in rows(view) {
        out.extend(texts(&row));
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
/// document of one version is read and changed by its own types, never by the other's; the type
/// `Message` both declare is the same id with different fields, which is what takes the wrong
/// schema.
#[test]
fn a_workspace_held_by_one_version_of_chat_is_read_and_changed_by_that_version_with_the_other_loaded()
 {
    // The current Chat loaded first, then the Chat before channels: each a package with its
    // own ids lock, as an upgraded workspace has them.
    let (current, _) = Replica::new(
        "ada",
        Workspace::new(),
        &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
    );
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
    let (older, _) = Replica::new("ada", Workspace::new(), &[(BEFORE, BEFORE_IDS)]);
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
    // the current version's documents read.
    let mut seen_again = current.host.open_resource(&target(&channel), None).unwrap();
    assert_eq!(
        said(&mut seen_again),
        ["Shipped."],
        "the channel still reads its message with the older Chat loaded too"
    );
    post(&current, &seen_again, "Yes.");
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
    let (ada, loaded) = Replica::new(
        "ada",
        Workspace::new(),
        &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let ben = ada.fork("ben", &[crate::fixtures::AGENT, (CHAT, &loaded[1].ids)]);
    let one = call(
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
    ada.sync_with(&ben);

    // Ada's view stands the divider over the first of the two, and not over the second.
    let mut view = ada.host.open_resource(&target(&design), None).unwrap();
    assert_eq!(unseen_from(&mut view), item_of(&one), "over the first");

    // She reads: opening the channel marks what it showed seen, and nothing is left unread.
    ada.host
        .runtime()
        .mark_seen(&tilolang_tilo::personal::actor("ada"), &target(&design))
        .unwrap();
    ada.commit();
    let mut reopened = ada.host.open_resource(&target(&design), None).unwrap();
    assert_eq!(unseen_from(&mut reopened), None, "nothing unseen");

    // A message from Ben afterwards is what is new, alone.
    let answer = call(&ben, "send", vec![design, rich("Any time."), Value::none()]);
    ben.commit();
    ada.sync_with(&ben);
    assert_eq!(
        unseen_from(&mut reopened),
        item_of(&answer),
        "his answer is what is new"
    );
}

tilo_test_env::scenario! {
    fn people_have_conversations_of_their_own;

    phase a_direct_message_opens_with_a_person_and_takes_their_messages;
    // A direct message is a conversation in the same model as a channel: it stands in the sidebar
    // with everyone the workspace has, pressing a person opens the conversation with them (the
    // one already open, not a new empty one), and it takes messages as a channel does.
    {
        // A workspace with Ben in it: everyone the workspace has, from its own membership.
        let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace-dm", "ada");
        policy
            .members
            .insert("ben".to_owned(), tilo_action_policy::Role::Editor);
        let (ada, loaded) = Replica::access(
            "ada",
            Workspace::new(),
            &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
            Some(tilo_action_policy::MemberAccess::person(policy, "ada")),
        );
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let ben = ada.fork("ben", &[crate::fixtures::AGENT, (CHAT, &loaded[1].ids)]);
        // The workspace's membership, as the host hands it to the runtime.
        ada.host.runtime().set_members(
            vec![
                tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
                tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            ],
            None,
        );

        // The sidebar offers Ben under Direct messages: everyone in the workspace, from its
        // membership rather than a copy Chat keeps.
        let sidebar = ada.view(APP);
        let section = {
            let seen = sidebar.tree().of_kind("ui.NavSection");
            seen.into_iter()
                .find(|s| s.props.get("title") == Some(&Value::text("Direct messages")))
                .expect("the direct messages section")
                .clone()
        };
        let people = texts(&section);
        assert!(people.contains(&"ben".to_owned()), "{people:?}");

        // Pressing them opens the conversation with Ben, once: a person has one.
        let person = {
            let items = section.of_kind("ui.NavItem");
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
            vec![all.clone(), Value::Actor(Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben")))],
        );
        let again = call(
            &ada,
            "message_person",
            vec![all, Value::Actor(Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben")))],
        );
        assert_eq!(
            target(&dm).id_string(),
            target(&again).id_string(),
            "a person has one conversation"
        );
        // It takes a message from Ben, which Ada reads as any conversation's: an item created by
        // `insert` is held in its holder's document, and the store's index of documents learns of it
        // on the next scan, as a `commit` and a sync does.
        // Ada's own sync first: the workspace's host scans the documents, so the new conversation is
        // one the store can find.
        ada.commit();
        ben.commit();
        ada.sync_with(&ben);
        call(
            &ben,
            "send",
            vec![dm.clone(), rich("Still on for Saturday?"), Value::none()],
        );
        ben.commit();
        ada.sync_with(&ben);
        let mut conversation = ada.host.open_resource(&target(&dm), None).unwrap();
        assert_eq!(said(&mut conversation), ["Still on for Saturday?"]);
    }

    phase a_conversation_says_who_is_typing_from_presence;
    // A conversation says who is typing in it from the system's presence: the lenses on the
    // channel a person has open carry whether that person is composing a message there, never
    // what it says. The line goes when they stop, and a lens that is only open (not composing)
    // says nothing.
    {
        let (ada, _) = Replica::new("ada", Workspace::new(), &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let mut view = ada.host.open_resource(&target(&design), None).unwrap();
        let channel = target(&design).id_string();

        // Nobody at the channel but Ada: no one is typing.
        let typing = |view: &mut ViewInstance| -> Vec<String> {
            view.poll_patches().unwrap();
            texts(view.tree())
                .into_iter()
                .filter(|text| text.contains("is typing") || text.contains("are typing"))
                .collect()
        };
        assert!(typing(&mut view).is_empty(), "no one typing yet");

        // Ben has the channel open and is composing: the conversation says so, in his name. The
        // runtime's lenses are what `lenses(c)` reads; a view that read them runs again.
        let ben = Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben"));
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

    phase a_places_person_is_a_row_of_them_and_your_own_conversation_is_your_notes;
    // A place that stands for a person reads as them: the sidebar's direct messages carry the
    // person as `person`, so the list draws their avatar and presence, and the conversation with
    // yourself says so rather than showing your own name. What a view reads of the viewer
    // (`workspace.me`) is the person the window is open for, each replica its own.
    {
        let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace-dm", "ada");
        policy
            .members
            .insert("ben".to_owned(), tilo_action_policy::Role::Editor);
        let (ada, _loaded) = Replica::access(
            "ada",
            Workspace::new(),
            &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
            Some(tilo_action_policy::MemberAccess::person(policy, "ada")),
        );
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        ada.host.runtime().set_members(
            vec![
                tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
                tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            ],
            None,
        );

        // Everyone else in the workspace is a place that stands for them.
        let sidebar = ada.view(APP);
        let section = {
            let seen = sidebar.tree().of_kind("ui.NavSection");
            seen.into_iter()
                .find(|s| s.props.get("title") == Some(&Value::text("Direct messages")))
                .expect("the direct messages section")
                .clone()
        };
        let places = section.of_kind("ui.NavItem");
        let ben = places
            .iter()
            .find(|item| item.props.get("key") == Some(&Value::text("/person/ben")))
            .copied()
            .expect("Ben's place");
        assert_eq!(
            ben.props.get("person"),
            Some(&Value::Actor(Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben")))),
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
            vec![all, Value::Actor(Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")))],
        );
        ada.commit();
        let sidebar = ada.view(APP);
        let notes_place = {
            let items = sidebar.tree().of_kind("ui.NavItem");
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
}

/// How many ways the view offers to make a channel: the bar's primary, and any empty state's
/// own call to action. The host reads the bar's off the action's declaration, and its header
/// rules only ever drop a control that repeats one the content or the sidebar offers, so a view
/// offering this action once offers it once at every width.
fn new_channel_offered(view: &ViewInstance) -> usize {
    let mut nodes = view.tree().of_kind("ui.Button");
    nodes.extend(view.tree().of_kind("ui.EmptyState"));
    nodes
        .into_iter()
        .filter_map(|node| {
            let named = if node.kind == "ui.EmptyState" {
                node.props.get("action")
            } else {
                node.props.get("label")
            };
            match named {
                Some(Value::Text(label)) if label == "New channel" => Some(()),
                _ => None,
            }
        })
        .count()
}

/// The channels the app's own read holds, in the sidebar's order.
fn channels(r: &Replica) -> Vec<tilolang_runtime::ResourceRef> {
    match r.read(&r.root(APP), "channels") {
        Value::List(items) => items.iter().map(target).collect(),
        other => panic!("the app's channels: {other:?}"),
    }
}

/// What one channel is called, as the app stores it.
fn name_of(r: &Replica, channel: &tilolang_runtime::ResourceRef) -> String {
    match r.read(channel, "name") {
        Value::Text(name) => name,
        other => panic!("a channel's name: {other:?}"),
    }
}

/// The messages a channel holds, in the order they were said.
fn messages(
    r: &Replica,
    channel: &tilolang_runtime::ResourceRef,
) -> Vec<tilolang_runtime::ResourceRef> {
    match r.read(channel, "messages") {
        Value::List(items) => items.iter().map(target).collect(),
        other => panic!("a channel's messages: {other:?}"),
    }
}

tilo_test_env::scenario! {
    fn a_channel_made_at_the_window_takes_a_message_that_is_answered_reacted_edited_and_pinned;

    phase the_bar_and_the_apps_own_api_each_make_a_channel;
    {
        let (ada, _) = Replica::new(
            "ada",
            Workspace::new(),
            &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
        );
        ada.commit();
        // With no channel yet the places say so, with the line that says what a channel is for,
        // and the bar's primary is the one way to make the first; the places draw no call to
        // action beside it, which a wide window would show twice and a narrow one on two screens.
        let sidebar = ada.view(APP);
        let states = sidebar.tree().of_kind("ui.EmptyState");
        assert_eq!(
            states.first().and_then(|state| state.props.get("title")),
            Some(&Value::text("No channels yet")),
            "the places say the workspace has no channels where they would stand"
        );
        assert!(
            states
                .first()
                .is_some_and(|state| state.props.contains_key("message")),
            "with the line that says what a channel is for"
        );
        assert!(
            states
                .iter()
                .all(|state| !state.props.contains_key("action")),
            "no empty state draws a call to action beside the bar's"
        );
        assert_eq!(
            new_channel_offered(&sidebar),
            1,
            "the window offers the way to make the first channel once"
        );

        // The window bar's primary action, pressed as the window presses it.
        let make = button(sidebar.tree(), "New channel").expect("the bar's New channel");
        assert!(
            matches!(sidebar.press(&make).unwrap(), InvokeOutcome::Done(_)),
            "the bar's New channel runs"
        );
        ada.commit();
        assert_eq!(channels(&ada).len(), 1, "the bar made a channel");
        assert_eq!(name_of(&ada, &channels(&ada)[0]), "new-channel");
        let sidebar = ada.view(APP);
        assert!(
            texts(sidebar.tree()).contains(&"new-channel".to_owned()),
            "the sidebar lists the channel the bar made: {:?}",
            texts(sidebar.tree())
        );
        assert!(
            sidebar
                .tree()
                .first_of_kind("ui.EmptyState")
                .is_none_or(|state| state.props.get("title") != Some(&Value::text("No channels yet"))),
            "no empty state saying the workspace has no channels once one exists"
        );
        assert_eq!(
            new_channel_offered(&sidebar),
            1,
            "the bar keeps its primary once the empty state is gone"
        );

        // What the window's bar, the palette and system search run: an address on the app's
        // root and no argument, built through the app's own API and carried out as the person's
        // step. A name the action defaults fills in.
        let meta = ada.host.package(APP).unwrap().meta;
        let action = crate::fixtures::action_id(&meta, "new_channel");
        let root = ada.root(APP);
        let args = serde_json::Map::new();
        let planned = ada.host.carrying_out(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            || {
                ada.host.plan(&tilolang_tilo::publish::PlanRequest {
                    app: APP,
                    kind: &tilolang_tilo::meta::MetaExt::kind_of(&meta, "Conversations"),
                    action_id: &action,
                    resource: Some(root.id_string().as_str()),
                    args: &args,
                    caller: None,
                    from: None,
                })
            },
        );
        if let Err(why) = &planned {
            panic!("New channel run by address: {why}");
        }
        let made = ada
            .host
            .take_returned(&action)
            .expect("the invocation answers the channel it made");
        ada.commit();
        assert_eq!(channels(&ada).len(), 2, "the address made a channel");
        assert_eq!(name_of(&ada, &made), "new-channel");
        assert!(
            channels(&ada).iter().any(|channel| channel == &made),
            "the app's own read holds what the invocation made"
        );
        let sidebar = ada.view(APP);
        assert_eq!(
            texts(sidebar.tree())
                .iter()
                .filter(|text| *text == "new-channel")
                .count(),
            2,
            "the sidebar lists both: {:?}",
            texts(sidebar.tree())
        );
    }

    phase the_message_it_holds_is_answered_reacted_to_edited_and_pinned;
    {
        let (ada, _) = Replica::new(
            "ada",
            Workspace::new(),
            &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
        );
        ada.commit();
        let sidebar = ada.view(APP);
        let make = button(sidebar.tree(), "New channel").expect("the bar's New channel");
        sidebar.press(&make).unwrap();
        ada.commit();
        let channel = channels(&ada).remove(0);
        let mut view = ada.host.open_resource(&channel, None).unwrap();

        // Say something in it.
        post(&ada, &view, "Hello team");
        assert_eq!(said(&mut view), ["Hello team"]);

        // Answer it: Reply on its row puts it over the composer, and the message quotes it.
        let row = rows(&mut view).remove(0);
        let reply = button(&row, "Reply").expect("Reply on a message");
        view.press(&reply).unwrap();
        view.poll_patches().unwrap();
        assert!(
            view.tree().first_of_kind("ui.Quote").is_some(),
            "the message being answered stands over the composer"
        );
        post(&ada, &view, "Welcome to launch");
        let rows_shown = rows(&mut view);
        assert_eq!(said(&mut view), ["Hello team", "Welcome to launch"]);
        assert_eq!(
            rows_shown[1]
                .first_of_kind("ui.Quote")
                .and_then(|quote| quote.props.get("excerpt")),
            Some(&Value::text("Hello team")),
            "the answer quotes what it answers"
        );

        // React to it: the hover bar's picker adds the emoji.
        let row = rows(&mut view).remove(0);
        let picker = row
            .first_of_kind("ui.EmojiPicker")
            .expect("a reaction on a message's toolbar")
            .key
            .clone();
        assert!(
            matches!(
                view.fire_with(&picker, "pick", Some(&Value::text("🎉")))
                    .unwrap(),
                InvokeOutcome::Done(_)
            ),
            "the picker reacts"
        );
        ada.commit();
        view.poll_patches().unwrap();
        let row = rows(&mut view).remove(0);
        assert_eq!(
            reactions(&row),
            [(
                "🎉".to_owned(),
                vec![Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
                    "ada"
                ))]
            )],
            "the message carries the reaction"
        );

        // Fix what it says: Edit in its place, then Save.
        let edit = button(&row, "Edit").expect("Edit on your own message");
        view.press(&edit).unwrap();
        view.poll_patches().unwrap();
        let row = rows(&mut view).remove(0);
        let editor = row
            .first_of_kind("ui.RichInput")
            .expect("the editor in the message's place")
            .key
            .clone();
        view.edit(&editor, &rich("Hello team!")).unwrap();
        let save = button(&row, "Save").expect("Save beside the editor");
        assert!(matches!(view.press(&save).unwrap(), InvokeOutcome::Done(_)));
        ada.commit();
        view.poll_patches().unwrap();
        assert_eq!(said(&mut view), ["Hello team!", "Welcome to launch"]);

        // Keep it: Pin on the message's toolbar.
        let row = rows(&mut view).remove(0);
        let pin = button(&row, "Pin").expect("Pin on a message");
        view.press(&pin).unwrap();
        ada.commit();
        view.poll_patches().unwrap();
        let row = rows(&mut view).remove(0);
        assert!(
            texts(&row).contains(&"Pinned".to_owned()),
            "the message says it is pinned: {:?}",
            texts(&row)
        );
        assert!(
            button(&row, "Unpin").is_some(),
            "Pin turns into Unpin"
        );

        // Each of them is an action a script or an agent runs as the person presses it: edit,
        // react, pin and answer by address, and the app's own read shows all of it.
        let first = messages(&ada, &channel).remove(0);
        let first = Value::resource(&first);
        call(&ada, "edit", vec![first.clone(), rich("Hello from a script")]);
        call(&ada, "react", vec![first.clone(), Value::text("👍")]);
        call(&ada, "pin", vec![first.clone(), Value::Bool(true)]);
        call(
            &ada,
            "send",
            vec![
                Value::resource(&channel),
                rich("And an answer"),
                Value::Opt(Some(Box::new(first))),
            ],
        );
        ada.commit();
        let mut view = ada.host.open_resource(&channel, None).unwrap();
        assert_eq!(
            said(&mut view),
            ["Hello from a script", "Welcome to launch", "And an answer"]
        );
        let row = rows(&mut view).remove(0);
        assert!(
            reactions(&row)
                .iter()
                .any(|(emoji, who)| emoji == "👍" && !who.is_empty()),
            "the message carries the reaction a script added: {:?}",
            reactions(&row)
        );
        assert!(
            texts(&row).contains(&"Pinned".to_owned()),
            "the message is still pinned"
        );
        assert_eq!(
            rows(&mut view)[2]
                .first_of_kind("ui.Quote")
                .and_then(|quote| quote.props.get("excerpt")),
            Some(&Value::text("Hello from a script")),
            "the answer a script sent quotes what it answers"
        );
    }
}

/// A message sent in a channel is found by the system's search, and a read of it holds its
/// text: what another app's page or an agent asking about the workspace gets.
#[test]
fn a_message_is_found_by_search_and_a_read_holds_what_it_says() {
    use std::sync::Arc;

    use tilo_desktop_search::Source as _;
    use tilo_desktop_search::content_index::AppContentIndex;
    use tilo_desktop_search::indexed::IndexedContent;
    use tilo_resource::search_index::{IndexState, Publisher};

    let (ada, _) = Replica::new(
        "ada",
        Workspace::new(),
        &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let message = call(
        &ada,
        "send",
        vec![
            design,
            rich("The launch build is on the test machines."),
            Value::none(),
        ],
    );
    ada.commit();

    // The system's search index, filled from what the app publishes and asked as the session
    // asks it.
    let index = AppContentIndex::in_memory();
    let published = tilolang_tilo::search::publish_whole(
        Publisher::default(),
        IndexState {
            generation: 1,
            secrets_unlocked: None,
        },
        tilolang_tilo::search::whole(&ada.host),
    );
    for delta in &published.deltas {
        index
            .publish("workspace", APP, "chat", delta)
            .expect("the index takes what the app publishes");
    }
    let hits = IndexedContent::new(Arc::new(index), str::to_owned, "ada").query("launch build");
    let found = hits
        .iter()
        .find(|hit| hit.title.contains("launch build"))
        .unwrap_or_else(|| panic!("the message is found: {hits:#?}"));
    assert_eq!(
        found.subtitle.as_deref(),
        Some("Message"),
        "what search finds says what it is: {found:#?}"
    );

    // The app's own read of the message holds what it says.
    let read = ada
        .host
        .answer(
            tilolang_tilo::effects::HostEffect::ReadValues {
                item: target(&message).id_string(),
            },
            None,
        )
        .expect("the message reads");
    let read = serde_json::to_string(&read.value).expect("the reading as JSON");
    assert!(
        read.contains("The launch build is on the test machines."),
        "the read holds the message's text: {read}"
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

/// Sending is an action on a channel, a thread or a direct conversation: the change a send in a
/// channel commits names the channel's kind, and the package's surface publishes the send there,
/// or every other device in a real workspace refuses the message.
#[test]
fn a_message_sent_in_a_channel_names_a_verb_the_channels_surface_publishes() {
    let (ada, _) = Replica::new(
        "ada",
        Workspace::new(),
        &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let channel = match &design {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a channel: {other:?}"),
    };
    let before = ada.versions(&channel).len();
    call(
        &ada,
        "send",
        vec![
            design,
            rich("Is the build on the test machines?"),
            Value::none(),
        ],
    );
    ada.commit();
    // The send is a version of the channel itself, made by the channel's own action.
    let versions = ada.versions(&channel);
    assert!(
        versions.len() > before,
        "the send commits a change of the channel"
    );
    let meta = ada.host.package(APP).unwrap().meta;
    let send = crate::fixtures::action_id(&meta, "send");
    let last = versions.last().unwrap();
    assert_eq!(last.action, send, "{versions:#?}");
    let surfaces = tilolang_tilo::surface::package_surfaces(&meta, &tilolang_runtime::SourceWords);
    let surface = surfaces
        .iter()
        .find(|surface| surface.kind == "tl.so.tilo.lang.chat.TextChannel")
        .unwrap_or_else(|| panic!("no surface of the channel"));
    assert!(
        surface.verb(&last.action).is_some(),
        "{} publishes no {}",
        surface.kind,
        last.action
    );
}

/// The asks the agents' package lists, each as its row names the agent asked.
fn asks(r: &Replica) -> Vec<String> {
    let mut view = r.view(AGENT_APP);
    rows(&mut view)
        .iter()
        .map(|row| texts(row).join(" "))
        .collect()
}

const AGENT_APP: &str = "so.tilo.agent";

/// Writing to an agent asks it: each message sent in a direct conversation with one makes one
/// ask of it, made from the message by whoever wrote it, whether a person presses Send or a
/// script invokes `send`, and a conversation with a person asks nobody.
#[test]
fn each_message_to_an_agent_asks_it_once() {
    let (ada, _) = Replica::new(
        "ada",
        crate::fixtures::Workspace::new(),
        &[crate::fixtures::AGENT, (CHAT, CHAT_IDS)],
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.host.root_of(AGENT_APP).unwrap();
    ada.commit();
    let member = |m| Value::Actor(Actor::from(m));
    let tide = call(
        &ada,
        "message_person",
        vec![
            all.clone(),
            member(tilolang_tilo::runtime::ids::MemberId::agent("tide")),
        ],
    );
    let ben = call(
        &ada,
        "message_person",
        vec![
            all,
            member(tilolang_tilo::runtime::ids::MemberId::person("ben")),
        ],
    );
    ada.commit();
    let tide_view = ada.host.open_resource(&target(&tide), None).unwrap();
    let ben_view = ada.host.open_resource(&target(&ben), None).unwrap();
    post(&ada, &tide_view, "What changed in the build?");
    post(&ada, &ben_view, "Lunch at one?");
    assert_eq!(
        asks(&ada).len(),
        1,
        "one ask, of the agent: {:?}",
        asks(&ada)
    );

    post(&ada, &tide_view, "And the tests?");
    assert_eq!(
        asks(&ada).len(),
        2,
        "a second message asks again: {:?}",
        asks(&ada)
    );
    // Each ask is made from its message, which its row names.
    for message in ["What changed in the build?", "And the tests?"] {
        assert_eq!(
            asks(&ada).iter().filter(|a| a.contains(message)).count(),
            1,
            "one ask made from {message:?}: {:?}",
            asks(&ada)
        );
    }

    // A script's send asks as a person's does, made from the message it sent.
    let sent = call(&ada, "send", vec![tide, rich("Any news?"), Value::none()]);
    call(&ada, "send", vec![ben, rich("Coffee?"), Value::none()]);
    ada.commit();
    assert_eq!(
        asks(&ada).len(),
        3,
        "the script's message asks too: {:?}",
        asks(&ada)
    );
    let made: Vec<_> = ada
        .host
        .store()
        .linked(&target(&sent))
        .unwrap()
        .into_iter()
        .filter(|link| link.how == tilolang_runtime::store::LinkHow::CreatedFrom)
        .collect();
    assert_eq!(
        made.len(),
        1,
        "one ask is made from the sent message: {made:?}"
    );

    let mut tide_view = tide_view;
    assert_eq!(
        said(&mut tide_view),
        ["What changed in the build?", "And the tests?", "Any news?"]
    );
}
