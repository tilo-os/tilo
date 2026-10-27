use super::*;
use crate::testing::item;
use ui::framework::testing::TestApp;
use ui::search::SearchProvider as _;
use ui::tilo_resource::Resource as _;

/// Outside a call, a voice channel offers joining it, not its controls:
/// search and menus must not offer turning a camera on with no call to show
/// it in, and an agent is told why.
#[test]
fn a_calls_own_controls_are_unavailable_until_you_join_it() {
    use ui::tilo_actions::Availability;
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let resources = app.harness().resources();
    let call = crate::testing::call_row("c_voice");
    let availability = |action: &str| {
        ui::tilo_actions::flatten(&resources)
            .into_iter()
            .find(|flat| flat.resource_id.as_str() == call && flat.action.id == action)
            .map(|flat| flat.action.contract.availability.clone())
    };
    assert!(matches!(
        availability("join-call"),
        Some(Availability::Available)
    ));
    for id in [
        "leave-call",
        "mute",
        "unmute",
        "deafen",
        "undeafen",
        "camera-on",
        "camera-off",
        "share-screen",
        "stop-sharing-screen",
    ] {
        let now = availability(id);
        assert!(
            matches!(&now, Some(Availability::Unavailable { code, .. }) if code == "not_in_call"),
            "{id}: {now:?}"
        );
    }
}

/// The row of sidebar place `id` where the shell's list paints it.
fn sidebar_row(app: &mut TestApp<ChatShell>, id: &str) -> vello::kurbo::Rect {
    app.with_widget::<ui::widget::NavList, _>(|list| list.row_rect(id))
        .flatten()
        .unwrap_or_else(|| panic!("{id} is a row of the sidebar"))
}

fn right_click(app: &mut TestApp<ChatShell>, row: vello::kurbo::Rect) {
    let harness = app.harness();
    harness.send(&ui::synthetic_input::cursor_moved(
        row.center().x,
        row.center().y,
    ));
    for state in [
        winit::event::ElementState::Pressed,
        winit::event::ElementState::Released,
    ] {
        harness.send(&ui::synthetic_input::mouse_input(
            state,
            winit::event::MouseButton::Right,
        ));
    }
    app.settle();
}

/// The room's channels are the shell's sidebar: choosing one opens it, a
/// right click on a channel or a category asks for its menu, and its
/// Rename names it in place there.
#[test]
fn the_rooms_channels_are_the_shells_sidebar() {
    let mut app = TestApp::<ChatShell>::capture(Some("channels")).unwrap();
    app.harness().resize((1280, 720)).unwrap();
    app.settle();
    let frontend = sidebar_row(&mut app, "channel:c_frontend");
    app.harness()
        .click_at(frontend.center().x, frontend.center().y);
    app.settle();
    assert_eq!(app.surface_mut::<ChatApp>().active_channel, "c_frontend");

    let random = sidebar_row(&mut app, "channel:c_random");
    right_click(&mut app, random);
    let target = app
        .surface_mut::<ChatApp>()
        .menu
        .as_ref()
        .map(|menu| menu.target.clone());
    assert_eq!(
        target,
        Some(crate::app::MenuTarget::Channel("c_random".into())),
        "a channel's row asks for the channel's menu"
    );
    // Its Rename, the second row, opens the name in place in the sidebar,
    // which takes the keyboard.
    for key in ["Down", "Down", "Enter"] {
        app.harness().key(key);
    }
    app.settle();
    let old = app
        .surface_mut::<ChatApp>()
        .channel("c_random")
        .map(|channel| channel.name.clone())
        .expect("the channel");
    app.harness().key("End");
    for _ in old.chars() {
        app.harness().key("Backspace");
    }
    app.harness().type_text("watercooler");
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        app.surface_mut::<ChatApp>()
            .channel("c_random")
            .map(|channel| channel.name.as_str()),
        Some("watercooler")
    );

    let category = app
        .surface_mut::<ChatApp>()
        .sidebar_categories()
        .first()
        .map(|category| category.id.clone())
        .expect("the fixture has a category");
    let header = sidebar_row(&mut app, &crate::sidebar::category_place(&category));
    right_click(&mut app, header);
    let target = app
        .surface_mut::<ChatApp>()
        .menu
        .as_ref()
        .map(|menu| menu.target.clone());
    assert_eq!(target, Some(crate::app::MenuTarget::Category(category)));
}

