//! Every tool step of a turn on a workspace's server says in the server's log what it cost: the
//! tool, its whole time and the time of its parts, the bytes it answered its model with, and
//! whether its action needs its person. A line names no argument, no answer and no word of a
//! person's.
use super::*;
use crate::workspace_actions::hosted_secret_use::logged;

/// A word only the turn's search asks for.
const SOUGHT: &str = "zanzibar-quince";
/// A word only the paragraph the turn writes holds.
const PUT_DOWN: &str = "cloudberry-seven";
/// The app the turn lists and writes in, as its model names it.
const NAMED: &str = "word-processor";

/// The number the field `name` of a timing line holds.
fn number(line: &str, name: &str) -> u64 {
    let (_, rest) = line
        .split_once(&format!(" {name}="))
        .unwrap_or_else(|| panic!("{name} stands in {line}"));
    rest.split(' ')
        .next()
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("{name} is a number in {line}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_tool_step_of_a_turn_says_what_it_cost_and_nothing_it_was_asked() {
    let lines = logged();
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    let call = |id: &str, name: &str, input: serde_json::Value| {
        Content::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            extra_content: None,
        })
    };
    // One answer with two calls, a search and a listing; then one with a write; then words.
    let first = vec![
        call("find", "tilo_find", serde_json::json!({"query": SOUGHT})),
        call("list", "tilo_ls", serde_json::json!({"path": NAMED})),
    ];
    let second = vec![call(
        "write",
        "tilo_do",
        serde_json::json!({"path": format!("{NAMED}#append-paragraph"),
            "args": {"text": format!("The marmalade is ordered: {PUT_DOWN}.")}}),
    )];
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model: Model = {
        let asked = Arc::clone(&asked);
        Arc::new(move || {
            Some(Box::new(Scripted {
                answers: vec![
                    answer(first.clone(), StopReason::ToolCalls, 1),
                    answer(second.clone(), StopReason::ToolCalls, 1),
                    answer(
                        vec![Content::Text {
                            text: "I looked, and wrote it down.".into(),
                        }],
                        StopReason::EndTurn,
                        1,
                    ),
                ],
                asked: Arc::clone(&asked),
            })
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        })
    };
    answered(&served, &workspace, &said, model).await;
    let answers = tools_answered(&asked);
    assert_eq!(answers.len(), 3, "three tools answered: {answers:?}");

    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    let timing: Vec<&str> = logged
        .lines()
        .filter(|line| line.contains("timing tool"))
        .collect();
    assert_eq!(
        timing.len(),
        3,
        "one line for each tool step of the turn: {timing:#?}"
    );
    for (at, tool) in ["tilo_find", "tilo_ls", "tilo_do"].into_iter().enumerate() {
        let line = timing[at];
        assert!(
            line.contains("tilo_server::workspace_agents: timing tool")
                && line.contains(&format!(" tool={tool} ")),
            "{line}"
        );
        assert_eq!(
            number(line, "result_bytes"),
            u64::try_from(answers[at].len()).unwrap(),
            "the bytes its model read back: {line}"
        );
        let (whole, note, tool_time) = (
            number(line, "whole_ms"),
            number(line, "note_ms"),
            number(line, "tool_ms"),
        );
        assert!(
            whole >= note && whole >= tool_time,
            "a step holds its note and its tool: {line}"
        );
        assert!(
            number(line, "tool_ms") >= number(line, "own_ms"),
            "a tool's own work is a part of it: {line}"
        );
        assert!(
            line.contains(" outcome=ok ")
                && line.contains(" needs_person=false ")
                && line.contains(" pending=false"),
            "nothing of it failed or waits for a person: {line}"
        );
        for kept_out in [SOUGHT, PUT_DOWN, NAMED, "heron", "Gooseberry", "marmalade"] {
            assert!(
                !line.contains(kept_out),
                "a timing line holds no argument, no answer and no word of a person's: {line}"
            );
        }
    }
    // Each part is counted where its tool asked the workspace's action service for it, and each
    // line says which call of its answer it was.
    assert_eq!(number(timing[0], "searches"), 1, "{}", timing[0]);
    assert!(number(timing[1], "lists") >= 1, "{}", timing[1]);
    assert_eq!(number(timing[1], "searches"), 0, "{}", timing[1]);
    assert!(number(timing[2], "admits") >= 1, "{}", timing[2]);
    let places: Vec<(u64, u64)> = timing
        .iter()
        .map(|line| (number(line, "call"), number(line, "calls")))
        .collect();
    assert_eq!(places, [(1, 2), (2, 2), (1, 1)], "{timing:#?}");
    page.peer.close();
}
