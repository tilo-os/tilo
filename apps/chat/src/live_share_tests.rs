//! A window shared live: the message that carries it, who may end it, the
//! card a reader sees and what the desktop is asked to draw in it.

use crate::testing::MountedChat;
use serde_json::json;
use ui::framework::testing::TestApp;
use ui::tilo_actions::{Acting, ActionInvoker};
use ui::window_share::{EmbedState, OpenWindow};

use crate::app::Hit;
use crate::chat::ShareAccess;
use crate::resource::ChatShell;
use crate::testing::{chain, message_item, mounted_rows};

fn share(app: &mut TestApp<ChatShell>, access: Option<&str>) -> crate::model::Message {
    share_as(app, None, access)
}

/// The window `sharer` shares, or the person at this window where nobody is named.
fn share_as(
    app: &mut TestApp<ChatShell>,
    sharer: Option<ActionInvoker>,
    access: Option<&str>,
) -> crate::model::Message {
    let mut arguments = json!({
        "channel": "c_chat",
        "attached": {"app": "so.tilo.notes", "record": "plan",
            "kind": "notes.workspace", "id": "plan", "label": "Plan"},
    });
    if let Some(access) = access {
        arguments["access"] = json!(access);
    }
    match sharer {
        Some(sharer) => app
            .call_by(sharer, &chain(), "share-window", arguments)
            .map(drop),
        None => app.call_on(&chain(), "share-window", arguments).map(drop),
    }
    .expect("the window is shared");
    app.settle();
    mounted_rows(app, "c_chat")
        .into_iter()
        .rfind(|m| m.share.is_some())
        .expect("the share is posted")
}

fn notes_window() -> OpenWindow {
    OpenWindow {
        tab: 7,
        app: "so.tilo.notes".into(),
        app_title: "Notes".into(),
        title: "Plan".into(),
        record: "plan".into(),
        kind: "notes.workspace".into(),
        // The note the window shows, not the whole notebook.
        item: Some("notes.workspace/pages/p1".into()),
    }
}

fn hits(app: &mut TestApp<ChatShell>) -> Vec<String> {
    app.chat()
        .hits
        .iter()
        .map(|(_, hit)| hit.semantic_id())
        .collect()
}

#[test]
fn a_shared_window_is_a_live_message_only_its_sharer_changes_it() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let shared = share(&mut app, None);
    let live = shared.share.clone().unwrap();
    assert_eq!(
        live.access,
        ShareAccess::View,
        "view only unless the sharer says so"
    );
    assert!(live.open && !live.ended);
    assert_eq!(
        shared.text, "",
        "the card says what it is; no text is made up"
    );
    let at = shared.resource().expect("it names its resource");
    assert_eq!(
        (at.app.as_str(), at.record.as_deref()),
        ("so.tilo.notes", Some("plan"))
    );
    assert_eq!(at.reference.label, "Plan");
    let editable = share(&mut app, Some("edit"));
    assert_eq!(editable.share.unwrap().access, ShareAccess::Edit);

    // Someone else neither ends it nor says whether it is open.
    let other = ActionInvoker::person("u_bob");
    for (action, arguments) in [
        ("stop-sharing", json!({})),
        ("share-open", json!({"open": false})),
        ("set-share-access", json!({"access": "edit"})),
    ] {
        assert!(
            app.call_by(other.clone(), &message_item(&shared.id), action, arguments)
                .is_err(),
            "{action} is the sharer's"
        );
    }

    app.call_on(
        &message_item(&shared.id),
        "share-open",
        json!({"open": false}),
    )
    .unwrap();
    app.settle();
    let closed = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .find(|m| m.id == shared.id)
        .unwrap();
    assert!(!closed.share.unwrap().open, "its window was closed");

    // Its sharer switches what the others may do in place, the share live.
    app.call_on(
        &message_item(&shared.id),
        "set-share-access",
        json!({"access": "edit"}),
    )
    .unwrap();
    app.settle();
    let switched = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .find(|m| m.id == shared.id)
        .unwrap()
        .share
        .unwrap();
    assert_eq!(switched.access, ShareAccess::Edit);
    assert!(!switched.ended, "switching keeps the share live");
}

