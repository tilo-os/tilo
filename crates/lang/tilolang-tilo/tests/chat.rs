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
const APP: &str = "so.tilo.chat";

pub(crate) fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
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
    /// The rich text a node shows outside any fold: what it says now, not what it said before.
    fn standing<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
        if node.kind == "ui.DisclosureRow" {
            return;
        }
        if node.kind == "ui.BlockEditor" {
            out.push(node);
        }
        for child in &node.children {
            standing(child, out);
        }
    }
    rows(view)
        .iter()
        .map(|row| {
            let mut editors = Vec::new();
            standing(row, &mut editors);
            let text: Vec<String> = editors.into_iter().flat_map(texts).collect();
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

/// What the text prop `name` of `node` says, whether it was passed as a value or an option.
fn text_prop(node: &UiNode, name: &str) -> Option<String> {
    match node.props.get(name) {
        Some(Value::Text(text)) => Some(text.clone()),
        Some(Value::Opt(Some(inner))) => match &**inner {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Open `at` the way the system does: run its type's `opened action` as the person, then show
/// its window. Opening a conversation is what makes the draft its person writes in.
fn open(r: &Replica, at: &tilolang_runtime::ResourceRef) -> ViewInstance {
    if let Some(action) = r.host.runtime().opened_action(&at.ty) {
        match r
            .host
            .runtime()
            .invoke_action(
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(&r.name)),
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
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let team = call(&ada, "new_category", vec![all.clone(), Value::text("Team")]);
    let design = call(
        &ada,
        "new_channel",
        vec![
            all,
            Value::text("design"),
            Value::text(""),
            Value::some(team),
        ],
    );
    ada.commit();
    let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
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

    let mut ada_design = open(&ada, &target(&design));
    let mut ben_design = open(&ben, &target(&design));

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
    let mut ben_thread = open(&ben, &target(&thread));
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
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
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
    let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    let mut ada_design = open(&ada, &target(&design));
    let mut ben_design = open(&ben, &target(&design));
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
        let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let mut view = open(&ada, &target(&design));
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
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
        let mut ada_design = open(&ada, &target(&design));
        let mut ben_design = open(&ben, &target(&design));
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
            // The message says it was edited, and what it said before stands under that line.
            let history = row
                .of_kind("ui.DisclosureRow")
                .into_iter()
                .find(|row| text_prop(row, "title") == Some("Edited".to_owned()))
                .unwrap_or_else(|| panic!("marked as edited: {:?}", texts(&row)));
            assert!(
                texts(history).contains(&"The bulid is on the test machines.".to_owned()),
                "the words before the edit: {:?}",
                texts(history)
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
                texts(&row).contains(&"Edited".to_owned()),
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
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
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
        let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);

        // A holder's action on a message is in the message's menu (`std.transcript` puts every
        // action offered on an item there), as a destructive one.
        let mut ada_design = open(&ada, &target(&design));
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
                    && destructive == &Some(Value::Bool(true))),
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
        let mut ben_design = open(&ben, &target(&design));
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
    let (later, _) = Replica::new("ada", ada.doc.fork(), &[(CHAT, CHAT_IDS)]);
    let sidebar = later.view(APP);
    let shown = texts(sidebar.tree());
    assert!(
        shown.contains(&"Plans".to_owned()),
        "the conversation is a channel: {shown:?}"
    );

    let mut channel = open(&later, &target(&plans));
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
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(
            &ada,
            "new_channel",
            vec![all.clone(), Value::text("design")],
        );
        let random = call(&ada, "new_channel", vec![all, Value::text("random")]);
        ada.commit();
        let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
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
        let mut channel = open(&ada, &target(&design));
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
        let mut reopened = open(&ada, &target(&design));
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
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
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
        let mut channel = open(&ada, &target(&design));
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
        let mut read = open(&ada, &target(&design));
        assert_eq!(header_count(&mut read), None, "and the header agrees");
    }

    phase a_readers_face_stands_where_their_read_mark_ends;
    // Who has read to a line is each member's own read mark: the transcript asks `seen_by(c, m)`
    // and shows that person's face on the one line their reading ends at. A reader's mark moves
    // their face down the conversation, and the person looking never sees their own.
    {
        let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
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
    let (current, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
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
    let mut seen = open(&current, &target(&channel));
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
    let mut thread = open(&older, &target(&plans));
    assert!(
        says(&mut thread, "Walk on Saturday?"),
        "{:?}",
        sent(&mut thread)
    );

    // The current Chat's channel still reads its message, and still takes one written by the
    // current version's action: the older version's schema, loaded second, never takes over how
    // the current version's documents read.
    let mut seen_again = open(&current, &target(&channel));
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
    thread = open(&older, &target(&plans));
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
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
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
    let mut view = open(&ada, &target(&design));
    assert_eq!(unseen_from(&mut view), item_of(&one), "over the first");

    // She reads: opening the channel marks what it showed seen, and nothing is left unread.
    ada.host
        .runtime()
        .mark_seen(&tilolang_tilo::personal::actor("ada"), &target(&design))
        .unwrap();
    ada.commit();
    let mut reopened = open(&ada, &target(&design));
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
            &[(CHAT, CHAT_IDS)],
            Some(tilo_action_policy::MemberAccess::person(policy, "ada")),
        );
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
        // The workspace's membership, as the host hands it to the runtime.
        ada.host.runtime().set_members(
            vec![
                tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
                tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            ],
            None,
        );

        // The sidebar keeps its channels in sight and offers the workspace's people on a page:
        // pressing Everyone opens the people, each with the way to the conversation with them.
        let sidebar = ada.view(APP);
        let section = {
            let seen = sidebar.tree().of_kind("ui.NavSection");
            seen.into_iter()
                .find(|s| s.props.get("title") == Some(&Value::text("Direct messages")))
                .expect("the direct messages section")
                .clone()
        };
        let people = texts(&section);
        assert!(people.contains(&"Everyone".to_owned()), "{people:?}");
        let everyone = {
            let items = section.of_kind("ui.NavItem");
            items
                .into_iter()
                .find(|item| item.props.get("key") == Some(&Value::text("/people")))
                .expect("the people page's place")
                .clone()
        };
        assert!(
            sidebar.event_action(&everyone.key, "select").is_some(),
            "the place opens the people page"
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
        let mut conversation = open(&ada, &target(&dm));
        assert_eq!(said(&mut conversation), ["Still on for Saturday?"]);
    }

    phase a_conversation_says_who_is_typing_from_presence;
    // A conversation says who is typing in it from the system's presence: the lenses on the
    // channel a person has open carry whether that person is composing a message there, never
    // what it says. The line goes when they stop, and a lens that is only open (not composing)
    // says nothing.
    {
        let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
        let all = Value::resource(&ada.host.root_of(APP).unwrap());
        ada.commit();
        let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
        ada.commit();
        let mut view = open(&ada, &target(&design));
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
    // (`invoker`) is the person the window is open for, each replica its own.
    {
        let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace-dm", "ada");
        policy
            .members
            .insert("ben".to_owned(), tilo_action_policy::Role::Editor);
        let (ada, _loaded) = Replica::access(
            "ada",
            Workspace::new(),
            &[(CHAT, CHAT_IDS)],
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

        // Everyone in the workspace is a row of the people page, with their face, and the
        // conversation with them one press away.
        let root = ada.host.root_of(APP).unwrap();
        let people_view = view_id(&ada, "people");
        let mut page = ada.host.open_resource(&root, Some(&people_view)).unwrap();
        page.poll_patches().unwrap();
        let shown = texts(page.tree());
        for part in ["ada", "ben"] {
            assert!(shown.contains(&part.to_owned()), "{part} in {shown:?}");
        }
        // The row's own controls: what a person's card holds opens from the row's "About" and
        // is not the row's.
        fn row_buttons(node: &UiNode, out: &mut Vec<String>) {
            if node.kind == "ui.PopoverButton" {
                return;
            }
            if node.kind == "ui.Button"
                && let Some(label) = text_prop(node, "label")
            {
                out.push(label);
            }
            for child in &node.children {
                row_buttons(child, out);
            }
        }
        let mut labels = Vec::new();
        row_buttons(page.tree(), &mut labels);
        assert_eq!(
            labels.iter().filter(|l| *l == "Message").count(),
            1,
            "one Message control, for Ben: your own place is your notes: {labels:?}"
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

/// A grant that lets a person write a channel is the grant that lets them post in it: the call
/// and the change read one node, the channel's own.
#[test]
fn a_grant_that_lets_a_person_write_a_channel_lets_them_post_there() {
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(
        &ada,
        "new_channel",
        vec![all.clone(), Value::text("design")],
    );
    let random = call(&ada, "new_channel", vec![all, Value::text("random")]);
    ada.commit();

    // Ben is a Viewer of the workspace, granted edit on `design` alone: the grant a share of the
    // channel writes, on the channel's own node.
    let share = |channel: &tilolang_runtime::ResourceRef, access| {
        let node = tilo_action_policy::TreePath::workspace("workspace")
            .app(APP)
            .kind("tl.so.tilo.chat.TextChannel")
            .any(tilo_action_policy::Level::Resource)
            .item(
                channel
                    .item
                    .clone()
                    .expect("a channel is an item of its document"),
            );
        let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace", "ada");
        policy
            .members
            .insert("ben".to_owned(), tilo_action_policy::Role::Viewer);
        policy.grants.extend(
            tilo_action_policy::share::share_grants("workspace", "ada", "share-1", &node, access)
                .unwrap(),
        );
        policy
    };
    let access: std::rc::Rc<std::cell::RefCell<Option<tilo_action_policy::MemberAccess>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let (ben, _) = Replica::following("ben", ada.doc.fork(), &[(CHAT, &loaded[0].ids)], {
        let access = std::rc::Rc::clone(&access);
        move || access.borrow().clone()
    });
    let plan = |channel: &tilolang_runtime::ResourceRef| {
        let meta = ben.host.package(APP).unwrap().meta;
        let send = meta.actions.iter().find(|a| a.name == "send").unwrap();
        let kind = tilolang_tilo::meta::MetaExt::kind_of(&meta, "TextChannel");
        ben.host.plan(&tilolang_tilo::publish::PlanRequest {
            app: APP,
            kind: &kind,
            action_id: &send.id,
            resource: Some(&channel.id_string()),
            args: serde_json::json!({ "body": "Hello there" })
                .as_object()
                .unwrap(),
            caller: None,
            from: None,
        })
    };

    // Edit on the channel: ben posts there, and nowhere he holds nothing.
    let design_ref = target(&design);
    let random_ref = target(&random);
    *access.borrow_mut() = Some(tilo_action_policy::MemberAccess::person(
        share(&design_ref, tilo_action_policy::share::ShareAccess::Edit),
        "ben",
    ));
    assert!(
        plan(&design_ref).is_ok(),
        "the grant that writes the channel posts in it: {:?}",
        plan(&design_ref).err()
    );
    let elsewhere = plan(&random_ref).err().expect("no grant, no post");
    assert!(elsewhere.contains("not allowed"), "{elsewhere}");

    // Viewing it is no writing it: a share to read refuses the post.
    *access.borrow_mut() = Some(tilo_action_policy::MemberAccess::person(
        share(&design_ref, tilo_action_policy::share::ShareAccess::View),
        "ben",
    ));
    let reading_only = plan(&design_ref).err().expect("viewing is not posting");
    assert!(reading_only.contains("not allowed"), "{reading_only}");
}

#[test]
fn a_channel_lists_its_threads_and_opening_one_opens_it() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    let ask = call(
        &ada,
        "send",
        vec![
            design.clone(),
            rich("Can we move the account step to the end?"),
            Value::none(),
        ],
    );
    let account = call(
        &ada,
        "start_thread",
        vec![design.clone(), ask, Value::text("Account step")],
    );
    let when = call(
        &ada,
        "send",
        vec![
            design.clone(),
            rich("When does the build go out?"),
            Value::none(),
        ],
    );
    call(
        &ada,
        "start_thread",
        vec![design.clone(), when, Value::text("Release date")],
    );
    ada.commit();

    // The channel lists the threads its messages started, each a row of the shared list under
    // the section that counts them.
    let mut view = open(&ada, &target(&design));
    view.poll_patches().unwrap();
    let rows: Vec<&UiNode> = view
        .tree()
        .of_kind("ui.Section")
        .into_iter()
        .flat_map(|section| section.of_kind("ui.ListRow"))
        .collect();
    let titles: Vec<String> = rows
        .iter()
        .filter_map(|row| text_prop(row, "title"))
        .collect();
    assert_eq!(titles, ["Account step", "Release date"], "{titles:?}");

    // Each row shows the thread it lists, and activating it opens that thread.
    let row = rows
        .into_iter()
        .find(|row| text_prop(row, "title") == Some("Account step".to_owned()))
        .expect("the account thread's row");
    assert!(
        matches!(row.props.get("item"), Some(Value::Ref(reference))
            if reference.target.id_string() == target(&account).id_string()),
        "the row names its thread: {:?}",
        row.props.get("item")
    );
    let opened = view
        .event_action(&row.key, "activate")
        .expect("the row opens its thread");
    assert!(
        matches!(opened.args.first(), Some(Value::Ref(reference))
            if reference.target.id_string() == target(&account).id_string()),
        "the row opens the thread it lists: {:?}",
        opened.args
    );
}

#[test]
fn a_place_is_keyed_by_its_conversation_so_a_rename_and_a_twin_name_stay_apart() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    // Ada owns the workspace: a channel's name is its moderators' to change.
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
        ],
        None,
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(
        &ada,
        "new_channel",
        vec![all.clone(), Value::text("design")],
    );
    let team = call(&ada, "new_category", vec![all.clone(), Value::text("Team")]);
    let twin = call(
        &ada,
        "new_channel",
        vec![
            all,
            Value::text("design"),
            Value::text(""),
            Value::Opt(Some(Box::new(team))),
        ],
    );
    ada.commit();
    // The place of `at`: the row the sidebar draws for that conversation.
    let place = |view: &ViewInstance, at: &tilolang_runtime::ResourceRef| -> UiNode {
        view.tree()
            .of_kind("ui.NavItem")
            .into_iter()
            .find(|item| {
                matches!(item.props.get("item"), Some(Value::Ref(reference))
                    if reference.target.id_string() == at.id_string())
            })
            .unwrap_or_else(|| panic!("a place for {at:?}"))
            .clone()
    };
    // Two channels called `design` stand apart, each place keyed by its own id.
    let sidebar = ada.view(APP);
    assert_eq!(
        text_prop(&place(&sidebar, &target(&design)), "key"),
        Some(target(&design).id_string()),
        "the channel's own id"
    );
    assert_eq!(
        text_prop(&place(&sidebar, &target(&twin)), "key"),
        Some(target(&twin).id_string()),
        "the twin's own id"
    );

    // A rename changes what the place says, never the key it keeps.
    call(
        &ada,
        "rename_channel",
        vec![design.clone(), Value::text("design reviews")],
    );
    ada.commit();
    let mut renamed = ada.view(APP);
    let renamed_place = place(&renamed, &target(&design));
    assert_eq!(
        text_prop(&renamed_place, "key"),
        Some(target(&design).id_string()),
        "a rename keeps the place"
    );
    assert_eq!(
        text_prop(&renamed_place, "label"),
        Some("design reviews".to_owned())
    );

    // Choosing the renamed channel selects it by that key.
    let outcome = renamed.fire(&renamed_place.key, "select").unwrap();
    assert!(matches!(outcome, InvokeOutcome::Done(_)), "{outcome:?}");
    renamed.poll_patches().unwrap();
    let selected = renamed
        .tree()
        .first_of_kind("ui.NavList")
        .expect("the sidebar's list")
        .props
        .get("selected")
        .cloned();
    assert_eq!(
        selected,
        Some(Value::text(target(&design).id_string())),
        "the place chosen is the conversation's id"
    );
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
            &[(CHAT, CHAT_IDS)],
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
        let made = target(
            &ada
                .host
                .take_returned(&action)
                .expect("the invocation answers the channel it made"),
        );
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
            &[(CHAT, CHAT_IDS)],
        );
        ada.commit();
        let sidebar = ada.view(APP);
        let make = button(sidebar.tree(), "New channel").expect("the bar's New channel");
        sidebar.press(&make).unwrap();
        ada.commit();
        let channel = channels(&ada).remove(0);
        let mut view = open(&ada, &channel);

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
        let mut view = open(&ada, &channel);
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
    use tilo_desktop_search::indexed::IndexedContent;
    use tilo_resource::search_index::{IndexState, Publisher};
    use tilo_workspace_service::search::{AppContentIndex, IndexSearch};

    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
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
    let search = IndexSearch::new(
        Arc::new(index),
        tilo_workspace_service::search::deciding(|_, _| true),
    );
    let hits = IndexedContent::new(
        Arc::new(search),
        tilo_actions::remote::Caller::local_person("ada"),
        str::to_owned,
        "ada",
        Arc::new(|| {}),
    )
    .query("launch build");
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
        .read_values(&target(&message).id_string())
        .expect("the message reads");
    let read = serde_json::to_string(&read).expect("the reading as JSON");
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
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
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
        .find(|surface| surface.kind == "tl.so.tilo.chat.TextChannel")
        .unwrap_or_else(|| panic!("no surface of the channel"));
    assert!(
        surface.verb(&last.action).is_some(),
        "{} publishes no {}",
        surface.kind,
        last.action
    );
}

/// A report is settled by the workspace's moderators alone: a member who may write in the channel
/// files one, and their own `decide` of it is refused with the reason, while a moderator's
/// stands. Who moderates is the role the workspace gave them, not anything Chat keeps.
#[test]
fn a_report_is_settled_by_a_moderator_and_refused_to_anyone_else() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            tilolang_runtime::members::Member::person("mo", tilo_actions::policy::Role::Moderator),
        ],
        None,
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let message = call(
        &ada,
        "send",
        vec![design.clone(), rich("Buy cheap followers here")],
    );
    ada.commit();

    let meta = ada.host.package(APP).unwrap().meta;
    let as_person = |who: &str, action: &str, args: Vec<Value>| {
        ada.host.runtime().invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(who)),
            (crate::fixtures::action_id(&meta, action).as_str(), args),
        )
    };
    let report = match as_person("ben", "report", vec![design, message, Value::text("spam")]) {
        Ok(InvokeOutcome::Done(report)) => report,
        other => panic!("Ben files the report: {other:?}"),
    };
    ada.commit();

    let refused = as_person(
        "ben",
        "decide",
        vec![report.clone(), Value::text("removed")],
    );
    assert!(
        !matches!(refused, Ok(InvokeOutcome::Done(_))),
        "an editor's settling of a report is refused: {refused:?}"
    );
    assert!(
        format!("{refused:?}").contains("Only the workspace's moderators settle a report"),
        "with the reason: {refused:?}"
    );

    let settled = as_person("mo", "decide", vec![report, Value::text("removed")]);
    assert!(
        matches!(settled, Ok(InvokeOutcome::Done(_))),
        "a moderator settles it: {settled:?}"
    );
}

/// A report reaches its reporter and the workspace's moderators alone: a member reading the
/// conversation it was filed in sees no report there, while whoever holds the moderator role,
/// or one that includes it, does, without Chat naming who moderates.
#[test]
fn a_report_is_read_by_its_reporter_and_the_moderators_and_no_other_member() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let message = call(
        &ada,
        "send",
        vec![design.clone(), rich("Buy cheap followers")],
    );
    ada.commit();
    let meta = ada.host.package(APP).unwrap().meta;
    let report = match ada.host.runtime().invoke_action(
        &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben")),
        (
            crate::fixtures::action_id(&meta, "report").as_str(),
            vec![design.clone(), message, Value::text("spam")],
        ),
    ) {
        Ok(InvokeOutcome::Done(report)) => target(&report),
        other => panic!("Ben files the report: {other:?}"),
    };
    ada.commit();

    let [ben, cy, mo, owner] = ["ben", "cy", "mo", "ola"]
        .map(|name| Replica::new(name, Workspace::new(), &[(CHAT, CHAT_IDS)]).0);
    mo.hold(tilo_action_policy::Role::Moderator);
    owner.hold(tilo_action_policy::Role::Owner);
    cy.hold(tilo_action_policy::Role::Editor);
    for other in [&ben, &cy, &mo, &owner] {
        ada.send_to(other);
    }
    let channel = target(&design);
    let reports = |replica: &Replica| match replica.read(&channel, "reports") {
        Value::List(reports) => reports.len(),
        other => panic!("a channel's reports are a list, got {other:?}"),
    };
    let reads =
        |replica: &Replica| match replica
            .doc
            .command(tilo_resource_store::wire::Command::Get {
                kind: tilolang_tilo::meta::MetaExt::kind_of(&meta, "Report"),
                id: report.id_string(),
            }) {
            tilo_resource_store::wire::Reply::Resource(found) => found.is_some(),
            other => panic!("a read answered {other:?}"),
        };
    assert_eq!(
        (reports(&cy), reads(&cy)),
        (0, false),
        "a member reading the channel sees no report"
    );
    assert_eq!((reports(&ben), reads(&ben)), (1, true), "its reporter does");
    assert_eq!((reports(&mo), reads(&mo)), (1, true), "a moderator does");
    assert_eq!(
        (reports(&owner), reads(&owner)),
        (1, true),
        "an owner, whose role includes moderating, does"
    );
}

/// The bridge package is its own app: the service's records and views live there, and Chat stays
/// general — no service type, no per-service field. A message from outside shows its author's
/// picture and the emoji it carries through Chat's own outside-author fields.
const DISCORD: &str = include_str!("../../../../apps/tl/discord/app.tl");
const DISCORD_IDS: &str = include_str!("../../../../apps/tl/discord/app.ids");

#[test]
fn chat_holds_no_service_type_and_shows_an_outside_author_from_its_own_fields() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let meta = ada.host.package(APP).unwrap().meta;
    assert!(
        !meta.types.iter().any(|t| t.name == "Discord"),
        "Chat holds no Discord type"
    );
    let conversations = meta
        .types
        .iter()
        .find(|t| t.name == "Conversations")
        .expect("Conversations");
    assert!(
        !conversations.fields.iter().any(|f| f.name == "discord"),
        "Conversations carries no service collection"
    );

    // The bridge is a package of its own, with the service's records.
    let (bridge, _) = Replica::new("ada", Workspace::new(), &[(DISCORD, DISCORD_IDS)]);
    let bridge_meta = bridge
        .host
        .package("so.tilo.discord.preview")
        .expect("the Discord package")
        .meta;
    for name in [
        "Accounts", "Discord", "Mapping", "Link", "DChannel", "DRole",
    ] {
        assert!(
            bridge_meta.types.iter().any(|t| t.name == name),
            "the bridge package declares {name}"
        );
    }

    // A message from outside, as the scenario relays it: the author's picture, the service's
    // name, and the pictures its words carry.
    // The workspace's conversations root stands first, as a window opens it.
    let _ = ada.host.root_of(APP).unwrap();
    ada.commit();
    let scenario = meta
        .scenarios
        .iter()
        .find(|s| s.name == "a message from Discord")
        .expect("the scenario")
        .action
        .clone();
    let channel = match ada.host.runtime().invoke_action(
        &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        (scenario.as_str(), vec![]),
    ) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("the scenario runs: {other:?}"),
    };
    ada.commit();
    let mut view = ada.host.open_resource(&target(&channel), None).unwrap();
    let rows = rows(&mut view);
    let mut said: Vec<String> = rows.iter().flat_map(texts).collect();
    let nodes: Vec<tilolang_runtime::UiNode> = rows
        .iter()
        .flat_map(|row| {
            crate::fixtures::nodes(row)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect();
    said.extend(texts(view.tree()));
    assert!(
        said.iter()
            .any(|text| text.contains("wrote this in Discord")),
        "the outside author and the service: {said:?}"
    );
    let pictures: Vec<String> = nodes
        .into_iter()
        .filter(|node| node.kind == "ui.Avatar")
        .filter_map(|node| node.props.get("picture").cloned())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text),
            Value::Opt(Some(inner)) => match *inner {
                Value::Text(text) => Some(text),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert!(
        pictures
            .iter()
            .any(|picture| picture == "https://cdn.discordapp.com/avatars/1/abc.png"),
        "the author's picture: {pictures:?}"
    );
    assert!(
        pictures
            .iter()
            .any(|picture| picture == "https://cdn.discordapp.com/emojis/2.png"),
        "the emoji the message carries: {pictures:?}"
    );

    // The bridge mark is a generic conversation field, set by Chat's own verb.
    let Value::Ref(reference) = &channel else {
        panic!("a channel is a reference: {channel:?}");
    };
    let channel_ty = meta
        .types
        .iter()
        .find(|t| t.id == reference.target.ty)
        .expect("the channel's type");
    let service = channel_ty
        .fields
        .iter()
        .find(|f| f.name == "service")
        .expect("a service field");
    assert_eq!(
        ada.host
            .runtime()
            .read(&reference.target, &service.id)
            .unwrap(),
        Value::Text("Discord".to_owned()),
        "the channel says the service it is carried to"
    );
}
/// The id of Chat's view `name`, as a window opens one.
fn view_id(r: &Replica, name: &str) -> String {
    r.host
        .package(APP)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("the view {name}"))
        .id
        .clone()
}

