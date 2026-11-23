//! A member's own agent: a person who does not keep the workspace makes an agent of their own in
//! it, mentions it in a tilolang Chat channel, and it answers there, in that conversation, through
//! Chat's own `send`, as the agent acting for them and bounded by what they may do.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "one scenario, end to end, reads as one test; a fixture failing is its test failing"
)]

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use serde_json::{Value as Json, json};
use tilo_action_policy::Actor as Who;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionTarget, TestProvider, WORKSPACE_AGENT_CREATE,
};
use tilo_agent_session::model::PlanTurns;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, Usage,
};
use tilo_resource_store::Query;
use tilo_resource_store::wire::{Command, Edit};
use tilo_resource_store::{Cell, Resource, Stamp, Value};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use tilo_workspace_agent::{OWNER, WorkspaceAgents};

use super::super::fixtures::{TestGroupKeys, device_cert, governor, test_host_holding};
use super::super::{MemberDevice, Workspaces};
use super::tests::invited;
use crate::agents::agent_keys::AgentKeys;
use crate::agents::triggered::{self, CHAT, MESSAGE};
use crate::agents::{Model, ServerAgents};
use crate::apps::triggers::{Firing, Runner, StartRun};
use crate::service::local_caller;
use crate::work::Order;

/// Bea's agent.
const SCOUT: &str = "scout";
/// What Bea's agent answers, as its model says it.
const PLAN_READY: &str = "the plan is ready";

/// The firings Bea's device was asked to start.
#[derive(Default)]
struct Started(Mutex<Vec<Firing>>);

impl StartRun for Started {
    fn start(&self, firing: &Firing) -> Result<u64, String> {
        let mut started = self.0.lock().unwrap();
        started.push(firing.clone());
        Ok(u64::try_from(started.len()).unwrap())
    }
}

/// A model that answers at once, in words.
struct Answers;

impl PlanTurns for Answers {
    fn turn(
        &mut self,
        _: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::Text {
                    text: format!("Bea, {PLAN_READY}."),
                }],
            },
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            cost_micros: 0,
        })
    }

    fn model(&self) -> &'static str {
        "scripted"
    }
}

/// The key Bea's device makes for her agent: one fixed key here, which the dispatch standing in
/// for her device's takes as the agent's own.
struct Keys;

impl AgentKeys for Keys {
    fn grant(
        &self,
        _: tilo_server_protocol::WorkspaceId,
        _: &str,
        _: &str,
        _: [u8; 32],
    ) -> Result<[u8; 32], String> {
        Ok([9; 32])
    }

    fn sign(
        &self,
        _: tilo_server_protocol::WorkspaceId,
        _: &[u8; 32],
        _: &mut InvokeRequest,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// `operation` of `workspace`'s agents with `arguments`, asked by `device` over `workspaces`'
/// dispatch, as every caller asks it.
async fn ask_agents(
    workspaces: &Workspaces,
    workspace: &str,
    device: &DeviceCert,
    operation: &str,
    arguments: Json,
) -> Result<Json, ActionCallError> {
    let Json::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, operation),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let caller = local_caller(device).in_workspace(workspace);
    let receipt = settled(workspaces.device_actions().invoke(&caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Json::Null)),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("an agent's change completes at once: {other:?}"),
    }
}

/// Chat's write of `id`, a `kind` in `parent`, as the app writes it for the invocation of `by`
/// that the dispatch delivered to it, through `caller`'s device.
async fn chat_writes(
    workspaces: &Workspaces,
    workspace: &str,
    caller: &Caller,
    by: &Who,
    (kind, id, parent): (&str, &str, Option<&str>),
    fields: Vec<(&str, String)>,
    body: Option<Json>,
) -> Result<(), String> {
    let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
    let stamp = Stamp {
        verb: "send".into(),
        invoker: Stamp::by(by),
        invocation: invocation.0.to_string(),
        at: jiff::Timestamp::now().to_string(),
        ..Stamp::default()
    };
    let mut commands = vec![Command::Put {
        kind: kind.into(),
        id: id.into(),
        resource: Resource {
            readers: "*".into(),
            parent: parent.map(str::to_owned),
            order: None,
            fields: fields
                .into_iter()
                .map(|(name, value)| (name.to_owned(), Cell::One(Value::Text(value))))
                .collect(),
        },
        stamp: stamp.clone(),
    }];
    if let Some(body) = body {
        commands.push(Command::Edit {
            kind: kind.into(),
            id: id.into(),
            edits: vec![Edit::Set {
                field: "body".into(),
                value: body,
            }],
            stamp,
        });
    }
    let context = crate::apps::services::ResourceRequestContext::authenticated(
        CHAT,
        workspace,
        caller.clone(),
    )
    .serving(vec![(invocation, by.clone())]);
    workspaces
        .resource_command(&context, Command::Batch(commands))
        .await
        .map(drop)
}

/// What a pending answer comes to, on this runtime.
async fn settled<T: Send + 'static>(pending: Pending<T>) -> T {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pending.then(move |value| {
        let _ = sender.send(value);
    });
    tilo_test_env::within("the dispatch's answer", receiver)
        .await
        .unwrap()
        .expect("answered")
}

