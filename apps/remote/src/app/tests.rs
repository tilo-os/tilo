use super::*;
use crate::model::Remote;
use crate::resource::RemoteShell;
use ui::framework::ResourceApp as _;
use ui::headless::Framed;
use ui::prelude::TestApp;
use ui::tilo_resource::{Atom, Invoke};

/// What `app` submitted, comparable: each invocation's action and arguments.
fn sent(app: &RemoteApp) -> Vec<(&'static str, serde_json::Map<String, serde_json::Value>)> {
    app.submitted
        .borrow()
        .iter()
        .map(|invoke| (invoke.id, invoke.arguments.clone()))
        .collect()
}

/// `invoke`, comparable as [`sent`] lists it.
fn as_sent(invoke: &Invoke<Remote>) -> (&'static str, serde_json::Map<String, serde_json::Value>) {
    (invoke.id, invoke.arguments.clone())
}

/// The field edit pinning or unpinning `id`.
fn pin(id: ConnectionId, pinned: bool) -> Invoke<Remote> {
    let item = Remote::item(id);
    Invoke::edit(&Remote::paths().connections.item(&item).favorite.set(pinned)).unwrap()
}

/// The field edit removing `id`.
fn removal(id: ConnectionId) -> Invoke<Remote> {
    Invoke::edit(&Remote::paths().connections.remove(&Remote::item(id))).unwrap()
}

/// The insert saving `connection` as the `at`th one.
fn insertion(connection: &Connection, at: usize) -> Invoke<Remote> {
    let item = Remote::item(connection.id);
    Invoke::edit(&Remote::paths().connections.insert_as(&item, at, connection)).unwrap()
}

/// An app holding the sample connections and the legacy RDP profile, as
/// the `session-rdp` scenario's actions leave it.
fn demo() -> RemoteApp {
    RemoteApp::sample()
}

/// A hosted Remote: the framework's live app with its resource.
fn hosted() -> TestApp<RemoteShell> {
    TestApp::<RemoteShell>::new().unwrap()
}

fn hosted_sample() -> TestApp<RemoteShell> {
    let mut app = hosted();
    for invoke in RemoteShell::scenario(None) {
        app.invoke(invoke).unwrap();
    }
    app.settle();
    app
}

/// The semantic node the window publishes for `target`, if any.
fn find_node(app: &mut TestApp<RemoteShell>, target: &str) -> Option<ui::a11y::AccessNode> {
    fn find<'a>(node: &'a ui::a11y::AccessNode, target: &str) -> Option<&'a ui::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(target)
            || node.semantic_key.as_deref() == Some(target)
        {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    fn walk(node: &ui::headless::InspectionNode, target: &str) -> Option<ui::a11y::AccessNode> {
        node.semantics
            .as_ref()
            .and_then(|semantics| find(semantics, target).cloned())
            .or_else(|| node.children.iter().find_map(|child| walk(child, target)))
    }
    walk(&app.harness().inspection().root, target)
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
        let app = RemoteApp::new();
        assert!(app.workspace.connections().is_empty());
        assert!(app.tilo_devices.is_empty(), "no fabricated nearby devices");
        assert_eq!(app.screen, Screen::List);
        assert!(
            hosted().state().connections.is_empty(),
            "an empty live app must not write machines into the document"
        );
    }

    eprintln!("phase sample_connections_are_valid_and_lived_in");
    {
        let connections = sample_connections();
        assert_eq!(connections.len(), 8);
        let mut names = std::collections::HashSet::new();
        for connection in &connections {
            assert!(names.insert(connection.name.as_str()), "duplicate name");
            assert!(!connection.host.0.is_empty());
            assert!(!connection.group.0.is_empty());
            if connection.kind == ConnectionKind::Tilo {
                assert_eq!(connection.port, 0);
                assert_eq!(connection.auth, AuthMethod::DeviceIdentity);
            } else {
                assert_ne!(connection.port, 0, "network transports need a port");
            }
            assert!(connection.profile_issue().is_none(), "{}", connection.name);
        }
        assert!(connections.iter().any(|c| c.favorite));
        assert!(
            connections.iter().all(|c| c.last_used_ms.is_some()),
            "every sample machine has been used"
        );
        assert!(
            connections
                .iter()
                .all(|c| crate::session::connection_readiness(c)
                    != TransportReadiness::BackendUnavailable),
            "the list holds only machines Remote opens"
        );
        assert_eq!(
            connections,
            sample_connections(),
            "a scenario stages the same frame every run"
        );
        let sample = Sample::new();
        assert!(
            connections
                .iter()
                .all(|c| sample.presence.contains_key(&c.id)),
            "a capture knows whether each sample machine answered"
        );
    }
}

