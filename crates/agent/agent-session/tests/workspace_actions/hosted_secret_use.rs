//! A person's assistant on the server signs in to their mail with a password it never sees, with
//! no device of theirs on while it works.
//!
//! The whole chain through the server's own interfaces: the assistant needs a mail account and
//! asks for the password as a secret; the person's device answers into the protected path, their
//! vault on the server; the turn that follows holds only the reference and calls the use; the first
//! use asks the person; they answer "Always"; the server signs in and adds the account; the second
//! use asks nothing. The password is in no message, no tool result, no model request, no log line
//! and nowhere on the server's disk in the clear.
use super::hosted_agent_turns::{SCOUT, conversation_part, make_agent, write_to_agent};
use super::*;
use std::sync::Mutex;
use tilo_actions::remote::{person_accounts, person_vault};
use tilo_server::{
    workspace_agents::HostAgents as Agents,
    workspaces::{
        Workspaces,
        person_model::{
            ProviderProtocol,
            scripted::{Linking, Reply, ScriptedProvider},
        },
    },
};
use ui::tilo_resource::{Id, Resource as _};

const CHAT: &str = "so.tilo.chat";
/// What the person types into the protected field. Nothing but the server's sign-in may hold it.
pub(super) const PASSWORD: &str = "only-the-mail-server-may-read-this";
const IMAP: &str = "imap://imap.example.test:993";
const SMTP: &str = "smtp://smtp.example.test:465";
/// What the agent is told when its person is asked first.
const ASKED: &str = "Your person is asked in the conversation";

/// Every log line the test's process writes, at every level.
#[derive(Clone, Default)]
pub(super) struct Lines(pub(super) Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

pub(super) fn logged() -> Lines {
    // One for the process: a second test in it reads the same lines.
    static LINES: std::sync::OnceLock<Lines> = std::sync::OnceLock::new();
    LINES
        .get_or_init(|| {
            let lines = Lines::default();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .with_writer(lines.clone())
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("the test process's only subscriber");
            lines
        })
        .clone()
}

/// Every file under `dir` whose bytes hold `needle`.
pub(super) fn files_holding(dir: &Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
    let mut holding = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => waiting.push(path),
                Ok(metadata) if metadata.is_file() => {
                    let bytes = std::fs::read(&path).unwrap_or_default();
                    if bytes.windows(needle.len()).any(|window| window == needle) {
                        holding.push(path);
                    }
                }
                _ => {}
            }
        }
    }
    holding
}

/// A server with one person, their assistant on it, and their own model.
pub(super) struct Hosted {
    /// The server itself, kept running until the test ends: a test that needs something of
    /// the server's own reads it here.
    server: Box<dyn std::any::Any>,
    pub(super) actions: tilo_agent_session::workspace_actions::WorkspaceActions,
    pub(super) workspaces: Workspaces,
    pub(super) workspace: String,
    /// The person's own device: on only where the test says so.
    pub(super) page: Browser,
    pub(super) root: SigningKey,
    pub(super) browser: crate::support::Device,
    /// The server's own device: what another person's page names as what serves the workspace.
    served_by: crate::support::Device,
    pub(super) agents: Agents,
    /// The person's model, scripted, and everything it was sent.
    pub(super) model: Arc<ScriptedProvider>,
    /// What the mail servers' stand-in was asked to sign in with.
    proved: Arc<Mutex<Vec<String>>>,
    pub(super) state: tempfile::TempDir,
    /// Where the server is reached: what a stand-in for a window in the workspace's machine
    /// dials.
    pub(super) address: iroh::EndpointAddr,
}

impl Hosted {
    pub(super) async fn start() -> Self {
        Self::start_placing(None).await
    }

