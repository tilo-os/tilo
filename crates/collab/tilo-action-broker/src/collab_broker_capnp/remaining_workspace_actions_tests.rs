use super::*;
use crate::collab_broker_capnp::{BrokerChannels, FanoutState, PeerPublisher};
use crate::services::{HostServices, WorkspaceGovernanceRequest, WorkspaceGovernanceServices};
use crate::test_support::WorkspaceId;
use std::sync::Mutex;
use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

thread_local! {
    static EFFECT_BARRIER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::default();
}

pub(super) fn at_effect_barrier() {
    let hook = EFFECT_BARRIER.with(|hook| hook.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the test keeps its fixture and guards alive through its assertions"
)]
#[test]
fn registered_services_recheck_original_certificate_after_signed_admission() {
    use super::super::host_steps::signed_test_support::Fixture;
    for kind in 0..3 {
        let governance = Arc::new(Governance::default());
        let keyboard = Arc::new(Keyboard::default());
        let library = Arc::new(Library::default());
        let services = HostServices {
            workspace_governance: Some(governance.clone()),
            keyboard_shortcuts: Some(keyboard.clone()),
            shortcut_library: Some(library.clone()),
            ..Default::default()
        };
        let f = if kind == 1 {
            Fixture::new(services)
        } else {
            Fixture::new_certified(services)
        };
        let directory = tempfile::tempdir().unwrap();
        let (_, _, _, _, mut request) = fixture(directory.path());
        request.target.provider = tilo_actions::remote::workspace_provider(&f.room);
        request.target.address = match kind {
            0 => tilo_actions::ActionAddress::new("workspace", "inspect"),
            1 => tilo_actions::ActionAddress::new("keyboard-shortcuts", "rebind"),
            _ => tilo_actions::ActionAddress::new(crate::automation::LIBRARY_RESOURCE, "list"),
        };
        let admitted = f.admission_check(&request);
        let authority = f.authority.clone();
        EFFECT_BARRIER.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                admitted();
                authority
                    .revoked
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }));
        });
        let (workspace, actor, now) = f
            .state
            .decide_for(&f.caller, &f.room, |workspace, actor, now| {
                (workspace.clone(), actor.clone(), now)
            })
            .unwrap();
        assert!(
            f.state
                .start_remaining_workspace_action(
                    &f.caller,
                    Some(&f.origin),
                    (&workspace, &actor, now),
                    &request,
                    None
                )
                .unwrap()
                .is_err()
        );
        assert!(governance.0.lock().unwrap().is_empty());
        assert_eq!(keyboard.0.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(library.0.lock().unwrap().is_empty());
        let record = f.assert_signed(&request, true);
        assert!(!record.uncertain);
        assert!(
            matches!(record.update, InvocationUpdate::Failed { ref error, .. } if error.code == "host_action_refused")
        );
        assert!(EFFECT_BARRIER.with(|hook| hook.borrow().is_none()));
    }
}

#[test]
fn library_replans_after_custody_instead_of_applying_a_stale_delete() {
    let directory = tempfile::tempdir().unwrap();
    let (_, _, mut workspace, caller, mut request) = fixture(directory.path());
    workspace.shortcuts.push(
        serde_json::from_value(serde_json::json!({"id":"saved","name":"Saved","steps":[]}))
            .unwrap(),
    );
    let mut tree = crate::test_support::WorkspaceTree::default();
    tree.workspaces.push(workspace.clone());
    let tree = tree.shared();
    let library = Arc::new(Library::default());
    let (state, _) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.authority()),
        None,
        None,
        HostServices {
            shortcut_library: Some(library.clone()),
            ..Default::default()
        },
    );
    *state.journal.lock().unwrap() = Ok(Some(
        super::super::invocation_journal::InvocationJournal::open(
            &directory.path().join("library"),
        )
        .unwrap(),
    ));
    request.target.address =
        tilo_actions::ActionAddress::new(crate::automation::LIBRARY_RESOURCE, "delete");
    request.confirmation = Some(Confirmation::Confirmed);
    request.argument =
        InvokeArgument::Json(serde_json::from_value(serde_json::json!({"id": "saved"})).unwrap());
    let argument = request.argument.to_slot();
    let current = tree.clone();
    EFFECT_BARRIER.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            current.write().workspaces[0].shortcuts.clear();
        }));
    });
    let actor = BrokerActor::for_caller(&caller);
    assert!(
        state
            .start_remaining_workspace_action(
                &caller,
                None,
                (&workspace, &actor, 0),
                &request,
                argument.as_deref()
            )
            .unwrap()
            .is_err()
    );
    assert!(library.0.lock().unwrap().is_empty());
    assert!(tree.read().workspaces[0].shortcuts.is_empty());
    assert!(EFFECT_BARRIER.with(|hook| hook.borrow().is_none()));
}

