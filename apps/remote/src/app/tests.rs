use super::*;
use crate::resource::RemoteShell;
use ui::framework::ResourceApp as _;
use ui::headless::Framed;
use ui::prelude::TestApp;

fn doc() -> AutoCommit {
    AutoCommit::new()
}

/// An app holding the sample connections and the legacy RDP profile, as
/// the `session-rdp` scenario's actions leave it.
fn demo() -> RemoteApp {
    RemoteApp::sample()
}

fn framed(app: RemoteApp, w: f64, h: f64) -> Framed<RemoteApp> {
    Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport((w, h)),
        Theme::dark(),
    )
}

fn pending_confirmation_is(app: &RemoteApp, id: ConnectionId) -> bool {
    app.pending_confirmation
        .as_ref()
        .is_some_and(|pending| pending.id() == id)
}

#[test]
fn the_form_saves_through_the_upsert_action_and_reports_problems_at_save() {
    eprintln!("phase an_incomplete_draft_names_each_problem_by_its_field");
    {
        let mut app = demo();
        app.apply(Action::NewConnection);
        assert_eq!(app.screen, Screen::Form { editing: None });
        assert_eq!(app.problems, FormProblems::default(), "quiet until saved");
        app.fields.port.set(String::new());
        app.apply(Action::SaveForm);
        assert!(app.problems.name.is_some());
        assert!(app.problems.host.is_some());
        assert!(app.problems.port.is_some());
        assert_eq!(app.screen, Screen::Form { editing: None }, "still editing");
        assert_eq!(app.workspace.connections(), sample_connections().as_slice());
    }

    eprintln!("phase a_complete_draft_is_written_and_marked_in_the_list");
    {
        let mut app = demo();
        app.apply(Action::NewConnection);
        app.fields.name.set("Media server".into());
        app.fields.host.set("media.home.arpa".into());
        app.fields.user.set("media".into());
        app.apply(Action::SetAuth(AuthMethod::PublicKey));
        app.apply(Action::SaveForm);
        assert_eq!(app.screen, Screen::List);
        let saved = app
            .workspace
            .connections()
            .iter()
            .find(|c| c.name == "Media server")
            .expect("saved");
        assert_eq!(saved.port, 22);
        assert_eq!(saved.auth, AuthMethod::PublicKey);
        assert_eq!(app.selected, Some(saved.id));
        assert_eq!(saved.host, "media.home.arpa");
    }

    eprintln!("phase switching_protocol_keeps_typed_text_and_resets_the_port");
    {
        let mut app = demo();
        app.apply(Action::NewConnection);
        app.fields.name.set("Lab box".into());
        app.apply(Action::SetKind(ConnectionKind::Vnc));
        assert_eq!(app.fields.name.with(Clone::clone), "Lab box");
        assert_eq!(
            app.fields.port.with(Clone::clone),
            "22",
            "a typed port is kept"
        );
        app.fields.port.set(String::new());
        app.apply(Action::SetKind(ConnectionKind::Vnc));
        assert_eq!(app.fields.port.with(Clone::clone), "5900");
        app.apply(Action::SetKind(ConnectionKind::Tilo));
        assert_eq!(app.draft.auth, AuthMethod::DeviceIdentity);
        app.apply(Action::PickTiloDevice(1));
        assert_eq!(app.fields.name.with(Clone::clone), "Workshop Laptop");
        assert!(app.form_model(None).devices[1].picked);
    }

    eprintln!("phase after_a_failed_save_problems_follow_each_edit");
    {
        let mut app = demo();
        app.apply(Action::NewConnection);
        app.revalidate();
        assert_eq!(app.problems, FormProblems::default(), "quiet while typing");
        app.apply(Action::SaveForm);
        assert!(app.problems.name.is_some());
        app.fields.name.set("Lab box".into());
        app.revalidate();
        assert!(app.problems.name.is_none(), "a fixed field loses its error");
        assert!(app.problems.host.is_some(), "the rest stay");
        app.apply(Action::NewConnection);
        app.revalidate();
        assert_eq!(
            app.problems,
            FormProblems::default(),
            "a new draft starts quiet"
        );
    }

    eprintln!("phase editing_preserves_favourite_and_history");
    {
        let mut app = demo();
        let id = app.first_of_kind(ConnectionKind::Ssh).unwrap();
        let before = app.workspace.get(id).unwrap().clone();
        app.apply(Action::EditConnection(id));
        assert_eq!(app.fields.host.with(Clone::clone), before.host);
        app.fields.name.set("Build server (rack 2)".into());
        app.apply(Action::SaveForm);
        let after = app.workspace.get(id).unwrap();
        assert_eq!(after.name, "Build server (rack 2)");
        assert_eq!(after.favorite, before.favorite);
        assert_eq!(after.last_used_ms, before.last_used_ms);
    }
}