    /// [`Self::start`], on a server that places its workspaces' Browser windows through
    /// `browsers`, where it is handed any: the assistant's turns reach those windows.
    pub(super) async fn start_placing(
        browsers: Option<Arc<tilo_server::hosted_browser::HostedBrowsers>>,
    ) -> Self {
        let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let server_device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
        let browser = enrol(&root, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        // A window's session reaches the server at the address it is told, and the server
        // certifies that session's device as its person's stand-in.
        let reached = browsers.is_some().then(|| {
            tilo_server::hosted_browser::keep_stand_in(&state.path().join("server"), &root)
                .expect("the root signs the server's stand-in");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(
                tilo_server::hosted_browser::SERVER_ENV,
                format!(
                    "{}@127.0.0.1:7704",
                    hex::encode(server_device.cert.device_public_key)
                ),
            );
            env
        });
        let (server, actions) = tilo_server::agents::start_serving_documents(
            crate::harness::hosted::on_this_machine(state.path()),
            tilo_server::daemon::ServerIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: server_device.key.clone(),
                device_cert: server_device.cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            Some(Arc::new(crate::common::catalogue)),
            None,
            None,
            browsers.clone(),
        )
        .await
        .unwrap();
        drop(reached);
        let address = server.node_addr();
        let workspaces = server.workspaces().unwrap().clone();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
        let keeper = root.verifying_key().to_bytes();
        let person = hex::encode(keeper);
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
                &keeper,
                &Linking {
                    name: "Google",
                    protocol: ProviderProtocol::OpenAiCompatible,
                    endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                    key: Some("the-keepers-own-model-key"),
                    models: &["gemini-main"],
                },
                true,
            )
            .unwrap();
        let proved = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&proved);
        workspaces.sign_in_accounts_through(move |account, password| {
            kept.lock()
                .unwrap()
                .push(format!("{account} with {password}"));
            Ok("signed in".into())
        });
        let agents = Agents::new(workspaces.clone(), actions.clone(), None).placing(browsers);
        Self {
            server: Box::new(server),
            actions,
            workspaces,
            workspace,
            page,
            root,
            browser,
            served_by: server_device,
            agents,
            model,
            proved,
            state,
            address,
        }
    }

    pub(super) fn person(&self) -> String {
        hex::encode(self.root.verifying_key().to_bytes())
    }

    /// The person's conversation with their assistant.
    fn conversation(&self) -> Id {
        chat::Chat::direct_id([MemberId::person(self.person()), MemberId::agent(SCOUT)])
    }

    /// The person writes `words` to their assistant, and the server answers the one turn that
    /// makes with the model's next `steps`: each tool call, then its closing words.
    pub(super) async fn turn(&self, words: &str, steps: Vec<Reply>) {
        for step in steps {
            self.model.then(step);
        }
        write_to_agent(
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
    }

    /// The questions of `kind` the conversation holds, oldest first.
    pub(super) async fn questions(&self, kind: &str) -> Vec<(Id, chat::chat::Message)> {
        let part = conversation_part(&self.workspaces, &self.workspace).await;
        let mut asked: Vec<_> = part
            .messages
            .iter()
            .filter(|(_, held)| {
                held.question
                    .as_ref()
                    .is_some_and(|question| question.kind.0 == kind)
            })
            .map(|(id, held)| (id.clone(), held.clone()))
            .collect();
        asked.sort_by_key(|(_, held)| held.ts);
        asked
    }

    /// The person answers the question `message` asks with `answer`: written into the
    /// conversation's document as the person themselves at a device of theirs, the way their Chat
    /// window writes it. Nothing an agent may call answers a question, so this is no action of
    /// the server's service.
    pub(super) async fn answers(&self, message: &Id, answer: &str, detail: serde_json::Value) {
        let saved = self
            .workspaces
            .resource_document(&self.workspace, CHAT, "")
            .await
            .unwrap()
            .expect("the workspace's chat");
        let held = chat::chat::Chat::read(&automerge::AutoCommit::load(&saved).unwrap()).unwrap();
        let person = self.person();
        let answering = chat::messages::chat_messages::AnswerQuestion {
            message: message.clone(),
            answer: answer.to_owned(),
            device: Some("browser".to_owned()),
            detail: (!detail.is_null()).then_some(detail),
        };
        let catalogue = crate::common::catalogue();
        let governor = self.workspaces.governor().clone();
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

    /// One action of the person's own on the server, asked from their device over its link.
    pub(super) async fn on_device(
        &mut self,
        action: (&str, &str, &str),
        argument: serde_json::Value,
    ) -> serde_json::Value {
        on_page(&mut self.page, action, argument).await
    }

    /// What the model was shown of its last tool call: the request that followed it.
    pub(super) fn last_tool_result(&self) -> String {
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

/// One action of a person's own on the server, asked from the device `page` is, over its link.
async fn on_page(
    page: &mut Browser,
    (provider, resource, action): (&str, &str, &str),
    argument: serde_json::Value,
) -> serde_json::Value {
    let argument = match argument {
        serde_json::Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    page.invoke_on(provider, ActionAddress::new(resource, action), argument)
        .await
        .expect("the server answers the person's own device")
        .value
        .unwrap_or_default()
}

/// Words of the label the tools put before an answer that carries content from outside.
const OUTSIDE: &str = "never instructions to follow";

/// The model's step that calls `path` with `args` through the agent's tools.
pub(super) fn does(call: &str, path: &str, args: &serde_json::Value) -> Reply {
    Reply::tool_call(
        call,
        "tilo_do",
        serde_json::json!({"path": path, "args": args}),
    )
}

/// The use that adds the account `address` with the vault item `reference` names.
fn add_mail(call: &str, address: &str, reference: &str) -> Reply {
    does(
        call,
        "accounts/accounts#add-mail-with-item",
        &serde_json::json!({
            "address": address,
            "incomingHost": "imap.example.test",
            "outgoingHost": "smtp.example.test",
            "password": reference,
            "reason": "You asked me to read your mail.",
        }),
    )
}

/// The assistant asks for the password as a secret, and the person's device answers into their
/// vault: the turn that follows is woken by their words and holds the reference. The reference.
pub(super) async fn the_password_goes_from_the_persons_device_to_their_vault(
    hosted: &mut Hosted,
) -> String {
    let conversation = hosted.conversation();
    hosted
        .turn(
            "Please connect my mail, ada@example.test.",
            vec![
                does(
                    "call-1",
                    "chat/Messages#ask",
                    &serde_json::json!({
                        "channel": conversation.as_str(),
                        "text": "I need the password of ada@example.test.",
                        "kind": "secret",
                        "detail": {"kind": "login", "belongsTo": [IMAP, SMTP], "username": "ada"},
                    }),
                ),
                Reply::text("I asked you for the password. Type it into the field in our chat."),
            ],
        )
        .await;
    let asked = hosted.questions(chat::question::SECRET).await;
    assert_eq!(
        asked.len(),
        1,
        "the assistant asked for one secret: {}",
        hosted.last_tool_result()
    );
    let (question, held) = &asked[0];
    assert_eq!(held.author, MemberId::agent(SCOUT));
    assert_eq!(
        held.question.as_ref().unwrap().person,
        MemberId::person(hosted.person()),
        "only its own person is asked"
    );

    // The protected path: the person's device hands what they typed to their vault on the server,
    // and answers the question with the reference the vault gave.
    let kept = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD,
            ),
            serde_json::json!({
                "kind": "login", "belongsTo": [IMAP, SMTP], "username": "ada", "secret": PASSWORD,
            }),
        )
        .await;
    let reference = kept["reference"].as_str().expect("a reference").to_owned();
    assert!(
        person_vault::ItemRef::parse(&reference).is_some(),
        "{reference}"
    );
    hosted
        .answers(
            question,
            chat::question::FILLED,
            serde_json::json!({"reference": reference}),
        )
        .await;
    reference
}

/// The turn that holds the reference calls the use. Nobody watches, so the server asks the
/// person and uses nothing. They answer "Always", and the server signs in and adds the account.
pub(super) async fn the_first_use_asks_and_always_signs_in(hosted: &Hosted, reference: &str) {
    hosted
        .turn(
            "It is in my vault now.",
            vec![
                add_mail("call-2", "ada@example.test", reference),
                Reply::text("I asked you to allow it. It waits for your answer."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        told.contains(ASKED),
        "the agent is told its person is asked: {told}"
    );
    assert!(
        hosted.proved.lock().unwrap().is_empty(),
        "nothing signed in unasked"
    );
    let asked = hosted.questions(chat::question::ACTION).await;
    assert_eq!(asked.len(), 1, "one question about the use");
    let (question, held) = &asked[0];
    let asks = held.question.as_ref().unwrap();
    assert_eq!(asks.person, MemberId::person(hosted.person()));
    assert_eq!(
        held.text, "You asked me to read your mail.",
        "why, in the agent's words"
    );
    let item = asks
        .uses_item()
        .expect("the card says which item of the vault");
    assert_eq!(item.belongs_to, [IMAP, SMTP]);
    let shown = format!("{:?} {}", asks.draft(), asks.detail.0);
    assert!(
        shown.contains("ada@example.test"),
        "what it signs in to: {shown}"
    );
    let id = reference.trim_start_matches("vault.item:");
    assert!(
        !shown.contains(id),
        "the item's address is withheld: {shown}"
    );

    hosted
        .answers(question, chat::question::ALWAYS, serde_json::Value::Null)
        .await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = hosted.questions(chat::question::ACTION).await;
        let outcome = now[0].1.question.as_ref().unwrap().outcome.clone();
        if outcome
            .as_ref()
            .is_some_and(|outcome| outcome.outcome.0 == chat::question::DONE)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the server never carried the answer out: {outcome:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        hosted.proved.lock().unwrap().clone(),
        vec![format!("ada@example.test with {PASSWORD}")],
        "the server is signed in to with what the vault keeps"
    );
}

/// After "Always" the assistant uses the item at those servers alone: no question, the account
/// in its answer, each use in the item's list, where its person reads the leave.
async fn the_second_use_asks_nothing(hosted: &mut Hosted, reference: &str) {
    hosted
        .turn(
            "Add news@example.test at the same servers too.",
            vec![
                add_mail("call-3", "news@example.test", reference),
                Reply::text("Added news@example.test to your mail accounts."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        told.contains("news@example.test") && !told.contains(ASKED),
        "{told}"
    );
    assert!(
        !told.contains(OUTSIDE),
        "a use of the vault answers this server's own words, no content from outside: {told}"
    );
    assert_eq!(
        hosted.questions(chat::question::ACTION).await.len(),
        1,
        "no second question"
    );
    assert_eq!(
        hosted.proved.lock().unwrap().len(),
        2,
        "both accounts signed in"
    );

    let accounts = hosted
        .on_device(
            (
                person_accounts::PROVIDER,
                person_accounts::RESOURCE,
                person_accounts::LIST,
            ),
            serde_json::Value::Null,
        )
        .await
        .to_string();
    for address in ["ada@example.test", "news@example.test"] {
        assert!(accounts.contains(address), "{address}: {accounts}");
    }
    let remembers = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::USES,
            ),
            serde_json::json!({"item": reference}),
        )
        .await;
    assert_eq!(
        remembers["uses"].as_array().unwrap().len(),
        2,
        "{remembers}"
    );
    assert_eq!(remembers["uses"][1]["by"], "Scout");
    assert_eq!(
        remembers["always"].as_array().unwrap().len(),
        1,
        "{remembers}"
    );
    assert_eq!(remembers["always"][0]["agent"], SCOUT);
    assert_eq!(
        remembers["always"][0]["places"],
        serde_json::json!([IMAP, SMTP])
    );
}

/// The password reached the sign-in and nothing else: no request the model was sent, which
/// holds every tool result and every message it read, no message of the conversation, no log
/// line and no file of the server in the clear.
async fn the_password_is_nowhere_a_model_or_a_reader_looks(hosted: &Hosted, lines: &Lines) {
    let asked = hosted.model.asked();
    assert!(
        asked.len() >= 6,
        "three turns of two calls each: {}",
        asked.len()
    );
    for call in &asked {
        let sent = format!("{:?}", call.request);
        assert!(
            !sent.contains(PASSWORD),
            "a model request holds the password"
        );
    }
    let last = format!("{:?}", asked.last().unwrap().request);
    assert!(
        last.contains("vault.item:"),
        "the model works with the reference"
    );

    let part = conversation_part(&hosted.workspaces, &hosted.workspace).await;
    let said = format!("{:?}", part.messages.iter().collect::<Vec<_>>());
    assert!(said.contains("ada@example.test") && !said.contains(PASSWORD));
    let saved = hosted
        .workspaces
        .resource_document(&hosted.workspace, CHAT, chat::parts::part(0).as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !saved
            .windows(PASSWORD.len())
            .any(|window| window == PASSWORD.as_bytes())
    );

    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    assert!(
        logged.len() > 1_000,
        "the log was observed: {} bytes",
        logged.len()
    );
    assert!(!logged.contains(PASSWORD), "a log line holds the password");

    let holding = files_holding(hosted.state.path(), PASSWORD.as_bytes());
    assert!(
        holding.is_empty(),
        "in the clear on the server's disk: {holding:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_assistant_signs_in_to_mail_with_a_password_it_never_sees() {
    let lines = logged();
    let mut hosted = Hosted::start().await;
    let reference = the_password_goes_from_the_persons_device_to_their_vault(&mut hosted).await;
    the_first_use_asks_and_always_signs_in(&hosted, &reference).await;
    the_second_use_asks_nothing(&mut hosted, &reference).await;
    the_password_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines).await;
    hosted.page.peer.close();
}

/// A use that was refused: the agent is not told that its person is asked, nobody is asked,
/// and nothing signed in.
async fn refused_and_nobody_is_asked(hosted: &Hosted, told: &str) {
    assert!(
        !told.contains(ASKED),
        "the agent is told it was refused: {told}"
    );
    assert!(
        hosted.questions(chat::question::ACTION).await.is_empty(),
        "nobody is asked about a use that names no item of theirs"
    );
    assert!(
        hosted.proved.lock().unwrap().is_empty(),
        "nothing signed in"
    );
}

/// A reference opens nothing by itself. One that names no item of the vault of the person the
/// turn acts for, as the address of an item in anybody else's vault does, reaches the action as
/// it was written and is refused there: nobody is asked, and nothing signs in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reference_her_vault_does_not_hold_is_refused_by_the_use_and_nobody_is_asked() {
    let hosted = Hosted::start().await;
    let elsewhere = format!("vault.item:{}", "0123456789abcdef".repeat(2));
    hosted
        .turn(
            "Sign in to my mail with the password you were given.",
            vec![
                add_mail("call-1", "ada@example.test", &elsewhere),
                Reply::text("That was refused."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        !told.contains("is a resource of the workspace"),
        "a secret's reference is no path of the workspace, and is not refused as one: {told}"
    );
    refused_and_nobody_is_asked(&hosted, &told).await;
    hosted.page.peer.close();
}

/// Where a secret is taken, a text that is no reference stays a text: a path of the workspace,
/// one that climbs out of it and a reference spelled to climb read no resource and open
/// nothing. Each is refused, nobody is asked, and nothing signs in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_path_where_a_secret_is_taken_reads_no_resource_and_opens_nothing() {
    let hosted = Hosted::start().await;
    for (call, written) in [
        ("call-1", "chat"),
        ("call-2", "notes/Launch checklist"),
        ("call-3", "../../../etc/passwd"),
        ("call-4", "vault.item:../../people"),
    ] {
        hosted
            .turn(
                "Sign in to my mail with this.",
                vec![
                    add_mail(call, "ada@example.test", written),
                    Reply::text("That was refused."),
                ],
            )
            .await;
        let told = hosted.last_tool_result();
        refused_and_nobody_is_asked(&hosted, &told).await;
    }
    hosted.page.peer.close();
}

/// What another person typed into their own protected field. Nothing of her turn may hold it.
const HIS_PASSWORD: &str = "only-his-own-mail-server-may-read-this";

/// Admit the person of `device` to the workspace as an editor: the server issues them an
/// invitation and their device redeems it.
async fn admit(hosted: &Hosted, device: &crate::support::Device) {
    let invitation = hosted
        .workspaces
        .issue(
            &hosted.workspace,
            &hosted.workspaces.governor().clone(),
            tilo_server::workspaces::IssueInvitationRequest {
                request_id: device.cert.device_public_key,
                role: tilo_peer_state::join::Role::Editor,
                recipient_user_public_key: Some(device.cert.user_public_key),
                invitee_name: Some("Bo".into()),
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    let verified = tilo_peer_state::join::decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let endpoint = test_server::endpoint(iroh::SecretKey::from_bytes(&device.key.to_bytes())).await;
    tilo_server::workspace_join::WorkspaceJoinClient::new(endpoint)
        .redeem_hint(
            verified.grant().transport.first().unwrap(),
            invitation.signed_invitation.clone(),
            device.cert.clone(),
            &device.key,
            device.cert.device_public_key,
        )
        .await
        .unwrap();
}

/// What another person keeps is not hers to use. Bo, another member of the workspace, keeps a
/// password of his own in his vault on the same server, for the very servers her assistant's
/// call names. The reference of his item reaches the action as it was written, and the vault of
/// the person the turn acts for answers: it holds no such item. Nobody is asked, nothing signs
/// in, his item remembers no use, and what he typed is nowhere her turn or a reader looks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_persons_item_is_refused_by_the_use_and_stays_unused() {
    let lines = logged();
    let mut hosted = Hosted::start().await;
    let vault = |action| (person_vault::PROVIDER, person_vault::RESOURCE, action);
    // Her own vault keeps an item too: his is not looked for in an empty one.
    let hers = hosted
        .on_device(
            vault(person_vault::ADD),
            serde_json::json!({
                "kind": "login", "belongsTo": [IMAP, SMTP], "username": "ada", "secret": PASSWORD,
            }),
        )
        .await;
    let hers = hers["reference"].as_str().expect("a reference").to_owned();

    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let his_device = enrol(&bo, "bo-browser", DeviceCapabilities::member());
    admit(&hosted, &his_device).await;
    let server = hosted
        .server
        .downcast_ref::<tilo_server::daemon::Server>()
        .expect("the server the test started");
    // A member the workspace let in joins it from a page of their own: only a device of the
    // server's own person asks the server to govern a workspace first.
    let peer = Browser::session(server, &his_device, &hosted.served_by);
    let events = peer.events();
    let joined =
        tilo_server_protocol::WorkspaceId::parse(&hosted.workspace).expect("the workspace's id");
    peer.send(tilo_session_client::Cmd::Join(joined))
        .expect("his page asks to join");
    crate::support::admitted_to(&peer, joined).await;
    let mut his_page = Browser::joined(
        (peer, events),
        &his_device,
        &hosted.served_by,
        &hosted.workspace,
    );
    let his = on_page(
        &mut his_page,
        vault(person_vault::ADD),
        serde_json::json!({
            "kind": "login", "belongsTo": [IMAP, SMTP], "username": "bo", "secret": HIS_PASSWORD,
        }),
    )
    .await;
    let his = his["reference"].as_str().expect("a reference").to_owned();
    assert!(person_vault::ItemRef::parse(&his).is_some(), "{his}");
    assert_ne!(his, hers, "two persons' items are two items");

    hosted
        .turn(
            "Sign in to my mail with the password you were given.",
            vec![
                add_mail("call-1", "ada@example.test", &his),
                Reply::text("That was refused."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        !told.contains("is a resource of the workspace"),
        "a secret's reference is no path of the workspace, and is not refused as one: {told}"
    );
    assert!(
        told.contains("That item is not kept here"),
        "her own vault answers for the reference, and it holds no such item: {told}"
    );
    refused_and_nobody_is_asked(&hosted, &told).await;

    // His item is as he kept it: it remembers no use and no leave.
    let uses = on_page(
        &mut his_page,
        vault(person_vault::USES),
        serde_json::json!({"item": his}),
    )
    .await;
    assert!(
        uses["uses"].as_array().is_some_and(Vec::is_empty) && uses.get("always").is_none(),
        "his item was never used: {uses}"
    );
    // What he typed is in no request her model was sent, no log line and no file in the clear.
    for call in &hosted.model.asked() {
        assert!(
            !format!("{:?}", call.request).contains(HIS_PASSWORD),
            "a model request holds another person's password"
        );
    }
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    assert!(
        !logged.contains(HIS_PASSWORD),
        "a log line holds another person's password"
    );
    let holding = files_holding(hosted.state.path(), HIS_PASSWORD.as_bytes());
    assert!(
        holding.is_empty(),
        "in the clear on the server's disk: {holding:?}"
    );
    his_page.peer.close();
    hosted.page.peer.close();
}

/// What a person's "Always" to a question its agent reworded does: nothing.
#[path = "hosted_reworded_question.rs"]
mod reworded;

/// Which device of its person a turn acts as beside the device the server placed for a window.
#[path = "hosted_placed_window.rs"]
mod placed_window;

/// A field of a web page filled from the person's vault, by a turn on their server.
#[path = "hosted_page_fill.rs"]
mod page_fill;

/// The same server, person and assistant, using a service the person connected.
#[path = "hosted_connections.rs"]
mod connections;

/// The same server, person and assistant, opening Browser in the workspace's machine.
#[path = "hosted_turn_browser.rs"]
mod turn_browser;

/// The same server, person and assistant, sending the workspace's Browser window to an address.
#[path = "hosted_turn_pages.rs"]
mod turn_pages;

/// The same server, person and assistant, signing a mail account in from its address alone.
#[path = "hosted_mail_connect.rs"]
mod mail_connect;

/// A person's first message, sent the moment their app says it is ready.
#[path = "hosted_first_message.rs"]
mod first_message;