/// Ending is one way: an ended share is not switched, and asking for its
/// window to open again shares nothing.
#[test]
fn an_ended_share_is_not_switched_and_reopening_shares_nothing() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let shared = share(&mut app, None);
    app.call_on(&message_item(&shared.id), "stop-sharing", json!({}))
        .unwrap();
    assert!(
        app.call_on(
            &message_item(&shared.id),
            "set-share-access",
            json!({"access": "view"}),
        )
        .is_err(),
        "an ended share is switched no more"
    );
    app.call_on(
        &message_item(&shared.id),
        "share-open",
        json!({"open": true}),
    )
    .unwrap();
    app.settle();
    let ended_message = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .find(|m| m.id == shared.id)
        .unwrap();
    let label = app.chat().model_message_label(&ended_message);
    assert!(
        label.ends_with("Plan\nNo longer shared"),
        "a screen reader hears it ended: {label}"
    );
    let ended = ended_message.share.unwrap();
    assert!(ended.ended);
    assert!(
        !ended.open,
        "reopening the window shares nothing once it ended"
    );
}

#[test]
fn a_window_is_shared_whole_from_its_resource_into_a_channel_that_takes_posts() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    for arguments in [
        // A part of a resource is a reference, not a window.
        json!({"channel": "c_chat",
            "attached": {"app": "so.tilo.notes", "record": "plan",
                "kind": "notes.workspace", "id": "plan", "label": "Plan",
                "span": {"start": 1, "end": 4, "unit": "Chars"}}}),
        // A window lives in a resource; a path or a link is not one.
        json!({"channel": "c_chat",
            "attached": {"app": "/home/ada/plan.md", "record": "plan",
                "kind": "notes.workspace", "id": "plan", "label": "Plan"}}),
        json!({"channel": "c_voice",
            "attached": {"app": "so.tilo.notes", "record": "plan",
                "kind": "notes.workspace", "id": "plan", "label": "Plan"}}),
    ] {
        assert!(app.call_on(&chain(), "share-window", arguments).is_err());
    }
    app.set_role(ui::permissions::Role::Viewer);
    assert!(
        app.call_on(
            &chain(),
            "share-window",
            json!({"channel": "c_chat",
                "attached": {"app": "so.tilo.notes", "record": "plan",
                    "kind": "notes.workspace", "id": "plan", "label": "Plan"}})
        )
        .is_err(),
        "who may not post may not share"
    );
    app.settle();
    assert!(
        mounted_rows(&mut app, "c_chat")
            .iter()
            .all(|m| m.share.is_none())
    );
}

/// The card asks this window's desktop to draw the resource's own app in
/// it, opens the resource on a press, and once ended asks for nothing and
/// opens nothing.
#[test]
fn the_card_embeds_the_resource_its_desktop_draws_and_ends_as_a_neutral_card() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let window = ui::hosted_window::HostedWindow::new();
    let _hosted = ui::window_toolbar::enter(Some(&window));
    ui::window_share::set_drawn(true);
    let shared = share(&mut app, None);
    app.harness().resize((1280, 900)).unwrap();
    app.settle();
    let embeds = window.embeds();
    assert_eq!(embeds.len(), 1, "{embeds:?}");
    let embed = &embeds[0];
    assert_eq!(embed.key, shared.id);
    assert_eq!(embed.view.app, "so.tilo.notes");
    assert_eq!(embed.view.record, "plan");
    let aspect = embed.rect.width() / embed.rect.height();
    assert!(
        (aspect - crate::live_share::ASPECT).abs() < 0.01,
        "{aspect}"
    );
    assert!(embed.rect.width() <= 440.0);
    assert!(embed.clip.contains(embed.rect.center()));

    window.set_embed_state(&shared.id, EmbedState::Showing);
    let found = hits(&mut app);
    assert!(found.contains(&Hit::OpenShare(shared.id.clone()).semantic_id()));
    assert!(
        found.contains(&Hit::StopShare(shared.id.clone()).semantic_id()),
        "the sharer stops it from the message"
    );
    assert!(
        app.harness()
            .a11y_node_named("Open Plan, shared by You")
            .is_some()
            || app.harness().a11y_node_named("Stop sharing").is_some()
    );

    // The sharer switches what the others may do from the card, in place.
    let switch = Hit::SwitchShare(shared.id.clone(), ShareAccess::Edit).semantic_id();
    let edit_rect = app
        .chat()
        .hits
        .iter()
        .find(|(_, hit)| hit.semantic_id() == switch)
        .map(|(rect, _)| *rect)
        .expect("the card offers Can edit to its sharer");
    assert!(
        found.contains(&Hit::SwitchShare(shared.id.clone(), ShareAccess::View).semantic_id()),
        "and View only beside it"
    );
    app.harness()
        .click_at(edit_rect.center().x, edit_rect.center().y);
    app.settle();
    let switched = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .find(|m| m.id == shared.id)
        .unwrap()
        .share
        .unwrap();
    assert_eq!(switched.access, ShareAccess::Edit, "the press switched it");
    assert!(!switched.ended);

    // The head's own control opens it as a window. Nothing serves opening
    // here, so it says why.
    let open = Hit::OpenShare(shared.id.clone()).semantic_id();
    let open_rect = app
        .chat()
        .hits
        .iter()
        .rev()
        .find(|(_, hit)| hit.semantic_id() == open)
        .map(|(rect, _)| *rect)
        .expect("the head offers the window");
    assert!(
        open_rect.width() < 60.0,
        "the control in the head, not the card: {open_rect:?}"
    );
    app.harness()
        .click_at(open_rect.center().x, open_rect.center().y);
    assert!(
        open_settles(&mut app, &shared.id),
        "the press reached the control"
    );

    app.call_on(&message_item(&shared.id), "stop-sharing", json!({}))
        .unwrap();
    app.settle();
    app.settle();
    assert!(
        window.embeds().is_empty(),
        "an ended share shows nothing live"
    );
    let found = hits(&mut app);
    assert!(!found.contains(&Hit::OpenShare(shared.id.clone()).semantic_id()));
    assert!(!found.contains(&Hit::StopShare(shared.id).semantic_id()));
    ui::window_share::set_drawn(false);
}

