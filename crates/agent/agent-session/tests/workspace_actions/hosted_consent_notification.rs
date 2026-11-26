//! Consent raised on a server reaches the person it waits for: a use of their
//! vault is put to them in their conversation, the server tells their devices about
//! it with a button for each answer, and the answer a device gives travels back
//! and the work goes on. A leave given earlier answers the next use without
//! asking, and nothing is used while the question waits.
use super::hosted_agent_turns::{conversation_part, make_agent};
use super::*;
use serde_json::{Value, json};
use std::sync::Mutex;
use tilo_actions::remote::person_telling::{Kind, Told};
use tilo_actions::remote::person_vault;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, MemberId};
use tilo_server::workspaces::Workspaces;
use tilo_server::workspaces::hosted_use::{HostedTurn, OpenAsk, TurnUse, UseAnswer};
use tilo_workspace_service::agents::{WorkspaceActionProvider, invoke_named};
use ui::tilo_resource::{Id, Resource as _};

const CHAT: &str = "so.tilo.chat";
const PASSWORD: &str = "only-the-mail-server-may-read-this";
const IMAP: &str = "imap://imap.example.test:993";
const SMTP: &str = "smtp://smtp.example.test:465";
const AGENT: &str = "scout";

/// The use that adds the account `address` with the vault item `reference` names.
fn add_mail(address: &str, reference: &Value) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::person_accounts::PROVIDER.into(),
            address: ActionAddress::new(
                tilo_actions::remote::person_accounts::RESOURCE,
                tilo_actions::remote::person_accounts::ADD_MAIL_WITH_ITEM,
            ),
        },
        argument: InvokeArgument::Json(
            json!({
                "address": address,
                "incomingHost": "imap.example.test",
                "outgoingHost": "smtp.example.test",
                "password": reference,
                "reason": "You asked me to read your mail.",
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The ask a use answered with, when it asked.
fn asked(taken: Result<TurnUse, ActionCallError>) -> OpenAsk {
    match taken {
        Ok(TurnUse::Asks(open)) => *open,
        other => panic!("the person is asked first: {other:?}"),
    }
}

/// The notification of a waiting question of `person` on this server, once it is told.
async fn asking_told(workspaces: &Workspaces, person: &[u8; 32]) -> Told {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(told) = workspaces
            .told_after(person, 0)
            .told
            .into_iter()
            .find(|told| told.kind == Kind::Question && told.waits())
        {
            return told;
        }
        assert!(
            Instant::now() < deadline,
            "the server never told the person about the question"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The person answers `question` as `answer` in their conversation, as their Chat window
/// writes it on their own device.
async fn answers(
    workspaces: &Workspaces,
    workspace: &str,
    person: &[u8; 32],
    question: &Id,
    answer: &str,
) {
    let saved = workspaces
        .resource_document(workspace, CHAT, "")
        .await
        .unwrap()
        .expect("the workspace's chat");
    let held = chat::chat::Chat::read(&automerge::AutoCommit::load(&saved).unwrap()).unwrap();
    let person = hex::encode(person);
    let answering = chat::messages::chat_messages::AnswerQuestion {
        message: question.clone(),
        answer: answer.to_owned(),
        device: Some("browser".to_owned()),
    };
    let catalogue = crate::common::catalogue();
    let governor = workspaces.governor().clone();
    let part = chat::parts::part(0);
    workspaces
        .author_application(
            workspace,
            &governor,
            &catalogue,
            CHAT,
            part.as_str(),
            |doc| {
                chat::demo::apply_part_doc_as(
                    doc,
                    &held,
                    |replica| replica.as_invoker(&person),
                    None,
                    chat::parts::ChatMessages::ANSWER_QUESTION.with(answering),
                )
                .map_err(|error| format!("{error:?}"))
            },
        )
        .await
        .expect("the person answers their question");
}

/// What the mail servers' stand-in was asked to sign in with.
fn proving(workspaces: &Workspaces) -> Arc<Mutex<Vec<String>>> {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&asked);
    workspaces.sign_in_accounts_through(move |account, password| {
        kept.lock()
            .unwrap()
            .push(format!("{account} with {password}"));
        Ok("signed in".into())
    });
    asked
}

/// A use of the person's vault on their server asks them in their conversation, and their
/// device hears about it with a button for each answer; their answer travels back and the use
/// is made. The leave that answer wrote answers the next use without asking.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_use_on_a_server_asks_the_person_where_their_device_hears_it_and_their_answer_goes_on() {
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server_device = enrol(&ada, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&ada, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let served = crate::harness::hosted::start(
        &ada,
        &server_device,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let workspace_id = WorkspaceId::parse(&workspace).unwrap();
    let person = ada.verifying_key().to_bytes();

    let page = Browser::open_at(&served, &browser, &server_device, &workspace).await;
    let keeper = hex::encode(person);
    make_agent(&page, &workspaces, &workspace, &keeper, None).await;
    let governor = workspaces.governor().clone();
    workspaces
        .prepare_application(&workspace, &governor, &crate::common::catalogue(), CHAT, "")
        .await
        .unwrap();
    let proved = proving(&workspaces);
    // The server reads its conversations for the questions that wait for a person, so a
    // question reaches them with no device of theirs looking.
    tilo_workspace_service::agents::telling::start(&workspaces);

    // Their password goes from their device to their vault on the server, as Settings does.
    let mut page = page;
    let kept = page
        .invoke_on(
            person_vault::PROVIDER,
            ActionAddress::new(person_vault::RESOURCE, person_vault::ADD),
            InvokeArgument::Json(
                json!({
                    "kind": "login",
                    "belongsTo": [IMAP, SMTP],
                    "username": "ada",
                    "secret": PASSWORD,
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .expect("their device keeps the login in their vault");
    let reference = kept.value.unwrap()["reference"].clone();

    // A turn of their agent needs the account. The server asks them first and uses nothing.
    let _engine = served
        .approving_for(
            &workspace,
            &[AGENT],
            &[CHAT, "so.tilo.passwords", "so.tilo.mail"],
        )
        .await;
    let device = served
        .agent_link()
        .roster_session_device(&served.roster.sessions, workspace_id, AGENT, &person)
        .await
        .expect("the agent's approved signing session");
    let agent = workspaces
        .hosted_roster_agent(workspace_id, AGENT, &person, None)
        .await
        .expect("the agent Ada created");
    let conversation =
        chat::Chat::direct_id([MemberId::person(keeper.clone()), MemberId::agent(AGENT)]);
    let caller = Caller::delegated_agent(AGENT, person, device)
        .in_workspace(&workspace)
        .named("Scout")
        .on_roster(agent.created_at, person)
        .on_roster_turn(tilo_action_policy::turn::Turn::new(
            chat::summons::conversation_address(&conversation),
            "message-1",
        ));
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &caller,
        bound: None,
    };
    let adding = add_mail("ada@example.test", &reference);
    let open = asked(
        workspaces
            .use_for_turn(&turn, &adding, json!({"part": "p0"}))
            .await,
    );
    assert_eq!(open.agent, AGENT);
    assert_eq!(open.person, keeper);
    assert_eq!(open.reason, "You asked me to read your mail.");
    assert!(proved.lock().unwrap().is_empty(), "nothing signed in yet");
    assert_eq!(workspaces.open_use_asks(&workspace), vec![open.clone()]);
    assert!(
        workspaces.open_use_asks(&workspace).len() == 1,
        "the one question waits for its answer"
    );

    // The question is put to them where the turn works, as its tools do.
    let service = WorkspaceActionProvider::new(served.roster.actions.clone(), workspace_id);
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    invoke_named(
        &service,
        &caller,
        &route,
        &ui::resource::chain_resource_id(&chat::parts::chain()),
        "ask",
        json!({
            "channel": conversation.as_str(),
            "text": open.reason.clone(),
            "kind": chat::question::ACTION,
            "answers": [
                {"id": chat::question::ALLOW},
                {"id": chat::question::ALWAYS},
                {"id": chat::question::DENY},
            ],
            "detail": open.detail.clone(),
        })
        .as_object()
        .unwrap()
        .clone(),
    )
    .await
    .expect("the question is put to them");

    // Their device hears about it: the question is in their log, with a button for each answer.
    let told = asking_told(&workspaces, &person).await;
    assert_eq!(told.words, "You asked me to read your mail.");
    assert_eq!(
        told.from.as_ref().map(|from| from.name.as_str()),
        Some("Scout"),
        "the notification names the agent that waits"
    );
    let offered: Vec<String> = told
        .actions
        .iter()
        .map(|offered| offered.id.clone())
        .collect();
    assert_eq!(
        offered,
        vec![
            chat::question::ALLOW.to_owned(),
            chat::question::ALWAYS.to_owned(),
            chat::question::DENY.to_owned(),
        ],
        "one button for each answer"
    );
    let allow = &told.actions[0];
    assert!(allow.main, "allow is the answer offered first");
    assert_eq!(allow.target.app, CHAT);
    assert_eq!(
        allow.target.resource,
        ui::resource::chain_resource_id(&chat::parts::chain())
    );
    assert_eq!(
        allow.target.action,
        chat::parts::ChatMessages::ANSWER_QUESTION.id()
    );
    assert_eq!(
        allow.arguments.get("answer").and_then(|a| a.as_str()),
        Some(chat::question::ALLOW)
    );
    // What a device shows of it: the agent's name over the reason it gives.
    let (title, body) = tilo_workspace_service::agents::telling::words_outside(&told);
    assert_eq!(title, "Scout needs you");
    assert_eq!(body, "You asked me to read your mail.");

    // They answer "Always" on their device: the server reads the answer from their
    // conversation and carries the use out, answering their account.
    let the_question: Id = allow
        .arguments
        .get("message")
        .and_then(|id| id.as_str())
        .map(|id| Id::from_raw(id.to_owned()))
        .expect("the button names the question");
    answers(
        &workspaces,
        &workspace,
        &person,
        &the_question,
        chat::question::ALWAYS,
    )
    .await;
    // Their answer is in their conversation now, and the server carries out what it
    // kept under that question with the answer they gave there.
    let part = conversation_part(&workspaces, &workspace).await;
    let given = part
        .messages
        .get(&the_question)
        .and_then(|held| held.question.as_ref())
        .and_then(|question| match question.state() {
            chat::question::QuestionState::Answered(given) => Some(given.answer.0.clone()),
            _ => None,
        })
        .expect("their answer reached the conversation");
    let answer = match given.as_str() {
        chat::question::ALLOW => UseAnswer::Once,
        chat::question::ALWAYS => UseAnswer::Always,
        _ => UseAnswer::No,
    };
    workspaces
        .answer_use(&turn, &open.ask, &open.detail, answer)
        .await
        .expect("their answer is carried out")
        .expect("it answers the account");
    assert_eq!(
        proved.lock().unwrap().clone(),
        vec![format!("ada@example.test with {PASSWORD}")]
    );
    assert!(workspaces.open_use_asks(&workspace).is_empty());

    // The leave their answer wrote stands for the next use: it asks nothing and is made.
    let next = add_mail("news@example.test", &reference);
    assert!(
        matches!(
            workspaces.use_for_turn(&turn, &next, json!({})).await,
            Ok(TurnUse::Done(_))
        ),
        "the second use is not asked about"
    );
    assert_eq!(
        proved.lock().unwrap().clone(),
        vec![
            format!("ada@example.test with {PASSWORD}"),
            format!("news@example.test with {PASSWORD}"),
        ]
    );
    page.peer.close();
}