/// The drafts the app holds, as the person looking reads them: another member's are not here.
fn drafts(r: &Replica) -> Vec<tilolang_runtime::ResourceRef> {
    match r.read(&r.root(APP), "drafts") {
        Value::List(items) => items.iter().map(target).collect(),
        other => panic!("the app's drafts: {other:?}"),
    }
}

/// What a draft holds, as it is written.
fn draft_said(r: &Replica, draft: &tilolang_runtime::ResourceRef) -> String {
    match r.read(draft, "body") {
        Value::Rich(rich) => rich.written_text(),
        other => panic!("a draft's body: {other:?}"),
    }
}

#[test]
fn a_draft_is_its_authors_alone_and_the_send_empties_it() {
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();

    // Opening the channel makes the draft Ada writes in, on this device and on her others.
    let view = open(&ada, &target(&design));
    ada.commit();
    let written = drafts(&ada);
    assert_eq!(written.len(), 1, "opening the channel made one draft");
    assert_eq!(draft_said(&ada, &written[0]), "", "and it is empty");

    // Another member reads no draft of hers: what a person is half-way through writing is
    // theirs until they send it. Opening the channel makes him one of his own, never hers.
    let ben = ada.fork("ben", &[(CHAT, &loaded[0].ids)]);
    ben.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
        ],
        None,
    );
    // What Ben reads of the drafts, by the rule the runtime reads them by for him.
    let ben_reads = || -> Vec<tilolang_runtime::ResourceRef> {
        let him = Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben"));
        drafts(&ben)
            .into_iter()
            .filter(|d| ben.host.runtime().may_read(&him, d))
            .collect()
    };
    assert!(ben_reads().is_empty(), "Ada's draft is not Ben's to read");
    let _ben_view = open(&ben, &target(&design));
    ben.commit();
    let his = ben_reads();
    assert_eq!(his.len(), 1, "Ben writes in a draft of his own");
    assert_ne!(
        his[0].id_string(),
        written[0].id_string(),
        "one person's draft is never another's"
    );

    // Sending names the draft: the message is what it holds, and one press empties it.
    let before = drafts(&ada);
    let input = view
        .tree()
        .first_of_kind("ui.RichInput")
        .expect("the composer")
        .key
        .clone();
    view.edit(&input, &rich("The build is up.")).unwrap();
    ada.commit();
    let message = call(
        &ada,
        "send",
        vec![
            design.clone(),
            rich(""),
            Value::none(),
            Value::List(Vec::new()),
            Value::text(""),
            Value::some(Value::resource(&before[0])),
        ],
    );
    ada.commit();
    assert_eq!(
        draft_said(&ada, &before[0]),
        "",
        "the send emptied the draft"
    );
    match ada.read(&target(&message), "body") {
        Value::Rich(rich) => assert_eq!(rich.written_text(), "The build is up."),
        other => panic!("the message says: {other:?}"),
    }
}