#[test]
fn connecting_asks_first_revalidates_and_explains_unavailable_transports() {
    eprintln!("phase connect_requires_confirmation_before_opening_a_broker_route");
    {
        let mut app = demo();
        let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
        app.apply(Action::RequestConnect(id));
        assert!(pending_confirmation_is(&app, id));
        assert!(app.session.borrow().live.is_none());
        assert!(app.session.borrow().collab_outbox.is_empty());
    }

    eprintln!("phase confirmation_revalidates_a_collaboratively_changed_target");
    {
        let mut app = demo();
        let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
        app.apply(Action::RequestConnect(id));
        app.workspace.get_mut(id).unwrap().host = SecretKey::generate().public().to_string();
        app.apply(Action::ConfirmPending);
        assert!(app.session.borrow().live.is_none());
        assert_eq!(app.toasts.len(), 1, "the person learns why nothing opened");
    }

    eprintln!("phase unavailable_transports_open_a_recovery_state_without_a_socket");
    {
        let mut app = demo();
        let ssh = app.first_of_kind(ConnectionKind::Ssh).unwrap();
        if !crate::session::live_sessions_enabled() {
            app.apply(Action::RequestConnect(ssh));
            assert!(app.pending_confirmation.is_none(), "nothing to confirm");
            assert_eq!(app.screen, Screen::Session { id: ssh });
            assert!(matches!(
                app.session.borrow().live,
                Some(LiveSession::Unavailable {
                    readiness: TransportReadiness::NeedsLiveSessionCapability,
                    ..
                })
            ));
            assert!(
                app.status_model(ssh).action.is_none(),
                "editing cannot grant access"
            );
        }
        let rdp = app.first_of_kind(ConnectionKind::Rdp).unwrap();
        app.apply(Action::RequestConnect(rdp));
        assert_eq!(app.screen, Screen::Session { id: rdp });
        assert_eq!(
            app.status_model(rdp).action,
            Some(("Edit connection", Action::EditConnection(rdp)))
        );
        app.apply(Action::Disconnect);
        assert_eq!(app.screen, Screen::List);
        assert!(app.session_for.is_none());
    }

    eprintln!("phase exhausted_session_generation_fails_closed_without_reuse");
    {
        let mut app = demo();
        let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
        app.session_generation = u64::MAX;
        app.open_session(id);
        assert_eq!(app.session_generation, u64::MAX);
        assert!(matches!(
            app.session.borrow().live,
            Some(LiveSession::Error(_))
        ));
        app.disconnect_session();
        app.open_session(id);
        assert_eq!(app.session_generation, u64::MAX);
        assert!(matches!(
            app.session.borrow().live,
            Some(LiveSession::Error(_))
        ));
    }
}

#[test]
fn leaving_a_session_that_is_not_live_closes_it() {
    let mut app = demo();
    app.stage_scenario(Some("session-closed"));
    assert!(app.session_for.is_some());
    app.apply(Action::Show(Destination::Connections));
    assert!(app.session_for.is_none(), "the rail no longer offers it");
    assert_eq!(app.screen, Screen::List);
    assert_eq!(app.destination, Destination::Connections);

    let mut app = demo();
    app.stage_scenario(Some("session-ssh"));
    app.apply(Action::Show(Destination::Pinned));
    assert!(app.session_for.is_some(), "a connected session stays open");
    assert_eq!(app.destination, Destination::Pinned);
}