/// What the desktop asks of Chat: share this window, stop sharing that
/// one; and what Chat tells it back: which windows it shares, and whether
/// a shared window is still open.
#[test]
fn the_desktop_shares_and_stops_through_chat_and_hears_what_is_shared() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let window = ui::hosted_window::HostedWindow::new();
    let _hosted = ui::window_toolbar::enter(Some(&window));
    ui::window_share::set_drawn(true);
    ui::window_share::publish_windows(vec![notes_window()]);
    app.chat().active_channel = "c_chat".into();

    // "Share to channel…" from the window's menu opens the panel on it.
    ui::window_share::ask_to_share(notes_window());
    app.settle();
    let panel = app.chat().live_shares.panel.clone();
    assert_eq!(
        panel.map(|panel| (panel.chosen, panel.access)),
        Some((Some(7), ShareAccess::View))
    );
    app.chat().choose_share_access(ShareAccess::Edit);
    let found = hits(&mut app);
    assert!(found.contains(&Hit::SharePick(7).semantic_id()));
    assert!(found.contains(&Hit::ShareChosen.semantic_id()));
    let share = app
        .harness()
        .a11y_node_named("Share")
        .expect("the panel's Share button");
    app.harness()
        .perform_a11y_action(share, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    let shared = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .rfind(|m| m.share.is_some())
        .expect("shared from the panel");
    assert_eq!(shared.share.as_ref().unwrap().access, ShareAccess::Edit);
    let at = shared.resource().expect("it names its resource");
    assert_eq!(
        (at.app.as_str(), at.record.as_deref()),
        ("so.tilo.notes", Some("plan"))
    );
    let reference = &at.reference;
    assert_eq!(
        (reference.id.as_str(), reference.kind.as_str()),
        ("notes.workspace/pages/p1", "notes.workspace.pages"),
        "the note the window shows is what is shared"
    );
    assert!(app.chat().live_shares.panel.is_none());
    app.settle();
    let told = ui::window_share::shared();
    assert_eq!(told.len(), 1);
    assert_eq!(
        (told[0].app.as_str(), told[0].record.as_str()),
        ("so.tilo.notes", "plan")
    );
    assert_eq!(told[0].place, "#chat");

    // Closing the window says so on the message; the card stops being live.
    ui::window_share::publish_windows(Vec::new());
    app.settle();
    app.settle();
    let closed = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .find(|m| m.id == shared.id)
        .unwrap();
    assert!(!closed.share.unwrap().open);

    // "Stop sharing" on the window ends it.
    ui::window_share::ask_to_stop(shared.id.clone());
    app.settle();
    app.settle();
    let ended = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .find(|m| m.id == shared.id)
        .unwrap();
    assert!(ended.share.unwrap().ended);
    assert!(ui::window_share::shared().is_empty());
    ui::window_share::set_drawn(false);
}