#[test]
fn shared_space_visible_stop_button_uses_the_same_consent_handler() {
    let mut app = TestApp::<ChatShell>::capture(Some("live-context")).unwrap();
    assert!(app.surface_mut::<crate::ChatApp>().sharing_context);
    app.settle();
    // The visible Stop sharing control is the window bar's slot; the desktop
    // hands its press to the app, which answers the same consent effect.
    let offers_stop = || {
        ui::window_toolbar::declared().is_some_and(|toolbar| {
            toolbar
                .actions
                .iter()
                .any(|action| action.id == "stop-sharing" && action.enabled)
        })
    };
    assert!(offers_stop(), "the window's bar shows Stop sharing");
    ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
        id: "stop-sharing".into(),
    });
    app.settle();
    assert!(!app.surface_mut::<crate::ChatApp>().sharing_context);
    assert!(!offers_stop(), "Stop sharing leaves the bar once stopped");
    assert!(ui::view::capability::view_state(app.harness().root_widget()).is_none());
}

#[test]
fn shared_space_context_consent_is_local_and_relaunch_starts_private() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    app.call("share-live-context", serde_json::json!({}))
        .unwrap();
    let state = app
        .call("live-context-status", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(state["sharing"], true);
    assert_eq!(state["videoCapture"], false);
    let view = ui::view::capability::view_state(app.harness().root_widget()).unwrap();
    assert!(
        view.view_mode.unwrap().starts_with("chat-context-v1:"),
        "the framework root forwards presence"
    );
    app.call("stop-sharing-live-context", serde_json::json!({}))
        .unwrap();
    assert!(ui::view::capability::view_state(app.harness().root_widget()).is_none());
    app.call("share-live-context", serde_json::json!({}))
        .unwrap();
    app = app.relaunch(&[]).unwrap();
    let state = app
        .call("live-context-status", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(
        state["sharing"], false,
        "consent is never restored from persistence"
    );
}

/// The `#chat` channel of a new chat: its item resource and itself.
fn general(app: &TestApp<ChatShell>) -> (String, crate::model::Channel) {
    let channel = app
        .state()
        .channel_rows()
        .into_iter()
        .find(|channel| channel.id == "c_chat")
        .unwrap();
    (item("channels", &channel.id), channel)
}

fn messages(app: &mut TestApp<ChatShell>) -> Vec<crate::model::Message> {
    let held = crate::testing::mounted_messages(app);
    let chat = app.state();
    chat.channel_rows()
        .iter()
        .flat_map(|channel| chat.message_rows(&held, &channel.id))
        .collect()
}

#[test]
fn reviewed_feedback_search_opens_the_report_and_triages_through_the_door() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let (resource, channel) = general(&app);
    let mut preview = ui::feedback::Draft {
        text: "The reader loses my position".into(),
        ..Default::default()
    }
    .preview()
    .unwrap();
    preview.approve();
    let submission = preview
        .submission(
            "0123456789abcdef0123456789abcdef".into(),
            ui::feedback::Destination {
                room: "room".into(),
                provider: "demo-chat".into(),
                channel: channel.id,
            },
        )
        .unwrap();
    for _ in 0..2 {
        app.call_on(
            &resource,
            "submit-feedback",
            serde_json::json!({"submission": submission}),
        )
        .unwrap();
    }
    app.settle();
    let state = app.state();
    assert_eq!(state.feedback.len(), 1);
    let report_id = item("feedback", &submission.id);
    let entries = Chat::search(&state);
    let source = ui::framework::testing::search_source::<Chat>(entries.clone());
    assert!(
        source
            .search("reader loses")
            .iter()
            .any(|hit| hit.title == submission.feedback.text),
        "system search finds the reviewed report"
    );
    let entry = entries
        .iter()
        .find(|entry| entry.anchor.as_deref() == Some(report_id.as_str()))
        .expect("the report's entry opens the report");
    app.call("open", serde_json::json!({ "anchor": entry.anchor }))
        .unwrap();
    app.settle();
    assert_eq!(
        app.surface_mut::<ChatApp>().revealed_message.as_deref(),
        Some(crate::feedback::message_id(&submission.id).as_str())
    );
    app.call_on(
        &report_id,
        "triage",
        serde_json::json!({"update": {"status": "investigating"}}),
    )
    .unwrap();
    app.settle();
    let report = app.read_document(|doc| {
        crate::feedback::report(doc, &submission.id)
            .unwrap()
            .unwrap()
    });
    assert_eq!(report.submission, submission);
    assert_eq!(report.triage.len(), 1);
    assert_eq!(report.triage[0].author, report.author);
    assert_eq!(
        report.triage[0].update,
        crate::feedback::TriageUpdate::Investigating
    );
}

