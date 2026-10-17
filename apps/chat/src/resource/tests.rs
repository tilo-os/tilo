use super::*;
use crate::testing::item;
use ui::framework::testing::TestApp;
use ui::search::SearchProvider as _;
use ui::tilo_resource::Resource as _;

#[test]
fn sidebar_remains_docked_and_semantically_resizable_in_a_medium_chat_window() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    app.harness().resize((760, 720)).unwrap();
    let resources = app.harness().resources();
    let resize = resources
        .iter()
        .find(|resource| resource.label == "Resize sidebar")
        .expect("a medium conversation keeps its shared resize handle");
    let address = ui::tilo_actions::door::survey(std::slice::from_ref(resize))
        .into_iter()
        .find(|offered| offered.address.action == "set-value")
        .expect("the separator offers set-value")
        .address;
    let arguments = serde_json::Map::from_iter([(
        ui::tilo_actions::PROMPTED_PARAM.to_owned(),
        serde_json::Value::String("40%".into()),
    )]);
    assert!(
        app.harness()
            .invoke_action(&address, &arguments, false, None)
            .unwrap()
            .completed()
            .is_some()
    );
    assert_eq!(
        app.with_surface::<ChatApp, _>(|surface| (
            surface.left_flap.is_docked(),
            surface.sidebar_rect().width(),
            surface.content_rect().width(),
        )),
        Some((true, 228.0, 456.0))
    );
    assert!(app.harness().component_contract_violations().is_empty());
    app.harness().resize((600, 720)).unwrap();
    assert!(
        app.harness()
            .resources()
            .iter()
            .all(|resource| resource.label != "Resize sidebar"),
        "a genuinely narrow window uses the shared overlay drawer"
    );
    app.harness().resize((760, 720)).unwrap();
    assert_eq!(
        app.with_surface::<ChatApp, _>(|surface| surface.sidebar_rect().width()),
        Some(228.0),
        "the usable resized width survives narrow-window collapse"
    );
}

#[test]
fn sidebar_shared_resize_uses_the_complete_mounted_app_door() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    app.harness().resize((960, 720)).unwrap();
    let resources = app.harness().resources();
    let resize = resources
        .iter()
        .find(|resource| resource.label == "Resize sidebar")
        .expect("the live shared separator has its localized human label");
    let spec = resize
        .actions
        .iter()
        .find(|action| action.id == "set-value")
        .expect("the mounted app publishes exact resize");
    assert_eq!(spec.kind, ui::tilo_actions::OperationKind::Effect);
    assert!(app.harness().component_contract_violations().is_empty());
    let address = ui::tilo_actions::door::survey(std::slice::from_ref(resize))
        .into_iter()
        .find(|offered| offered.address.action == "set-value")
        .expect("the separator offers set-value")
        .address;
    let arguments = serde_json::Map::from_iter([(
        ui::tilo_actions::PROMPTED_PARAM.to_owned(),
        serde_json::Value::String("30%".into()),
    )]);
    assert!(
        app.harness()
            .invoke_action(&address, &arguments, false, None)
            .expect("the actual published shared control accepts canonical invocation")
            .completed()
            .is_some()
    );
    assert_eq!(
        app.with_surface::<ChatApp, _>(|surface| surface.left_flap.list_rect().width()),
        Some(212.0)
    );
    let mut viewer = TestApp::<ChatShell>::with_role(ui::permissions::Role::Viewer).unwrap();
    viewer.harness().resize((960, 720)).unwrap();
    assert!(viewer.harness().component_contract_violations().is_empty());
    let resources = viewer.harness().resources();
    let separator = resources
        .iter()
        .find(|resource| resource.label == "Resize sidebar")
        .expect("the Viewer frame observes the actual shared separator");
    for action in &separator.actions {
        if matches!(action.id.as_str(), "set-value" | "increment" | "decrement") {
            assert_eq!(action.kind, ui::tilo_actions::OperationKind::Effect);
        }
    }
}