/// Whether opening what `message` shares came to an outcome within a
/// bounded wait: the app it asks answers on a later frame, and how many
/// frames later is the machine's to say.
fn open_settles(app: &mut TestApp<ChatShell>, message: &str) -> bool {
    for _ in 0..400 {
        app.settle();
        if app.chat().resource_navigation.status(message, 0).is_some() {
            return true;
        }
        let _ = app
            .harness()
            .wait_until(ui::time::Duration::from_millis(10), |_| false);
    }
    false
}

/// The rect of the control that does `hit`, the one drawn last.
fn hit_rect(app: &mut TestApp<ChatShell>, hit: &Hit) -> Option<ui::kurbo::Rect> {
    let wanted = hit.semantic_id();
    app.chat()
        .hits
        .iter()
        .rev()
        .find(|(_, hit)| hit.semantic_id() == wanted)
        .map(|(rect, _)| *rect)
}

/// Where the desktop draws views, a press on a live card grows the view in
/// place to a size to work at and asks the desktop for the resource's app
/// there, taking input; the head makes it smaller again. What is grown is
/// how this window shows the share, never written to the message.
#[test]
fn a_press_grows_the_view_to_work_in_where_the_conversation_shows_it() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let window = ui::hosted_window::HostedWindow::new();
    let _hosted = ui::window_toolbar::enter(Some(&window));
    ui::window_share::set_drawn(true);
    let shared = share(&mut app, Some("edit"));
    app.harness().resize((1280, 900)).unwrap();
    app.settle();
    let miniature = window.embeds().into_iter().next().expect("the miniature");
    assert!(!miniature.working, "a glance until it is pressed");
    assert!(miniature.rect.width() <= 440.0);
    assert!(
        hit_rect(&mut app, &Hit::ShrinkShare(shared.id.clone())).is_none(),
        "nothing to make smaller yet"
    );
    // The card is named by what a press does, with its sharer's name as
    // everyone reads it.
    let press = app.chat().grow_share_label(&shared);
    assert!(
        press.starts_with("Work on Plan here, shared by "),
        "{press}"
    );
    assert!(
        app.harness().a11y_node_named(&press).is_some(),
        "the card says what a press does: {press}"
    );

    let middle = miniature.rect.center();
    app.harness().click_at(middle.x, middle.y);
    app.settle();
    app.settle();
    let grown = window.embeds().into_iter().next().expect("the view");
    assert!(grown.working, "the desktop is asked for a view to work in");
    assert_eq!(grown.key, shared.id);
    assert_eq!(grown.label, "Plan", "named for a screen reader");
    assert!(
        grown.rect.width() > 2.0 * miniature.rect.width().min(300.0) && grown.rect.width() > 440.0,
        "it takes the conversation's column: {:?}",
        grown.rect
    );
    assert!(
        grown.rect.height() >= 280.0 && grown.rect.height() <= 0.62 * 900.0 + 1.0,
        "a height to work at that leaves the conversation in reach: {:?}",
        grown.rect
    );
    let after = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .find(|m| m.id == shared.id)
        .unwrap();
    assert_eq!(
        after.share, shared.share,
        "growing it changed nothing in the message"
    );

    // A press in the view is the view's: Chat makes nothing of it.
    assert!(
        !app.chat()
            .hits
            .iter()
            .any(|(rect, hit)| rect.contains(grown.rect.center())
                && matches!(hit, Hit::GrowShare(_) | Hit::OpenShare(_))),
        "the card is no button while it is worked in"
    );
    assert!(app.harness().a11y_node_named("Make Plan smaller").is_some());
    assert!(
        hit_rect(&mut app, &Hit::OpenShare(shared.id.clone())).is_some(),
        "the window is still one press away"
    );

    // A popover over the conversation hides the view and keeps it running.
    app.chat().open_share_panel(None);
    app.settle();
    let covered = window.embeds();
    assert_eq!(covered.len(), 1, "{covered:?}");
    assert!(covered[0].working && covered[0].clip.is_zero_area());
    app.chat().live_shares.panel = None;
    app.settle();
    assert!(!window.embeds()[0].clip.is_zero_area());

    let smaller = hit_rect(&mut app, &Hit::ShrinkShare(shared.id.clone())).expect("Make smaller");
    app.harness()
        .click_at(smaller.center().x, smaller.center().y);
    app.settle();
    app.settle();
    let again = window.embeds().into_iter().next().expect("the miniature");
    assert!(!again.working && again.rect.width() <= 440.0);

    // A share that ended is grown no more.
    app.harness().click_at(middle.x, middle.y);
    app.settle();
    assert!(window.embeds()[0].working);
    app.call_on(&message_item(&shared.id), "stop-sharing", json!({}))
        .unwrap();
    app.settle();
    app.settle();
    assert!(window.embeds().is_empty());
    assert!(app.chat().live_shares.grown.is_empty());
    ui::window_share::set_drawn(false);
}

