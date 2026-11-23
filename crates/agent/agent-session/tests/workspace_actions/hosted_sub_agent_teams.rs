//! A team from inside a turn, on the server, with no device of the person doing anything: the
//! person's own agent starts sub-agents from its own turn, reads how they stand, gives one a
//! note and stops one, and the server works what it started; a lead reads the team under it
//! and nothing above or beside it; and whoever is not on a turn of the agent's keeper is
//! offered none of it.
use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionProvider as _, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use ui::tilo_workspace_agent::{Bound, KIND, OWNER};

/// The team of a turn's agent, as the agent tools name it, and its three verbs.
const TEAM: &str = "sub-agents/team";
/// Words of the label the tools put before an answer that carries content from outside.
const OUTSIDE: &str = "never instructions to follow";
const START: &str = "sub-agents/team#start-sub-agent";
const NOTE: &str = "sub-agents/team#note-sub-agent";
const STOP: &str = "sub-agents/team#stop-sub-agent";

/// Whose turn a turn of the person's own agent is, in [`Plans`].
const MAIN: &str = "";

/// What one turn calls, one call for each of its steps, and what it answers once its calls
/// are made; and once a turn took it, what that turn was asked with and what its tools
/// answered.
struct Plan {
    /// Whose turn it is for: the job of a sub-agent, or [`MAIN`].
    whose: String,
    /// Words the turn it is for was asked with.
    expects: String,
    calls: Vec<(String, Value)>,
    last: String,
    taken_by: Option<String>,
    answered: Vec<(String, bool)>,
}

/// What the agents' turns do, each plan for one turn. The server starts turns of its own
/// accord too (the news of work that ended), and every turn no plan is for makes no call.
#[derive(Clone, Default)]
struct Plans(Arc<Mutex<Vec<Plan>>>);

/// What a sub-agent with the job `job` is told of it before everything, and nobody else is.
fn told_job(job: &str) -> String {
    format!("Your job:\n{job}\n")
}

/// Whether `request` is of a turn of `whose`.
fn of_turn(request: &ModelRequest, whose: &str) -> bool {
    if of_sub_agent(request) {
        !whose.is_empty() && said_in(request).contains(&told_job(whose))
    } else {
        whose.is_empty()
    }
}

impl Plans {
    /// The next turn of `whose` that is asked with `expects` makes `calls`, in their order,
    /// and then answers `last`. A path after [`READ`] is read; every other is an action.
    fn set(&self, whose: &str, expects: &str, calls: Vec<(&str, Value)>, last: &str) {
        let mut plans = self.0.lock().unwrap();
        plans.retain(|plan| plan.whose != whose);
        plans.push(Plan {
            whose: whose.to_owned(),
            expects: expects.to_owned(),
            calls: calls
                .into_iter()
                .map(|(path, args)| (path.to_owned(), args))
                .collect(),
            last: last.to_owned(),
            taken_by: None,
            answered: Vec::new(),
        });
    }

    /// What the tools answered the turn that took the plan of `whose`: each answer's words,
    /// and whether it was a failure.
    fn answers(&self, whose: &str) -> Vec<(String, bool)> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|plan| plan.whose == whose)
            .map(|plan| plan.answered.clone())
            .unwrap_or_default()
    }

    /// A model that answers each call of a turn with what is planned for that turn.
    fn model(&self, asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
        let plans = self.clone();
        plays(asked, move |request| {
            let words = asked_in(request);
            let said = tool_answers(request);
            let mut plans = plans.0.lock().unwrap();
            let planned = plans.iter_mut().find(|plan| {
                of_turn(request, &plan.whose)
                    && match &plan.taken_by {
                        Some(taken) => *taken == words,
                        None => words.contains(&plan.expects),
                    }
            });
            let Some(plan) = planned else {
                return says("Told.");
            };
            plan.taken_by = Some(words);
            let done = said.len();
            plan.answered = said;
            match plan.calls.get(done) {
                Some((path, args)) => match path.strip_prefix(READ) {
                    Some(read) => answer(
                        vec![Content::ToolCall(ToolCall {
                            id: format!("read-{done}"),
                            name: "tilo_read".into(),
                            input: json!({"path": read}),
                            extra_content: None,
                        })],
                        StopReason::ToolCalls,
                        1_000,
                    ),
                    None => does(done, path, args),
                },
                None => says(&plan.last),
            }
        })
    }
}