#[test]
fn keyboard_rechecks_live_policy_and_execution_after_custody() {
    for change in ["policy", "execution", "workspace"] {
        let directory = tempfile::tempdir().unwrap();
        let (_, _, workspace, caller, mut request) = fixture(directory.path());
        let caller = caller.in_new_execution();
        let mut tree = crate::test_support::WorkspaceTree::default();
        tree.workspaces.push(workspace.clone());
        let tree = tree.shared();
        let keyboard = Arc::new(Keyboard::default());
        let (state, _) = CapnpBrokerState::new_configured(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.authority()),
            None,
            None,
            HostServices {
                keyboard_shortcuts: Some(keyboard.clone()),
                ..Default::default()
            },
        );
        *state.journal.lock().unwrap() = Ok(Some(
            super::super::invocation_journal::InvocationJournal::open(
                &directory.path().join("keyboard"),
            )
            .unwrap(),
        ));
        request.target.address = tilo_actions::ActionAddress::new("keyboard-shortcuts", "rebind");
        let current = tree.clone();
        let execution = caller.clone();
        let room = workspace.room.to_string();
        EFFECT_BARRIER.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || match change {
                "execution" => execution.retire_execution(),
                "workspace" => current.write().workspaces.clear(),
                _ => {
                    let mut tree = current.write();
                    tree.workspaces[0].set_owner("other");
                    tree.workspaces[0]
                        .policy
                        .grants
                        .push(tilo_action_policy::Grant::new(
                            "keyboard-denied",
                            tilo_action_policy::Subject::person("owner"),
                            tilo_action_policy::TreePath::room(room).app("so.tilo.settings"),
                            tilo_action_policy::KindSet::READ,
                            tilo_action_policy::Answer::Deny,
                            tilo_action_policy::Subject::person("owner"),
                        ));
                }
            }));
        });
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
                .unwrap()
                .is_err()
        );
        assert_eq!(keyboard.0.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}

struct LostGovernance;
impl WorkspaceGovernanceServices for LostGovernance {
    fn app_id(&self) -> &'static str {
        "so.tilo.workspace"
    }
    fn handles(&self, action: &str) -> bool {
        action == "inspect"
    }
    fn descriptors(&self, workspace: &Workspace) -> Vec<ActionDescriptor> {
        Governance::default().descriptors(workspace)
    }
    fn start(
        &self,
        _: &WorkspaceGovernanceRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::services::WorkspaceGovernanceOutcome>, String>
    {
        let (answer, pending) = tilo_actions::remote::pending();
        drop(answer);
        Ok(pending)
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the test keeps its fixture and guards alive through its assertions"
)]
#[test]
fn lost_governance_answer_has_signed_uncertain_terminal() {
    use super::super::host_steps::signed_test_support::Fixture;
    let f = Fixture::new_certified(HostServices {
        workspace_governance: Some(Arc::new(LostGovernance)),
        ..Default::default()
    });
    let directory = tempfile::tempdir().unwrap();
    let (_, _, _, _, mut request) = fixture(directory.path());
    request.target.provider = tilo_actions::remote::workspace_provider(&f.room);
    let (workspace, actor, now) = f
        .state
        .decide_for(&f.caller, &f.room, |workspace, actor, now| {
            (workspace.clone(), actor.clone(), now)
        })
        .unwrap();
    // Exercise the registered service, not a published app's action directory.
    f.state
        .start_remaining_workspace_action(
            &f.caller,
            Some(&f.origin),
            (&workspace, &actor, now),
            &request,
            None,
        )
        .expect("registered governance is handled")
        .unwrap();
    let record = f.assert_signed(&request, true);
    assert!(record.uncertain);
    assert!(
        matches!(record.update, InvocationUpdate::Failed { ref error, .. } if error.code == "host_step_stopped")
    );
    f.reopen_journal();
    let record = f.assert_signed(&request, true);
    assert!(record.uncertain);
}

#[derive(Default)]
struct Governance(Mutex<Vec<(tilo_actions::InvocationId, bool)>>);
impl WorkspaceGovernanceServices for Governance {
    fn app_id(&self) -> &'static str {
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
    let mut workspace = Workspace::new(WorkspaceId(96), "Governance");
    workspace.set_owner("owner");
    let mut tree = crate::test_support::WorkspaceTree::default();
    tree.workspaces.push(workspace.clone());
    let (state, _) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.shared().authority()),
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

#[allow(
    clippy::significant_drop_tightening,
    reason = "the test keeps its fixture and guards alive through its assertions"
)]
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
    fn app_id(&self) -> &'static str {
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
        Ok((tilo_actions::ActionOutput::default(), false))
    }
}

#[derive(Default)]
struct Library(Mutex<Vec<(tilo_actions::InvocationId, tilo_actions::ActionContext)>>);
impl crate::services::ShortcutLibraryServices for Library {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        settings_store::hardware::ShortcutConfig::default()
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

#[allow(
    clippy::significant_drop_tightening,
    reason = "the test keeps its fixture and guards alive through its assertions"
)]
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
            workspace.shortcuts.push(
                serde_json::from_value(serde_json::json!({"id":"saved","name":"Saved","steps":[]}))
                    .unwrap(),
            );
            let mut tree = crate::test_support::WorkspaceTree::default();
            tree.workspaces.push(workspace.clone());
            let (state, _) = CapnpBrokerState::new_configured(
                &Arc::new(FanoutState::new()),
                Arc::new(Mutex::new(BrokerChannels::default())),
                PeerPublisher::disabled(),
                Some(tree.shared().authority()),
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
            let (resource, action) = if keyboard {
                ("keyboard-shortcuts", "rebind")
            } else {
                request.argument = InvokeArgument::Json(
                    serde_json::from_value(serde_json::json!({"id": "saved"})).unwrap(),
                );
                (crate::automation::LIBRARY_RESOURCE, "delete")
            };
            let argument = request.argument.to_slot();
            request.target.address = tilo_actions::ActionAddress::new(resource, action);
            if !keyboard {
                // Deleting a saved shortcut retains its destructive-action floor.
                request.confirmation = Some(Confirmation::Confirmed);
            }
            let actor = BrokerActor::for_caller(&caller);
            let result = state
                .start_remaining_workspace_action(
                    &caller,
                    None,
                    (&workspace, &actor, 0),
                    &request,
                    argument.as_deref(),
                )
                .unwrap();
            assert_eq!(
                result.is_ok(),
                failure.is_none(),
                "keyboard={keyboard}: {result:?}"
            );
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