#[test]
fn rows_say_when_a_machine_was_used_or_why_it_cannot_open() {
    let app = demo();
    let rows = app.rows();
    assert_eq!(rows.len(), 9);
    assert!(
        rows.iter().take_while(|row| row.favourite).count() == 2,
        "pinned connections lead the list"
    );
    let studio = rows
        .iter()
        .find(|row| row.name == "Studio desktop")
        .unwrap();
    assert_eq!(studio.kind, ConnectionKind::Tilo);
    assert_eq!(studio.address, "Tilo device", "no endpoint id to read");
    assert_eq!(studio.meta, "1 min ago");
    let build = rows.iter().find(|row| row.name == "Build server").unwrap();
    assert_eq!(build.address, "admin@build.lab.internal");
    assert_eq!(
        build.meta, "12 min ago",
        "what this device lacks is said on opening, not in the list"
    );
    let pi = rows
        .iter()
        .find(|row| row.name == "Raspberry Pi cluster")
        .unwrap();
    assert_eq!(
        pi.presence,
        Presence::Offline,
        "reachability is this device's observation"
    );
    let workstation = rows
        .iter()
        .find(|row| row.name == "Windows workstation")
        .unwrap();
    assert_eq!(workstation.meta, "RDP isn't supported");
    for row in &rows {
        assert!(!row.address.contains(" · "), "{}", row.address);
    }

    eprintln!("phase pinned_destination_lists_only_pinned_connections");
    let mut app = demo();
    app.apply(Action::Show(Destination::Pinned));
    assert!(app.rows().iter().all(|row| row.favourite));
    assert_eq!(app.rows().len(), 2);
}

#[test]
fn the_form_saves_through_the_resource_and_reports_problems_at_save() {
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
        assert!(app.submitted.borrow().is_empty(), "nothing written");
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
        let at = app.workspace.connections().len() - 1;
        assert_eq!(
            sent(&app),
            [as_sent(&insertion(saved, at))],
            "the new connection went through the resource's door"
        );
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
        assert_eq!(app.fields.name.with(Clone::clone), "Workshop laptop");
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
}

/// Editing a saved connection writes only the fields the form changed,
/// so its pin and when it was last used stay.
#[test]
fn editing_preserves_favourite_and_history() {
    let mut app = demo();
    let id = app.first_of_kind(ConnectionKind::Ssh).unwrap();
    let before = app.workspace.get(id).unwrap().clone();
    app.fields.secret.set("typed before".into());
    app.apply(Action::EditConnection(id));
    assert_eq!(app.fields.host.with(Clone::clone), before.host.0);
    assert!(
        app.fields.secret.with(String::is_empty),
        "a saved credential is never read back into the form"
    );
    app.fields.name.set("Build server (rack 2)".into());
    app.apply(Action::SaveForm);
    let after = app.workspace.get(id).unwrap();
    assert_eq!(after.name, "Build server (rack 2)");
    assert_eq!(after.favorite, before.favorite);
    assert_eq!(after.last_used_ms, before.last_used_ms);
    let item = Remote::item(id);
    let name = Remote::paths().connections.item(&item).name;
    let edited = name.edits(&before.name, &after.name);
    assert_eq!(
        sent(&app),
        [as_sent(&Invoke::group(
            edited.iter().map(|edit| Invoke::edit(edit).unwrap())
        ))],
        "an edit submits only the fields it changed, as one step"
    );
}