#[test]
fn shared_space_visible_stop_button_uses_the_same_consent_handler() {
    let mut app = TestApp::<ChatShell>::capture(Some("live-context")).unwrap();
    assert!(app.surface_mut::<crate::ChatApp>().sharing_context);
    assert!(app.harness().a11y_node_named("Stop sharing").is_some());
    let target = app
        .surface_mut::<crate::ChatApp>()
        .hits
        .iter()
        .find(|(_, hit)| matches!(hit, crate::app::Hit::StopSharingLiveContext))
        .expect("the painted stop control has a pointer target")
        .0;
    app.harness().click_at(target.center().x, target.center().y);
    assert!(!app.surface_mut::<crate::ChatApp>().sharing_context);
    assert!(app.harness().a11y_node_named("Stop sharing").is_none());
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

/// The general channel of a new chat: its item resource and itself.
fn general(app: &TestApp<ChatShell>) -> (String, crate::model::Channel) {
    let channel = app
        .state()
        .channel_rows()
        .into_iter()
        .find(|channel| channel.id == "c_general")
        .unwrap();
    (item("channels", &channel.id), channel)
}

/// Every message of the chat `app` holds.
fn messages(app: &TestApp<ChatShell>) -> Vec<crate::model::Message> {
    let chat = app.state();
    chat.channel_rows()
        .iter()
        .flat_map(|channel| chat.message_rows(&channel.id))
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
                room: channel.workspace,
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
fn search_hits_open_the_workspace_channel_message_and_person_they_name() {
    let mut app = TestApp::<ChatShell>::capture(None).unwrap();
    let entries = Chat::search(&app.state());
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
    let jam = anchor("Weekend Jam", "workspaces");
    app.call("open", serde_json::json!({ "anchor": jam }))
        .unwrap();
    app.settle();
    assert_eq!(app.surface_mut::<ChatApp>().active_ws, "ws_jam");
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
    let (channel, _) = general(&app);
    app.call_on(&channel, "post", serde_json::json!({"text": "café notes"}))
        .unwrap();
    app.settle();
    let id = messages(&app)
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
        &message,
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
            &crate::testing::call_row("c_lounge"),
            "call-status",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(status["refreshRequested"], true);
    assert_eq!(status["reported"]["capture_active"], false);
    assert_eq!(status["reported"]["muted"], true);
    assert_eq!(status["channel"], serde_json::Value::Null);
    assert!(messages(&app).is_empty());
}

#[test]
fn shared_space_resource_journey_preserves_address_author_and_viewer_boundary() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let (channel, _) = general(&app);
    let reference = serde_json::json!({
        "kind": "word_processor.block", "id": "word_processor.block:paragraph-7",
        "label": "Proposal", "span": {"start": 2, "end": 9, "unit": "Chars"}
    });
    app.call_on(
        &channel,
        "share-resource",
        serde_json::json!({"reference": reference, "source_provider": "record:so.tilo.word-processor:proposal", "text": "Discuss this paragraph"}),
    )
    .unwrap();
    app.settle();
    let message = messages(&app)
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
            &channel,
            "share-resource",
            serde_json::json!({"reference": reference})
        )
        .is_err()
    );
    assert_eq!(messages(&app).len(), 1);
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
    let (channel, _) = general(&app);
    for id in [
        "/home/person/private",
        "https://example.test",
        "block\nsecret",
        "",
    ] {
        assert!(
            app.call_on(
                &channel,
                "share-resource",
                serde_json::json!({
                    "reference": {"kind": "word_processor.block", "id": id, "label": "Proposal"}
                })
            )
            .is_err()
        );
    }
    assert!(
        app.call_on(
            &item("channels", "c_lounge"),
            "share-resource",
            serde_json::json!({
                "reference": {"kind": "word_processor.block", "id": "block-7", "label": "Proposal"}
            })
        )
        .is_err()
    );
    assert!(messages(&app).is_empty());
}