/// A read of `path`, as a plan names it.
fn read(path: &str) -> String {
    format!("{READ}{path}")
}

/// `set-sub-agent-limits` on the person's own agent.
fn limits(depth: Option<u8>, at_once: Option<u32>) -> Invoke<WorkspaceAgents> {
    WorkspaceAgents::SET_SUB_AGENT_LIMITS.with(workspace_agents::SetSubAgentLimits {
        agent: Id::from_raw(SCOUT),
        depth,
        at_once,
    })
}

/// The conversation of `person` with the agent `agent`.
fn direct(person: &str, agent: &str) -> Id {
    chat::Chat::direct_id([MemberId::person(person.to_owned()), MemberId::agent(agent)])
}

/// The sub-agents the list holds for `person`, by their ids.
async fn sub_agents(workspaces: &Workspaces, workspace: &str, person: &str) -> Vec<String> {
    roster(workspaces, workspace)
        .await
        .sub_agents_of(person)
        .map(|agent| agent.id.0.clone())
        .collect()
}

/// Whether `shown` names any of `others`, in whatever case.
fn names_any(shown: &str, others: &[&str]) -> bool {
    let shown = shown.to_lowercase();
    others.iter().any(|other| shown.contains(other))
}

/// The person's own agent starts sub-agents from its own turn, through the turn's own tools.
/// Past the number its person lets work at once the list refuses in its own words, and what
/// it refused is not kept. The turn stops one and reads how its team stands. Nothing else is
/// needed to start one: the server carries its job into a conversation of its own, as the
/// words of the agent whose turn started it, and works it there. One level down, where one
/// level is all its person allows, the sub-agent's own turn is refused a sub-agent of its own.
/// A later turn of the person's own agent reads how the work ended and gives it a note, which
/// the server carries to it, and it works again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_main_agents_own_turn_starts_reads_notes_and_stops_its_sub_agents() {
    const DECK: &str = "Find the Q3 board deck in Files.";
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let ada = themselves(&root, &browser, &workspace);
    let thread = direct(&person, SCOUT);
    let plans = Plans::default();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&served, &workspace, plans.model(&asked)).await;
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        limits(None, Some(2)),
    )
    .await
    .expect("its keeper says in person how many work at once");

    plans.set(
        MAIN,
        "Find the deck and draft the memo.",
        vec![
            (
                START,
                json!({"name": "Find the deck", "job": DECK, "role": "Researcher"}),
            ),
            (
                START,
                json!({"name": "Draft the memo", "job": "Draft the memo for the board."}),
            ),
            (
                START,
                json!({"name": "Third", "job": "One more than may work at once."}),
            ),
            (STOP, json!({"agent": "draft-the-memo"})),
            (&read(TEAM), Value::Null),
        ],
        "Started.",
    );
    plans.set(
        DECK,
        DECK,
        vec![
            (START, json!({"name": "Deeper", "job": "Go deeper."})),
            (&read(TEAM), Value::Null),
        ],
        "The deck is not in Files.",
    );
    let first = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "Find the deck and draft the memo."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let reply = reply_of(&workspaces, &workspace, &first, SCOUT).await;
    assert_eq!(reply.text, "Started.");
    let said = plans.answers(MAIN);
    assert_eq!(
        said.len(),
        5,
        "every call of the turn was answered: {said:?}"
    );
    assert!(
        said.iter().all(|(answer, _)| !answer.contains(OUTSIDE)),
        "what a team's verbs and its read answer is the list's own, no content from outside: \
         {said:?}"
    );
    assert!(
        !said[0].1 && said[0].0.contains("find-the-deck"),
        "the turn's own tool started one and was told its id: {}",
        said[0].0
    );
    assert!(!said[1].1, "and a second: {}", said[1].0);
    assert!(
        said[2].1 && said[2].0.contains("2 sub-agents are at work under"),
        "past the number at once the list refuses in its own words: {}",
        said[2].0
    );
    assert!(
        !said[3].1 && said[3].0.contains("stopped"),
        "it stops one of its own: {}",
        said[3].0
    );
    assert!(
        !said[4].1
            && [
                "find-the-deck",
                "draft-the-memo",
                "working",
                "stopped",
                "Researcher"
            ]
            .iter()
            .all(|word| said[4].0.contains(word))
            && !names_any(&said[4].0, &["third"]),
        "it reads how each of its team stands: {}",
        said[4].0
    );
    let list = roster(&workspaces, &workspace).await;
    let started = list
        .get(&Id::from_raw("find-the-deck"))
        .cloned()
        .expect("the list holds what the turn started");
    let sub = started.sub.clone().unwrap();
    assert_eq!(
        (
            started.keeper.as_str(),
            sub.started_by.as_str(),
            sub.depth,
            sub.role.as_str(),
            sub.stands
        ),
        (person.as_str(), SCOUT, 1, "Researcher", Stands::Working),
        "it works for the same person, started by the agent whose turn it was"
    );
    assert_eq!(
        list.get(&Id::from_raw("draft-the-memo"))
            .and_then(|agent| agent.sub.as_ref())
            .map(|sub| sub.stands),
        Some(Stands::Stopped)
    );
    assert!(
        list.get(&Id::from_raw("third")).is_none(),
        "what the list refused is not kept"
    );

    // Nothing else starts it: the server carries its job into its own conversation, as the
    // words of the agent whose turn started it, and works it there.
    let own = direct(&person, "find-the-deck");
    agents.look(&workspace).await.unwrap();
    let jobs = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await;
    assert_eq!(
        (jobs[0].1.text.as_str(), &jobs[0].1.author),
        (DECK, &MemberId::agent(SCOUT))
    );
    agents.look(&workspace).await.unwrap();
    let answered = reply_of(&workspaces, &workspace, &jobs[0].0, "find-the-deck").await;
    assert_eq!(answered.text, "The deck is not in Files.");
    let below = plans.answers(DECK);
    assert_eq!(below.len(), 2, "{below:?}");
    assert!(
        below[0].1 && below[0].0.contains("go 1 deep under"),
        "past the depth its person allows the list refuses in its own words: {}",
        below[0].0
    );
    assert!(
        !below[1].1 && !names_any(&below[1].0, &["draft-the-memo", "scout", "deeper"]),
        "it has no team, and reads nobody above or beside it: {}",
        below[1].0
    );
    assert_eq!(
        sub_agents(&workspaces, &workspace, &person).await.len(),
        2,
        "what the list refused a sub-agent is not kept either"
    );
    assert!(
        said_there(
            &workspaces,
            &workspace,
            direct(&person, "draft-the-memo").as_str()
        )
        .await
        .is_empty(),
        "the one that was stopped before the server reached it was told nothing"
    );
    stands_as(&workspaces, &workspace, "find-the-deck", Stands::Done).await;

    // A later turn of its own reads how the work ended, and gives it a note.
    plans.set(
        MAIN,
        "How do they stand?",
        vec![
            (&read(TEAM), Value::Null),
            (
                NOTE,
                json!({"agent": "find-the-deck", "text": "Look in Mail too."}),
            ),
        ],
        "Noted.",
    );
    plans.set(
        DECK,
        "Look in Mail too.",
        Vec::new(),
        "It is not in Mail either.",
    );
    let second = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "How do they stand?"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &second, SCOUT).await;
    let said = plans.answers(MAIN);
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(
        !said[0].1 && said[0].0.contains("done") && said[0].0.contains("The deck is not in Files."),
        "it reads how the work ended, and its line: {}",
        said[0].0
    );
    assert!(!said[1].1, "its note is taken: {}", said[1].0);
    agents.look(&workspace).await.unwrap();
    let notes = caused(&workspaces, &workspace, (own.as_str(), "note"), 1).await;
    assert_eq!(
        (notes[0].1.text.as_str(), &notes[0].1.author),
        ("Look in Mail too.", &MemberId::agent(SCOUT)),
        "the note is its starter's own words"
    );
    agents.look(&workspace).await.unwrap();
    let again = reply_of(&workspaces, &workspace, &notes[0].0, "find-the-deck").await;
    assert_eq!(again.text, "It is not in Mail either.");
    page.peer.close();
}

