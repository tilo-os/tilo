//! A team from inside a turn, on the server, with no device of the person doing anything: the
//! person mentions their own agent in a channel of Chat, its turn starts a sub-agent, reads its
//! team, notes and stops one through the team's own verbs, and the server asks what it started
//! its job at once, made from the reply that turn writes.
use super::*;
use serde_json::json;

/// The team of a turn's agent, as the agent tools name it, and its verbs.
const START: &str = "sub-agents/team#start-sub-agent";
const NOTE: &str = "sub-agents/team#note-sub-agent";
const STOP: &str = "sub-agents/team#stop-sub-agent";
/// What the person asks their own agent.
const ASKED: &str = " find the deck and draft the memo.";
/// The jobs its turn gives.
const DECK: &str = "Find the Q3 board deck in Files.";
const MEMO: &str = "Draft the memo for the board.";

/// The person's own agent's turn: it starts two sub-agents, notes the first, stops the
/// second, then answers. A sub-agent answers its job at once, and the news of a sub-agent's
/// work is answered in a line.
fn the_team_plays(request: &ModelRequest) -> ModelResponse {
    if of_sub_agent(request) {
        return answer(
            vec![Content::Text {
                text: "The deck is in Files, under Board.".into(),
            }],
            StopReason::EndTurn,
            1_000,
        );
    }
    if said_in(request).contains("Your sub-agent") {
        return answer(
            vec![Content::Text {
                text: "The deck is found.".into(),
            }],
            StopReason::EndTurn,
            1_000,
        );
    }
    let calls = [
        (
            START,
            json!({"name": "Find the deck", "id": "find-the-deck", "job": DECK, "role": "Researcher"}),
        ),
        (
            START,
            json!({"name": "Draft the memo", "id": "draft-the-memo", "job": MEMO}),
        ),
        (
            NOTE,
            json!({"agent": "find-the-deck", "text": "The deck may be a PDF."}),
        ),
        (STOP, json!({"agent": "draft-the-memo"})),
    ];
    let done = tool_answers(request).len();
    match calls.get(done) {
        Some((path, args)) => does(done.saturating_add(1), path, args),
        None => answer(
            vec![Content::Text {
                text: "Started.".into(),
            }],
            StopReason::EndTurn,
            1_000,
        ),
    }
}

/// The person mentions their own agent; its turn starts and steers a team through the team's
/// verbs, each answered as the list decided it. The sub-agent it started is asked its job at
/// once, in its name, made from the reply its turn wrote; the one it stopped is asked nothing
/// that waits; the note asks the first again. The first answers its job as its own reply, reads
/// nothing of the person's channel, and its starter is asked with the news.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_main_agents_own_turn_starts_notes_and_stops_its_sub_agents() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&server, &workspace, plays(&asked, the_team_plays)).await;

    mentions(&workspaces, &workspace, &person, (SCOUT, "Scout", ASKED)).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "the mention is its one turn: {turns:?}");
    answers(&agents, &workspace, &turns[0]).await;
    let main = asks_of(&workspaces, &workspace, &person, SCOUT).await;
    let main = answered(&workspaces, &workspace, &person, &main[0].at).await;
    let reply = main.latest().unwrap().clone();
    assert!(reply.body.contains("Started."), "{}", reply.body);
    let last = asked
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|request| !of_sub_agent(request))
        .cloned()
        .unwrap();
    let said = tool_answers(&last);
    assert_eq!(
        said.len(),
        4,
        "every call of the turn was answered: {said:?}"
    );
    assert!(
        said.iter().all(|(_, failed)| !failed),
        "the list took every verb: {}",
        tools_said(&last)
    );

    // The deck's sub-agent was asked its job at once, made from the reply the turn wrote; at
    // the next look the note asks it again, and the memo's, stopped before anyone took its
    // job, waits for nothing.
    let turns: Vec<_> = agents
        .turns(&workspace)
        .await
        .unwrap()
        .into_iter()
        .filter(|turn| turn.agent.id.0 == "find-the-deck")
        .collect();
    assert_eq!(turns.len(), 2, "its job and the note: {turns:?}");
    let deck = asks_of(&workspaces, &workspace, &person, "find-the-deck").await;
    assert_eq!(deck.len(), 2, "its job and the note: {deck:?}");
    assert!(
        deck[0]
            .from
            .as_ref()
            .is_some_and(|from| from.same(&reply.at)),
        "its job is made from the reply of the turn that started it: {:?}",
        deck[0].from
    );
    assert_eq!(deck[0].said.as_deref().map(str::trim), Some(DECK));
    let memo = asks_of(&workspaces, &workspace, &person, "draft-the-memo").await;
    assert!(
        memo.iter().all(|asked| asked.state != AskState::Waiting),
        "a stopped sub-agent's job waits for nobody: {memo:?}"
    );
    stands_as(&workspaces, &workspace, "draft-the-memo", Stands::Stopped).await;

    // The deck's sub-agent answers its job, told its job and nothing of the person's channel.
    let job = turns
        .iter()
        .find(|turn| turn.line.as_ref().is_some_and(|line| line.job))
        .expect("its job's turn");
    answers(&agents, &workspace, job).await;
    let sub_told = asked
        .lock()
        .unwrap()
        .iter()
        .filter(|request| of_sub_agent(request))
        .map(|request| format!("{:?}", request.messages))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(sub_told.contains(DECK), "it is told its job: {sub_told}");
    assert!(
        !sub_told.contains("draft the memo."),
        "it is told nothing of its person's channel: {sub_told}"
    );
    let news = news_of(&workspaces, &workspace, &person, ("find-the-deck", 1)).await;
    assert_eq!(news[0].agent, MemberId::agent(SCOUT), "its starter is told");
    page.peer.close();
}

