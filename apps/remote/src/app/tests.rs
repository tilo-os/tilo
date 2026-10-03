use super::*;
use ui::headless::Framed;

fn doc() -> AutoCommit {
    AutoCommit::new()
}

/// An app holding the sample connections, as a scenario's actions leave it.
fn demo() -> RemoteApp {
    let mut workspace = Workspace::new();
    for connection in sample_connections(ui::clock::local_now().timestamp()) {
        workspace.upsert(connection);
    }
    let mut app = RemoteApp::from_parts(workspace, seed_tilo_devices());
    app.seeded = true;
    app
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
fn the_live_app_starts_empty_and_the_samples_are_complete() {
    eprintln!("phase live_new_starts_empty_and_writes_nothing");
    {
        let mut app = RemoteApp::new();
        assert!(app.workspace.connections().is_empty());
        assert!(app.tilo_devices.is_empty(), "no fabricated nearby devices");
        assert_eq!(app.screen, Screen::List);
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        assert!(
            RemoteDoc::list(&state).is_empty(),
            "an empty live app must not write machines into the document"
        );
    }

    eprintln!("phase sample_connections_are_valid_and_lived_in");
    {
        let connections = sample_connections(1_782_812_520);
        assert_eq!(connections.len(), 8);
        let mut names = std::collections::HashSet::new();
        for connection in &connections {
            assert!(names.insert(connection.name.as_str()), "duplicate name");
            assert!(!connection.host.is_empty());
            assert!(!connection.group.is_empty());
            if connection.kind == ConnectionKind::Tilo {
                assert_eq!(connection.port, 0);
                assert_eq!(connection.auth, AuthMethod::DeviceIdentity);
            } else {
                assert_ne!(connection.port, 0, "network transports need a port");
            }
            assert!(connection.profile_issue().is_none(), "{}", connection.name);
        }
        assert!(connections.iter().any(|c| c.favorite));
        assert!(connections.iter().any(|c| c.last_used.is_none()));
        let again = sample_connections(1_782_812_520);
        assert_eq!(
            connections, again,
            "a scenario stages the same frame every run"
        );
    }
}

#[test]
fn rows_say_when_a_machine_was_used_or_why_it_cannot_open() {
    let app = demo();
    let rows = app.rows();
    assert_eq!(rows.len(), 8);
    assert!(
        rows.iter().take_while(|row| row.favourite).count() == 2,
        "pinned connections lead the list"
    );
    let studio = rows
        .iter()
        .find(|row| row.name == "Studio desktop")
        .unwrap();
    assert_eq!(studio.tag, "Tilo");
    assert_eq!(studio.meta, "1 min ago");
    let workstation = rows
        .iter()
        .find(|row| row.name == "Windows workstation")
        .unwrap();
    assert_eq!(workstation.meta, "Not supported");
    for row in &rows {
        assert!(!row.address.contains(" · "), "{}", row.address);
        assert!(!row.address.starts_with("Tilo device"));
    }

    eprintln!("phase pinned_destination_lists_only_pinned_connections");
    let mut app = demo();
    app.apply(Action::Show(Destination::Pinned), &mut doc());
    assert!(app.rows().iter().all(|row| row.favourite));
    assert_eq!(app.rows().len(), 2);
}

#[test]
fn the_form_saves_through_the_upsert_action_and_reports_problems_at_save() {
    eprintln!("phase an_incomplete_draft_names_each_problem_by_its_field");
    {
        let mut app = demo();
        let mut state = doc();
        app.apply(Action::NewConnection, &mut state);
        assert_eq!(app.screen, Screen::Form { editing: None });
        assert_eq!(app.problems, FormProblems::default(), "quiet until saved");
        app.fields.port.set(String::new());
        app.apply(Action::SaveForm, &mut state);
        assert!(app.problems.name.is_some());
        assert!(app.problems.host.is_some());
        assert!(app.problems.port.is_some());
        assert_eq!(app.screen, Screen::Form { editing: None }, "still editing");
        assert!(app.last_dispatched().is_none(), "nothing written");
    }

    eprintln!("phase a_complete_draft_is_written_and_marked_in_the_list");
    {
        let mut app = demo();
        let mut state = doc();
        app.apply(Action::NewConnection, &mut state);
        app.fields.name.set("Media server".into());
        app.fields.host.set("media.home.arpa".into());
        app.fields.user.set("media".into());
        app.apply(Action::SetAuth(AuthMethod::PublicKey), &mut state);
        app.apply(Action::SaveForm, &mut state);
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
        assert!(
            RemoteDoc::list(&state)
                .iter()
                .any(|c| c.name == "Media server"),
            "the upsert action reached the document"
        );
    }

    eprintln!("phase switching_protocol_keeps_typed_text_and_resets_the_port");
    {
        let mut app = demo();
        let mut state = doc();
        app.apply(Action::NewConnection, &mut state);
        app.fields.name.set("Lab box".into());
        app.apply(Action::SetKind(ConnectionKind::Vnc), &mut state);
        assert_eq!(app.fields.name.with(Clone::clone), "Lab box");
        assert_eq!(
            app.fields.port.with(Clone::clone),
            "22",
            "a typed port is kept"
        );
        app.fields.port.set(String::new());
        app.apply(Action::SetKind(ConnectionKind::Vnc), &mut state);
        assert_eq!(app.fields.port.with(Clone::clone), "5900");
        app.apply(Action::SetKind(ConnectionKind::Tilo), &mut state);
        assert_eq!(app.draft.auth, AuthMethod::DeviceIdentity);
        app.apply(Action::PickTiloDevice(1), &mut state);
        assert_eq!(app.fields.name.with(Clone::clone), "Workshop laptop");
        assert!(app.form_model(None).devices[1].picked);
    }

    eprintln!("phase editing_preserves_favourite_and_history");
    {
        let mut app = demo();
        let mut state = doc();
        let id = app.first_of_kind(ConnectionKind::Ssh).unwrap();
        let before = app.workspace.get(id).unwrap().clone();
        app.apply(Action::EditConnection(id), &mut state);
        assert_eq!(app.fields.host.with(Clone::clone), before.host);
        app.fields.name.set("Build server (rack 2)".into());
        app.apply(Action::SaveForm, &mut state);
        let after = app.workspace.get(id).unwrap();
        assert_eq!(after.name, "Build server (rack 2)");
        assert_eq!(after.favorite, before.favorite);
        assert_eq!(after.last_used, before.last_used);
    }
}

#[test]
fn favourites_and_deletion_go_through_the_document_actions() {
    let mut app = demo();
    let mut state = doc();
    let id = app.first_of_kind(ConnectionKind::Vnc).unwrap();
    assert!(!app.workspace.get(id).unwrap().favorite);
    app.apply(Action::ToggleFavorite(id), &mut state);
    assert!(app.workspace.get(id).unwrap().favorite);
    assert_eq!(
        app.last_dispatched(),
        Some(&crate::actions::encode(
            &crate::actions::set_favorite_action(id, true)
        ))
    );

    app.apply(Action::RequestDelete(id), &mut state);
    assert!(pending_confirmation_is(&app, id));
    assert!(app.dialog.as_ref().is_some_and(ModalDialog::is_open));
    assert!(
        app.workspace.get(id).is_some(),
        "nothing removed before confirming"
    );
    app.apply(Action::CancelPending, &mut state);
    assert!(app.dialog.is_none());
    assert!(app.workspace.get(id).is_some());

    app.apply(Action::RequestDelete(id), &mut state);
    app.apply(Action::ConfirmPending, &mut state);
    assert!(app.workspace.get(id).is_none());
    assert_eq!(
        app.last_dispatched(),
        Some(&crate::actions::encode(&crate::actions::delete_action(id)))
    );
}

#[test]
fn connecting_asks_first_revalidates_and_explains_unavailable_transports() {
    eprintln!("phase connect_requires_confirmation_before_opening_a_broker_route");
    {
        let mut app = demo();
        let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
        app.apply(Action::RequestConnect(id), &mut doc());
        assert!(pending_confirmation_is(&app, id));
        assert!(app.session.borrow().live.is_none());
        assert!(app.session.borrow().collab_outbox.is_empty());
    }

    eprintln!("phase confirmation_revalidates_a_collaboratively_changed_target");
    {
        let mut app = demo();
        let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
        app.apply(Action::RequestConnect(id), &mut doc());
        app.workspace.get_mut(id).unwrap().host = SecretKey::generate().public().to_string();
        app.apply(Action::ConfirmPending, &mut doc());
        assert!(app.session.borrow().live.is_none());
        assert_eq!(app.toasts.len(), 1, "the person learns why nothing opened");
    }

    eprintln!("phase unavailable_transports_open_a_recovery_state_without_a_socket");
    {
        let mut app = demo();
        let ssh = app.first_of_kind(ConnectionKind::Ssh).unwrap();
        if !crate::session::live_sessions_enabled() {
            app.apply(Action::RequestConnect(ssh), &mut doc());
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
        app.apply(Action::RequestConnect(rdp), &mut doc());
        assert_eq!(app.screen, Screen::Session { id: rdp });
        assert_eq!(
            app.status_model(rdp).action,
            Some(("Edit connection", Action::EditConnection(rdp)))
        );
        app.apply(Action::Disconnect, &mut doc());
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
    }
}

#[test]
fn a_session_outcome_is_recorded_once() {
    let mut app = demo();
    let id = app.first_of_kind(ConnectionKind::Ssh).unwrap();
    app.session_for = Some(id);
    app.session_generation = 7;
    app.session.borrow_mut().live = Some(LiveSession::Error("Connection refused".into()));
    app.pump_session();
    app.pump_session();
    assert_eq!(app.pending_records.len(), 1);
    assert_eq!(app.workspace.get(id).unwrap().presence, Presence::Offline);
    let mut state = doc();
    RemoteDoc::upsert(&mut state, app.workspace.get(id).unwrap());
    app.flush_records(&mut state);
    let stored = RemoteDoc::list(&state);
    assert_eq!(stored[0].presence, Presence::Offline);
    assert_eq!(
        stored[0].last_used,
        app.workspace.get(id).unwrap().last_used
    );

    eprintln!("phase a_preview_records_nothing");
    let mut app = demo();
    app.preview_session(ConnectionKind::Vnc, SessionStatus::Failed("refused".into()));
    app.pump_session();
    assert!(app.pending_records.is_empty());
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
    app.apply(Action::Disconnect, &mut doc());
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
fn a_picked_search_hit_shows_its_connection() {
    let mut app = demo();
    let id = app.first_of_kind(ConnectionKind::Vnc).unwrap();
    app.apply(Action::NewConnection, &mut doc());
    app.open_requests()
        .borrow_mut()
        .push(format!("remote.connection:{id}"));
    app.open_found();
    assert_eq!(app.screen, Screen::List);
    assert_eq!(app.selected, Some(id));
    assert!(app.rows().iter().any(|row| row.id == id && row.selected));
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
            staged.stage(Some(scenario));
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