/// A lead with a team. Its person allows two levels, the person's own agent starts a lead
/// and a sub-agent beside it, and the lead's own turn starts a worker, which the server works
/// too. The lead reads its worker and nothing of the agent above it or the sub-agent beside
/// it in the team; it reads Chat as far as its chain may; and it steers nobody beside it. The
/// one beside and the worker read no team, steer nobody above or beside them, and the worker is
/// refused a third level in the list's own words. The person's own agent, two levels up, reads
/// both levels and steers the worker. What a lead gives its own team goes by the list, which
/// keeps it, and its note reaches its worker as the lead's own words.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lead_reads_the_team_under_it_and_nothing_above_or_beside_it() {
    const LEAD: &str = "Build the feature with a team of your own.";
    const BESIDE: &str = "Find the Q3 board deck in Files.";
    const WORKER: &str = "Write the tests for the feature.";
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let ada = themselves(&root, &browser, &workspace);
    // The person is called Ada here, as their page says when it joins: the workspace's member
    // list is whom a name in a message is bound to.
    let named = page
        .peer
        .ask_workspaces(tilo_server_protocol::workspaces::WorkspacesRequest::Name {
            workspace: workspace.clone(),
            name: "Ada".to_owned(),
        })
        .unwrap();
    assert!(matches!(
        tilo_test_env::within("the workspace names its person", named)
            .await
            .unwrap()
            .unwrap(),
        tilo_server_protocol::workspaces::WorkspacesReply::Named
    ));
    let thread = direct(&person, SCOUT);
    let plans = Plans::default();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&served, &workspace, plans.model(&asked)).await;
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        limits(Some(2), None),
    )
    .await
    .expect("its keeper says in person how deep sub-agents go");

    plans.set(
        MAIN,
        "Build the feature and find the deck.",
        vec![
            (START, json!({"name": "Lead", "job": LEAD})),
            (START, json!({"name": "Beside", "job": BESIDE})),
        ],
        "Started.",
    );
    plans.set(
        LEAD,
        LEAD,
        vec![
            (START, json!({"name": "Worker", "job": WORKER})),
            (&read(TEAM), Value::Null),
            (&read("chat/Messages"), Value::Null),
            (NOTE, json!({"agent": "beside", "text": "Stop looking."})),
            (STOP, json!({"agent": "beside"})),
        ],
        "The feature is planned.",
    );
    plans.set(
        BESIDE,
        BESIDE,
        vec![
            (&read(TEAM), Value::Null),
            (NOTE, json!({"agent": "lead", "text": "Hurry."})),
        ],
        "The deck is not in Files.",
    );
    plans.set(
        WORKER,
        WORKER,
        vec![
            (&read(TEAM), Value::Null),
            (START, json!({"name": "Deeper", "job": "Go deeper."})),
            (STOP, json!({"agent": "lead"})),
        ],
        "The tests are written.",
    );
    let first = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (
            thread.as_str(),
            "The launch's code word is heron. Build the feature and find the deck.",
        ),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &first, SCOUT).await;
    let said = plans.answers(MAIN);
    assert!(
        said.len() == 2 && !said[0].1 && !said[1].1,
        "the person's own agent started both: {said:?}"
    );

    // The server works the lead and the one beside it.
    let (lead_in, beside_in) = (direct(&person, "lead"), direct(&person, "beside"));
    agents.look(&workspace).await.unwrap();
    let lead_job = caused(&workspaces, &workspace, (lead_in.as_str(), "job"), 1).await;
    let beside_job = caused(&workspaces, &workspace, (beside_in.as_str(), "job"), 1).await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &lead_job[0].0, "lead").await;
    reply_of(&workspaces, &workspace, &beside_job[0].0, "beside").await;

    let lead = plans.answers(LEAD);
    assert_eq!(lead.len(), 5, "{lead:?}");
    assert!(
        !lead[0].1 && lead[0].0.contains("worker"),
        "a lead's own turn starts its worker: {}",
        lead[0].0
    );
    assert!(
        !lead[1].1
            && lead[1].0.contains("worker")
            && lead[1].0.contains("working")
            && !names_any(&lead[1].0, &["beside", "scout", "heron", "board deck"]),
        "a lead reads its own team, and nobody above or beside it: {}",
        lead[1].0
    );
    assert!(
        !lead[2].1 && lead[2].0.contains("heron"),
        "a lead reads Chat as far as its chain may, its person's thread with their agent too: {}",
        lead[2].0
    );
    for (words, failed) in &lead[3..] {
        assert!(
            *failed && words.contains("steers it"),
            "a lead steers nobody beside it: {words}"
        );
    }
    let beside = plans.answers(BESIDE);
    assert_eq!(beside.len(), 2, "{beside:?}");
    assert!(
        !beside[0].1 && !names_any(&beside[0].0, &["lead", "worker", "scout", "heron"]),
        "the one beside the lead has no team and reads none: {}",
        beside[0].0
    );
    assert!(
        beside[1].1 && beside[1].0.contains("steers it"),
        "and steers nobody beside it: {}",
        beside[1].0
    );

    // The server works the lead's worker as it works any sub-agent: its job is its lead's
    // own words.
    let worker_in = direct(&person, "worker");
    agents.look(&workspace).await.unwrap();
    let worker_job = caused(&workspaces, &workspace, (worker_in.as_str(), "job"), 1).await;
    assert_eq!(
        (worker_job[0].1.text.as_str(), &worker_job[0].1.author),
        (WORKER, &MemberId::agent("lead"))
    );
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &worker_job[0].0, "worker").await;
    let worker = plans.answers(WORKER);
    assert_eq!(worker.len(), 3, "{worker:?}");
    assert!(
        !worker[0].1 && !names_any(&worker[0].0, &["lead", "beside", "scout", "heron"]),
        "a worker has no team, and reads nobody above it: {}",
        worker[0].0
    );
    assert!(
        worker[1].1 && worker[1].0.contains("go 2 deep under"),
        "past the depth its person allows the list refuses in its own words: {}",
        worker[1].0
    );
    assert!(
        worker[2].1 && worker[2].0.contains("steers it"),
        "nor does it steer whoever started it: {}",
        worker[2].0
    );
    for ended in ["lead", "beside"] {
        stands_as(&workspaces, &workspace, ended, Stands::Done).await;
    }
    stands_as(&workspaces, &workspace, "worker", Stands::Done).await;
    // The person's own agent was told how the lead and the one beside it ended, and answered
    // that before it is asked again: an agent takes one turn at a time in a conversation.
    let ended = caused(&workspaces, &workspace, (thread.as_str(), "news"), 2).await;
    agents.look(&workspace).await.unwrap();
    for (news, _) in &ended {
        reply_of(&workspaces, &workspace, news, SCOUT).await;
    }

    // Two levels up, the person's own agent reads both levels and steers the worker.
    plans.set(
        MAIN,
        "How does the team stand?",
        vec![
            (&read(TEAM), Value::Null),
            (
                NOTE,
                json!({"agent": "worker", "text": "Name each test after what it proves."}),
            ),
            (STOP, json!({"agent": "beside"})),
        ],
        "Read.",
    );
    let second = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "How does the team stand?"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &second, SCOUT).await;
    let main = plans.answers(MAIN);
    assert_eq!(main.len(), 3, "{main:?}");
    assert!(
        !main[0].1
            && ["lead", "beside", "worker", "The tests are written.", "done"]
                .iter()
                .all(|word| main[0].0.contains(word)),
        "two levels up, it reads both levels: {}",
        main[0].0
    );
    assert!(
        !main[1].1 && !main[2].1,
        "and steers whoever is under it, however deep: {:?}",
        &main[1..]
    );
    let list = roster(&workspaces, &workspace).await;
    let below = list
        .get(&Id::from_raw("worker"))
        .and_then(|agent| agent.sub.clone())
        .expect("the list holds the worker");
    assert_eq!((below.started_by.as_str(), below.depth), ("lead", 2));
    assert!(
        below
            .notes
            .iter()
            .any(|note| note.by == SCOUT && note.text == "Name each test after what it proves."),
        "the worker's item holds the note of the agent two levels up: {:?}",
        below.notes
    );

    // First the work under way ends: the worker answers the note from two levels up, and the
    // lead the news of both pieces of its worker's work.
    agents.look(&workspace).await.unwrap();
    let noted = caused(&workspaces, &workspace, (worker_in.as_str(), "note"), 1).await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &noted[0].0, "worker").await;
    let told = caused(&workspaces, &workspace, (lead_in.as_str(), "news"), 2).await;
    agents.look(&workspace).await.unwrap();
    for (news, _) in &told {
        reply_of(&workspaces, &workspace, news, "lead").await;
    }

    // What a lead gives its team goes by the list, which keeps all three.
    plans.set(
        LEAD,
        "Say where the feature stands.",
        vec![
            (
                START,
                json!({"name": "Namer", "id": "namer", "job": "Ask @Ada which tests matter."}),
            ),
            (
                NOTE,
                json!({"agent": "worker", "text": "Tell @Ada when the tests pass."}),
            ),
            (
                NOTE,
                json!({"agent": "worker", "text": "Name the files too."}),
            ),
        ],
        "Said.",
    );
    let again = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (lead_in.as_str(), "Say where the feature stands."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &again, "lead").await;
    let lead = plans.answers(LEAD);
    assert_eq!(lead.len(), 3, "{lead:?}");
    for (answer, failed) in &lead {
        assert!(
            !*failed,
            "the list keeps what a lead gives its own team: {answer}"
        );
    }

    // The server writes the lead's note to its worker as the lead's own words.
    agents.look(&workspace).await.unwrap();
    let notes = caused(&workspaces, &workspace, (worker_in.as_str(), "note"), 2).await;
    assert!(
        notes.iter().any(|(_, note)| {
            note.text == "Name the files too." && note.author == MemberId::agent("lead")
        }),
        "a lead's plain note reaches its worker as the lead's own words: {notes:?}"
    );
    agents.look(&workspace).await.unwrap();
    let namer = roster(&workspaces, &workspace)
        .await
        .get(&Id::from_raw("namer"))
        .and_then(|agent| agent.sub.clone())
        .expect("the list holds the sub-agent the lead started");
    assert_eq!(
        (namer.started_by.as_str(), namer.job.as_str()),
        ("lead", "Ask @Ada which tests matter.")
    );
    page.peer.close();
}

