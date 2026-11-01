//! The collaboration manager driven by an app: what a window's first frame
//! publishes, its settings and records on the manager's routes. The manager
//! runs against the collab crate's fake broker
//! (`collaboration::test_support`).

use crate::collaboration::CollabRequest;
use crate::collaboration::test_support::{
    Captures, FakeBroker, document_reads_publishable, eventually, manager_on,
    manager_on_record_route, ordinary_catalogue_manager, stand_up_fake_broker, wake_channel,
};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct MonitorPrefs {
    confirm_kill: bool,
    interval_ms: u64,
}

impl crate::storage::SettingsSchema for MonitorPrefs {
    const SCOPE: crate::storage::Scope = crate::storage::Scope::Personal;
}

/// Whether the calling test `name` (in this module) runs its body here.
/// An app's settings and live-activity channels are process-wide and
/// flush through the transport that connected last, so another test's
/// manager connecting in the same process would take over the route.
fn in_own_process(name: &str) -> bool {
    crate::testing::in_own_process(&crate::testing::test_path(module_path!(), name))
}

// A setting written before the app's settings channel connects is never
// lost: it waits, and lands in the person's records in the order it was
// made once the channel connects.
#[cfg(unix)]
#[test]
fn a_write_before_the_channel_connects_lands_after_it_connects_in_order() {
    use settings_store::SettingsSource;
    const APP: &str = "so.tilo.monitor";
    if !in_own_process("a_write_before_the_channel_connects_lands_after_it_connects_in_order") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let records = settings_store::Store::open(dir.path()).unwrap();
    records
        .update_section(
            "app_personal",
            serde_json::from_value(serde_json::json!({APP: {"interval_ms": 1000}})).unwrap(),
        )
        .unwrap();
    // As at launch: the channel is the app's settings before anything
    // reaches the shell.
    crate::collaboration::channel_settings::install(APP);
    let prefs = crate::storage::Settings::<MonitorPrefs>::for_app(APP);
    prefs.update(|prefs| prefs.interval_ms = 1500).unwrap();
    prefs.update(|prefs| prefs.confirm_kill = true).unwrap();
    prefs.update(|prefs| prefs.interval_ms = 2000).unwrap();
    assert_eq!(
        prefs.get(),
        MonitorPrefs {
            confirm_kill: true,
            interval_ms: 2000
        },
        "the writes show at once"
    );
    assert_eq!(
        records.load()["app_personal"][APP]["interval_ms"],
        1000,
        "nothing reached the shell yet"
    );

    let captures = Captures {
        settings: Some((
            APP.to_owned(),
            Arc::clone(&records) as Arc<dyn SettingsSource>,
        )),
        ..Captures::default()
    };
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(captures)),
        ..FakeBroker::default()
    };
    let (fd, _, _thread) = stand_up_fake_broker(broker);
    let _manager = manager_on(fd, APP, Uuid::new_v4());
    eventually("the early writes reach the person's record", || {
        let stored = &records.load()["app_personal"][APP];
        stored["confirm_kill"] == true && stored["interval_ms"] == 2000
    });
    assert_eq!(
        prefs.get(),
        MonitorPrefs {
            confirm_kill: true,
            interval_ms: 2000
        },
        "the shell's view, once it answers, is what was written last"
    );
    crate::collaboration::channel_settings::uninstall();
}

