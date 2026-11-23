//! What an agent on the server asks its person before it does it, with no device of theirs on
//! while it works.
//!
//! An action the person's grants leave to them is kept as its asked record at a page of theirs,
//! and the turn ends. The record waits there, over a restart of the server. The person's own
//! approve does what the server kept, once. Approving and granting lets exactly that action stand
//! for them, so the next one goes ahead unasked. A removal carried inside another action asks by
//! its own name, and granting the carrier lets no later removal through.
use super::hosted_agent_turns::{SCOUT, conversation_part, make_agent, write_to_agent};
use super::*;
use tilo_server::workspaces::{
    Workspaces,
    person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    },
};
use tilo_workspace_service::agents::{ServerAgents, WorkspaceActionProvider, invoke_named};
use ui::tilo_resource::{Id, Resource as _};
use ui::tilo_workspace_agent::{WorkspaceAgents, workspace_agents};

const CHAT: &str = "so.tilo.chat";
const EBOOK: &str = "so.tilo.ebook";
/// What the agent is told when its person is asked first.
const ASKED: &str = "asked:";
/// Taking the one book on the shelf off it: Undo cannot give the reader back the book's file,
/// so the action asks first.
const REMOVE: &str = "ebook/1 book#remove-from-library";

/// A message as Chat keeps it.
type Held = chat::chat::Message;

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
        let governor = workspaces.governor().clone();
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
    /// The questions of any kind that wait in the conversation, oldest first.
    /// One change to the conversation's document, written as `who` at `at_ms`: whether the document
    /// took it. Nothing an agent may call answers a question, so an answer is no action of the
    /// server's service: it is written as a device writes it.
    /// `who` answers the question `message` asks with `answer`, at `at_ms`: whether the document
    /// took the answer.
    /// The server looks at the workspace, and the question `message` asks is read once what came
    /// of its answer is said for good.
    /// The message `message`, as the document holds it now.
    /// The question `message` asks, as the document holds it now.
    /// What stands for the person, as their own device reads it from the server.
    /// The questions that wait for the person, as their own device reads them from the server.
    /// The messages of the conversation that carry the cause `key`.
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

/// The one question the conversation holds about an action, once the turn that asked ended.
/// Whether the book titled `title` stands on the shelf of the workspace's library now, as
/// the server keeps the library.
async fn on_the_shelf(hosted: &Hosted, title: &str) -> bool {
    let saved = hosted
        .workspaces
        .resource_document(&hosted.workspace, EBOOK, "")
        .await
        .unwrap()
        .expect("the server keeps the workspace's library");
    ebook::resource::Ebook::read(&automerge::AutoCommit::load(&saved).unwrap())
        .unwrap()
        .entries
        .values()
        .any(|book| book.title == title)
}

/// Wait, driving the server, until the book `title` is off the shelf.
async fn until_off_the_shelf(hosted: &Hosted, title: &str) {
    let deadline = tilo_test_env::hang_deadline();
    while on_the_shelf(hosted, title).await {
        assert!(
            Instant::now() < deadline,
            "{title} was never taken off the shelf"
        );
        hosted.agents.look(&hosted.workspace).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// An action that asks first waits as its asked record at its person's page, and the turn ends.
/// Nothing is done while it waits, over a restart of the server. Their approve does what the
/// server kept, once, and lets nothing stand.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_action_that_asks_first_waits_for_its_person_and_is_done_once_on_their_answer() {
    use tilo_actions::remote::asked::{AskedBy, Offer};
    let mut hosted = Hosted::start().await;
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    let told = hosted.tool_results();
    assert!(
        told.contains(ASKED),
        "the agent is told its person is asked, and its turn ended: {told}"
    );

    // The person's own page lists the call as its asked record: who asks, what would be done,
    // on which record, and that Undo cannot take it back.
    let asked = hosted.page.asks().await;
    assert_eq!(
        asked.len(),
        1,
        "one question about the action; the model was shown:\n{}",
        hosted.tool_results()
    );
    let question = &asked[0];
    let AskedBy::Agent(who) = &question.by else {
        panic!("an agent asks: {question:?}");
    };
    assert_eq!((who.id.as_str(), who.name.as_str()), (SCOUT, "Scout"));
    assert_eq!(question.record.workspace, hosted.workspace);
    assert_eq!(question.workspace_name, "Team", "{question:?}");
    assert_eq!(question.name, "Remove from library", "{question:?}");
    assert!(question.destructive, "Undo cannot bring the book back");
    assert!(
        question
            .facts
            .iter()
            .any(|fact| fact.value.contains("book:dusk")),
        "it names the book: {question:?}"
    );
    assert_eq!(
        question.offers,
        [Offer::Approve, Offer::ApproveAndGrant, Offer::Decline],
        "a removal may stand for its person"
    );

    // Nobody answered: a look does nothing, however often the server looks.
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert_eq!(hosted.page.asks().await.len(), 1);
    assert!(on_the_shelf(&hosted, "Dusk").await, "nothing ran");

    // The server restarts, and the record still waits.
    hosted.restart().await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert_eq!(hosted.page.asks().await.len(), 1, "the record survives");

    // The person approves: what the server kept runs, once.
    let question = hosted.page.asks().await.remove(0);
    hosted
        .page
        .resolve(&question, "approve", None)
        .await
        .expect("the person approves it");
    assert!(
        hosted.page.asks().await.is_empty(),
        "answered, it waits for nobody any more"
    );
    until_off_the_shelf(&hosted, "Dusk").await;

    // Allowed once is allowed once: the next removal asks again.
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    assert_eq!(
        hosted.page.asks().await.len(),
        1,
        "allowed once, nothing stands: {}",
        hosted.tool_results()
    );
    hosted.page.peer.close();
}

/// Approving and granting lets exactly what was asked stand for its person: the next such action
/// goes ahead with nobody asked, and what it did is in the workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approving_and_granting_lets_that_action_go_ahead_unasked() {
    let mut hosted = Hosted::start().await;
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    let asked = hosted.page.asks().await;
    assert_eq!(asked.len(), 1, "{}", hosted.tool_results());
    hosted
        .page
        .resolve(&asked[0], "approve_and_grant", None)
        .await
        .expect("the person lets it stand");
    until_off_the_shelf(&hosted, "Dusk").await;

    // The next removal goes ahead unasked.
    hosted
        .shelves_then_removes("Put Dawn on the shelf, then take it off again.", "dawn")
        .await;
    assert!(
        !hosted.tool_results().contains(ASKED),
        "nobody is asked a second time: {}",
        hosted.tool_results()
    );
    assert!(
        hosted.page.asks().await.is_empty(),
        "no second question: {}",
        hosted.tool_results()
    );
    until_off_the_shelf(&hosted, "Dawn").await;
    hosted.page.peer.close();
}

