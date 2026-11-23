//! What the server's log keeps of a tool call of a turn on a workspace's server: its tool, each
//! argument by its name and the bytes of its value, of a refusal its sentence without anything
//! the call wrote, and of an answer its size. No value of an argument and nothing a tool
//! answered stands in a line, at info or with debug switched on: the log these tests read is
//! written at every level.
use super::*;
use std::collections::BTreeSet;

/// The action the refused call names: the letter's app has none called this.
const NO_SUCH: &str = "quillfeather-nine";
/// What the refused call would have written.
const UNWRITTEN: &str = "saltmarsh-eleven";
/// What the call that succeeds writes, before the turn lists what the app holds.
const WRITTEN: &str = "tidetable-forty";
/// The line of a call that was refused, at info.
const REFUSED: &str = "a tool of an agent on the workspace's server was refused or failed";
/// The line of every call, at debug.
const ANSWERED: &str = "what a tool of an agent on the workspace's server answered";

/// A workspace of `served` as [`Served::workspace`] sets one up, without its letter and without
/// the searches that wait for it: Ada's agent in it under no rule of its own, its chat, what
/// she tells the agent there, and the word processor open. The workspace, her page and her
/// message.
async fn set_up(served: &mut Served) -> (String, Browser, Id) {
    let (workspace, page) = served.opened.pop().expect("a workspace made at the start");
    let person = person_of(&served.ada);
    make_agent(&page, &served.workspaces, &workspace, &person, None).await;
    narrow_agent(&page, &served.workspaces, &workspace, &person, Vec::new()).await;
    let governor = served.workspaces.device_cert().clone();
    served
        .workspaces
        .prepare_application(&workspace, &governor, &crate::common::catalogue(), CHAT, "")
        .await
        .unwrap();
    let said = write_to_agent(
        &served.actions,
        &served.workspaces,
        &workspace,
        (&served.ada, &served.devices.1),
        HERS,
    )
    .await;
    let her_agent = on_a_turn(
        &Served::person(&served.ada, &served.devices.1, &workspace),
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &said),
        Vec::new(),
    );
    let open = serde_json::json!({"path": format!("{NAMED}#open"), "args": {}});
    let (text, failed) = served.tool(&workspace, &her_agent, "tilo_do", open).await;
    assert!(!failed, "the word processor opens: {text}");
    (workspace, page, said)
}

/// One call of a model's answer.
fn call(id: &str, name: &str, input: &serde_json::Value) -> Content {
    Content::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        input: input.clone(),
        extra_content: None,
    })
}