#[test]
fn search_hits_open_the_channel_message_and_person_they_name() {
    let mut app = TestApp::<ChatShell>::capture(None).unwrap();
    // The record's entries, and the messages the window holds.
    let mut entries = Chat::search(&app.state());
    entries.extend(
        app.with_surface::<ChatApp, _>(|surface| {
            ui::framework::ResourceSurface::<Chat, crate::effects::ChatEffect>::search_entries(
                surface,
            )
        })
        .unwrap(),
    );
    let anchor = |query: &str, field: &str| {
        entries
            .iter()
            .filter(|entry| entry.title.contains(query) || entry.text.contains(query))
            .filter_map(|entry| entry.anchor.clone())
            .find(|anchor| anchor.starts_with(&format!("{}/{field}/", Chat::KIND)))
            .unwrap_or_else(|| panic!("search finds {field} for {query:?}"))
    };
    let frontend = anchor("frontend", "channels");
    app.call("open", serde_json::json!({ "anchor": frontend }))
        .unwrap();
    app.settle();
    assert_eq!(app.surface_mut::<ChatApp>().active_channel, "c_frontend");
    let concept = anchor("host tile keeps the live camera", "messages");
    app.call("open", serde_json::json!({ "anchor": concept }))
        .unwrap();
    app.settle();
    assert_eq!(
        app.surface_mut::<ChatApp>().revealed_message.as_deref(),
        Some("m_concept")
    );
    let hangout = anchor("Hangout", "categories");
    app.call("open", serde_json::json!({ "anchor": hangout }))
        .unwrap();
    app.settle();
    assert_eq!(app.surface_mut::<ChatApp>().active_channel, "c_ideas");
    let milo = anchor("Milo Park", "people");
    app.call("open", serde_json::json!({ "anchor": milo }))
        .unwrap();
    app.settle();
    assert_eq!(
        app.surface_mut::<ChatApp>().revealed_person.as_deref(),
        Some("u_milo")
    );
}

#[test]
fn resource_range_opens_and_highlights_current_text_through_the_public_view() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let _ = general(&app);
    app.call_on(
        &crate::testing::chain(),
        "post",
        serde_json::json!({"channel": "c_chat", "text": "café notes"}),
    )
    .unwrap();
    app.settle();
    let id = messages(&mut app)
        .into_iter()
        .find(|m| m.text == "café notes")
        .unwrap()
        .id;
    let message = item("messages", &id);
    app.call_on(
        &message,
        "reveal-message-range",
        serde_json::json!({"start": 3, "end": 4, "unit": "Chars"}),
    )
    .unwrap();
    app.settle();
    let surface = app.surface_mut::<crate::ChatApp>();
    let mut bodies = surface.body_views.borrow_mut();
    let (_, view) = bodies
        .get_mut(&(0, id.clone()))
        .expect("real layout produced the message body");
    assert_eq!(view.editor_mut().selected_text().as_deref(), Some("é"));
    drop(bodies);
    app.call_on(
        &crate::testing::message_item(&id),
        "edit-message",
        serde_json::json!({"text": "changed"}),
    )
    .unwrap();
    app.settle();
    let surface = app.surface_mut::<crate::ChatApp>();
    let mut bodies = surface.body_views.borrow_mut();
    let (_, view) = bodies.get_mut(&(0, id)).unwrap();
    assert!(
        view.editor_mut().selected_text().is_none(),
        "edits invalidate the old highlight"
    );
}

#[test]
fn shared_space_call_status_returns_a_report_and_never_claims_live_capture() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let status = app
        .call_on(
            &crate::testing::call_row("c_voice"),
            "call-status",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(status["refreshRequested"], true);
    assert_eq!(status["reported"]["capture_active"], false);
    assert_eq!(status["reported"]["muted"], true);
    assert_eq!(status["channel"], serde_json::Value::Null);
    assert!(messages(&mut app).is_empty());
}