#[cfg(unix)]
#[test]
fn an_apps_settings_reach_the_shells_records_and_follow_them() {
    use settings_store::SettingsSource;
    const APP: &str = "so.tilo.monitor";
    if !in_own_process("an_apps_settings_reach_the_shells_records_and_follow_them") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let records = settings_store::Store::open(dir.path()).unwrap();
    records
        .update_section(
            "app_personal",
            serde_json::from_value(serde_json::json!({APP: {"interval_ms": 1000}})).unwrap(),
        )
        .unwrap();
    let captures = Captures {
        settings: Some((
            APP.to_owned(),
            Arc::clone(&records) as Arc<dyn SettingsSource>,
        )),
        ..Captures::default()
    };
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(captures)),
        ..FakeBroker::default()
    };
    let (fd, _, _thread) = stand_up_fake_broker(broker);
    crate::collaboration::channel_settings::install(APP);
    let _manager = manager_on(fd, APP, Uuid::new_v4());
    let prefs = crate::storage::Settings::<MonitorPrefs>::for_app(APP);

    eventually("the shell's view reaches the app", || {
        prefs.get().interval_ms == 1000
    });

    // The app's write lands in the person's record.
    prefs.update(|prefs| prefs.confirm_kill = true).unwrap();
    eventually("the write reaches the person's record", || {
        records.load()["app_personal"][APP]["confirm_kill"] == true
    });
    assert_eq!(records.load()["app_personal"][APP]["interval_ms"], 1000);

    // Another device's write, as the device fabric applies it, reaches
    // the app's settings.
    let before = prefs.revision();
    records
        .update_section(
            "app_personal",
            serde_json::from_value(
                serde_json::json!({APP: {"confirm_kill": true, "interval_ms": 3000}}),
            )
            .unwrap(),
        )
        .unwrap();
    eventually("the record's change reaches the app", || {
        prefs.get().interval_ms == 3000
    });
    assert_ne!(prefs.revision(), before, "views following it refresh");

    // Another app's settings are out of reach: a write that changes
    // them is refused (one that changes nothing writes nothing).
    let error = crate::storage::Settings::<MonitorPrefs>::for_app("so.tilo.notes")
        .set(&MonitorPrefs {
            confirm_kill: true,
            interval_ms: 5000,
        })
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        records.load()["app_personal"]
            .get("so.tilo.notes")
            .is_none()
    );
    crate::collaboration::channel_settings::uninstall();
}

/// The first frame of an app on a live manager publishes what it offers,
/// and publishes it again when the broker refused it.
#[cfg(unix)]
#[test]
fn manager_publishes_the_first_frame_without_input() {
    // Case `first_frame_publishes_actions_without_input`.
    {
        let broker = FakeBroker::default();
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let app_id = crate::collaboration::local_app_id();
        let manager = manager_on(fd, &app_id, Uuid::new_v4());
        let mut app = crate::App::with_collaboration_manager(
            crate::State::new(),
            Box::new(crate::widget::Button::new("complete", "Complete").on_view(|| {})),
            manager,
        );
        assert!(captures.lock().unwrap().action_publications.is_empty());
        // This calls the production layout/draw passes. No input, palette,
        // explicit publication, or app-owned action invocation primes the app.
        crate::app::draw_prepared_component_test_frame(&mut app);
        tilo_test_env::wait_until(
            "the first rendered frame must publish its semantic actions to the broker without input",
            || {
                let seen = captures.lock().unwrap();
                seen.action_directories.iter().any(|id| id == &app_id)
                    && seen.action_publications.iter().any(|(id, actions)| {
                        id == &app_id
                            && actions
                                .iter()
                                .any(|(label, action)| label == "Complete" && !action.is_empty())
                    })
            },
        );
    }

    // Case `a_refused_action_publication_is_republished`.
    {
        let broker = FakeBroker::default();
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        captures.lock().unwrap().refuse_action_publications = 1;
        let app_id = crate::collaboration::local_app_id();
        let manager = manager_on(fd, &app_id, Uuid::new_v4());
        let mut app = crate::App::with_collaboration_manager(
            crate::State::new(),
            Box::new(crate::widget::Button::new("complete", "Complete").on_view(|| {})),
            manager,
        );
        crate::app::draw_prepared_component_test_frame(&mut app);
        tilo_test_env::wait_until("the broker must see the first surface", || {
            captures.lock().unwrap().refused_action_publications == 1
        });
        // Nothing about the surface changes: only the refusal can make the
        // app publish it again.
        tilo_test_env::wait_until(
            "a surface the broker refused must be republished, not counted as published",
            || {
                app.pump_collab_presence();
                let seen = captures.lock().unwrap();
                seen.action_publications.iter().any(|(id, actions)| {
                    id == &app_id
                        && actions
                            .iter()
                            .any(|(label, action)| label == "Complete" && !action.is_empty())
                })
            },
        );
    }
}

#[test]
fn native_admission_ack_wakes_parked_owner_without_an_action() {
    held_directory_observation(false);
}

#[test]
fn native_admission_failure_wakes_owner_without_claiming_retry() {
    held_directory_observation(true);
}