/// Admit the person of `device` to `workspace` as an editor: the server issues them an
/// invitation and their device redeems it.
async fn admit(workspaces: &Workspaces, workspace: &str, device: &crate::support::Device) {
    crate::workspace_actions::resource_support::admit_with(
        workspaces,
        workspaces.governor(),
        workspace,
        device,
        tilo_peer_state::join::Role::Editor,
        "Bo",
    )
    .await;
}

/// What the server's action service answers `caller` outside any turn for a start on the
/// team of `provider`: how the call ended, in words.
async fn outside_a_turn(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: &str,
    caller: &Caller,
    provider: &str,
    arguments: &Value,
) -> String {
    let service = tilo_workspace_service::agents::WorkspaceActionProvider::new(
        actions.clone(),
        WorkspaceId::parse(workspace).unwrap(),
    );
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.to_owned(),
            address: tilo_actions::ActionAddress::new("team", "start-sub-agent"),
        },
        argument: InvokeArgument::Json(arguments.as_object().unwrap().clone()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let caller = caller.clone();
    tokio::task::spawn_blocking(move || {
        match service
            .invoke(&caller, request)
            .wait_timeout(Duration::from_secs(30))
        {
            Ok(Some(Ok(receipt))) => match receipt.status {
                tilo_actions::remote::ReceiptStatus::Rejected { error } => error.to_string(),
                other => format!("{other:?}"),
            },
            Ok(Some(Err(error))) => error.to_string(),
            _ => "the service did not answer".to_owned(),
        }
    })
    .await
    .unwrap()
}