/// What the person asked for in the turn that asks them back.
const WORK: &str = "Send the deck to the board.";

/// A person's stop of a turn takes back what that turn asked them: the asked record the server
/// kept for an action that asks first leaves their waiting list, nothing the server kept is done,
/// and no model is asked again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_stop_takes_back_what_the_stopped_turn_asked() {
    let mut hosted = Hosted::start().await;
    let shelved = super::api_questions::shelved("dusk", "dusk", "");
    // The turn asks to take a book off the shelf, and works on: the next call of its model
    // never answers.
    for step in [
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
        WORK,
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
    while hosted.model.asked().len() < 4 || hosted.page.asks().await.is_empty() {
        assert!(
            Instant::now() < deadline,
            "the turn never asked and went on: {}",
            hosted.tool_results()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

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

    // What it asked is taken back, nothing waits for her, and no model is asked again.
    assert!(
        hosted.page.asks().await.is_empty(),
        "the stopped turn's ask is taken back"
    );
    assert!(on_the_shelf(&hosted, "dusk").await, "nothing was done");
    hosted.agents.look(&hosted.workspace).await.unwrap();
    hosted.agents.look(&hosted.workspace).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hosted.model.asked().len(), 4, "no model is asked again");
    hosted.page.peer.close();
}

/// The assistant shelves a book and takes it off again, the removal carried inside the
/// library's `carrier` (`apply` or `group`): what Undo cannot take back becomes one question
/// to its person, whatever carries it, and grants nothing for a later removal.
async fn the_removal_asks_first_inside(carrier: &str) {
    use tilo_actions::remote::asked::Offer;
    let mut hosted = Hosted::start().await;
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
    let asked = hosted.page.asks().await;
    assert_eq!(
        asked.len(),
        1,
        "inside {carrier} the removal is one question: {told}"
    );
    let question = &asked[0];
    assert_eq!(
        question.name, "Remove from library",
        "inside {carrier} the question says what would be done, not what carries it: {question:?}"
    );
    assert!(
        question
            .facts
            .iter()
            .any(|fact| fact.value.contains("book:dusk")),
        "inside {carrier} it names the book: {question:?}"
    );
    assert!(
        question.destructive,
        "inside {carrier} it does not say Undo takes it back"
    );
    assert_eq!(
        question.offers,
        [Offer::Approve, Offer::Decline],
        "inside {carrier} the question offers no always"
    );

    // Approving and granting the carrier does not let a later removal through unasked.
    let question = hosted.page.asks().await.remove(0);
    hosted
        .page
        .resolve(&question, "approve_and_grant", None)
        .await
        .expect("the person lets the call it was shown stand");
    until_off_the_shelf(&hosted, "dusk").await;
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    assert_eq!(
        hosted.page.asks().await.len(),
        1,
        "inside {carrier} a later removal asks again: {}",
        hosted.tool_results()
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
    let mut hosted = Hosted::approved_for(&[CHAT]).await;
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
    assert!(hosted.page.asks().await.is_empty(), "nobody was asked");
}
