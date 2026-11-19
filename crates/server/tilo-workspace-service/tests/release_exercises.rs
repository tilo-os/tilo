//! End-to-end verification of Release Exercise A (Runtime) and
//! Release Exercise B (Personal Assistant).
//!
//! Each test drives the production workspace service, automation dispatch,
//! activity undo, notification log, dated learned facts, per-thread topic
//! isolation, and delegated turn account-binding paths directly.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in an integration test these are the assertions and retained service handles"
)]

tilo_automerge_isolate::test_worker!();

use std::path::{Path, PathBuf};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use agent_provider_config::protocol::{Message, ModelRequest, Role, ToolChoice};
use agent_provider_config::{
    Interaction, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
};
use ed25519_dalek::SigningKey;
use serde_json::{Map, Value, json};
use tilo_action_policy::{ActionKind, Answer, KindSet, Rule, TreePath};
use tilo_actions::OperationKind;
use tilo_actions::contract::ActionContract;
use tilo_actions::remote::person_accounts::{
    AccountAssistants, BeginSend, MailAccounts, PROVIDER as ACCOUNTS_PROVIDER,
    RESOURCE as ACCOUNTS_RESOURCE, SendMail, StartedSend,
};
use tilo_actions::remote::person_telling::{
    BY_ANSWERED, BY_DISMISSED, Cause as TellingCause, Kind as TellingKind, ListAfter, Tell,
};
use tilo_actions::remote::workspace_activity::{
    ConcurrentEditGuard, Relation, Session as ActivitySession, UndoEffect, UndoEffectState,
    UndoOutcomeKind, UndoSessionOutcome, guard_concurrent_human_edit,
};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_automation::dispatch::{
    Armed, DiscretionaryAvailability, DispatchAdmission, DispatchClass, Event, Firer, Firing,
    ScheduleEvent, SourceEvent, TimeEvent, admit_firing, classify_trigger, dispatch,
    partition_admitted,
};
use tilo_automation::shortcut::{CatchUp, Schedule, Shortcut, TimeOfDay, TriggerSpec};
use tilo_clock::time::Instant;
use tilo_server_protocol::{WorkFrom, WorkSetting, WorkspaceId};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use tilo_workspace_service::Reaching;
use tilo_workspace_service::service::{
    ServiceConfig, ServiceIdentity, WorkspaceService, local_caller,
};
use tilo_workspace_service::workspaces::ToTell;
use tilo_workspace_service::workspaces::hosted_use::{HostedTurn, TurnMail};
use tilo_workspace_service::workspaces::person_learned::{
    DateWindow, FactAuthority, FactCorrection, FactUpdate, IsoDate, WindowBoundary,
};
use tilo_workspace_service::workspaces::person_model::{
    AllowedDestination, AnswerCost, DestinationPolicy, ModelRefusal, SpendError, Spender,
};
use tilo_workspace_service::workspaces::person_threads::{
    ThreadTopic, TopicAuthority, TopicSelection, TopicStoreError, topic_options_for_sender,
    topic_options_for_sender_with_question,
};

const ROOT_AGENT: &str = "pa-root";
const SUB_AGENT: &str = "research-worker";

fn person_root(state_root: &Path, person: &[u8; 32]) -> PathBuf {
    state_root
        .join("workspaces")
        .join("people")
        .join(hex::encode(person))
}

fn owner_identity(seed: u8) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[seed; 32]);
    let device = SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: format!("device-{seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (device, cert)
}