/// A team is answered to a turn of the agent, whichever member asked it. A turn another member
/// starts on the agent reads the team and starts a sub-agent that works for them, in a
/// conversation of theirs with it, as one more actor in the chain of the agent and both people.
/// Outside any turn, the keeper themselves and their agent are
/// answered what an unknown provider is. And what its keeper bounds the agent to holds
/// through the turn's own tools: bounded off the workspace's agents it starts none and keeps
/// what the bound leaves it, and with the bound taken off it starts one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_team_is_answered_to_any_members_turn_of_the_agent() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let ada = themselves(&root, &browser, &workspace);
    let thread = direct(&person, SCOUT);
    let plans = Plans::default();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&served, &workspace, plans.model(&asked)).await;
    let start = json!({"name": "Find the deck", "job": "Find the Q3 board deck in Files."});

    // Another member writes to the agent: its turn acts for them.
    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo_device = enrol(&bo, "bo-browser", DeviceCapabilities::member());
    admit(&workspaces, &workspace, &bo_device).await;
    let his = hex::encode(bo.verifying_key().to_bytes());
    plans.set(
        MAIN,
        "Start a sub-agent for me.",
        vec![
            (START, start.clone()),
            ("sub-agentz/team#start-sub-agent", start.clone()),
            (&read(TEAM), Value::Null),
            (&read("sub-agentz/team"), Value::Null),
        ],
        "I cannot.",
    );
    let from_him = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&bo, &bo_device),
        "Start a sub-agent for me.",
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &from_him, SCOUT).await;
    let said = plans.answers(MAIN);
    assert_eq!(said.len(), 4, "{said:?}");
    assert!(!said[0].1, "his turn starts one: {said:?}");
    assert!(
        said[1].1 && said[3].1,
        "what does not exist is nothing: {said:?}"
    );
    assert!(
        !said[2].1 && said[2].0.contains("find-the-deck"),
        "his turn reads the agent's team, the one it started in it: {said:?}"
    );
    assert_eq!(
        sub_agents(&workspaces, &workspace, &his).await,
        ["find-the-deck"],
        "it works for him"
    );
    assert!(
        sub_agents(&workspaces, &workspace, &person)
            .await
            .is_empty(),
        "and for nobody else"
    );
    let his_sub = roster(&workspaces, &workspace)
        .await
        .get(&Id::from_raw("find-the-deck"))
        .and_then(|agent| agent.sub.clone())
        .expect("the list holds it");
    assert_eq!(his_sub.started_by, SCOUT);
    assert_eq!(
        his_sub.conversation,
        direct(&his, "find-the-deck").as_str(),
        "in a conversation of his with it"
    );

    // Outside any turn there is no team to ask for.
    let outside = super::their_agent(&ada, SCOUT)
        .named("Scout")
        .in_agent_session("agent-session:test-scout");
    for (who, caller) in [
        ("the keeper themselves", &ada),
        ("the agent outside a turn", &outside),
    ] {
        let team = outside_a_turn(&actions, &workspace, caller, "tilo.sub-agents", &start).await;
        let unknown = outside_a_turn(&actions, &workspace, caller, "tilo.no-such", &start).await;
        assert_eq!(
            team, unknown,
            "{who} is answered what an unknown provider is"
        );
    }
    assert!(
        sub_agents(&workspaces, &workspace, &person)
            .await
            .is_empty()
    );

    // What its keeper bounds the agent to holds through the turn's own tools.
    let bound = |rules: Vec<Bound>| {
        WorkspaceAgents::SET_AGENT_BOUND.with(workspace_agents::SetAgentBound {
            agent: Id::from_raw(SCOUT),
            bound: rules,
        })
    };
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        bound(vec![Bound {
            node: format!("workspace:{workspace}/app:{OWNER}/kind:{KIND}"),
            kinds: vec!["undoable".to_owned()],
            answer: "deny".to_owned(),
        }]),
    )
    .await
    .expect("its keeper bounds it in person");
    plans.set(
        MAIN,
        "Start a sub-agent.",
        vec![(START, start.clone()), (&read(TEAM), Value::Null)],
        "I may not.",
    );
    let bounded = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "Start a sub-agent."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &bounded, SCOUT).await;
    let said = plans.answers(MAIN);
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(
        said[0].1 && refused_in_words(&said[0].0),
        "bounded off the workspace's agents, its turn starts none: {}",
        said[0].0
    );
    assert!(
        !said[1].1,
        "and keeps what the bound leaves it: {}",
        said[1].0
    );
    assert!(
        sub_agents(&workspaces, &workspace, &person)
            .await
            .is_empty()
    );

    // With the bound taken off, the same call starts one.
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        bound(Vec::new()),
    )
    .await
    .expect("its keeper takes the bound off in person");
    plans.set(
        MAIN,
        "Now start it.",
        vec![(START, start.clone())],
        "Started.",
    );
    let free = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "Now start it."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &free, SCOUT).await;
    let said = plans.answers(MAIN);
    assert!(
        said.len() == 1 && !said[0].1,
        "its keeper's own turn starts one: {said:?}"
    );
    let hers = sub_agents(&workspaces, &workspace, &person).await;
    assert_eq!(hers.len(), 1, "one works for her: {hers:?}");
    stands_as(&workspaces, &workspace, &hers[0], Stands::Working).await;
    page.peer.close();
}

