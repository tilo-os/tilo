//! A person's own agent on a computer of theirs (`tilo login`, then
//! `tilo mcp`): its session says it runs an agent, as `tilo login` makes it
//! say, and from then on the host serves it as that agent acting for the
//! person, within an agent's bounds. What it lists is what it can run;
//! what people alone are offered it is not; what it writes is by the agent,
//! for the person; and an action that asks first goes to the person.
use super::agent_mcp::tool;
use super::*;
use serde_json::{Value, json};
use tilo_mcp::McpServer;
use tilo_session_actions::SessionActions;
use ui::tilo_resource::Resource as _;

const TASKS: &str = "so.tilo.tasks";

/// What a tool call answers, the answer whole: a listing's rows and what each
/// names, for a test that needs them.
async fn tool_typed(
    server: &McpServer<SessionActions>,
    name: &str,
    arguments: Value,
) -> (Value, bool) {
    let answer = server
        .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": name, "arguments": arguments,
                                  "_meta": {"so.tilo.mcp": {"structured": true}}}}))
        .await
        .expect("a call is answered");
    let failed = answer["result"]["isError"] == true || answer.get("error").is_some();
    (answer, failed)
}

/// The action ids a `tilo_ls` listing names (`#<action>` before its
/// parameters), in order.
fn listed_actions(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| line.split_once('#'))
        .map(|(_, rest)| {
            rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
        })
        .filter(|action| !action.is_empty())
        .collect()
}

/// The resources a `tilo_ls` listing names, by the name a path takes: a row
/// is `Issues  tasks.issues  [26 actions]`; the app's own head row
/// (`tasks  (so.tilo.tasks)`) and its actions name none.
fn listed_resources(listing: &str) -> Vec<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let columns: Vec<&str> = line
                .split("  ")
                .filter(|column| !column.is_empty())
                .collect();
            (columns.len() >= 2 && !columns[1].trim().starts_with('('))
                .then(|| columns[0].trim().trim_end_matches('/').to_owned())
        })
        .collect()
}

/// The resource a `tilo_ls tasks` listing names the issues by, as a path
/// takes it: the row whose kind is `tasks.issues` (`Issues  tasks.issues`).
fn issues_resource(listing: &str) -> String {
    listing
        .lines()
        .find_map(|line| {
            let mut columns = line.split("  ").map(str::trim).filter(|c| !c.is_empty());
            let name = columns.next()?;
            (columns.next() == Some("tasks.issues")).then(|| name.trim_end_matches('/').to_owned())
        })
        .unwrap_or_else(|| panic!("tasks lists where its issues are: {listing}"))
}

/// The agent "Claude Code" of the person whose root is `person` (hex): one agent on every
/// computer of theirs.
fn claude_code(person: &str) -> String {
    let root: [u8; 32] = hex::decode(person).unwrap().try_into().unwrap();
    tilo_actions::remote::device_agent_id(&root, "Claude Code")
}

/// Each Tasks record the page received (the tracker, and each part its issues are kept in),
/// every change it received loaded.
fn tasks_records(page: &Browser) -> Vec<automerge::AutoCommit> {
    page.documents
        .iter()
        .filter(|(route, _)| route.app_id == TASKS)
        .map(|(_, changes)| {
            let mut doc = automerge::AutoCommit::new();
            for (_, bytes) in changes {
                tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes)
                    .expect("a change the page received loads");
            }
            doc
        })
        .collect()
}