#[test]
fn a_new_room_record_starts_with_its_creators_workspace() {
    let app = TestApp::<ChatShell>::new().unwrap();
    let state = app.state();
    assert_eq!(
        state
            .workspace_rows()
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect::<Vec<_>>(),
        ["ws_home"]
    );
    assert_eq!(state.people.len(), 1, "only its creator");
    assert!(
        state
            .channel_rows()
            .iter()
            .any(|channel| channel.name == "general"),
        "the starter channels"
    );
    assert!(state.messages.is_empty(), "no fabricated conversation");
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
fn the_root_offers_call_starts_only_once_there_is_a_workspace() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let start = root_action(&mut app, "start-call");
    assert_eq!(start.label, "Start a call");
    assert_eq!(start.kind, ui::tilo_actions::OperationKind::Effect);
    assert_eq!(
        start.contract.retry,
        ui::tilo_actions::RetryPolicy::Idempotent
    );
    assert_eq!(
        root_action(&mut app, "start-video-call")
            .contract
            .availability,
        ui::tilo_actions::Availability::Unavailable {
            code: crate::effects::VIDEO_CALL_UNAVAILABLE.into(),
            reason: crate::effects::video_call_unavailable_reason(),
        }
    );
    let empty = ChatApp::new_ephemeral();
    assert!(empty.workspaces.is_empty());
    assert!(
        empty
            .effect_statuses()
            .contains(&ui::framework::EffectStatus::unavailable(
                crate::effects::ChatEffect::START_CALL,
                "no_workspace",
                crate::i18n::t("chat-call-no-workspace"),
            )),
        "a chat with no conversation yet offers no call to start"
    );
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
    let refused = app.call("start-video-call", serde_json::json!({}));
    assert!(refused.is_err(), "{refused:?}");
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
        "post",
        "create-workspace",
    ] {
        assert!(root.iter().any(|action| action == id), "{id}: {root:?}");
    }
    let general = on(&item("channels", "c_general"));
    for id in [
        "open-channel",
        "post",
        "share-resource",
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
    let lounge = on(&crate::testing::call_row("c_lounge"));
    for id in ["join-call", "leave-call", "mute", "unmute", "call-status"] {
        assert!(lounge.iter().any(|action| action == id), "{id}: {lounge:?}");
    }
    let workspace = on(&item("workspaces", "ws_home"));
    for id in ["open-workspace", "create-channel", "create-category"] {
        assert!(
            workspace.iter().any(|action| action == id),
            "{id}: {workspace:?}"
        );
    }
}

#[test]
fn an_agent_posts_and_reacts_in_one_call_each_as_itself_for_its_person() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent-task:44444444-4444-4444-8444-444444444444")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let (channel, _) = general(&app);
    app.call_by(
        agent.clone(),
        &channel,
        "post",
        serde_json::json!({"text": "Summary: two open questions"}),
    )
    .unwrap();
    app.settle();
    let posted = messages(&app).pop().unwrap();
    assert_eq!(posted.text, "Summary: two open questions");
    assert_eq!(
        posted.author, "agent-task:44444444-4444-4444-8444-444444444444",
        "the post is the agent's, not its person's"
    );
    app.call_by(
        agent,
        &item("messages", &posted.id),
        "react",
        serde_json::json!({"emoji": "👀"}),
    )
    .unwrap();
    app.settle();
    let reacted = messages(&app).pop().unwrap();
    assert_eq!(reacted.reactions[0].emoji, "👀");
    let entry = app.history().pop().unwrap();
    let invoker = entry.invoker.unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada")),
        "history names the agent and the person it acted for"
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
        &channel,
        "post",
        serde_json::json!({"text": "Standup moved to 10"}),
    )
    .unwrap();
    app.settle();
    let posted = item("messages", &messages(&app).pop().unwrap().id);
    app.call_by(
        agent.clone(),
        &posted,
        "react",
        serde_json::json!({"emoji": "👍"}),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        &posted,
        "edit-message",
        serde_json::json!({"text": "Standup moved to 10:30"}),
    )
    .unwrap();
    let rename = Chat::paths()
        .channels
        .item(&ui::tilo_resource::Id::from_raw("c_general"))
        .topic
        .set("Daily notes".into());
    app.call_by(
        agent.clone(),
        Chat::KIND,
        "field-edit",
        serde_json::json!({ "edit": rename }),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        Chat::KIND,
        "create-workspace",
        serde_json::json!({"name": "Offsite"}),
    )
    .unwrap();
    app.call_by(agent, &channel, "mark-read", serde_json::json!({}))
        .unwrap();
    app.settle();

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry.invoker.as_ref().is_some_and(|invoker| {
            invoker.actor == "agent-task:55555555-5555-4555-8555-555555555555"
        })
    }));
    assert_eq!(
        sentences,
        [
            "Posted: Standup moved to 10",
            "Reacted with 👍",
            "Edited a message: Standup moved to 10:30",
            "Set the topic of #general to Daily notes",
            "Created the workspace Offsite",
            "Marked #general read",
        ]
    );
}

