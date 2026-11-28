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

use std::sync::Arc;
use tilo_action_policy::ActingPolicy as _;

use collab_types::store::wire::{Command, Edit};
use collab_types::store::{Cell, Query, Resource, Value};
use ed25519_dalek::SigningKey;
use serde_json::{Value as Json, json};
use tilo_actions::remote::TestProvider;
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget, WORKSPACE_AGENT_CREATE,
};
use tilo_agent_session::model::PlanTurns;
use tilo_member_types::Actor as Who;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, Usage,
};
use tilo_resource_store::Stamp;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use tilo_workspace_agent::{OWNER, WorkspaceAgents};

use tilo_app_host::agents::{Model, ServerAgents};
use tilo_app_host::test_support::Started;
use tilo_app_host::triggers::{Runner, StartRun};
use tilo_workspace_service::agent_shortcuts::{self, CHAT, MESSAGE};
use tilo_workspace_service::work::Order;
use tilo_workspace_service::workspaces::fixtures::answer_grant;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, device_cert, hosting_device, test_host_holding,
};
use tilo_workspace_service::workspaces::fixtures::{invitation, invited, membership_log_alone};
use tilo_workspace_service::workspaces::{MemberDevice, Workspaces};

use super::{govern, mount, record_route, settled};

/// Bea's agent.
pub(super) const SCOUT: &str = "scout";
/// What Bea's agent answers, as its model says it.
pub(super) const PLAN_READY: &str = "the plan is ready";

/// A model that answers at once, in words.
pub(super) struct Answers;

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

