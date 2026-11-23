//! A person's assistant on the server signs in to their mail with a password it never sees, with
//! no device of theirs on while it works.
//!
//! The whole chain through the server's own interfaces: the person mentions the assistant in a
//! channel of Chat, which asks it; it needs a mail account and asks for the password through the
//! vault; the server asks the person's own device in the system's protected field, keeps what
//! they type in the workspace's vault on the server and tells the assistant the reference; the
//! turn that follows holds only the reference and calls the use; the first use asks the person in
//! the assistant's reply; they answer "always" there; the server signs in and adds the account;
//! the second use asks nothing. The password is in no message, no reply, no tool result, no model
//! request, no log line and nowhere on the server's disk in the clear.
use super::hosted_agent_turns::{SCOUT, make_agent};
use super::*;
use std::sync::Mutex;
use tilo_actions::remote::person_telling::Kind;
use tilo_actions::remote::{person_accounts, person_vault};
use tilo_server::workspaces::{
    Workspaces,
    person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    },
};
use tilo_workspace_service::agents::{ServerAgents as Agents, windows::Windows};
use tilo_workspace_service::asks as tl_asks;
use tilo_workspace_service::tl_records::runtime::Value as TlValue;
/// What the person types into the protected field. Nothing but the server's sign-in may hold it.
pub(super) const PASSWORD: &str = "only-the-mail-server-may-read-this";
const IMAP: &str = "imap://imap.example.test:993";
const SMTP: &str = "smtp://smtp.example.test:465";
/// What the agent is told when its person is asked first.
const ASKED: &str = "Your person is asked in your reply";

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