#[test]
fn favourites_and_deletion_go_through_the_document_actions() {
    let mut app = demo();
    let id = app.first_of_kind(ConnectionKind::Vnc).unwrap();
    assert!(!app.workspace.get(id).unwrap().favorite);
    app.apply(Action::ToggleFavorite(id));
    assert!(app.workspace.get(id).unwrap().favorite);
    assert_eq!(sent(&app).last(), Some(&as_sent(&pin(id, true))));

    app.apply(Action::RequestDelete(id));
    assert!(pending_confirmation_is(&app, id));
    assert!(app.dialog.as_ref().is_some_and(ModalDialog::is_open));
    assert!(
        app.workspace.get(id).is_some(),
        "nothing removed before confirming"
    );
    app.apply(Action::CancelPending);
    assert!(app.dialog.is_none());
    assert!(app.workspace.get(id).is_some());

    app.apply(Action::RequestDelete(id));
    app.apply(Action::ConfirmPending);
    assert!(app.workspace.get(id).is_none());
    assert_eq!(sent(&app).last(), Some(&as_sent(&removal(id))));
}

#[test]
fn hosted_controls_change_the_resource_and_keep_credentials_on_the_device() {
    eprintln!("phase pinning_is_one_undo_step");
    {
        let mut app = hosted_sample();
        let id = sample_id(3);
        let pinned = |app: &TestApp<RemoteShell>| {
            app.state()
                .list()
                .into_iter()
                .find(|connection| connection.id == id)
                .unwrap()
                .favorite
        };
        assert!(!pinned(&app));
        app.invoke(pin(id, true)).unwrap();
        app.settle();
        assert!(pinned(&app), "the pin reached the resource");
        assert!(app.undo());
        assert!(!pinned(&app), "one undo takes it back");
    }

    eprintln!("phase a_collaborators_deletion_leaves_no_ghost_row");
    {
        let mut app = hosted_sample();
        let id = sample_id(5);
        let password = || ui::storage::secrets(RemoteShell::ID).item(SSH_PASSWORD, &id.to_string());
        password().put_blocking(b"hunter2").unwrap();
        let delete = || {
            Remote::DELETE_CONNECTION.with(crate::resource::remote::DeleteConnection {
                connection: Remote::item(id),
            })
        };
        assert!(
            app.invoke(delete()).is_err(),
            "Undo cannot bring the password back, so deleting asks first"
        );
        app.invoke_confirmed(delete()).unwrap();
        app.settle();
        assert!(
            app.state()
                .list()
                .iter()
                .all(|connection| connection.id != id)
        );
        assert!(
            find_node(&mut app, &format!("remote.connection:{id}")).is_none(),
            "a deleted connection leaves no row behind"
        );
        assert!(
            password().get_blocking().unwrap().is_none(),
            "a connection deleted through any door takes this device's password with it"
        );
    }

    eprintln!("phase save_writes_the_profile_and_keeps_the_password_on_this_device");
    {
        let mut app = demo();
        app.secrets = Some(ui::storage::secrets(RemoteShell::ID));
        app.apply(Action::NewConnection);
        app.fields.name.set("Test".into());
        app.fields.host.set("1.2.3.4".into());
        app.fields.secret.set("hunter2".into());
        app.apply(Action::SaveForm);
        let saved = app
            .workspace
            .connections()
            .iter()
            .find(|connection| connection.name == "Test")
            .expect("saved")
            .clone();
        assert_eq!(saved.port, 22);
        let at = app.workspace.connections().len() - 1;
        assert_eq!(
            sent(&app),
            [as_sent(&insertion(&saved, at))],
            "only the profile goes through the resource's door"
        );
        assert!(
            !format!("{:?}", sent(&app)).contains("hunter2"),
            "the password never enters the document"
        );
        assert!(
            app.fields.secret.with(String::is_empty),
            "the form lets go of it"
        );
        let secret = ui::storage::secrets(RemoteShell::ID)
            .item(SSH_PASSWORD, &saved.id.to_string())
            .get_blocking()
            .unwrap()
            .expect("the password is in this device's secret store");
        assert_eq!(secret.expose(), b"hunter2");

        app.apply(Action::RequestDelete(saved.id));
        app.apply(Action::ConfirmPending);
        assert!(
            ui::storage::secrets(RemoteShell::ID)
                .item(SSH_PASSWORD, &saved.id.to_string())
                .get_blocking()
                .unwrap()
                .is_none(),
            "a deleted connection's password goes with it"
        );
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
        app.workspace.get_mut(id).unwrap().host = Atom(SecretKey::generate().public().to_string());
        app.apply(Action::ConfirmPending);
        assert!(app.session.borrow().live.is_none());
        assert!(
            ui::attention::entries().iter().any(|entry| entry.what()
                == "This connection changed while you were asked. Review it, then try again."),
            "the person learns why nothing opened, in the ledger"
        );
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
            assert!(
                app.submitted.borrow().is_empty(),
                "a session that never started marks nothing used"
            );
        }
        let rdp = app.first_of_kind(ConnectionKind::Rdp).unwrap();
        app.apply(Action::RequestConnect(rdp));
        assert_eq!(app.screen, Screen::Session { id: rdp });
        assert_eq!(
            app.status_model(rdp).action,
            Some(("Edit connection", Action::EditConnection(rdp)))
        );
        // Ending a live session asks first: its scrollback goes with it.
        app.apply(Action::Disconnect);
        assert!(app.dialog.is_some(), "closing a session asks first");
        app.apply(Action::ConfirmPending);
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
fn leaving_a_session_that_is_not_live_closes_it() {
    let mut app = demo();
    app.stage(Some("session-closed"));
    assert!(app.session_for.is_some());
    app.apply(Action::Show(Destination::Connections));
    assert!(app.session_for.is_none(), "the rail no longer offers it");
    assert_eq!(app.screen, Screen::List);
    assert_eq!(app.destination, Destination::Connections);

    let mut app = demo();
    app.stage(Some("session-ssh"));
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
        app.stage(Some(scenario));
        let id = app.first_of_kind(kind).unwrap();
        let before = app.workspace.presence_of(id);
        assert!(matches!(app.content(), Content::Surface(shown) if shown == id));
        app.pump_session();
        assert!(
            app.submitted.borrow().is_empty(),
            "{scenario} wrote nothing"
        );
        assert_eq!(
            app.workspace.presence_of(id),
            before,
            "{scenario} observed no machine"
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
fn opening_a_session_marks_the_machine_used_and_this_device_observes_it() {
    eprintln!("phase opening_marks_last_used_for_every_device");
    {
        let mut app = demo();
        let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
        app.open_session(id);
        let now = app.now_ms();
        assert_eq!(app.workspace.get(id).unwrap().last_used_ms, Some(now));
        assert_eq!(
            sent(&app),
            [as_sent(&Remote::MARK_USED.with(
                crate::resource::remote::MarkUsed {
                    connection: Remote::item(id),
                    at_ms: now,
                }
            ))],
            "when it was last used follows the person to their devices"
        );
    }

    eprintln!("phase the_outcome_is_observed_on_this_device_only");
    {
        let mut app = demo();
        let id = app.first_of_kind(ConnectionKind::Ssh).unwrap();
        assert_eq!(app.workspace.presence_of(id), Presence::Online);
        app.session_for = Some(id);
        app.session.borrow_mut().live = Some(LiveSession::Error("Connection refused".into()));
        app.pump_session();
        app.pump_session();
        assert_eq!(app.workspace.presence_of(id), Presence::Offline);
        assert!(
            app.submitted.borrow().is_empty(),
            "whether a machine answered is never written to the document"
        );
    }

    eprintln!("phase a_capture_reaches_nothing");
    {
        let mut app = demo();
        app.capture = true;
        let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
        app.open_session(id);
        assert!(matches!(
            app.session.borrow().live,
            Some(LiveSession::Preview(SessionStatus::Connecting))
        ));
        assert!(app.session.borrow().collab_outbox.is_empty());
        assert!(app.submitted.borrow().is_empty());
    }

    eprintln!("phase a_preview_observes_nothing");
    {
        let mut app = demo();
        app.preview_session(ConnectionKind::Vnc, SessionStatus::Failed("refused".into()));
        let id = app.session_for.unwrap();
        let before = app.workspace.presence_of(id);
        app.pump_session();
        assert_eq!(app.workspace.presence_of(id), before);
    }
}

#[test]
fn credentials_are_filed_by_what_the_connection_signs_in_with() {
    assert_eq!(
        secret_purpose(ConnectionKind::Ssh, AuthMethod::Password),
        SSH_PASSWORD
    );
    assert_eq!(
        secret_purpose(ConnectionKind::Ssh, AuthMethod::PublicKey),
        KEY_PASSPHRASE
    );
    assert_eq!(
        secret_purpose(ConnectionKind::Vnc, AuthMethod::Password),
        VNC_PASSWORD
    );
}

#[test]
fn tilo_sessions_use_the_authenticated_broker_and_consent() {
    let mut app = demo();
    let target = SecretKey::generate().public();
    let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
    app.workspace.get_mut(id).unwrap().host = Atom(target.to_string());
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
    app.apply(Action::ConfirmPending);
    assert!(matches!(
        app.take_collab_requests().as_slice(),
        [ui::collaboration::CollabRequest::CloseWorkspaceControl { .. }]
    ));
}

/// A unit of the target's stream lost on the way leaves a gap in its
/// sequence numbers: the session holds back the deltas after it and asks the
/// target for a keyframe through the consented route.
#[test]
fn a_tilo_stream_gap_asks_the_target_for_a_keyframe() {
    const IDR: [u8; 12] = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x65, 0x88, 0x84];
    const DELTA: [u8; 6] = [0, 0, 0, 1, 0x41, 0x9a];
    let mut app = demo();
    let target = SecretKey::generate().public();
    let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
    app.workspace.get_mut(id).unwrap().host = Atom(target.to_string());
    app.open_session(id);
    let session_id = match app.session.borrow().live.as_ref() {
        Some(LiveSession::Tilo(session)) => session.id,
        _ => panic!("Tilo session"),
    };
    app.handle_collab_event(&ui::collaboration::CollabEvent::WorkspaceControl {
        session_id,
        event: runtime_collab::WorkspaceControlEvent::StreamStarted {
            surface: 4,
            width: 1280,
            height: 720,
        },
    });
    let _ = app.take_collab_requests();
    let frame = |app: &mut RemoteApp, seq: u64, unit: &[u8]| {
        app.handle_collab_event(&ui::collaboration::CollabEvent::WorkspaceControl {
            session_id,
            event: runtime_collab::WorkspaceControlEvent::Frame {
                surface: 4,
                encoded_h264: unit.to_vec(),
                timestamp: seq,
                seq,
            },
        });
        app.take_collab_requests()
            .into_iter()
            .filter_map(|request| match request {
                ui::collaboration::CollabRequest::WorkspaceControlInput {
                    event: runtime_collab::WorkspaceControlInput::RequestKeyframe { seq },
                    ..
                } => Some(seq),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(frame(&mut app, 0, &IDR).is_empty());
    assert!(frame(&mut app, 1, &DELTA).is_empty());
    // The decoder may still be starting up on its first unit (a real one
    // waits for its first picture): the gap is noticed as unit 3 arrives,
    // not once the worker reaches it.
    assert_eq!(
        frame(&mut app, 3, &DELTA),
        [3],
        "one request for the gap, naming the unit after it"
    );
    assert!(
        frame(&mut app, 5, &IDR).is_empty(),
        "the keyframe starts the chain again"
    );
    assert!(
        frame(&mut app, 6, &DELTA).is_empty(),
        "and the deltas after it continue it"
    );
}

/// A demo app with a connected Tilo session showing a stream.
fn connected_tilo_session() -> RemoteApp {
    let mut app = demo();
    let target = SecretKey::generate().public();
    let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
    app.workspace.get_mut(id).unwrap().host = Atom(target.to_string());
    app.open_session(id);
    let session_id = match app.session.borrow().live.as_ref() {
        Some(LiveSession::Tilo(session)) => session.id,
        _ => panic!("Tilo session"),
    };
    app.handle_collab_event(&ui::collaboration::CollabEvent::WorkspaceControl {
        session_id,
        event: runtime_collab::WorkspaceControlEvent::StreamStarted {
            surface: 4,
            width: 1280,
            height: 720,
        },
    });
    let _ = app.take_collab_requests();
    app
}

/// The keys `app` sent its Tilo session since last asked, as evdev keycode
/// and whether it went down.
fn sent_keys(app: &mut RemoteApp) -> Vec<(u32, bool)> {
    app.take_collab_requests()
        .into_iter()
        .filter_map(|request| match request {
            ui::collaboration::CollabRequest::WorkspaceControlInput {
                event:
                    runtime_collab::WorkspaceControlInput::Key {
                        keycode, pressed, ..
                    },
                ..
            } => Some((keycode, pressed)),
            _ => None,
        })
        .collect()
}

/// A key event for the physical key `code`.
fn key_at(code: winit::keyboard::KeyCode, state: ElementState) -> KeyEvent {
    let mut event = ui::synthetic_input::key_event(Key::Named(NamedKey::Control), None, state);
    event.physical_key = PhysicalKey::Code(code);
    event
}

/// A key held on the remote machine when the session pane loses the
/// keyboard (a click elsewhere, another window) is released there: its
/// release goes to whatever has the keyboard now, and the remote side would
/// hold the key, repeating it, until the person pressed it again.
#[test]
fn a_key_held_when_the_pane_loses_the_keyboard_is_released_on_the_remote_machine() {
    let mut app = connected_tilo_session();
    let keys = sent_keys;
    let key = key_at;
    let mut pane = SessionPane::new(Rc::clone(&app.session));
    let ctrl = winit::keyboard::KeyCode::ControlLeft;
    let pressed = ElementState::Pressed;
    assert!(pane.handle_key(&key(ctrl, pressed), ModifiersState::empty()));
    assert_eq!(keys(&mut app), [(29, true)], "Ctrl goes down there");

    pane.set_focus(false);
    assert_eq!(
        keys(&mut app),
        [(29, false)],
        "and comes up when the pane lets go"
    );
    pane.set_focus(false);
    assert!(keys(&mut app).is_empty(), "once");

    pane.set_focus(true);
    assert!(pane.handle_key(&key(ctrl, pressed), ModifiersState::empty()));
    assert!(pane.handle_key(&key(ctrl, ElementState::Released), ModifiersState::empty()));
    assert_eq!(keys(&mut app), [(29, true), (29, false)]);
    pane.set_focus(false);
    assert!(
        keys(&mut app).is_empty(),
        "a key released already is not released again"
    );
}

/// The window takes some keys before the session pane sees them: Ctrl+K
/// opens the action palette, which then keeps every key release, Ctrl's
/// among them, from the window behind it. A key the pane pressed on the
/// remote machine is still released there when it comes up; otherwise Ctrl
/// stays held on the remote side and the next letters typed there arrive as
/// Ctrl chords (Ctrl+W closing a tab, Ctrl+Q quitting).
#[test]
fn a_key_whose_release_the_window_takes_is_still_released_on_the_remote_machine() {
    use winit::keyboard::KeyCode;
    let mut app = connected_tilo_session();
    let pane = SessionPane::new(Rc::clone(&app.session));
    let released = |code| ui::widget::PhysicalInput::Key {
        key: PhysicalKey::Code(code),
        state: ElementState::Released,
    };
    assert!(pane.handle_key(
        &key_at(KeyCode::ControlLeft, ElementState::Pressed),
        ModifiersState::empty()
    ));
    assert_eq!(sent_keys(&mut app), [(29, true)], "Ctrl goes down there");

    // The palette took the release; the window still observed it.
    Widget::observe_physical_input(&mut app, released(KeyCode::ControlLeft));
    assert_eq!(
        sent_keys(&mut app),
        [(29, false)],
        "Ctrl comes up there with the key"
    );
    pane.handle_key(
        &key_at(KeyCode::ControlLeft, ElementState::Released),
        ModifiersState::empty(),
    );
    assert!(
        sent_keys(&mut app).is_empty(),
        "a release that reaches the pane after all is not sent twice"
    );
    Widget::observe_physical_input(&mut app, released(KeyCode::ShiftLeft));
    assert!(
        sent_keys(&mut app).is_empty(),
        "a key the pane never pressed there is not released there"
    );

    // A dialog over the window or History holding it: the window observes
    // the keyboard as lost.
    assert!(pane.handle_key(
        &key_at(KeyCode::ShiftLeft, ElementState::Pressed),
        ModifiersState::empty()
    ));
    assert_eq!(sent_keys(&mut app), [(42, true)]);
    Widget::observe_physical_input(&mut app, ui::widget::PhysicalInput::Focused(false));
    assert_eq!(sent_keys(&mut app), [(42, false)]);
}

#[test]
fn resource_operations_settle_only_their_own_session() {
    let mut app = demo();
    let id = app.first_of_kind(ConnectionKind::Tilo).unwrap();
    app.session_for = Some(id);
    app.session_generation = 42;
    app.session.borrow_mut().live = Some(LiveSession::Preview(SessionStatus::Connecting));
    app.pending_resource_operation = Some(PendingResourceOperation::Connect {
        reply: None,
        connection_id: id,
        session_generation: 42,
    });
    assert!(matches!(
        app.pending_outcome(true),
        Some(Err(ref message)) if message.contains("cancelled")
    ));
    assert!(app.session.borrow().live.is_none());

    app.session_for = Some(id);
    app.session_generation = 99;
    app.session.borrow_mut().live = Some(LiveSession::Preview(SessionStatus::Connecting));
    app.pending_resource_operation = Some(PendingResourceOperation::Connect {
        reply: None,
        connection_id: id,
        session_generation: 98,
    });
    assert!(matches!(
        app.pending_outcome(true),
        Some(Err(ref message)) if message.contains("cancelled")
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
    app.apply(Action::NewConnection);
    app.open_requests()
        .borrow_mut()
        .push(crate::resource::connection_resource_id(id));
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
    assert_eq!(vnc_keysym(&Key::Named(NamedKey::Escape)), Some(0xff1b));
    assert_eq!(vnc_keysym(&Key::Character("a".into())), Some(0x61));
    // Ctrl+C is the Control key held, then C: both reach a VNC desktop.
    assert_eq!(vnc_keysym(&Key::Named(NamedKey::Control)), Some(0xffe3));
    assert_eq!(vnc_keysym(&Key::Named(NamedKey::Shift)), Some(0xffe1));
    assert_eq!(vnc_keysym(&Key::Named(NamedKey::Alt)), Some(0xffe9));
    assert_eq!(vnc_keysym(&Key::Named(NamedKey::Home)), Some(0xff50));
    assert_eq!(vnc_keysym(&Key::Named(NamedKey::PageDown)), Some(0xff56));
    assert_eq!(vnc_keysym(&Key::Named(NamedKey::F5)), Some(0xffc2));
    assert_eq!(vnc_keysym(&Key::Character("é".into())), Some(0xe9));
    assert_eq!(
        vnc_keysym(&Key::Character("ł".into())),
        Some(0x0100_0142),
        "past Latin-1 a character is its Unicode keysym"
    );
    assert_eq!(
        ui::evdev::from_key_code(winit::keyboard::KeyCode::KeyA),
        Some(30)
    );
}

/// The wheel scrolls a VNC desktop: a wheel click up is a click of button 4,
/// down of button 5, sideways 6 and 7; a trackpad's pixels add up to whole
/// clicks, and a fling sends a bounded burst.
#[test]
fn the_wheel_scrolls_a_vnc_desktop() {
    let mut pending = (0.0, 0.0);
    let line = |x, y| MouseScrollDelta::LineDelta(x, y);
    let pixels = |x, y| MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(x, y));
    let notch = f64::from(ui::tokens::metrics::Metrics::BASE.wheel_line);
    let mut clicks =
        |delta: &MouseScrollDelta, scale: f64| vnc_wheel_clicks(&mut pending, delta, scale, notch);
    assert_eq!(clicks(&line(0.0, 1.0), 1.0), [0x08]);
    assert_eq!(clicks(&line(0.0, -2.0), 1.0), [0x10, 0x10]);
    assert_eq!(clicks(&line(1.0, 0.0), 1.0), [0x20]);
    assert_eq!(clicks(&line(-1.0, 0.0), 1.0), [0x40]);
    // At scale 2, a click is two notches of physical pixels.
    let most_of_a_click = -0.6 * notch * 2.0;
    assert!(
        clicks(&pixels(0.0, most_of_a_click), 2.0).is_empty(),
        "under a click at this scale"
    );
    assert_eq!(
        clicks(&pixels(0.0, most_of_a_click), 2.0),
        [0x10],
        "the two together make one"
    );
    assert_eq!(clicks(&line(0.0, 500.0), 1.0).len(), VNC_WHEEL_BURST);
}

#[test]
fn the_window_bar_names_the_screen_and_holds_its_actions() {
    use super::window_bar::{LEAVE_FORM, NEW_CONNECTION, SAVE};
    use ui::window_toolbar::{ToolbarEvent, deliver};

    let ids = |bar: &ui::window_toolbar::WindowToolbar| {
        bar.actions
            .iter()
            .map(|action| action.id.clone())
            .collect::<Vec<_>>()
    };
    let label = |bar: &ui::window_toolbar::WindowToolbar| {
        bar.location.as_ref().map(|location| location.label.clone())
    };
    let press = |app: &mut RemoteApp, id: &str| {
        deliver(ToolbarEvent::Action { id: id.into() });
        app.take_window_toolbar_events();
        app.apply_requests();
    };

    let mut app = demo();
    let bar = app.window_toolbar();
    assert_eq!(label(&bar).as_deref(), Some("Connections"));
    assert_eq!(ids(&bar), [NEW_CONNECTION]);

    press(&mut app, NEW_CONNECTION);
    assert_eq!(app.screen, Screen::Form { editing: None });
    let bar = app.window_toolbar();
    assert_eq!(label(&bar).as_deref(), Some("New connection"));
    assert_eq!(ids(&bar), [SAVE]);
    assert!(
        bar.leading.iter().any(|action| action.id == LEAVE_FORM),
        "the form is left from the bar"
    );

    press(&mut app, SAVE);
    assert!(
        app.problems.name.is_some(),
        "Save in the bar saves the form"
    );
    press(&mut app, LEAVE_FORM);
    assert_eq!(app.screen, Screen::List);

    let rdp = app.first_of_kind(ConnectionKind::Rdp).unwrap();
    let name = app.workspace.get(rdp).unwrap().name.clone();
    app.apply(Action::RequestConnect(rdp));
    assert_eq!(label(&app.window_toolbar()), Some(name));
}
