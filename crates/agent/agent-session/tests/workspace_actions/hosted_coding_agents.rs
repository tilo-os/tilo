//! A coding agent on the server, through the joined path: a sub-agent whose work a coding
//! program does in the workspace's machine, with no device of its person doing anything.
//!
//! The machine is this computer with the real launcher, and the coding program a scripted
//! stand-in that behaves as Claude Code does at its edges (`coding_agents::testing`, beside the
//! server's own keeper of coding agents). Everything else is what a person's server does: the
//! workspace's list of its agents, Chat's conversations, the server's own look at them, the
//! workspace's own model behind its settings and its vault, and Tilo's own MCP server for
//! the turn that is going.
use super::*;
use tilo_actions::remote::person_asking::{ActionName, Asked, Words};
use tilo_server::{
    coding_agents::{
        harness::Start,
        runner::Machines,
        testing::{Local, seen_of},
    },
    workspace_agents::ServerAgents as Agents,
    workspaces::{
        person_asking::Allowing,
        person_model::{
            ProviderProtocol,
            scripted::{Linking, Reply, ScriptedProvider},
        },
    },
};
use ui::tilo_workspace_agent::Sub;
use ui::tilo_workspace_agent::coding::{Coding, NewCoding};

/// The person's key at their provider. Nothing in the machine may ever hold it, and nothing
/// the workspace keeps.
const KEY: &str = "sk-ada-MARKER-KEY-THAT-STAYS-ON-THE-SERVER";
/// What of the key is looked for.
const MARK: &str = "MARKER-KEY-THAT-STAYS";
/// The repository among the workspace's files that the coding agents work in.
const REPOSITORY: &str = "code/hello";
/// How a coding agent's answer begins while its person has not said it may start.
const NOT_STARTED: &str = "I have not started.";
/// What a coding agent is started for in these tests.
const JOB: &str = "Add a line to README that says hello.";

/// A test of a coding agent, on both kinds of server: under its own name where the server
/// starts its workspaces' programs itself, and with `_where_its_machine_is_placed` where the
/// workspace's machine is on another computer and the program is asked of that computer's
/// member device ([`Local::placed`]). Every guard holds on both, so every test goes on both.
macro_rules! on_both_kinds {
    ($here:ident, $placed:ident, |$machine:ident| $body:block) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $here() {
            let $machine = Local::new();
            $body
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $placed() {
            let $machine = Local::placed();
            $body
        }
    };
}

/// The workspace's own model as it linked it: a scripted provider behind the workspace's
/// settings, with [`KEY`] in its vault. It answers `replies` in order, whoever asks: the
/// workspace's own agent and the coding program alike.
async fn links_model(
    workspaces: &Workspaces,
    workspace: &str,
    replies: impl IntoIterator<Item = Reply>,
) -> Arc<ScriptedProvider> {
    let theirs = ScriptedProvider::answering(replies);
    workspaces.answer_models_with(theirs.clone());
    links_model_in(workspaces, workspace).await;
    theirs
}

/// [`links_model`]'s provider kept in `workspace` alone, for a workspace whose answers come from
/// the provider another one registered: each workspace keeps a model of its own to ask.
async fn links_model_in(workspaces: &Workspaces, workspace: &str) {
    workspaces
        .link_model_for(
            workspace,
            &Linking {
                name: "Anthropic",
                protocol: ProviderProtocol::Anthropic,
                endpoint: "https://api.anthropic.com",
                key: Some(KEY),
                models: &["claude-parts"],
            },
            true,
        )
        .await
        .unwrap();
}

/// The agents the server works, with coding agents worked on this computer by the scripted
/// stand-in; and what the stand-in is started for, each time.
async fn coding_on(
    served: &crate::harness::hosted::Hosted,
    workspaces: &[&str],
    machine: &Arc<Local>,
) -> (Agents, Arc<Mutex<Vec<Start>>>) {
    machine.repository(REPOSITORY);
    let (harnesses, starts) = machine.stand_in();
    let machines: Arc<dyn Machines> = machine.clone();
    for workspace in &workspaces[1..] {
        served.approving(workspace, &[SCOUT]).await;
    }
    let agents = served
        .approving(workspaces[0], &[SCOUT])
        .await
        .with_coding_by(machines, harnesses);
    (agents, starts)
}

/// The conversation of `person` and `agent`.
fn direct(person: &str, agent: &str) -> Id {
    chat::Chat::direct_id([MemberId::person(person.to_owned()), MemberId::agent(agent)])
}

/// The person's own agent starts a coding agent named `name` for `job`, to work in
/// [`REPOSITORY`] by the stand-in.
async fn starts_coder(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    (name, job): (&str, &str),
) {
    let sub_agent = NewSubAgent {
        name: name.to_owned(),
        job: job.to_owned(),
        role: Some("Coder".to_owned()),
        coding: Some(NewCoding {
            harness: Some("stand-in".to_owned()),
            repository: REPOSITORY.to_owned(),
            ..NewCoding::default()
        }),
        ..NewSubAgent::default()
    };
    on_roster(
        workspaces,
        workspace,
        (Some(SCOUT), person),
        WorkspaceAgents::START_SUB_AGENT.with(workspace_agents::StartSubAgent { sub_agent }),
    )
    .await
    .expect("the person's own agent starts a coding agent");
}