/// A conversation with an agent offers the model its answers come from, chosen there through
/// the same change Settings' Agent models pane makes, with what the answers here have cost so
/// far. A conversation between people offers no model: which one an agent answers with is not
/// theirs to choose.
/// The grant admitting `agent` to `workspace` as `person`'s agent: an agent is the pair of its
/// id and the person it acts for, and only that person's grants reach it.
fn admitted(workspace: &str, agent: &str, person: &str) -> tilo_action_policy::Grant {
    tilo_action_policy::Grant::new(
        format!("agent-member:{agent}"),
        tilo_action_policy::Grantee::agent(agent),
        tilo_action_policy::TreePath::workspace(workspace),
        tilo_action_policy::KindSet::READ_UNDOABLE,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(person),
    )
}

#[test]
fn a_conversation_with_an_agent_offers_the_model_its_answers_come_from() {
    let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace-model", "ada");
    policy
        .grants
        .push(admitted("workspace-model", "helper", "ada"));
    let (ada, _) = Replica::access(
        "ada",
        Workspace::new(),
        &[(CHAT, CHAT_IDS)],
        Some(tilo_action_policy::MemberAccess::person(policy, "ada")),
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let reading = tilolang_runtime::ResourceRef::parse_id(
        "settings/agent-models",
        "so.tilo.settings.sections#AgentModels",
    )
    .expect("the settings reading");
    ada.host
        .runtime()
        .set_reading(
            &reading,
            &serde_json::json!({
                "providers": [{
                    "id": "local",
                    "label": "On this device",
                    "enabled": true,
                    "models": [{"id": "small", "label": "Small model"}],
                    "model": "small",
                    "model_label": "Small model",
                }],
                "in_use": {"provider": "local", "model": "small", "model_label": "Small model"},
            })
            .to_string(),
        )
        .expect("the model choice is served");

    let helper = Actor::from(tilolang_tilo::runtime::ids::MemberId::agent("helper"));
    let direct = call(&ada, "message_person", vec![all, Value::Actor(helper)]);
    ada.commit();
    let view = open(&ada, &target(&direct));
    let switcher = view
        .tree()
        .first_of_kind("ui.PopoverButton")
        .expect("the model switcher")
        .clone();
    assert_eq!(
        text_prop(&switcher, "label"),
        Some("Small model".to_owned()),
        "the model its answers come from: {:?}",
        switcher.props
    );
    let shown: Vec<String> = texts(&switcher);
    for part in [
        "On this device",
        "Small model",
        "Answers so far: $0",
        "Undo the choice",
    ] {
        assert!(
            shown.iter().any(|text| text.contains(part)),
            "{part} stands in the switcher: {shown:?}"
        );
    }
}

/// What a person is half-way through writing stands under the conversation's row in the
/// sidebar, so a half-written message is found again rather than lost in a composer they have
/// to remember to open. Sending it empties the draft, and the line goes with it.
#[test]
fn the_sidebar_says_what_is_half_written_in_a_conversation() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();

    let view = open(&ada, &target(&design));
    let input = view
        .tree()
        .first_of_kind("ui.RichInput")
        .expect("the composer")
        .key
        .clone();
    view.edit(&input, &rich("Let us try\nthe second line"))
        .unwrap();
    ada.commit();

    // The row says the first line with text, under the channel's name.
    let sidebar = ada.view(APP);
    let row = sidebar
        .tree()
        .of_kind("ui.NavItem")
        .into_iter()
        .find(|item| item.props.get("label") == Some(&Value::text("design")))
        .expect("the channel's row")
        .clone();
    assert_eq!(
        text_prop(&row, "secondary"),
        Some("Draft: Let us try".to_owned()),
        "the row says what is being written: {:?}",
        row.props
    );

    // Sending empties the draft, and the row stops saying it.
    let send = button(view.tree(), "Send").expect("the composer's send");
    let sent = view.press(&send).unwrap();
    assert!(matches!(sent, InvokeOutcome::Done(_)), "sent: {sent:?}");
    ada.commit();
    let sidebar = ada.view(APP);
    let row = sidebar
        .tree()
        .of_kind("ui.NavItem")
        .into_iter()
        .find(|item| item.props.get("label") == Some(&Value::text("design")))
        .expect("the channel's row")
        .clone();
    assert_eq!(
        text_prop(&row, "secondary"),
        None,
        "an empty draft says nothing"
    );
}