fn held_directory_observation(refused: bool) {
    let ack = Arc::new(tokio::sync::Notify::new());
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            directory_ack: Some(ack.clone()),
            refuse_action_directories: usize::from(refused),
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (scope, wakes) = wake_channel();
    let _entry = scope.enter();
    let (fd, captures, worker) = stand_up_fake_broker(broker);
    let manager = manager_on(fd, &crate::collaboration::local_app_id(), Uuid::new_v4());
    let mut app = crate::App::with_collaboration_manager(
        crate::State::new(),
        Box::new(crate::widget::Button::new("complete", "Complete").on_view(|| {})),
        manager,
    );
    crate::app::draw_prepared_component_test_frame(&mut app);
    eventually("directory RPC reaches actual host", || {
        !captures.lock().unwrap().directory_attempts.is_empty()
    });
    app.pump_collab_presence();
    let before = app.native_admission_snapshot();
    while wakes.try_recv().is_ok() {}
    ack.notify_one();
    let deadline = tilo_test_env::hang_deadline();
    let observed = loop {
        if wakes
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .is_err()
        {
            break false;
        }
        app.pump_collab_presence();
        let snapshot = app.native_admission_snapshot();
        if snapshot.directory.transport_directory_rpc_successes == Some(u64::from(!refused))
            && snapshot.directory.transport_directory_rpc_failures == Some(u64::from(refused))
        {
            break true;
        }
    };
    drop(app);
    worker.join().unwrap();
    assert_eq!(
        before.directory.transport_directory_rpc_successes,
        Some(0),
        "enqueue is not RPC success"
    );
    assert_eq!(before.directory.transport_directory_rpc_failures, Some(0));
    assert!(before.directory.directory_enqueued);
    assert!(before.connection.manager_connected);
    assert!(
        observed,
        "actual RPC result must wake the profiled owner before any app action"
    );
    assert_eq!(
        captures.lock().unwrap().directory_attempts.len(),
        1,
        "observation introduces no retries"
    );
}

/// Ordinary application startup must not acquire an unrelated workspace's
/// eager discovery inventory. Discovery remains on the published directory;
/// this socket fixture certifies transport readiness, not broker authority.
#[test]
#[cfg(unix)]
fn ordinary_application_starts_with_an_oversized_workspace_catalogue() {
    let cluster = Uuid::new_v4();
    let (manager, captures, worker, admission, healthy) = ordinary_catalogue_manager(cluster, 2049);
    let subscriptions = captures.lock().unwrap().action_catalogue_subscriptions;
    if admission.is_ok() && healthy {
        // What the app publishes, the host files under the route its
        // connection joined: the Browser's.
        let publication_app_id = "so.tilo.browser";
        let mut app = crate::App::with_collaboration_manager(
            crate::State::new(),
            Box::new(crate::widget::Button::new("complete", "Complete").on_view(|| {})),
            manager,
        );
        crate::app::draw_prepared_component_test_frame(&mut app);
        eventually(
            "ordinary first frame publishes its actions and pull directory",
            || {
                let seen = captures.lock().unwrap();
                seen.action_directories
                    .iter()
                    .any(|id| id == publication_app_id)
                    && seen.action_publications.iter().any(|(id, actions)| {
                        id == publication_app_id
                            && actions
                                .iter()
                                .any(|(label, action)| label == "Complete" && !action.is_empty())
                    })
            },
        );
        drop(app);
    } else {
        drop(manager);
    }
    worker.join().unwrap();
    assert!(
        admission.is_ok(),
        "ordinary route must become ready: {admission:?}"
    );
    assert!(
        healthy,
        "unconsumed metadata must not disconnect the application"
    );
    assert_eq!(
        subscriptions, 0,
        "ordinary startup does not subscribe to eager discovery"
    );
}