/// The coding sub-agent `agent` once its item holds what `ready` looks for: what makes it a
/// sub-agent, and its work in the machine.
async fn item_when(
    workspaces: &Workspaces,
    workspace: &str,
    (agent, what): (&str, &str),
    ready: impl Fn(&Sub, &Coding) -> bool,
) -> (Sub, Coding) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let sub = roster(workspaces, workspace)
            .await
            .get(&Id::from_raw(agent))
            .and_then(|found| found.sub.clone());
        if let Some(sub) = &sub
            && let Some(coding) = &sub.coding
            && ready(sub, coding)
        {
            return (sub.clone(), coding.clone());
        }
        assert!(
            Instant::now() < deadline,
            "{agent}'s item never said {what}: {sub:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// What the person's own agent was told in its thread once that is `count` pieces of news,
/// each with the line it answered there.
async fn starter_told(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    count: usize,
) -> Vec<(Kept, Kept)> {
    let thread = direct(person, SCOUT);
    let news = caused(workspaces, workspace, (thread.as_str(), "news"), count).await;
    let mut told = Vec::new();
    for (id, said) in news {
        let line = reply_of(workspaces, workspace, &id, SCOUT).await;
        told.push((said, line));
    }
    told
}

/// Start the coding agent `name` (its id `id`) for [`JOB`] and see it through its job alone:
/// the server writes the job into its conversation, its turn there answers that it has not
/// started, and its starter, told so, says its line in the main thread, which is one call of
/// the person's model. `told` is how many pieces of news the main thread then holds. The job's
/// message.
async fn started_and_waiting(
    agents: &Agents,
    workspaces: &Workspaces,
    (workspace, person): (&str, &str),
    (name, id): (&str, &str),
    told: usize,
) -> Id {
    starts_coder(workspaces, workspace, person, (name, JOB)).await;
    let own = direct(person, id);
    agents.look(workspace).await.unwrap();
    let jobs = caused(workspaces, workspace, (own.as_str(), "job"), 1).await;
    let (job, written) = &jobs[0];
    assert_eq!(written.text, JOB);
    assert_eq!(
        written.author,
        MemberId::agent(SCOUT),
        "the job is its starter's own words"
    );
    agents.look(workspace).await.unwrap();
    let waits = reply_of(workspaces, workspace, job, id).await;
    assert!(
        waits.text.starts_with(NOT_STARTED),
        "its job alone starts nothing, and it says so: {}",
        waits.text
    );
    stands_as(workspaces, workspace, id, Stands::NeedsPerson).await;
    starter_told(workspaces, workspace, person, told).await;
    job.clone()
}

/// What `agent` wrote into `conversation` in its own name, as messages of its own.
async fn said_by(
    workspaces: &Workspaces,
    workspace: &str,
    (conversation, agent): (&Id, &str),
) -> Vec<String> {
    said_there(workspaces, workspace, conversation.as_str())
        .await
        .into_iter()
        .filter(|(_, message)| message.author == MemberId::agent(agent))
        .map(|(_, message)| message.text)
        .collect()
}

/// What `agent` spent on its day, as the workspace's list of its agents counts it.
async fn spent_by(workspaces: &Workspaces, workspace: &str, agent: &str) -> Option<u64> {
    roster(workspaces, workspace)
        .await
        .get(&Id::from_raw(agent))
        .and_then(|agent| agent.spent.clone())
        .map(|spent| spent.cost_micros)
}

/// Every file under `folder`, as text.
fn read_under(folder: &Path, into: &mut Vec<String>) {
    for entry in std::fs::read_dir(folder).unwrap() {
        let at = entry.unwrap().path();
        if at.is_dir() {
            read_under(&at, into);
        } else if let Ok(bytes) = std::fs::read(&at) {
            into.push(String::from_utf8_lossy(&bytes).into_owned());
        }
    }
}

/// What a person lets stay when they let their own agent start coding agents in the
/// workspace's machine without asking them.
fn starting_unasked(workspace: &str) -> Allowing {
    Allowing {
        scope: Asked {
            workspace: workspace.to_owned(),
            agent: SCOUT.to_owned(),
            action: ActionName {
                provider: "tilo.coding".to_owned(),
                kind: "machine".to_owned(),
                action: "start".to_owned(),
            },
            on: None,
        },
        name: "Scout".to_owned(),
        words: Words {
            action: "Start coding agents in the machine".to_owned(),
            on: String::new(),
            place: "Team".to_owned(),
        },
        asked_by: None,
    }
}

// A coding agent's job alone starts nothing: it answers that it has not started, it stands
// as needing its person, nothing is started in the machine and nobody's model is asked for
// it. Where its person let their own agent start coding agents unasked, the job starts it;
// and once they took that back, the next job starts nothing again.
on_both_kinds! {
    a_job_alone_starts_nothing_unless_its_person_let_coding_agents_start_unasked,
    a_job_alone_starts_nothing_unless_its_person_let_coding_agents_start_unasked_where_its_machine_is_placed,
    |machine| {
    team!(root, browser, actions, workspaces, workspace, page, person, served);
    let key = root.verifying_key().to_bytes();
    let theirs = links_model(
        &workspaces,
        &workspace,
        [
            Reply::text("Fix hello waits for your word."),
            Reply::text("Hello, twice.").costing(700),
            Reply::text("Say twice is done."),
            Reply::text("Third one waits for your word."),
        ],
    ).await;
    let (agents, _) = coding_on(&served, &[&workspace], &machine).await;

    // Nothing stands for its person: the job alone starts nothing.
    started_and_waiting(
        &agents,
        &workspaces,
        (&workspace, &person),
        ("Fix hello", "fix-hello"),
        1,
    )
    .await;
    let (_, coding) = item_when(
        &workspaces,
        &workspace,
        ("fix-hello", "that it waits for its person"),
        |sub, _| sub.stands == Stands::NeedsPerson,
    )
    .await;
    assert_eq!(
        (
            coding.repository.as_str(),
            coding.directory.as_str(),
            coding.branch.as_str()
        ),
        (REPOSITORY, "code/hello.agents/fix-hello", "agent/fix-hello")
    );
    assert!(
        coding.process.is_none() && coding.session.is_empty(),
        "no process of it ever was: {coding:?}"
    );
    assert_eq!(machine.started(), 0, "nothing was started in the machine");
    let told = starter_told(&workspaces, &workspace, &person, 1).await;
    assert!(
        told[0].0.text.contains("needs its person") && told[0].0.text.contains(NOT_STARTED),
        "its starter is told that it waits: {}",
        told[0].0.text
    );
    assert_eq!(told[0].1.text, "Fix hello waits for your word.");
    assert_eq!(
        theirs.asked().len(),
        1,
        "the person's model was asked for its starter's line, and not for the coding agent"
    );

    // Its person lets their own agent start coding agents here without asking: kept in their
    // own store on the server, where an answer of always is kept.
    let allowed = workspaces
        .let_always(&key, starting_unasked(&workspace))
        .expect("the person's own store keeps what they let stay");
    starts_coder(
        &workspaces,
        &workspace,
        &person,
        ("Say twice", "Say hello twice."),
    )
    .await;
    let second = direct(&person, "say-twice");
    agents.look(&workspace).await.unwrap();
    let jobs = caused(&workspaces, &workspace, (second.as_str(), "job"), 1).await;
    agents.look(&workspace).await.unwrap();
    let done = reply_of(&workspaces, &workspace, &jobs[0].0, "say-twice").await;
    assert_eq!(done.text, "Hello, twice.", "its job started it");
    stands_as(&workspaces, &workspace, "say-twice", Stands::Done).await;
    assert_eq!(machine.started(), 1);
    let handed = machine.machines()[0].handed();
    assert_eq!(handed.len(), 1, "{handed:?}");
    assert!(
        handed[0].contains("\"text\":\"Say hello twice.\""),
        "its job is handed over in its own words and nothing beside them: {handed:?}"
    );
    let used = workspaces
        .permissions_of(&key)
        .permissions
        .into_iter()
        .find(|permission| permission.id == allowed.id)
        .map(|permission| permission.used_times);
    assert_eq!(used, Some(1), "the start it let is counted on it");
    starter_told(&workspaces, &workspace, &person, 2).await;

    // Taken back, it covers nothing from that moment: the next job waits again.
    assert!(
        workspaces
            .take_back_permission(&key, &allowed.id)
            .await
            .expect("their own store takes it back")
    );
    started_and_waiting(
        &agents,
        &workspaces,
        (&workspace, &person),
        ("Third one", "third-one"),
        3,
    )
    .await;
    assert_eq!(machine.started(), 1, "and nothing more was started");
    assert_eq!(theirs.asked().len(), 4);
    let _ = browser;
    page.peer.close();
    }
}

// Its person writes to it, and those words start it: the coding program is handed the job
// first and then their words, in one process, in a working copy and a branch of its own.
// Each thing it says is a message of its own in its conversation, its last words are the
// reply, its item says which coding program works and how, what it cost is counted on the
// day of the person's own agent, and that agent is told and says one line in the main thread.
on_both_kinds! {
    its_persons_words_start_it_with_its_job_and_what_it_says_is_relayed,
    its_persons_words_start_it_with_its_job_and_what_it_says_is_relayed_where_its_machine_is_placed,
    |machine| {
    team!(root, browser, actions, workspaces, workspace, page, person, served);
    let ada = themselves(&root, &browser, &workspace);
    let theirs = links_model(
        &workspaces,
        &workspace,
        [
            Reply::text("Fix hello waits for your word."),
            Reply::text("The README says hello now.").costing(1_200),
            Reply::text("Yes, one line.").costing(800),
            Reply::text("Fix hello is done: the README says hello."),
        ],
    ).await;
    let (agents, starts) = coding_on(&served, &[&workspace], &machine).await;
    let own = direct(&person, "fix-hello");
    let thread = direct(&person, SCOUT);
    started_and_waiting(
        &agents,
        &workspaces,
        (&workspace, &person),
        ("Fix hello", "fix-hello"),
        1,
    )
    .await;

    // Their own words to it are their yes to this start.
    let go = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "Go. Is it one line?"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let answered = reply_of(&workspaces, &workspace, &go, "fix-hello").await;
    assert_eq!(
        answered.text, "Yes, one line.",
        "the reply holds its last words"
    );
    assert_eq!(
        end_of(&answered),
        Some(chat::agent_stream::AgentFinish::Complete)
    );

    // One process, handed the job first and then their words.
    let machines = machine.machines();
    assert_eq!((machine.started(), machines.len()), (1, 1));
    let handed = machines[0].handed();
    assert_eq!(handed.len(), 2, "{handed:?}");
    assert!(
        handed[0].contains(JOB) && handed[1].contains("Go. Is it one line?"),
        "the job first, then what its person wrote: {handed:?}"
    );
    assert_eq!(
        starts.lock().unwrap().as_slice(),
        [Start::default()],
        "a first start takes up no earlier conversation"
    );
    assert!(
        machine
            .files()
            .join("code/hello.agents/fix-hello/.git")
            .exists(),
        "it works in a working copy of its own"
    );

    // What it said before its last words is a message of its own, in its own conversation.
    let said = said_by(&workspaces, &workspace, (&own, "fix-hello")).await;
    assert!(
        said.iter().any(|text| text == "The README says hello now."),
        "each thing it says is a message of its conversation: {said:?}"
    );
    assert!(
        said_by(&workspaces, &workspace, (&thread, "fix-hello"))
            .await
            .iter()
            .all(|text| text.contains("Your sub-agent")),
        "and nothing of it stands in the main thread but its news"
    );

    // Its item says how it stands, which coding program works, and how.
    let (sub, coding) = item_when(
        &workspaces,
        &workspace,
        ("fix-hello", "what it spent"),
        |sub, coding| sub.stands == Stands::Done && coding.spent_micros == 2_000,
    )
    .await;
    assert_eq!(sub.line, "Yes, one line.");
    assert_eq!(
        (coding.harness.as_str(), coding.harness_name.as_str()),
        ("stand-in", "Stand-in")
    );
    assert!(
        coding.session.starts_with("stand-in-"),
        "the coding program's own id for the conversation: {}",
        coding.session
    );
    let process = coding.process.expect("its process");
    assert!(
        process.started_ms > 0 && process.ended.is_empty() && process.stopped_by.is_empty(),
        "its process lives, ready for its next words: {process:?}"
    );

    // Its starter is told how the work ended, and says one line in the main thread.
    let told = starter_told(&workspaces, &workspace, &person, 2).await;
    assert!(
        told.iter().any(|(news, line)| {
            news.author == MemberId::agent("fix-hello")
                && news.text.contains("stands: done")
                && news.text.contains("Yes, one line.")
                && line.text == "Fix hello is done: the README says hello."
        }),
        "{told:?}"
    );

    // The person's own model answered, asked on the server with their key; and what the
    // coding program's calls cost is counted on the day of the person's own agent.
    let asked = theirs.asked();
    assert_eq!(asked.len(), 4);
    assert!(
        asked
            .iter()
            .all(|call| call.key.as_deref() == Some(KEY) && call.model == "claude-parts"),
        "{asked:?}"
    );
    assert!(
        serde_json::to_string(&asked[1].request)
            .unwrap()
            .contains(JOB),
        "the coding program asked the person's model about its job"
    );
    assert_eq!(spent_by(&workspaces, &workspace, SCOUT).await, Some(2_000));
    assert_eq!(
        spent_by(&workspaces, &workspace, "fix-hello").await,
        None,
        "a coding agent has no day of its own"
    );
    page.peer.close();
    }
}

// What its person writes while it works reaches the process that is working, not a second
// one, and the answer then stands under their newest words. A note of its starter reaches
// the same process afterwards.
on_both_kinds! {
    words_written_while_it_works_and_a_note_after_reach_the_same_process,
    words_written_while_it_works_and_a_note_after_reach_the_same_process_where_its_machine_is_placed,
    |machine| {
    team!(root, browser, actions, workspaces, workspace, page, person, served);
    let ada = themselves(&root, &browser, &workspace);
    let theirs = links_model(
        &workspaces,
        &workspace,
        [
            Reply::text("Fix hello waits for your word."),
            Reply::text("The job is looked at."),
            Reply::text("Goodbye."),
            Reply::text("Fix hello is done."),
            Reply::text("Greeted."),
            Reply::text("Fix hello greeted the reader."),
        ],
    ).await;
    let (agents, _) = coding_on(&served, &[&workspace], &machine).await;
    let own = direct(&person, "fix-hello");
    let copy = machine.files().join("code/hello.agents/fix-hello");
    started_and_waiting(
        &agents,
        &workspaces,
        (&workspace, &person),
        ("Fix hello", "fix-hello"),
        1,
    )
    .await;

    // It is at work on what its person wrote, and stays so until the test lets it go on.
    let first = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "wait for the word"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    until("the coding program is at work", || {
        copy.join("waiting").exists()
    })
    .await;

    // Its person writes again: the words are handed to the process that is working.
    let second = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "And say goodbye."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    until("their next words are handed to the process", || {
        machine.machines().first().is_some_and(|working| {
            working
                .handed()
                .iter()
                .any(|handed| handed.contains("And say goodbye."))
        })
    })
    .await;
    assert!(
        copy.join("waiting").exists(),
        "it was still at work when they reached it"
    );
    assert_eq!(machine.started(), 1, "no second process was started");
    std::fs::write(copy.join("go"), "").unwrap();

    // One turn: it went on with what they wrote, and its answer stands there.
    let answered = reply_of(&workspaces, &workspace, &second, "fix-hello").await;
    assert_eq!(answered.text, "Goodbye.");
    let left = reply_of(&workspaces, &workspace, &first, "fix-hello").await;
    assert!(
        left.text.contains("going on"),
        "the reply it left says the answer follows below: {}",
        left.text
    );
    let said = said_by(&workspaces, &workspace, (&own, "fix-hello")).await;
    assert!(
        said.iter().any(|text| text == "the word came"),
        "what it said to their first words is a message of its own: {said:?}"
    );
    let handed = machine.machines()[0].handed();
    assert_eq!(handed.len(), 3, "its job, and both messages: {handed:?}");
    starter_told(&workspaces, &workspace, &person, 2).await;

    // A note of its starter is written into its conversation and reaches the same process.
    on_roster(
        &workspaces,
        &workspace,
        (Some(SCOUT), person.as_str()),
        WorkspaceAgents::NOTE_SUB_AGENT.with(workspace_agents::NoteSubAgent {
            agent: Id::from_raw("fix-hello".to_owned()),
            text: NonBlank::parse("Also greet the reader.").unwrap(),
        }),
    )
    .await
    .expect("its starter gives it a note");
    agents.look(&workspace).await.unwrap();
    let notes = caused(&workspaces, &workspace, (own.as_str(), "note"), 1).await;
    agents.look(&workspace).await.unwrap();
    let greeted = reply_of(&workspaces, &workspace, &notes[0].0, "fix-hello").await;
    assert_eq!(greeted.text, "Greeted.");
    let handed = machine.machines()[0].handed();
    assert!(
        handed
            .last()
            .unwrap()
            .contains("A note from Scout: Also greet the reader."),
        "the note reaches it as its starter's: {handed:?}"
    );
    assert_eq!(
        (machine.started(), machine.machines().len()),
        (1, 1),
        "one process took everything"
    );
    starter_told(&workspaces, &workspace, &person, 3).await;
    assert_eq!(theirs.asked().len(), 6);
    page.peer.close();
    }
}

// A stop ends the coding program. Its person's stop on the message it works on, through
// Chat: its process ends, its reply says it was stopped, and its item says stopped, and by
// that person. Then new work, and the stop of the agent that started it, on its item: the
// process ends and its item says by that agent.
on_both_kinds! {
    its_person_stops_it_on_its_message_and_its_starter_on_its_item,
    its_person_stops_it_on_its_message_and_its_starter_on_its_item_where_its_machine_is_placed,
    |machine| {
    team!(root, browser, actions, workspaces, workspace, page, person, served);
    let ada = themselves(&root, &browser, &workspace);
    let theirs = links_model(
        &workspaces,
        &workspace,
        [
            Reply::text("Fix hello waits for your word."),
            Reply::text("The job is looked at."),
        ],
    ).await;
    let (agents, starts) = coding_on(&served, &[&workspace], &machine).await;
    let own = direct(&person, "fix-hello");
    let copy = machine.files().join("code/hello.agents/fix-hello");
    let lives = |coding: &Coding| {
        coding
            .process
            .as_ref()
            .map(|process| process.started_ms)
            .filter(|_| {
                coding
                    .process
                    .as_ref()
                    .is_some_and(|process| process.ended.is_empty())
            })
    };
    started_and_waiting(
        &agents,
        &workspaces,
        (&workspace, &person),
        ("Fix hello", "fix-hello"),
        1,
    )
    .await;

    // It is at work, and its item says a process of it lives.
    let first = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "wait for the word"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    until("the coding program is at work", || {
        copy.join("waiting").exists()
    })
    .await;
    let (_, working) = item_when(
        &workspaces,
        &workspace,
        ("fix-hello", "that its process lives"),
        |_, coding| lives(coding).is_some() && !coding.session.is_empty(),
    )
    .await;
    let began = lives(&working).unwrap();

    // Its person stops it on the message.
    stop_agent(&actions, &workspace, &ada, &first)
        .await
        .expect("its person stops it");
    agents.look(&workspace).await.unwrap();
    let stopped = reply_of(&workspaces, &workspace, &first, "fix-hello").await;
    assert_eq!(
        end_of(&stopped),
        Some(chat::agent_stream::AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        stopped.work
    );
    let (sub, coding) = item_when(
        &workspaces,
        &workspace,
        ("fix-hello", "that its process ended"),
        |sub, coding| {
            sub.stands == Stands::Stopped
                && coding
                    .process
                    .as_ref()
                    .is_some_and(|process| !process.ended.is_empty())
        },
    )
    .await;
    let ended = coding.process.expect("its process");
    assert_eq!(sub.stands, Stands::Stopped);
    assert_eq!(
        (
            ended.started_ms,
            ended.ended.as_str(),
            ended.stopped_by.as_str(),
            ended.exit
        ),
        (
            began,
            "stopped",
            format!("person:{person}").as_str(),
            Some(130)
        ),
        "the process ended, stopped by its person"
    );
    assert!(ended.ended_ms.is_some());

    // New work starts it again, taking its conversation up where it stood. A message's time
    // is kept in seconds, and a stop ends what stood when it was given: the new work is
    // written a second later.
    std::fs::remove_file(copy.join("waiting")).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "wait for the word once more"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    until("the coding program is at work again", || {
        copy.join("waiting").exists()
    })
    .await;
    assert_eq!(machine.started(), 2);
    assert_eq!(
        starts.lock().unwrap().last().unwrap().session,
        Some(working.session.clone()),
        "it takes its conversation up by the coding program's own id for it"
    );
    item_when(
        &workspaces,
        &workspace,
        ("fix-hello", "that a new process lives"),
        |sub, coding| {
            sub.stands == Stands::Working && lives(coding).is_some_and(|again| again != began)
        },
    )
    .await;

    // The agent that started it stops it, on its item.
    on_roster(
        &workspaces,
        &workspace,
        (Some(SCOUT), person.as_str()),
        stop("fix-hello"),
    )
    .await
    .expect("the agent that started it stops it");
    agents.look(&workspace).await.unwrap();
    let (_, coding) = item_when(
        &workspaces,
        &workspace,
        ("fix-hello", "that its second process ended"),
        |_, coding| {
            coding
                .process
                .as_ref()
                .is_some_and(|process| process.started_ms != began && !process.ended.is_empty())
        },
    )
    .await;
    let ended = coding.process.expect("its process");
    assert_eq!(
        (ended.ended.as_str(), ended.stopped_by.as_str()),
        ("stopped", "agent:scout"),
        "the process ended, stopped by the agent that started it"
    );
    let stopped = reply_of(&workspaces, &workspace, &second, "fix-hello").await;
    assert_eq!(
        end_of(&stopped),
        Some(chat::agent_stream::AgentFinish::Cancelled)
    );
    stands_as(&workspaces, &workspace, "fix-hello", Stands::Stopped).await;
    assert_eq!(
        theirs.asked().len(),
        2,
        "nothing asked a model after the job"
    );
    page.peer.close();
    }
}

// The person's key is nowhere a program in the machine could find it, and nowhere in what
// the workspace keeps of the coding agent: not in what its processes were started with, not
// in a file among the workspace's files or in its working copy, not in a frame into the
// machine, not in what it printed, not in a message, not in its item. The key was used all
// the same: on the server, where the provider is called.
on_both_kinds! {
    the_persons_key_is_nowhere_in_the_machine_or_in_what_the_workspace_keeps,
    the_persons_key_is_nowhere_in_the_machine_or_in_what_the_workspace_keeps_where_its_machine_is_placed,
    |machine| {
    team!(root, browser, actions, workspaces, workspace, page, person, served);
    let ada = themselves(&root, &browser, &workspace);
    let theirs = links_model(
        &workspaces,
        &workspace,
        [
            Reply::text("Fix hello waits for your word."),
            Reply::text("The README says hello now.").costing(900),
            Reply::text("Fix hello is done."),
        ],
    ).await;
    let (agents, _) = coding_on(&served, &[&workspace], &machine).await;
    let own = direct(&person, "fix-hello");
    let thread = direct(&person, SCOUT);
    started_and_waiting(
        &agents,
        &workspaces,
        (&workspace, &person),
        ("Fix hello", "fix-hello"),
        1,
    )
    .await;
    let asks = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "where is your model"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &asks, "fix-hello").await;
    starter_told(&workspaces, &workspace, &person, 2).await;

    // The key was used: the person's model was asked with it, on the server.
    let asked = theirs.asked();
    assert_eq!(asked.len(), 3);
    assert!(
        asked.iter().all(|call| call.key.as_deref() == Some(KEY)),
        "every call of the person's model carried their key to the provider: {asked:?}"
    );

    // Everywhere a key could be in the machine, and in what the workspace keeps.
    let machines = machine.machines();
    assert_eq!(machines.len(), 1);
    let (launcher, harness) = (machines[0].launcher(), machines[0].harness());
    let mut everywhere = vec![seen_of(launcher), seen_of(harness)];
    everywhere.extend(machine.started_with());
    everywhere.extend(machines[0].went_in());
    everywhere.extend(machines[0].came_out());
    let before_files = everywhere.len();
    read_under(machine.files(), &mut everywhere);
    let files = everywhere.len() - before_files;
    for conversation in [&own, &thread] {
        for (_, message) in said_there(&workspaces, &workspace, conversation.as_str()).await {
            everywhere.push(format!("{message:?}"));
        }
    }
    everywhere.push(format!("{:?}", roster(&workspaces, &workspace).await));
    assert!(
        files > 5 && everywhere.len() > 20,
        "the places a key could be were looked at: {files} files of {}",
        everywhere.len()
    );
    for place in &everywhere {
        assert!(
            !place.contains(MARK),
            "the person's key is where the machine or the workspace holds it: {}",
            place.chars().take(400).collect::<String>()
        );
    }

    // What was looked at is the real thing: the coding program was started with its endpoint
    // and with a word in the key's place, and it said where its model is.
    let started_with = seen_of(harness);
    assert!(
        started_with.contains("ANTHROPIC_BASE_URL=http://127.0.0.1:")
            && started_with.contains("ANTHROPIC_AUTH_TOKEN=a-word-not-a-key"),
        "the coding program's own variables were read"
    );
    let said = said_by(&workspaces, &workspace, (&own, "fix-hello")).await;
    assert!(
        said.iter()
            .any(|text| text.starts_with("my model is at http://127.0.0.1:")),
        "{said:?}"
    );
    page.peer.close();
    }
}

// The one-way rule holds through Tilo's own MCP server, as the coding program reaches it.
// It reads the chat's messages and is refused, so nothing of the main thread reaches the
// machine. It writes into the main thread and is refused. The same write into its own
// conversation is taken: the tools work, and the rule is where it writes.
on_both_kinds! {
    a_coding_program_reads_nothing_of_the_main_thread_and_writes_only_where_it_works,
    a_coding_program_reads_nothing_of_the_main_thread_and_writes_only_where_it_works_where_its_machine_is_placed,
    |machine| {
    team!(root, browser, actions, workspaces, workspace, page, person, served);
    let ada = themselves(&root, &browser, &workspace);
    let theirs = links_model(
        &workspaces,
        &workspace,
        [
            Reply::text("Noted."),
            Reply::text("Fix hello waits for your word."),
            Reply::text("The job is looked at."),
            Reply::text("Fix hello is done."),
        ],
    ).await;
    let (agents, _) = coding_on(&served, &[&workspace], &machine).await;
    let own = direct(&person, "fix-hello");
    let thread = direct(&person, SCOUT);

    // The main thread holds something only it holds.
    let secret = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "The launch's code word is heron."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &secret, SCOUT).await;
    started_and_waiting(
        &agents,
        &workspaces,
        (&workspace, &person),
        ("Fix hello", "fix-hello"),
        1,
    )
    .await;
    let answers = |said: &[String]| -> Vec<String> {
        said.iter()
            .filter(|text| text.starts_with("tilo answered"))
            .cloned()
            .collect()
    };

    // It reads the chat's messages through Tilo's MCP server, and is refused.
    let reads = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "ask tilo to read chat/Messages"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &reads, "fix-hello").await;
    starter_told(&workspaces, &workspace, &person, 2).await;
    let read = answers(&said_by(&workspaces, &workspace, (&own, "fix-hello")).await);
    assert_eq!(read.len(), 1, "{read:?}");
    assert!(
        read[0].contains("not_permitted: "),
        "its read of the chat is refused as not allowed: {}",
        read[0]
    );

    // It writes into the main thread, and is refused.
    let aimed = said(thread.as_str(), "written from the machine", None);
    let writes = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (
            own.as_str(),
            &format!("ask tilo to do chat/Messages#post with {aimed} end"),
        ),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &writes, "fix-hello").await;
    let wrote = answers(&said_by(&workspaces, &workspace, (&own, "fix-hello")).await);
    assert_eq!(wrote.len(), 2, "{wrote:?}");
    assert!(
        wrote.iter().all(|text| refused_in_words(text)),
        "its read and its write into the main thread are both refused: {wrote:?}"
    );
    assert!(
        said_there(&workspaces, &workspace, thread.as_str())
            .await
            .iter()
            .all(|(_, message)| message.text != "written from the machine"),
        "and nothing of it stands in the main thread"
    );

    // The same write into its own conversation is taken.
    let aimed = said(own.as_str(), "written from the machine", None);
    let writes = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (
            own.as_str(),
            &format!("ask tilo to do chat/Messages#post with {aimed} end"),
        ),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &writes, "fix-hello").await;
    let said_now = said_by(&workspaces, &workspace, (&own, "fix-hello")).await;
    assert!(
        said_now
            .iter()
            .any(|text| text == "written from the machine"),
        "where it works its words are taken: {said_now:?}"
    );

    // Nothing that went into the machine holds a word of the main thread.
    let machines = machine.machines();
    assert_eq!(machines.len(), 1);
    let went_in = machines[0].went_in();
    assert!(
        went_in.len() > 6,
        "what went into the machine was looked at"
    );
    for text in &went_in {
        assert!(
            !text.contains("heron"),
            "the main thread reached the machine: {}",
            text.chars().take(400).collect::<String>()
        );
    }
    assert_eq!(theirs.asked().len(), 4);
    page.peer.close();
    }
}