/// Who made each change to Tasks the page holds, from the action it carries, and in which run.
fn tasks_authors(page: &Browser) -> Vec<(tilo_actions::ActionInvoker, Option<String>)> {
    tasks_records(page)
        .into_iter()
        .flat_map(|mut doc| {
            doc.get_changes(&[])
                .iter()
                .filter_map(|change| change.message())
                .filter_map(tilo_actions::ActionContext::from_change_message)
                .filter_map(|context| Some((context.invoker?, context.group.map(|group| group.id))))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The work group each entry the agent made in Tasks carries, as the page holds them (a write
/// continuing the one before it, as a batch's later steps do, says it once).
fn tasks_groups(page: &Browser) -> Vec<Option<tilo_actions::WorkGroup>> {
    tasks_records(page)
        .into_iter()
        .flat_map(|mut doc| {
            // Read as History reads them: a change that continues the one before it
            // carries the entry it continues.
            let mut entries = tilo_actions::EntryReader::default();
            doc.get_changes(&[])
                .iter()
                .filter_map(|change| entries.read(change.actor_id(), change.message()))
                .filter(|context| {
                    context
                        .invoker
                        .as_ref()
                        .is_some_and(|invoker| invoker.acting.is_agent())
                })
                .map(|context| context.group)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The comments on the issue titled `title`, in order, in each Tasks record
/// the page received.
fn page_comments(page: &Browser, title: &str) -> Vec<Vec<String>> {
    tasks_records(page)
        .iter()
        .map(|doc| {
            tasks::issue_list::IssueList::read(doc).map_or_else(
                |_| Vec::new(),
                |list| {
                    list.issues
                        .values()
                        .filter(|issue| issue.title == title)
                        .flat_map(|issue| {
                            issue.comments.values().map(|message| message.text.clone())
                        })
                        .collect()
                },
            )
        })
        .collect()
}

/// Wait until a change to Tasks reaches `page` as the agent's, for `person`.
async fn until_the_agents_change(page: &mut Browser, person: &str) {
    page.until(|page| {
        let by = tasks_authors(page);
        if by.iter().any(|(invoker, _)| the_agent_for(invoker, person)) {
            return Ok(());
        }
        Err(format!(
            "no change to Tasks reached the page as the agent's, for the person: {by:?}; the page \
             holds {:?}, and could not open {:?}",
            page.documents
                .iter()
                .map(|(route, changes)| (route.app_id.clone(), changes.len()))
                .collect::<Vec<_>>(),
            page.unopened()
        ))
    })
    .await;
}

/// How the person's server runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Serves {
    /// The workspace's host, running its apps.
    Apps,
    /// The host with shells for terminals, and the person's laptop answering what it asks them
    /// about the agent.
    AppsAsking,
    /// As tilo.sh's server runs: records served, no agents.
    Records,
}

/// The server a person's workspace is on.
enum Running {
    Host(Box<Hosted>),
    Records(Box<tilo_server::daemon::Server>),
}

impl Running {
    fn server(&self) -> &tilo_server::daemon::Server {
        match self {
            Self::Host(hosted) => &hosted.server,
            Self::Records(server) => server,
        }
    }
}

/// A person's workspace on a server of theirs, and the computer their agent "Claude Code" runs
/// on (`tilo login`, then `tilo mcp`).
struct OwnAgent {
    root: ed25519_dalek::SigningKey,
    host: crate::support::Device,
    computer: crate::support::Device,
    /// The person, as the actions the agent runs for them name them.
    person: String,
    running: Running,
    _asked: Option<tokio::task::JoinHandle<()>>,
    _shells: Option<tempfile::TempDir>,
    _state: tempfile::TempDir,
}

impl OwnAgent {
    async fn start(serves: Serves) -> Self {
        let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let host = enrol(&root, "home-server", DeviceCapabilities::member());
        let computer = enrol(
            &root,
            "Claude Code on build-box",
            DeviceCapabilities::member(),
        );
        let state = crate::support::scratch_dir();
        let shells = (serves == Serves::AppsAsking).then(|| tempfile::tempdir().unwrap());
        let running = match serves {
            Serves::Records => Running::Records(Box::new(
                crate::harness::hosted::serving_documents(
                    &root,
                    &host,
                    state.path(),
                    crate::common::catalogue,
                )
                .await,
            )),
            Serves::Apps | Serves::AppsAsking => {
                let options = Options {
                    shell: shells.as_ref().map(|shells| {
                        tilo_server::agents::host_shells(shells.path().join("shells")).unwrap()
                    }),
                    ..Options::offering(crate::common::catalogue)
                };
                Running::Host(Box::new(
                    crate::harness::hosted::start(&root, &host, state.path(), options).await,
                ))
            }
        };
        // The person's laptop answers what the host asks them about the agent.
        let asked = if serves == Serves::AppsAsking {
            let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
            Some(
                crate::harness::hosted::Laptop::new(
                    ["Claude Code".to_owned()],
                    crate::harness::hosted::Unlisted::Deny,
                )
                .spawn(
                    running.server(),
                    &host,
                    root.clone(),
                    &laptop,
                    &crate::support::WORKSPACE,
                )
                .await,
            )
        } else {
            None
        };
        Self {
            person: hex::encode(root.verifying_key().to_bytes()),
            root,
            host,
            computer,
            running,
            _asked: asked,
            _shells: shells,
            _state: state,
        }
    }

    fn server(&self) -> &tilo_server::daemon::Server {
        self.running.server()
    }

    /// A tab of the person's, open on their workspace.
    async fn tab(&self) -> Browser {
        let device = enrol(&self.root, "browser", DeviceCapabilities::member());
        Browser::open_at(self.server(), &device, &self.host, WORKSPACE).await
    }

    /// The agent's computer, entered as a device of the person's, its session saying, as `tilo
    /// login` makes it say, that it runs the agent.
    async fn agent(&self) -> Browser {
        let agent = Browser::open_at(self.server(), &self.computer, &self.host, WORKSPACE).await;
        tilo_session_client::device::session_as_agent(&agent.peer, "Claude Code")
            .await
            .expect("the session is named as the agent");
        agent
    }

    /// What the agent asks the host over `agent`'s session.
    fn actions(&self, agent: &Browser) -> SessionActions {
        SessionActions::new(
            &agent.peer,
            agent.workspace,
            self.host.cert.device_public_key,
        )
    }

    /// `tilo mcp` on the agent's computer, asking over `actions`.
    fn mcp(&self, actions: &SessionActions) -> McpServer<SessionActions> {
        McpServer::new(actions.clone(), WORKSPACE.into(), self.person.clone())
    }

    /// One agent connection per `runs`, each a run of its own over the one session `agent`
    /// holds, as `tilo mcp` serves them.
    fn runs(&self, agent: &Browser, runs: &[&str]) -> Vec<McpServer<SessionActions>> {
        let actions = self.actions(agent);
        runs.iter()
            .map(|run| self.mcp(&actions).with_agent_session(*run))
            .collect()
    }
}

/// Whether `invoker` is the person's agent "Claude Code", acting for them.
fn the_agent_for(invoker: &tilo_actions::ActionInvoker, person: &str) -> bool {
    invoker.acting.is_agent()
        && invoker.acting.agent_id() == Some(claude_code(person).as_str())
        && invoker.acting.acted_for_person() == Some(person)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_own_agent_runs_what_it_lists_as_an_agent_and_asks_them_first() {
    let own = OwnAgent::start(Serves::AppsAsking).await;
    let person = own.person.as_str();
    let mut browser = own.tab().await;
    let tracker = browser.create(TASKS);
    let books = browser.create(EBOOK);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    // The tracker keeps its issues in the parts of its chain.
    browser.follow_chain(TASKS, tasks::issue_list::chain().chain_head().as_str());
    let agent = own.agent().await;
    let actions = own.actions(&agent);
    let server = own.mcp(&actions).with_agent_session("session-3");

    // Terminal: what it lists, it runs, as listed; a session first, then
    // that session's own listing.
    let (ran, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "terminal#run", "args": {"command": "echo first"}}),
    )
    .await;
    assert!(!failed && ran.contains("first"), "{ran}");
    let (terminal, failed) = tool(&server, "tilo_ls", json!({"path": "terminal"})).await;
    assert!(!failed, "{terminal}");
    let (session, failed) = tool(
        &server,
        "tilo_ls",
        json!({"path": "terminal/Terminal session"}),
    )
    .await;
    assert!(!failed, "{session}");
    assert!(
        listed_actions(&session)
            .iter()
            .any(|action| action == "run"),
        "{session}"
    );
    // As listed, on the session, and as the app's own `run`.
    for path in ["terminal/Terminal session#run", "terminal#run"] {
        let (again, failed) = tool(
            &server,
            "tilo_do",
            json!({"path": path, "args": {"command": "echo listed"}}),
        )
        .await;
        assert!(
            !failed && again.contains("listed"),
            "{path} as listed: {again}"
        );
    }

    // Tasks: what people alone are offered (planning an issue) is not
    // listed to the agent, asked over its own session.
    let caller = tilo_actions::remote::Caller::local_person("unused");
    let offered = tilo_actions::remote::ActionProvider::query(
        &actions,
        &caller,
        CatalogueQuery {
            provider: Some(resource_provider(&tracker)),
            max_results: 500,
            ..CatalogueQuery::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let listed: Vec<_> = offered
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .collect();
    assert!(
        !listed.is_empty(),
        "the agent is listed the tracker's actions"
    );
    assert!(
        listed.iter().all(|action| action.contract.reaches_agents()),
        "nothing for people alone is listed to an agent: {:?}",
        listed
            .iter()
            .filter(|action| !action.contract.reaches_agents())
            .map(|action| &action.address)
            .collect::<Vec<_>>()
    );

    // Tasks: every action it lists on the tracker is one the host carries out
    // for it, and the new issue it makes is by the agent, for the person. The
    // issues are kept in parts beside the tracker, listed as `Issues`.
    let (listing, failed) = tool(&server, "tilo_ls", json!({"path": "tasks"})).await;
    assert!(!failed, "{listing}");
    assert!(
        listed_actions(&listing)
            .iter()
            .any(|action| action == "create"),
        "{listing}"
    );
    let (made, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "tasks#create",
               "args": {"title": "Launch plan", "body": "Ship it."}}),
    )
    .await;
    assert!(!failed, "tasks#create as listed: {made}");
    // The issue is written to a part of the issues, a document of Tasks beside the tracker.
    until_the_agents_change(&mut browser, person).await;

    // Removing a book asks first: the agent's own word does not do, and the
    // person, asked on their laptop, allows it.
    let books_provider = resource_provider(&books);
    let remove = || {
        request(
            &books_provider,
            ActionAddress::new("", "remove-from-library"),
            InvokeArgument::Json(json!({"book": "book:dusk"}).as_object().unwrap().clone()),
        )
    };
    let refused = tilo_actions::remote::ActionProvider::invoke(
        &actions,
        &caller,
        InvokeRequest {
            confirmation: Some(Confirmation::Confirmed),
            ..remove()
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("person")),
        "an agent confirms nothing by its own word: {refused:?}"
    );
    let forged = tilo_actions::remote::ActionProvider::invoke(
        &actions,
        &caller,
        InvokeRequest {
            confirmation: Some(Confirmation::Approved("x".into())),
            ..remove()
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(&forged, Err(ActionCallError::Refused { .. })),
        "a token the host did not issue confirms nothing: {forged:?}"
    );
    let asked = remove();
    let accepted = tilo_actions::remote::ActionProvider::invoke(&actions, &caller, asked.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(accepted.status, ReceiptStatus::Accepted),
        "it waits for the person: {accepted:?}"
    );
    // Asked again under its id once it moved, the host answers how it stands.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(moved) =
            tilo_actions::remote::ActionProvider::next_change(&actions, &caller, &asked.invocation)
        {
            tilo_test_env::within("the person's answer moves the invocation", moved).await;
        }
        let now = tilo_actions::remote::ActionProvider::invoke(&actions, &caller, asked.clone())
            .await
            .unwrap();
        match now {
            Ok(receipt) if matches!(receipt.status, ReceiptStatus::Completed { .. }) => break,
            Ok(receipt) if matches!(receipt.status, ReceiptStatus::Accepted) => {}
            other => panic!("the person allowed it, yet it did not run: {other:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "the person's answer never ran it"
        );
    }
}

/// The agent as the workspace's listing leads it, on an app nobody started:
/// it starts the app as the listing says to (and asking again once it runs
/// still answers), comments on an issue with one batched call and then one
/// more, reads the issue back, and everything it wrote is one run of the
/// agent's, the connection's, which History lists with its changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_own_agent_lists_a_stopped_app_into_running_writes_and_reads_as_one_run() {
    let own = OwnAgent::start(Serves::Apps).await;
    let mut browser = own.tab().await;
    let agent = own.agent().await;
    let server = own
        .mcp(&own.actions(&agent))
        .with_agent_session("session-7");

    // Nobody started Tasks: listing it starts it, so the agent sees its issues and can act
    // on them without opening anything first.
    let (started, failed) = tool(&server, "tilo_ls", json!({"path": "tasks"})).await;
    assert!(
        !failed && !started.contains("not running"),
        "listing an installed app starts it: {started}"
    );
    // Opening it anyway still answers what is there.
    let (again, failed) = tool(&server, "tilo_do", json!({"path": "tasks#open"})).await;
    assert!(!failed, "tasks#open on running tasks: {again}");

    // An issue, found down the app's listing as an agent reads it.
    let (made, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "tasks#create",
               "args": {"title": "Launch notes", "body": "What the launch is about."}}),
    )
    .await;
    assert!(!failed, "tasks#create: {made}");
    let (listing, failed) = tool(&server, "tilo_ls", json!({"path": "tasks"})).await;
    assert!(!failed, "{listing}");
    let issues = issues_resource(&listing);
    let (rows, failed) = tool(
        &server,
        "tilo_ls",
        json!({"path": format!("tasks/{issues}")}),
    )
    .await;
    assert!(!failed, "{rows}");
    let issue = listed_resources(&rows)
        .into_iter()
        .find(|row| row.contains("Launch notes"))
        .unwrap_or_else(|| panic!("the tracker lists the issue: {rows}"));
    let issue_at = format!("tasks/{issues}/{issue}");
    // The listing names the issue by its id, which the comment it takes names.
    let (typed, failed) = tool_typed(
        &server,
        "tilo_ls",
        json!({"path": format!("tasks/{issues}")}),
    )
    .await;
    assert!(!failed, "{typed}");
    let issue_id = typed
        .pointer("/result/structuredContent/rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|row| row["name"].as_str() == Some(issue.as_str()))
        .and_then(|row| row["id"].as_str())
        .and_then(|id| id.rsplit('/').next())
        .unwrap_or_else(|| panic!("the tracker names the issue by its id: {typed}"))
        .to_owned();
    // What the issues list they run: an issue is made in one call, and an
    // issue takes comments, the write this test makes in a batch. The comment
    // is the tracker's item action, so the issues list it only by a call that
    // names the issue.
    let write = "tasks#comment";
    // The person's page follows the part the issue is kept in.
    browser.follow_chain(TASKS, tasks::issue_list::chain().chain_head().as_str());

    // One batched call writing three comments, then one more call.
    let lines = [
        "Launch notes, from Claude Code.",
        "Each line lands in the workspace as I write it.",
        "Following me puts this window in front of you.",
    ];
    let (batched, failed) = tool(
        &server,
        "tilo_do",
        json!({"calls": lines
            .iter()
            .map(|text| json!({"path": write,
                               "args": {"issue": issue_id.as_str(), "text": text}}))
            .collect::<Vec<_>>()}),
    )
    .await;
    assert!(!failed, "the batched write: {batched}");
    let (single, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": write,
               "args": {"issue": issue_id.as_str(), "text": "A second invocation, a moment later."}}),
    )
    .await;
    assert!(!failed, "the single write: {single}");
    let (read, failed) = tool(&server, "tilo_read", json!({"path": issue_at})).await;
    assert!(!failed, "the issue read back: {read}");
    let sent: Vec<&str> = lines
        .iter()
        .copied()
        .chain(["A second invocation, a moment later."])
        .collect();
    for line in &sent {
        assert!(read.contains(line), "the issue reads {line:?}: {read}");
    }
    // What the host answered `ok` reaches the person's page too, every line
    // in the order it was written: an answer is a promise about the record,
    // not about the host's own copy of it.
    browser
        .until(|browser| {
            let seen = page_comments(browser, "Launch notes");
            seen.iter()
                .any(|texts| texts == &sent)
                .then_some(())
                .ok_or_else(|| {
                    format!("the page never read the issue as written ({sent:?}): {seen:?}")
                })
        })
        .await;

    // Every change it made is the connection's run, as History groups an
    // agent's runs.
    let groups = browser
        .until(|browser| {
            let groups = tasks_groups(browser);
            if groups.len() >= 3 {
                Ok(groups)
            } else {
                Err(format!("the agent's writes never arrived: {groups:?}"))
            }
        })
        .await;
    assert!(
        groups
            .iter()
            .all(|group| group.as_ref().is_some_and(|group| {
                group.kind == "agent-session"
                    && group.id.starts_with("agent-session:")
                    && group.label.as_deref() == Some("Claude Code")
            })),
        "every change the agent made is its run: {groups:?}"
    );
}

/// Each run opens Tasks and makes an issue: both succeed, whichever run asks.
async fn open_and_write(runs: &[McpServer<SessionActions>]) -> Vec<String> {
    let mut titles = Vec::new();
    for (index, server) in runs.iter().enumerate() {
        let (opened, failed) = tool(server, "tilo_do", json!({"path": "tasks#open"})).await;
        assert!(!failed, "tasks#open from run {index}: {opened}");
        let title = format!("From the agent's run {index}");
        let (made, failed) = tool(
            server,
            "tilo_do",
            json!({"path": "tasks#create",
                   "args": {"title": title, "body": "A line of this issue."}}),
        )
        .await;
        assert!(!failed, "tasks#create from run {index}: {made}");
        titles.push(title);
    }
    titles
}

/// What the tracker lists, as `server`'s agent reads the issues.
async fn tracker_issues(server: &McpServer<SessionActions>) -> String {
    let (listing, failed) = tool(server, "tilo_ls", json!({"path": "tasks"})).await;
    assert!(!failed, "{listing}");
    let issues = issues_resource(&listing);
    let (rows, failed) = tool(
        server,
        "tilo_ls",
        json!({"path": format!("tasks/{issues}")}),
    )
    .await;
    assert!(!failed, "{rows}");
    rows
}

/// With a tab of the person's open on their workspace, two agent connections
/// on their logged-in computer each open Tasks and make an issue, and the tab
/// sees the issues arrive as the agent's, for the person.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logged_in_computers_agents_act_while_the_persons_tab_is_open() {
    let own = OwnAgent::start(Serves::Records).await;
    let person = own.person.as_str();
    let mut tab = own.tab().await;
    let agent = own.agent().await;
    let runs = own.runs(&agent, &["run-a", "run-b"]);
    let titles = open_and_write(&runs).await;
    let rows = tracker_issues(&runs[0]).await;
    for title in &titles {
        assert!(
            rows.contains(title.as_str()),
            "the tracker lists {title:?}: {rows}"
        );
    }

    until_the_agents_change(&mut tab, person).await;
}

/// With no tab of the person's open (it entered the workspace once, then
/// closed), their logged-in computer's agents still open Tasks and make
/// issues: the host runs the apps, and the issues are there to read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logged_in_computers_agents_act_with_no_tab_open() {
    let own = OwnAgent::start(Serves::Records).await;
    // The person's tab made the workspace, and is gone.
    let tab = own.tab().await;
    tab.peer.close();
    drop(tab);
    let agent = own.agent().await;
    let runs = own.runs(&agent, &["run-a", "run-b"]);
    let titles = open_and_write(&runs).await;

    // Each issue is in the tracker, as the other run reads it.
    let rows = tracker_issues(&runs[1]).await;
    for title in &titles {
        assert!(
            rows.contains(title.as_str()),
            "the tracker lists {title:?}: {rows}"
        );
    }
}