impl Lines {
    /// Each line written inside a turn on `workspace`. The process's other tests run their own
    /// turns beside this one's, each on a workspace of its own, into the same lines.
    pub(super) fn of_turns_on(&self, workspace: &str) -> Vec<String> {
        let inside = format!("turn{{workspace={workspace} ");
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .filter(|line| line.contains(&inside))
            .map(str::to_owned)
            .collect()
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

/// A server with one person, their assistant on it, and the workspace's own model.
pub(super) struct Hosted {
    /// The server itself, kept running until the test ends.
    pub(super) server: crate::harness::hosted::Hosted,
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
    /// The workspace's model, scripted, and everything it was sent.
    pub(super) model: Arc<ScriptedProvider>,
    /// What the mail servers' stand-in was asked to sign in with.
    proved: Arc<Mutex<Vec<String>>>,
    pub(super) state: tempfile::TempDir,
    /// Where the server is reached: what a stand-in for a window in the workspace's machine
    /// dials.
    pub(super) address: iroh::EndpointAddr,
    /// Where the person writes to their assistant.
    chat: InChat,
}

/// A person writing to their assistant in tl Chat, and the asks and replies that makes.
pub(super) struct InChat {
    workspaces: Workspaces,
    workspace: String,
    /// The person, by their person id.
    person: String,
    /// The channel of Chat the person writes to their assistant in, by its id and type, once
    /// they wrote there.
    channel: Mutex<Option<(String, String)>>,
}

impl InChat {
    /// Where `person` writes to Scout in `workspace`. Scout is a member that reads the
    /// workspace, as the grant a person gives an agent they add makes it: an ask of an agent
    /// that may not read what it is asked about is no turn.
    pub(super) async fn new(workspaces: &Workspaces, workspace: &str, person: String) -> Self {
        use ui::permissions::policy::{Answer, Grant, Grantee, KindSet, TreePath};
        let member = Grant::new(
            format!("agent-member:{SCOUT}"),
            Grantee::agent(SCOUT),
            TreePath::workspace(workspace),
            KindSet::READ,
            Answer::Allow,
            Grantee::person(person.clone()),
        );
        let governor = workspaces.governor().clone();
        workspaces
            .put_workspace_grants(workspace, &governor, [0x5c; 32], vec![member])
            .await
            .expect("the workspace takes Scout as a member");
        Self {
            workspaces: workspaces.clone(),
            workspace: workspace.to_owned(),
            person,
            channel: Mutex::new(None),
        }
    }

    /// The person writes `words` to their assistant in the channel they talk to it in, as
    /// "@Scout <words>": the mention asks it, as their Chat makes it as it takes the message in.
    pub(super) async fn says(&self, words: &str) {
        let packages = tl_asks::chat_packages().unwrap();
        let ada = tl_asks::Actor::Member(MemberId::person(self.person.clone()));
        let chat = packages.app().to_owned();
        let channel = self.channel.lock().unwrap().clone();
        let words = format!(" {words}");
        let channel = self
            .workspaces
            .with_tl_record(&self.workspace, &packages, ada.clone(), move |opened| {
                let known = channel.and_then(|(id, ty)| tl_asks::ResourceRef::parse_id(&id, ty));
                let channel = match known {
                    Some(channel) => channel,
                    None => {
                        let all = opened
                            .host()
                            .root_of(&chat)
                            .map_err(|error| error.to_string())?;
                        let made = opened.invoke(
                            &ada,
                            (&chat, "new_channel"),
                            vec![TlValue::resource(&all), TlValue::Text("with-scout".into())],
                            None,
                        )?;
                        let TlValue::Ref(made) = made else {
                            return Err("the channel was not made".to_owned());
                        };
                        (*made.target).clone()
                    }
                };
                opened.invoke(
                    &ada,
                    (&chat, "send"),
                    vec![
                        TlValue::resource(&channel),
                        tl_asks::mentioning(&MemberId::agent(SCOUT), "Scout", &words),
                        TlValue::Opt(None),
                    ],
                    None,
                )?;
                Ok((channel.id_string(), channel.ty))
            })
            .await
            .expect("the person writes to their assistant");
        *self.channel.lock().unwrap() = Some(channel);
    }

    /// The asks of the workspace and their replies, as the person's devices read them, oldest
    /// first.
    pub(super) async fn asked(&self) -> Vec<tl_asks::Asked> {
        let packages = tl_asks::chat_packages().unwrap();
        let mut asked = self
            .workspaces
            .with_tl_record(
                &self.workspace,
                &packages,
                tl_asks::Actor::Member(MemberId::person(self.person.clone())),
                |opened| tl_asks::asks(opened),
            )
            .await
            .unwrap();
        asked.sort_by(|one, other| one.asked_at.cmp(&other.asked_at));
        asked
    }

    /// What the questions the assistant's replies put to its person say, oldest first.
    pub(super) async fn questions(&self) -> Vec<String> {
        self.asked()
            .await
            .iter()
            .flat_map(|asked| asked.replies.iter())
            .flat_map(|reply| reply.steps.iter())
            .filter(|step| step.called.starts_with("question:"))
            .map(|step| step.answered.clone())
            .collect()
    }
}

/// The person at their own devices: every question the server puts to them there, each waiting
/// for the answer the test gives it, as their shell's sheet waits for theirs.
#[derive(Default)]
pub(super) struct Devices {
    asked: Mutex<Vec<AskedThere>>,
}

/// A question put to the person's devices, and where its answer goes.
pub(super) struct AskedThere {
    pub(super) question: tilo_server_protocol::certify::PendingConfirmation,
    answer: Option<tokio::sync::oneshot::Sender<tilo_server_protocol::certify::ConfirmationAnswer>>,
}

impl tilo_server::workspaces::AsksPeople for Devices {
    fn ask(
        &self,
        _person: [u8; 32],
        question: tilo_server_protocol::certify::PendingConfirmation,
    ) -> tilo_server::workspaces::PersonAnswer {
        let (answer, answered) = tokio::sync::oneshot::channel();
        self.asked.lock().unwrap().push(AskedThere {
            question,
            answer: Some(answer),
        });
        Box::pin(async move {
            answered
                .await
                .map_err(|_| "nobody answered in time".to_owned())
        })
    }
}

impl Devices {
    /// The questions put to the person's devices, once there are `many`, oldest first.
    pub(super) async fn asked(
        &self,
        many: usize,
    ) -> Vec<tilo_server_protocol::certify::PendingConfirmation> {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let asked: Vec<_> = self
                .asked
                .lock()
                .unwrap()
                .iter()
                .map(|asked| asked.question.clone())
                .collect();
            if asked.len() >= many {
                return asked;
            }
            assert!(
                Instant::now() < deadline,
                "her devices were asked {} times, not {many}",
                asked.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// How many questions were put to the person's devices so far.
    pub(super) fn count(&self) -> usize {
        self.asked.lock().unwrap().len()
    }

    /// The person answers the question at `place` with `answer`, on the device that shows it.
    pub(super) fn answer(
        &self,
        place: usize,
        answer: tilo_server_protocol::certify::ConfirmationAnswer,
    ) {
        let sender = self.asked.lock().unwrap()[place]
            .answer
            .take()
            .expect("a question is answered once");
        sender
            .send(answer)
            .expect("the server waits for the answer");
    }

    /// Nobody answers the question at `place`: it lapses.
    pub(super) fn lapse(&self, place: usize) {
        drop(self.asked.lock().unwrap()[place].answer.take());
    }
}

impl Hosted {
    /// The person's devices from now on, where the server asks them: theirs to answer.
    pub(super) fn her_devices(&self) -> Arc<Devices> {
        let devices = Arc::new(Devices::default());
        self.workspaces.ask_people(devices.clone());
        devices
    }

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
        let server = crate::harness::hosted::start(
            &root,
            &server_device,
            state.path(),
            crate::harness::hosted::Options {
                browsers: browsers.clone(),
                ..crate::harness::hosted::Options::offering(crate::common::catalogue)
            },
        )
        .await;
        drop(reached);
        let actions = server.roster.actions.clone();
        let address = server.node_addr();
        let workspaces = server.workspaces().unwrap().clone();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        let page = Browser::open_at(&server, &browser, &server_device, &workspace).await;
        let acts_for = root.verifying_key().to_bytes();
        let person = hex::encode(acts_for);
        make_agent(&page, &workspaces, &workspace, &person, None).await;
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
                    models: &["gemini-main"],
                },
                true,
            )
            .await
            .unwrap();
        let proved = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&proved);
        workspaces.sign_in_accounts_through(move |account, password| {
            kept.lock()
                .unwrap()
                .push(format!("{account} with {password}"));
            Ok("signed in".into())
        });
        let agents = server
            .approving(&workspace, &[SCOUT])
            .await
            .placing(browsers.map(|browsers| browsers as Arc<dyn Windows>));
        let chat = InChat::new(&workspaces, &workspace, person).await;
        Self {
            server,
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
            chat,
        }
    }

