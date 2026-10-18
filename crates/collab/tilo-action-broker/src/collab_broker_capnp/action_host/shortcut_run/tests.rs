use super::*;
use crate::collab_broker_capnp::InvocationOwner;
use crate::collab_broker_capnp::invocation_journal::{InvocationJournal, InvocationKey};
use crate::collab_broker_capnp::{BrokerChannels, FanoutState, PeerPublisher};
use crate::services::{
    HostServices, LocalPersonServices, ShortcutLibraryRequest, ShortcutLibraryServices,
};
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use std::sync::Mutex;

struct Library(tokio::runtime::Handle);

impl ShortcutLibraryServices for Library {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        Default::default()
    }
    fn store(&self, _: &ShortcutLibraryRequest<'_>) -> Result<(), String> {
        Err("not a library editing test".into())
    }
    fn runtime(&self) -> Result<tokio::runtime::Handle, String> {
        Ok(self.0.clone())
    }
}

struct Person {
    certificate: tilo_user_identity::DeviceCert,
    time: crate::peer_actions::PolicyTime,
}

impl LocalPersonServices for Person {
    fn certificate(&self, _: &str, person: &str) -> Result<tilo_user_identity::DeviceCert, String> {
        if person
            != crate::collab_broker_capnp::principal_id_for_root(self.certificate.user_public_key)
        {
            return Err("different person".into());
        }
        Ok(self.certificate.clone())
    }
}

impl crate::peer_actions::CertificateAuthority for Person {
    fn policy_time(&self) -> Result<Option<crate::peer_actions::PolicyTime>, String> {
        Ok(Some(self.time.clone()))
    }
    fn authorize(
        &self,
        _: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if certificate == &self.certificate {
            Ok(())
        } else {
            Err("certificate replaced".into())
        }
    }
}

struct Fixture {
    state: Arc<CapnpBrokerState>,
    caller: Caller,
    origin: InvocationCaller,
    request: InvokeRequest,
    key: InvocationKey,
    _clock_origin: crate::peer_actions::PolicyOrigin,
    _root: tempfile::TempDir,
    _environment: tilo_test_env::ScopedEnv,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment
            .set("XDG_CONFIG_HOME", root.path().join("config"))
            .set("TILO_IDENTITY_ROOT", root.path().join("identity"))
            .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &root.path().join("config/tilo/identity"),
            &tilo_user_identity::IdentityPaths::under(root.path().join("identity")),
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let owner =
            crate::test_owner::SessionOwner::initialize(&root.path().join("owner")).unwrap();
        let certificate = owner.peer().authority().clone();
        let (clock, _control) = crate::peer_actions::OwnerPolicyClock::for_test(
            certificate.issued_at_ns + 1_000_000_000,
        );
        let clock_origin = clock.new_origin();
        let time = clock_origin.capture().unwrap();
        let person = Arc::new(Person {
            certificate: certificate.clone(),
            time: time.clone(),
        });
        let principal =
            crate::collab_broker_capnp::principal_id_for_root(certificate.user_public_key);
        let mut workspace = Workspace::new(WorkspaceId(9), "Shortcuts");
        workspace.set_owner(principal.clone());
        let room = workspace.room.to_string();
        workspace.shortcuts.push(
            serde_json::from_value(serde_json::json!({
                "id": "echo", "name": "Echo", "steps": [], "output": {"literal": "done"},
                "saved_by": principal,
                "triggers": [
                    {"trigger": "keyboard", "chord": "Super+Shift+E"},
                    {"trigger": "app_opened", "app_id": "notes"}
                ]
            }))
            .unwrap(),
        );
        for (id, target) in [("middle", "echo"), ("outer", "middle"), ("denied", "echo")] {
            let mut shortcut = workspace.shortcuts[0].clone();
            shortcut.id = id.into();
            shortcut.name = id.into();
            shortcut.steps = vec![tilo_automation::Step::new(
                "child",
                tilo_automation::StepKind::Invoke {
                    action: tilo_actions::ActionKey::new(
                        SHORTCUTS_APP_ID,
                        tilo_automation::SHORTCUT_RESOURCE_KIND,
                        "run",
                    ),
                    resource: tilo_automation::Selector::Fixed(
                        tilo_automation::shortcut_resource_id(target),
                    ),
                    args: Default::default(),
                    appearance: None,
                },
            )];
            if id != "denied" {
                shortcut.grant = vec![tilo_automation::action_rule(
                    SHORTCUTS_APP_ID,
                    tilo_automation::SHORTCUT_RESOURCE_KIND,
                    None,
                    "run",
                    tilo_action_policy::KindSet::ALL,
                    tilo_action_policy::Answer::Allow,
                )];
            }
            workspace.shortcuts.push(shortcut);
        }
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge) = CapnpBrokerState::new_configured(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared().authority()),
            None,
            None,
            HostServices {
                shortcut_library: Some(Arc::new(Library(tokio::runtime::Handle::current()))),
                local_person: Some(person.clone()),
                native_certificates: Some(person),
                ..Default::default()
            },
        );
        let journal = InvocationJournal::open(&root.path().join("journal")).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        let key = InvocationKey {
            room: room.clone(),
            host: journal.host(),
            invocation,
            owner: InvocationOwner::AuthenticatedLocal {
                root: certificate.user_public_key,
                device: certificate.device_public_key,
                agent: None,
            },
        };
        *state.journal.lock().unwrap() = Ok(Some(journal));
        let caller = Caller::local_person(principal).in_room(&room);
        let origin = InvocationCaller::NativePerson {
            invocation,
            room: room.clone(),
            certificate,
            time,
        };
        let request = InvokeRequest {
            invocation,
            target: tilo_actions::remote::RemoteActionTarget {
                provider: workspace_provider(&room),
                address: ActionAddress::new("shortcuts.shortcut:echo", "run"),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            on_behalf_of: None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        Self {
            state,
            caller,
            origin,
            request,
            key,
            _clock_origin: clock_origin,
            _root: root,
            _environment: environment,
        }
    }

    async fn invoke(&self) -> Result<InvocationReceipt, ActionHostError> {
        self.state
            .action_invoke_with_origin(&self.caller, None, None, Some(&self.origin), &self.request)
            .await
            .unwrap()
    }

    async fn finish(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !self
                .state
                .action_observe(&self.caller, &self.request.invocation)
                .is_some_and(|state| state.update.is_terminal())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the shortcut completes");
    }
}

