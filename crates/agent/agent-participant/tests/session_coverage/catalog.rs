//! An agent told only the relevant apps' operations: its instructions list
//! every granted app in a line, a system message describes the one the
//! task is about, and a lookup brings in another's, with the same prefix.
//! An agent told every app's keeps describing an app once it described it.

use tilo_agent_participant::ModelPlanner;
use tilo_agent_participant::profile::Catalog;
use tilo_relay_protocol::agents::TaskState;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use super::{
    CANVAS, CHAT, MESSAGES, SHEETS, endpoint_planner, enrol, message_text, model_endpoint, offers,
    planning_agent, run_task, set_cells_plan, start_host, steward, tool_call,
};

const NOTES: &str = "so.tilo.notes";

/// A planner at `url` told only the relevant apps.
fn relevant_planner(url: &str) -> Box<dyn tilo_agent_participant::Planner> {
    Box::new(
        ModelPlanner::new(Box::new(tilo_agent_participant::OpenAiModel::new(
            url,
            "canned",
            tilo_agent_participant::ApiKey::None,
            tilo_test_env::hang_bound(),
            tilo_model_gateway::openai::Target::Standard,
        )))
        .cataloguing(Catalog::Relevant),
    )
}

/// The names of the tools a request offers.
fn tools(request: &serde_json::Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

/// A task about Sheets, by an agent granted Sheets, Canvas and Notes: its
/// instructions list all three and Sheets alone is described; the model looks
/// Notes up, is answered with its operations, and plans the cells in the
/// same conversation, whose second request starts with the same bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_is_told_its_apps_in_full_and_looks_up_another() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let set_cells = "sheets.workbook#set-cells";
    let (url, sent) = model_endpoint(vec![
        tool_call(
            "l1",
            "list_operations",
            &serde_json::json!({"application": NOTES}),
        ),
        tool_call(
            "p1",
            "plan",
            &serde_json::json!({"steps": [{"operation": set_cells,
                "arguments": {"sheet": "00000000-0000-4000-8000-000000000001", "start": "A1",
                    "rows": [["Visits", "Signups"], [240, 18]]}}],
                "then": ""}),
        ),
    ]);
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[SHEETS, CANVAS, NOTES],
        relevant_planner(&url),
        tilo_content::ContentStore::memory(),
    )
    .await;

    let info = run_task(
        &mut agent,
        &control,
        "In Sheets, enter Visits and Signups in the first row and 240 and 18 under them",
    );
    assert_eq!(info.state, TaskState::Completed, "{info:?}");
    let actions: Vec<&str> = info.steps.iter().map(|step| step.action.as_str()).collect();
    assert_eq!(actions, ["set-cells"], "{info:?}");
    let requests: Vec<serde_json::Value> = sent
        .try_iter()
        .map(|body| serde_json::from_str(&body).unwrap())
        .collect();
    assert_eq!(requests.len(), 2, "the lookup and the plan");
    // The instructions with the line per app, the reference, the request.
    let (first, second) = (&requests[0], &requests[1]);
    let text = |at: usize| message_text(&first["messages"][at]);
    let (instructions, reference, turn) = (text(0), text(1), text(2));
    assert_eq!(first["messages"][1]["role"], "system");
    for app in [SHEETS, CANVAS, NOTES] {
        assert!(
            instructions.contains(&format!("\n- {app} (")),
            "{app}: {instructions}"
        );
    }
    assert!(reference.contains(&format!("## {SHEETS}\n")), "{reference}");
    assert!(offers(&turn, set_cells), "{turn}");
    assert!(
        reference.contains(&format!("{set_cells} — ")),
        "{reference}"
    );
    for left in [CANVAS, NOTES] {
        assert!(
            !reference.contains(&format!("## {left}\n")),
            "{left}: {reference}"
        );
    }
    assert!(tools(first).contains(&"list_operations".to_owned()));
    let answered = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "tool")
        .map(message_text)
        .unwrap();
    assert!(
        answered.contains(&format!("What {NOTES} can do")) && answered.contains("notes."),
        "{answered}"
    );
    assert_eq!(first["tools"], second["tools"]);
    for at in 0..3 {
        assert_eq!(first["messages"][at], second["messages"][at]);
    }

    agent.close();
    host.relay.shutdown().await;
}

/// An agent granted more apps than its reference describes at once (mode
/// `all`, more than four) keeps describing an app once a plan was told it:
/// a task naming Notes adds it beside Sheets, and the next task, about
/// Sheets alone, opens with the same system messages and tools, which a
/// provider reads from its cache.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reference_keeps_the_apps_it_described() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let (url, sent) = model_endpoint(
        ["A1", "A2", "A3"]
            .map(|cell| set_cells_plan(&[], &[("Sheet1", cell, "x")]))
            .to_vec(),
    );
    let (mut agent, control) = planning_agent(
        &root,
        (&host, &host_device),
        &[SHEETS, CANVAS, NOTES, CHAT, MESSAGES],
        endpoint_planner(&url),
        tilo_content::ContentStore::memory(),
    )
    .await;
    agent.open(SHEETS, None).unwrap();
    for task in [
        "In Sheets, put x in A1",
        "In Sheets, put x in A2, as the Notes page says",
        "In Sheets, put x in A3",
    ] {
        let info = run_task(&mut agent, &control, task);
        assert_eq!(info.state, TaskState::Completed, "{task}: {info:?}");
    }
    let requests: Vec<serde_json::Value> = sent
        .try_iter()
        .map(|body| serde_json::from_str(&body).unwrap())
        .collect();
    assert_eq!(requests.len(), 3, "one plan a task");
    let reference = |at: usize| {
        let message = &requests[at]["messages"][1];
        assert_eq!(message["role"], "system");
        message_text(message)
    };
    let described = |at: usize, app: &str| reference(at).contains(&format!("## {app}\n"));
    assert!(
        described(0, SHEETS) && !described(0, NOTES),
        "{}",
        reference(0)
    );
    assert!(
        described(1, SHEETS) && described(1, NOTES),
        "{}",
        reference(1)
    );
    assert_eq!(requests[0]["messages"][0], requests[2]["messages"][0]);
    let system = |at: usize| requests[at]["messages"].as_array().unwrap()[..2].to_vec();
    assert_eq!(system(1), system(2));
    assert_eq!(requests[1]["tools"], requests[2]["tools"]);

    agent.close();
    host.relay.shutdown().await;
}