/// How many of the changes to Tasks the tab holds are the agent's, for
/// `person`, in a run of its own.
fn agents_changes(tab: &Browser, person: &str) -> usize {
    tasks_authors(tab)
        .iter()
        .filter(|(invoker, run)| {
            the_agent_for(invoker, person)
                && run
                    .as_ref()
                    .is_some_and(|run| run.starts_with("agent-session:"))
        })
        .count()
}

/// The agent makes an issue titled `title` on `server`, and the person's tab
/// sees it arrive as the agent's, in its run, for the person; no change it
/// makes is the person's own.
async fn agents_issue_reaches(
    tab: &mut Browser,
    server: &McpServer<SessionActions>,
    person: &str,
    title: &str,
) {
    tab.pump();
    let before = agents_changes(tab, person);
    let (made, failed) = tool(
        server,
        "tilo_do",
        json!({"path": "tasks#create", "args": {"title": title}}),
    )
    .await;
    assert!(!failed, "tasks#create: {made}");
    // The issue is kept in a part beside the tracker, which the tab's window
    // follows.
    tab.follow_chain(TASKS, tasks::issue_list::chain().chain_head().as_str());
    tab.until(|tab| {
        (agents_changes(tab, person) > before)
            .then_some(())
            .ok_or_else(|| {
                format!(
                    "{title:?} never arrived as the agent's run: {:?}",
                    tasks_authors(tab)
                )
            })
    })
    .await;
    let persons: Vec<_> = tasks_authors(tab)
        .into_iter()
        .filter(|(invoker, _)| !invoker.acting.is_agent())
        .collect();
    assert!(
        persons.is_empty(),
        "a change of the agent's was the person's: {persons:?}"
    );
}