/// On a server that keeps people's own workspaces, the server's own person keeps an assistant
/// in a workspace no server keeps as theirs. A listing leaves the server's own account out of
/// the workspace's members, and the assistant's turn still starts a sub-agent: what it may do
/// is decided by who the workspace's governance keeps, its own person among them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_servers_own_persons_turn_starts_a_sub_agent_where_peoples_own_workspaces_are_kept() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    assert!(
        workspaces
            .offer_own_workspaces(tilo_server::workspaces::OwnWorkspaceOffer::free(
                "tilo.test"
            ))
            .unwrap(),
        "the server keeps people's own workspaces from now on"
    );
    let (_, listed) = workspaces.membership(&workspace).await.unwrap();
    assert!(
        !listed.contains_key(&person),
        "a listing leaves the server's own account out"
    );
    let (_, kept) = workspaces.kept_membership(&workspace).await.unwrap();
    assert!(
        kept.get(&person)
            .is_some_and(|mark| !mark.removed && !mark.banned && mark.role == "owner"),
        "the workspace's governance keeps its own person, its owner: {kept:?}"
    );

    let ada = themselves(&root, &browser, &workspace);
    let thread = direct(&person, SCOUT);
    let plans = Plans::default();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&served, &workspace, plans.model(&asked)).await;
    plans.set(
        MAIN,
        "Find the deck.",
        vec![(
            START,
            json!({"name": "Find the deck", "job": "Find the Q3 board deck in Files."}),
        )],
        "Started.",
    );
    let first = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "Find the deck."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &first, SCOUT).await;
    let said = plans.answers(MAIN);
    assert!(
        said.len() == 1 && !said[0].1 && said[0].0.contains("find-the-deck"),
        "the server's own person's assistant started a sub-agent: {said:?}"
    );
    stands_as(&workspaces, &workspace, "find-the-deck", Stands::Working).await;
    page.peer.close();
}

