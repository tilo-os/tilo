//! A member's own agent run by the server that keeps the workspace: the server asks with a ticket
//! to run Bea's agent, the agent shows it while it waits, and once Bea redeems it with her root
//! the server takes its turns while none of her devices does, signing as the agent with a key the
//! server made and her root certified. It stops once she revokes that key.
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

use collab_types::store::{Cell, Query, Value, wire::Command};
use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionTarget, TestProvider, WORKSPACE_AGENT_CREATE,
    WORKSPACE_AGENT_UPDATE,
};
use tilo_agent_session::model::PlanTurns;
use tilo_resource::Id;
use tilo_resource_store::Stamp;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason};
use tilo_workspace_agent::{Agent, OWNER, WorkspaceAgents};

use super::own_agent_tests::{Answers, PLAN_READY, SCOUT, argument, chat_writes, mentioning};
use super::{govern, record_route, settled};
use tilo_workspace_service::agents::agent_keys::AgentKeys;
use tilo_workspace_service::agents::ticketed_keys::{
    AgentKeyAsk, AgentKeyTicket, AgentKeyTickets, DeviceAgentKeys,
};
use tilo_workspace_service::agents::triggered::{CHAT, MESSAGE};
use tilo_workspace_service::agents::{Model, ServerAgents};
use tilo_workspace_service::apps::triggers::{Runner, StartRun};
use tilo_workspace_service::work::{Actor, Ineligible, NotTaken, Order, Turn};
use tilo_workspace_service::workspaces::fixtures::Started;
use tilo_workspace_service::workspaces::fixtures::invited;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, TestHost, device_cert, hosting_device, test_host_holding,
};
use tilo_workspace_service::workspaces::servers::{self, Servers};
use tilo_workspace_service::workspaces::{MemberDevice, Workspaces};

use super::mount;

/// The keys on Bea's device, which takes none of her agent's turns itself.
struct BeasDevice;

impl AgentKeys for BeasDevice {
    fn grant(&self, _: WorkspaceId, _: &str, _: &str, _: [u8; 32]) -> Result<[u8; 32], String> {
        Err("Bea's device takes no turn of her agent here".to_owned())
    }

    fn sign(&self, _: WorkspaceId, _: &[u8; 32], _: &mut InvokeRequest) -> Result<(), String> {
        Err("Bea's device signs nothing as her agent here".to_owned())
    }

    fn made(&self, _: &[u8; 32]) -> Option<(DeviceCert, SigningKey)> {
        None
    }
}

/// The tickets the server's daemon serves for agents' keys: each one asked, with where its
/// redemption's certificate goes.
#[derive(Default)]
struct Tickets(Mutex<Vec<(AgentKeyAsk, tokio::sync::watch::Sender<Option<DeviceCert>>)>>);

impl AgentKeyTickets for Tickets {
    fn ask(&self, ask: &AgentKeyAsk) -> Result<AgentKeyTicket, String> {
        let (sender, certified) = tokio::sync::watch::channel(None);
        let mut asked = self.0.lock().unwrap();
        asked.push((ask.clone(), sender));
        Ok(AgentKeyTicket {
            ticket: format!("tilo1agentticket{}", asked.len()),
            certified,
        })
    }
}