/// A model that answers with each of `steps` in order, then with words. What it was asked is
/// kept in `asked`.
fn scripted(steps: Vec<Vec<Content>>, asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    let asked = Arc::clone(asked);
    Arc::new(move || {
        let mut answers: Vec<ModelResponse> = steps
            .iter()
            .map(|calls| answer(calls.clone(), StopReason::ToolCalls, 1))
            .collect();
        answers.push(answer(
            vec![Content::Text {
                text: "That is all.".into(),
            }],
            StopReason::EndTurn,
            1,
        ));
        Some(Box::new(Scripted {
            answers,
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// Where `word` stands in `logged`: each line that holds it, up to where the word begins.
fn places_of<'a>(logged: &'a str, word: &str) -> Vec<&'a str> {
    logged
        .lines()
        .filter_map(|line| line.split_once(word).map(|(before, _)| before))
        .collect()
}

/// Each argument of `input` as a line names it: its name and the bytes of its value.
fn named_and_sized(input: &serde_json::Value) -> BTreeSet<String> {
    input
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, value)| format!("{name}:{}", value.to_string().len()))
        .collect()
}

/// The arguments the log's `line` names, each with its size.
fn arguments_in(line: &str) -> BTreeSet<String> {
    let (_, rest) = line
        .split_once(" arguments=")
        .unwrap_or_else(|| panic!("a call's line names its arguments: {line}"));
    rest.split(' ')
        .next()
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_call_is_logged_by_its_tool_and_its_arguments_names_and_sizes() {
    let lines = logged();
    let mut served = Served::start(1).await;
    let (workspace, page, said) = set_up(&mut served).await;
    let input = serde_json::json!({
        "path": format!("{NAMED}#{NO_SUCH}"),
        "args": {"text": format!("Pinned up by the quay: {UNWRITTEN}.")},
    });
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model = scripted(vec![vec![call("refused", "tilo_do", &input)]], &asked);
    answered(&served, &workspace, &said, model).await;
    let answers = tools_answered(&asked);
    assert_eq!(answers.len(), 1, "one tool answered: {answers:?}");
    // Its model reads the refusal whole, and the refusal names what the call asked for.
    eprintln!("the refusal as its model read it: {}", answers[0]);
    assert!(
        answers[0].contains(NO_SUCH),
        "the refusal names the action the call asked for: {answers:?}"
    );

    // Nothing of the call stands anywhere in the log; its own lines are those of its turn.
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    let turn = lines.of_turns_on(&workspace);
    let refused: Vec<&str> = turn
        .iter()
        .map(String::as_str)
        .filter(|line| line.contains(REFUSED))
        .collect();
    let told: Vec<&str> = turn
        .iter()
        .map(String::as_str)
        .filter(|line| line.contains(ANSWERED))
        .collect();
    assert_eq!(
        (refused.len(), told.len()),
        (1, 1),
        "the refused call has one line at info and, with debug on, one at debug"
    );
    assert!(refused[0].contains(" INFO "), "at info");
    assert!(told[0].contains("DEBUG "), "at debug");
    // Nothing the call wrote stands anywhere in the log, at any level.
    let mut places = places_of(&logged, NO_SUCH);
    places.extend(places_of(&logged, UNWRITTEN));
    assert!(
        places.is_empty(),
        "no value of a call's argument stands in the log; one stands after each of: {places:#?}"
    );
    // What the lines keep: the tool, and each argument by its name and its size.
    for line in [refused[0], told[0]] {
        assert!(
            line.contains("tilo_workspace_service::agents: ") && line.contains(" tool=tilo_do "),
            "{line}"
        );
        assert_eq!(
            arguments_in(line),
            named_and_sized(&input),
            "each argument by its name and the bytes of its value: {line}"
        );
    }
    // Of the refusal the line keeps its own sentence, and at debug the answer is its size.
    let (_, sentence) = refused[0]
        .split_once(" said=")
        .unwrap_or_else(|| panic!("the refusal's sentence stands in {}", refused[0]));
    let kept: Vec<&str> = sentence
        .split('…')
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .collect();
    assert!(
        !kept.is_empty() && kept.iter().all(|piece| answers[0].contains(piece)),
        "the refusal's own sentence as its model read it, with what the call wrote cut out: {}",
        refused[0]
    );
    assert_eq!(
        number(told[0], "answered_bytes"),
        u64::try_from(answers[0].len()).unwrap(),
        "an answer by its size alone: {}",
        told[0]
    );
    page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_that_succeeds_is_logged_without_what_it_wrote_or_its_tool_answered() {
    let lines = logged();
    let mut served = Served::start(1).await;
    let (workspace, page, said) = set_up(&mut served).await;
    let write = serde_json::json!({
        "path": format!("{NAMED}#append-paragraph"),
        "args": {"text": format!("Pinned up by the quay: {WRITTEN}.")},
    });
    let list = serde_json::json!({"path": NAMED});
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model = scripted(
        vec![
            vec![call("write", "tilo_do", &write)],
            vec![call("list", "tilo_ls", &list)],
        ],
        &asked,
    );
    answered(&served, &workspace, &said, model).await;
    let answers = tools_answered(&asked);
    assert_eq!(answers.len(), 2, "two tools answered: {answers:?}");
    eprintln!("what the two tools answered, as their model read it: {answers:#?}");
    // What a tool answered, a whole line of several words at a time: the listing's above all,
    // which no call of the turn wrote.
    let shown: Vec<&str> = answers
        .iter()
        .flat_map(|answer| answer.lines())
        .map(str::trim)
        .filter(|line| line.len() >= 16 && line.contains(' '))
        .collect();
    assert!(
        !shown.is_empty(),
        "a tool answered a whole line: {answers:?}"
    );

    // Nothing of either call stands anywhere in the log; their own lines are those of its turn.
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    let turn = lines.of_turns_on(&workspace);
    assert_eq!(
        turn.iter().filter(|line| line.contains(REFUSED)).count(),
        0,
        "neither call is refused: {answers:?}"
    );
    let told: Vec<&str> = turn
        .iter()
        .map(String::as_str)
        .filter(|line| line.contains(ANSWERED))
        .collect();
    assert_eq!(
        told.len(),
        2,
        "with debug on, each call has its line at debug"
    );
    // Nothing a call wrote and no line a tool answered stands anywhere in the log, at any level.
    let mut places = places_of(&logged, WRITTEN);
    for line in &shown {
        places.extend(places_of(&logged, line));
    }
    assert!(
        places.is_empty(),
        "no value of an argument and nothing of an answer stands in the log; something stands \
         after each of: {places:#?}"
    );
    // What a line keeps: the tool, each argument by its name and its size, the answer's size.
    for ((line, (tool, input)), answer) in told
        .iter()
        .zip([("tilo_do", &write), ("tilo_ls", &list)])
        .zip(&answers)
    {
        assert!(
            line.contains("DEBUG ")
                && line.contains("tilo_workspace_service::agents: ")
                && line.contains(&format!(" tool={tool} ")),
            "{line}"
        );
        assert_eq!(
            arguments_in(line),
            named_and_sized(input),
            "each argument by its name and the bytes of its value: {line}"
        );
        assert_eq!(
            number(line, "answered_bytes"),
            u64::try_from(answer.len()).unwrap(),
            "an answer by its size alone: {line}"
        );
    }
    page.peer.close();
}
