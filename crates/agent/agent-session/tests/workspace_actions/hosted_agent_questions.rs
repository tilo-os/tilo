//! What an agent on the server asks its person before it does it, with no device of theirs on
//! while it works.
//!
//! An action the person's grants leave to them becomes a question in the conversation the turn
//! is in, and the turn ends. The question waits there: over a restart of the server, for a day.
//! The person's own answer does what the server kept, once. "Always" lets exactly that action
//! stand for them, so the next one goes ahead unasked, until they take it back. Nobody else's answer
//! counts, and a question an agent wrote itself does nothing and lets nothing stand.
use super::hosted_agent_turns::{SCOUT, conversation_part, make_agent, write_to_agent};
use super::*;
use std::sync::Mutex;
use tilo_actions::remote::person_asking::{self, ActionName, Asked, Words};
use tilo_server::workspaces::{
    Workspaces,
    person_asking::kept_asks::KeptAsk,
    person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    },
};
use tilo_workspace_service::agents::{
    Answered, Carried, Carries, ServerAgents, ServerAsk, WorkspaceActionProvider, invoke_named,
};
use ui::tilo_resource::{Id, NonBlank, Resource as _};
use ui::tilo_workspace_agent::{WorkspaceAgents, workspace_agents};

const CHAT: &str = "so.tilo.chat";
const EBOOK: &str = "so.tilo.ebook";
/// What the agent is told when its person is asked first.
const ASKED: &str = "Your person is asked about this in the conversation";
/// Taking the one book on the shelf off it: Undo cannot give the reader back the book's file,
/// so the action asks first.
const REMOVE: &str = "ebook/1 book#remove-from-library";
/// A day, in seconds.
const A_DAY: i64 = 24 * 60 * 60 * 1_000;

/// A message as Chat keeps it.
type Held = chat::chat::Message;

/// Who writes an answer into the conversation's document.
#[derive(Clone, Copy)]
enum Who<'a> {
    /// The person asked, at a device of theirs.
    Herself,
    /// Her own agent, acting for her.
    HerAgent,
    /// Somebody else, at a device of their own.
    Another(&'a str),
}

/// A server with one person, their assistant on it, and the workspace's own model.
struct Hosted {
    /// The server itself, kept running until the test ends.
    server: crate::harness::hosted::Hosted,
    actions: tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: Workspaces,
    workspace: String,
    /// The person's own device: on only where the test says so.
    page: Browser,
    root: SigningKey,
    browser: crate::support::Device,
    agents: ServerAgents,
    /// The workspace's model, scripted, and everything it was sent.
    model: Arc<ScriptedProvider>,
    _state: tempfile::TempDir,
}

impl Hosted {
    async fn start() -> Self {
        Self::approved_for(&[]).await
    }