/// Where no desktop draws views a press on the card opens the resource as a
/// window, as before, and the head offers no control of its own.
#[test]
fn where_no_desktop_draws_views_a_press_opens_the_window() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let window = ui::hosted_window::HostedWindow::new();
    let _hosted = ui::window_toolbar::enter(Some(&window));
    ui::window_share::set_drawn(false);
    let shared = share(&mut app, None);
    app.harness().resize((1280, 900)).unwrap();
    app.settle();
    let card = hit_rect(&mut app, &Hit::OpenShare(shared.id.clone())).expect("the card opens it");
    assert!(card.width() > 200.0, "the card itself: {card:?}");
    assert!(hit_rect(&mut app, &Hit::GrowShare(shared.id.clone())).is_none());
    app.harness().click_at(card.center().x, card.center().y);
    assert!(
        open_settles(&mut app, &shared.id),
        "the press asked for the window"
    );
    assert!(app.chat().live_shares.grown.is_empty());
}

/// Someone the share lets only look reads it where the conversation shows
/// it: the card says so, and what they may do there stays the workspace's.
#[test]
fn a_view_only_share_is_read_in_place() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let window = ui::hosted_window::HostedWindow::new();
    let _hosted = ui::window_toolbar::enter(Some(&window));
    ui::window_share::set_drawn(true);
    let other = ActionInvoker::person("u_bob");
    app.call_by(
        other,
        &chain(),
        "share-window",
        json!({"channel": "c_chat",
            "attached": {"app": "so.tilo.notes", "record": "plan",
                "kind": "notes.workspace", "id": "plan", "label": "Plan"}}),
    )
    .expect("someone else shares a window, view only");
    app.harness().resize((1280, 900)).unwrap();
    app.settle();
    let shared = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .rfind(|m| m.share.is_some())
        .expect("the share is posted");
    let label = app.chat().grow_share_label(&shared);
    assert!(
        label.starts_with("Read Plan here, shared by "),
        "a reader is told they read it: {label}"
    );
    ui::window_share::set_drawn(false);
}

/// An agent shares what it works on with nothing but the resource's address:
/// no window of that resource is open on any device, and the card still asks
/// the desktop for the resource's own app, live, to watch and to work in.
/// Nobody's desktop says whether "its window" is open, so the share stays
/// live until the agent, or the person it acts for, ends it.
#[test]
fn an_agent_shares_a_resource_with_no_window_of_it_open() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let window = ui::hosted_window::HostedWindow::new();
    let _hosted = ui::window_toolbar::enter(Some(&window));
    ui::window_share::set_drawn(true);
    ui::window_share::publish_windows(Vec::new());
    let agent = ActionInvoker::agent("44444444-4444-4444-8444-444444444444").acting_for("ada");
    app.call_by(
        agent,
        &chain(),
        "share-window",
        json!({"channel": "c_chat", "access": "edit",
            "text": "I am drafting the plan here.",
            "attached": {"app": "so.tilo.notes", "record": "plan",
                "kind": "notes.workspace", "id": "plan", "label": "Plan"}}),
    )
    .expect("an agent shares a window by its resource's address");
    app.harness().resize((1280, 900)).unwrap();
    app.settle();
    app.settle();
    let shared = mounted_rows(&mut app, "c_chat")
        .into_iter()
        .rfind(|m| m.share.is_some())
        .expect("the share is posted");
    assert_eq!(shared.text, "I am drafting the plan here.");
    assert!(
        !app.chat().is_me(&shared.author),
        "the message is the agent's own, not its person's"
    );
    let share = shared.share.unwrap();
    assert_eq!(share.access, ShareAccess::Edit);
    assert!(
        share.open && !share.ended,
        "live, and no desktop without its window took that back: {share:?}"
    );
    let embeds = window.embeds();
    assert_eq!(embeds.len(), 1, "{embeds:?}");
    assert_eq!(
        (embeds[0].view.app.as_str(), embeds[0].view.record.as_str()),
        ("so.tilo.notes", "plan"),
        "the desktop is asked for the resource's own app"
    );

    // Its person works in it where the conversation shows it.
    let middle = embeds[0].rect.center();
    app.harness().click_at(middle.x, middle.y);
    app.settle();
    app.settle();
    let grown = window.embeds().into_iter().next().expect("the view");
    assert!(grown.working && grown.label == "Plan");
    ui::window_share::set_drawn(false);
}