fn start_direct(
    fixture: &Fixture,
    trigger: Option<&tilo_automation::TriggerSpec>,
) -> Result<u64, ActionHostError> {
    let room = fixture.caller.room().unwrap();
    if let Some(trigger) = trigger {
        fixture.state.start_triggered_shortcut_run(
            &fixture.caller,
            room,
            "echo",
            trigger,
            tilo_automation::RunId(fixture.request.invocation.0),
            serde_json::Map::new(),
        )
    } else {
        fixture.state.start_interactive_shortcut_run(
            &fixture.caller,
            room,
            "echo",
            serde_json::Map::new(),
        )
    }
}

#[tokio::test(flavor = "current_thread")]
async fn direct_palette_keyboard_and_unattended_runs_cross_signed_barriers() {
    for trigger in [
        None,
        Some(tilo_automation::TriggerSpec::Keyboard {
            chord: "Super+Shift+E".into(),
        }),
        Some(tilo_automation::TriggerSpec::AppOpened {
            app_id: "notes".into(),
        }),
    ] {
        let fixture = Fixture::new();
        let job = start_direct(&fixture, trigger.as_ref()).unwrap();
        // The run has not been polled: evidence must already be durable.
        let mut key = fixture.key.clone();
        let completed = fixture.state.completed_invocations.lock().unwrap();
        let (_, update) = completed.get(&job).unwrap();
        let tilo_actions::InvocationUpdate::Accepted { invocation } = update else {
            panic!("run must be admitted before executing");
        };
        key.invocation = *invocation;
        drop(completed);
        let record = fixture
            .state
            .journal
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .lookup(&key)
            .unwrap()
            .unwrap();
        let expected_trigger = trigger.as_ref().map_or_else(
            || serde_json::json!({"trigger": "manual"}),
            |trigger| serde_json::to_value(trigger).unwrap(),
        );
        assert_eq!(record.intent["host_start"]["trigger"], expected_trigger);
        assert_eq!(
            record.intent["host_start"]["mode"],
            if matches!(
                trigger,
                Some(tilo_automation::TriggerSpec::AppOpened { .. })
            ) {
                "unattended"
            } else {
                "interactive"
            }
        );
        assert_eq!(record.intent["host_start"]["inputs"], serde_json::json!({}));
        let evidence = record.admission.as_ref().expect("direct run is signed");
        crate::action_evidence::verify(evidence.public_receipt(), &evidence.public_receipt().facts)
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !fixture.state.has_finished_shortcut_runs() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let finished = fixture.state.take_finished_shortcut_runs();
        assert_eq!(finished.len(), 1);
        let expected = trigger
            .as_ref()
            .map_or("manual", tilo_automation::TriggerSpec::kind);
        assert_eq!(finished[0].report.trigger, expected);
        assert_eq!(
            finished[0].report.mode,
            if matches!(
                trigger,
                Some(tilo_automation::TriggerSpec::AppOpened { .. })
            ) {
                tilo_automation::RunMode::Unattended
            } else {
                tilo_automation::RunMode::Interactive
            }
        );
        let record = fixture
            .state
            .journal
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .lookup(&key)
            .unwrap()
            .unwrap();
        assert!(record.terminal.is_some());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn direct_entry_points_refuse_missing_journal_before_spawning() {
    for trigger in [
        None,
        Some(tilo_automation::TriggerSpec::Keyboard {
            chord: "Super+Shift+E".into(),
        }),
        Some(tilo_automation::TriggerSpec::AppOpened {
            app_id: "notes".into(),
        }),
    ] {
        let fixture = Fixture::new();
        *fixture.state.journal.lock().unwrap() = Ok(None);
        assert!(start_direct(&fixture, trigger.as_ref()).is_err());
        tokio::task::yield_now().await;
        assert!(!fixture.state.has_finished_shortcut_runs());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn direct_palette_refuses_native_custody_mismatch_without_fallback() {
    let mut fixture = Fixture::new();
    let room = fixture.caller.room().unwrap().to_string();
    fixture.caller = Caller::local_person("another-person").in_room(room);
    assert!(start_direct(&fixture, None).is_err());
    tokio::task::yield_now().await;
    assert!(!fixture.state.has_finished_shortcut_runs());
    assert!(
        fixture
            .state
            .completed_invocations
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn signed_run_keeps_original_uuid_owner_and_terminal_and_retries_only_once() {
    let fixture = Fixture::new();
    fixture.invoke().await.unwrap();
    // This assertion runs before yielding to the shortcut task: admission is
    // durable before the runtime has an opportunity to execute the run.
    let record = fixture
        .state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .lookup(&fixture.key)
        .unwrap()
        .unwrap();
    let evidence = record
        .admission
        .as_ref()
        .expect("host signed the run admission");
    crate::action_evidence::verify(evidence.public_receipt(), &evidence.public_receipt().facts)
        .unwrap();
    assert_eq!(record.key.invocation, fixture.request.invocation);
    assert!(
        fixture.state.pending_invocations.lock().unwrap().is_empty(),
        "a host run needs no fake application peer"
    );
    fixture.finish().await;
    fixture.invoke().await.unwrap();
    assert_eq!(fixture.state.take_finished_shortcut_runs().len(), 1);
    let record = fixture
        .state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .lookup(&fixture.key)
        .unwrap()
        .unwrap();
    assert!(
        record.terminal.is_some(),
        "terminal receipt crossed the journal barrier"
    );
    assert!(
        matches!(record.update, tilo_actions::InvocationUpdate::Succeeded { invocation, .. } if invocation == fixture.request.invocation)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn journal_failure_starts_no_run_and_can_retry_after_repair() {
    let fixture = Fixture::new();
    let journal = std::mem::replace(
        &mut *fixture.state.journal.lock().unwrap(),
        Err("journal write failed".into()),
    );
    assert!(fixture.invoke().await.is_err());
    tokio::task::yield_now().await;
    assert!(!fixture.state.has_finished_shortcut_runs());
    assert!(
        fixture
            .state
            .remote_job(&fixture.caller, &fixture.request.invocation)
            .is_none()
    );
    *fixture.state.journal.lock().unwrap() = journal;
    fixture.invoke().await.unwrap();
    fixture.finish().await;
    assert_eq!(fixture.state.take_finished_shortcut_runs().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn certified_run_cannot_downgrade_to_an_unjournaled_job() {
    let fixture = Fixture::new();
    *fixture.state.journal.lock().unwrap() = Ok(None);
    assert!(fixture.invoke().await.is_err());
    tokio::task::yield_now().await;
    assert!(!fixture.state.has_finished_shortcut_runs());
    assert!(
        fixture
            .state
            .remote_job(&fixture.caller, &fixture.request.invocation)
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_barrier_failure_never_publishes_success() {
    let fixture = Fixture::new();
    fixture.invoke().await.unwrap();
    *fixture.state.journal.lock().unwrap() = Err("terminal append failed".into());
    // The run contains no steps and completes on its first poll. A marker
    // queued after it proves that poll occurred without a wall-clock delay.
    let (sent, received) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = sent.send(());
    });
    received.await.unwrap();
    let observed = fixture
        .state
        .action_observe(&fixture.caller, &fixture.request.invocation)
        .unwrap();
    assert!(matches!(
        observed.update,
        tilo_actions::InvocationUpdate::Accepted { .. }
    ));
    assert!(!fixture.state.has_finished_shortcut_runs());
}

#[tokio::test(flavor = "current_thread")]
async fn nested_steps_keep_original_owner_and_sign_each_steps_own_uuid() {
    let mut fixture = Fixture::new();
    fixture.request.target.address.resource = tilo_automation::shortcut_resource_id("outer");
    fixture.invoke().await.unwrap();
    fixture.finish().await;
    let finished = fixture.state.take_finished_shortcut_runs();
    assert_eq!(finished.len(), 3, "outer, middle and leaf each executed");
    let mut invocations = std::collections::HashSet::new();
    for run in &finished {
        assert_eq!(run.report.outcome, tilo_automation::RunOutcome::Completed);
        let mut key = fixture.key.clone();
        key.invocation = tilo_actions::InvocationId(run.report.run.0);
        assert!(invocations.insert(key.invocation));
        let record = fixture
            .state
            .journal
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .lookup(&key)
            .unwrap()
            .unwrap();
        assert_eq!(record.key.owner, fixture.key.owner);
        let evidence = record
            .admission
            .as_ref()
            .expect("each step has signed custody");
        // Guarded history retains a request digest, not the private request.
        // The signed receipt must still identify this step's own UUID.
        assert_eq!(evidence.public_receipt().facts.invocation, key.invocation);
        crate::action_evidence::verify(evidence.public_receipt(), &evidence.public_receipt().facts)
            .unwrap();
        assert!(record.terminal.is_some());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn propagated_custody_does_not_bypass_the_shortcut_grant() {
    let mut fixture = Fixture::new();
    fixture.request.target.address.resource = tilo_automation::shortcut_resource_id("denied");
    fixture.invoke().await.unwrap();
    fixture.finish().await;
    let finished = fixture.state.take_finished_shortcut_runs();
    assert_eq!(finished.len(), 1, "the ungranted child never starts");
    assert!(matches!(
        finished[0].report.outcome,
        tilo_automation::RunOutcome::Failed { .. }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn an_unscoped_broker_door_does_not_invent_native_custody() {
    let fixture = Fixture::new();
    let door = BrokerDoor::new(fixture.state.clone());
    assert!(
        tilo_actions::remote::ActionHost::invoke(&door, &fixture.caller, fixture.request.clone())
            .await
            .unwrap()
            .is_err(),
        "an identity-bearing host requires original custody"
    );
    assert!(!fixture.state.has_finished_shortcut_runs());
    assert!(
        fixture
            .state
            .completed_invocations
            .lock()
            .unwrap()
            .is_empty()
    );
    let mut key = fixture.key.clone();
    key.owner = InvocationOwner::Local {
        actor: BrokerActor::for_caller(&fixture.caller).id,
    };
    assert!(
        fixture
            .state
            .journal
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .lookup(&key)
            .unwrap()
            .is_none(),
        "actor text never establishes host admission"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mismatched_original_uuid_and_retired_host_start_nothing() {
    let mut fixture = Fixture::new();
    fixture.request.invocation = tilo_actions::InvocationId::new();
    assert!(fixture.invoke().await.is_err());
    fixture.request.invocation = fixture.key.invocation;
    fixture
        .state
        .accepting
        .store(false, std::sync::atomic::Ordering::Release);
    assert!(fixture.invoke().await.is_err());
    tokio::task::yield_now().await;
    assert!(!fixture.state.has_finished_shortcut_runs());
}
