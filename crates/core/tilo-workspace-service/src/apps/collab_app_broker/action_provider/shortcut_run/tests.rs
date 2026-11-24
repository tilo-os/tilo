#![allow(
    clippy::significant_drop_tightening,
    reason = "every test here keeps its fixture and guards alive through its assertions"
)]
use super::super::*;
use super::*;
use crate::apps::collab_app_broker::InvocationOwner;
use crate::apps::collab_app_broker::invocation_journal::{InvocationJournal, InvocationKey};
use crate::apps::services::{
    LocalPersonServices, ProcessServices, ShortcutLibraryRequest, ShortcutLibraryServices,
};
use crate::apps::test_support::{Workspace, WorkspaceId, WorkspaceTree};

struct Library(tokio::runtime::Handle);

impl ShortcutLibraryServices for Library {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        settings_store::hardware::ShortcutConfig::default()
    }
    fn store(&self, _: ShortcutLibraryRequest) -> crate::apps::services::ShortcutStore {
        Box::pin(async { Err("not a library editing test".into()) })
    }
    fn runtime(&self) -> Result<tokio::runtime::Handle, String> {
        Ok(self.0.clone())
    }
}

struct Person {
    certificate: tilo_user_identity::DeviceCert,
    time: crate::apps::policy_clock::PolicyTime,
}

impl LocalPersonServices for Person {
    fn certificate(&self, _: &str, person: &str) -> Result<tilo_user_identity::DeviceCert, String> {
        if person
            != crate::apps::collab_app_broker::person_id_for_root(self.certificate.user_public_key)
        {
            return Err("different person".into());
        }
        Ok(self.certificate.clone())
    }
}

impl crate::apps::certificate_authority::CertificateAuthority for Person {
    fn policy_time(&self) -> Result<Option<crate::apps::policy_clock::PolicyTime>, String> {
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
    state: Arc<AppBrokerState>,
    caller: Caller,
    origin: InvocationCaller,
    request: InvokeRequest,
    key: InvocationKey,
    _clock_origin: crate::apps::policy_clock::PolicyOrigin,
    _root: tempfile::TempDir,
    _environment: tilo_test_env::ScopedEnv,
}

impl Fixture {
    fn new() -> Self {
        Self::with_shortcuts(&[])
    }