#[test]
fn shared_space_resource_journey_preserves_address_author_and_viewer_boundary() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let _ = general(&app);
    let reference = serde_json::json!({
        "kind": "word_processor.block", "id": "word_processor.block:paragraph-7",
        "label": "Proposal", "span": {"start": 2, "end": 9, "unit": "Chars"}
    });
    app.call_on(
        &crate::testing::chain(),
        "share-resource",
        serde_json::json!({"channel": "c_chat", "reference": reference, "source_provider": "record:so.tilo.word-processor:proposal", "text": "Discuss this paragraph"}),
    )
    .unwrap();
    app.settle();
    let message = messages(&mut app)
        .into_iter()
        .find(|m| m.text == "Discuss this paragraph")
        .unwrap();
    assert_eq!(serde_json::to_value(&message.reference).unwrap(), reference);
    let target = item("messages", &message.id);
    let result = app
        .call_on(&target, "read-message", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(result["reference"], reference);
    assert_eq!(
        result["sourceProvider"],
        "record:so.tilo.word-processor:proposal"
    );
    assert_eq!(result["author"], message.author);
    app.set_role(ui::permissions::Role::Viewer);
    assert!(
        app.call_on(
            &crate::testing::chain(),
            "share-resource",
            serde_json::json!({"channel": "c_chat", "reference": reference})
        )
        .is_err()
    );
    assert_eq!(messages(&mut app).len(), 1);
    let read = app
        .call_on(&target, "read-message", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(
        read["reference"], reference,
        "viewers can inspect an address, with no grant to its target"
    );
}

#[test]
fn shared_space_invalid_resource_never_posts_and_voice_has_no_timeline() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let _ = general(&app);
    for id in [
        "/home/person/private",
        "https://example.test",
        "block\nsecret",
        "",
    ] {
        assert!(
            app.call_on(
                &crate::testing::chain(),
                "share-resource",
                serde_json::json!({"channel": "c_chat",
                    "reference": {"kind": "word_processor.block", "id": id, "label": "Proposal"}
                })
            )
            .is_err()
        );
    }
    assert!(
        app.call_on(
            &crate::testing::chain(),
            "share-resource",
            serde_json::json!({"channel": "c_voice",
                "reference": {"kind": "word_processor.block", "id": "block-7", "label": "Proposal"}
            })
        )
        .is_err()
    );
    assert!(messages(&mut app).is_empty());
}

#[test]
fn a_new_room_record_opens_with_chat_and_voice_and_nothing_else() {
    let app = TestApp::<ChatShell>::new().unwrap();
    let state = app.state();
    assert_eq!(state.people.len(), 1, "only its creator");
    assert_eq!(
        state
            .channel_rows()
            .iter()
            .map(|channel| (channel.name.clone(), channel.kind))
            .collect::<Vec<_>>(),
        [
            ("chat".to_owned(), crate::model::ChannelKind::Text),
            ("voice".to_owned(), crate::model::ChannelKind::Voice),
        ],
        "exactly one text channel #chat and one voice channel #voice"
    );
    assert!(
        state.categories.is_empty(),
        "no categories unless someone makes one"
    );
    let mut app = app;
    assert!(
        crate::testing::mounted_messages(&mut app).is_empty(),
        "no fabricated conversation"
    );
}

/// The root's action `id`, as the window publishes it now.
fn root_action(app: &mut TestApp<ChatShell>, id: &str) -> ui::tilo_actions::ActionSpec {
    fn find(
        resources: &[ui::tilo_actions::Resource],
        id: &str,
    ) -> Option<ui::tilo_actions::ActionSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == Chat::KIND {
                resource
                    .actions
                    .iter()
                    .find(|action| action.id == id)
                    .cloned()
            } else {
                find(&resource.children, id)
            }
        })
    }
    find(&app.harness().resources(), id).unwrap_or_else(|| panic!("the Chat root publishes {id}"))
}

#[test]
fn the_root_offers_call_starts_with_and_without_the_camera() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    for (id, label) in [
        ("start-call", "Start a call"),
        ("start-video-call", "Start a video call"),
    ] {
        let start = root_action(&mut app, id);
        assert_eq!(start.label, label);
        assert_eq!(start.kind, ui::tilo_actions::OperationKind::Effect);
        assert_eq!(
            start.contract.retry,
            ui::tilo_actions::RetryPolicy::Idempotent
        );
    }
}