/// Rich text whose words are `words`, mentioning `agent`.
fn mentioning(words: &str, agent: &str) -> Json {
    let named = json!({ "agent": agent }).to_string();
    json!({"blocks": [{"text": words, "marks": [
        {"start": 0, "end": 5, "name": "mention", "value": {"Str": named}}
    ]}]})
}

/// The text a JSON argument `name` of `request` holds.
fn argument(request: &InvokeRequest, name: &str) -> Json {
    match &request.argument {
        InvokeArgument::Json(arguments) => arguments.get(name).cloned().unwrap_or(Json::Null),
        other => panic!("the call carries JSON arguments: {other:?}"),
    }
}

/// Ada keeps the workspace on her server; Bea, a member, makes an agent of her own there from her
/// own device, and it acts for her. She mentions it in a Chat channel: her trigger fires on her
/// device, and the turn answers in that conversation through Chat's `send`, as her agent acting
/// for her, its reply a message of the channel, to hers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_makes_their_own_agent_and_it_answers_their_mention() {
    let (ada_state, bea_state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let keys = Arc::new(TestGroupKeys::default());
    let bea_root = SigningKey::from_bytes(&[0xbe; 32]);
    let (bea_key, bea_cert) = device_cert(&bea_root, "bea-laptop", DeviceCapabilities::member());
    let (ada_key, ada_cert) = governor();
    let ada = test_host_holding(ada_state.path(), &ada_key, &ada_cert, Arc::clone(&keys)).await;
    let (ada, (bea_key, bea_cert), created, invitation) =
        invited(ada, &ada_cert, "Team", 0x61, (bea_key, bea_cert)).await;
    let workspace = created.workspace;
    let bea = test_host_holding(bea_state.path(), &bea_key, &bea_cert, keys).await;
    bea.workspaces().set_member_device(Arc::new(MemberDevice {
        cert: bea_cert.clone(),
        signer: Arc::new(bea_key.clone()),
    }));
    bea.workspaces().join_member(&invitation).await.unwrap();
    let beas = bea.workspaces();
    let bea_person = hex::encode(bea_cert.user_public_key);

    // Bea makes an agent of her own, from her own device, over its dispatch.
    let made = ask_agents(
        beas,
        &workspace,
        &bea_cert,
        WORKSPACE_AGENT_CREATE,
        json!({ "name": "Scout", "id": SCOUT }),
    )
    .await
    .expect("Bea makes her own agent");
    assert_eq!(made["agent"], SCOUT, "{made}");
    let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let roster = beas
        .read_resource::<WorkspaceAgents>(&workspace, (OWNER, record.as_str()))
        .await
        .unwrap()
        .expect("the workspace lists its agents");
    let scout = roster
        .agents
        .iter()
        .find(|agent| agent.id.0 == SCOUT)
        .expect("Scout is listed");
    assert_eq!(
        tilo_action_policy::PersonId::parse(&scout.acts_for)
            .ok()
            .and_then(|person| person.root()),
        Some(bea_cert.user_public_key),
        "Scout acts for Bea, who made it"
    );

    // Her trigger for it is saved as hers, and arms on her device.
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        beas.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    let mut writes = beas.document_writes();
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let _ = beas.own_route_keys(&route, true).await;
    runner.arm().await;

    // Chat, built into this build, answers for its record on Bea's device, as the daemon serves
    // it: its own package runs each action over the device's store. Bea opens it and makes a
    // channel with it.
    crate::tl_records::provided::register(beas);
    let chat = crate::tl_records::built_in(CHAT)
        .unwrap()
        .expect("Chat is built in");
    let as_bea = crate::tl_records::member(tilo_actions::MemberId::person(bea_person.clone()));
    beas.with_tl_record(&workspace, &chat, as_bea, |opened| {
        opened.open_root().map(drop)
    })
    .await
    .expect("Bea opens Chat");
    let chat_provider = format!("{}:{CHAT}:", tilo_actions::remote::RESOURCE_PROVIDER);
    let chat_answers = beas
        .providers()
        .get(&chat_provider)
        .expect("Chat answers on Bea's device")
        .answers;
    let caller_bea = Caller::authenticated(bea_cert.user_public_key, bea_cert.device_public_key)
        .in_workspace(&workspace);
    let new_channel = beas
        .record_rows(&workspace, &route, &bea_person)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| row.action)
        .find(|action| action.address.action == "new_channel")
        .expect("Chat offers Bea a new channel");
    let made = settled(chat_answers.invoke(
        &caller_bea,
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: chat_provider.clone(),
                address: new_channel.address,
            },
            argument: InvokeArgument::Json(
                json!({ "name": "general" }).as_object().unwrap().clone(),
            ),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        },
    ))
    .await
    .expect("Chat makes the channel");
    let ReceiptStatus::Completed { output } = made.status else {
        panic!("making a channel completes at once: {made:?}");
    };
    let general = output
        .resources
        .first()
        .and_then(|made| made.strip_prefix(&format!("tl:{CHAT}:")))
        .expect("the channel Chat made")
        .to_owned();

    // Bea mentions her agent in the channel, as Chat writes her `send`.
    let bea_who = tilo_actions::Acting::person(bea_person.clone()).by;
    chat_writes(
        beas,
        &workspace,
        &caller_bea,
        &bea_who,
        (MESSAGE, "m-1", Some(general.as_str())),
        Vec::new(),
        Some(mentioning("Scout, what is planned?", SCOUT)),
    )
    .await
    .unwrap();
    let mut since = Vec::new();
    while let Ok(write) = writes.try_recv() {
        since.push(write);
    }
    runner.pass(since).await;
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(
        firings.len(),
        1,
        "her mention fires her trigger once: {firings:?}"
    );
    let firing = &firings[0];
    assert_eq!(firing.shortcut, triggered::shortcut_id(SCOUT));
    assert_eq!(firing.saver, bea_person, "the trigger is Bea's");

    // The firing starts the agent's answer on her device; the engine there takes the turn, and
    // its tools go through the device's dispatch, which hands a call the workspace's grants
    // admit to the provider that answers it here: Chat's `send` is carried out by Chat.
    let dispatch = {
        let providers = beas.providers();
        Arc::new(TestProvider::new().answering(move |caller, request| {
            match providers.get(&request.target.provider) {
                Some(registered) => registered.answers.invoke(caller, request.clone()),
                None => Pending::ready(Ok(tilo_actions::remote::InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Completed {
                        output: tilo_actions::ActionOutput::default(),
                    },
                })),
            }
        }))
    };
    let model: Model = Arc::new(|| Some(Box::new(Answers) as Box<dyn PlanTurns>));
    let agents =
        ServerAgents::on_device(beas.clone(), dispatch.clone(), Arc::new(Keys)).scripted(model);
    let event = serde_json::from_value(firing.inputs[tilo_automation::EVENT_INPUT].clone())
        .expect("the firing carries the change");
    let taken = agents
        .triggered(&workspace, SCOUT, event, firing.saver.clone())
        .await
        .expect("the turn starts");
    assert_eq!(
        taken,
        crate::work::Started::Taken,
        "Bea's device takes her agent's turn"
    );

    // The reply arrives through Chat's `send` in that conversation, to her message, as Scout
    // acting for Bea.
    let deadline = tilo_test_env::hang_deadline();
    let sent = loop {
        let asked = dispatch.asked();
        let callers = dispatch.callers();
        if let Some(at) = asked
            .iter()
            .position(|request| request.target.address.action == "send")
        {
            break (asked[at].clone(), callers[at].clone());
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the agent never answered in the conversation: {asked:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let (request, caller) = sent;
    assert_eq!(
        request.target.address.resource,
        format!("tl:{CHAT}:{general}"),
        "it answers in the channel she wrote in"
    );
    assert_eq!(
        argument(&request, "reply_to"),
        json!(format!("tl:{CHAT}:m-1"))
    );
    assert!(
        argument(&request, "body")
            .as_str()
            .is_some_and(|body| body.contains(PLAN_READY)),
        "{request:?}"
    );
    assert_eq!(
        caller.acting().agent_id(),
        Some(SCOUT),
        "it is Scout's own call"
    );
    assert_eq!(
        caller.user_public_key(),
        Some(&bea_cert.user_public_key),
        "Scout acts for Bea"
    );

    // Its reply is Chat's own write: a message of the channel, to her message, stamped as Scout's
    // `send` for Bea.
    let context =
        crate::apps::services::ResourceRequestContext::authenticated(CHAT, &workspace, caller_bea);
    let deadline = tilo_test_env::hang_deadline();
    let (reply_id, reply) = loop {
        let page = beas
            .resource_command(&context, Command::Query(Query::of(MESSAGE, 50)))
            .await
            .unwrap();
        let tilo_resource_store::wire::Reply::Page(page) = page else {
            panic!("a query answers a page: {page:?}");
        };
        if let Some(reply) = page
            .resources
            .into_iter()
            .find(|(id, message)| id != "m-1" && message.parent.as_deref() == Some(&general))
        {
            break reply;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Chat never carried out the reply"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(
        reply.fields.get("reply_to"),
        Some(&Cell::One(Value::Text("m-1".to_owned()))),
        "{reply:?}"
    );
    let versions = beas
        .resource_command(
            &context,
            Command::Versions {
                kind: MESSAGE.into(),
                id: reply_id.clone(),
            },
        )
        .await
        .unwrap();
    let tilo_resource_store::wire::Reply::Versions(versions) = versions else {
        panic!("the reply has a history: {versions:?}");
    };
    let made = versions
        .first()
        .and_then(|version| version.stamp.clone())
        .expect("the reply names the action that made it");
    assert_eq!(made.verb, "send", "{made:?}");
    assert_eq!(
        made.invoker,
        Stamp::by(&caller.acting().by),
        "Scout wrote it: {made:?}"
    );
    assert_eq!(
        made.acted_for.as_deref(),
        Some(bea_person.as_str()),
        "for Bea"
    );
    bea.shutdown().await;
    ada.shutdown().await;
}
