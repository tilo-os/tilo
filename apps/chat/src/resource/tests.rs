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
    let entry = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .next_back()
        .unwrap();
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

    let english = include_str!("../../locales/en-US.ftl");
    let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry.invoker.as_ref().is_some_and(|invoker| {
                invoker.actor == "agent-task:55555555-5555-4555-8555-555555555555"
            })
        })
        .map(|entry| reader.sentence(&entry))
        .collect();
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
    let composer = TestApp::<ChatShell>::capture(Some("composer")).unwrap();
    let draft = composer.read_document(|doc| crate::projection::draft_text(doc, "c_crit"));
    assert!(draft.contains("Enter sends"), "{draft:?}");
}
