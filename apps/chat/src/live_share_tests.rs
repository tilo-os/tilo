//! A window shared live: the message that carries it, who may end it, the
//! card a reader sees and what the desktop is asked to draw in it.

use serde_json::json;
use ui::framework::testing::TestApp;
use ui::tilo_actions::ActionInvoker;
use ui::window_share::{EmbedState, OpenWindow};

use crate::app::{ChatApp, Hit};
use crate::record::ShareAccess;
use crate::resource::ChatShell;
use crate::testing::{chain, message_item, mounted_rows};

const PLAN: &str = "record:so.tilo.notes:plan";

fn share(app: &mut TestApp<ChatShell>, access: Option<&str>) -> crate::model::Message {
    let mut arguments = json!({
        "channel": "c_chat",
        "reference": {"kind": "notes.workspace", "id": "plan", "label": "Plan"},
        "source_provider": PLAN,
    });
    if let Some(access) = access {
        arguments["access"] = json!(access);
    }
    app.call_on(&chain(), "share-window", arguments)
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
    app.surface_mut::<ChatApp>()
        .hits
        .iter()
        .map(|(_, hit)| hit.semantic_id())
        .collect()
}

#[test]
fn a_shared_window_is_a_live_message_only_its_sharer_ends() {
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
    assert_eq!(shared.source_provider.as_deref(), Some(PLAN));
    assert_eq!(shared.reference.as_ref().unwrap().label, "Plan");
    let read = app
        .call_on(&message_item(&shared.id), "read-message", json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(
        read["share"]["access"], "view",
        "an agent reads what is shared and how"
    );

    let editable = share(&mut app, Some("edit"));
    assert_eq!(editable.share.unwrap().access, ShareAccess::Edit);

    // Someone else neither ends it nor says whether it is open.
    let other = ActionInvoker::actor("u_bob");
    for (action, arguments) in [
        ("stop-sharing", json!({})),
        ("share-open", json!({"open": false})),
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

    app.call_on(&message_item(&shared.id), "stop-sharing", json!({}))
        .unwrap();
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
    let label = app
        .surface_mut::<ChatApp>()
        .model_message_label(&ended_message);
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
fn a_window_is_shared_whole_from_its_record_into_a_channel_that_takes_posts() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    for arguments in [
        // A part of a resource is a reference, not a window.
        json!({"channel": "c_chat", "source_provider": PLAN,
            "reference": {"kind": "notes.workspace", "id": "plan", "label": "Plan",
                "span": {"start": 1, "end": 4, "unit": "Chars"}}}),
        // A window lives in a record; a path or a link is not one.
        json!({"channel": "c_chat", "source_provider": "/home/ada/plan.md",
            "reference": {"kind": "notes.workspace", "id": "plan", "label": "Plan"}}),
        json!({"channel": "c_voice", "source_provider": PLAN,
            "reference": {"kind": "notes.workspace", "id": "plan", "label": "Plan"}}),
    ] {
        assert!(app.call_on(&chain(), "share-window", arguments).is_err());
    }
    app.set_role(ui::permissions::Role::Viewer);
    assert!(
        app.call_on(
            &chain(),
            "share-window",
            json!({"channel": "c_chat", "source_provider": PLAN,
                "reference": {"kind": "notes.workspace", "id": "plan", "label": "Plan"}})
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

    // A press on the live view itself opens it: the view draws, but the
    // card takes the press. Nothing serves opening here, so it says why.
    let middle = embed.rect.center();
    app.harness().click_at(middle.x, middle.y);
    app.settle();
    assert!(
        app.surface_mut::<ChatApp>()
            .resource_navigation
            .status(&shared.id)
            .is_some(),
        "the press reached the card"
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
    app.surface_mut::<ChatApp>().active_channel = "c_chat".into();

    // "Share to channel…" from the window's menu opens the panel on it.
    ui::window_share::ask_to_share(notes_window());
    app.settle();
    let panel = app.surface_mut::<ChatApp>().live_shares.panel.clone();
    assert_eq!(
        panel.map(|panel| (panel.chosen, panel.access)),
        Some((Some(7), ShareAccess::View))
    );
    app.surface_mut::<ChatApp>()
        .choose_share_access(ShareAccess::Edit);
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
    assert_eq!(shared.source_provider.as_deref(), Some(PLAN));
    let reference = shared.reference.as_ref().unwrap();
    assert_eq!(
        (reference.id.as_str(), reference.kind.as_str()),
        ("notes.workspace/pages/p1", "notes.workspace.pages"),
        "the note the window shows is what is shared"
    );
    assert!(app.surface_mut::<ChatApp>().live_shares.panel.is_none());
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