/// An agent of the person at the window, and another agent, by their ids.
const SCOUT: &str = "44444444-4444-4444-8444-444444444444";
const PLANNER: &str = "55555555-5555-4555-8555-555555555555";

/// The person at the window, by their id.
fn person_here(app: &mut TestApp<ChatShell>) -> String {
    app.chat()
        .me
        .id
        .person_id()
        .expect("a person is at the window")
        .to_owned()
}

/// The message `id` as the window holds it now.
fn held(app: &mut TestApp<ChatShell>, id: &str) -> crate::model::Message {
    app.settle();
    mounted_rows(app, "c_chat")
        .into_iter()
        .find(|m| m.id == id)
        .expect("the message stands")
}

/// A share an agent made for a person is that person's to end, themselves, and nobody
/// else's: another member, another member's agent, another agent of the same person, an
/// automation of theirs and an agent that acts for nobody are refused as before. The person
/// ends it and nothing more: what the others may do there, and whether its window is open,
/// stay its sharer's. The message stays, saying what was shared.
#[test]
fn a_person_ends_the_share_their_own_agent_made_and_nobody_else_does() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let person = person_here(&mut app);
    let shared = share_as(
        &mut app,
        Some(ActionInvoker::agent(SCOUT).acting_for(person.as_str())),
        Some("edit"),
    );
    assert!(
        !app.chat().is_me(&shared.author),
        "the share is the agent's own message, not its person's"
    );
    let item = message_item(&shared.id);
    for (who, invoker) in [
        ("another member", ActionInvoker::person("u_bob")),
        (
            "another member's agent",
            ActionInvoker::agent(PLANNER).acting_for("u_bob"),
        ),
        (
            "another agent of the same person",
            ActionInvoker::agent(PLANNER).acting_for(person.as_str()),
        ),
        (
            "an automation of the same person",
            ActionInvoker::automation("nightly").acting_for(person.as_str()),
        ),
        (
            "an agent that acts for nobody",
            ActionInvoker::agent(PLANNER),
        ),
    ] {
        assert!(
            app.call_by(invoker, &item, "stop-sharing", json!({}))
                .is_err(),
            "{who} does not end it"
        );
    }
    assert!(
        !held(&mut app, &shared.id).share.unwrap().ended,
        "a refused stop leaves it live"
    );
    for (action, arguments) in [
        ("share-open", json!({"open": false})),
        ("set-share-access", json!({"access": "view"})),
    ] {
        assert!(
            app.call_on(&item, action, arguments).is_err(),
            "{action} stays its sharer's"
        );
    }
    let before = held(&mut app, &shared.id).share.unwrap();
    assert_eq!(
        (before.access, before.open, before.ended),
        (ShareAccess::Edit, true, false),
        "what its person was refused changed nothing"
    );

    app.call_on(&item, "stop-sharing", json!({}))
        .expect("the person ends the share their own agent made for them");
    let ended = held(&mut app, &shared.id);
    let share = ended.share.clone().unwrap();
    assert!(
        share.ended && !share.open,
        "ended, and no longer open: {share:?}"
    );
    assert!(
        ended.resource().is_some(),
        "the message stays and says what was shared"
    );
}

/// What is not this person's own agent's share for them is not theirs to end: another
/// member's own share, one an agent made for another member, and one an agent made for
/// nobody.
#[test]
fn a_share_made_for_someone_else_or_for_nobody_is_not_theirs_to_end() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    person_here(&mut app);
    for (whose, sharer) in [
        ("of another member's own", ActionInvoker::person("u_bob")),
        (
            "an agent made for another member",
            ActionInvoker::agent(PLANNER).acting_for("u_bob"),
        ),
        ("an agent made for nobody", ActionInvoker::agent(PLANNER)),
    ] {
        let shared = share_as(&mut app, Some(sharer), None);
        assert!(
            app.call_on(&message_item(&shared.id), "stop-sharing", json!({}))
                .is_err(),
            "a share {whose} is not this person's to end"
        );
        assert!(
            !held(&mut app, &shared.id).share.unwrap().ended,
            "a share {whose} stays live"
        );
    }
}