/// Chat's write of `id`, a `kind` in `parent`, as the app writes it for the invocation of `by`
/// that the dispatch delivered to it, through `caller`'s device.
pub(super) async fn chat_writes(
    workspaces: &Workspaces,
    workspace: &str,
    caller: &Caller,
    by: &Who,
    (kind, id, parent): (&str, &str, Option<&str>),
    fields: Vec<(&str, String)>,
    body: Option<Json>,
) -> Result<(), String> {
    let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
    // A change records the id Chat's declaration keeps for `send`, as its schema declares it.
    let send = tilolang_workspace::records::built_in(CHAT)
        .unwrap()
        .expect("Chat is built in")
        .action_id("send")
        .expect("Chat declares `send`");
    let stamp = Stamp {
        verb: send,
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
    let context = tilo_app_host::services::ResourceRequestContext::authenticated(
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

/// Rich text whose words are `words`, mentioning the agent `agent` of `person`.
pub(super) fn mentioning(words: &str, (agent, person): (&str, &str)) -> Json {
    let named = json!({ "agent": { "id": agent, "person": person } }).to_string();
    json!({"blocks": [{"text": words, "marks": [
        {"start": 0, "end": 5, "name": "mention", "value": {"Str": named}}
    ]}]})
}

/// The text a JSON argument `name` of `request` holds.
pub(super) fn argument(request: &InvokeRequest, name: &str) -> Json {
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
    let (ada_key, ada_cert) = hosting_device();
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
    let made = govern(
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
        tilo_member_types::PersonId::parse(&scout.acts_for)
            .ok()
            .and_then(|person| person.root()),
        Some(bea_cert.user_public_key),
        "Scout acts for Bea, who made it"
    );
    // Bea lets her agent read and write in Chat as she may: her answers for it, which the
    // workspace's policy decides its calls by, as any person answers for their own agent.
    for kind in [
        tilo_action_types::ActionKind::Read,
        tilo_action_types::ActionKind::Undoable,
    ] {
        let answer = tilo_action_policy::agent_answers::grant(
            &workspace,
            (SCOUT, bea_person.as_str()),
            (
                &tilo_action_policy::agent_answers::AnswerScope::app(CHAT),
                kind,
            ),
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(bea_person.clone()),
        );
        answer_grant(beas, &workspace, answer)
            .await
            .expect("Bea answers for her agent");
    }

    // Her trigger for it is saved as hers, and arms on her device.
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        beas.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    let mut writes = beas.document_writes();
    let route = record_route(&workspace, CHAT);
    let _ = beas.own_route_keys(&route, true).await;
    runner.arm().await;

    // Chat, built into this build, answers for its record on Bea's device, as the daemon serves
    // it: its own package runs each action over the device's store. Bea opens it and makes a
    // channel with it.
    mount(beas);
    let chat = tilolang_workspace::records::built_in(CHAT)
        .unwrap()
        .expect("Chat is built in");
    let as_bea = tilo_workspace_service::tl_records::member(tilo_actions::MemberId::person(
        bea_person.clone(),
    ));
    tilolang_workspace::records::with_tl_record(
        beas,
        &workspace,
        &chat,
        (as_bea, &bea_person),
        |opened| opened.open_root().map(drop),
    )
    .await
    .expect("Bea opens Chat");
    let chat_provider = CHAT.to_owned();
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
            asks: false,
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
        Some(mentioning("Scout, what is planned?", (SCOUT, &bea_person))),
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
    assert_eq!(firing.shortcut, agent_shortcuts::shortcut_id(SCOUT));
    assert_eq!(firing.saver, bea_person, "the trigger is Bea's");

    // The firing starts the agent's answer on her device; the engine there takes the turn, and
    // its tools go through the device's dispatch, which hands a call the workspace's grants
    // admit to the provider that answers it here: Chat's `send` is carried out by Chat.
    // What each call the turn made was answered, for a failure to say.
    let answered = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let dispatch = {
        let providers = beas.providers();
        let answered = Arc::clone(&answered);
        Arc::new(TestProvider::new().answering(move |caller, request| {
            match providers.get(&request.target.provider) {
                Some(registered) => {
                    let answered = Arc::clone(&answered);
                    let action = request.target.address.action.clone();
                    registered
                        .answers
                        .invoke(caller, request.clone())
                        .map(move |receipt| {
                            answered
                                .lock()
                                .unwrap()
                                .push(format!("{action}: {receipt:?}"));
                            receipt
                        })
                }
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
    let agents = ServerAgents::on_device(
        beas.clone(),
        dispatch.clone(),
        Arc::new(super::CertifiedKeys::of(&bea_root, &workspace, SCOUT)),
    )
    .scripted(model);
    let event = serde_json::from_value(firing.inputs[tilo_automation::EVENT_INPUT].clone())
        .expect("the firing carries the change");
    let taken = agents
        .triggered(
            &workspace,
            SCOUT,
            tilo_workspace_service::work::Where::of(&event).expect("the change is a message"),
            event,
            firing.saver.clone(),
            (tilo_actions::InvocationId::new(), Arc::default()),
        )
        .await
        .expect("the turn starts");
    assert!(
        matches!(taken, tilo_workspace_service::work::Started::Taken(_)),
        "Bea's device takes her agent's turn: {taken:?}"
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
        argument(&request, "reply_to")["id"],
        json!("m-1"),
        "it answers her message"
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
    let context = tilo_app_host::services::ResourceRequestContext::authenticated(
        CHAT, &workspace, caller_bea,
    );
    let deadline = tilo_test_env::hang_deadline();
    let (reply_id, reply) = loop {
        let page = beas
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
            "Chat never carried out the reply; the turn's calls were answered {:?}",
            answered.lock().unwrap()
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

/// The agent a rich text's first mention names, as its mark keeps the member's JSON.
fn mentioned(body: &Json) -> Option<tilo_member_types::MemberId> {
    if let Ok(member) = serde_json::from_value::<tilo_member_types::MemberId>(body.clone())
        && member.is_agent()
    {
        return Some(member);
    }
    match body {
        Json::Object(fields) => fields.values().find_map(mentioned),
        Json::Array(items) => items.iter().find_map(mentioned),
        Json::String(text) => serde_json::from_str::<Json>(text)
            .ok()
            .filter(|inner| !inner.is_string())
            .and_then(|inner| mentioned(&inner)),
        _ => None,
    }
}

/// Ada adds Bea's agent to a conversation of her own: it stays Bea's. What it names is the agent
/// and the person it is of, so it reads with Bea's reach, Bea's answers and her stop apply to it,
/// and Ada's own agent of the same id is another member, which nothing Bea says reaches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_added_to_someone_elses_conversation_stays_its_persons() {
    use tilo_action_policy::{Answer, Authority, Context, TreePath, WorkspacePolicy};
    use tilo_action_types::ActionKind;
    use tilo_member_types::{Acting, MemberId};
    let states = [(); 2].map(|()| tempfile::tempdir().unwrap());
    let keys = Arc::new(TestGroupKeys::default());
    let bea_root = SigningKey::from_bytes(&[0xbe; 32]);
    let (bea_key, bea_cert) = device_cert(&bea_root, "bea-laptop", DeviceCapabilities::member());
    let (ada_key, ada_cert) = hosting_device();
    let ada = test_host_holding(states[0].path(), &ada_key, &ada_cert, Arc::clone(&keys)).await;
    let (ada, (bea_key, bea_cert), created, invitation) =
        invited(ada, &ada_cert, "Team", 0x64, (bea_key, bea_cert)).await;
    let workspace = created.workspace;
    let bea = test_host_holding(states[1].path(), &bea_key, &bea_cert, keys).await;
    bea.workspaces().set_member_device(Arc::new(MemberDevice {
        cert: bea_cert.clone(),
        signer: Arc::new(bea_key),
    }));
    bea.workspaces().join_member(&invitation).await.unwrap();
    let (adas, beas) = (ada.workspaces(), bea.workspaces());
    // Ada writes in Chat from her own device too: the server keeps her member copy.
    adas.set_member_device(Arc::new(MemberDevice {
        cert: ada_cert.clone(),
        signer: Arc::new(ada_key.clone()),
    }));
    let (ada_person, bea_person) = (
        hex::encode(ada_cert.user_public_key),
        hex::encode(bea_cert.user_public_key),
    );
    govern(
        beas,
        &workspace,
        &bea_cert,
        WORKSPACE_AGENT_CREATE,
        json!({ "name": "Scout", "id": SCOUT }),
    )
    .await
    .expect("Bea makes her own agent");
    let (beas_scout, adas_scout) = (
        MemberId::agent(SCOUT, bea_person.as_str()),
        MemberId::agent(SCOUT, ada_person.as_str()),
    );

    // Ada writes in a conversation of hers, adding Bea's Scout to it by its mention.
    mount(adas);
    let route = record_route(&workspace, CHAT);
    let _ = adas.own_route_keys(&route, true).await;
    let chat = tilolang_workspace::records::built_in(CHAT)
        .unwrap()
        .expect("Chat is built in");
    tilolang_workspace::records::with_tl_record(
        adas,
        &workspace,
        &chat,
        (
            tilo_workspace_service::tl_records::member(MemberId::person(ada_person.clone())),
            &ada_person,
        ),
        |opened| opened.open_root().map(drop),
    )
    .await
    .expect("Ada opens Chat");
    let caller_ada = Caller::authenticated(ada_cert.user_public_key, ada_cert.device_public_key)
        .in_workspace(&workspace);
    chat_writes(
        adas,
        &workspace,
        &caller_ada,
        &Acting::person(ada_person.clone()).by,
        (MESSAGE, "m-ada", None),
        Vec::new(),
        Some(mentioning("Scout, join us", (SCOUT, &bea_person))),
    )
    .await
    .expect("Ada writes in her conversation");
    let context = tilo_app_host::services::ResourceRequestContext::authenticated(
        CHAT, &workspace, caller_ada,
    );
    let content = Command::Content {
        kind: MESSAGE.to_owned(),
        id: "m-ada".to_owned(),
    };
    let collab_types::store::wire::Reply::Content(Some(message)) =
        adas.resource_command(&context, content).await.unwrap()
    else {
        panic!("Ada's message is kept");
    };
    let added = mentioned(&message).unwrap_or_else(|| panic!("no agent is named in {message}"));
    assert_eq!(added, beas_scout, "the agent Ada added is Bea's Scout");
    assert_ne!(
        added, adas_scout,
        "Ada's own agent of that id is another member"
    );
    // What the agent `member` may do in Chat, as its turns for its own person are decided.
    let chat = TreePath::workspace(&workspace).app(CHAT);
    let cx = || Context::at_time(tilo_clock::wall());
    let decides = |policy: &WorkspacePolicy, member: &MemberId| {
        let person = member.agent_person().expect("an agent").as_str().to_owned();
        let acting = Acting::member(member.clone())
            .handed()
            .acting_for(Acting::person(person));
        let preset = acting.handed_preset(&workspace);
        [ActionKind::Read, ActionKind::Undoable].map(|kind| {
            Authority::of_acting(&acting, policy, &preset)
                .decide(&chat, kind, &cx())
                .answer
        })
    };
    let policy = beas.policy_of(&workspace).await.unwrap();
    assert_eq!(
        decides(&policy, &added)[0],
        Answer::Allow,
        "the agent Ada added reads in Chat with Bea's reach"
    );

    // Bea keeps her Scout from changing anything in Chat; Ada's answer for her own Scout of that
    // id reaches Bea's not at all.
    let unanswered = beas.policy_of(&workspace).await.unwrap();
    for (writer, person, answer) in [
        (beas, &bea_person, Answer::Deny),
        (adas, &ada_person, Answer::Allow),
    ] {
        answer_grant(
            writer,
            &workspace,
            tilo_action_policy::agent_answers::grant(
                &workspace,
                (SCOUT, person.as_str()),
                (
                    &tilo_action_policy::agent_answers::AnswerScope::app(CHAT),
                    ActionKind::Undoable,
                ),
                answer,
                tilo_action_policy::Grantee::person(person.clone()),
            ),
        )
        .await
        .expect("each person answers for their own agent");
    }
    let answered = beas.policy_of(&workspace).await.unwrap();
    assert_ne!(decides(&unanswered, &added)[1], Answer::Deny);
    assert_eq!(
        decides(&answered, &added)[1],
        Answer::Deny,
        "Bea's answer decides the agent Ada added"
    );
    let adas_policy = adas.policy_of(&workspace).await.unwrap();
    assert_ne!(
        decides(&adas_policy, &adas_scout)[1],
        Answer::Deny,
        "Ada's Scout is not held by Bea's answer"
    );

    // Bea stops her Scout: the agent in Ada's conversation stops, Ada's own does not.
    beas.answer_stop(&workspace, SCOUT)
        .await
        .expect("Bea stops her own agent");
    for policy in [
        beas.policy_of(&workspace).await.unwrap(),
        membership_log_alone(beas, &workspace).await,
    ] {
        let (id, person) = added.as_agent().expect("an agent");
        assert!(policy.stops(id, person.as_str(), &cx()), "Bea stopped it");
        assert_eq!(decides(&policy, &added), [Answer::Deny; 2]);
        assert!(
            !policy.stops(SCOUT, &ada_person, &cx()),
            "Ada's Scout is not Bea's to stop"
        );
        assert_ne!(decides(&policy, &adas_scout), [Answer::Deny; 2]);
    }

    bea.shutdown().await;
    ada.shutdown().await;
}

/// An agent is the pair of its id and the person its certificate chains to: Bea, an editor, stops
/// the agent she made and lets it go on again without administering the workspace. Cy, another
/// editor, may write a stop and an answer for an agent of the same id, but they are his own, for
/// his own agent of that id, and reach Bea's not at all; nor does what Bea answered reach his.
/// Every replica decides it alike, one holding nothing but the workspace's membership log as
/// well: nothing of it reads the workspace's record of its agents.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_its_person_answers_for_or_stops_an_agent() {
    use tilo_action_policy::{Answer, Authority, Context, TreePath, WorkspacePolicy};
    use tilo_action_types::ActionKind;
    use tilo_member_types::Acting;
    let states = [(); 3].map(|()| tempfile::tempdir().unwrap());
    let keys = Arc::new(TestGroupKeys::default());
    let bea_root = SigningKey::from_bytes(&[0xbe; 32]);
    let cy_root = SigningKey::from_bytes(&[0xc7; 32]);
    let (bea_key, bea_cert) = device_cert(&bea_root, "bea-laptop", DeviceCapabilities::member());
    let (cy_key, cy_cert) = device_cert(&cy_root, "cy-laptop", DeviceCapabilities::member());
    let (ada_key, ada_cert) = hosting_device();
    let ada = test_host_holding(states[0].path(), &ada_key, &ada_cert, Arc::clone(&keys)).await;
    let (ada, (bea_key, bea_cert), created, beas_invitation) =
        invited(ada, &ada_cert, "Team", 0x62, (bea_key, bea_cert)).await;
    let workspace = created.workspace;
    let cys_invitation = invitation(&ada, &ada_cert, &workspace, 0x63, &cy_cert).await;
    let join = |state: &std::path::Path, key: SigningKey, cert: DeviceCert| {
        let keys = Arc::clone(&keys);
        let state = state.to_owned();
        async move {
            let host = test_host_holding(&state, &key, &cert, keys).await;
            host.workspaces().set_member_device(Arc::new(MemberDevice {
                cert,
                signer: Arc::new(key),
            }));
            host
        }
    };
    let bea = join(states[1].path(), bea_key, bea_cert.clone()).await;
    let cy = join(states[2].path(), cy_key, cy_cert.clone()).await;
    bea.workspaces()
        .join_member(&beas_invitation)
        .await
        .unwrap();
    cy.workspaces().join_member(&cys_invitation).await.unwrap();
    let (beas, cys) = (bea.workspaces(), cy.workspaces());
    let (bea_person, cy_person) = (
        hex::encode(bea_cert.user_public_key),
        hex::encode(cy_cert.user_public_key),
    );

    govern(
        beas,
        &workspace,
        &bea_cert,
        WORKSPACE_AGENT_CREATE,
        json!({ "name": "Scout", "id": SCOUT }),
    )
    .await
    .expect("Bea makes her own agent");

    // What the agent `SCOUT` acting for `person` may do in Chat, by `policy`.
    let chat = TreePath::workspace(&workspace).app(CHAT);
    let cx = || Context::at_time(tilo_clock::wall());
    let decides = |policy: &WorkspacePolicy, person: &str| {
        let acting = Acting::handed_agent(SCOUT, person);
        let preset = acting.handed_preset(&workspace);
        [ActionKind::Read, ActionKind::Undoable].map(|kind| {
            Authority::of_acting(&acting, policy, &preset)
                .decide(&chat, kind, &cx())
                .answer
        })
    };
    // Bea keeps her agent from changing anything in Chat. A `SCOUT` certified by Cy's root is
    // another agent: nothing Bea answers for hers reaches it.
    let unanswered = beas.policy_of(&workspace).await.unwrap();
    answer_grant(
        beas,
        &workspace,
        tilo_action_policy::agent_answers::grant(
            &workspace,
            (SCOUT, bea_person.as_str()),
            (
                &tilo_action_policy::agent_answers::AnswerScope::app(CHAT),
                ActionKind::Undoable,
            ),
            Answer::Deny,
            tilo_action_policy::Grantee::person(bea_person.clone()),
        ),
    )
    .await
    .expect("Bea answers for her agent");
    let before = beas.policy_of(&workspace).await.unwrap();
    assert_ne!(decides(&unanswered, &bea_person)[1], Answer::Deny);
    assert_eq!(
        decides(&before, &bea_person)[1],
        Answer::Deny,
        "Bea's agent changes nothing in Chat"
    );
    assert_eq!(
        decides(&before, &cy_person),
        decides(&unanswered, &cy_person),
        "Cy's agent of that id is decided as if Bea had answered nothing"
    );
    let cys_stop = tilo_action_policy::agent_answers::stop_grant(&workspace, SCOUT, &cy_person);
    let cys_answer = tilo_action_policy::agent_answers::grant(
        &workspace,
        (SCOUT, cy_person.as_str()),
        (
            &tilo_action_policy::agent_answers::AnswerScope::app(CHAT),
            ActionKind::Undoable,
        ),
        Answer::Allow,
        tilo_action_policy::Grantee::person(cy_person.clone()),
    );
    // Bea's answer reaches Cy's device through the serving device in its own time: Cy's writes
    // are weighed against copies of his that hold it, or its arrival would read as their effect.
    let deadline = tilo_test_env::hang_deadline();
    let beas_agent = decides(&before, &bea_person);
    while decides(&cys.policy_of(&workspace).await.unwrap(), &bea_person) != beas_agent
        || decides(&membership_log_alone(cys, &workspace).await, &bea_person) != beas_agent
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Bea's answer never reached Cy"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    for change in [cys_stop, cys_answer] {
        answer_grant(cys, &workspace, change)
            .await
            .expect("Cy's say about his own agent of that id is his");
    }
    let cys_after = [
        cys.policy_of(&workspace).await.unwrap(),
        membership_log_alone(cys, &workspace).await,
    ];
    for policy in &cys_after {
        assert!(policy.stops(SCOUT, &cy_person, &cx()), "Cy stopped his own");
        assert!(
            !policy.stops(SCOUT, &bea_person, &cx()),
            "and not Bea's: her agent is not his to stop"
        );
        assert_eq!(
            decides(policy, &bea_person),
            beas_agent,
            "Bea's agent is decided as before Cy wrote anything"
        );
        assert_eq!(
            decides(policy, &cy_person),
            [Answer::Deny; 2],
            "a `{SCOUT}` certified by Cy's root is his, and stopped"
        );
    }

    beas.answer_stop(&workspace, SCOUT)
        .await
        .expect("Bea stops her own agent");
    for policy in [
        beas.policy_of(&workspace).await.unwrap(),
        membership_log_alone(beas, &workspace).await,
    ] {
        assert!(policy.stops(SCOUT, &bea_person, &cx()), "Bea stopped hers");
        assert_eq!(decides(&policy, &bea_person), [Answer::Deny; 2]);
    }
    beas.answer_go_on(&workspace, SCOUT)
        .await
        .expect("and lets it go on");
    let policy = beas.policy_of(&workspace).await.unwrap();
    assert!(!policy.stops(SCOUT, &bea_person, &cx()));
    assert_eq!(decides(&policy, &bea_person), decides(&before, &bea_person));
    assert!(
        !membership_log_alone(beas, &workspace)
            .await
            .stops(SCOUT, &bea_person, &cx()),
        "a replica holding the membership log alone lets it go on too"
    );

    cy.shutdown().await;
    bea.shutdown().await;
    ada.shutdown().await;
}