/// `tilo login` checks the tools it added by starting `tilo mcp`, which
/// starts the peer: its session enters the workspace first. The login then
/// enters as the same device to show the person's tab the computer is in,
/// which cuts the peer's session off until it comes back, and closes. The
/// agent's first call after the login, on the peer's session, is the
/// agent's, in a run, for the person.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_call_after_tilo_login_is_the_agents() {
    let own = OwnAgent::start(Serves::Records).await;
    let person = own.person.as_str();
    let mut tab = own.tab().await;
    // The peer, as the login's check of its tools starts it.
    let peer = own.agent().await;
    // The login's own entry as the same device, closed once the tab saw it.
    let login = own.agent().await;
    login.peer.close();
    drop(login);
    let runs = own.runs(&peer, &["run-a"]);
    // The agent's first call, made while the peer is still finding its way
    // back into the workspace: it waits for the connection, and nothing lists
    // the workspace first.
    let (opened, failed) = tool(&runs[0], "tilo_do", json!({"path": "tasks#open"})).await;
    assert!(!failed, "the first call after the login: {opened}");
    agents_issue_reaches(&mut tab, &runs[0], person, "The first issue").await;

    // Whatever else drops the peer's connection, it comes back as the agent's run.
    peer.peer.send(tilo_session_client::Cmd::Reconnect).unwrap();
    peer.peer
        .until(tilo_test_env::hang_bound(), |status| {
            (status.phase == tilo_session_client::TransportPhase::Connected
                && status.workspaces.values().all(|ready| *ready))
            .then_some(())
        })
        .await
        .expect("the peer comes back");
    agents_issue_reaches(&mut tab, &runs[0], person, "After the connection came back").await;
}

