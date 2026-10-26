//! What a host worker's pump reports when its collaboration is not connected.
//!
//! These drive a real [`HostWorker`] mounted on a real owner record; only the
//! injected transport is a fixture.
use super::*;
use crate::application::{
    AppComponent, ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget,
    PreparedComponentInputs, RestoreInput,
};
use crate::application_test_support::{
    APP, DEVICE, author, definition, provenance, runner_inputs, runner_permissions,
};
use crate::collaboration::{CollabEvent, CollabRequest, PeerTransport};
use crate::widget::Container;
use automerge::{ROOT, transaction::Transactable};
use std::rc::Rc;

/// A peer transport that only ever reports the state it was built with.
struct SilentPeer {
    connected: bool,
}

impl PeerTransport for SilentPeer {
    fn is_connected(&self) -> bool {
        self.connected
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Err("peer transport is a fixture".into())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        vec![]
    }
}

struct Root {
    inner: Container,
}

impl Widget for Root {
    crate::testing::delegate_widget_methods!(
        inner: sync,
        build_layout,
        apply_layout,
        draw,
        handle_event,
        get_rect
    );

    fn name(&self) -> &str {
        "host worker pump probe"
    }
}
crate::testing::test_app_widget!(Root, "Host worker pump probe");

fn factory(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|doc| {
                doc.put(ROOT, "body", "admitted real owner record")
                    .map_err(|error| error.to_string())
            })
            .map_err(crate::launch::LaunchError::Record)?;
    }
    Ok(AppComponent::new(
        context,
        Box::new(Root {
            inner: Container::new(),
        }),
        tilo_actions::ActionRegistry::new(),
    ))
}

/// Mount the ordinary probe app as a host worker whose record's collaboration
/// manager reports `collaboration`.
fn mount_worker(
    collaboration: CollaborationManager,
) -> (
    tempfile::TempDir,
    crate::record::RecordOwners,
    crate::host_worker::HostWorker,
) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    let mut owners = crate::record::RecordOwners::new(root.clone());
    let room = uuid::Uuid::new_v4().to_string();
    let records = owners.open(APP, &room).unwrap();
    let views =
        Rc::new(crate::application_view::InstanceViewStore::new(root, DEVICE, &room).unwrap());
    let permissions = runner_permissions();
    let definition = definition(factory);
    let prepare = |target| {
        HostContext::prepare(
            &definition,
            HostContextInputs {
                records: records.clone(),
                views: views.clone(),
                device_id: DEVICE.into(),
                instance_id: 21,
                permissions: permissions.clone(),
                author: author(),
                action_context: provenance(),
            },
            target,
        )
        .unwrap()
    };
    let mut seed = prepare(&LaunchTarget::New);
    seed.initialize_new_record(|doc| {
        doc.put(ROOT, "body", "admitted real owner record")
            .map_err(|error| error.to_string())
    })
    .unwrap();
    let target = LaunchTarget::Record {
        checkout: seed.record().clone(),
        reveal: None,
    };
    drop(seed);
    let inputs = PreparedComponentInputs {
        context: prepare(&target),
        target,
        restore: RestoreInput::None,
        runner_inputs: ComponentRunnerInputs { ..runner_inputs() },
    };
    let mut worker =
        crate::host_worker::HostWorker::mount(&definition, inputs, collaboration, permissions)
            .unwrap();
    // A refused authorizer is checked before the connection every pass, so the
    // fixture installs a permissive one: this test is about the transport.
    worker
        .install_published_invocation_authorizer(std::sync::Arc::new(|_| Err(String::new())))
        .unwrap();
    (dir, owners, worker)
}

#[test]
fn host_worker_pump_names_an_offline_transport_and_not_an_uncertain_outcome() {
    // No collaboration service: the record's manager reports why it is
    // offline. The pump never sent an invocation, so it must not claim an
    // unknown application outcome: that belongs to a sent invocation.
    let (_dir, _owners, mut worker) = mount_worker(CollaborationManager::disabled());
    assert!(!worker.app_mut().collaboration_is_connected());
    let error = worker
        .pump(runner_permissions().borrow().clone())
        .unwrap_err();
    assert!(
        !error.contains("outcome unknown"),
        "an unsent invocation has no unknown outcome: {error}"
    );
    assert!(
        error.contains("no collaboration service is available"),
        "the refusal names the actual transport state: {error}"
    );

    // A transport that is still settling reports that, not a lost connection.
    let (_dir, _owners, mut connecting) =
        mount_worker(CollaborationManager::with_peer_transport(SilentPeer {
            connected: false,
        }));
    assert!(!connecting.app_mut().collaboration_is_connected());
    let error = connecting
        .pump(runner_permissions().borrow().clone())
        .unwrap_err();
    assert!(!error.contains("outcome unknown"), "{error}");
    assert!(error.contains("still connecting"), "{error}");
}

#[test]
fn host_worker_pump_reports_a_lost_transport_and_never_an_unknown_outcome() {
    // The enqueue failure closes this connection; the pump's refusal then
    // carries that reason instead of calling the state an unknown outcome of
    // an invocation the worker never sent.
    let (_dir, _owners, mut worker) =
        mount_worker(CollaborationManager::with_peer_transport(SilentPeer {
            connected: true,
        }));
    assert!(worker.app_mut().collaboration_is_connected());
    assert!(
        worker
            .app_mut()
            .collaboration_manager_mut()
            .try_send(CollabRequest::Presence {
                key: crate::presence::PresenceKey::pointer_default(),
                payload: crate::collaboration::PresencePayload::Caret { pos: 0 },
            })
            .is_err()
    );
    assert!(!worker.app_mut().collaboration_is_connected());
    let error = worker
        .pump(runner_permissions().borrow().clone())
        .unwrap_err();
    assert!(!error.contains("outcome unknown"), "{error}");
    assert!(error.contains("peer transport is a fixture"), "{error}");
}