/// What the package manifest offers while Chat is closed is what the running
/// app publishes on its root, with the same label, kind and confirmation, so
/// launching it from the catalogue lands on a real action.
#[test]
fn the_actions_the_manifest_offers_while_closed_are_published() {
    use tilo_app_package::manifest::{Manifest, OfferKind, OfferOperation};
    use ui::tilo_actions::{ActionKind, ConfirmationPolicy, OperationKind};
    let manifest =
        Manifest::from_toml_str(include_str!("../../tilo-app.toml")).expect("the manifest");
    assert!(!manifest.offers.is_empty(), "Chat offers actions");
    let mut app = TestApp::<ChatShell>::new().unwrap();
    for offer in &manifest.offers {
        assert_eq!(offer.resource, Chat::KIND, "offered on the root");
        let action = root_action(&mut app, &offer.action);
        assert_eq!(action.label, offer.label);
        let kind = match offer.kind {
            OfferKind::Read => ActionKind::Read,
            OfferKind::Undoable => ActionKind::Undoable,
            OfferKind::Outside => ActionKind::Outside,
        };
        assert_eq!(action.contract.kind, kind, "{}", offer.action);
        let operation = match offer.operation {
            OfferOperation::Document => OperationKind::Document,
            OfferOperation::Effect => OperationKind::Effect,
            OfferOperation::View => OperationKind::View,
        };
        assert_eq!(action.kind, operation, "{}", offer.action);
        assert_eq!(
            offer.confirm,
            action.contract.confirmation != ConfirmationPolicy::None,
            "{}",
            offer.action
        );
        assert!(
            action.params().is_empty(),
            "{} takes no parameters",
            offer.action
        );
    }
}

#[test]
fn start_call_through_the_door_makes_a_voice_channel_and_joins_it() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    app.call("start-call", serde_json::json!({})).unwrap();
    app.settle();
    assert!(
        app.state()
            .channel_rows()
            .iter()
            .any(|channel| channel.kind == crate::model::ChannelKind::Voice),
        "the starter's voice channel stands"
    );
}

#[test]
fn a_window_publishes_its_effects_beside_the_chat_verbs() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let surface = app.agent_surface();
    let on = |resource: &str| {
        surface
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["resource"] == resource)
            .map(|row| row["action"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let root = on(Chat::KIND);
    for id in [
        "attach-file",
        "select-input",
        "start-call",
        "create-channel",
        "create-category",
    ] {
        assert!(root.iter().any(|action| action == id), "{id}: {root:?}");
    }
    let general = on(&item("channels", "c_chat"));
    for id in [
        "open-channel",
        "submit-feedback",
        "mark-read",
        "move-channel",
        "delete-channel",
        "read-messages",
        "read-feedback-destination",
    ] {
        assert!(
            general.iter().any(|action| action == id),
            "{id}: {general:?}"
        );
    }
    // Posting is the chat's messages' own: the chain the window publishes.
    let messages = on(&crate::testing::chain());
    for id in ["post", "post-file", "share-resource", "stream-output"] {
        assert!(
            messages.iter().any(|action| action == id),
            "{id}: {messages:?}"
        );
    }
    // The desktop's roster joins a voice channel by this exact id, with
    // Chat closed as much as open: `chat.workspace/channels/<id>/call`
    // (shell `overlay_host::voice::join_call_target`). A new id is a broken
    // Join everywhere.
    assert_eq!(
        crate::testing::call_row("c_voice"),
        "chat.workspace/channels/c_voice/call"
    );
    assert_eq!(
        crate::testing::item("channels", "c_voice"),
        "chat.workspace/channels/c_voice",
        "and reads the channel's name from this row's label"
    );
    let lounge = on(&crate::testing::call_row("c_voice"));
    for id in ["join-call", "leave-call", "mute", "unmute", "call-status"] {
        assert!(lounge.iter().any(|action| action == id), "{id}: {lounge:?}");
    }
    assert!(
        !root.iter().any(|action| action.contains("workspace")),
        "Chat runs in a workspace and makes none: {root:?}"
    );
}

#[test]
fn feedback_is_filed_by_people_and_never_offered_to_agents() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let resources = app.harness().resources();
    let submit = ui::tilo_actions::door::survey(&resources)
        .into_iter()
        .find(|descriptor| {
            descriptor.address.resource.as_str() == item("channels", "c_chat")
                && descriptor.address.action == "submit-feedback"
        })
        .expect("a channel takes feedback");
    // The submission is what the feedback sheet reviewed with a person; an
    // agent offered it files nothing and posts nothing.
    assert!(!submit.contract.reaches_agents());
    assert!(submit.contract.reaches_people());
}