#[test]
fn every_scenario_stages_through_the_resource_and_its_surface() {
    for scenario in SCENARIOS {
        let app = TestApp::<ChatShell>::capture(Some(scenario))
            .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        let state = app.state();
        if *scenario == "starter" {
            assert!(state.workspace_rows().iter().all(|ws| ws.id == "ws_home"));
        } else {
            assert!(
                state.message_row("m_concept").is_some(),
                "{scenario} shows the demo conversation"
            );
        }
    }
    let agent = TestApp::<ChatShell>::capture(Some("agent-work")).unwrap();
    let state = agent.state();
    let reply = state
        .message_row(&crate::testing::agent_reply())
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
            .map(|(id, channel)| {
                (
                    id.as_str().to_owned(),
                    channel.workspace.as_str().to_owned(),
                )
            })
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

#[test]
fn explicit_start_is_semantic_and_refuses_without_a_selected_saved_task() {
    let mut surface = ChatApp::new_ephemeral();
    assert_eq!(
        crate::app::Hit::AgentStart.semantic_id(),
        "chat.agent-start"
    );
    assert!(!surface.start_agent_task());
    assert!(
        surface
            .answer_effect(crate::effects::ChatEffect::StartAgentTask)
            .is_err()
    );
    assert_eq!(
        crate::effects::ChatEffect::START_AGENT_TASK.id,
        "start-agent-task"
    );
}

#[test]
fn agent_row_task_navigation_keeps_source_custody_through_the_production_door() {
    // Capture mode deliberately has inert effects. Stage the same explicit
    // fixture on a live host so the published door must reach Chat's handler.
    let mut app = TestApp::<ChatShell>::new().unwrap();
    app.with_surface::<ChatApp, _>(|surface| surface.stage_scenario("agent-task"));
    let workspace = app.surface_mut::<ChatApp>().active_ws.clone();
    let target = "chat.agent:scribe";
    // The quick composer has saved this task, but Chat has not projected it.
    let arguments = serde_json::json!({"task": "task:newly-queued", "run_epoch": 0});
    app.call_on(target, "open-task-conversation", arguments.clone())
        .unwrap();
    app.settle();
    let surface = app.surface_mut::<ChatApp>();
    assert_eq!(surface.active_ws, workspace);
    assert_eq!(surface.direct_agent.as_deref(), Some("scribe"));
    assert_eq!(
        surface.direct_agent_tasks().unwrap().selected_identity(),
        Some(("task:newly-queued", 0))
    );
    assert!(surface.direct_agent_tasks().unwrap().current().is_none());
    assert!(
        app.call("start-agent-task", serde_json::json!({})).is_err(),
        "opening is not approval and must not start a different projected task"
    );

    for (field, value) in [("workspace", "another-room"), ("agent", "another-agent")] {
        let mut forged = arguments.clone();
        forged[field] = value.into();
        assert!(
            app.call_on(target, "open-task-conversation", forged)
                .is_err(),
            "the caller cannot select {field}"
        );
    }
    assert!(
        app.call_on(
            target,
            "open-task-conversation",
            serde_json::json!({"task": "task:newly-queued"})
        )
        .is_err(),
        "epoch zero must be explicit, never a default chosen for the caller"
    );
    assert!(
        app.call_on("chat.agent:unadmitted", "open-task-conversation", arguments)
            .is_err()
    );
    app.harness().resize((420, 680)).unwrap();
    let chat = app
        .harness()
        .targets()
        .keys()
        .find(|name| name.ends_with("/ResourceApp[0]"))
        .cloned()
        .expect("the live Chat surface is addressable");
    app.harness()
        .perform_semantic_named(
            &chat,
            Some("chat.agent-window"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    let id = app
        .surface_mut::<ChatApp>()
        .agent_surfaces
        .values()
        .next()
        .map(|surface| {
            assert_eq!(
                surface
                    .view
                    .direct_agent_tasks()
                    .unwrap()
                    .selected_identity(),
                Some(("task:newly-queued", 0))
            );
            surface.id.clone()
        })
        .expect("floating retains the selected task conversation");
    app.with_surface::<ChatApp, _>(|surface| {
        ui::widget::FloatingSurfaceProvider::on_floating_surface_host_message(
            surface,
            &ui::floating_surface::FloatingSurfaceHostMessage::Reattach { id },
        );
    });
    app.settle();
    let surface = app.surface_mut::<ChatApp>();
    assert_eq!(surface.active_ws, workspace);
    assert_eq!(surface.direct_agent.as_deref(), Some("scribe"));
    assert_eq!(
        surface.direct_agent_tasks().unwrap().selected_identity(),
        Some(("task:newly-queued", 0))
    );
}

#[test]
fn generated_agent_task_surface_report() {
    let report = ui::framework::testing::surface_report::<crate::ChatShell>();
    assert!(report["unbound"].as_array().unwrap().is_empty());
    assert!(report["view"].is_null());
    let effects = report["effects"].as_array().unwrap();
    for action in ["start-agent-task", "open-task-conversation"] {
        assert!(effects.iter().any(|id| id == action), "missing {action}");
    }
}

#[test]
fn authenticated_agent_entry_has_navigation_without_an_ordinary_authority_lane() {
    let mut surface = ChatApp::new_ephemeral();
    surface.active_ws = "ws_home".into();
    surface.room_agents.push("scribe".into());

    let expected_identity =
        ui::tilo_actions::ResourceSemanticIdentity::new("chat.agent", "chat.agent:scribe");
    let listed = surface
        .published_rows()
        .into_iter()
        .find(|row| row.id() == "chat.agent:scribe")
        .expect("the authenticated Agent is in the live row listing")
        .into_resource();
    assert_eq!(listed.semantic_identity, expected_identity);
    let resolved = surface
        .published_row("chat.agent:scribe")
        .expect("the exact authenticated Agent row resolves")
        .into_resource();
    assert_eq!(resolved.semantic_identity, expected_identity);

    assert_eq!(listed.actions.len(), 2);
    assert_eq!(listed.actions[0].id, "open-agent-conversation");
    let open_task = &listed.actions[1];
    assert_eq!(open_task.id, "open-task-conversation");
    assert_eq!(
        open_task
            .params()
            .iter()
            .map(|param| param.name.as_ref())
            .collect::<Vec<_>>(),
        ["task", "run_epoch"]
    );
    assert_eq!(resolved.actions[1].params(), open_task.params());
    assert!(surface.published_row("chat.agent:unadmitted").is_none());
    assert!(
        !<Chat as ui::tilo_resource::Resource>::declarations()
            .iter()
            .any(|declaration| declaration.id == "post-agent-message"),
        "execution interaction is task steering, not a second message lane"
    );
}

/// A file another step handed over posts by its content address: described
/// from its bytes where it is posted, and refused when it is not here.
#[test]
fn a_handed_over_file_posts_as_a_file() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let (channel, _) = general(&app);
    let hash = app.content().put(b"minutes of the meeting").unwrap();
    app.call_on(
        &channel,
        "post-file",
        serde_json::json!({
            "content": ui::tilo_actions::content_resource(&hash),
            "name": "minutes.txt",
            "text": "Here are the minutes",
        }),
    )
    .unwrap();
    app.settle();
    let posted = messages(&app).pop().unwrap();
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
            &channel,
            "post-file",
            serde_json::json!({"content": missing, "name": "x.txt"}),
        )
        .is_err()
    );
}