    /// A fixture whose workspace also saves `extra`, appended to the ones
    /// every scenario has.
    fn with_extra(extra: tilo_automation::Shortcut) -> Self {
        Self::with_shortcuts(&[extra])
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, end to end, reads as one test"
    )]
    fn with_shortcuts(extra: &[tilo_automation::Shortcut]) -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment
            .set("XDG_CONFIG_HOME", scratch.path().join("config"))
            .set("TILO_IDENTITY_ROOT", scratch.path().join("identity"))
            .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
        tilo_identity::sources::test_trust::install();
        tilo_identity::provisioning::ensure_collaboration_identity(
            &tilo_identity::store::account_identity_dir(
                &scratch.path().join("config/tilo/identity"),
            ),
            &tilo_device_identity::IdentityPaths::under(scratch.path().join("identity")),
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let owner =
            crate::apps::test_owner::SessionOwner::initialize(&scratch.path().join("owner"))
                .unwrap();
        let certificate = owner.device();
        let (clock, _control) = crate::apps::policy_clock::OwnerPolicyClock::for_test(
            certificate.issued_at_ns.saturating_add(1_000_000_000),
        );
        let clock_origin = clock.new_origin();
        let time = clock_origin.capture().unwrap();
        let person = Arc::new(Person {
            certificate: certificate.clone(),
            time: time.clone(),
        });
        let actor = crate::apps::collab_app_broker::person_id_for_root(certificate.user_public_key);
        let mut workspace = Workspace::new(WorkspaceId::from_u128(9), "Shortcuts");
        workspace.policy.set_owner(actor.clone());
        let workspace_id = workspace.id().to_string();
        let echo: tilo_automation::Shortcut = serde_json::from_value(serde_json::json!({
            "id": "echo", "name": "Echo", "steps": [], "output": {"literal": "done"},
            "saved_by": actor,
            "triggers": [
                {"trigger": "keyboard", "chord": "Super+Shift+E"},
                {"trigger": "app_opened", "app_id": "notes"}
            ]
        }))
        .unwrap();
        workspace
            .shortcuts
            .push(tilo_automation::SavedShortcut::encode(&echo));
        for (id, target) in [("middle", "echo"), ("outer", "middle"), ("denied", "echo")] {
            let mut shortcut = echo.clone();
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
                    args: std::collections::BTreeMap::default(),
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
            workspace
                .shortcuts
                .push(tilo_automation::SavedShortcut::encode(&shortcut));
        }
        workspace
            .shortcuts
            .extend(extra.iter().map(tilo_automation::SavedShortcut::encode));
        // Saved by the person, proven by this device.
        for saved in &mut workspace.shortcuts {
            let mut shortcut = saved.decode().unwrap();
            tilo_peer_state::workspace::sign_saved_shortcut(
                &workspace_id,
                &mut shortcut,
                &certificate,
                owner.signing_key(),
            )
            .unwrap();
            *saved = tilo_automation::SavedShortcut::encode(&shortcut);
        }
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            ProcessServices {
                shortcut_library: Some(Arc::new(Library(tokio::runtime::Handle::current()))),
                local_person: Some(person.clone()),
                native_certificates: Some(person),
                ..Default::default()
            },
        );
        let journal = InvocationJournal::open(&scratch.path().join("journal")).unwrap();
        let invocation = tilo_actions::InvocationId::new();
        let key = InvocationKey {
            workspace: workspace_id.clone(),
            host: journal.host(),
            invocation,
            owner: InvocationOwner::AuthenticatedLocal {
                root: certificate.user_public_key,
                device: certificate.device_public_key,
                agent: None,
            },
        };
        *state.journal.lock().unwrap() = Ok(Some(journal));
        let caller = Caller::local_person(actor).in_workspace(&workspace_id);
        let origin = InvocationCaller::NativePerson {
            invocation,
            workspace: workspace_id.clone(),
            certificate: Box::new(certificate),
            time,
        };
        let request = InvokeRequest {
            invocation,
            target: tilo_actions::remote::RemoteActionTarget {
                provider: workspace_provider(&workspace_id),
                address: ActionAddress::new("shortcuts.shortcut:echo", "run"),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            on_behalf_of: None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        Self {
            state,
            caller,
            origin,
            request,
            key,
            _clock_origin: clock_origin,
            _root: scratch,
            _environment: environment,
        }
    }

    #[allow(
        clippy::future_not_send,
        reason = "the fixture's broker state is thread confined to the test's current-thread runtime"
    )]
    async fn invoke(&self) -> Result<InvocationReceipt, ActionCallError> {
        self.state
            .action_invoke_with_origin(&self.caller, None, None, Some(&self.origin), &self.request)
            .await
            .unwrap()
    }

    #[allow(
        clippy::future_not_send,
        reason = "the fixture's broker state is thread confined to the test's current-thread runtime"
    )]
    async fn finish(&self) {
        tilo_test_env::within("the shortcut completes", async {
            while !self
                .state
                .action_observe(&self.caller, &self.request.invocation)
                .is_some_and(|state| state.update.is_terminal())
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }
}

fn start_direct(
    fixture: &Fixture,
    trigger: Option<&tilo_automation::TriggerSpec>,
) -> Result<u64, ActionCallError> {
    let workspace = fixture.caller.workspace().unwrap();
    if let Some(trigger) = trigger {
        fixture.state.start_triggered_shortcut_run(
            &fixture.caller,
            workspace,
            "echo",
            trigger,
            tilo_automation::AutomationInvocationId(fixture.request.invocation.0),
            serde_json::Map::new(),
        )
    } else {
        fixture.state.start_interactive_shortcut_run(
            &fixture.caller,
            workspace,
            "echo",
            serde_json::Map::new(),
        )
    }
}

#[tokio::test(flavor = "current_thread")]
async fn direct_palette_and_triggered_runs_cross_signed_barriers() {
    for trigger in [
        None,
        Some(tilo_automation::TriggerSpec::AppOpened {
            app_id: "notes".into(),
        }),
    ] {
        let fixture = Fixture::new();
        let job = start_direct(&fixture, trigger.as_ref()).unwrap();
        // The run has not been polled: evidence must already be durable.
        let mut key = fixture.key.clone();
        key.invocation = {
            let completed = fixture.state.completed_invocations.lock().unwrap();
            let (_, update) = completed.get(&job).unwrap();
            let tilo_actions::InvocationUpdate::Accepted { invocation } = update else {
                panic!("run must be admitted before executing");
            };
            *invocation
        };
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
        crate::apps::action_evidence::verify(
            evidence.public_receipt(),
            &evidence.public_receipt().facts,
        )
        .unwrap();
        tilo_test_env::within("the shortcut run", async {
            while !fixture.state.has_finished_shortcut_runs() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let finished = fixture.state.take_finished_invocations();
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
                tilo_automation::InvocationMode::Unattended
            } else {
                tilo_automation::InvocationMode::Interactive
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
async fn direct_palette_refuses_native_ownership_mismatch_without_fallback() {
    let mut fixture = Fixture::new();
    let workspace = fixture.caller.workspace().unwrap().to_string();
    fixture.caller = Caller::local_person("another-person").in_workspace(workspace);
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
    crate::apps::action_evidence::verify(
        evidence.public_receipt(),
        &evidence.public_receipt().facts,
    )
    .unwrap();
    assert_eq!(record.key.invocation, fixture.request.invocation);
    assert!(
        fixture.state.pending_invocations.lock().unwrap().is_empty(),
        "a host run needs no fake application peer"
    );
    fixture.finish().await;
    fixture.invoke().await.unwrap();
    assert_eq!(fixture.state.take_finished_invocations().len(), 1);
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
    assert_eq!(fixture.state.take_finished_invocations().len(), 1);
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
    let finished = fixture.state.take_finished_invocations();
    assert_eq!(finished.len(), 3, "outer, middle and leaf each executed");
    let mut invocations = std::collections::HashSet::new();
    for run in &finished {
        assert_eq!(
            run.report.outcome,
            tilo_automation::InvocationOutcome::Completed
        );
        let mut key = fixture.key.clone();
        key.invocation = tilo_actions::InvocationId(run.report.invocation.0);
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
            .expect("each step has signed ownership");
        // Guarded history retains a request digest, not the private request.
        // The signed receipt must still identify this step's own UUID.
        assert_eq!(evidence.public_receipt().facts.invocation, key.invocation);
        crate::apps::action_evidence::verify(
            evidence.public_receipt(),
            &evidence.public_receipt().facts,
        )
        .unwrap();
        assert!(record.terminal.is_some());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn propagated_ownership_does_not_bypass_the_shortcut_grant() {
    let mut fixture = Fixture::new();
    fixture.request.target.address.resource = tilo_automation::shortcut_resource_id("denied");
    fixture.invoke().await.unwrap();
    fixture.finish().await;
    let finished = fixture.state.take_finished_invocations();
    assert_eq!(finished.len(), 1, "the ungranted child never starts");
    assert!(matches!(
        finished[0].report.outcome,
        tilo_automation::InvocationOutcome::Failed { .. }
    ));
}

/// A shortcut that runs the fixture's "denied" shortcut, whose run it cannot
/// do: the step fails, and every wait after it is a step that will never
/// finish, which is where a stop reaches a run.
fn waiting_shortcut() -> tilo_automation::Shortcut {
    tilo_automation::Shortcut {
        id: "waiting".into(),
        name: "Waiting".into(),
        inputs: Vec::new(),
        steps: vec![run_echo(), ask_a_person()],
        step_timeout_secs: None,
        output: None,
        grant: vec![tilo_automation::action_rule(
            SHORTCUTS_APP_ID,
            tilo_automation::SHORTCUT_RESOURCE_KIND,
            None,
            "run",
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
        )],
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

/// The one step [`waiting_shortcut`] ends at: asking a person a question no
/// unattended run may answer, so the run is at that step when a stop lands.
fn ask_a_person() -> tilo_automation::Step {
    tilo_automation::Step::new(
        "ask",
        tilo_automation::StepKind::Ask {
            param: tilo_actions::ActionParam::required(
                "which",
                tilo_actions::ParamType::Text,
                "Which one?",
            ),
        },
    )
}

fn run_echo() -> tilo_automation::Step {
    tilo_automation::Step::new(
        "first",
        tilo_automation::StepKind::Invoke {
            action: tilo_actions::ActionKey::new(
                SHORTCUTS_APP_ID,
                tilo_automation::SHORTCUT_RESOURCE_KIND,
                "run",
            ),
            resource: tilo_automation::Selector::Fixed(tilo_automation::shortcut_resource_id(
                "echo",
            )),
            args: std::collections::BTreeMap::default(),
            appearance: None,
        },
    )
}

/// Whether a finished run is waiting, without taking it.
#[allow(
    clippy::future_not_send,
    reason = "the fixture's broker state is thread confined to the test's current-thread runtime"
)]
async fn wait_for_finished(fixture: &Fixture) {
    tilo_test_env::within("the shortcut finishes", async {
        while !fixture.state.has_finished_shortcut_runs() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_stopped_run_ends_as_cancelled_keeping_the_steps_it_reached() {
    let fixture = Fixture::with_extra(waiting_shortcut());
    let workspace = fixture.caller.workspace().unwrap();
    fixture
        .state
        .start_interactive_shortcut_run(
            &fixture.caller,
            workspace,
            "waiting",
            serde_json::Map::new(),
        )
        .unwrap();
    // "waiting" goes as soon as it is started; the child it runs joins it.
    tilo_test_env::within("the run starts", async {
        while fixture.state.running_shortcut_runs(workspace).is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let stopped = fixture.state.stop_shortcut_runs(
        workspace,
        &automation::StopRequest::Shortcut("waiting".into()),
        "asked",
    );
    assert_eq!(
        stopped
            .iter()
            .map(|run| run.shortcut.as_str())
            .collect::<Vec<_>>(),
        vec!["waiting"],
        "the stop names the run going: {stopped:?}"
    );
    assert_eq!(stopped[0].name, "Waiting", "and what to call it");
    assert_eq!(stopped[0].stopped.as_deref(), Some("asked"));
    assert!(
        fixture
            .state
            .stop_shortcut_runs(
                workspace,
                &automation::StopRequest::Shortcut("waiting".into()),
                "asked"
            )
            .is_empty(),
        "a run is told to stop once, not again"
    );
    let mut reported = None;
    tilo_test_env::within("the stopped run ends", async {
        loop {
            if let Some(finished) = fixture
                .state
                .take_finished_invocations()
                .into_iter()
                .find(|run| run.report.shortcut == "waiting")
            {
                reported = Some(finished);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let reported = reported.expect("the stopped run still reports");
    assert_eq!(reported.report.shortcut, "waiting");
    assert!(
        matches!(
            reported.report.outcome,
            tilo_automation::InvocationOutcome::Cancelled { .. }
        ),
        "a stopped run is cancelled, not failed: {:?}",
        reported.report
    );
    assert!(
        fixture.state.running_shortcut_runs(workspace).is_empty(),
        "a stopped run is no longer going"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stopping_a_shortcut_that_is_not_running_stops_nothing_and_is_no_error() {
    let fixture = Fixture::new();
    let workspace = fixture.caller.workspace().unwrap();
    let stopped = fixture.state.stop_shortcut_runs(
        workspace,
        &automation::StopRequest::Shortcut("echo".into()),
        "asked",
    );
    assert!(stopped.is_empty());
    // The same command after a run finished is what somebody wanted anyway:
    // nothing is going, and nothing complains.
    let fixture = Fixture::new();
    fixture.invoke().await.unwrap();
    fixture.finish().await;
    wait_for_finished(&fixture).await;
    let _ = fixture.state.take_finished_invocations();
    let workspace = fixture.caller.workspace().unwrap();
    assert!(
        fixture
            .state
            .stop_shortcut_runs(workspace, &automation::StopRequest::All, "asked")
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_unscoped_broker_api_does_not_invent_native_ownership() {
    let fixture = Fixture::new();
    let api = BrokerApi::new(fixture.state.clone());
    assert!(
        tilo_actions::remote::ActionProvider::invoke(
            &api,
            &fixture.caller,
            fixture.request.clone()
        )
        .await
        .unwrap()
        .is_err(),
        "an identity-bearing host requires original ownership"
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
        actor: BrokerActor::for_caller(&fixture.caller).member().clone(),
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

/// A shortcut whose every step runs the fixture's `echo`, under a name of
/// its own so its runs can be told from `echo`'s.
fn several_steps(count: usize) -> tilo_automation::Shortcut {
    let mut shortcut = waiting_shortcut();
    shortcut.id = "several".into();
    shortcut.name = "Several".into();
    shortcut.steps = (0..count).map(|_| run_echo()).collect();
    shortcut
}

/// The record of a run of `several` that has finished.
#[allow(
    clippy::future_not_send,
    reason = "the fixture's broker state is thread confined to the test's current-thread runtime"
)]
async fn finished_run(fixture: &Fixture) -> automation::FinishedInvocation {
    tilo_test_env::within("the run finishes", async {
        loop {
            if let Some(run) = fixture
                .state
                .take_finished_invocations()
                .into_iter()
                .find(|run| run.shortcut.id == "several")
            {
                return run;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn a_continued_run_does_not_run_the_steps_before_the_one_it_goes_on_from() {
    let fixture = Fixture::with_extra(several_steps(4));
    let workspace = fixture.caller.workspace().unwrap().to_owned();
    let origin = fixture
        .state
        .start_interactive_shortcut_run(
            &fixture.caller,
            &workspace,
            "several",
            serde_json::Map::new(),
        )
        .expect("the shortcut starts");
    let first = finished_run(&fixture).await;
    assert_eq!(
        first.report.outcome,
        tilo_automation::InvocationOutcome::Completed
    );
    assert_eq!(first.report.steps.len(), 4, "{:?}", first.report);

    // The person fixed the third step and runs the shortcut again from it.
    let third = tilo_automation::StepPath::parse("2");
    let continued = fixture
        .state
        .start_interactive_shortcut_run_continuing(
            &fixture.caller,
            &workspace,
            "several",
            serde_json::Map::new(),
            Some(tilo_automation::Continuation {
                invocation: first.report.invocation,
                from: third.clone(),
            }),
        )
        .expect("a recorded run can be continued");
    assert_ne!(continued, origin, "continuing starts a run of its own");
    let second = finished_run(&fixture).await;
    assert_eq!(
        second.report.outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{:?}",
        second.report
    );
    assert_eq!(second.report.steps.len(), 4, "{:?}", second.report);
    // The two steps before the one it goes on from were not run again: they
    // keep the invocations the first run gave them, which is what keeps
    // their effects the ones that happened.
    for index in 0..2 {
        assert_eq!(
            second.report.steps[index].invocation, first.report.steps[index].invocation,
            "step {index} is taken from the record, not run again"
        );
        assert_eq!(
            second.report.steps[index].path,
            first.report.steps[index].path
        );
    }
    // The step it goes on from and everything after it run again, as this
    // run's own steps.
    for index in 2..4 {
        let path = tilo_automation::StepPath::parse(&index.to_string());
        assert_eq!(
            second.report.steps[index].invocation,
            Some(tilo_automation::step_invocation_id(
                second.report.invocation,
                &path
            )),
            "step {index} runs again under this run's own invocation"
        );
        assert_ne!(
            second.report.steps[index].invocation, first.report.steps[index].invocation,
            "step {index} is not taken from the record"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn continuing_a_run_the_broker_did_not_record_is_refused_and_starts_nothing() {
    let fixture = Fixture::with_extra(several_steps(1));
    let workspace = fixture.caller.workspace().unwrap().to_owned();
    let refused = fixture.state.start_interactive_shortcut_run_continuing(
        &fixture.caller,
        &workspace,
        "several",
        serde_json::Map::new(),
        Some(tilo_automation::Continuation {
            invocation: tilo_automation::AutomationInvocationId::new(),
            from: tilo_automation::StepPath::parse("0"),
        }),
    );
    assert!(
        matches!(refused, Err(ActionCallError::Refused { .. })),
        "a run the broker has no record of is refused: {refused:?}"
    );
    assert!(
        fixture.state.running_shortcut_runs(&workspace).is_empty(),
        "and nothing starts"
    );
    assert!(!fixture.state.has_finished_shortcut_runs(), "nor finishes");
}

#[tokio::test(flavor = "current_thread")]
async fn continuing_a_run_from_the_run_itself_is_refused() {
    let fixture = Fixture::with_extra(several_steps(1));
    let workspace = fixture.caller.workspace().unwrap().to_owned();
    let refused = fixture.state.start_interactive_shortcut_run_continuing(
        &fixture.caller,
        &workspace,
        "several",
        serde_json::Map::new(),
        Some(tilo_automation::Continuation {
            invocation: tilo_automation::AutomationInvocationId::new(),
            from: tilo_automation::StepPath::root(),
        }),
    );
    assert!(
        matches!(refused, Err(ActionCallError::InvalidRequest { .. })),
        "a continuation names the step it goes on from: {refused:?}"
    );
}

/// A shortcut whose one step runs `echo`, which its grant leaves to the person.
fn guarded_shortcut() -> tilo_automation::Shortcut {
    tilo_automation::Shortcut {
        id: "guarded".into(),
        name: "Guarded".into(),
        steps: vec![run_echo()],
        grant: vec![tilo_automation::action_rule(
            SHORTCUTS_APP_ID,
            tilo_automation::SHORTCUT_RESOURCE_KIND,
            None,
            "run",
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Ask,
        )],
        ..waiting_shortcut()
    }
}

/// The dispatch that carried the call, as the person's answer reaches it: what it was handed.
#[derive(Default)]
struct Carried {
    readmitted: std::sync::Mutex<Vec<InvokeRequest>>,
    declined: std::sync::Mutex<Vec<tilo_actions::InvocationId>>,
}

impl tilo_actions::remote::asked::AskedDispatch for Carried {
    fn context(&self, _: tilo_actions::InvocationId) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn readmit(
        &self,
        _: &Caller,
        request: InvokeRequest,
        _: &serde_json::Value,
    ) -> Result<(), ActionCallError> {
        self.readmitted.lock().unwrap().push(request);
        Ok(())
    }

    fn declined(
        &self,
        _: &Caller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<(), ActionCallError> {
        self.declined.lock().unwrap().push(invocation);
        Ok(())
    }
}

/// A run of `guarded` invoked through the broker, refused for want of the person's yes, and
/// kept as its asked record the way the dispatch that carried it keeps it.
struct Asked {
    fixture: Fixture,
    store: Arc<tilo_actions::remote::asked::AskedStore>,
    carried: Arc<Carried>,
    record: tilo_actions::remote::asked::RecordRef,
    facts: tilo_actions::remote::asked::AskedFacts,
}

impl Asked {
    #[allow(
        clippy::future_not_send,
        reason = "the fixture's broker state is thread confined to the test's current-thread runtime"
    )]
    async fn new() -> Self {
        let mut fixture = Fixture::with_extra(guarded_shortcut());
        fixture.request.target.address = ActionAddress::new("shortcuts.shortcut:guarded", "run");
        let Err(ActionCallError::ConfirmationRequired(facts)) = fixture.invoke().await else {
            panic!("a run whose step the grant leaves to the person asks before it starts");
        };
        let store = tilo_actions::remote::asked::AskedStore::opened(None, None, Vec::new())
            .expect("the store keeps the call");
        let carried = Arc::new(Carried::default());
        let dispatch: Arc<dyn tilo_actions::remote::asked::AskedDispatch> = carried.clone();
        let record = store
            .for_dispatch("device", Arc::downgrade(&dispatch))
            .write_asked(
                fixture.caller.workspace().unwrap(),
                &fixture.caller,
                &fixture.request,
                &facts,
            )
            .unwrap();
        Self {
            fixture,
            store,
            carried,
            record,
            facts: *facts,
        }
    }

    async fn answer(
        &self,
        answering: &Caller,
        resolution: tilo_actions::remote::asked::Offer,
    ) -> Result<(), ActionCallError> {
        self.store
            .resolve(
                &self.record,
                answering,
                &tilo_actions::remote::asked::Resolve {
                    resolution,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    }

    /// Nothing of the run started: no job answers for its call, and no run of it finished.
    fn ran_nothing(&self) {
        assert!(
            self.fixture
                .state
                .action_observe(&self.fixture.caller, &self.fixture.request.invocation)
                .is_none(),
            "nothing was admitted for the call"
        );
        assert!(!self.fixture.state.has_finished_shortcut_runs());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_step_the_grant_leaves_to_the_person_asks_before_the_run_starts() {
    let asked = Asked::new().await;
    // What the person is shown: the shortcut, and the step that needs their yes with what
    // it acts on.
    assert_eq!(asked.facts.on.as_deref(), Some("Guarded"));
    assert!(matches!(
        &asked.facts.by,
        tilo_actions::remote::asked::AskedBy::App { app, title, .. }
            if app == SHORTCUTS_APP_ID && title == "Guarded"
    ));
    assert_eq!(asked.facts.facts.len(), 1, "{:?}", asked.facts);
    assert!(
        asked.facts.facts[0].value.contains("echo"),
        "{:?}",
        asked.facts.facts
    );
    asked.ran_nothing();
}

#[tokio::test(flavor = "current_thread")]
async fn the_persons_approve_runs_the_shortcut_with_the_step_that_asked() {
    let asked = Asked::new().await;
    asked
        .answer(
            &asked.fixture.caller,
            tilo_actions::remote::asked::Offer::Approve,
        )
        .await
        .expect("the person it asks answers it");
    let readmitted = asked.carried.readmitted.lock().unwrap().pop().unwrap();
    assert_eq!(
        readmitted.approval().map(|approval| &approval.record),
        Some(&asked.record)
    );
    // The dispatch sends the same call again carrying the approve: the run starts and its
    // step runs `echo`.
    let fixture = &asked.fixture;
    fixture
        .state
        .action_invoke_with_origin(
            &fixture.caller,
            None,
            None,
            Some(&fixture.origin),
            &readmitted,
        )
        .await
        .unwrap()
        .expect("the approved call runs");
    fixture.finish().await;
    let report = fixture
        .state
        .shortcut_run_report(
            tilo_automation::AutomationInvocationId(readmitted.invocation.0),
            "guarded",
        )
        .expect("the run is recorded");
    assert_eq!(
        report.outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{report:?}"
    );
    assert_eq!(report.steps.len(), 1, "{report:?}");
    assert!(
        matches!(
            report.steps[0].status,
            tilo_automation::StepStatus::Succeeded { .. }
        ),
        "{report:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_declined_run_runs_nothing() {
    let asked = Asked::new().await;
    asked
        .answer(
            &asked.fixture.caller,
            tilo_actions::remote::asked::Offer::Decline,
        )
        .await
        .expect("the person it asks declines it");
    assert_eq!(
        *asked.carried.declined.lock().unwrap(),
        vec![asked.fixture.request.invocation]
    );
    assert!(asked.carried.readmitted.lock().unwrap().is_empty());
    asked.ran_nothing();
}

#[tokio::test(flavor = "current_thread")]
async fn an_agent_cannot_answer_for_the_person_it_acts_for() {
    let asked = Asked::new().await;
    let person = asked
        .fixture
        .caller
        .acting()
        .person_id()
        .unwrap()
        .to_owned();
    let agent = Caller::local_agent("scout")
        .acting_for(person)
        .in_workspace(asked.fixture.caller.workspace().unwrap());
    asked
        .answer(&agent, tilo_actions::remote::asked::Offer::Approve)
        .await
        .expect_err("an agent answers for nobody");
    assert!(asked.carried.readmitted.lock().unwrap().is_empty());
    asked.ran_nothing();
}

/// A run a pressed key started says so on its call through the dispatch: its history names the
/// keyboard trigger with its chord. A trigger the shortcut does not have reads as a run by hand.
#[tokio::test(flavor = "current_thread")]
async fn a_run_a_pressed_key_started_says_so_in_its_history() {
    for (chord, kind) in [("Super+Shift+E", "keyboard"), ("Super+Shift+Q", "manual")] {
        let mut fixture = Fixture::new();
        let pressed = tilo_automation::TriggerSpec::Keyboard {
            chord: chord.into(),
        };
        fixture.request.context = Some(automation::fired_context(&pressed));
        fixture.invoke().await.unwrap();
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
        let expected = if kind == "keyboard" {
            serde_json::to_value(&pressed).unwrap()
        } else {
            serde_json::json!({"trigger": "manual"})
        };
        assert_eq!(record.intent["host_start"]["trigger"], expected, "{chord}");
        fixture.finish().await;
        let finished = fixture.state.take_finished_invocations();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].report.trigger, kind, "{chord}");
    }
}