    pub(super) fn person(&self) -> String {
        hex::encode(self.root.verifying_key().to_bytes())
    }

    /// The server stops and starts again: whatever ran its agents is gone, and what works them
    /// now knows only what the workspace and the server's own store hold.
    pub(super) async fn restart(&mut self) {
        self.agents = self.server.approving(&self.workspace, &[SCOUT]).await;
    }

    /// The person writes `words` to their assistant in the channel they talk to it in.
    pub(super) async fn says(&self, words: &str) {
        self.chat.says(words).await;
    }

    /// The person writes `words` to their assistant, and the server answers the one turn that
    /// makes with the model's next `steps`: each tool call, then its closing words.
    pub(super) async fn turn(&self, words: &str, steps: Vec<Reply>) {
        for step in steps {
            self.model.then(step);
        }
        self.says(words).await;
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

    /// The asks of the workspace and their replies, as the person's devices read them.
    pub(super) async fn asked(&self) -> Vec<tl_asks::Asked> {
        self.chat.asked().await
    }

    /// What the questions the assistant's replies put to its person say, oldest first.
    pub(super) async fn questions(&self) -> Vec<String> {
        self.chat.questions().await
    }

    /// The person answers the question that waits with `answer`, written to their assistant
    /// where it asked, and the server looks: it carries the answer out.
    pub(super) async fn answers(&self, answer: &str) {
        self.says(answer).await;
        Box::pin(self.agents.look(&self.workspace)).await.unwrap();
    }

    /// Everything the workspace's asks and their replies say, for what must not be in it.
    pub(super) async fn said(&self) -> String {
        format!("{:?}", self.asked().await)
    }

    /// The bytes the server keeps of the workspace's resources, where the asks and replies are.
    pub(super) async fn record_bytes(&self) -> Vec<u8> {
        self.workspaces
            .store_documents(&self.workspace)
            .await
            .unwrap()
            .concat()
    }

    /// One action of the person's own on the server, asked from their device over its link.
    pub(super) async fn on_device(
        &mut self,
        action: (&str, &str, &str),
        argument: serde_json::Value,
    ) -> serde_json::Value {
        on_page(&mut self.page, action, argument).await
    }

    /// The items the workspace's vault keeps beside the key the workspace's model is called with,
    /// as her own device lists them: that key is an entry of the vault's own ([`Self::start`] links
    /// the model with it), and no sign-in of hers.
    pub(super) async fn workspace_vault(&mut self) -> Vec<serde_json::Value> {
        let kept = self
            .on_device(
                (
                    person_vault::PROVIDER,
                    person_vault::RESOURCE,
                    person_vault::LIST,
                ),
                serde_json::Value::Null,
            )
            .await;
        let model_key = |item: &serde_json::Value| {
            item["kind"] == "key"
                && item["belongsTo"]
                    == serde_json::json!(["https://generativelanguage.googleapis.com"])
        };
        let items = kept["items"]
            .as_array()
            .unwrap_or_else(|| panic!("the workspace's vault: {kept}"));
        assert_eq!(
            items.iter().filter(|item| model_key(item)).count(),
            1,
            "the model's key is the vault's: {kept}"
        );
        items
            .iter()
            .filter(|item| !model_key(item))
            .cloned()
            .collect()
    }

    /// What the model was shown of each of its tool calls, one a line.
    pub(super) fn tool_results(&self) -> String {
        self.model
            .asked()
            .iter()
            .filter_map(|asked| asked.request.messages.last())
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The assistant is woken once by what the server tells it, and answers `says`: everything
    /// its model was sent for that turn.
    pub(super) async fn woken_with(&self, says: &str) -> String {
        self.woken_doing(vec![Reply::text(says)]).await
    }

    /// The one turn the server starts for the assistant on its own, taking the model's next
    /// `steps`: what the assistant was told it was asked, the first request of that turn.
    pub(super) async fn woken_doing(&self, steps: Vec<Reply>) -> String {
        let before = self.model.asked().len();
        for step in steps {
            self.model.then(step);
        }
        let deadline = tilo_test_env::hang_deadline();
        let turns = loop {
            let turns = self.agents.turns(&self.workspace).await.unwrap();
            if !turns.is_empty() {
                break turns;
            }
            assert!(
                Instant::now() < deadline,
                "the assistant was never told what came of it"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(turns.len(), 1, "it is told once: {turns:?}");
        tokio::time::timeout(
            Duration::from_secs(60),
            self.agents.answer(&self.workspace, &turns[0]),
        )
        .await
        .expect("the turn ends")
        .unwrap();
        let asked = self.model.asked();
        format!("{:?}", asked.get(before).expect("it was asked").request)
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

/// The assistant asks for the password through the vault. The ask ends at once; the server asks
/// her own device in the protected field, in the system's words, keeps what she types in the
/// workspace's vault, and asks the assistant once with the reference. The reference.
pub(super) async fn the_password_goes_from_the_persons_device_to_their_vault(
    hosted: &Hosted,
) -> String {
    let devices = hosted.her_devices();
    hosted
        .turn(
            "Please connect my mail, ada@example.test.",
            vec![
                does(
                    "call-3",
                    "vault/vault#ask",
                    &serde_json::json!({
                        "kind": "login", "belongsTo": [IMAP, SMTP], "username": "ada",
                    }),
                ),
                Reply::text("I asked you for the password. Type it where your device asks."),
            ],
        )
        .await;
    assert!(
        hosted.last_tool_result().contains("protected field"),
        "the ask through the vault says she is asked where it cannot read: {}",
        hosted.last_tool_result()
    );
    assert!(
        hosted.questions().await.is_empty(),
        "no question was put in the reply"
    );

    // Her device is asked in the system's protected field, in the system's words.
    let asked = devices.asked(1).await;
    let ask = asked[0].ask.as_ref().expect("a question in words");
    assert!(ask.secret && ask.choices.is_empty(), "{ask:?}");
    for said in [
        format!("asks for the password of ada at {IMAP} and {SMTP}."),
        "never sees what you type: your vault keeps it for those places alone.".to_owned(),
    ] {
        assert!(ask.question.contains(&said), "{said:?}: {}", ask.question);
    }
    devices.answer(
        0,
        tilo_server_protocol::certify::ConfirmationAnswer::Secret(
            tilo_server_protocol::certify::TypedSecret::new(PASSWORD.to_owned()),
        ),
    );

    // The assistant is woken once with the reference the workspace's vault keeps it under.
    let told = hosted.woken_with("Thanks, it is in your vault now.").await;
    let at = told
        .find("vault.item:")
        .expect("the assistant is told the reference");
    let reference = told[at..at + "vault.item:".len() + 32].to_owned();
    assert!(
        person_vault::ItemRef::parse(&reference).is_some(),
        "{reference}"
    );
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
    let asked = hosted.questions().await;
    assert_eq!(asked.len(), 1, "one question about the use: {asked:?}");
    assert!(
        asked[0].starts_with("You asked me to read your mail.") && asked[0].contains("always"),
        "why, in the agent's words, and how it is answered: {}",
        asked[0]
    );
    let id = reference.trim_start_matches("vault.item:");
    assert!(
        !asked[0].contains(id),
        "the question withholds the item's address"
    );
    // Her devices hear of it with none of them looking: the question waits in her log, named by
    // the agent that asks.
    let waiting = || {
        hosted
            .workspaces
            .told_after(&hosted.root.verifying_key().to_bytes(), 0)
            .told
            .into_iter()
            .filter(|told| told.kind == Kind::Question && told.waits())
            .collect::<Vec<_>>()
    };
    let told = waiting();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(
        told[0].words,
        "You asked me to read your mail.\n\nAddress: ada@example.test\nIncomingHost: \
         imap.example.test\nOutgoingHost: smtp.example.test\nPassword: •••\n\nAnswer allow, \
         always or no.",
        "she is told what the use would sign in with, the password withheld"
    );
    assert_eq!(
        told[0].from.as_ref().map(|from| from.name.as_str()),
        Some("Scout")
    );

    // She answers in the channel, as she writes to it: the answer is carried out, and the
    // assistant is asked once what came of it.
    hosted.answers("always").await;
    let told = hosted.woken_with("Your mail is connected.").await;
    assert!(waiting().is_empty(), "the answered question waits no more");
    assert!(
        told.contains("allowed, and from now on without asking"),
        "the assistant is told how she answered: {told}"
    );
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
    assert_eq!(hosted.questions().await.len(), 1, "no second question");
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
        asked.len() >= 7,
        "three turns of two calls each, and the one told what came of the answer: {}",
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

    let said = hosted.said().await;
    assert!(said.contains("ada@example.test") && !said.contains(PASSWORD));
    let saved = hosted.record_bytes().await;
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
    let reference = the_password_goes_from_the_persons_device_to_their_vault(&hosted).await;
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
        hosted.questions().await.is_empty(),
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
async fn a_reference_the_vault_does_not_hold_is_refused_by_the_use_and_nobody_is_asked() {
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

/// Admit the person of `device` to `workspace` as an editor: the server issues them an
/// invitation and their device redeems it.
async fn admit(hosted: &Hosted, workspace: &str, device: &crate::support::Device) {
    crate::workspace_actions::resource_support::admit_with(
        &hosted.workspaces,
        hosted.workspaces.governor(),
        workspace,
        device,
        tilo_peer_state::join::Role::Editor,
        "Bo",
    )
    .await;
}

/// What another workspace keeps is not hers to use. Bo, who keeps a workspace of his own on the
/// same server, keeps a password of his own in its vault, for the very servers her assistant's
/// call names. The reference of his item reaches the action as it was written, and the vault of
/// the workspace the turn acts in answers: it holds no such item. Nobody is asked, nothing signs
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

    // Another person, with a workspace of his own on the same server, which his item is kept in.
    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let his_device = enrol(&bo, "bo-browser", DeviceCapabilities::member());
    let elsewhere = hosted
        .workspaces
        .create("Elsewhere")
        .await
        .unwrap()
        .workspace;
    admit(&hosted, &elsewhere, &his_device).await;
    // A member joins a workspace from a page of their own: only a device of the server's own
    // person asks the server to govern a workspace first.
    let peer = Browser::session(&hosted.server, &his_device, &hosted.served_by);
    let events = peer.events();
    let joined = tilo_server_protocol::WorkspaceId::parse(&elsewhere).expect("the workspace's id");
    peer.send(tilo_session_client::Cmd::Join(joined))
        .expect("his page asks to join");
    crate::support::admitted_to(&peer, joined).await;
    let mut his_page = Browser::joined((peer, events), &his_device, &hosted.served_by, &elsewhere);
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