    /// [`Self::start`], its assistant approved for `apps` alone where any are named.
    async fn approved_for(apps: &[&str]) -> Self {
        let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let holder = enrol(&root, "tilo-sh", DeviceCapabilities::member());
        let browser = enrol(&root, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let server = crate::harness::hosted::start(
            &root,
            &holder,
            state.path(),
            crate::harness::hosted::Options::offering(crate::common::catalogue),
        )
        .await;
        let actions = server.roster.actions.clone();
        let workspaces = server.workspaces().unwrap().clone();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        let page = Browser::open_at(&server, &browser, &holder, &workspace).await;
        let acts_for = root.verifying_key().to_bytes();
        let person = hex::encode(acts_for);
        make_agent(&page, &workspaces, &workspace, &person, None).await;
        let catalogue = crate::common::catalogue();
        let governor = workspaces.device_cert().clone();
        workspaces
            .prepare_application(&workspace, &governor, &catalogue, CHAT, "")
            .await
            .unwrap();
        let model = ScriptedProvider::answering(Vec::new());
        workspaces.answer_models_with(model.clone());
        workspaces
            .link_model_for(
                &workspace,
                &Linking {
                    name: "Google",
                    protocol: ProviderProtocol::OpenAiCompatible,
                    endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                    key: Some("the-persons-own-model-key"),
                    models: &["gemini-main", "gemini-pro"],
                },
                true,
            )
            .await
            .unwrap();
        let agents = Box::pin(server.approving_for(&workspace, &[SCOUT], apps)).await;
        Self {
            server,
            actions,
            workspaces,
            workspace,
            page,
            root,
            browser,
            agents,
            model,
            _state: state,
        }
    }

    fn acts_for(&self) -> [u8; 32] {
        self.root.verifying_key().to_bytes()
    }

    fn person(&self) -> String {
        hex::encode(self.acts_for())
    }

    /// The server stops and starts again: whatever ran its agents is gone, and what works them now
    /// knows only what the documents and the server's own store hold.
    async fn restart(&mut self) {
        self.agents = self.server.approving(&self.workspace, &[SCOUT]).await;
    }

    /// The person writes `words` to their assistant, and the server answers the one turn that
    /// makes with the model's next `steps`: each tool call, then its closing words. That turn
    /// has ended when this returns, whoever began it.
    async fn turn(&self, words: &str, steps: Vec<Reply>) {
        for step in steps {
            self.model.then(step);
        }
        let message = write_to_agent(
            &self.actions,
            &self.workspaces,
            &self.workspace,
            (&self.root, &self.browser),
            words,
        )
        .await;
        let turns = self.agents.turns(&self.workspace).await.unwrap();
        assert_eq!(turns.len(), 1, "one turn for {words:?}: {turns:?}");
        tokio::time::timeout(
            Duration::from_secs(60),
            self.agents.answer(&self.workspace, &turns[0]),
        )
        .await
        .expect("the turn ends")
        .unwrap();
        // The server leaves a turn to the one that goes in its place, and says so at once. The
        // turn before this one, whose reply has ended, may hold the place an instant longer:
        // then this one is begun by a look, as that turn makes one itself once it gave the
        // place back. Either way it has ended once its reply has.
        let reply = chat::chat::reply_id(&message, &MemberId::agent(SCOUT));
        let part = conversation_part(&self.workspaces, &self.workspace).await;
        if chat::summons::unanswered(part.messages.get(&reply)) {
            self.replied(&message).await;
        }
    }

    /// The assistant shelves the book `book` and asks to take it off the shelf again.
    async fn shelves_then_removes(&self, words: &str, book: &str) {
        let shelved = super::api_questions::shelved(book, book, "");
        self.turn(
            words,
            vec![
                does(&format!("{book}-open"), "ebook#open", serde_json::json!({})),
                does(&format!("{book}-shelve"), "ebook#shelve", shelved),
                does(
                    &format!("{book}-remove"),
                    REMOVE,
                    serde_json::json!({"book": format!("book:{book}")}),
                ),
                Reply::text("That is all I did."),
            ],
        )
        .await;
    }

    /// The questions about an action the conversation holds, oldest first.
    async fn questions(&self) -> Vec<(Id, Held)> {
        let part = conversation_part(&self.workspaces, &self.workspace).await;
        let mut asked: Vec<(Id, Held)> = part
            .messages
            .iter()
            .filter(|(_, held)| {
                held.question
                    .as_ref()
                    .is_some_and(|question| question.kind.0 == chat::question::ACTION)
            })
            .map(|(id, held)| (id.clone(), held.clone()))
            .collect();
        asked.sort_by_key(|(id, held)| (held.ts, id.to_string()));
        asked
    }

    /// The questions of any kind that wait in the conversation, oldest first.
    async fn waits(&self) -> Vec<(Id, Held)> {
        let part = conversation_part(&self.workspaces, &self.workspace).await;
        let mut asked: Vec<(Id, Held)> = part
            .messages
            .iter()
            .filter(|(_, held)| {
                held.question
                    .as_ref()
                    .is_some_and(chat::question::Question::waiting)
            })
            .map(|(id, held)| (id.clone(), held.clone()))
            .collect();
        asked.sort_by_key(|(id, held)| (held.ts, id.to_string()));
        asked
    }

    /// One change to the conversation's document, written as `who` at `at_ms`: whether the document
    /// took it. Nothing an agent may call answers a question, so an answer is no action of the
    /// server's service: it is written as a device writes it.
    async fn writes(
        &self,
        who: Who<'_>,
        at_ms: Option<i64>,
        change: impl Into<ui::tilo_resource::Invoke<chat::parts::ChatMessages>>,
    ) -> bool {
        let saved = self
            .workspaces
            .resource_document(&self.workspace, CHAT, "")
            .await
            .unwrap()
            .expect("the workspace's chat");
        let held = chat::chat::Chat::read(&automerge::AutoCommit::load(&saved).unwrap()).unwrap();
        let person = self.person();
        let change = change.into();
        let catalogue = crate::common::catalogue();
        let governor = self.workspaces.device_cert().clone();
        let part = chat::parts::part(0);
        self.workspaces
            .author_application(
                &self.workspace,
                &governor,
                &catalogue,
                CHAT,
                part.as_str(),
                |doc| {
                    chat::demo::apply_part_doc_as(
                        doc,
                        &held,
                        |replica| match who {
                            Who::Herself => replica.as_invoker(&person),
                            Who::HerAgent => replica.as_agent_for(SCOUT, &person),
                            Who::Another(other) => replica.as_invoker(other),
                        },
                        at_ms,
                        change,
                    )
                    .map_err(|error| format!("{error:?}"))
                },
            )
            .await
            .is_ok()
    }

    /// `who` answers the question `message` asks with `answer`, at `at_ms`: whether the document
    /// took the answer.
    async fn answers(&self, who: Who<'_>, message: &Id, answer: &str, at_ms: Option<i64>) -> bool {
        let answering = chat::messages::chat_messages::AnswerQuestion {
            message: message.clone(),
            answer: answer.to_owned(),
            device: Some("browser".to_owned()),
        };
        self.writes(
            who,
            at_ms,
            chat::parts::ChatMessages::ANSWER_QUESTION.with(answering),
        )
        .await
    }

    /// The server looks at the workspace, and the question `message` asks is read once what came
    /// of its answer is said for good.
    async fn settled(&self, message: &Id) -> chat::question::Question {
        self.agents.look(&self.workspace).await.unwrap();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let question = self.question(message).await;
            let ended = question.outcome.as_ref().is_some_and(|outcome| {
                [
                    chat::question::DONE,
                    chat::question::DROPPED,
                    chat::question::FAILED,
                ]
                .contains(&outcome.outcome.0.as_str())
            });
            if ended {
                return question;
            }
            assert!(
                Instant::now() < deadline,
                "the server never said what came of the answer: {:?}",
                question.outcome
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The message `message`, as the document holds it now.
    async fn questions_of_any_kind(&self, message: &Id) -> Held {
        conversation_part(&self.workspaces, &self.workspace)
            .await
            .messages
            .get(message)
            .cloned()
            .expect("the message is in the conversation")
    }

    /// The question `message` asks, as the document holds it now.
    async fn question(&self, message: &Id) -> chat::question::Question {
        conversation_part(&self.workspaces, &self.workspace)
            .await
            .messages
            .get(message)
            .and_then(|held| held.question.clone())
            .expect("the question is in the conversation")
    }

    /// What stands for the person, as their own device reads it from the server.
    async fn permissions(&mut self) -> Vec<serde_json::Value> {
        let listed = self
            .page
            .invoke_on(
                person_asking::PROVIDER,
                ActionAddress::new(person_asking::PERMISSIONS, person_asking::LIST),
                InvokeArgument::None,
            )
            .await
            .expect("the server answers the person's own device")
            .value
            .unwrap_or_default();
        listed["permissions"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("a list of permissions: {listed}"))
    }

    /// The questions that wait for the person, as their own device reads them from the server.
    async fn waiting(&mut self) -> Vec<serde_json::Value> {
        let listed = self
            .page
            .invoke_on(
                person_asking::PROVIDER,
                ActionAddress::new(person_asking::WAITING, person_asking::LIST),
                InvokeArgument::None,
            )
            .await
            .expect("the server answers the person's own device")
            .value
            .unwrap_or_default();
        listed["questions"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("a list of questions: {listed}"))
    }

    /// The messages of the conversation that carry the cause `key`.
    async fn caused(&self, key: &str) -> Vec<(Id, Held)> {
        conversation_part(&self.workspaces, &self.workspace)
            .await
            .messages
            .iter()
            .filter(|(_, held)| held.cause.as_ref().is_some_and(|cause| cause.key.0 == key))
            .map(|(id, held)| (id.clone(), held.clone()))
            .collect()
    }

    /// The server looks until the agent's reply to `message` ended: the reply.
    async fn replied(&self, message: &Id) -> Held {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            // Boxed, or the server's whole round lives in the future of every helper waiting here.
            Box::pin(self.agents.look(&self.workspace)).await.unwrap();
            let part = conversation_part(&self.workspaces, &self.workspace).await;
            let reply = part
                .messages
                .get(&chat::chat::reply_id(message, &MemberId::agent(SCOUT)));
            if let Some(reply) = reply
                && !chat::summons::unanswered(Some(reply))
            {
                return reply.clone();
            }
            assert!(
                Instant::now() < deadline,
                "the agent's reply to {message} never ended: {:?}",
                reply.map(|reply| reply.work.clone())
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The asker of the question `message` asks is told how the server carried its answer out,
    /// once, and its turn about that ends: the message that told it.
    async fn told_how_it_ended(&self, message: &Id) -> Held {
        self.model.then(Reply::text("Noted."));
        let key = format!("question:{message}");
        let deadline = tilo_test_env::hang_deadline();
        let (told, held) = loop {
            if let Some(found) = self.caused(&key).await.into_iter().next() {
                break found;
            }
            assert!(
                Instant::now() < deadline,
                "nobody told the agent how {message} ended"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(
            held.cause.as_ref().map(|cause| cause.kind.0.as_str()),
            Some(chat::cause::NEWS),
            "news of what the server did with the answer"
        );
        self.replied(&told).await;
        assert_eq!(self.caused(&key).await.len(), 1, "told once");
        held
    }

    /// What the conversation's newest answer says answered it.
    async fn last_answered(&self) -> chat::chat::Answered {
        let part = conversation_part(&self.workspaces, &self.workspace).await;
        let mut answered: Vec<(i64, chat::chat::Answered)> = part
            .messages
            .iter()
            .filter(|(_, held)| held.author == MemberId::agent(SCOUT))
            .filter_map(|(_, held)| Some((held.ts, held.work.as_ref()?.answered.clone()?)))
            .collect();
        answered.sort_by_key(|(ts, _)| *ts);
        answered
            .pop()
            .map(|(_, answered)| answered)
            .expect("an answer names the model that wrote it")
    }

    /// Choose the model the agent answers with, as their Agents settings do:
    /// the workspace's own agents resource, changed by the person.
    async fn set_agent_model(&self, model: Option<&str>) {
        let document = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
        let saved = self
            .workspaces
            .resource_document(&self.workspace, ui::tilo_workspace_agent::OWNER, &document)
            .await
            .unwrap()
            .expect("the workspace's agents");
        let mut agents = ui::tilo_resource::Replica::<WorkspaceAgents>::from_doc(
            automerge::AutoCommit::load(&saved).unwrap(),
        )
        .as_author(&self.person());
        agents
            .apply(
                WorkspaceAgents::SET_AGENT_MODEL.with(workspace_agents::SetAgentModel {
                    agent: Id::from_raw(SCOUT.to_owned()),
                    model: model.map(str::to_owned),
                }),
            )
            .unwrap();
        let route = DocumentRoute {
            app_id: ui::tilo_workspace_agent::OWNER.into(),
            cluster_id: self.workspace.clone(),
            document: document.clone(),
        };
        let bytes = std::mem::take(agents.doc_mut()).save();
        self.page
            .peer
            .write(route, &collab_types::DocumentMutation::Snapshot(bytes))
            .unwrap();
        // The next turn reads the agents resource the server holds: wait for it.
        let wanted = model.map(str::to_owned);
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let bytes = self
                .workspaces
                .resource_document(&self.workspace, ui::tilo_workspace_agent::OWNER, &document)
                .await
                .unwrap()
                .unwrap();
            let held =
                WorkspaceAgents::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap();
            let model = held
                .agents
                .find(&Id::from_raw(SCOUT.to_owned()))
                .and_then(|agent| agent.model.clone());
            if model == wanted {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the agent's model is not kept: {model:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Everything the model was shown of its tool calls, for a failed assertion to say.
    fn tool_results(&self) -> String {
        self.model
            .asked()
            .iter()
            .filter_map(|asked| asked.request.messages.last())
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What the model was shown of its last tool call: the request that followed it.
    fn last_tool_result(&self) -> String {
        let asked = self.model.asked();
        format!(
            "{:?}",
            asked
                .last()
                .expect("the model was asked")
                .request
                .messages
                .last()
        )
    }
}

/// The model's step that calls `path` with `args` through the agent's tools.
fn does(call: &str, path: &str, args: serde_json::Value) -> Reply {
    let mut input = serde_json::Map::new();
    input.insert("path".to_owned(), path.into());
    input.insert("args".to_owned(), args);
    Reply::tool_call(call, "tilo_do", serde_json::Value::Object(input))
}

/// What a question's detail says, as JSON.
fn detail(question: &chat::question::Question) -> serde_json::Value {
    question.kept().unwrap_or_default()
}

/// The one question the conversation holds about an action, once the turn that asked ended.
async fn the_one_question(hosted: &Hosted) -> (Id, Held) {
    let asked = hosted.questions().await;
    assert_eq!(
        asked.len(),
        1,
        "one question about the action; the model was shown:\n{}",
        hosted.tool_results()
    );
    asked.into_iter().next().unwrap()
}

/// An action that asks first becomes a question in the conversation, and the turn ends. Nothing
/// is done while it waits, over a restart of the server and a day. The person's own answer does what
/// the server kept, once, and lets nothing stand.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_action_that_asks_first_waits_in_the_conversation_and_is_done_once_on_its_persons_answer()
 {
    let mut hosted = Hosted::start().await;
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    let told = hosted.tool_results();
    assert!(
        told.contains(ASKED),
        "the agent is told its person is asked, and its turn ended: {told}"
    );

    let (id, held) = the_one_question(&hosted).await;
    let question = held.question.clone().unwrap();
    assert_eq!(held.author, MemberId::agent(SCOUT), "its asker wrote it");
    assert_eq!(
        question.person,
        MemberId::person(hosted.person()),
        "it asks the person the turn acted for"
    );
    assert!(question.waiting());
    let offered: Vec<&str> = question
        .answers
        .iter()
        .map(|answer| answer.id.as_str())
        .collect();
    assert_eq!(
        offered,
        [
            chat::question::ALLOW,
            chat::question::ALWAYS,
            chat::question::DENY
        ]
    );
    let shown = detail(&question);
    assert_eq!(
        shown["ask"].as_str().map(str::len),
        Some(32),
        "it carries the server's id for what it kept: {shown}"
    );
    assert!(
        question.draft().is_some(),
        "the invocation as its person reviews it: {shown}"
    );
    let always = &shown["always"];
    assert_eq!(always["agent"]["id"], SCOUT, "{shown}");
    assert_eq!(always["workspace"], hosted.workspace.as_str(), "{shown}");
    assert_eq!(always["action"]["provider"], EBOOK, "{shown}");
    assert_eq!(always["action"]["action"], "remove-from-library", "{shown}");
    assert!(always["on"]["resource"].is_string(), "{shown}");
    assert_eq!(always["words"]["place"], "Team", "{shown}");
    assert_eq!(
        hosted.workspaces.open_asks(&hosted.workspace).len(),
        1,
        "the server keeps what was asked"
    );

    // Nobody answered: a look does nothing, however often the server looks.
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert!(hosted.question(&id).await.waiting());

    // The person's own device lists it among the questions that wait for them: who asked, in
    // which words, and each answer as the typed action that gives it.
    let waits = hosted.waiting().await;
    assert_eq!(waits.len(), 1, "{waits:?}");
    let one = &waits[0];
    assert_eq!(one["workspace"], hosted.workspace.as_str(), "{one}");
    assert_eq!(one["message"], id.as_str(), "{one}");
    assert_eq!(one["conversation"], held.channel.as_str(), "{one}");
    assert_eq!(one["kind"], chat::question::ACTION, "{one}");
    assert_eq!(one["words"], held.text.as_str(), "{one}");
    assert_eq!(
        one["by"],
        serde_json::json!({"id": SCOUT, "name": "Scout"}),
        "{one}"
    );
    assert_eq!(
        one["askedAtMs"].as_i64(),
        Some(held.ts),
        "since when it waits: {one}"
    );
    let answers = one["answers"].as_array().unwrap();
    assert_eq!(answers.len(), 3, "{one}");
    assert_eq!(answers[0]["target"]["app"], CHAT, "{one}");
    assert_eq!(answers[0]["target"]["action"], "answer-question", "{one}");
    assert_eq!(
        answers[0]["arguments"],
        serde_json::json!({"message": id.as_str(), "answer": chat::question::ALLOW}),
        "{one}"
    );

    // The server restarts, and the person answers a day later.
    hosted.restart().await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert!(hosted.question(&id).await.waiting());
    let later = held.ts + A_DAY;
    assert!(
        hosted
            .answers(Who::Herself, &id, chat::question::ALLOW, Some(later))
            .await,
        "the person asked answers, at a device of theirs"
    );
    let settled = hosted.settled(&id).await;
    hosted.told_how_it_ended(&id).await;
    assert_eq!(
        settled
            .outcome
            .as_ref()
            .map(|outcome| outcome.outcome.0.as_str()),
        Some(chat::question::DONE),
        "what the server kept ran: {:?}",
        settled.outcome
    );
    assert!(
        settled
            .answered
            .as_ref()
            .is_some_and(|answered| answered.at >= held.ts + A_DAY),
        "answered a day after it was asked: {:?}",
        settled.answered
    );

    // It ran once: nothing is kept any more, and a later look finds nothing to do.
    assert!(hosted.workspaces.open_asks(&hosted.workspace).is_empty());
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.restart().await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let after = hosted.question(&id).await;
    assert_eq!(after.outcome, settled.outcome, "said once, for good");
    assert_eq!(
        hosted.questions().await.len(),
        1,
        "and nothing was asked again"
    );

    assert!(
        hosted.waiting().await.is_empty(),
        "answered, it waits for nobody any more"
    );

    // Allowed once is allowed once: nothing stands.
    assert!(hosted.permissions().await.is_empty());
    assert!(
        hosted
            .workspaces
            .permissions_of(&hosted.acts_for())
            .permissions
            .is_empty()
    );
    hosted.page.peer.close();
}

/// "Always" lets exactly what was asked stand for its person: the next such action goes ahead with
/// nobody asked and is counted, the person reads it on their own device with the action that
/// takes it back, and from the moment they do the action asks again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_lets_that_action_stand_until_its_person_takes_it_back() {
    let mut hosted = Hosted::start().await;
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    let (id, _) = the_one_question(&hosted).await;
    assert!(hosted.permissions().await.is_empty(), "nothing stands yet");
    assert!(
        hosted
            .answers(Who::Herself, &id, chat::question::ALWAYS, None)
            .await
    );
    let settled = hosted.settled(&id).await;
    hosted.told_how_it_ended(&id).await;
    assert_eq!(
        settled
            .outcome
            .as_ref()
            .map(|outcome| outcome.outcome.0.as_str()),
        Some(chat::question::DONE),
        "{:?}",
        settled.outcome
    );

    // The person reads what stands, on their own device: exactly what was asked.
    let stands = hosted.permissions().await;
    assert_eq!(stands.len(), 1, "{stands:?}");
    let one = stands[0].clone();
    let shown = detail(&settled);
    assert_eq!(one["source"], "asking");
    assert_eq!(one["agent"]["id"], SCOUT);
    assert_eq!(one["workspace"], hosted.workspace.as_str());
    assert_eq!(one["action"], shown["always"]["action"], "{one}");
    assert_eq!(one["on"], shown["always"]["on"], "{one}");
    assert_eq!(one["words"], shown["always"]["words"], "{one}");
    assert_eq!(one["askedBy"]["message"], id.as_str(), "{one}");
    assert_eq!(one["usedTimes"], 0, "{one}");

    // The next such action goes ahead unasked, and is counted.
    hosted
        .shelves_then_removes("Put Dawn on the shelf, then take it off again.", "dawn")
        .await;
    let told = hosted.last_tool_result();
    assert!(
        !hosted.tool_results().contains("dawn-remove") || !told.contains(ASKED),
        "nobody is asked a second time: {told}"
    );
    assert_eq!(
        hosted.questions().await.len(),
        1,
        "no second question: {}",
        hosted.tool_results()
    );
    let stands = hosted.permissions().await;
    assert_eq!(stands[0]["usedTimes"], 1, "{stands:?}");
    assert!(stands[0]["lastUsedMs"].as_u64().is_some(), "{stands:?}");

    // The person takes it back with the action its listing names, and the action asks again.
    let take_back = &one["takeBack"];
    let left = hosted
        .page
        .invoke_on(
            take_back["provider"].as_str().unwrap(),
            ActionAddress::new(
                take_back["resource"].as_str().unwrap(),
                take_back["action"].as_str().unwrap(),
            ),
            InvokeArgument::Json(take_back["arguments"].as_object().unwrap().clone()),
        )
        .await
        .expect("the person takes back what stood for them")
        .value
        .unwrap_or_default();
    assert_eq!(left["permissions"], serde_json::json!([]), "{left}");
    hosted
        .shelves_then_removes("Put Noon on the shelf, then take it off again.", "noon")
        .await;
    let asked = hosted.questions().await;
    assert_eq!(
        asked.len(),
        2,
        "taken back, it asks again: {}",
        hosted.tool_results()
    );
    assert!(asked[1].1.question.as_ref().unwrap().waiting());
    assert!(hosted.permissions().await.is_empty());
    hosted.page.peer.close();
}

/// Only the person asked answers. Their own agent's answer and another person's are refused by
/// the document, and nothing is done or stands. A question the agent wrote itself, under the id of
/// what the server kept and showing a wider "always", is not the server's: the person's answer to
/// it does nothing and lets nothing stand. And "deny" drops what was asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_counts_only_as_the_persons_own_to_the_question_the_server_put() {
    let mut hosted = Hosted::start().await;
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    let (id, held) = the_one_question(&hosted).await;

    for (who, name) in [
        (Who::HerAgent, "her own agent"),
        (Who::Another("b0b0b0b0"), "another person"),
    ] {
        for answer in [chat::question::ALWAYS, chat::question::ALLOW] {
            assert!(
                !hosted.answers(who, &id, answer, None).await,
                "{name} does not answer for her ({answer})"
            );
        }
    }
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert!(hosted.question(&id).await.waiting(), "nothing was answered");
    assert_eq!(hosted.workspaces.open_asks(&hosted.workspace).len(), 1);
    assert!(hosted.permissions().await.is_empty());

    // The agent writes a question of its own under the kept ask's id, showing an always that
    // covers every action of the app, and the person is led to answer it.
    let real = detail(held.question.as_ref().unwrap());
    let mut forged = real.clone();
    forged["always"]["action"]["action"] = serde_json::json!("purge");
    forged["always"]["on"] = serde_json::Value::Null;
    let asking = chat::messages::chat_messages::Ask {
        channel: held.channel.clone(),
        text: NonBlank::parse("Please confirm so I can go on.").unwrap(),
        answers: Some(
            [
                chat::question::ALLOW,
                chat::question::ALWAYS,
                chat::question::DENY,
            ]
            .into_iter()
            .map(|id| chat::question::Choice {
                id: id.to_owned(),
                label: String::new(),
            })
            .collect(),
        ),
        kind: Some(chat::question::ACTION.to_owned()),
        detail: Some(forged),
        parent: None,
    };
    assert!(
        hosted
            .writes(
                Who::HerAgent,
                None,
                chat::parts::ChatMessages::ASK.with(asking)
            )
            .await,
        "an agent may ask its person anything in words"
    );
    let asked = hosted.questions().await;
    assert_eq!(asked.len(), 2);
    let (forged_id, _) = asked
        .iter()
        .find(|(other, _)| *other != id)
        .cloned()
        .unwrap();
    assert!(
        hosted
            .answers(Who::Herself, &forged_id, chat::question::ALWAYS, None)
            .await,
        "the person answers what they are shown"
    );
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let untouched = hosted.question(&forged_id).await;
    assert!(
        untouched.outcome.is_none(),
        "the server carries nothing out for a question it did not put: {:?}",
        untouched.outcome
    );
    assert!(hosted.permissions().await.is_empty(), "and nothing stands");
    assert!(
        hosted.question(&id).await.waiting(),
        "the question the server put still waits"
    );
    assert_eq!(hosted.workspaces.open_asks(&hosted.workspace).len(), 1);

    // The person says no to the one the server put: it is dropped, and nothing stands.
    assert!(
        hosted
            .answers(Who::Herself, &id, chat::question::DENY, None)
            .await
    );
    let settled = hosted.settled(&id).await;
    hosted.told_how_it_ended(&id).await;
    assert_eq!(
        settled
            .outcome
            .as_ref()
            .map(|outcome| outcome.outcome.0.as_str()),
        Some(chat::question::DROPPED),
        "{:?}",
        settled.outcome
    );
    assert!(hosted.workspaces.open_asks(&hosted.workspace).is_empty());
    assert!(hosted.permissions().await.is_empty());
    hosted.page.peer.close();
}

/// What a carrier of the server's own heard: what each ask kept, and its person's answer.
#[derive(Default)]
struct Hears(Mutex<Vec<(serde_json::Value, Answered)>>);

impl Carries for Hears {
    fn carry<'a>(&'a self, ask: &'a KeptAsk, answer: Answered) -> Carried<'a> {
        self.0.lock().unwrap().push((ask.kept.clone(), answer));
        Box::pin(async { Ok(None) })
    }
}

/// Code of the server asks a person for one of their agents, under a word of its own and in its
/// own words. The question waits over a restart of the server, and while whoever carries the
/// answer out is not there yet. Then the carrier hears the person's own answer once: with
/// "always", what was asked stands first; a "no" reaches it as a "no". Asked the same again, the
/// one question stands. What always would cover has to be that agent's, in that workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_the_servers_own_code_asks_reaches_its_carrier_once_with_the_persons_answer() {
    const CARRIER: &str = "start-a-program";
    const WORDS: &str = "May I start programs in this workspace's machine?";
    let mut hosted = Hosted::start().await;
    let heard = Arc::new(Hears::default());
    hosted.agents.carries(CARRIER, heard.clone());
    let scope = Asked {
        workspace: hosted.workspace.clone(),
        agent: SCOUT.to_owned(),
        action: ActionName {
            provider: "tilo.workspace".into(),
            kind: "machine".into(),
            action: "start-a-program".into(),
        },
        on: None,
    };
    let (workspace, acts_for) = (hosted.workspace.clone(), hosted.acts_for());
    let asking = move |kept: serde_json::Value, always: Option<Asked>| ServerAsk {
        workspace: workspace.clone(),
        person: acts_for,
        agent: SCOUT.to_owned(),
        conversation: String::new(),
        words: WORDS.to_owned(),
        carrier: CARRIER.to_owned(),
        kept,
        always,
        named: Words {
            action: "Start a program".into(),
            on: String::new(),
            place: String::new(),
        },
    };
    let first = serde_json::json!({"job": 1});
    hosted
        .agents
        .ask(asking(first.clone(), Some(scope.clone())))
        .await
        .expect("the server's own code asks");
    hosted
        .agents
        .ask(asking(first.clone(), Some(scope.clone())))
        .await
        .expect("asked the same again is no failure");
    let (id, held) = the_one_question(&hosted).await;
    assert_eq!(held.text, WORDS, "in the server's own words");
    assert_eq!(
        held.author,
        MemberId::agent(SCOUT),
        "as that agent's question"
    );
    let question = held.question.clone().unwrap();
    assert_eq!(question.person, MemberId::person(hosted.person()));
    assert_eq!(question.answers.len(), 3, "allow, always and deny");
    let shown = detail(&question);
    assert!(
        shown.get("draft").is_none(),
        "no invocation is shown: {shown}"
    );
    assert_eq!(shown["always"]["action"]["action"], "start-a-program");
    assert_eq!(shown["always"]["words"]["place"], "Team", "{shown}");

    // Nothing is heard while the question waits, and the answer waits while its carrier is not
    // there: the server restarted, and whatever registered it has not started yet.
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert!(heard.0.lock().unwrap().is_empty());
    assert!(
        hosted
            .answers(Who::Herself, &id, chat::question::ALWAYS, None)
            .await
    );
    hosted.restart().await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(heard.0.lock().unwrap().is_empty());
    assert!(hosted.question(&id).await.outcome.is_none());
    assert!(
        hosted
            .workspaces
            .stands(&hosted.acts_for(), &scope)
            .is_none()
    );

    // The carrier is there again: it hears the answer once, and what was asked stands.
    hosted.agents.carries(CARRIER, heard.clone());
    let settled = hosted.settled(&id).await;
    assert_eq!(
        settled
            .outcome
            .as_ref()
            .map(|outcome| outcome.outcome.0.as_str()),
        Some(chat::question::DONE),
        "{:?}",
        settled.outcome
    );
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert_eq!(
        heard.0.lock().unwrap().clone(),
        vec![(first.clone(), Answered::Always)],
        "once, with the person's answer and what the server kept"
    );
    assert!(
        hosted
            .workspaces
            .stands(&hosted.acts_for(), &scope)
            .is_some()
    );
    let stands = hosted.permissions().await;
    assert_eq!(stands.len(), 1, "{stands:?}");
    assert_eq!(stands[0]["action"]["action"], "start-a-program");
    assert!(stands[0].get("on").is_none(), "every thing of that kind");

    // A question that offers no always, and a no: the carrier hears the no.
    let second = serde_json::json!({"job": 2});
    hosted
        .agents
        .ask(asking(second.clone(), None))
        .await
        .expect("asked");
    let asked = hosted.questions().await;
    assert_eq!(asked.len(), 2);
    let (other, other_held) = asked
        .iter()
        .find(|(asked, _)| *asked != id)
        .cloned()
        .unwrap();
    let offered: Vec<String> = other_held
        .question
        .as_ref()
        .unwrap()
        .answers
        .iter()
        .map(|answer| answer.id.clone())
        .collect();
    assert_eq!(offered, [chat::question::ALLOW, chat::question::DENY]);
    assert!(
        !hosted
            .answers(Who::Herself, &other, chat::question::ALWAYS, None)
            .await,
        "an answer the question does not offer is none"
    );
    assert!(
        hosted
            .answers(Who::Herself, &other, chat::question::DENY, None)
            .await
    );
    let settled = hosted.settled(&other).await;
    assert_eq!(
        settled
            .outcome
            .as_ref()
            .map(|outcome| outcome.outcome.0.as_str()),
        Some(chat::question::DROPPED)
    );
    assert_eq!(
        heard.0.lock().unwrap().last().cloned(),
        Some((second, Answered::No)),
        "a denial reaches the carrier as a denial"
    );

    // What always would cover is that agent's, in that workspace; the word of an invocation's
    // own carrier is taken.
    let mut anothers = scope.clone();
    anothers.agent = "somebody-elses".into();
    let mut elsewhere = scope.clone();
    elsewhere.workspace = "another-workspace".into();
    for wider in [anothers, elsewhere] {
        assert!(
            hosted
                .agents
                .ask(asking(serde_json::json!({"job": 3}), Some(wider)))
                .await
                .is_err()
        );
    }
    let mut reserved = asking(serde_json::json!({"job": 3}), None);
    reserved.carrier = "action".into();
    assert!(hosted.agents.ask(reserved).await.is_err());
    assert_eq!(hosted.questions().await.len(), 2, "and nothing was asked");
    hosted.page.peer.close();
}

/// The agent asks its person `words`, offering `answers` (each an id and what its button
/// says), in a turn of its own: the person writes `request` to it, and its model takes Chat's
/// own `ask` through the turn's tools and ends. The question's message.
async fn agent_asks(hosted: &Hosted, request: &str, words: &str, answers: &[(&str, &str)]) -> Id {
    let conversation =
        chat::Chat::direct_id([MemberId::person(hosted.person()), MemberId::agent(SCOUT)]);
    let offered: Vec<serde_json::Value> = answers
        .iter()
        .map(|(id, label)| serde_json::json!({"id": id, "label": label}))
        .collect();
    let asking = serde_json::json!({
        "channel": conversation.as_str(),
        "text": words,
        "answers": offered,
    });
    hosted
        .turn(
            request,
            vec![
                does("ask", "chat/Messages#ask", asking),
                Reply::text("I asked you, and go on when you have answered."),
            ],
        )
        .await;
    let part = conversation_part(&hosted.workspaces, &hosted.workspace).await;
    let (id, held) = part
        .messages
        .iter()
        .find(|(_, held)| held.text == words)
        .unwrap_or_else(|| panic!("the agent asks its person: {}", hosted.tool_results()));
    assert_eq!(held.author, MemberId::agent(SCOUT), "its asker wrote it");
    assert_eq!(
        held.question
            .as_ref()
            .map(|question| question.person.clone()),
        Some(MemberId::person(hosted.person())),
        "and it asks its own person"
    );
    id.clone()
}

/// What the person asked for in the turn that asks them back.
const WORK: &str = "Send the deck to the board.";

/// A turn asks and ends. While the question waits nothing is owed and no model is called. The
/// server restarts, the person answers a day later on a device that writes no words of its own,
/// and the asker is woken with what it asked and what they answered: once, however often the
/// server looks and restarts after that.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_wakes_its_asker_once_a_day_later_and_after_a_restart() {
    const WORDS: &str = "Two versions of the deck exist. Which one?";
    let mut hosted = Hosted::start().await;
    let id = agent_asks(
        &hosted,
        WORK,
        WORDS,
        &[("newest", "The newest"), ("signed", "The signed one")],
    )
    .await;
    let key = format!("question:{id}");
    let asked_at = hosted.questions_of_any_kind(&id).await.ts;
    let before = hosted.model.asked().len();

    // It waits: nothing is owed, nothing is written, no model is asked.
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert!(
        hosted
            .agents
            .turns(&hosted.workspace)
            .await
            .unwrap()
            .is_empty(),
        "a question that waits owes no turn"
    );
    assert!(hosted.caused(&key).await.is_empty());
    assert_eq!(hosted.model.asked().len(), before, "and costs nothing");

    // The server restarts. A day later the person answers, and only answers.
    hosted.restart().await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert!(hosted.caused(&key).await.is_empty());
    hosted
        .model
        .then(Reply::text("The signed one it is. I go on with the deck."));
    assert!(
        hosted
            .answers(Who::Herself, &id, "signed", Some(asked_at + A_DAY))
            .await
    );
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while hosted.caused(&key).await.is_empty() {
        assert!(Instant::now() < deadline, "the answer never woke its asker");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let woken = hosted.caused(&key).await;
    assert_eq!(woken.len(), 1);
    let (wake, held) = woken[0].clone();
    assert_eq!(
        wake,
        chat::cause::message_id(&MemberId::person(hosted.person()), &key),
        "the wake stands where its person and the question's key derive"
    );
    let cause = held.cause.clone().unwrap();
    assert_eq!(cause.kind.0, "answer");
    assert_eq!(cause.person, MemberId::person(hosted.person()));
    assert!(WORDS.starts_with(cause.label.as_str()), "{}", cause.label);
    assert!(
        held.text.contains(WORDS) && held.text.contains("The signed one"),
        "what it asked and what they answered: {}",
        held.text
    );

    // The turn it starts is the asker's, told both, and it ends.
    let reply = hosted.replied(&wake).await;
    assert!(
        reply.text.contains("The signed one it is"),
        "{}",
        reply.text
    );
    let asked = hosted.model.asked();
    assert_eq!(asked.len(), before + 1, "one call of its person's model");
    let told = format!("{:?}", asked[before].request.messages);
    assert!(
        told.contains("Which one?") && told.contains("The signed one"),
        "{told}"
    );
    assert!(told.contains(WORK), "and what it was doing: {told}");

    // Once and only once, whatever the server does next.
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.restart().await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hosted.caused(&key).await.len(), 1, "one wake");
    assert_eq!(hosted.model.asked().len(), before + 1, "one turn");
    assert!(
        hosted
            .agents
            .turns(&hosted.workspace)
            .await
            .unwrap()
            .is_empty()
    );
    hosted.page.peer.close();
}

/// A question its asker took back wakes nobody, and neither does one whose asker already said
/// what came of the answer. Where the person's own words follow their answer, as Chat's window
/// writes them, those words are the one thing that asks the agent: no second turn beside them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_taken_back_or_settled_wakes_nobody_and_the_persons_words_wake_once() {
    use chat::messages::chat_messages::{SettleQuestion, WithdrawQuestion};
    let hosted = Hosted::start().await;
    let yes_or_no = [("yes", "Yes"), ("no", "No")];

    let taken_back = agent_asks(
        &hosted,
        "Get me to the coast tomorrow.",
        "Shall I book the earlier train?",
        &yes_or_no,
    )
    .await;
    assert!(
        hosted
            .writes(
                Who::HerAgent,
                None,
                chat::parts::ChatMessages::WITHDRAW_QUESTION.with(WithdrawQuestion {
                    message: taken_back.clone(),
                    why: Some("It has left already.".to_owned()),
                }),
            )
            .await
    );
    assert!(
        !hosted.answers(Who::Herself, &taken_back, "yes", None).await,
        "a question taken back takes no answer"
    );

    let settled = agent_asks(
        &hosted,
        "Tidy up after the meeting.",
        "Shall I file the notes?",
        &yes_or_no,
    )
    .await;
    assert!(hosted.answers(Who::Herself, &settled, "yes", None).await);
    assert!(
        hosted
            .writes(
                Who::HerAgent,
                None,
                chat::parts::ChatMessages::SETTLE_QUESTION.with(SettleQuestion {
                    message: settled.clone(),
                    outcome: chat::question::DONE.to_owned(),
                    detail: None,
                }),
            )
            .await
    );
    let before = hosted.model.asked().len();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    for id in [&taken_back, &settled] {
        assert!(
            hosted.caused(&format!("question:{id}")).await.is_empty(),
            "nobody is woken for {id}"
        );
    }
    assert_eq!(hosted.model.asked().len(), before, "and no model is asked");

    // Her own words follow her answer: they ask the agent, once.
    let spoken = agent_asks(
        &hosted,
        "Paint the fence.",
        "Which colour?",
        &[("blue", "Blue"), ("green", "Green")],
    )
    .await;
    let before = hosted.model.asked().len();
    assert!(hosted.answers(Who::Herself, &spoken, "blue", None).await);
    hosted.model.then(Reply::text("Blue it is."));
    let words = write_to_agent(
        &hosted.actions,
        &hosted.workspaces,
        &hosted.workspace,
        (&hosted.root, &hosted.browser),
        "Blue",
    )
    .await;
    let reply = hosted.replied(&words).await;
    assert!(reply.text.contains("Blue it is"), "{}", reply.text);
    hosted.agents.look(&hosted.workspace).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        hosted
            .caused(&format!("question:{spoken}"))
            .await
            .is_empty(),
        "no second turn beside her words"
    );
    assert_eq!(
        hosted.model.asked().len(),
        before + 1,
        "one turn for one answer"
    );
    hosted.page.peer.close();
}

/// A person's stop of a turn takes back what that turn asked them and still waits: the question
/// its model asked, and the question this server put for an action that asks first. An answer
/// after that is refused, nothing the server kept is done, and nobody is woken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_stop_takes_back_what_the_stopped_turn_asked() {
    let mut hosted = Hosted::start().await;
    let conversation =
        chat::Chat::direct_id([MemberId::person(hosted.person()), MemberId::agent(SCOUT)]);
    let asking = serde_json::json!({
        "channel": conversation.as_str(),
        "text": "Which shelf?",
        "answers": [{"id": "top", "label": "The top one"}, {"id": "low", "label": "The low one"}],
    });
    let shelved = super::api_questions::shelved("dusk", "dusk", "");
    // The turn asks its person, asks to take a book off the shelf, and works on: the next call
    // of its model never answers.
    for step in [
        does("ask", "chat/Messages#ask", asking),
        does("open", "ebook#open", serde_json::json!({})),
        does("shelve", "ebook#shelve", shelved),
        does("remove", REMOVE, serde_json::json!({"book": "book:dusk"})),
        Reply::never(),
    ] {
        hosted.model.then(step);
    }
    let first = write_to_agent(
        &hosted.actions,
        &hosted.workspaces,
        &hosted.workspace,
        (&hosted.root, &hosted.browser),
        "Tidy my shelf.",
    )
    .await;
    let turns = hosted.agents.turns(&hosted.workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let (agents, workspace, turn) = (
        hosted.agents.clone(),
        hosted.workspace.clone(),
        turns[0].clone(),
    );
    let working = tokio::spawn(async move { agents.answer(&workspace, &turn).await });
    let deadline = tilo_test_env::hang_deadline();
    while hosted.model.asked().len() < 5 || hosted.waits().await.len() < 2 {
        assert!(
            Instant::now() < deadline,
            "the turn never asked twice and went on: {}",
            hosted.tool_results()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let asked = hosted.waits().await;
    assert_eq!(asked.len(), 2, "what it asked and what the server put");
    assert_eq!(hosted.workspaces.open_asks(&hosted.workspace).len(), 1);

    // She stops it, as her page does.
    let herself = tilo_actions::remote::Caller::authenticated(
        hosted.root.verifying_key().to_bytes(),
        hosted.browser.cert.device_public_key,
    )
    .in_workspace(&hosted.workspace);
    let holder = WorkspaceActionProvider::new(
        hosted.actions.clone(),
        WorkspaceId::parse(&hosted.workspace).unwrap(),
    );
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: hosted.workspace.clone(),
        document: chat::parts::part(0).as_str().to_owned(),
    };
    let stopping = serde_json::json!({"message": first.as_str()});
    invoke_named(
        &holder,
        &herself,
        &route,
        &format!("{}/messages/{first}", chat::parts::ChatMessages::KIND),
        "stop-agent",
        stopping.as_object().unwrap().clone(),
    )
    .await
    .expect("she stops her agent's work");
    hosted.agents.look(&hosted.workspace).await.unwrap();
    tokio::time::timeout(Duration::from_secs(60), working)
        .await
        .expect("the stopped turn ends")
        .expect("and did not panic")
        .unwrap();

    // Both are taken back, and say why; neither takes her answer any more.
    for (id, _) in &asked {
        let question = hosted.question(id).await;
        let chat::question::QuestionState::Withdrawn(taken) = question.state() else {
            panic!("{id} still stands: {question:?}");
        };
        assert!(taken.why.contains("stopped"), "{}", taken.why);
        let answer = if question.kind.0 == chat::question::ACTION {
            chat::question::ALLOW
        } else {
            "top"
        };
        assert!(
            !hosted.answers(Who::Herself, id, answer, None).await,
            "a question taken back takes no answer"
        );
    }

    // Nothing the server kept is done, nothing waits for her, nobody is woken, no model is asked.
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        hosted.workspaces.open_asks(&hosted.workspace).is_empty(),
        "what the server kept for the action is gone"
    );
    assert!(hosted.waiting().await.is_empty());
    for (id, _) in &asked {
        assert!(hosted.caused(&format!("question:{id}")).await.is_empty());
    }
    assert_eq!(hosted.model.asked().len(), 5, "and no model is asked again");
    hosted.page.peer.close();
}

/// The assistant shelves a book and takes it off again, the removal carried inside the
/// library's `carrier` (`apply` or `group`): what Undo cannot take back becomes one question
/// to its person, whatever carries it, and the agent is told its person is asked.
async fn the_removal_asks_first_inside(carrier: &str) {
    let hosted = Hosted::start().await;
    let carried =
        serde_json::json!({"action": "remove-from-library", "args": {"book": "book:dusk"}});
    let args = if carrier == "apply" {
        serde_json::json!({"action": {"op": carried}})
    } else {
        serde_json::json!({"actions": [carried]})
    };
    hosted
        .turn(
            "Put Dusk on the shelf, then take it off again.",
            vec![
                does("dusk-open", "ebook#open", serde_json::json!({})),
                does(
                    "dusk-shelve",
                    "ebook#shelve",
                    super::api_questions::shelved("dusk", "dusk", ""),
                ),
                does("dusk-remove", &format!("ebook/1 book#{carrier}"), args),
                Reply::text("That is all I did."),
            ],
        )
        .await;
    let told = hosted.tool_results();
    assert!(
        told.contains(ASKED),
        "inside {carrier} the agent is told its person is asked: {told}"
    );
    let (_, held) = the_one_question(&hosted).await;
    assert!(
        held.question
            .as_ref()
            .is_some_and(chat::question::Question::waiting),
        "inside {carrier} the question waits for its person"
    );
    assert_eq!(
        hosted.workspaces.open_asks(&hosted.workspace).len(),
        1,
        "inside {carrier} the server keeps what was asked"
    );
    hosted.page.peer.close();
}

/// What Undo cannot take back asks first by the assistant's tools inside `apply`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_undo_cannot_take_back_asks_its_person_first_inside_apply() {
    Box::pin(the_removal_asks_first_inside("apply")).await;
}

/// What Undo cannot take back asks first by the assistant's tools inside a `group`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_undo_cannot_take_back_asks_its_person_first_inside_a_group() {
    Box::pin(the_removal_asks_first_inside("group")).await;
}

/// An action that asks its person, carried inside another.
#[path = "hosted_question_in_carrier.rs"]
mod in_a_carrier;

/// Which model wrote each answer stays with the answer: a person switches the agent's model
/// between two turns, and every reply names the provider and model that wrote it, what it cost
/// and the tokens it used — the same conversation, read later, says both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_reply_names_the_model_that_wrote_it_across_a_switch() {
    let hosted = Hosted::start().await;
    hosted
        .turn(
            "Good morning",
            vec![Reply::text("Good morning, Ada.").costing(1_200)],
        )
        .await;
    let first = hosted.last_answered().await;
    assert_eq!(first.provider, "Google");
    assert_eq!(first.model, "gemini-main");
    assert_eq!(first.cost_micros, 1_200);

    hosted.set_agent_model(Some("Google/gemini-pro")).await;
    hosted
        .turn(
            "And tomorrow?",
            vec![Reply::text("Tomorrow too.").costing(300)],
        )
        .await;
    let second = hosted.last_answered().await;
    assert_eq!(second.provider, "Google");
    assert_eq!(second.model, "gemini-pro");
    assert_eq!(second.cost_micros, 300);
    assert_eq!(first.model, "gemini-main", "the first answer keeps its own");
    hosted.page.peer.close();
}

/// An assistant whose approval names no Books opens none: its open of a book is refused in
/// words it can tell its person, and no book is started.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_approved_without_books_starts_no_book() {
    let hosted = Hosted::approved_for(&[CHAT]).await;
    hosted
        .turn(
            "Open a book.",
            vec![
                does("open", "ebook#open", serde_json::json!({})),
                Reply::text("I may not."),
            ],
        )
        .await;
    let told = hosted.tool_results();
    assert!(
        told.contains("this agent was not approved to change"),
        "{told}"
    );
    assert!(hosted.questions().await.is_empty(), "nobody was asked");
}