#[test]
fn a_channels_name_topic_and_place_are_the_moderators_to_change() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
        ],
        None,
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(
        &ada,
        "new_channel",
        vec![all.clone(), Value::text("design")],
    );
    let team = call(&ada, "new_category", vec![all.clone(), Value::text("Team")]);
    // A category shows in the sidebar while it holds a channel.
    call(
        &ada,
        "new_channel",
        vec![
            all,
            Value::text("roadmap"),
            Value::text(""),
            Value::Opt(Some(Box::new(team.clone()))),
        ],
    );
    call(&ada, "send", vec![design.clone(), rich("Keep this.")]);
    ada.commit();

    // Renaming a channel or a category, saying what a channel is for, and setting it aside keep
    // everything said in it: the message is still there when it comes back.
    call(
        &ada,
        "rename_channel",
        vec![design.clone(), Value::text("design reviews")],
    );
    call(
        &ada,
        "set_topic",
        vec![design.clone(), Value::text("Reviews and critique")],
    );
    call(&ada, "rename_category", vec![team, Value::text("Product")]);
    let sidebar = ada.view(APP);
    let names: Vec<String> = sidebar
        .tree()
        .of_kind("ui.NavItem")
        .into_iter()
        .filter_map(|item| match item.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"design reviews".to_owned()), "{names:?}");
    let sections: Vec<String> = sidebar
        .tree()
        .of_kind("ui.NavSection")
        .into_iter()
        .filter_map(|section| match section.props.get("title") {
            Some(Value::Text(title)) => Some(title.clone()),
            _ => None,
        })
        .collect();
    assert!(sections.contains(&"Product".to_owned()), "{sections:?}");
    assert_eq!(name_of(&ada, &target(&design)), "design reviews");
    assert_eq!(messages(&ada, &target(&design)).len(), 1);

    // What the channel is for stands over its composer, and its row offers what is done to the
    // channel itself without opening it. A category's heading offers what is done to the group.
    let opened = open(&ada, &target(&design));
    let shown = texts(opened.tree());
    assert!(
        shown.contains(&"Reviews and critique".to_owned()),
        "the topic stands in the window: {shown:?}"
    );
    let sidebar = ada.view(APP);
    let row = sidebar
        .tree()
        .of_kind("ui.NavItem")
        .into_iter()
        .find(|item| item.props.get("label") == Some(&Value::text("design reviews")))
        .expect("the channel's row")
        .clone();
    let channel_menu: Vec<String> = row
        .of_kind("ui.MenuItem")
        .iter()
        .filter_map(|item| text_prop(item, "label"))
        .collect();
    for command in ["Rename channel", "Set the topic", "Archive channel"] {
        assert!(
            channel_menu.contains(&command.to_owned()),
            "{command} stands on the channel's row: {channel_menu:?}"
        );
    }
    // The channel's own page offers them too, and a moderator moves and deletes it there.
    let page_menu: Vec<String> = opened
        .tree()
        .of_kind("ui.MenuItem")
        .iter()
        .filter_map(|item| text_prop(item, "label"))
        .collect();
    for command in [
        "Rename channel",
        "Set the topic",
        "Move up",
        "Delete channel",
    ] {
        assert!(
            page_menu.contains(&command.to_owned()),
            "{command} stands on the channel's page: {page_menu:?}"
        );
    }
    let section = sidebar
        .tree()
        .of_kind("ui.NavSection")
        .into_iter()
        .find(|section| text_prop(section, "title") == Some("Product".to_owned()))
        .expect("the category's heading")
        .clone();
    let category_menu: Vec<String> = section
        .of_kind("ui.MenuItem")
        .iter()
        .filter_map(|item| text_prop(item, "label"))
        .collect();
    for command in ["Rename category", "Move up", "Delete category"] {
        assert!(
            category_menu.contains(&command.to_owned()),
            "{command} stands on the category's heading: {category_menu:?}"
        );
    }

    // Set aside, the channel leaves the sidebar and keeps its messages; restoring brings it back.
    call(
        &ada,
        "archive_channel",
        vec![design.clone(), Value::Bool(true)],
    );
    ada.commit();
    let sidebar = ada.view(APP);
    let shown = sidebar
        .tree()
        .of_kind("ui.NavItem")
        .into_iter()
        .filter_map(|item| match item.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        !shown.contains(&"design reviews".to_owned()),
        "set aside: {shown:?}"
    );
    assert_eq!(
        messages(&ada, &target(&design)).len(),
        1,
        "its messages stay"
    );
    call(
        &ada,
        "archive_channel",
        vec![design.clone(), Value::Bool(false)],
    );
    ada.commit();
    let sidebar = ada.view(APP);
    assert!(
        sidebar
            .tree()
            .of_kind("ui.NavItem")
            .into_iter()
            .any(|item| item.props.get("label") == Some(&Value::text("design reviews"))),
        "restoring puts it back"
    );

    // Only the workspace's moderators decide a channel's name, topic or side.
    let meta = ada.host.package(APP).unwrap().meta;
    let as_ben = |action: &str, args: Vec<Value>| {
        ada.host.runtime().invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ben")),
            (crate::fixtures::action_id(&meta, action).as_str(), args),
        )
    };
    for (action, args) in [
        ("rename_channel", vec![design.clone(), Value::text("mine")]),
        ("set_topic", vec![design.clone(), Value::text("mine")]),
        ("archive_channel", vec![design.clone(), Value::Bool(true)]),
    ] {
        let refused = as_ben(action, args);
        assert!(
            !matches!(refused, Ok(InvokeOutcome::Done(_))),
            "an editor's {action} is refused: {refused:?}"
        );
    }
}