/// An agent on a logged-in computer whose harness named its session ("Release prep") goes by
/// that name on the workspace's server too: every change it makes is labelled and named by the
/// session, not by the agent's product, whatever the server knows the agent's member as.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logged_in_agents_session_name_reaches_its_server() {
    let own = OwnAgent::start(Serves::Apps).await;
    let mut browser = own.tab().await;
    let agent = own.agent().await;
    let mcp = own.mcp(&own.actions(&agent));
    browser.follow_chain(TASKS, tasks::issue_list::chain().chain_head().as_str());
    // What `tilo mcp` writes into the client's initialize when the harness named the session.
    let opened = mcp
        .handle(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"experimental": {"so.tilo": {"session-name": "Release prep"}}},
            "clientInfo": {"name": "claude-code", "version": "2"}}}),
        )
        .await
        .expect("initialize is answered");
    assert!(opened.get("result").is_some(), "initialize: {opened}");
    let (made, failed) = tool(
        &mcp,
        "tilo_do",
        json!({"path": "tasks#create", "args": {"title": "Release notes"}}),
    )
    .await;
    assert!(!failed, "tasks#create: {made}");

    let groups = browser
        .until(|browser| {
            let groups = tasks_groups(browser);
            (!groups.is_empty())
                .then_some(groups)
                .ok_or_else(|| "the agent's issue never arrived".to_owned())
        })
        .await;
    assert!(
        groups
            .iter()
            .all(|group| group.as_ref().is_some_and(|group| {
                group.kind == "agent-session"
                    && group.session.as_deref() == Some("Release prep")
                    && group.label.as_deref() == Some("Release prep")
            })),
        "the agent's invocation goes by its session's name: {groups:?}"
    );
    // Among the people here it goes by that name too, as its windows do.
    browser
        .until(|browser| {
            let agents: Vec<String> = browser
                .here()
                .into_iter()
                .filter(|member| member.agent)
                .map(|member| member.label)
                .collect();
            agents
                .iter()
                .any(|label| label == "Release prep")
                .then_some(())
                .ok_or_else(|| format!("the agent is never here as its session: {agents:?}"))
        })
        .await;
}