/// A sub-agent of a sub-agent may do nothing its starter may not. The person approves their
/// own agent for Chat alone and allows two levels. Its turn starts a lead, whose turn starts
/// a worker: each answers in its own conversation, which Chat lets the approved key write,
/// and each is refused filing an issue in Tasks, which the approval leaves out, as the agent
/// above it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agent_of_a_sub_agent_does_nothing_its_starter_may_not() {
    const LEAD: &str = "Plan the launch with a team of your own.";
    const WORKER: &str = "File the launch's issues.";
    const FILE: &str = "tasks/tasks.issues#create-many";
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let ada = themselves(&root, &browser, &workspace);
    let thread = direct(&person, SCOUT);
    let plans = Plans::default();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = served
        .approving_for(&workspace, &[SCOUT], &["so.tilo.chat"])
        .await
        .scripted(plans.model(&asked));
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        limits(Some(2), None),
    )
    .await
    .expect("its keeper says in person how deep sub-agents go");
    let issue = || json!({"issues": [{"title": "Ship it", "status": "todo"}]});
    plans.set(
        MAIN,
        "Plan the launch.",
        vec![
            (START, json!({"name": "Lead", "job": LEAD})),
            (FILE, issue()),
        ],
        "Started.",
    );
    plans.set(
        LEAD,
        LEAD,
        vec![
            (START, json!({"name": "Worker", "job": WORKER})),
            (FILE, issue()),
        ],
        "Led.",
    );
    plans.set(WORKER, WORKER, vec![(FILE, issue())], "Tried.");

    let first = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (thread.as_str(), "Plan the launch."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &first, SCOUT).await;
    let main = plans.answers(MAIN);
    assert_eq!(main.len(), 2, "{main:?}");
    assert!(!main[0].1, "its own agent starts the lead: {}", main[0].0);
    assert!(
        main[1].1 && refused_in_words(&main[1].0),
        "its approval leaves Tasks out: {}",
        main[1].0
    );

    let lead_in = direct(&person, "lead");
    agents.look(&workspace).await.unwrap();
    let lead_job = caused(&workspaces, &workspace, (lead_in.as_str(), "job"), 1).await;
    agents.look(&workspace).await.unwrap();
    let led = reply_of(&workspaces, &workspace, &lead_job[0].0, "lead").await;
    assert_eq!(
        (led.text.as_str(), &led.author),
        ("Led.", &MemberId::agent("lead"))
    );
    let lead = plans.answers(LEAD);
    assert_eq!(lead.len(), 2, "{lead:?}");
    assert!(!lead[0].1, "the lead starts its worker: {}", lead[0].0);
    assert!(
        lead[1].1 && refused_in_words(&lead[1].0),
        "the lead may not file what its starter may not: {}",
        lead[1].0
    );

    let worker_in = direct(&person, "worker");
    agents.look(&workspace).await.unwrap();
    let worker_job = caused(&workspaces, &workspace, (worker_in.as_str(), "job"), 1).await;
    agents.look(&workspace).await.unwrap();
    let worked = reply_of(&workspaces, &workspace, &worker_job[0].0, "worker").await;
    assert_eq!(
        (worked.text.as_str(), &worked.author),
        ("Tried.", &MemberId::agent("worker"))
    );
    let worker = plans.answers(WORKER);
    assert_eq!(worker.len(), 1, "{worker:?}");
    assert!(
        worker[0].1 && refused_in_words(&worker[0].0),
        "nor may the worker the lead started: {}",
        worker[0].0
    );
    page.peer.close();
}