#[test]
fn staged_sessions_show_a_connected_surface_and_record_nothing() {
    for (scenario, kind) in [
        ("session-ssh", ConnectionKind::Ssh),
        ("session-vnc", ConnectionKind::Vnc),
    ] {
        let mut app = demo();
        app.stage_scenario(Some(scenario));
        let id = app.first_of_kind(kind).unwrap();
        assert!(matches!(app.content(), Content::Surface(shown) if shown == id));
        app.pump_session();
        assert_eq!(
            app.workspace.connections(),
            sample_connections().as_slice(),
            "samples change no saved profile"
        );
        let session = app.session.borrow();
        assert_eq!(
            SessionPane::terminal_grid(&session).is_some(),
            kind.is_terminal()
        );
        assert_eq!(session.fb_image.is_some(), !kind.is_terminal());
    }
}

#[test]
fn tilo_sessions_use_the_authenticated_broker_and_consent() {
    let mut app = demo();
    let target = SecretKey::generate().public();
    let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
    app.workspace.get_mut(id).unwrap().host = target.to_string();
    app.open_session(id);
    let session_id = match app.session.borrow().live.as_ref() {
        Some(LiveSession::Tilo(session)) => session.id,
        _ => panic!("Tilo session"),
    };
    assert!(matches!(
        app.take_collab_requests().as_slice(),
        [ui::collaboration::CollabRequest::OpenWorkspaceControl { target_device, .. }]
            if target_device == target.as_bytes()
    ));
    let motion = runtime_collab::WorkspaceControlInput::PointerMotion { x: 1.0, y: 2.0 };
    assert!(
        !app.session.borrow_mut().queue_tilo_input(motion.clone()),
        "no input before the target consents"
    );
    app.handle_collab_event(&ui::collaboration::CollabEvent::WorkspaceControl {
        session_id,
        event: runtime_collab::WorkspaceControlEvent::StreamStarted {
            surface: 4,
            width: 1280,
            height: 720,
        },
    });
    assert!(app.session.borrow_mut().queue_tilo_input(motion.clone()));
    app.handle_collab_event(&ui::collaboration::CollabEvent::WorkspaceControl {
        session_id,
        event: runtime_collab::WorkspaceControlEvent::Closed(
            runtime_collab::WorkspaceControlCloseReason::Revoked,
        ),
    });
    assert!(!app.session.borrow_mut().queue_tilo_input(motion));
    let _ = app.take_collab_requests();
    app.apply(Action::Disconnect);
    assert!(matches!(
        app.take_collab_requests().as_slice(),
        [ui::collaboration::CollabRequest::CloseWorkspaceControl { .. }]
    ));
}

#[test]
fn resource_operations_settle_only_their_own_session() {
    let mut app = demo();
    let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
    let invocation = tilo_actions::InvocationId::new();
    app.session_for = Some(id);
    app.session_generation = 42;
    app.session.borrow_mut().live = Some(LiveSession::Preview(SessionStatus::Connecting));
    app.pending_resource_operation = Some(PendingResourceOperation::Connect {
        invocation,
        connection_id: id,
        session_generation: 42,
    });
    assert!(matches!(
        app.poll_resource_operation(invocation, true, &doc(), &automerge::ROOT),
        Some(tilo_actions::InvocationUpdate::Cancelled { .. })
    ));
    assert!(app.session.borrow().live.is_none());

    let stale = tilo_actions::InvocationId::new();
    app.session_for = Some(id);
    app.session_generation = 99;
    app.session.borrow_mut().live = Some(LiveSession::Preview(SessionStatus::Connecting));
    app.pending_resource_operation = Some(PendingResourceOperation::Connect {
        invocation: stale,
        connection_id: id,
        session_generation: 98,
    });
    assert!(matches!(
        app.poll_resource_operation(stale, true, &doc(), &automerge::ROOT),
        Some(tilo_actions::InvocationUpdate::Cancelled { .. })
    ));
    assert!(
        app.session.borrow().live.is_some(),
        "a stale cancellation keeps the replacement session"
    );
}