/// A person's own agent opens the workspace's Terminal session on a terminal, as `run` is
/// asked: what is typed there on its computer is recorded on the session by the host, as the
/// commands `run` runs are, once each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_typed_on_a_terminal_the_agent_opened_is_recorded_on_the_session() {
    Box::pin(typed_on_a_terminal_is_recorded()).await;
}

async fn typed_on_a_terminal_is_recorded() {
    use ush::environment_terminals::{FromTerminal, ToTerminal, quic, wire};
    let own = OwnAgent::start(Serves::AppsAsking).await;
    let agent = own.agent().await;
    let actions = own.actions(&agent);
    let server = own.mcp(&actions).with_agent_session("session-7");

    let (opened, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "terminal#open-terminal", "args": {"cols": 100, "rows": 30}}),
    )
    .await;
    assert!(!failed, "{opened}");
    let after = opened.split_once("stream").map_or("", |(_, after)| after);
    let stream: String = after
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|part| part.len() == 32)
        .unwrap_or_else(|| panic!("a stream: {opened}"))
        .to_owned();

    // The agent's computer takes the stream as itself and types a command.
    let endpoint =
        test_server::endpoint(iroh::SecretKey::from_bytes(&own.computer.key.to_bytes())).await;
    let address = own.server().node_addr();
    let typed = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Handle::current();
            let (connection, mut to, mut from) = quic::dial(&endpoint, address, &runtime).unwrap();
            wire::write_hello(&mut to, &wire::Hello { stream }).unwrap();
            let welcome = wire::read_welcome(&mut from).unwrap();
            assert!(
                matches!(welcome, Some(wire::Welcome::Opened { .. })),
                "{welcome:?}"
            );
            let mut drawn = Vec::new();
            let mut typed = false;
            loop {
                let text = String::from_utf8_lossy(&drawn).into_owned();
                if text.contains("typed-42") {
                    break;
                }
                // The shell's prompt (it ends `$ `, or `# ` for root) says it reads a line.
                if !typed && (text.contains("$ ") || text.contains("# ")) {
                    let line = b"echo typed-$((40+2))\r".to_vec();
                    wire::write_to(&mut to, &ToTerminal::In(line)).unwrap();
                    typed = true;
                }
                match wire::read_from(&mut from).unwrap() {
                    Some(FromTerminal::Out(bytes)) => drawn.extend(bytes),
                    other => panic!("{other:?}: {text:?}"),
                }
            }
            drop(connection);
        }),
    )
    .await;
    typed.expect("the terminal answers in time").unwrap();

    // The host recorded it on the session, by the agent, once: read again each time the
    // workspace's records change.
    let watching = tilo_actions::remote::Caller::local_person("unused");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let changed = tilo_actions::remote::ActionProvider::changed(
            &actions,
            &watching,
            tilo_actions::remote::WatchScope::Records,
        )
        .expect("the session follows the workspace's records");
        let (session, failed) = tool(
            &server,
            "tilo_read",
            json!({"path": "terminal/Terminal session"}),
        )
        .await;
        assert!(!failed, "{session}");
        let recorded = session.matches("echo typed-$((40+2))").count();
        if recorded > 0 {
            assert_eq!(recorded, 1, "recorded once: {session}");
            assert!(session.contains(r#""exit":0"#), "as it ended: {session}");
            assert!(
                session.contains(r#""actor":{"agent""#),
                "by the agent: {session}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never recorded: {session}"
        );
        // A wait the session refuses says why at once; the deadline only backs that up.
        if let Ok(Some(Err(refused))) =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed).await
        {
            panic!("the session refused to follow the workspace's records: {refused}");
        }
    }
}