#[test]
fn an_agent_posts_and_reacts_in_one_call_each_as_itself_for_its_person() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent-task:44444444-4444-4444-8444-444444444444")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let _ = general(&app);
    app.call_by(
        agent.clone(),
        &crate::testing::chain(),
        "post",
        serde_json::json!({"channel": "c_chat", "text": "Summary: two open questions"}),
    )
    .unwrap();
    app.settle();
    let posted = messages(&mut app).pop().unwrap();
    assert_eq!(posted.text, "Summary: two open questions");
    assert_eq!(
        posted.author, "agent-task:44444444-4444-4444-8444-444444444444",
        "the post is the agent's, not its person's"
    );
    let react = crate::parts::ChatPart::paths()
        .messages
        .item(&ui::tilo_resource::Id::from_raw(&posted.id))
        .reactions
        .react("👀");
    app.call_by(
        agent,
        &crate::testing::chain(),
        ui::tilo_resource::FIELD_EDIT,
        serde_json::json!({ (ui::tilo_resource::EDIT_PARAM): react }),
    )
    .unwrap();
    app.settle();
    let reacted = messages(&mut app).pop().unwrap();
    assert_eq!(reacted.reactions[0].emoji, "👀");
    assert_eq!(
        reacted.reactions[0]
            .reactors
            .iter()
            .map(|reactor| reactor.by.as_str())
            .collect::<Vec<_>>(),
        ["agent-task:44444444-4444-4444-8444-444444444444"],
        "the reaction is the agent's"
    );
}

#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent-task:55555555-5555-4555-8555-555555555555")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let (channel, _) = general(&app);
    app.call_by(
        agent.clone(),
        &crate::testing::chain(),
        "post",
        serde_json::json!({"channel": "c_chat", "text": "Standup moved to 10"}),
    )
    .unwrap();
    app.settle();
    let posted_id = messages(&mut app).pop().unwrap().id;
    let posted = crate::testing::message_item(&posted_id);
    let react = crate::parts::ChatPart::paths()
        .messages
        .item(&ui::tilo_resource::Id::from_raw(&posted_id))
        .reactions
        .react("👍");
    app.call_by(
        agent.clone(),
        &crate::testing::chain(),
        ui::tilo_resource::FIELD_EDIT,
        serde_json::json!({ (ui::tilo_resource::EDIT_PARAM): react }),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        &posted,
        "edit-message",
        serde_json::json!({"text": "Standup moved to 10:30"}),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        &channel,
        "set-channel-topic",
        serde_json::json!({"topic": "Daily notes"}),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        Chat::KIND,
        "create-category",
        serde_json::json!({"name": "Offsite"}),
    )
    .unwrap();
    app.call_by(
        agent,
        &channel,
        "mark-read",
        serde_json::json!({"part": 0, "seen": 1}),
    )
    .unwrap();
    app.settle();

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry.invoker.as_ref().is_some_and(|invoker| {
            invoker.actor == "agent-task:55555555-5555-4555-8555-555555555555"
        })
    }));
    assert_eq!(
        sentences,
        // What it did to the messages is in the history of the part that
        // holds them; the record's history has the rest.
        [
            "Set the topic of #chat to Daily notes",
            "Created the category Offsite",
            "Marked #chat read",
        ]
    );
}

#[test]
fn every_scenario_stages_through_the_resource_and_its_surface() {
    for scenario in SCENARIOS {
        let mut app = TestApp::<ChatShell>::capture(Some(scenario))
            .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        let held = crate::testing::mounted_messages(&mut app);
        let state = app.state();
        if *scenario == "starter" {
            assert_eq!(
                state.channel_rows().len(),
                2,
                "the first-run room: #chat and #voice"
            );
        } else {
            assert!(
                state.message_row(&held, "m_concept").is_some(),
                "{scenario} shows the demo conversation"
            );
        }
    }
    let mut agent = TestApp::<ChatShell>::capture(Some("agent-work")).unwrap();
    let held = crate::testing::mounted_messages(&mut agent);
    let state = agent.state();
    let reply = state
        .message_row(&held, &crate::testing::agent_reply())
        .expect("agent reply is durable");
    assert_eq!(reply.author, DEMO_AGENT, "the agent authors its own reply");
    assert_eq!(reply.parent.as_deref(), Some(DEMO_SUMMONS));
    let work = reply.work.expect("reply carries work evidence");
    assert_eq!(
        work.reasoning.as_ref().unwrap().route,
        crate::model::AgentRoute::Model
    );
    assert!(matches!(
        work.trace.as_slice(),
        [crate::model::AgentStep {
            outcome: crate::model::AgentStepOutcome::Asked,
            ..
        }]
    ));
    let mut composer = TestApp::<ChatShell>::capture(Some("composer")).unwrap();
    assert!(
        !composer.read_document(|doc| crate::projection::has_draft(doc, "c_crit")),
        "a draft is never the room's"
    );
    let draft = composer
        .with_surface::<ChatApp, _>(|chat| chat.composer_main.value())
        .unwrap();
    assert!(draft.contains("Enter sends"), "{draft:?}");
}