#[test]
fn a_carried_message_shows_its_persona_and_a_pinned_one_stands_on_pins() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let carried = call(
        &ada,
        "relay",
        vec![
            design.clone(),
            rich("Morning, all."),
            Value::text("Robin"),
            Value::text("Discord"),
        ],
    );
    let own = call(&ada, "send", vec![design.clone(), rich("On my way.")]);
    call(&ada, "pin", vec![own.clone(), Value::Bool(true)]);
    ada.commit();

    // A message carried in from another service opens no run of the carrier: the account that
    // wrote it there stands in the message, with where it wrote.
    let mut channel = open(&ada, &target(&design));
    let shown_rows = rows(&mut channel);
    let authors: Vec<bool> = channel
        .tree()
        .of_kind("ui.ListItem")
        .into_iter()
        .map(|item| item.props.get("author").is_some())
        .collect();
    assert_eq!(
        authors,
        [false, true],
        "the carried message opens no run; Ada's own does"
    );
    let carried_texts = texts(&shown_rows[0]);
    for part in ["Robin wrote this in Discord", "Morning, all."] {
        assert!(
            carried_texts.contains(&part.to_owned()),
            "{part} in {carried_texts:?}"
        );
    }

    // The channel's pins stand on a page of their own, with what was pinned.
    let pins_view = view_id(&ada, "pins");
    let mut pins = ada
        .host
        .open_resource(&target(&design), Some(&pins_view))
        .unwrap();
    pins.poll_patches().unwrap();
    let shown = texts(pins.tree());
    assert!(
        shown.contains(&"On my way.".to_owned()),
        "the pin: {shown:?}"
    );
    assert!(
        !shown.contains(&"Morning, all.".to_owned()),
        "and only what was pinned: {shown:?}"
    );
    drop(carried);
}