impl Tickets {
    /// How many tickets were asked for.
    fn asked(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// Redeem the last ticket asked for as a device holding `root` does: it reads what the ticket
    /// waits for, and the root signs that key's certificate for the agent it names.
    fn redeem(&self, root: &SigningKey) -> DeviceCert {
        let asked = self.0.lock().unwrap();
        let (ask, sender) = asked.last().expect("a ticket was asked for");
        let tab = tilo_device_identity::login::Tab {
            device_public_key: hex::encode(ask.key),
            label: ask.label.clone(),
            key_for: Some(tilo_device_identity::login::KeyFor::Agent(
                tilo_device_identity::login::AgentAsked {
                    workspace: ask.workspace.to_string(),
                    agent: ask.agent.clone(),
                },
            )),
        };
        let mut cert = tab
            .certificate(root.verifying_key().to_bytes(), tilo_clock::unix_now_ns())
            .unwrap();
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        sender.send(Some(cert.clone())).unwrap();
        cert
    }
}

/// `host`'s person's entry for it among their servers, offering compute: it keeps their
/// workspaces always on, and runs every member's shortcuts there.
async fn offers_compute(host: &TestHost, device: &DeviceCert) {
    let workspaces = host.workspaces();
    let admin = person_workspace_id(&device.user_public_key);
    workspaces
        .create_workspace("Servers", Some(&admin))
        .await
        .unwrap();
    servers::apply(
        workspaces,
        Servers::ENROLL_SERVER.with((
            hex::encode(device.device_public_key),
            "host".into(),
            serde_json::to_string(&host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    servers::apply(
        workspaces,
        Servers::SET_APPETITE.with((
            Id::from_raw(hex::encode(device.device_public_key)),
            0,
            true,
            0,
        )),
    )
    .await
    .unwrap();
}

/// `agent` as `workspaces` reads it in `workspace`, once `ready` holds of it.
async fn agent_once(
    workspaces: &Workspaces,
    workspace: &str,
    agent: &str,
    what: &str,
    ready: impl Fn(&Agent) -> bool,
) -> Agent {
    let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let found = workspaces
            .read_resource::<WorkspaceAgents>(workspace, (OWNER, record.as_str()))
            .await
            .ok()
            .flatten()
            .and_then(|agents| agents.agents.iter().find(|its| its.id.0 == agent).cloned());
        if let Some(found) = found.filter(|found| ready(found)) {
            return found;
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Whether `workspaces`' device takes a turn of `agent` in `workspace` now, or why not.
async fn takes_agents_turn(
    workspaces: &Workspaces,
    workspace: &str,
    agent: &str,
) -> Result<(), NotTaken> {
    let turn = Turn::once(
        format!("{CHAT}/probe#answer"),
        json!({ "probe": uuid::Uuid::new_v4().to_string() }),
    )
    .acting_as(Actor::Agent(agent.to_owned()));
    tilo_workspace_service::work::takes_turn(workspaces, workspace, &turn)
        .await
        .map(drop)
}

/// The workspace Ada keeps on her server, which offers compute; Bea, a member, in it from her
/// own device, with Scout, the agent she made there.
struct Hosted {
    ada: TestHost,
    ada_cert: DeviceCert,
    bea: TestHost,
    bea_cert: DeviceCert,
    bea_root: SigningKey,
    workspace: String,
    _state: (tempfile::TempDir, tempfile::TempDir),
}

async fn hosted() -> Hosted {
    super::set_surfaces();
    let (ada_state, bea_state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let keys = Arc::new(TestGroupKeys::default());
    let bea_root = SigningKey::from_bytes(&[0xbe; 32]);
    let (bea_key, bea_cert) = device_cert(&bea_root, "bea-laptop", DeviceCapabilities::member());
    let (ada_key, ada_cert) = hosting_device();
    let ada = test_host_holding(ada_state.path(), &ada_key, &ada_cert, Arc::clone(&keys)).await;
    offers_compute(&ada, &ada_cert).await;
    let (ada, (bea_key, bea_cert), created, invitation) =
        invited(ada, &ada_cert, "Team", 0x62, (bea_key, bea_cert)).await;
    let workspace = created.workspace;
    let bea = test_host_holding(bea_state.path(), &bea_key, &bea_cert, keys).await;
    bea.workspaces().set_member_device(Arc::new(MemberDevice {
        cert: bea_cert.clone(),
        signer: Arc::new(bea_key.clone()),
    }));
    bea.workspaces().join_member(&invitation).await.unwrap();
    let made = govern(
        bea.workspaces(),
        &workspace,
        &bea_cert,
        WORKSPACE_AGENT_CREATE,
        json!({ "name": "Scout", "id": SCOUT }),
    )
    .await
    .expect("Bea makes her own agent");
    assert_eq!(made["agent"], SCOUT, "{made}");
    // Her device holds her root, and runs an engine that takes none of her agent's turns.
    let idle = Arc::new(TestProvider::new());
    let _ = ServerAgents::on_device(bea.workspaces().clone(), idle, Arc::new(BeasDevice));
    Hosted {
        ada,
        ada_cert,
        bea,
        bea_cert,
        bea_root,
        workspace,
        _state: (ada_state, bea_state),
    }
}

/// The device's dispatch on `workspaces`, as it hands a call the workspace's grants admit to the
/// provider registered for it here, and completes any other.
fn dispatch_of(workspaces: &Workspaces) -> Arc<TestProvider> {
    let providers = workspaces.providers();
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
}

/// The engine of Ada's server, as its daemon starts it: the keys of a device whose identity store
/// holds no root, asking with the tickets its daemon serves, running in the engine's own loop.
fn server_engine(
    adas: &Workspaces,
    dispatch: Arc<TestProvider>,
) -> (Arc<DeviceAgentKeys>, Arc<Tickets>) {
    let keys = Arc::new(DeviceAgentKeys::of(adas, None));
    let tickets = Arc::new(Tickets::default());
    let model: Model = Arc::new(|| Some(Box::new(Answers) as Box<dyn PlanTurns>));
    let agents = ServerAgents::on_device(
        adas.clone(),
        dispatch,
        Arc::clone(&keys) as Arc<dyn AgentKeys>,
    )
    .scripted(model)
    .asking_with(
        Arc::clone(&keys),
        Arc::clone(&tickets) as Arc<dyn AgentKeyTickets>,
    );
    tokio::spawn(agents.run());
    (keys, tickets)
}

/// Bea's root revokes `key`, announced from her device as every device announces a revocation.
async fn revoke(hosted: &Hosted, key: [u8; 32]) {
    let mut entry = RevocationEntry {
        user_public_key: hosted.bea_root.verifying_key().to_bytes(),
        revoked_device_public_key: key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &hosted.bea_root).unwrap();
    hosted
        .bea
        .workspaces()
        .announce_revocations(vec![entry])
        .await
        .unwrap();
}

/// Ada keeps the workspace on her server, which asks with a ticket to run Scout, Bea's own agent;
/// Scout shows the ticket while it waits, and Bea redeems it with her root. While none of her
/// devices takes its turns, she mentions it in Chat: the server's runner fires her trigger, its
/// engine takes the turn, and Scout answers in that conversation through Chat's `send`, as Scout
/// acting for Bea, every call signed by the key the server made for Scout and Bea's root
/// certified. Once her root revokes that key, the server signs as Scout no more, and asks no more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_that_keeps_the_workspace_runs_a_members_agent_she_approved() {
    let hosted = hosted().await;
    let (adas, beas) = (hosted.ada.workspaces(), hosted.bea.workspaces());
    let workspace = hosted.workspace.clone();
    let workspace_id = WorkspaceId::parse(&workspace).unwrap();
    let bea_person = hex::encode(hosted.bea_cert.user_public_key);
    let ada_device = hex::encode(hosted.ada_cert.device_public_key);

    // The server's engine asks with a ticket to run Scout, naming a key of its own, and Scout
    // shows that ticket on Bea's device while it waits for her approval.
    let dispatch = dispatch_of(adas);
    let (keys, tickets) = server_engine(adas, dispatch.clone());
    let scout = agent_once(
        beas,
        &workspace,
        SCOUT,
        "Scout never showed the server's ticket",
        |agent| agent.runs_on.iter().any(|runs| runs.device == ada_device),
    )
    .await;
    let runs = &scout.runs_on[0];
    assert!(runs.waits_for_approval(), "{runs:?}");
    assert_eq!(runs.ticket.as_deref(), Some("tilo1agentticket1"));
    assert_eq!(tickets.asked(), 1, "one ticket asks for Scout");

    // Not approved, the server does not act as Scout.
    assert_eq!(
        takes_agents_turn(adas, &workspace, SCOUT).await.err(),
        Some(NotTaken::Ineligible(Ineligible::CannotActAs)),
        "the server takes no turn of an agent nobody approved it for"
    );

    // Bea redeems the ticket on her device, which holds her root.
    let approved = tickets.redeem(&hosted.bea_root);
    let key = approved.device_public_key;
    agent_once(
        adas,
        &workspace,
        SCOUT,
        "the server never kept the certificate",
        |agent| {
            agent
                .runs_on
                .iter()
                .any(|runs| runs.device == ada_device && runs.certificate.is_some())
        },
    )
    .await;
    assert!(
        takes_agents_turn(adas, &workspace, SCOUT).await.is_ok(),
        "the server takes Scout's turn once Bea approved it"
    );

    // Chat answers for its record on both devices, each opening it as its own person. Bea makes
    // a channel and mentions Scout there; her own device starts nothing.
    let ada_person = hex::encode(hosted.ada_cert.user_public_key);
    for (host, person) in [(&hosted.ada, ada_person), (&hosted.bea, bea_person.clone())] {
        let workspaces = host.workspaces();
        mount(workspaces);
        // Chat's types are declared so a write to its record is judged by them.
        tilo_workspace_service::workspaces::fixtures::declare_built_in_app(host, CHAT);
        let chat = tilolang_workspace::records::built_in(CHAT)
            .unwrap()
            .expect("Chat is built in");
        let as_person = tilo_workspace_service::tl_records::member(tilo_actions::MemberId::person(
            person.clone(),
        ));
        tilolang_workspace::records::with_tl_record(
            workspaces,
            &workspace,
            &chat,
            (as_person, &person),
            |opened| opened.open_root().map(drop),
        )
        .await
        .expect("Chat opens");
    }
    let route = record_route(&workspace, CHAT);
    let _ = beas.own_route_keys(&route, true).await;
    let chat_provider = format!("{}:{CHAT}:", tilo_actions::remote::RESOURCE_PROVIDER);
    let caller_bea = Caller::authenticated(
        hosted.bea_cert.user_public_key,
        hosted.bea_cert.device_public_key,
    )
    .in_workspace(&workspace);
    let new_channel = beas
        .record_rows(&workspace, &route, &bea_person)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| row.action)
        .find(|action| action.address.action == "new_channel")
        .expect("Chat offers Bea a new channel");
    let made = settled(
        beas.providers()
            .get(&chat_provider)
            .unwrap()
            .answers
            .invoke(
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
            ),
    )
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

    // The server runs every member's shortcuts: Bea's trigger for Scout arms there. The save
    // reaches the server's copy of the workspace through the link, and its runner re-arms when
    // that copy changes; this one arms once, so wait for the trigger to be there first.
    let arming = std::time::Instant::now() + tilo_test_env::hang_bound();
    while !adas.described().await.iter().any(|workspace| {
        workspace.shortcuts.iter().any(|saved| {
            saved.id() == tilo_workspace_service::agents::triggered::shortcut_id(SCOUT)
        })
    }) {
        assert!(
            std::time::Instant::now() < arming,
            "the server never held Bea's trigger"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        adas.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order {
            server_grace: std::time::Duration::ZERO,
            other_grace: std::time::Duration::ZERO,
        },
    );
    let mut writes = adas.document_writes();
    // A shortcut is armed by following its save, as the daemon does: Bea's trigger reaches the
    // server's copy of the workspace after the runner starts, and arms it when it does.
    let mut governance = adas.governance_changes();
    let mut changed = true;
    let bea_who = tilo_actions::Acting::person(bea_person.clone()).by;
    chat_writes(
        beas,
        &workspace,
        &caller_bea,
        &bea_who,
        (MESSAGE, "m-1", Some(general.as_str())),
        Vec::new(),
        Some(mentioning("Scout, what is planned?", (SCOUT, &bea_person))),
    )
    .await
    .unwrap();

    // Her mention reaches the server, which fires her trigger and takes the firing.
    let deadline = tilo_test_env::hang_deadline();
    let (mut seen_writes, mut announced) = (0_usize, 0_usize);
    let firing = loop {
        let mut since = Vec::new();
        while let Ok(write) = writes.try_recv() {
            since.push(write);
        }
        seen_writes += since.len();
        while governance.try_recv().is_ok() {
            changed = true;
            announced += 1;
        }
        if std::mem::take(&mut changed) {
            runner.arm().await;
        }
        runner.pass(since).await;
        if let Some(firing) = started.0.lock().unwrap().first().cloned() {
            break firing;
        }
        if std::time::Instant::now() >= deadline {
            // A test asserting the firing says what the server held instead: the agent, the
            // shortcut, and what the shortcut's proof names.
            let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
            let roster = adas
                .read_resource::<WorkspaceAgents>(&workspace, (OWNER, record.as_str()))
                .await
                .ok()
                .flatten();
            let saved: Vec<String> = adas
                .described()
                .await
                .iter()
                .flat_map(|held| held.shortcuts.iter().map(|saved| saved.id().to_owned()))
                .collect();
            let hers: Vec<String> = beas
                .described()
                .await
                .iter()
                .flat_map(|held| held.shortcuts.iter().map(|saved| saved.id().to_owned()))
                .collect();
            panic!(
                "the server never fired Bea's trigger: agent {:?}, the server's shortcuts \
                 {saved:?}, Bea's {hers:?}, writes read {since_len}, changes announced \
                 {announced}",
                roster.map(|roster| roster.agents.len()),
                since_len = seen_writes,
                announced = announced,
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(firing.saver, bea_person, "the trigger is Bea's");

    // The firing's run asks the agent's answer of the server's engine, which takes the turn.
    let event = serde_json::from_value(firing.inputs[tilo_automation::EVENT_INPUT].clone())
        .expect("the firing carries the change");
    let engine = adas.agent_keys().expect("the server runs its engine");
    let place = tilo_workspace_service::agents::triggered::Where::of(&event)
        .expect("the change is a message");
    let taken = settled(engine.answer(
        workspace.clone(),
        SCOUT.to_owned(),
        place,
        event,
        firing.saver.clone(),
        (tilo_actions::InvocationId::new(), Arc::default()),
    ))
    .await
    .expect("the turn starts");
    assert!(
        matches!(taken, tilo_workspace_service::work::Started::Taken(_)),
        "the server takes Scout's turn: {taken:?}"
    );

    // Scout answers through Chat's `send` in that conversation, as Scout acting for Bea, the call
    // signed by the server's key for Scout, which Bea's root certified for Scout alone.
    let deadline = tilo_test_env::hang_deadline();
    let (request, caller) = loop {
        let asked = dispatch.asked();
        if let Some(at) = asked
            .iter()
            .position(|request| request.target.address.action == "send")
        {
            break (asked[at].clone(), dispatch.callers()[at].clone());
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Scout never answered in the conversation: {asked:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(
        request.target.address.resource,
        format!("tl:{CHAT}:{general}")
    );
    assert_eq!(
        argument(&request, "reply_to")["id"],
        json!("m-1"),
        "a reference to the message it answers"
    );
    // The reply begins empty, naming the turn, and takes the model's words as it writes them.
    let deadline = tilo_test_env::hang_deadline();
    while !dispatch.asked().iter().any(|asked| {
        matches!(asked.target.address.action.as_str(), "send" | "edit")
            && argument(asked, "body")
                .as_str()
                .is_some_and(|body| body.contains(PLAN_READY))
    }) {
        assert!(
            std::time::Instant::now() < deadline,
            "the reply never took the answer: {:?}",
            dispatch.asked()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(caller.acting().agent_id(), Some(SCOUT), "Scout's own call");
    assert_eq!(
        caller.user_public_key(),
        Some(&hosted.bea_cert.user_public_key),
        "for Bea"
    );
    let signer = tilo_server_protocol::invocation::verify_invocation(
        &workspace,
        &request,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .expect("the call is signed");
    assert_eq!(signer.device_public_key, key, "by the server's key");
    assert_eq!(
        signer.user_public_key,
        hosted.bea_root.verifying_key().to_bytes(),
        "certified by Bea's root"
    );
    let scope = signer.capabilities.agent.expect("an agent's grant");
    assert!(scope.is_agent(SCOUT), "for Scout alone");
    assert_eq!(scope.workspace(), Some(*workspace_id.as_uuid().as_bytes()));

    // Its reply is Chat's own write on the server, stamped as Scout's `send` for Bea.
    let context = tilo_workspace_service::apps::services::ResourceRequestContext::authenticated(
        CHAT, &workspace, caller_bea,
    );
    let deadline = tilo_test_env::hang_deadline();
    let (reply_id, reply) = loop {
        let page = adas
            .resource_command(&context, Command::Query(Query::of(MESSAGE, 50)))
            .await
            .unwrap();
        let collab_types::store::wire::Reply::Page(page) = page else {
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
    let versions = adas
        .resource_command(
            &context,
            Command::Versions {
                kind: MESSAGE.into(),
                id: reply_id,
            },
        )
        .await
        .unwrap();
    let collab_types::store::wire::Reply::Versions(versions) = versions else {
        panic!("the reply has a history: {versions:?}");
    };
    let made = versions
        .first()
        .and_then(|version| version.stamp.clone())
        .expect("the reply names the action that made it");
    let send = tilolang_workspace::records::built_in(CHAT)
        .unwrap()
        .expect("Chat is built in")
        .action_id("send");
    assert_eq!(Some(made.verb.clone()), send, "{made:?}");
    assert_eq!(
        made.invoker,
        Stamp::by(&caller.acting().by),
        "Scout wrote it"
    );
    assert_eq!(
        made.acted_for.as_deref(),
        Some(bea_person.as_str()),
        "for Bea"
    );

    // Bea's root revokes the key, as it revokes any of her keys: the server signs as Scout no
    // more, its open connections end, and it asks for that key no more.
    let ticketed = Arc::clone(&keys) as Arc<dyn AgentKeys>;
    assert_eq!(
        ticketed.revocation(&hosted.bea_cert.user_public_key, &key),
        Ok(None),
        "the key acts"
    );
    revoke(&hosted, key).await;
    let deadline = tilo_test_env::hang_deadline();
    while !adas
        .revocations()
        .is_revoked(&hosted.bea_cert.user_public_key, &key)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the server never took Bea's revocation"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        ticketed
            .revocation(&hosted.bea_cert.user_public_key, &key)
            .unwrap()
            .is_some(),
        "a connection the agent has open ends"
    );
    let mut probe = request.clone();
    assert!(
        ticketed.sign(workspace_id, &key, &mut probe).is_err(),
        "the server signs nothing more with the revoked key"
    );
    assert_eq!(
        takes_agents_turn(adas, &workspace, SCOUT).await.err(),
        Some(NotTaken::Ineligible(Ineligible::CannotActAs)),
        "the server takes no turn of Scout once Bea revoked its key"
    );
    assert_eq!(tickets.asked(), 1, "no ticket asks for a revoked key again");
    hosted.bea.shutdown().await;
    hosted.ada.shutdown().await;
}

/// A ticket certifies nothing until the agent's own person redeems it: a certificate Ada's root
/// signs for the server's key to run Bea's agent is not taken, no change of an agent writes where
/// it runs past the device that runs it, and the server acts as Scout no more for either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_agents_own_person_approves_a_device_for_it() {
    let hosted = hosted().await;
    let (adas, beas) = (hosted.ada.workspaces(), hosted.bea.workspaces());
    let workspace = hosted.workspace.clone();
    let ada_device = hex::encode(hosted.ada_cert.device_public_key);
    let (_keys, tickets) = server_engine(adas, dispatch_of(adas));
    agent_once(
        adas,
        &workspace,
        SCOUT,
        "Scout never showed the server's ticket",
        |agent| agent.runs_on.iter().any(|runs| runs.device == ada_device),
    )
    .await;

    // Another person's root redeems it: the certificate is not Bea's grant, and nothing keeps it.
    let ada_root = SigningKey::from_bytes(&[0xad; 32]);
    tickets.redeem(&ada_root);
    // Nor does a change of the agent say where it runs.
    for (action, arguments) in [
        (
            WorkspaceAgents::RUN_AGENT_ON.id,
            json!({ "agent": SCOUT, "device": ada_device, "certificate": "00" }),
        ),
        (
            WorkspaceAgents::WAIT_TO_RUN_AGENT.id,
            json!({ "agent": SCOUT, "device": ada_device, "ticket": "tilo1other" }),
        ),
    ] {
        let refused = govern(
            adas,
            &workspace,
            &hosted.ada_cert,
            WORKSPACE_AGENT_UPDATE,
            json!({ "action": action, "arguments": arguments }),
        )
        .await
        .expect_err("no change of an agent says where it runs");
        assert!(
            matches!(&refused, ActionCallError::InvalidRequest { .. }),
            "{refused:?}"
        );
    }
    assert_eq!(
        takes_agents_turn(adas, &workspace, SCOUT).await.err(),
        Some(NotTaken::Ineligible(Ineligible::CannotActAs)),
        "the server still takes no turn of Scout"
    );
    let scout = agent_once(beas, &workspace, SCOUT, "Bea reads Scout", |_| true).await;
    assert!(
        scout.runs_on.iter().all(|runs| runs.certificate.is_none()),
        "nobody approved any device for Scout: {:?}",
        scout.runs_on
    );
    hosted.bea.shutdown().await;
    hosted.ada.shutdown().await;
}

/// An agent the server itself adds is answered by its engine without a restart: what the host
/// writes reaches whoever follows the record, the engine included.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_the_server_adds_is_answered_without_a_restart() {
    let hosted = hosted().await;
    let adas = hosted.ada.workspaces();
    let workspace = hosted.workspace.clone();
    let _engine = server_engine(adas, dispatch_of(adas));
    let engine = adas.agent_keys().expect("the server runs its engine");
    let person = hex::encode(hosted.ada_cert.user_public_key);
    adas.author_agents(&workspace, |doc| {
        let mut made = tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
            .in_app("so.tilo.model", "Model")
            .as_author(&person)
            .in_app(tilo_workspace_agent::OWNER, "Workspace");
        made.apply(WorkspaceAgents::CREATE_AGENT.with(
            tilo_workspace_agent::workspace_agents::CreateAgent {
                name: tilo_resource::NonBlank::parse("Herald").unwrap(),
                id: Some("herald".into()),
                model: None,
                mcp_client: None,
                avatar: None,
            },
        ))
        .map_err(|error| error.to_string())?;
        std::mem::swap(doc, made.doc_mut());
        Ok(())
    })
    .await
    .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while !engine
        .answering()
        .iter()
        .any(|(agent, _)| agent == "herald")
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the engine never answered the agent the server added: {:?}",
            engine.answering()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    hosted.bea.shutdown().await;
    hosted.ada.shutdown().await;
}