async fn open_service(state_root: &Path, key: &SigningKey, cert: &DeviceCert) -> WorkspaceService {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .expect("loopback endpoint binds");
    WorkspaceService::open(
        ServiceConfig {
            reaching: Reaching::OnItsOwn,
            ..ServiceConfig::new(
                state_root.to_owned(),
                WorkSetting {
                    from: WorkFrom::Anyone,
                    ..WorkSetting::own_person()
                },
            )
        },
        ServiceIdentity {
            user_public_key: cert.user_public_key,
            device_signing_key: key.clone(),
            device_cert: cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint,
        None,
    )
    .await
    .expect("workspace service opens")
}

fn configure_providers(
    state_root: &Path,
    person: &[u8; 32],
    profiles: Vec<ProviderProfile>,
    default_id: &str,
    keys: &[(&ProviderProfile, &str)],
) {
    let root = person_root(state_root, person);
    std::fs::create_dir_all(&root).unwrap();
    let store = settings_store::profile(&root);
    let mut config = ProviderConfig::load_in(&*store);
    config.profiles = profiles;
    config.default_profile = Some(default_id.to_owned());
    config.save_in(&*store).unwrap();
    let credentials = accounts::ServiceCredentials::open_in(&root);
    for (profile, api_key) in keys {
        let (reference, authorization) =
            accounts::provider_api_key(&profile.id, &profile.endpoint).unwrap();
        credentials
            .store(&reference, &authorization, api_key)
            .unwrap();
    }
}

fn provider_profile(
    id: &str,
    label: &str,
    protocol: ProviderProtocol,
    endpoint: &str,
    models: &[&str],
) -> ProviderProfile {
    ProviderProfile {
        id: id.to_owned(),
        label: label.to_owned(),
        protocol,
        endpoint: endpoint.to_owned(),
        models: models.iter().map(|id| ModelDescriptor::new(*id)).collect(),
        default_model: models.first().map(|id| (*id).to_owned()),
        ..ProviderProfile::default()
    }
}

fn model_request(prompt: &str) -> ModelRequest {
    ModelRequest {
        messages: vec![
            Message::text(Role::System, "You assist Ada."),
            Message::text(Role::User, prompt),
        ],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 128,
    }
}

fn soon() -> Instant {
    tilo_clock::now()
        .checked_add(Duration::from_secs(15))
        .unwrap()
}

fn spend_cap(workspace_id: WorkspaceId, agent: &str, day: &str, limit_micros: u64) -> Spender {
    Spender {
        workspace: workspace_id.to_string(),
        agent: agent.to_owned(),
        name: "Worker".to_owned(),
        day: day.to_owned(),
        limit_micros: Some(limit_micros),
    }
}

fn invoke_request(provider: &str, resource: &str, action: &str, argument: Value) -> InvokeRequest {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.to_owned(),
            address: ActionAddress::new(resource, action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

async fn invoke_as_owner(
    service: &WorkspaceService,
    workspace_id: WorkspaceId,
    cert: &DeviceCert,
    request: InvokeRequest,
) -> Result<Value, ActionCallError> {
    let caller = local_caller(cert);
    let pending = service
        .actions()
        .serve_certified(
            workspace_id,
            &caller,
            cert,
            RemoteActionMessage::Invoke(request),
        )
        .expect("certified action returns a receipt");
    match pending.await {
        Some(RemoteActionMessage::Receipt(receipt)) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            ReceiptStatus::Rejected { error } => Err(error),
            other => panic!("unexpected receipt status: {other:?}"),
        },
        other => panic!("unexpected action reply: {other:?}"),
    }
}

fn agent_turn_caller(cert: &DeviceCert, workspace: &str, agent: &str) -> Caller {
    Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_workspace(workspace)
        .delegated_to(agent)
        .named(agent)
        .in_agent_session("agent-session:release-exercise")
        .on_turn(
            tilo_action_policy::turn::Turn::new("conversation-1", "message-1"),
            Vec::new(),
        )
}

fn iso_date(raw: &str) -> IsoDate {
    IsoDate::parse(raw).expect("valid ISO date")
}

// ============================================================================
// Release Exercise A — Runtime
// ============================================================================

/// Workspace data-destination policy blocks unapproved providers across main,
/// subtask, reflection, and classifier call paths and names allowed
/// alternatives in the refusal.
#[tokio::test]
async fn runtime_workspace_destination_policy_blocks_unapproved_provider_and_names_alternatives() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = owner_identity(10);
    let service = open_service(dir.path(), &key, &owner).await;
    let workspaces = service.workspaces();
    let created = workspaces.create("Confidential").await.unwrap();
    let workspace_id = created.workspace_id;
    let person = owner.user_public_key;

    let cloud = provider_profile(
        "cloud-frontier",
        "Cloud Frontier",
        ProviderProtocol::OpenAiCompatible,
        "https://api.frontier.example/v1/chat",
        &["frontier-pro"],
    );
    let local = provider_profile(
        "local-ollama",
        "Local Ollama",
        ProviderProtocol::OpenAiCompatible,
        "http://127.0.0.1:11434/v1",
        &["local-model"],
    );
    configure_providers(
        dir.path(),
        &person,
        vec![cloud.clone(), local.clone()],
        &cloud.id,
        &[(&cloud, "sk-test-cloud-key")],
    );
    workspaces.call_private_model_addresses(true);

    #[cfg(feature = "test-support")]
    let scripted = {
        use tilo_workspace_service::workspaces::person_model::scripted::{Reply, ScriptedProvider};
        let provider = ScriptedProvider::answering([Reply::text("local ok")]);
        workspaces.answer_models_with(provider.clone());
        provider
    };

    // Enforce NoneOutsideServer on this workspace.
    workspaces
        .set_workspace_destinations(workspace_id, DestinationPolicy::NoneOutsideServer)
        .unwrap();
    assert_eq!(
        workspaces.workspace_destinations(&workspace_id).unwrap(),
        DestinationPolicy::NoneOutsideServer
    );

    // Every call path (main task, direct request, subtask, reflection one-shot,
    // classifier) must refuse the unapproved external provider and name the
    // local alternative.
    for kind in [
        Interaction::Task,
        Interaction::Request,
        Interaction::Subtask,
        Interaction::OneShot,
        Interaction::Classifier,
    ] {
        let model = workspaces
            .person_model(person, kind)
            .working_in(workspace_id);
        let linked_err = model.linked().unwrap_err();
        assert_eq!(
            linked_err,
            ModelRefusal::WorkspaceRefuses {
                provider: "Cloud Frontier".into(),
                destination: "https://api.frontier.example".into(),
                allowed_alternatives: vec!["Local Ollama".into()],
            },
            "destination policy must block {kind:?} before dispatch"
        );
        let words = linked_err.words(None);
        assert!(words.contains("Cloud Frontier"), "words: {words}");
        assert!(words.contains("Local Ollama"), "words: {words}");

        let answer_err = model
            .answer(
                &model_request("Summarize private board notes"),
                soon(),
                &mut |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(answer_err, linked_err);
    }

    // Explicit Only(Provider) policy behaves identically and still names the
    // approved provider.
    workspaces
        .set_workspace_destinations(
            workspace_id,
            DestinationPolicy::Only(vec![AllowedDestination::Provider("Local Ollama".into())]),
        )
        .unwrap();
    let only_err = workspaces
        .person_model(person, Interaction::OneShot)
        .working_in(workspace_id)
        .answer(
            &model_request("Reflect on private notes"),
            soon(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert_eq!(
        only_err,
        ModelRefusal::WorkspaceRefuses {
            provider: "Cloud Frontier".into(),
            destination: "https://api.frontier.example".into(),
            allowed_alternatives: vec!["Local Ollama".into()],
        }
    );

    // Switching to the allowed local alternative passes destination checks.
    let local_model = workspaces
        .person_model(person, Interaction::Task)
        .working_in(workspace_id)
        .naming(Some("Local Ollama/local-model"));
    let linked = local_model.linked().unwrap();
    assert_eq!(linked.provider, "Local Ollama");
    assert_eq!(linked.model.id, "local-model");

    #[cfg(feature = "test-support")]
    {
        assert!(
            scripted.asked().is_empty(),
            "refused calls must never reach any provider"
        );
        let reply = local_model
            .answer(&model_request("Allowed local summary"), soon(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(reply.text(), "local ok");
        assert_eq!(scripted.asked().len(), 1);
    }

    // A corrupt policy file fails closed and never allows dispatch.
    let corrupt_ws = WorkspaceId::new();
    let policy_dir = dir.path().join("workspaces").join("workspace-destinations");
    std::fs::create_dir_all(&policy_dir).unwrap();
    std::fs::write(policy_dir.join(format!("{corrupt_ws}.json")), b"{not-json").unwrap();
    let corrupt_err = workspaces
        .person_model(person, Interaction::Task)
        .working_in(corrupt_ws)
        .naming(Some("Local Ollama/local-model"))
        .answer(&model_request("Must fail closed"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert!(matches!(
        corrupt_err,
        ModelRefusal::WorkspaceRefuses {
            allowed_alternatives,
            ..
        } if allowed_alternatives.is_empty()
    ));

    service.shutdown().await;
}

/// Two concurrent workers racing against one remaining daily budget cap cannot
/// overshoot: atomic pre-call reservation admits 1, refuses the 2nd before
/// dispatch, and reconciles actual spend upon completion (or releases cleanly
/// on failure).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_concurrent_workers_respect_atomic_daily_budget_cap() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = owner_identity(20);
    let service = open_service(dir.path(), &key, &owner).await;
    let workspaces = service.workspaces();
    let created = workspaces.create("Budgeted").await.unwrap();
    let workspace_id = created.workspace_id;
    let person = owner.user_public_key;

    let local = provider_profile(
        "local-budget",
        "Local Budget",
        ProviderProtocol::OpenAiCompatible,
        "http://127.0.0.1:11435/v1",
        &["gpt-4o-mini"],
    );
    configure_providers(dir.path(), &person, vec![local.clone()], &local.id, &[]);
    workspaces.call_private_model_addresses(true);

    let spender = spend_cap(workspace_id, "worker-pool", "2026-11-17", 1_000_000);

    #[cfg(feature = "test-support")]
    {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use tokio::sync::Notify;

        use agent_provider_config::protocol::{
            ErrorKind, ModelError, ModelResponse, StopReason, Usage,
        };
        use tilo_workspace_service::workspaces::person_model::{
            Events, ModelProvider, ProviderCall, ProviderFuture,
        };

        struct HoldingProvider {
            entered: AtomicUsize,
            release: Notify,
            cost_micros: u64,
            fail: AtomicBool,
        }

        impl ModelProvider for HoldingProvider {
            fn answer<'a>(&'a self, _: ProviderCall<'a>, _: Events<'a>) -> ProviderFuture<'a> {
                self.entered.fetch_add(1, Ordering::SeqCst);
                let cost_micros = self.cost_micros;
                Box::pin(async move {
                    self.release.notified().await;
                    if self.fail.load(Ordering::Acquire) {
                        return Err(ModelError::new(ErrorKind::RateLimited));
                    }
                    Ok(ModelResponse {
                        message: Message::text(Role::Assistant, "completed"),
                        stop_reason: StopReason::EndTurn,
                        usage: Usage {
                            input_tokens: 100,
                            output_tokens: 40,
                            total_tokens: 140,
                            ..Default::default()
                        },
                        cost_micros,
                    })
                })
            }
        }

        let holding = Arc::new(HoldingProvider {
            entered: AtomicUsize::new(0),
            release: Notify::new(),
            cost_micros: 250_000,
            fail: AtomicBool::new(false),
        });
        workspaces.answer_models_with(holding.clone());

        // Daily cap is $1.00; each worker reserves $0.60 -> only 1 of 2
        // concurrent workers can be admitted simultaneously.
        let model = workspaces
            .person_model(person, Interaction::Subtask)
            .spending_as(spender.clone())
            .reserving(600_000);

        let w1 = {
            let model = model.clone();
            tokio::spawn(async move {
                model
                    .answer(&model_request("worker 1"), soon(), &mut |_| {})
                    .await
            })
        };
        let w2 = {
            let model = model.clone();
            tokio::spawn(async move {
                model
                    .answer(&model_request("worker 2"), soon(), &mut |_| {})
                    .await
            })
        };

        // Wait until one worker is held inside the provider and the other has
        // been rejected atomically.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !(w1.is_finished() ^ w2.is_finished()) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(holding.entered.load(Ordering::SeqCst), 1);
        assert_eq!(model.reserved_by(&spender), 600_000);

        let (finished, in_flight) = if w1.is_finished() { (w1, w2) } else { (w2, w1) };
        let refused = finished.await.unwrap().unwrap_err();
        assert!(
            matches!(refused, ModelRefusal::DayLimitReached { .. }),
            "2nd racing worker must be refused: {refused:?}"
        );

        // Release the 1st worker: reservation reconciles from $0.60 down to
        // actual spend ($0.25) and in-flight reserved balance drops to 0.
        holding.release.notify_waiters();
        let answer = in_flight.await.unwrap().unwrap();
        assert_eq!(answer.cost, AnswerCost::Known(250_000));
        assert_eq!(model.reserved_by(&spender), 0);
        assert_eq!(model.try_spent_by(&spender), Ok(250_000));

        // A worker reserving $0.80 against $0.75 remaining is refused; a failed
        // call within remaining cap releases its reservation with 0 extra spend.
        let overshoot = model
            .clone()
            .reserving(800_000)
            .answer(&model_request("too expensive"), soon(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(matches!(overshoot, ModelRefusal::DayLimitReached { .. }));

        holding.fail.store(true, Ordering::Release);
        let failing = {
            let model = model.clone().reserving(500_000);
            tokio::spawn(async move {
                model
                    .answer(&model_request("fails"), soon(), &mut |_| {})
                    .await
            })
        };
        let wait_until = tokio::time::Instant::now() + Duration::from_secs(2);
        while holding.entered.load(Ordering::SeqCst) < 2 && tokio::time::Instant::now() < wait_until
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        holding.release.notify_waiters();
        let err = failing.await.unwrap().unwrap_err();
        assert!(matches!(err, ModelRefusal::Provider(_)));
        assert_eq!(model.reserved_by(&spender), 0);
        assert_eq!(model.try_spent_by(&spender), Ok(250_000));
    }

    #[cfg(all(feature = "agents", not(feature = "test-support")))]
    {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::TcpListener;
        use tokio::sync::Notify;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let entered = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());
        let entered_srv = Arc::clone(&entered);
        let release_srv = Arc::clone(&release);
        let server_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            entered_srv.fetch_add(1, Ordering::SeqCst);
            release_srv.notified().await;
            let sse = concat!(
                "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o-mini\",\"choices\":[],\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":500,\"total_tokens\":1500}}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse.len(),
                sse
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });

        let live_local = provider_profile(
            "local-live-budget",
            "Local Live Budget",
            ProviderProtocol::OpenAiCompatible,
            &format!("http://127.0.0.1:{port}/v1"),
            &["gpt-4o-mini"],
        );
        configure_providers(
            dir.path(),
            &person,
            vec![live_local.clone()],
            &live_local.id,
            &[],
        );

        let model = workspaces
            .person_model(person, Interaction::Subtask)
            .spending_as(spender.clone())
            .reserving(600_000);

        let w1 = {
            let model = model.clone();
            tokio::spawn(async move {
                model
                    .answer(&model_request("worker 1"), soon(), &mut |_| {})
                    .await
            })
        };
        let w2 = {
            let model = model.clone();
            tokio::spawn(async move {
                model
                    .answer(&model_request("worker 2"), soon(), &mut |_| {})
                    .await
            })
        };

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !(w1.is_finished() ^ w2.is_finished()) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        assert_eq!(model.reserved_by(&spender), 600_000);

        let (finished, in_flight) = if w1.is_finished() { (w1, w2) } else { (w2, w1) };
        assert!(matches!(
            finished.await.unwrap(),
            Err(ModelRefusal::DayLimitReached { .. })
        ));

        release.notify_waiters();
        let answer = in_flight.await.unwrap().unwrap();
        server_task.await.unwrap();
        let actual_cost = answer.cost.micros().expect("priced gpt-4o-mini cost");
        assert!(actual_cost > 0);
        assert_eq!(model.reserved_by(&spender), 0);
        assert_eq!(model.try_spent_by(&spender), Ok(actual_cost));
    }

    #[cfg(not(any(feature = "test-support", feature = "agents")))]
    {
        // Under the default build without an injected model backend, verify
        // persisted daily spend admission, overshoot refusal before provider
        // dispatch, clean reservation release on failure, and fail-closed
        // handling of corrupt spend files.
        let spend_key = format!("{workspace_id}/worker-pool");
        let spend_file = person_root(dir.path(), &person).join("model-spend.json");
        std::fs::write(
            &spend_file,
            serde_json::to_vec(&json!({
                spend_key: {
                    "day": "2026-11-17",
                    "micros": 400_000
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let base_model = workspaces
            .person_model(person, Interaction::Subtask)
            .spending_as(spender.clone());
        assert_eq!(base_model.try_spent_by(&spender), Ok(400_000));

        // Two concurrent workers: one asking for $0.70 against $0.60 remaining
        // must be refused with DayLimitReached; one asking for $0.60 passes
        // admission (hits Unavailable provider) and releases its reservation.
        let over_model = base_model.clone().reserving(700_000);
        let fit_model = base_model.clone().reserving(600_000);
        let over_req = model_request("overshoot");
        let fit_req = model_request("within cap");
        let mut over_events = |_| {};
        let mut fit_events = |_| {};
        let (over_res, fit_res) = tokio::join!(
            over_model.answer(&over_req, soon(), &mut over_events),
            fit_model.answer(&fit_req, soon(), &mut fit_events),
        );
        assert!(matches!(
            over_res,
            Err(ModelRefusal::DayLimitReached { .. })
        ));
        assert!(matches!(fit_res, Err(ModelRefusal::Provider(_))));
        assert_eq!(base_model.reserved_by(&spender), 0);
        assert_eq!(base_model.try_spent_by(&spender), Ok(400_000));
    }

    // Corrupt spending file must fail closed on capped calls rather than
    // silently resetting spend to $0.
    let corrupt_ws = WorkspaceId::new();
    let corrupt_dir = tempfile::tempdir().unwrap();
    let (k2, o2) = owner_identity(22);
    let svc2 = open_service(corrupt_dir.path(), &k2, &o2).await;
    let p2 = o2.user_public_key;
    configure_providers(corrupt_dir.path(), &p2, vec![local.clone()], &local.id, &[]);
    svc2.workspaces().call_private_model_addresses(true);
    std::fs::write(
        person_root(corrupt_dir.path(), &p2).join("model-spend.json"),
        b"{broken-spend",
    )
    .unwrap();
    let capped = spend_cap(corrupt_ws, "worker-pool", "2026-11-17", 1_000_000);
    let capped_model = svc2
        .workspaces()
        .person_model(p2, Interaction::Subtask)
        .spending_as(capped.clone());
    assert_eq!(
        capped_model.try_spent_by(&capped),
        Err(SpendError::Unreadable)
    );
    let refused = capped_model
        .answer(&model_request("must fail closed"), soon(), &mut |_| {})
        .await
        .unwrap_err();
    assert_eq!(refused, ModelRefusal::SpendUnreadable);

    svc2.shutdown().await;
    service.shutdown().await;
}

/// Unknown model pricing surfaces `AnswerCost::Unknown` explicitly and tracks
/// unpriced calls separately rather than reporting them as `$0`.
#[tokio::test]
async fn runtime_unknown_model_pricing_reports_unknown_not_zero() {
    assert!(AnswerCost::Unknown.is_unknown());
    assert_eq!(AnswerCost::Unknown.micros(), None);
    assert!(!AnswerCost::Known(0).is_unknown());
    assert_eq!(AnswerCost::Known(0).micros(), Some(0));
    assert_ne!(AnswerCost::Unknown, AnswerCost::Known(0));

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = owner_identity(30);
    let service = open_service(dir.path(), &key, &owner).await;
    let workspaces = service.workspaces();
    let created = workspaces.create("Pricing").await.unwrap();
    let workspace_id = created.workspace_id;
    let person = owner.user_public_key;
    let spender = spend_cap(workspace_id, "pricing-agent", "2026-11-17", 5_000_000);

    #[cfg(feature = "test-support")]
    {
        use tilo_workspace_service::workspaces::person_model::scripted::{Reply, ScriptedProvider};

        let unpriced = provider_profile(
            "custom-unpriced",
            "Custom Unpriced",
            ProviderProtocol::OpenAiCompatible,
            "http://127.0.0.1:11436/v1",
            &["custom-unpriced-model-v9"],
        );
        configure_providers(
            dir.path(),
            &person,
            vec![unpriced.clone()],
            &unpriced.id,
            &[],
        );
        workspaces.call_private_model_addresses(true);
        workspaces.answer_models_with(ScriptedProvider::answering([Reply::text("unpriced reply")]));

        let model = workspaces
            .person_model(person, Interaction::Task)
            .spending_as(spender.clone());
        let answer = model
            .answer(&model_request("Hello"), soon(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(answer.cost, AnswerCost::Unknown);
        assert!(answer.cost.is_unknown());
        assert_eq!(answer.cost.micros(), None);
        assert_eq!(model.unknown_calls_by(&spender), Ok(1));
        assert_eq!(model.try_spent_by(&spender), Ok(0));
    }

    #[cfg(all(feature = "agents", not(feature = "test-support")))]
    {
        use test_http::{Reply, Responder};

        let sse = concat!(
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"custom-unpriced-model-v9\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"unpriced reply\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"custom-unpriced-model-v9\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"custom-unpriced-model-v9\",\"choices\":[],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":30,\"total_tokens\":150}}\n\n",
            "data: [DONE]\n\n"
        );
        let responder = Responder::start(vec![
            Reply::status(200)
                .header("content-type", "text/event-stream")
                .body(sse),
        ]);
        let unpriced = provider_profile(
            "custom-unpriced",
            "Custom Unpriced",
            ProviderProtocol::OpenAiCompatible,
            &format!("{}/v1", responder.url()),
            &["custom-unpriced-model-v9"],
        );
        configure_providers(
            dir.path(),
            &person,
            vec![unpriced.clone()],
            &unpriced.id,
            &[],
        );
        workspaces.call_private_model_addresses(true);

        let model = workspaces
            .person_model(person, Interaction::Task)
            .spending_as(spender.clone());
        let answer = model
            .answer(&model_request("Hello"), soon(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(answer.cost, AnswerCost::Unknown);
        assert_eq!(model.unknown_calls_by(&spender), Ok(1));
        assert_eq!(model.try_spent_by(&spender), Ok(0));
    }

    #[cfg(not(any(feature = "test-support", feature = "agents")))]
    {
        let root = person_root(dir.path(), &person);
        std::fs::create_dir_all(&root).unwrap();
        let spend_key = format!("{workspace_id}/pricing-agent");
        std::fs::write(
            root.join("model-spend.json"),
            serde_json::to_vec(&json!({
                spend_key: {
                    "day": "2026-11-17",
                    "micros": 0,
                    "unknown_calls": 2
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let model = workspaces
            .person_model(person, Interaction::Task)
            .spending_as(spender.clone());
        assert_eq!(model.unknown_calls_by(&spender), Ok(2));
        assert_eq!(model.try_spent_by(&spender), Ok(0));
    }

    service.shutdown().await;
}

/// Deterministic due reminders classify as `DispatchClass::DeterministicDue`
/// and pass admission even when the decision model is down and discretionary
/// spend is exhausted, while discretionary proactive work defers.
#[test]
fn runtime_deterministic_due_reminder_bypasses_judge_outage_and_exhausted_budget() {
    let once_trigger = TriggerSpec::Once { at_ms: 14_000 };
    let due_time_trigger = TriggerSpec::ResourceTime {
        kind: "tasks.issues".to_owned(),
        field: "issues.*.due_ms".to_owned(),
        offset_secs: 0,
        filter: Map::new(),
    };
    let calendar_trigger = TriggerSpec::Schedule {
        schedule: Schedule::Calendar {
            at: TimeOfDay { hour: 9, minute: 0 },
            weekdays: Vec::new(),
        },
        catch_up: CatchUp::default(),
    };
    let polling_trigger = TriggerSpec::Schedule {
        schedule: Schedule::Interval { seconds: 900 },
        catch_up: CatchUp::default(),
    };
    let reactive_trigger = TriggerSpec::SourceChanged {
        kind: tilo_automation::source_change::PERSON_MAIL.to_owned(),
        at: String::new(),
        every_secs: None,
        filter: Map::new(),
    };

    for trigger in [&once_trigger, &due_time_trigger, &calendar_trigger] {
        let class = classify_trigger(trigger);
        assert_eq!(class, DispatchClass::DeterministicDue);
        assert!(class.is_deterministic_due());
        assert!(class.bypasses_discretionary_limits());
    }
    for trigger in [&polling_trigger, &reactive_trigger] {
        let class = classify_trigger(trigger);
        assert_eq!(class, DispatchClass::Discretionary);
        assert!(!class.is_deterministic_due());
        assert!(!class.bypasses_discretionary_limits());
    }

    let mut shortcut: Shortcut = serde_json::from_value(json!({
        "id": "due-and-discretionary",
        "name": "Due and discretionary",
        "steps": [],
        "saved_by": "ada"
    }))
    .unwrap();
    shortcut.triggers = vec![
        once_trigger,
        due_time_trigger,
        calendar_trigger,
        polling_trigger,
        reactive_trigger,
    ];
    let armed = [Armed {
        firer: Firer::Bot {
            installation: "pa-scheduler",
        },
        shortcut: &shortcut,
    }];

    let once_event = ScheduleEvent {
        workspace: "ws-1".to_owned(),
        trigger: 0,
        due_ms: 14_000,
    };
    let time_event = TimeEvent {
        workspace: "ws-1".to_owned(),
        trigger: 1,
        key: "issue-42".to_owned(),
        at_ms: 14_000,
        payload: Map::new(),
    };
    let calendar_event = ScheduleEvent {
        workspace: "ws-1".to_owned(),
        trigger: 2,
        due_ms: 14_000,
    };
    let polling_event = ScheduleEvent {
        workspace: "ws-1".to_owned(),
        trigger: 3,
        due_ms: 14_000,
    };
    let source_event = SourceEvent {
        workspace: "ws-1".to_owned(),
        trigger: 4,
        key: "mail-99".to_owned(),
        payload: Map::new(),
    };

    let mut firings = Vec::new();
    firings.extend(dispatch(&armed, Event::Schedule(&once_event)).firings);
    firings.extend(dispatch(&armed, Event::Time(&time_event)).firings);
    firings.extend(dispatch(&armed, Event::Schedule(&calendar_event)).firings);
    firings.extend(dispatch(&armed, Event::Schedule(&polling_event)).firings);
    firings.extend(dispatch(&armed, Event::Source(&source_event)).firings);
    assert_eq!(firings.len(), 5);

    // Outage + exhausted discretionary budget: all 3 deterministic due items
    // proceed; both discretionary items defer.
    let (admitted, deferred) = partition_admitted(
        firings.clone(),
        DiscretionaryAvailability::both_unavailable(),
    );
    let admitted_indices: Vec<usize> = admitted.iter().map(|firing| firing.trigger_index).collect();
    assert_eq!(admitted_indices, [0, 1, 2]);
    assert!(admitted.iter().all(Firing::is_deterministic_due));
    assert_eq!(deferred.len(), 2);
    for (firing, decision) in &deferred {
        assert!(!firing.is_deterministic_due());
        assert_eq!(*decision, DispatchAdmission::DeferredBudgetExhausted);
        assert!(!decision.is_admitted());
    }

    // Judge outage alone: due reminders proceed deterministically;
    // discretionary work defers with DeferredJudgeUnavailable.
    for firing in &firings {
        let decision = admit_firing(firing, DiscretionaryAvailability::judge_down());
        if firing.is_deterministic_due() {
            assert_eq!(decision, DispatchAdmission::ProceedDeterministic);
            assert!(decision.is_admitted());
        } else {
            assert_eq!(decision, DispatchAdmission::DeferredJudgeUnavailable);
            assert!(!decision.is_admitted());
        }
    }
}

/// Per-effect Undo preserves concurrent human edits (`PreservedConcurrentEdit`)
/// and reports already-delivered external actions (`AlreadyDelivered`) and lost
/// receipts (`Unconfirmed`) honestly instead of claiming complete success.
#[test]
fn runtime_per_effect_undo_preserves_concurrent_human_edits_and_reports_delivered_honestly() {
    assert_eq!(
        guard_concurrent_human_edit(&[Relation::Software]),
        ConcurrentEditGuard::SafeToUndo
    );
    assert_eq!(
        guard_concurrent_human_edit(&[Relation::Software, Relation::Person]),
        ConcurrentEditGuard::PreserveConcurrentEdit
    );
    assert_eq!(
        guard_concurrent_human_edit(&[Relation::Reader]),
        ConcurrentEditGuard::PreserveConcurrentEdit
    );

    let sheet_undone =
        UndoEffect::from_local_undo_attempt("Sheet row", &[Relation::Software], None)
            .with_resource("so.tilo.sheets", "sheet-budget")
            .with_invocation_id("inv-101");
    assert_eq!(sheet_undone.state, UndoEffectState::UndoneLocally);
    assert!(sheet_undone.is_undone());

    let doc_preserved = UndoEffect::from_local_undo_attempt(
        "Doc paragraph",
        &[Relation::Software, Relation::Person],
        Some("edited by Ada".into()),
    )
    .with_resource("so.tilo.writer", "doc-memo")
    .with_invocation_id("inv-102");
    assert_eq!(
        doc_preserved.state,
        UndoEffectState::PreservedConcurrentEdit
    );
    assert!(doc_preserved.preserved_concurrent_edit_flag());
    assert!(!doc_preserved.is_undone());
    assert_eq!(
        doc_preserved.phrase(),
        "Doc paragraph kept (edited by Ada)."
    );

    let email_delivered = UndoEffect::already_delivered("Email", None)
        .with_resource("so.tilo.mail", "msg-99")
        .with_invocation_id("inv-103");
    assert_eq!(email_delivered.state, UndoEffectState::AlreadyDelivered);
    assert!(email_delivered.is_already_delivered());
    assert!(!email_delivered.is_undone());
    assert_eq!(email_delivered.phrase(), "Email already delivered.");

    let external_contract = ActionContract::for_operation(OperationKind::Effect);
    let calendar_unconfirmed = UndoEffect::from_lost_receipt("Calendar update", &external_contract)
        .with_invocation_id("inv-104");
    assert_eq!(calendar_unconfirmed.state, UndoEffectState::Unconfirmed);
    assert!(calendar_unconfirmed.is_unconfirmed());

    let outcome = UndoSessionOutcome::new(vec![
        sheet_undone,
        doc_preserved.clone(),
        email_delivered.clone(),
        calendar_unconfirmed,
    ])
    .with_session_id("session-release-a");

    assert_eq!(outcome.kind(), UndoOutcomeKind::Partial);
    assert!(!outcome.is_complete());
    assert_eq!(outcome.undone_count(), 1);
    assert_eq!(outcome.preserved_concurrent_edit_count(), 1);
    assert_eq!(outcome.already_delivered_count(), 1);
    assert_eq!(outcome.unconfirmed_count(), 1);
    assert!(outcome.has_preserved_concurrent_edits());
    assert!(outcome.has_already_delivered());
    assert!(outcome.has_unconfirmed());
    assert_eq!(
        outcome.summary(),
        "Sheet row undone. Doc paragraph kept (edited by Ada). Email already delivered. Calendar update unconfirmed."
    );
    assert_eq!(outcome.unreverted_phrases().len(), 3);

    let mut session = ActivitySession::default();
    session.apply_undo_outcome(&outcome);
    assert_eq!(
        session.undo_status.as_deref(),
        Some(outcome.summary().as_str())
    );
    assert_eq!(session.unreverted.len(), 3);

    // JSON wire round-trip preserves every per-effect state.
    let encoded = serde_json::to_string(&outcome).unwrap();
    let decoded: UndoSessionOutcome = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, outcome);

    // When nothing could be reverted, outcome is NoneUndone (never Complete).
    let none_undone = UndoSessionOutcome::new(vec![doc_preserved, email_delivered]);
    assert_eq!(none_undone.kind(), UndoOutcomeKind::NoneUndone);
    assert!(!none_undone.is_complete());
}

// ============================================================================
// Release Exercise B — Personal Assistant
// ============================================================================

/// Poll `told_listed` once at wall timestamp `now_ms` so the server updates
/// `looked_at_ms` / `left_at_ms` / `back_at_ms` using the production code path.
fn poll_device_look_at(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    person: &[u8; 32],
    now_ms: u64,
) {
    tilo_clock::with_wall_ms(now_ms, || {
        let query = ListAfter {
            after: u64::MAX,
            wait_ms: Some(0),
            device: "mac".to_owned(),
        };
        let mut fut = Box::pin(workspaces.told_listed(person, &query));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let _ = matches!(fut.as_mut().poll(&mut cx), Poll::Ready(_) | Poll::Pending);
    });
}

/// Swiping/dismissing a notification or flooding 600 newer notices never
/// evicts an unanswered pending decision from catch-up or across a server
/// restart.
#[tokio::test]
async fn pa_pending_decision_survives_dismissal_and_600_new_notices_in_catch_up() {
    const MINUTE: u64 = 60 * 1_000;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = owner_identity(40);
    let service = open_service(dir.path(), &key, &owner).await;
    let workspaces = service.workspaces();
    let person = owner.user_public_key;

    let t0 = tilo_clock::unix_now_ms();
    // Device looks once at t0, establishing an active session baseline.
    poll_device_look_at(workspaces, &person, t0);

    let make_question = |id: &str, words: &str| ToTell {
        id: Some(id.to_owned()),
        kind: TellingKind::Question,
        tell: Tell {
            title: "Decision needed".to_owned(),
            words: words.to_owned(),
            ..Tell::default()
        },
        ..ToTell::default()
    };

    // Push three decisions: untouched, swiped/dismissed, and explicitly answered.
    let untouched = workspaces
        .tell(
            &person,
            make_question("decision-untouched", "Approve Q4 vendor quote?"),
        )
        .await
        .unwrap();
    assert!(untouched.waits());
    assert!(untouched.is_pending_decision());

    workspaces
        .tell(
            &person,
            make_question("decision-dismissed", "Send revised contract to legal?"),
        )
        .await
        .unwrap();
    let dismissed = workspaces
        .acknowledge_told(&person, "decision-dismissed", "iphone", BY_DISMISSED)
        .unwrap();
    assert!(
        dismissed.waits(),
        "dismissing a notification must not resolve the underlying pending decision"
    );
    assert!(dismissed.is_pending_decision());
    assert!(dismissed.resolved.is_none());

    workspaces
        .tell(
            &person,
            make_question("decision-answered", "Archive old draft?"),
        )
        .await
        .unwrap();
    let answered = workspaces
        .acknowledge_told(&person, "decision-answered", "mac", BY_ANSWERED)
        .unwrap();
    assert!(!answered.waits());
    assert!(!answered.is_pending_decision());

    // Push 600 new informational notices (well past the 500-entry history bound).
    for index in 0..600_u32 {
        workspaces
            .tell(
                &person,
                ToTell {
                    id: Some(format!("notice-{index}")),
                    kind: TellingKind::News,
                    tell: Tell {
                        words: format!("Background update #{index}"),
                        cause: Some(TellingCause {
                            kind: "event".to_owned(),
                            key: format!("evt-{index}"),
                            ..TellingCause::default()
                        }),
                        ..Tell::default()
                    },
                    ..ToTell::default()
                },
            )
            .await
            .unwrap();
    }

    // Person returns 20 minutes later (> 10-minute away threshold).
    poll_device_look_at(workspaces, &person, t0 + 20 * MINUTE);

    let after = workspaces.told_after(&person, 0);
    let pending_ids: Vec<&str> = after
        .told
        .iter()
        .filter(|item| item.is_pending_decision())
        .map(|item| item.id.as_str())
        .collect();
    let history_count = after
        .told
        .iter()
        .filter(|item| !item.is_pending_decision())
        .count();
    assert_eq!(pending_ids, ["decision-untouched", "decision-dismissed"]);
    assert_eq!(history_count, 500);
    assert!(
        !after.told.iter().any(|item| item.id == "decision-answered"),
        "already-answered decision ages out with bounded history"
    );
    assert!(
        !after.told.iter().any(|item| item.id == "notice-0"),
        "oldest informational notice is evicted"
    );
    assert!(
        after.told.iter().any(|item| item.id == "notice-599"),
        "newest informational notice is kept"
    );

    let stretch = workspaces.since_left(&person);
    let wait_ids: Vec<&str> = stretch.waits.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(wait_ids, ["decision-untouched", "decision-dismissed"]);

    // Restart the workspace service on the same disk directory and verify
    // durability of both pending decisions in catch-up.
    service.shutdown().await;
    let restarted = open_service(dir.path(), &key, &owner).await;
    let restarted_stretch = restarted.workspaces().since_left(&person);
    let restarted_wait_ids: Vec<&str> = restarted_stretch
        .waits
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(
        restarted_wait_ids,
        ["decision-untouched", "decision-dismissed"]
    );

    // Explicitly answering the previously dismissed decision clears it from waits.
    let resolved = restarted
        .workspaces()
        .acknowledge_told(&person, "decision-dismissed", "mac", BY_ANSWERED)
        .unwrap();
    assert!(!resolved.waits());
    assert!(!resolved.is_pending_decision());
    let remaining_waits: Vec<String> = restarted
        .workspaces()
        .since_left(&person)
        .waits
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(remaining_waits, ["decision-untouched"]);

    restarted.shutdown().await;
}

/// Dated learned facts render fresh relative phrasing ("in 3 to 6 months" on
/// 2026-11-17 -> "in 2 to 5 months" on 2026-12-17), compute exact deterministic
/// boundary wakes, and cannot be silently overwritten by a model once confirmed
/// by the person.
#[tokio::test]
async fn pa_dated_learned_fact_renders_relative_computes_wakes_and_protects_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = owner_identity(50);
    let service = open_service(dir.path(), &key, &owner).await;
    let workspaces = service.workspaces();
    let created = workspaces.create("Personal").await.unwrap();
    let workspace_key = created.workspace_id.to_string();
    let person = owner.user_public_key;

    // On 2026-11-17, "in 3 to 6 months" is 2027-02-17..=2027-05-17.
    let fact_id = workspaces
        .keep_dated_fact(
            &person,
            &workspace_key,
            Some(ROOT_AGENT),
            FactUpdate {
                text: "Planning a Series A raise".to_owned(),
                source: "Conversation on 2026-11-17".to_owned(),
                starts_on: Some("2027-02-17".to_owned()),
                ends_on: Some("2027-05-17".to_owned()),
                authority: FactAuthority::LargeModel,
            },
        )
        .unwrap();

    let listed = workspaces.learned_facts_for(&person, &workspace_key);
    assert_eq!(listed.facts.len(), 1);
    let fact = &listed.facts[0];
    assert_eq!(fact.id, fact_id);
    assert!(!fact.confirmed);
    assert_eq!(fact.effective_authority(), FactAuthority::LargeModel);
    assert_eq!(
        fact.date_window().unwrap(),
        DateWindow::parse(Some("2027-02-17"), Some("2027-05-17")).unwrap()
    );
    assert_eq!(
        fact.render_relative(iso_date("2026-11-17")).as_deref(),
        Some("in 3 to 6 months")
    );
    assert_eq!(
        fact.render_relative(iso_date("2026-12-17")).as_deref(),
        Some("in 2 to 5 months")
    );
    assert!(
        fact.render_for_date(iso_date("2026-12-17"))
            .contains("in 2 to 5 months")
    );

    // Exact deterministic boundary wakes: 1 month before start, window start,
    // and window end.
    let wakes = workspaces.upcoming_fact_wakes(&person, &workspace_key, iso_date("2026-11-17"));
    let summary: Vec<(IsoDate, WindowBoundary, bool)> = wakes
        .iter()
        .map(|wake| (wake.on, wake.boundary, wake.confirmed))
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                iso_date("2027-01-17"),
                WindowBoundary::OneMonthBeforeStart,
                false
            ),
            (iso_date("2027-02-17"), WindowBoundary::WindowStart, false),
            (iso_date("2027-05-17"), WindowBoundary::WindowEnd, false),
        ]
    );

    // Person confirms the fact.
    workspaces
        .confirm_learned_fact(&person, &workspace_key, &fact_id, None)
        .unwrap();
    let confirmed_listed = workspaces.learned_facts_for(&person, &workspace_key);
    let confirmed_fact = &confirmed_listed.facts[0];
    assert!(confirmed_fact.confirmed);
    assert_eq!(confirmed_fact.effective_authority(), FactAuthority::Person);
    let confirmed_wakes =
        workspaces.upcoming_fact_wakes(&person, &workspace_key, iso_date("2026-11-17"));
    assert!(confirmed_wakes.iter().all(|wake| wake.confirmed));

    // Neither a LargeModel nor a DecisionModel may silently overwrite a
    // person-confirmed fact.
    for authority in [FactAuthority::LargeModel, FactAuthority::DecisionModel] {
        let refused = workspaces.correct_dated_fact(
            &person,
            &workspace_key,
            Some(ROOT_AGENT),
            &fact_id,
            FactCorrection {
                text: "Delayed raise to next year".to_owned(),
                source: Some("Model guess".to_owned()),
                starts_on: Some(Some("2027-09-01".to_owned())),
                ends_on: Some(Some("2027-12-01".to_owned())),
                authority,
            },
        );
        assert!(
            refused.is_err(),
            "{authority:?} must not overwrite a person-confirmed fact"
        );
    }

    // Fact text and dates remain intact after refused model edits.
    let intact = workspaces.learned_facts_for(&person, &workspace_key);
    assert_eq!(intact.facts[0].text, "Planning a Series A raise");
    assert_eq!(intact.facts[0].starts_on.as_deref(), Some("2027-02-17"));
    assert_eq!(intact.facts[0].ends_on.as_deref(), Some("2027-05-17"));

    // The person themselves may still correct their confirmed fact.
    workspaces
        .correct_dated_fact(
            &person,
            &workspace_key,
            None,
            &fact_id,
            FactCorrection {
                text: "Planning a Series A raise".to_owned(),
                source: Some("Updated by Ada".to_owned()),
                starts_on: Some(Some("2027-03-17".to_owned())),
                ends_on: Some(Some("2027-06-17".to_owned())),
                authority: FactAuthority::Person,
            },
        )
        .unwrap();
    let updated = workspaces.learned_facts_for(&person, &workspace_key);
    assert_eq!(
        updated.facts[0]
            .render_relative(iso_date("2026-12-17"))
            .as_deref(),
        Some("in 3 to 6 months")
    );

    service.shutdown().await;
}

/// Per-thread topic separation keeps sender identity (contact groups) distinct
/// from per-thread topic, narrows model choices honestly, and preserves person
/// overrides against model overwrites across restarts.
#[tokio::test]
async fn pa_per_thread_topic_separates_sender_identity_and_preserves_person_overrides() {
    // Multi-group sender ("Investors" + "Friends"): never auto-tagged as
    // "Investors" so a personal dinner thread can choose "neither".
    let multi_group = topic_options_for_sender(
        &["Investors", "Friends"],
        &["Investors", "Customers", "Hiring"],
    );
    assert_eq!(
        multi_group,
        TopicSelection::AskModel {
            options: vec!["Investors".into(), "neither".into()],
            candidates: vec!["Investors".into()],
            narrowed: true,
        }
    );
    assert!(multi_group.needs_model());

    // Single-group sender resolves deterministically when the event asks only
    // about identity, and still consults the model when the question is broader.
    let single_identity = topic_options_for_sender(&["Investors"], &["Investors", "Customers"]);
    assert_eq!(
        single_identity,
        TopicSelection::Deterministic("Investors".into())
    );
    assert!(!single_identity.needs_model());

    let single_non_identity =
        topic_options_for_sender_with_question(&["Investors"], &["Investors", "Customers"], false);
    assert!(single_non_identity.needs_model());
    assert_eq!(single_non_identity.options(), ["Investors", "neither"]);

    // Unknown sender returns all event options plus "none".
    let unknown = topic_options_for_sender(&[] as &[&str], &["Investors", "Customers"]);
    assert_eq!(unknown.options(), ["Investors", "Customers", "none"]);

    // Only a person may promote a sender into a durable contact group.
    assert!(TopicAuthority::Person.may_promote_contact_group());
    assert!(!TopicAuthority::LargeModel.may_promote_contact_group());
    assert!(!TopicAuthority::DecisionModel.may_promote_contact_group());

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = owner_identity(60);
    let service = open_service(dir.path(), &key, &owner).await;
    let workspaces = service.workspaces();
    let person = owner.user_public_key;

    // Same sender (an investor who is also a friend) has two separate threads:
    // a fundraising thread and a dinner thread.
    workspaces
        .set_thread_topic(
            &person,
            ThreadTopic::new(
                "thread-raise",
                "Investors",
                ROOT_AGENT,
                TopicAuthority::LargeModel,
                1_000,
            ),
        )
        .unwrap();
    workspaces
        .set_thread_topic(
            &person,
            ThreadTopic::new(
                "thread-dinner",
                "Investors",
                "decision-model",
                TopicAuthority::DecisionModel,
                1_100,
            ),
        )
        .unwrap();

    // Person overrides only the dinner thread to "neither".
    workspaces
        .set_thread_topic(
            &person,
            ThreadTopic::new(
                "thread-dinner",
                "neither",
                "ada",
                TopicAuthority::Person,
                1_200,
            ),
        )
        .unwrap();

    // Neither DecisionModel nor LargeModel can overwrite the person's override.
    for (authority, actor) in [
        (TopicAuthority::DecisionModel, "decision-model"),
        (TopicAuthority::LargeModel, ROOT_AGENT),
    ] {
        let err = workspaces
            .set_thread_topic(
                &person,
                ThreadTopic::new("thread-dinner", "Investors", actor, authority, 1_300),
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                TopicStoreError::Outranked {
                    existing: TopicAuthority::Person,
                    attempted,
                    ..
                } if attempted == authority
            ),
            "expected Outranked for {authority:?}, got {err:?}"
        );
    }

    // Fundraising thread remains "Investors"; dinner thread stays "neither"
    // across a service restart.
    service.shutdown().await;
    let restarted = open_service(dir.path(), &key, &owner).await;
    let raise_topic = restarted
        .workspaces()
        .thread_topic(&person, "thread-raise")
        .unwrap()
        .unwrap();
    let dinner_topic = restarted
        .workspaces()
        .thread_topic(&person, "thread-dinner")
        .unwrap()
        .unwrap();
    assert_eq!(raise_topic.topic, "Investors");
    assert_eq!(dinner_topic.topic, "neither");
    assert_eq!(dinner_topic.authority, TopicAuthority::Person);

    restarted.shutdown().await;
}

/// Sub-agents inherit their verified root assistant's Mail (and, when enabled,
/// Calendar) account bindings, obey explicit narrowing rules, still require
/// person approval before sending mail, and lose access immediately when the
/// root assistant is revoked.
#[tokio::test]
async fn pa_sub_agent_inherits_root_mail_and_calendar_bindings_and_loses_access_on_revocation() {
    const MAIL_ACCOUNT: &str = "ada@example.test";

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = owner_identity(70);
    let service = open_service(dir.path(), &key, &owner).await;
    let workspaces = service.workspaces();
    let created = workspaces.create("Assistant Team").await.unwrap();
    let workspace = created.workspace;
    let workspace_id = created.workspace_id;
    let person = owner.user_public_key;
    let keeper = hex::encode(person);

    // Seed Ada's mail account and credentials in her personal settings directory.
    let root_dir = person_root(dir.path(), &person);
    std::fs::create_dir_all(&root_dir).unwrap();
    let mail_id = accounts::AccountId::from(MAIL_ACCOUNT.to_owned());
    let mut mail_account = accounts::Account::new(mail_id, accounts::email::TYPE_ID);
    mail_account.config = json!({
        "email": MAIL_ACCOUNT,
        "username": MAIL_ACCOUNT
    });
    let mut store = accounts::AccountStore::load_in(&root_dir);
    store.upsert(mail_account.clone());
    store.save_in(&root_dir).unwrap();
    let mut vault = accounts::AccountCredentials::open_in(&root_dir).unwrap();
    vault.put_for(&mail_account, &accounts::Secret::password("test-mail-pass"));
    vault.try_save().unwrap();

    // Grant ONLY the root assistant access to the mail account through the
    // production `person_accounts::ASSISTANT` action.
    let granted_val = invoke_as_owner(
        &service,
        workspace_id,
        &owner,
        invoke_request(
            ACCOUNTS_PROVIDER,
            ACCOUNTS_RESOURCE,
            person_accounts::ASSISTANT,
            json!({
                "account": MAIL_ACCOUNT,
                "agent": ROOT_AGENT,
                "may": true
            }),
        ),
    )
    .await
    .unwrap();
    let granted: AccountAssistants = serde_json::from_value(granted_val).unwrap();
    assert_eq!(granted.agents, vec![ROOT_AGENT]);

    let sub_caller = agent_turn_caller(&owner, &workspace, SUB_AGENT);
    let base_turn = HostedTurn {
        workspace: workspace_id,
        caller: &sub_caller,
        keeper: &keeper,
        bound: None,
    };

    // Without a verified root assistant, the sub-agent sees no mail actions.
    assert!(workspaces.mail_offered(&base_turn).await.is_empty());
    assert_eq!(
        workspaces
            .mail_for_turn(
                &base_turn,
                &invoke_request(
                    ACCOUNTS_PROVIDER,
                    ACCOUNTS_RESOURCE,
                    person_accounts::LIST,
                    json!({}),
                ),
            )
            .await,
        Err(ActionCallError::UnknownAction)
    );

    // With the verified root assistant, the sub-agent inherits the mail binding.
    let sub_turn = base_turn.under_root(Some(ROOT_AGENT));
    let offered: Vec<String> = workspaces
        .mail_offered(&sub_turn)
        .await
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect();
    assert!(offered.iter().any(|action| action == person_accounts::LIST));
    assert!(
        offered
            .iter()
            .any(|action| action == person_accounts::FETCH)
    );
    assert!(offered.iter().any(|action| action == person_accounts::READ));

    let listed = match workspaces
        .mail_for_turn(
            &sub_turn,
            &invoke_request(
                ACCOUNTS_PROVIDER,
                ACCOUNTS_RESOURCE,
                person_accounts::LIST,
                json!({}),
            ),
        )
        .await
        .unwrap()
    {
        TurnMail::Done(value) => serde_json::from_value::<MailAccounts>(value).unwrap(),
        TurnMail::Asks(_) => panic!("listing mail accounts does not ask"),
    };
    assert_eq!(listed.accounts.len(), 1);
    assert_eq!(listed.accounts[0].id, MAIL_ACCOUNT);

    // Explicit narrowing rules still bound the sub-agent.
    let narrow_rules = vec![Rule::new(
        TreePath::workspace(&workspace)
            .app(ACCOUNTS_PROVIDER)
            .kind(ACCOUNTS_RESOURCE)
            .resource(ACCOUNTS_RESOURCE)
            .action(person_accounts::LIST),
        KindSet::of(ActionKind::Outside),
        Answer::Allow,
    )];
    let narrowed_sub = HostedTurn {
        bound: Some(&narrow_rules),
        ..base_turn
    }
    .under_root(Some(ROOT_AGENT));
    let narrowed_offered: Vec<String> = workspaces
        .mail_offered(&narrowed_sub)
        .await
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect();
    assert_eq!(narrowed_offered, vec![person_accounts::LIST]);
    assert_eq!(
        workspaces
            .mail_for_turn(
                &narrowed_sub,
                &invoke_request(
                    ACCOUNTS_PROVIDER,
                    ACCOUNTS_RESOURCE,
                    person_accounts::FETCH,
                    json!({"account": MAIL_ACCOUNT, "folder": "inbox"}),
                ),
            )
            .await,
        Err(ActionCallError::NotPermitted)
    );

    // External mail sends by the sub-agent still ask the person and attribute
    // the inherited account binding to ROOT_AGENT.
    let begun = match workspaces
        .mail_for_turn(
            &sub_turn,
            &invoke_request(
                ACCOUNTS_PROVIDER,
                ACCOUNTS_RESOURCE,
                person_accounts::BEGIN_SEND,
                serde_json::to_value(BeginSend {
                    message: SendMail {
                        account: MAIL_ACCOUNT.to_owned(),
                        from_address: MAIL_ACCOUNT.to_owned(),
                        to: "bob@example.test".to_owned(),
                        body: "Here is the summary.".to_owned(),
                        ..SendMail::default()
                    },
                    ..BeginSend::default()
                })
                .unwrap(),
            ),
        )
        .await
        .unwrap()
    {
        TurnMail::Done(value) => serde_json::from_value::<StartedSend>(value).unwrap(),
        TurnMail::Asks(_) => panic!("begin_send stages without asking"),
    };
    let asked_send = match workspaces
        .mail_for_turn(
            &sub_turn,
            &invoke_request(
                ACCOUNTS_PROVIDER,
                ACCOUNTS_RESOURCE,
                person_accounts::SEND,
                json!({"send": begun.send}),
            ),
        )
        .await
        .unwrap()
    {
        TurnMail::Asks(asked) => asked,
        TurnMail::Done(_) => panic!("mail send must ask person before leaving"),
    };
    assert_eq!(asked_send.named_agent.as_deref(), Some(ROOT_AGENT));

    // Revoking the root assistant immediately cuts off the sub-agent.
    let revoked_val = invoke_as_owner(
        &service,
        workspace_id,
        &owner,
        invoke_request(
            ACCOUNTS_PROVIDER,
            ACCOUNTS_RESOURCE,
            person_accounts::ASSISTANT,
            json!({
                "account": MAIL_ACCOUNT,
                "agent": ROOT_AGENT,
                "may": false
            }),
        ),
    )
    .await
    .unwrap();
    let revoked: AccountAssistants = serde_json::from_value(revoked_val).unwrap();
    assert!(revoked.agents.is_empty());

    assert!(workspaces.mail_offered(&sub_turn).await.is_empty());
    assert_eq!(
        workspaces
            .mail_for_turn(
                &sub_turn,
                &invoke_request(
                    ACCOUNTS_PROVIDER,
                    ACCOUNTS_RESOURCE,
                    person_accounts::LIST,
                    json!({}),
                ),
            )
            .await,
        Err(ActionCallError::UnknownAction)
    );

    // Calendar inheritance & immediate revocation (compiled when `agents` is enabled).
    #[cfg(feature = "agents")]
    {
        use tilo_workspace_service::workspaces::hosted_use::{CALENDAR_ENTRIES, CALENDARS};

        const CAL_ACCOUNT: &str = "cal-work-ada";
        let cal_id = accounts::AccountId::from(CAL_ACCOUNT.to_owned());
        let mut cal_account =
            accounts::Account::new(cal_id, accounts::AccountTypeId::from("caldav"));
        cal_account.display_name = "Work Calendar".to_owned();
        cal_account.config = json!({
            "server_url": "https://calendar.example.test/dav/",
            "username": "ada",
            "calendars": [{
                "id": "https://calendar.example.test/dav/calendars/ada/work/",
                "name": "Work",
                "color": 0,
                "visible": true,
                "read_only": false
            }],
            "read_only": false
        });
        let mut cal_store = accounts::AccountStore::load_in(&root_dir);
        cal_store.upsert(cal_account);
        cal_store.save_in(&root_dir).unwrap();

        invoke_as_owner(
            &service,
            workspace_id,
            &owner,
            invoke_request(
                ACCOUNTS_PROVIDER,
                ACCOUNTS_RESOURCE,
                person_accounts::ASSISTANT,
                json!({
                    "account": CAL_ACCOUNT,
                    "agent": ROOT_AGENT,
                    "may": true
                }),
            ),
        )
        .await
        .unwrap();

        assert!(workspaces.calendars_offered(&base_turn).await.is_empty());
        let cal_offered: Vec<String> = workspaces
            .calendars_offered(&sub_turn)
            .await
            .into_iter()
            .filter_map(|row| row.action.map(|action| action.address.action))
            .collect();
        assert!(cal_offered.iter().any(|action| action == "list"));
        assert!(cal_offered.iter().any(|action| action == CALENDAR_ENTRIES));

        let cal_list = workspaces
            .calendars_for_turn(
                &sub_turn,
                &invoke_request(ACCOUNTS_PROVIDER, CALENDARS, "list", json!({})),
            )
            .await
            .unwrap();
        assert_eq!(cal_list["accounts"][0]["id"], CAL_ACCOUNT);

        invoke_as_owner(
            &service,
            workspace_id,
            &owner,
            invoke_request(
                ACCOUNTS_PROVIDER,
                ACCOUNTS_RESOURCE,
                person_accounts::ASSISTANT,
                json!({
                    "account": CAL_ACCOUNT,
                    "agent": ROOT_AGENT,
                    "may": false
                }),
            ),
        )
        .await
        .unwrap();

        assert!(workspaces.calendars_offered(&sub_turn).await.is_empty());
        assert_eq!(
            workspaces
                .calendars_for_turn(
                    &sub_turn,
                    &invoke_request(ACCOUNTS_PROVIDER, CALENDARS, "list", json!({})),
                )
                .await,
            Err(ActionCallError::UnknownAction)
        );
    }

    service.shutdown().await;
}