#[test]
fn the_workspaces_people_are_a_page_of_their_own() {
    // A workspace with Ben and an agent in it: a host with a policy serves the membership it
    // decides by.
    let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace-people", "ada");
    policy
        .members
        .insert("ben".to_owned(), tilo_action_policy::Role::Editor);
    policy
        .grants
        .push(admitted("workspace-people", "helper", "ada"));
    let (ada, _) = Replica::access(
        "ada",
        Workspace::new(),
        &[(CHAT, CHAT_IDS)],
        Some(tilo_action_policy::MemberAccess::person(policy, "ada")),
    );
    let all = ada.host.root_of(APP).unwrap();
    ada.commit();

    // The sidebar keeps its channels in sight and offers the people on a page: a workspace with
    // many members does not push its conversations off the screen.
    let sidebar = ada.view(APP);
    let labels: Vec<String> = sidebar
        .tree()
        .of_kind("ui.NavItem")
        .into_iter()
        .filter_map(|item| match item.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert!(labels.contains(&"Everyone".to_owned()), "{labels:?}");
    assert!(
        !labels.contains(&"Ben".to_owned()),
        "no member per row: {labels:?}"
    );

    // The page lists the membership, and an agent is marked as one.
    let people_view = view_id(&ada, "people");
    let mut people = ada.host.open_resource(&all, Some(&people_view)).unwrap();
    people.poll_patches().unwrap();
    let shown = texts(people.tree());
    for part in ["ada", "ben", "helper", "Agent"] {
        assert!(shown.contains(&part.to_owned()), "{part} in {shown:?}");
    }
}

/// A file a message carries whose content the host stopped keeping: the poster reads why on the
/// attachment, naming the limit and who can raise it, and the status goes when the limit is
/// raised and the content kept again. The record the message lives in is not touched: the file
/// still opens from the device that posted it.
#[test]
fn a_refusal_shows_on_the_attachment_and_clears_when_the_limit_is_raised() {
    use tilolang_tilo::content::{RefusedContent, WorkspaceContent};

    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let hash = "ab".repeat(32);
    let file = Value::file(tilolang_runtime::blob::FileValue::new(
        "site-plan.pdf",
        "application/pdf",
        30 * 1_048_576,
        hash.clone(),
    ));
    call(
        &ada,
        "send",
        vec![
            design.clone(),
            rich("The plan."),
            Value::none(),
            Value::List(vec![file]),
        ],
    );
    ada.commit();
    let mut view = open(&ada, &target(&design));
    let kept: Vec<String> = rows(&mut view).iter().flat_map(texts).collect();
    assert!(
        !kept.iter().any(|text| text.contains("didn't keep")),
        "kept at first: {kept:?}"
    );

    let twenty_five = 25 * 1_048_576;
    // The host serves the root from what its device reads: the limits its owners set and the
    // content a limit refused. A reading that moved reaches the window on its next frame.
    let served = std::rc::Rc::new(std::cell::RefCell::new(Some(WorkspaceContent {
        file_bytes: twenty_five,
        member_bytes: 250 * 1_048_576,
        hourly_bytes: 250 * 1_048_576,
        refusals: vec![RefusedContent {
            record: "so.tilo.chat/message".to_owned(),
            hash: hash.clone(),
            bytes: 30 * 1_048_576,
            limit: "file".to_owned(),
            allowed: twenty_five,
            member: "ada".to_owned(),
            at: 1,
            raise: "owners".to_owned(),
        }],
        host_person: None,
    })));
    ada.host.set_content_source({
        let served = std::rc::Rc::clone(&served);
        std::rc::Rc::new(move || served.borrow().clone())
    });
    assert!(
        ada.host.follow_content(),
        "the reading reaches the host the frame reads"
    );
    view.poll_patches().unwrap();
    let shown: Vec<String> = rows(&mut view).iter().flat_map(texts).collect();
    assert!(
        shown
            .iter()
            .any(|text| text.contains("site-plan.pdf") && text.contains("25 MiB")),
        "the file and what the limit allowed: {shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|text| text.contains("owners can raise it")),
        "who can raise it: {shown:?}"
    );

    // A member who posted none of it reads none of it: the refusal is the poster's, and the
    // workspace's owners' and the host's person's.
    let (ben, _) = Replica::new("ben", ada.doc.fork(), &[(CHAT, CHAT_IDS)]);
    ben.host.set_content_source({
        let served = std::rc::Rc::clone(&served);
        std::rc::Rc::new(move || served.borrow().clone())
    });
    assert!(ben.host.follow_content());
    let mut theirs = open(&ben, &target(&design));
    let shown: Vec<String> = rows(&mut theirs).iter().flat_map(texts).collect();
    assert!(
        !shown.iter().any(|text| text.contains("didn't keep")),
        "a refusal another member posted is not theirs to read: {shown:?}"
    );

    // Raised: the limit allows it, the host keeps it again, and the refusal record is gone.
    *served.borrow_mut() = Some(WorkspaceContent {
        file_bytes: 50 * 1_048_576,
        member_bytes: 250 * 1_048_576,
        hourly_bytes: 250 * 1_048_576,
        refusals: Vec::new(),
        host_person: None,
    });
    assert!(
        ada.host.follow_content(),
        "the raised limit reaches the host the frame reads"
    );
    view.poll_patches().unwrap();
    let shown: Vec<String> = rows(&mut view).iter().flat_map(texts).collect();
    assert!(
        !shown.iter().any(|text| text.contains("didn't keep")),
        "the status clears: {shown:?}"
    );
    assert!(
        shown.iter().any(|text| text.contains("site-plan.pdf")),
        "the attachment stays: {shown:?}"
    );
}

/// A message a bridge carries from an account a member linked wears that member's face, and so
/// their colour, beside the account's name; one linked to someone outside the workspace is
/// refused with the reason.
#[test]
fn a_relayed_message_from_a_linked_account_wears_its_members_face() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
        ],
        None,
    );
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    let person = |name: &str| Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name));
    let relayed = |member: &str| {
        let meta = ada.host.package(APP).unwrap().meta;
        ada.host.runtime().invoke_action(
            &person("ada"),
            (
                crate::fixtures::action_id(&meta, "relay").as_str(),
                vec![
                    design.clone(),
                    rich("Shipped it."),
                    Value::text("benji"),
                    Value::text("Discord"),
                    Value::Int(0),
                    Value::List(Vec::new()),
                    Value::Opt(None),
                    Value::text(""),
                    Value::text("Discord"),
                    Value::List(Vec::new()),
                    Value::Opt(Some(Box::new(Value::Actor(person(member))))),
                ],
            ),
        )
    };
    let outsider = relayed("eve");
    assert!(
        format!("{outsider:?}")
            .contains("The person a relayed message is linked to is not in this workspace"),
        "a stranger is no member's account: {outsider:?}"
    );
    assert!(
        matches!(relayed("ben"), Ok(InvokeOutcome::Done(_))),
        "Ben's linked account is carried"
    );
    ada.commit();

    let mut channel = open(&ada, &target(&design));
    let row = rows(&mut channel).remove(0);
    let faces: Vec<Option<Value>> = row
        .of_kind("ui.Avatar")
        .into_iter()
        .map(|face| face.props.get("who").cloned())
        .collect();
    assert!(
        faces.iter().any(|who| match who {
            Some(Value::Actor(who)) => *who == person("ben"),
            Some(Value::Opt(Some(inner))) => **inner == Value::Actor(person("ben")),
            _ => false,
        }),
        "the message wears Ben's face: {faces:?}"
    );
    assert!(
        texts(&row).contains(&"benji wrote this in Discord".to_owned()),
        "beside the account's own name: {:?}",
        texts(&row)
    );
}

