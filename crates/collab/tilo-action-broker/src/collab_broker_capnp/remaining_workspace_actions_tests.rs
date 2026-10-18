use super::*;
use crate::collab_broker_capnp::{BrokerChannels, FanoutState, PeerPublisher};
use crate::services::{HostServices, WorkspaceGovernanceRequest, WorkspaceGovernanceServices};
use crate::test_support::WorkspaceId;
use std::sync::Mutex;
use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

#[derive(Default)]
struct Governance(Mutex<Vec<(tilo_actions::InvocationId, bool)>>);
impl WorkspaceGovernanceServices for Governance {
    fn app_id(&self) -> &str {
        "so.tilo.workspace"
    }
    fn handles(&self, action: &str) -> bool {
        action == "inspect"
    }
    fn descriptors(&self, _: &Workspace) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor {
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new("workspace", "inspect"),
            resource_kind: "workspace".into(),
            resource_label: "Workspace".into(),
            action_label: "Inspect".into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::View,
            contract: tilo_actions::ActionContract::view(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }]
    }
    fn invoke(&self, request: &WorkspaceGovernanceRequest<'_>) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push((request.invocation, request.local_device));
        Ok(())
    }
}

fn fixture(
    root: &std::path::Path,
) -> (
    Arc<CapnpBrokerState>,
    Arc<Governance>,
    Workspace,
    Caller,
    InvokeRequest,
) {
    let service = Arc::new(Governance::default());
    let (state, _) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        None,
        None,
        None,
        HostServices {
            workspace_governance: Some(service.clone()),
            ..Default::default()
        },
    );
    *state.journal.lock().unwrap() = Ok(Some(
        super::super::invocation_journal::InvocationJournal::open(&root.join("journal")).unwrap(),
    ));
    let mut workspace = Workspace::new(WorkspaceId(96), "Governance");
    workspace.set_owner("owner");
    let caller = Caller::local_person("owner").in_room(workspace.room.to_string());
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(&workspace.room.to_string()),
            address: tilo_actions::ActionAddress::new("workspace", "inspect"),
        },
        argument: InvokeArgument::None,
        context: Some(tilo_actions::ActionContext {
            purpose: Some("Inspect this workspace".into()),
            ..Default::default()
        }),
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        appearance: None,
        wait_ms: None,
    };
    (state, service, workspace, caller, request)
}

#[test]
fn registered_governance_never_runs_before_durable_admission() {
    use super::super::invocation_journal::BarrierFailure;
    for failure in [
        BarrierFailure::File,
        BarrierFailure::Directory,
        BarrierFailure::AfterAdmission,
    ] {
        let root = tempfile::tempdir().unwrap();
        let (state, service, workspace, caller, request) = fixture(root.path());
        state
            .journal
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .fail_at = Some(failure);
        let actor = BrokerActor::for_caller(&caller);
        assert!(
            state
                .start_remaining_workspace_action(
                    &caller,
                    None,
                    (&workspace, &actor, 0),
                    &request,
                    None
                )
                .expect("registered governance is handled")
                .is_err()
        );
        assert!(service.0.lock().unwrap().is_empty());
        assert!(state.completed_invocations.lock().unwrap().is_empty());
    }
}

#[test]
fn registered_governance_keeps_original_uuid_context_and_terminal_owner() {
    let root = tempfile::tempdir().unwrap();
    let (state, service, workspace, caller, request) = fixture(root.path());
    let actor = BrokerActor::for_caller(&caller);
    let id = state
        .start_remaining_workspace_action(&caller, None, (&workspace, &actor, 0), &request, None)
        .unwrap()
        .unwrap();
    assert_eq!(*service.0.lock().unwrap(), vec![(request.invocation, true)]);
    let owner = super::super::InvocationOwner::Local {
        actor: "owner".into(),
    };
    let guard = state.journal.lock().unwrap();
    let journal = guard.as_ref().unwrap().as_ref().unwrap();
    let record = journal
        .lookup(&super::super::InvocationKey {
            room: workspace.room.to_string(),
            host: journal.host(),
            owner: owner.clone(),
            invocation: request.invocation,
        })
        .unwrap()
        .unwrap();
    assert_eq!(record.wire_id, id);
    assert_eq!(
        record.intent["context"],
        serde_json::to_value(&request.context).unwrap()
    );
    assert!(
        matches!(record.update, InvocationUpdate::Succeeded { invocation, .. } if invocation == request.invocation)
    );
    assert_eq!(
        state
            .completed_invocations
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .0,
        owner
    );
}