/// The jobs of a line two deep, and what each of its turns files in Tasks.
const LEAD: &str = "Plan the launch with a team of your own.";
const WORKER: &str = "File the launch's issues.";
const FILE: &str = "tasks/tasks.issues#create-many";

/// Whether a tool's answer says the call was refused, in the words the agent tools refuse in.
fn refused_in_words(answer: &str) -> bool {
    [
        "has no action",
        "no such action",
        "may not invoke",
        "not_permitted: ",
    ]
    .iter()
    .any(|said| answer.contains(said))
}

/// A turn of the line: the person's own agent starts the lead and files an issue, the lead
/// starts the worker and files one, the worker files one; each then answers.
fn the_line_plays(request: &ModelRequest) -> ModelResponse {
    let issue = || json!({"issues": [{"title": "Ship it", "status": "todo"}]});
    let (calls, last) = if of_sub_agent(request) && said_in(request).contains(WORKER) {
        (vec![(FILE, issue())], "Tried.")
    } else if of_sub_agent(request) && said_in(request).contains(LEAD) {
        (
            vec![
                (
                    START,
                    json!({"name": "Worker", "id": "worker", "job": WORKER}),
                ),
                (FILE, issue()),
            ],
            "Led.",
        )
    } else if of_sub_agent(request) || said_in(request).contains("Your sub-agent") {
        (Vec::new(), "Told.")
    } else {
        (
            vec![
                (START, json!({"name": "Lead", "id": "lead", "job": LEAD})),
                (FILE, issue()),
            ],
            "Started.",
        )
    };
    let done = tool_answers(request).len();
    match calls.get(done) {
        Some((path, args)) => does(done.saturating_add(1), path, args),
        None => answer(
            vec![Content::Text { text: last.into() }],
            StopReason::EndTurn,
            1_000,
        ),
    }
}

/// What the tools answered the last call of the turn `of` names, in the order asked.
fn answered_in(
    asked: &Mutex<Vec<ModelRequest>>,
    of: impl Fn(&ModelRequest) -> bool,
) -> Vec<(String, bool)> {
    asked
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|request| of(request))
        .map(tool_answers)
        .unwrap_or_default()
}

/// Answer the job of the sub-agent `agent` and the reply it wrote.
async fn job_answered(
    agents: &ServerAgents,
    workspaces: &Workspaces,
    (workspace, person): (&str, &str),
    agent: &str,
) -> asks::Replied {
    let turns = agents.turns(workspace).await.unwrap();
    let job = turns
        .iter()
        .find(|turn| turn.agent.id.0 == agent && turn.line.as_ref().is_some_and(|line| line.job))
        .unwrap_or_else(|| panic!("{agent} is asked its job: {turns:?}"));
    answers(agents, workspace, job).await;
    let asked = asks_of(workspaces, workspace, person, agent).await;
    let asked = answered(workspaces, workspace, person, &asked[0].at).await;
    asked.latest().unwrap().clone()
}

/// A sub-agent of a sub-agent may do nothing its starter may not. The person approves their
/// own agent for Chat alone and allows two levels. Its turn starts a lead, whose turn starts
/// a worker: each answers its job as its own reply, which Chat lets the approved key write,
/// and each is refused filing an issue in Tasks, which the approval leaves out, as the agent
/// above it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agent_of_a_sub_agent_does_nothing_its_starter_may_not() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&server, &browser, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = server
        .approving_for(&workspace, &[SCOUT], &["so.tilo.chat"])
        .await
        .scripted(plays(&asked, the_line_plays));
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        WorkspaceAgents::SET_SUB_AGENT_LIMITS.with(workspace_agents::SetSubAgentLimits {
            agent: Id::from_raw(SCOUT),
            depth: Some(2),
            at_once: None,
        }),
    )
    .await
    .expect("its keeper says in person how deep sub-agents go");

    mentions(
        &workspaces,
        &workspace,
        &person,
        (SCOUT, "Scout", " plan the launch."),
    )
    .await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "the mention is its one turn: {turns:?}");
    answers(&agents, &workspace, &turns[0]).await;
    let main = answered_in(&asked, |request| {
        !of_sub_agent(request) && !said_in(request).contains("Your sub-agent")
    });
    assert_eq!(main.len(), 2, "{main:?}");
    assert!(!main[0].1, "its own agent starts the lead: {}", main[0].0);
    assert!(
        main[1].1 && refused_in_words(&main[1].0),
        "its approval leaves Tasks out: {}",
        main[1].0
    );

    let led = job_answered(&agents, &workspaces, (&workspace, &person), "lead").await;
    assert!(led.body.contains("Led."), "{}", led.body);
    let lead = answered_in(&asked, |request| {
        of_sub_agent(request) && said_in(request).contains(LEAD)
    });
    assert_eq!(lead.len(), 2, "{lead:?}");
    assert!(!lead[0].1, "the lead starts its worker: {}", lead[0].0);
    assert!(
        lead[1].1 && refused_in_words(&lead[1].0),
        "the lead may not file what its starter may not: {}",
        lead[1].0
    );

    let worked = job_answered(&agents, &workspaces, (&workspace, &person), "worker").await;
    assert!(worked.body.contains("Tried."), "{}", worked.body);
    let worker = answered_in(&asked, |request| {
        of_sub_agent(request) && said_in(request).contains(WORKER)
    });
    assert_eq!(worker.len(), 1, "{worker:?}");
    assert!(
        worker[0].1 && refused_in_words(&worker[0].0),
        "nor may the worker the lead started: {}",
        worker[0].0
    );
    page.peer.close();
}