#[test]
fn canonical_search_hit_opens_connection_settings_through_host() {
    use ui::tilo_resource::Resource as _;
    let mut app = hosted_sample();
    let hit = crate::resource::Remote::search(&app.state())
        .into_iter()
        .find(|hit| hit.title == "Office NAS")
        .unwrap();
    let address = hit.action.expect("search publishes its canonical action");
    app.harness()
        .invoke_action(&address, &serde_json::Map::new(), false, None)
        .unwrap();
    app.settle();
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert_eq!(
                surface.screen,
                Screen::Form {
                    editing: Some(Uuid::from_u128(3))
                }
            );
            assert!(surface.session.borrow().live.is_none());
        })
        .unwrap();
}

#[test]
fn every_screen_renders_in_the_window_shell() {
    for (w, h) in [(1280.0, 800.0), (800.0, 600.0)] {
        let app = framed(demo(), w, h);
        let shell = app.shell.geometry().clone();
        assert!(shell.content_column.width() > 0.0, "the shell laid out");
        assert!(
            shell.sidebar_column.width() > 0.0,
            "the rail stays docked at {w}x{h}"
        );
        for scenario in [
            "form",
            "form-tilo",
            "form-invalid",
            "edit",
            "session-rdp",
            "session-failed",
            "session-closed",
            "confirm-delete",
        ] {
            let mut staged = demo();
            staged.stage_scenario(Some(scenario));
            let mut app = framed(staged, w, h);
            Framed::harness_mut(&mut app)
                .relayout_at(ui::headless::viewport((w, h)))
                .expect("layout settles");
            assert!(app.built.is_some(), "{scenario} built its views");
        }
    }
}

#[test]
fn keys_map_to_the_remote_machine() {
    assert_eq!(
        ssh_key_bytes(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()),
        Some(vec![b'\r'])
    );
    assert_eq!(
        ssh_key_bytes(
            &Key::Character("c".into()),
            Some("c"),
            ModifiersState::CONTROL
        ),
        Some(vec![3])
    );
    assert_eq!(
        vnc_keysym(&Key::Named(NamedKey::Escape), None),
        Some(0xff1b)
    );
    assert_eq!(
        vnc_keysym(&Key::Character("a".into()), Some("a")),
        Some(0x61)
    );
    assert_eq!(tilo_keycode(KeyCode::KeyA), Some(30));
    assert_eq!(tilo_keycode(KeyCode::F13), None);
}

fn hosted_sample() -> TestApp<RemoteShell> {
    let mut app = TestApp::<RemoteShell>::new().unwrap();
    for invoke in RemoteShell::scenario(None) {
        app.invoke(invoke).unwrap();
    }
    app.settle();
    app
}