/// What one person types is theirs: it is in their own record on each of
/// their devices, and never in another person's composer or in the room.
#[test]
fn a_draft_is_the_writers_own_and_follows_them_to_their_other_device() {
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let mut laptop = TestApp::<ChatShell>::for_person(&ada).unwrap();
    let mut phone = TestApp::<ChatShell>::for_person(&ada).unwrap();
    let mut bobs = TestApp::<ChatShell>::for_person(&bob).unwrap();
    let mut room = laptop.peer_document();
    bobs.merge_peer(&mut room).unwrap();
    phone.merge_peer(&mut room).unwrap();
    let channel = laptop
        .with_surface::<ChatApp, _>(|chat| chat.active_channel.clone())
        .unwrap();

    laptop.with_surface::<ChatApp, _>(|chat| {
        let mut doc = ui::automerge::AutoCommit::new();
        chat.composer_main.set_key(&channel);
        chat.composer_main.ensure_draft(&doc);
        chat.composer_main
            .insert_text(&mut ui::widget::Edits::lend(&mut doc), "hello from Ada");
    });
    laptop.harness().relayout();

    // Bob shares the room and sees none of it.
    let mut room = laptop.peer_document();
    bobs.merge_peer(&mut room).unwrap();
    bobs.harness().relayout();
    let bobs_composer = bobs
        .with_surface::<ChatApp, _>(|chat| {
            chat.composer_main.set_key(&channel);
            chat.composer_main.value()
        })
        .unwrap();
    assert_eq!(
        bobs_composer, "",
        "Ada's typing never shows in Bob's composer"
    );
    assert!(
        !room
            .save()
            .windows(b"hello from Ada".len())
            .any(|w| w == b"hello from Ada"),
        "the room's record carries none of it"
    );

    // Ada's phone has it, once her own record reaches it.
    let adas = laptop.person::<crate::drafts::ChatDrafts>().unwrap();
    let on_phone = phone.person::<crate::drafts::ChatDrafts>().unwrap();
    on_phone
        .record()
        .merge_peer_document(adas.record().document().unwrap())
        .unwrap();
    phone.harness().relayout();
    let phones_composer = phone
        .with_surface::<ChatApp, _>(|chat| {
            chat.composer_main.set_key(&channel);
            chat.composer_main.sync(&ui::automerge::AutoCommit::new());
            chat.composer_main.value()
        })
        .unwrap();
    assert_eq!(phones_composer, "hello from Ada");
}

/// Filing feedback is commenting: a commenter files a report through the
/// app's own door, a viewer cannot.
#[test]
fn a_commenter_files_feedback_and_a_viewer_cannot() {
    let submission = |app: &TestApp<ChatShell>| {
        let channel = app
            .state()
            .channels
            .iter()
            .find(|(_, channel)| channel.kind == crate::model::ChannelKind::Text)
            .map(|(id, _)| (id.as_str().to_owned(), "room".to_owned()))
            .expect("the chat has a text channel");
        ui::feedback::Submission {
            id: "0123456789abcdef0123456789abcdef".into(),
            destination: ui::feedback::Destination {
                room: channel.1,
                provider: "fixture-chat".into(),
                channel: channel.0,
            },
            feedback: ui::feedback::ReviewedFeedback {
                text: "The page loses my position".into(),
                context: Vec::new(),
                screenshot: None,
                ..Default::default()
            },
        }
    };
    let mut commenter = TestApp::<ChatShell>::with_role(ui::permissions::Role::Commenter).unwrap();
    let filed = submission(&commenter);
    commenter
        .invoke(crate::testing::submit_feedback(&filed))
        .expect("a commenter files feedback");
    assert!(
        commenter
            .state()
            .feedback
            .iter()
            .any(|(id, _)| id.as_str() == filed.id)
    );

    let mut viewer = TestApp::<ChatShell>::with_role(ui::permissions::Role::Viewer).unwrap();
    let refused = submission(&viewer);
    assert!(
        viewer
            .invoke(crate::testing::submit_feedback(&refused))
            .is_err(),
        "a viewer may not file feedback"
    );
}

#[test]
fn a_drawn_frame_hands_chat_the_hosts_permissions() {
    let mut viewer = TestApp::<ChatShell>::with_role(ui::permissions::Role::Viewer).unwrap();
    viewer.harness().resize((960, 720)).unwrap();
    assert_eq!(
        viewer.with_surface::<ChatApp, _>(|surface| surface.perms.can_participate()),
        Some(false)
    );
}