/// What an agent's model reasoned is written beside its answer as the answer comes, before the
/// answer has a word, and folds above it; Continue asks the agent to carry on from the answer,
/// and a person's own message has nothing to continue.
#[test]
fn an_answer_carries_its_reasoning_and_continues_where_it_stopped() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let design = call(&ada, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    // A turn's reply begins empty, naming the turn, and takes the model's reasoning before its
    // first word.
    let answer = call(
        &ada,
        "send",
        vec![
            design.clone(),
            rich(""),
            Value::Opt(None),
            Value::List(Vec::new()),
            Value::text("turn-1"),
        ],
    );
    call(
        &ada,
        "edit",
        vec![
            answer.clone(),
            rich(""),
            Value::text("turn-1"),
            Value::text("The person asks for the build; check the test machines first."),
        ],
    );
    call(
        &ada,
        "edit",
        vec![
            answer.clone(),
            rich("The build is on the test machines."),
            Value::text("turn-1"),
            Value::text("The person asks for the build; check the test machines first."),
        ],
    );
    ada.commit();

    let mut channel = open(&ada, &target(&design));
    let row = rows(&mut channel).remove(0);
    let reasoning = row
        .of_kind("ui.DisclosureRow")
        .into_iter()
        .find(|fold| text_prop(fold, "title") == Some("Reasoning".to_owned()))
        .unwrap_or_else(|| panic!("the reasoning folds above the answer: {:?}", texts(&row)));
    assert!(
        texts(reasoning)
            .contains(&"The person asks for the build; check the test machines first.".to_owned()),
        "{:?}",
        texts(reasoning)
    );

    // A person's message never carried reasoning, and an empty edit of it stays refused.
    let own = call(&ada, "send", vec![design.clone(), rich("Thanks.")]);
    let meta = ada.host.package(APP).unwrap().meta;
    let as_ada = |action: &str, args: Vec<Value>| {
        ada.host.runtime().invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (crate::fixtures::action_id(&meta, action).as_str(), args),
        )
    };
    let emptied = as_ada("edit", vec![own.clone(), rich("")]);
    assert!(
        format!("{emptied:?}").contains("Type a message first"),
        "{emptied:?}"
    );
    let refused = as_ada("continue_answer", vec![design.clone(), own]);
    assert!(
        format!("{refused:?}").contains("Only an agent's answer continues"),
        "{refused:?}"
    );

    // Continue is a reply to the answer, which the agent's next turn reads with it.
    let asked = call(
        &ada,
        "continue_answer",
        vec![design.clone(), answer.clone()],
    );
    ada.commit();
    match ada.read(&target(&asked), "reply_to") {
        Value::Opt(Some(replied)) => assert_eq!(
            target(&replied),
            target(&answer),
            "it answers the agent's message"
        ),
        other => panic!("it answers the agent's message: {other:?}"),
    }
}