fn command_key(app: &mut TestApp<RemoteShell>, key: &str) {
    let modifier = if ui::Platform::current() == ui::Platform::MacOs {
        "super"
    } else {
        "ctrl"
    };
    app.harness().set_modifiers(&[modifier.into()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
}

fn activate(app: &mut TestApp<RemoteShell>, label: &str) {
    let node = app
        .harness()
        .a11y_node_named(label)
        .unwrap_or_else(|| panic!("missing {label}"));
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
}

fn row_action(app: &mut TestApp<RemoteShell>, id: ConnectionId, action: usize) {
    // Synthetic children are routed by the production exported tree, not by
    // calling Remote's private action dispatcher directly.
    fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<String> {
        if node.action_target.as_deref() == Some(target) {
            return node.name.clone();
        }
        node.children.iter().find_map(|n| find(n, target))
    }
    let target = format!("remote.connection:{id}.action.{action}");
    let named = app
        .harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            fn walk(widget: &dyn Widget, target: &str) -> Option<String> {
                widget
                    .accessibility()
                    .and_then(|n| find(&n, target))
                    .or_else(|| widget.children().into_iter().find_map(|c| walk(c, target)))
            }
            walk(surface, &target)
        })
        .flatten()
        .expect("row action is published");
    // Disambiguate identical labels through the retained row's semantic subtree.
    let name = app
        .state()
        .iter()
        .find(|c| c.id == id)
        .unwrap()
        .name
        .clone();
    let row = app.harness().a11y_node_named(&name).unwrap();
    let nodes = app.harness().a11y_nodes();
    let parent = &nodes.iter().find(|(node, _)| *node == row).unwrap().1;
    let child = parent
        .children()
        .iter()
        .find(|child| {
            nodes
                .iter()
                .any(|(id, n)| id == *child && n.label() == Some(named.as_str()))
        })
        .copied()
        .unwrap();
    app.harness()
        .perform_a11y_action(child, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
}

#[test]
fn live_empty_fixture_roster_and_local_presence_preserve_main_contract() {
    let mut app = TestApp::<RemoteShell>::new().unwrap();
    assert!(app.state().is_empty());
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert!(surface.tilo_devices.is_empty());
            assert!(surface.workspace.connections().is_empty());
        })
        .unwrap();
    let app = demo();
    assert_eq!(app.workspace.connections().len(), 8);
    assert_eq!(sample_connections(), sample_connections());
    for c in app.workspace.connections() {
        assert!(c.profile_issue().is_none(), "{}", c.name);
    }
    let rows = app.rows();
    assert_eq!(rows.iter().take_while(|r| r.favourite).count(), 2);
    assert_eq!(
        rows.iter().find(|r| r.name == "Build Box").unwrap().meta,
        "12m ago"
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.kind == ConnectionKind::Rdp)
            .unwrap()
            .meta,
        TransportReadiness::BackendUnavailable.label()
    );
    let stored = serde_json::to_string(&sample_connections()).unwrap();
    assert!(!stored.contains("presence"));
    assert!(stored.contains("last_used_ms"));
}

#[test]
fn retained_row_actions_dispatch_one_undo_step_without_connecting() {
    let mut app = hosted_sample();
    let id = Uuid::from_u128(3);
    assert!(!app.state().iter().find(|c| c.id == id).unwrap().favorite);
    row_action(&mut app, id, 1);
    assert!(app.state().iter().find(|c| c.id == id).unwrap().favorite);
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert_eq!(surface.screen, Screen::List);
            assert!(surface.pending_confirmation.is_none());
            assert!(surface.session.borrow().live.is_none());
        })
        .unwrap();
    assert!(app.undo());
    assert!(!app.state().iter().find(|c| c.id == id).unwrap().favorite);
    row_action(&mut app, id, 0);
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert_eq!(surface.screen, Screen::Form { editing: Some(id) });
            assert!(surface.session.borrow().live.is_none());
        })
        .unwrap();
    activate(&mut app, "Delete connection");
    assert!(app.state().iter().any(|c| c.id == id));
    activate(&mut app, "Delete connection");
    assert!(app.state().iter().all(|c| c.id != id));
    assert!(app.undo());
    assert!(app.state().iter().any(|c| c.id == id));
}