/// SDK transport/owner ordering only. This uncertified test endpoint cannot
/// construct broker NativeDocumentReadContext and is not native authority proof.
#[test]
#[cfg(unix)]
fn committed_initial_route_uses_one_worker_and_blocks_default_publication() {
    use automerge::transaction::Transactable;
    let cluster = Uuid::new_v4();
    let want = crate::record::RecordCheckout::new(
        crate::record::RecordId::minted(),
        crate::record::RecordKind::new("report"),
    );
    let mut source = crate::record::RecordSet::in_memory().with_home(cluster.to_string());
    let mut original = crate::state::State::new();
    assert!(source.checkout(&mut original, None, &want));
    original
        .doc
        .put(automerge::ROOT, "body", "committed real history")
        .unwrap();
    original.doc.commit();
    let heads = original.doc.get_heads();
    let captures = Captures {
        committed_history_fixture: Some(original.doc.save()),
        ..Captures::default()
    };
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(captures)),
        ..FakeBroker::default()
    };
    let (fd, captures, _thread) = stand_up_fake_broker(broker);
    let mut manager = manager_on(fd, "so.tilo.test", cluster);
    assert!(manager.has_native_committed_primary());
    assert!(!manager.send(CollabRequest::document_snapshot(
        b"default before hydration".to_vec()
    )));
    let mut initial = None;
    tilo_test_env::wait_until("the native record to hydrate", || {
        initial = manager.prepare_native_record(&want);
        initial.is_some()
    });
    let read = initial.unwrap();
    let mut set = crate::record::RecordSet::in_memory().with_home(cluster.to_string());
    let mut state = crate::state::State::new();
    assert!(
        read.with_mutation(|mutation| {
            set.checkout_with_initial(&mut state, None, &want, mutation)
        })
        .unwrap()
    );
    assert_eq!(state.doc.get_heads(), heads);
    read.applied();
    manager.switch_native_record(&want).unwrap();
    assert_eq!(
        captures.lock().unwrap().joined.len(),
        2,
        "two routes must share the one inherited transport"
    );
    assert_eq!(
        captures.lock().unwrap().joined_record_kinds,
        ["", "report"],
        "the record route names its resource kind for the owner; the launch route none"
    );
    assert!(
        captures.lock().unwrap().signed_ops.is_empty(),
        "no default was signed or sent"
    );
    tilo_test_env::wait_until("the primary route to become publishable", || {
        document_reads_publishable(&manager, manager.primary_route().unwrap().route())
    });
}

/// A native window that shows another record in its component's place
/// moves its live broker connection to that record's route: one inherited
/// transport, a second route joined with the record's kind, and the
/// window's primary route then the new record's.
#[test]
#[cfg(unix)]
fn a_native_window_showing_another_record_joins_its_route_on_the_live_broker() {
    use crate::application::{LaunchTarget, RestoreInput};
    use crate::application_test_support::{
        DEVICE, runner_inputs, runner_permissions, runner_probe_definition,
    };
    let cluster = Uuid::new_v4();
    let captures = Captures {
        committed_history_fixture: Some(automerge::AutoCommit::new().save()),
        ..Captures::default()
    };
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(captures)),
        ..FakeBroker::default()
    };
    let (fd, captures, _thread) = stand_up_fake_broker(broker);
    let definition = runner_probe_definition();
    let shown = crate::record::RecordId::minted();
    let manager = manager_on_record_route(
        fd,
        definition.app_id().to_owned(),
        cluster,
        Some(shown.as_str().to_owned()),
    );
    assert!(manager.has_native_committed_primary());

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let mut owners = crate::record::RecordOwners::new(root.clone());
    let records = owners
        .open(definition.app_id(), &cluster.to_string())
        .unwrap();
    let views = std::rc::Rc::new(
        crate::application_view::InstanceViewStore::new(root, DEVICE, &cluster.to_string())
            .unwrap(),
    );
    let context = crate::application::HostContext::prepare_new_record(
        definition,
        crate::application::HostContextInputs {
            records,
            views,
            device_id: DEVICE.into(),
            instance_id: 41,
            permissions: runner_permissions(),
            author: collab_types::presence::UserId("probe-author".into()),
            action_context: tilo_actions::ActionContext::default(),
        },
        shown,
    )
    .unwrap();
    let reopener = context.reopener();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let mut app =
        crate::App::from_component_with_collaboration(component, runner_inputs(), manager).unwrap();
    assert_eq!(captures.lock().unwrap().joined.len(), 1, "the launch route");

    let wanted = crate::record::RecordId::minted();
    let (next, target) = reopener.prepare(definition, wanted.clone()).unwrap();
    let replacement = definition
        .instantiate(next, target, RestoreInput::None)
        .unwrap();
    let outcome = reopener.replace(replacement, vec![]);
    assert!(app.swap_requested_component());
    assert_eq!(outcome.get(), Some(Ok(())));
    assert_eq!(
        captures.lock().unwrap().joined.len(),
        2,
        "the record's route shares the one inherited transport"
    );
    assert_eq!(
        captures
            .lock()
            .unwrap()
            .joined_record_kinds
            .last()
            .map(String::as_str),
        Some(definition.record_kind.as_str()),
        "the record route names its kind for the owner"
    );
    assert_eq!(
        app.collaboration_manager()
            .primary_route()
            .and_then(|witness| witness.route().record.clone())
            .as_deref(),
        Some(wanted.as_str()),
        "the window's primary route is the record it shows now"
    );
    app.close_components();
}