/// A person is reported to the moderators for how they behave, not only for one message; a
/// moderator decides the report, and another overturns that decision, which opens it again. The
/// moderators read every report on a page of their own, and a member decides nothing.
#[test]
fn a_person_is_reported_and_a_decision_is_overturned_on_the_moderators_page() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    ada.host.runtime().set_members(
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            tilolang_runtime::members::Member::person("mo", tilo_actions::policy::Role::Moderator),
        ],
        None,
    );
    let root = ada.host.root_of(APP).unwrap();
    let all = Value::resource(&root);
    ada.commit();
    let meta = ada.host.package(APP).unwrap().meta;
    let person = |name: &str| Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name));
    let as_who = |who: &str, action: &str, args: Vec<Value>| {
        ada.host.runtime().invoke_action(
            &person(who),
            (crate::fixtures::action_id(&meta, action).as_str(), args),
        )
    };
    let done = |result: Result<InvokeOutcome, _>| match result {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("{other:?}"),
    };
    let refused = |result: Result<InvokeOutcome, _>, why: &str| {
        assert!(format!("{result:?}").contains(why), "`{why}`: {result:?}");
    };

    refused(
        as_who(
            "ben",
            "report_person",
            vec![all.clone(), Value::Actor(person("ben")), Value::text("me")],
        ),
        "You can't report yourself",
    );
    let report = done(as_who(
        "ben",
        "report_person",
        vec![
            all.clone(),
            Value::Actor(person("ada")),
            Value::text("Keeps talking over people in calls"),
        ],
    ));
    ada.commit();
    refused(
        as_who(
            "ben",
            "decide",
            vec![report.clone(), Value::text("Nothing")],
        ),
        "Only the workspace's moderators settle a report",
    );
    refused(
        as_who("mo", "overturn", vec![report.clone(), Value::text("Early")]),
        "Only a decided report can be overturned",
    );
    done(as_who(
        "mo",
        "decide",
        vec![report.clone(), Value::text("Talked it through")],
    ));
    ada.commit();
    refused(
        as_who("ben", "overturn", vec![report.clone(), Value::text("No")]),
        "Only the workspace's moderators overturn a decision",
    );
    done(as_who(
        "ada",
        "overturn",
        vec![report.clone(), Value::text("It happened again")],
    ));
    ada.commit();

    let reports_view = view_id(&ada, "reports");
    let mut page = ada.host.open_resource(&root, Some(&reports_view)).unwrap();
    page.poll_patches().unwrap();
    let shown = texts(page.tree());
    for part in [
        "Reported: Keeps talking over people in calls",
        "Overturned: It happened again",
        "Decide",
    ] {
        assert!(shown.contains(&part.to_owned()), "{part} in {shown:?}");
    }
}