#[test]
fn retained_form_saves_and_keeps_credentials_out_of_document() {
    let mut app = TestApp::<RemoteShell>::new().unwrap();
    activate(&mut app, "New connection");
    for (field, value) in [("name", "Test"), ("host", "1.2.3.4"), ("secret", "hunter2")] {
        app.harness()
            .click_named(&format!("remote.field:{field}"))
            .unwrap();
        app.harness().type_text(value);
    }
    app.harness().click_named("remote.form.save").unwrap();
    app.settle();
    let saved = app.state();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "Test");
    assert_eq!(saved[0].port, 22);
    let bytes = app.read_document(|doc| doc.clone().save());
    assert!(!bytes.windows(b"hunter2".len()).any(|s| s == b"hunter2"));
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            let secret = surface
                .secrets
                .as_ref()
                .unwrap()
                .item(SSH_PASSWORD, &saved[0].id.to_string())
                .get_blocking()
                .unwrap()
                .unwrap();
            assert_eq!(secret.expose(), b"hunter2");
            assert!(surface.fields.secret.with(String::is_empty));
        })
        .unwrap();
}

#[test]
fn retained_inputs_validate_on_blur_accept_typing_and_filter_port() {
    let mut app = TestApp::<RemoteShell>::new().unwrap();
    activate(&mut app, "New connection");
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert_eq!(surface.problems, FormProblems::default())
        })
        .unwrap();
    app.harness().click_named("remote.field:name").unwrap();
    app.harness().click_named("remote.field:host").unwrap();
    app.settle();
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert!(surface.problems.name.is_some());
            assert!(surface.problems.host.is_none());
        })
        .unwrap();
    app.harness().click_named("remote.field:name").unwrap();
    app.harness().type_text("ab");
    app.harness().click_named("remote.field:port").unwrap();
    command_key(&mut app, "a");
    app.harness().type_text("2x2");
    app.settle();
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert_eq!(surface.fields.name.with(Clone::clone), "ab");
            assert_eq!(surface.fields.port.with(Clone::clone), "22");
            assert!(surface.problems.name.is_none());
        })
        .unwrap();
    assert!(
        app.state().is_empty(),
        "private draft has not created a record"
    );
}

#[test]
fn peer_changes_refresh_retained_list_and_close_deleted_session() {
    use ui::tilo_actions::Action as _;
    let mut app = hosted_sample();
    let id = Uuid::from_u128(1);
    app.merge_peer_edit(|doc| {
        crate::actions::rename_action(id, "Peer renamed")
            .apply(doc)
            .unwrap()
    });
    assert!(app.harness().a11y_node_named("Peer renamed").is_some());
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            surface.preview_session(ConnectionKind::Ssh, SessionStatus::Connected);
            assert_eq!(surface.session_for, Some(id));
        })
        .unwrap();
    app.merge_peer_edit(|doc| crate::actions::delete_action(id).apply(doc).unwrap());
    assert!(app.harness().a11y_node_named("Peer renamed").is_none());
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert!(surface.session_for.is_none());
            assert!(surface.session.borrow().live.is_none());
        })
        .unwrap();
}

#[test]
fn capture_connect_uses_preview_and_records_no_attempt() {
    let mut app = TestApp::<RemoteShell>::capture(None).unwrap();
    let before = app.read_document(|doc| doc.clone().get_heads());
    activate(&mut app, "Studio Node");
    activate(&mut app, "Connect");
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert!(matches!(
                surface.session.borrow().live,
                Some(LiveSession::Preview(SessionStatus::Connecting))
            ));
            assert!(surface.take_collab_requests().is_empty());
        })
        .unwrap();
    assert_eq!(before, app.read_document(|doc| doc.clone().get_heads()));
}