#[test]
fn registered_governance_does_not_upgrade_a_transport_label_into_custody() {
    let root = tempfile::tempdir().unwrap();
    let (state, service, workspace, _, request) = fixture(root.path());
    let caller = Caller::authenticated([7; 32], [8; 32]).in_room(workspace.room.to_string());
    let actor = BrokerActor::for_caller(&caller);
    // Even a permission-bearing actor is not the caller's original certificate.
    let mut workspace = workspace;
    workspace.set_owner(actor.id());
    assert!(
        state
            .start_remaining_workspace_action(
                &caller,
                None,
                (&workspace, &actor, 0),
                &request,
                None
            )
            .unwrap()
            .is_err()
    );
    assert!(service.0.lock().unwrap().is_empty());
    assert!(state.completed_invocations.lock().unwrap().is_empty());
}

#[derive(Default)]
struct Keyboard(std::sync::atomic::AtomicUsize);
impl crate::services::KeyboardShortcutServices for Keyboard {
    fn app_id(&self) -> &str {
        "so.tilo.settings"
    }
    fn is_resource(&self, resource: &str) -> bool {
        resource == "keyboard-shortcuts"
    }
    fn catalogue_rows(
        &self,
        _: &str,
        _: &settings_store::hardware::ShortcutConfig,
    ) -> Vec<tilo_actions::remote::CatalogueRow> {
        Vec::new()
    }
    fn descriptor(
        &self,
        _: &settings_store::hardware::ShortcutConfig,
        address: &tilo_actions::ActionAddress,
    ) -> Option<ActionDescriptor> {
        let mut descriptor = Governance::default()
            .descriptors(&Workspace::new(WorkspaceId(97), "Keyboard"))
            .remove(0);
        descriptor.address = address.clone();
        Some(descriptor)
    }
    fn apply(
        &self,
        _: &mut settings_store::hardware::ShortcutConfig,
        _: &tilo_actions::ActionAddress,
        _: Option<&str>,
    ) -> Result<(tilo_actions::ActionOutput, bool), Box<ActionFailure>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok((Default::default(), false))
    }
}

#[derive(Default)]
struct Library(Mutex<Vec<(tilo_actions::InvocationId, tilo_actions::ActionContext)>>);
impl crate::services::ShortcutLibraryServices for Library {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        Default::default()
    }
    fn store(&self, request: &crate::services::ShortcutLibraryRequest<'_>) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push((request.invocation, request.context.clone()));
        Ok(())
    }
    fn runtime(&self) -> Result<tokio::runtime::Handle, String> {
        Err("no run requested".into())
    }
}

#[test]
fn keyboard_and_library_effects_require_admission_and_keep_request_identity() {
    use super::super::invocation_journal::{BarrierFailure, InvocationJournal};
    for failure in [
        Some(BarrierFailure::File),
        Some(BarrierFailure::Directory),
        Some(BarrierFailure::AfterAdmission),
        None,
    ] {
        for keyboard in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (_, _, mut workspace, caller, mut request) = fixture(root.path());
            let keys = Arc::new(Keyboard::default());
            let library = Arc::new(Library::default());
            let (state, _) = CapnpBrokerState::new_configured(
                &Arc::new(FanoutState::new()),
                Arc::new(Mutex::new(BrokerChannels::default())),
                PeerPublisher::disabled(),
                None,
                None,
                None,
                HostServices {
                    keyboard_shortcuts: Some(keys.clone()),
                    shortcut_library: Some(library.clone()),
                    ..Default::default()
                },
            );
            let mut journal = InvocationJournal::open(&root.path().join("effects")).unwrap();
            journal.fail_at = failure;
            *state.journal.lock().unwrap() = Ok(Some(journal));
            workspace.shortcuts.push(
                serde_json::from_value(serde_json::json!({"id":"saved","name":"Saved","steps":[]}))
                    .unwrap(),
            );
            let (resource, action, argument) = if keyboard {
                ("keyboard-shortcuts", "rebind", None)
            } else {
                (
                    crate::automation::LIBRARY_RESOURCE,
                    "delete",
                    Some(r#"{"id":"saved"}"#),
                )
            };
            request.target.address = tilo_actions::ActionAddress::new(resource, action);
            let actor = BrokerActor::for_caller(&caller);
            let result = state
                .start_remaining_workspace_action(
                    &caller,
                    None,
                    (&workspace, &actor, 0),
                    &request,
                    argument,
                )
                .unwrap();
            assert_eq!(result.is_ok(), failure.is_none());
            assert_eq!(
                keys.0.load(std::sync::atomic::Ordering::Relaxed),
                usize::from(keyboard && failure.is_none())
            );
            let stored = library.0.lock().unwrap();
            if !keyboard && failure.is_none() {
                assert_eq!(stored.len(), 1);
                assert_eq!(stored[0].0, request.invocation);
                assert_eq!(stored[0].1, request.context.clone().unwrap());
            } else {
                assert!(stored.is_empty());
            }
        }
    }
}