/// Bob's agent answering this person works for them, bounded by Bob: a share it makes is
/// this person's to end, and not Bob's, who only bounds what it does.
#[test]
fn a_share_another_members_agent_made_answering_this_person_is_theirs_to_end() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let person = person_here(&mut app);
    let mut answering = Acting::agent(SCOUT);
    answering.acted_for = vec![Acting::person(person.as_str()), Acting::person("u_bob")];
    let shared = share_as(&mut app, Some(ActionInvoker::admitted(&answering)), None);
    let item = message_item(&shared.id);
    assert!(
        app.call_by(
            ActionInvoker::person("u_bob"),
            &item,
            "stop-sharing",
            json!({})
        )
        .is_err(),
        "the agent's owner does not end a share made for someone else"
    );
    app.call_on(&item, "stop-sharing", json!({}))
        .expect("the person the agent answered ends the share it made for them");
    assert!(held(&mut app, &shared.id).share.unwrap().ended);
}

/// The page's own Chat offers the way to end such a share to the person the agent made it
/// for, on its card and in the message's menu, and to nobody else: this person is offered
/// neither on a share another member's agent made. What the others may do there is offered
/// to no one but its sharer. A press on the card's control ends the share through Chat's
/// `stop-sharing`.
#[test]
fn the_card_offers_ending_an_agents_share_to_its_person_and_to_nobody_else() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let window = ui::hosted_window::HostedWindow::new();
    let _hosted = ui::window_toolbar::enter(Some(&window));
    ui::window_share::set_drawn(true);
    ui::window_share::publish_windows(Vec::new());
    let person = person_here(&mut app);
    let theirs = share_as(
        &mut app,
        Some(ActionInvoker::agent(SCOUT).acting_for(person.as_str())),
        Some("edit"),
    );
    let anothers = share_as(
        &mut app,
        Some(ActionInvoker::agent(PLANNER).acting_for("u_bob")),
        Some("edit"),
    );
    app.harness().resize((1280, 1800)).unwrap();
    app.settle();
    app.settle();
    let found = hits(&mut app);
    for shared in [&theirs, &anothers] {
        assert!(
            found.contains(&Hit::GrowShare(shared.id.clone()).semantic_id()),
            "both cards are drawn: {found:?}"
        );
        for access in [ShareAccess::View, ShareAccess::Edit] {
            assert!(
                !found.contains(&Hit::SwitchShare(shared.id.clone(), access).semantic_id()),
                "what the others may do is its sharer's alone to switch"
            );
        }
    }
    assert!(
        found.contains(&Hit::StopShare(theirs.id.clone()).semantic_id()),
        "the person an agent shared it for is offered to stop sharing it: {found:?}"
    );
    assert!(
        !found.contains(&Hit::StopShare(anothers.id.clone()).semantic_id()),
        "nobody else is offered it"
    );

    // The message's menu offers the same, to the same person.
    for (shared, offered) in [(&theirs, true), (&anothers, false)] {
        app.chat()
            .open_message_menu(shared.id.clone(), ui::kurbo::Point::new(40.0, 40.0));
        let stops = app
            .chat()
            .menu
            .as_ref()
            .is_some_and(|menu| menu.items.contains(&crate::app::MenuAction::StopSharing));
        assert_eq!(stops, offered, "the menu of {}", shared.id);
        app.chat().menu = None;
    }
    app.settle();

    // A press on the card's control ends it, through Chat's own verb.
    let stop = hit_rect(&mut app, &Hit::StopShare(theirs.id.clone())).expect("the control");
    app.harness().click_at(stop.center().x, stop.center().y);
    app.settle();
    app.settle();
    let ended = held(&mut app, &theirs.id).share.unwrap();
    assert!(ended.ended && !ended.open, "the press ended it: {ended:?}");
    assert!(
        !held(&mut app, &anothers.id).share.unwrap().ended,
        "the share made for someone else stays live"
    );
    assert!(
        !hits(&mut app).contains(&Hit::StopShare(theirs.id.clone()).semantic_id()),
        "an ended share offers nothing to stop"
    );
    ui::window_share::set_drawn(false);
}