#[test]
fn typed_view_operation_and_nonretryable_transport_result_keep_their_contracts() {
    let mut app = demo();
    let id = Uuid::from_u128(1);
    let edit = tilo_actions::EncodedAction::encode_operation(
        crate::actions::REMOTE_OPERATION_TYPE,
        &crate::actions::RemoteOperation::EditConnection { connection_id: id },
    )
    .unwrap();
    let request = |kind| {
        ui::widget::ResourceOperation::local(
            tilo_actions::InvocationId::new(),
            kind,
            &edit.type_id,
            &edit.payload,
        )
    };
    assert!(matches!(
        app.start_operation(
            &request(tilo_actions::OperationKind::View),
            &mut doc(),
            &automerge::ROOT
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Completed(_))
    ));
    assert_eq!(app.screen, Screen::Form { editing: Some(id) });
    assert!(
        app.start_operation(
            &request(tilo_actions::OperationKind::Effect),
            &mut doc(),
            &automerge::ROOT
        )
        .is_err()
    );
    let id = app.first_of_kind(ConnectionKind::Rdp).unwrap();
    app.open_session(id);
    let invocation = tilo_actions::InvocationId::new();
    app.pending_resource_operation = Some(PendingResourceOperation::Connect {
        invocation,
        connection_id: id,
        session_generation: app.session_generation,
    });
    assert!(
        matches!(app.poll_resource_operation(invocation, false, &doc(), &automerge::ROOT), Some(tilo_actions::InvocationUpdate::Failed { error, .. }) if error.code == "remote.transport_unavailable" && !error.retryable)
    );
}

#[test]
fn real_tilo_attempt_records_milliseconds_once_and_live_surface_is_typed() {
    let mut app = hosted_sample();
    let before = app
        .state()
        .iter()
        .find(|c| c.id == Uuid::from_u128(2))
        .unwrap()
        .last_used_ms;
    activate(&mut app, "Studio Node");
    activate(&mut app, "Connect");
    let after = app
        .state()
        .iter()
        .find(|c| c.id == Uuid::from_u128(2))
        .unwrap()
        .last_used_ms;
    assert!(after.is_some_and(|at| at > 1_000_000_000_000));
    assert_ne!(before, after);
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            let rows = surface.live_session_rows();
            assert_eq!(rows.len(), 1);
            assert!(matches!(
                surface.session.borrow().live,
                Some(LiveSession::Tilo(_))
            ));
            surface.pump_session();
            surface.pump_session();
        })
        .unwrap();
    app.settle();
    assert_eq!(
        app.state()
            .iter()
            .find(|c| c.id == Uuid::from_u128(2))
            .unwrap()
            .last_used_ms,
        after
    );
}

#[serial_test::serial(clipboard)]
#[test]
fn retained_host_field_pastes_and_replaces_selection() {
    let mut app = TestApp::<RemoteShell>::new().unwrap();
    activate(&mut app, "New connection");
    app.harness().click_named("remote.field:host").unwrap();
    ui::text_selection::clipboard_copy("build-07.fleet.internal");
    command_key(&mut app, "v");
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert_eq!(
                surface.fields.host.with(Clone::clone),
                "build-07.fleet.internal"
            )
        })
        .unwrap();
    command_key(&mut app, "a");
    app.harness().type_text("x");
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert_eq!(surface.fields.host.with(Clone::clone), "x")
        })
        .unwrap();
}

#[test]
fn retained_filter_narrows_by_group_without_writing_shared_state() {
    let mut app = hosted_sample();
    let before = app.read_document(|doc| doc.clone().get_heads());
    command_key(&mut app, "f");
    app.harness().type_text("prod");
    app.settle();
    assert!(app.harness().a11y_node_named("Edge Gateway").is_some());
    assert!(app.harness().a11y_node_named("Backup Vault").is_some());
    assert!(app.harness().a11y_node_named("Office NAS").is_none());
    assert_eq!(before, app.read_document(|doc| doc.clone().get_heads()));
}

#[test]
fn hosted_confirmation_cancel_preserves_profile_and_clears_trap() {
    let mut app = hosted_sample();
    let id = Uuid::from_u128(3);
    row_action(&mut app, id, 0);
    activate(&mut app, "Delete connection");
    activate(&mut app, "Cancel");
    assert!(app.state().iter().any(|c| c.id == id));
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| {
            assert!(surface.dialog.is_none());
            assert!(surface.pending_confirmation.is_none());
        })
        .unwrap();
    app.harness().click_named("remote.form.save").unwrap();
    app.settle();
    app.harness()
        .with_widget_mut::<RemoteApp, _>(|surface| assert_eq!(surface.screen, Screen::List))
        .unwrap();
}