#[test]
fn stopping_shared_context_wakes_the_native_presentation() {
    let mut app = TestApp::<ChatShell>::capture(Some("live-context")).unwrap();
    assert!(app.surface_mut::<crate::ChatApp>().sharing_context);
    assert!(!app.harness().root_widget().needs_redraw());
    app.surface_mut::<crate::ChatApp>()
        .answer_effect(crate::effects::ChatEffect::StopSharingLiveContext)
        .unwrap();
    assert!(
        app.harness().root_widget().needs_redraw(),
        "semantic Stop must schedule its privacy indicator and withdrawn view frame"
    );
}

/// A file another step handed over posts by its content address: described
/// from its bytes where it is posted, and refused when it is not here.
#[test]
fn a_handed_over_file_posts_as_a_file() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let _ = general(&app);
    let hash = app.content().put(b"minutes of the meeting").unwrap();
    app.call_on(
        &crate::testing::chain(),
        "post-file",
        serde_json::json!({"channel": "c_chat",
            "content": ui::tilo_actions::content_resource(&hash),
            "name": "minutes.txt",
            "text": "Here are the minutes",
        }),
    )
    .unwrap();
    app.settle();
    let posted = messages(&mut app).pop().unwrap();
    assert_eq!(posted.text, "Here are the minutes");
    assert_eq!(
        posted.attachment,
        Some(crate::model::Attachment::File {
            name: "minutes.txt".into(),
            blob: hash,
            size: 22,
        })
    );
    let missing = tilo_content::hash_bytes(b"never stored");
    assert!(
        app.call_on(
            &crate::testing::chain(),
            "post-file",
            serde_json::json!({"channel": "c_chat", "content": missing, "name": "x.txt"}),
        )
        .is_err()
    );
}

/// A message another replica posts (a room's bot bridging Discord, another
/// member's device) shows in a window that is only being watched: its
/// arrival in the window's store wakes the window, so the window's own
/// frames show it, with no input and no frame forced to sync.
#[test]
fn a_message_another_replica_posts_shows_without_input() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    crate::testing::mounted_post(&mut app, "c_chat", "hello from here");
    app.settle();
    let shows = |app: &mut TestApp<ChatShell>, text: &str| {
        app.harness()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.label().is_some_and(|label| label.contains(text)))
    };
    assert!(shows(&mut app, "hello from here"));
    let text = "hello from the bridge";
    let changed = app
        .replica_arrives(&crate::parts::part(0), |doc| {
            let mut bot =
                ui::tilo_resource::Replica::<crate::parts::ChatPart>::from_doc(std::mem::take(doc))
                    .as_author("bot");
            bot.apply(crate::testing::post("c_chat", text, None))
                .expect("the bot posts");
            *doc = bot.doc().clone();
        })
        .unwrap();
    assert!(changed, "the bot's post is new here");
    for _ in 0..4 {
        app.frame();
    }
    assert!(shows(&mut app, text), "the bot's post shows with no input");
}

/// The channels a narrow window shows on their own know who is in a call,
/// though the conversation behind them is not drawn, and follow someone
/// joining it.
#[test]
fn the_compact_channels_show_who_is_in_a_call_and_who_joins() {
    let mut app = TestApp::<ChatShell>::capture(Some("mention")).unwrap();
    app.harness().resize((480, 800)).unwrap();
    app.settle();
    app.with_widget::<ui::widget::NavShell, _>(|shell| {
        shell.show(ui::widget::NavPane::Sidebar);
        shell.snap();
    });
    app.settle();
    let on_its_own = app.with_widget::<ui::widget::NavShell, _>(|shell| {
        (shell.geometry().sidebar, shell.front_pane())
    });
    assert_eq!(
        on_its_own,
        Some((
            ui::widget::SidebarPlacement::Screen,
            ui::widget::NavPane::Sidebar
        )),
        "the channels fill the window; the conversation is put away"
    );
    let standup = ChatApp::voice_presence_key("c_standup");
    let join = |app: &mut TestApp<ChatShell>, person: &str| {
        app.harness()
            .presence_mut()
            .set_caret(&standup, ui::permissions::UserId(person.into()), 0);
        app.settle();
    };
    let in_call = |app: &mut TestApp<ChatShell>, person: &str| {
        app.with_widget::<ui::widget::NavList, _>(|list| {
            list.row_rect(&format!("in-call:c_standup/{person}"))
        })
        .flatten()
        .is_some()
    };
    join(&mut app, "u_aria");
    join(&mut app, "u_milo");
    assert!(in_call(&mut app, "u_aria") && in_call(&mut app, "u_milo"));
    assert!(!in_call(&mut app, "u_sora"));
    join(&mut app, "u_sora");
    assert!(in_call(&mut app, "u_sora"), "who joins shows at once");
}