/// [`team!`], with a second workspace of the same person on the same server, joined from
/// another device of theirs.
macro_rules! two_teams {
    ($root:ident, ($browser:ident, $second:ident), $actions:ident, $workspaces:ident, ($workspace:ident, $other:ident), ($page:ident, $other_page:ident), $person:ident, $served:ident) => {
        let ($root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let server = enrol(&$root, "tilo-sh", DeviceCapabilities::member());
        let $browser = enrol(&$root, "browser", DeviceCapabilities::member());
        let $second = enrol(&$root, "laptop", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let $served = crate::harness::hosted::start(
            &$root,
            &server,
            state.path(),
            crate::harness::hosted::Options::offering(crate::common::catalogue),
        )
        .await;
        let $actions = $served.roster.actions.clone();
        let $workspaces = $served.workspaces().unwrap().clone();
        let $person = hex::encode($root.verifying_key().to_bytes());
        let $workspace = $workspaces.create("Team").await.unwrap().workspace;
        let $other = $workspaces.create("Other").await.unwrap().workspace;
        let $page = Browser::open_at(&$served, &$browser, &server, &$workspace).await;
        let $other_page = Browser::open_at(&$served, &$second, &server, &$other).await;
        let catalogue = crate::common::catalogue();
        let governor = $workspaces.governor().clone();
        for (page, workspace) in [(&$page, &$workspace), (&$other_page, &$other)] {
            make_agent(page, &$workspaces, workspace, &$person, None).await;
            $workspaces
                .prepare_application(workspace, &governor, &catalogue, CHAT, "")
                .await
                .unwrap();
        }
    };
}

/// Where the coding agent `agent` said its model is, in its own conversation.
async fn model_of(
    workspaces: &Workspaces,
    workspace: &str,
    (person, agent): (&str, &str),
) -> String {
    let said = said_by(workspaces, workspace, (&direct(person, agent), agent)).await;
    said.iter()
        .find_map(|text| text.strip_prefix("my model is at "))
        .unwrap_or_else(|| panic!("{agent} never said where its model is: {said:?}"))
        .to_owned()
}

// A coding program of another workspace gets nothing from this one's model. Each workspace's
// coding agent is a process of its own with an endpoint of its own. The other workspace's
// process asks this one's endpoint: it is turned away, nobody's model is asked, and nothing
// reaches this one's conversation. Its own endpoint answers it as before.
on_both_kinds! {
    a_coding_program_of_another_workspace_gets_nothing_from_this_ones_model,
    a_coding_program_of_another_workspace_gets_nothing_from_this_ones_model_where_its_machine_is_placed,
    |machine| {
    two_teams!(
        root,
        (browser, laptop),
        actions,
        workspaces,
        (workspace, other),
        (page, other_page),
        person,
        served
    );
    let ada = themselves(&root, &browser, &workspace);
    let ada_there = themselves(&root, &laptop, &other);
    let theirs = links_model(
        &workspaces,
        &workspace,
        [
            Reply::text("Fix hello waits for your word."),
            Reply::text("The job is looked at."),
            Reply::text("Fix hello is done."),
            Reply::text("Mend hello waits for your word."),
            Reply::text("The other job is looked at."),
            Reply::text("Mend hello is done."),
            Reply::text("Mine."),
        ],
    ).await;
    // The other workspace keeps a model of its own: its agent works through that one, never
    // through this one's.
    links_model_in(&workspaces, &other).await;
    let (agents, _) = coding_on(&served, &[&workspace, &other], &machine).await;

    // A coding agent of each workspace says where its model is.
    for (place, caller, (name, id)) in [
        (&workspace, &ada, ("Fix hello", "fix-hello")),
        (&other, &ada_there, ("Mend hello", "mend-hello")),
    ] {
        started_and_waiting(&agents, &workspaces, (place, &person), (name, id), 1).await;
        let asks = writes_in(
            &actions,
            &workspaces,
            place,
            caller,
            (direct(&person, id).as_str(), "where is your model"),
        )
        .await;
        agents.look(place).await.unwrap();
        reply_of(&workspaces, place, &asks, id).await;
        starter_told(&workspaces, place, &person, 2).await;
    }
    let ours = model_of(&workspaces, &workspace, (&person, "fix-hello")).await;
    let theirs_is = model_of(&workspaces, &other, (&person, "mend-hello")).await;
    assert_ne!(ours, theirs_is, "each has an endpoint of its own");
    let machines = machine.machines();
    assert_eq!(
        (machine.started(), machines.len()),
        (2, 2),
        "each is a process of its own"
    );
    assert_eq!(theirs.asked().len(), 6);
    let ours_said = said_by(
        &workspaces,
        &workspace,
        (&direct(&person, "fix-hello"), "fix-hello"),
    )
    .await;

    // The other workspace's process asks this one's endpoint: nothing comes back.
    let there = direct(&person, "mend-hello");
    let asks = writes_in(
        &actions,
        &workspaces,
        &other,
        &ada_there,
        (there.as_str(), &format!("ask the model at {ours}")),
    )
    .await;
    agents.look(&other).await.unwrap();
    reply_of(&workspaces, &other, &asks, "mend-hello").await;
    let said = said_by(&workspaces, &other, (&there, "mend-hello")).await;
    assert!(
        said.iter().any(|text| text == "from there came []"),
        "it got no answer from the other workspace's endpoint: {said:?}"
    );
    assert_eq!(
        machines[0].turned_away(),
        1,
        "this one's machine turned the caller away at its endpoint"
    );
    assert_eq!(
        theirs.asked().len(),
        6,
        "and nobody's model was asked for it"
    );
    assert_eq!(
        said_by(
            &workspaces,
            &workspace,
            (&direct(&person, "fix-hello"), "fix-hello"),
        )
        .await,
        ours_said,
        "nothing of it reached this one's conversation"
    );

    // Its own endpoint answers the very same request.
    let asks = writes_in(
        &actions,
        &workspaces,
        &other,
        &ada_there,
        (there.as_str(), &format!("ask the model at {theirs_is}")),
    )
    .await;
    agents.look(&other).await.unwrap();
    reply_of(&workspaces, &other, &asks, "mend-hello").await;
    let said = said_by(&workspaces, &other, (&there, "mend-hello")).await;
    assert!(
        said.iter()
            .any(|text| text.starts_with("from there came [HTTP/1.1 200")),
        "the same request is answered at its own endpoint: {said:?}"
    );
    assert_eq!(theirs.asked().len(), 7);
    page.peer.close();
    other_page.peer.close();
    }
}

// The day's limit of the person's own agent holds for its coding agent: once what the coding
// program's calls cost has used the day up, its next call is answered at its endpoint before
// the provider is asked, and the reply says the limit's words. What it spent is counted on
// the day of the person's own agent, whose own next turn then stops before it starts.
on_both_kinds! {
    the_days_limit_answers_a_coding_program_before_the_provider_is_asked,
    the_days_limit_answers_a_coding_program_before_the_provider_is_asked_where_its_machine_is_placed,
    |machine| {
    team!(root, browser, actions, workspaces, workspace, page, person, served);
    let ada = themselves(&root, &browser, &workspace);
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        WorkspaceAgents::SET_AGENT_SPEND_LIMIT.with(workspace_agents::SetAgentSpendLimit {
            agent: Id::from_raw(SCOUT.to_owned()),
            limit_micros: Some(1_000),
        }),
    )
    .await
    .expect("its keeper sets the day's limit of their own agent");
    let theirs = links_model(
        &workspaces,
        &workspace,
        [
            Reply::text("Fix hello waits for your word."),
            Reply::text("A long first look.").costing(1_500),
            Reply::text("never asked"),
        ],
    ).await;
    let (agents, _) = coding_on(&served, &[&workspace], &machine).await;
    let own = direct(&person, "fix-hello");
    started_and_waiting(
        &agents,
        &workspaces,
        (&workspace, &person),
        ("Fix hello", "fix-hello"),
        1,
    )
    .await;

    // Its job uses the day up; the call for its person's own words is refused at the endpoint.
    let more = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "Look further."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let refused = reply_of(&workspaces, &workspace, &more, "fix-hello").await;
    assert!(
        refused.text.contains("Scout") && refused.text.contains("has used today's"),
        "the reply says the limit's words, naming the agent whose day it is: {}",
        refused.text
    );
    assert_eq!(
        end_of(&refused),
        Some(chat::agent_stream::AgentFinish::Failed)
    );
    assert_eq!(
        theirs.asked().len(),
        2,
        "the call past the limit never reached the provider"
    );
    let answered = machine.machines()[0].answered();
    assert!(
        answered.contains(&402) && answered.contains(&200),
        "its endpoint answered the first call and refused the next: {answered:?}"
    );
    let said = said_by(&workspaces, &workspace, (&own, "fix-hello")).await;
    assert!(
        said.iter().any(|text| text == "A long first look."),
        "what it said before the limit stands: {said:?}"
    );

    // What it spent is on its item and on the day of the person's own agent.
    let (_, coding) = item_when(
        &workspaces,
        &workspace,
        ("fix-hello", "that it failed"),
        |sub, coding| sub.stands == Stands::Failed && coding.spent_micros == 1_500,
    )
    .await;
    assert_eq!(coding.spent_micros, 1_500);
    assert_eq!(spent_by(&workspaces, &workspace, SCOUT).await, Some(1_500));

    // Its starter is told, and stops before it starts: the day is used up for it too.
    let told = starter_told(&workspaces, &workspace, &person, 2).await;
    assert!(
        told.iter().any(|(news, line)| {
            news.text.contains("stands: failed") && line.text.contains("today's limit")
        }),
        "{told:?}"
    );
    assert_eq!(theirs.asked().len(), 2, "and no model was asked for that");
    page.peer.close();
    }
}
