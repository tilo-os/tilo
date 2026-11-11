//! A watch through the door: what it answers of changes, runs and presence.
use super::*;

/// A watch tells what happened since its cursor: a change to a resource and
/// who made it, a run that finished, and who is in the workspace.
#[test]
fn a_watch_answers_changes_runs_and_presence() {
    let host = Notes::new(true);
    run(async {
        let server = agent(&host);
        let (start, failed) = answer(&server, "tilo_watch", json!({"wait_ms": 50})).await;
        assert!(!failed, "{start}");
        assert_eq!(
            start["events"],
            json!([]),
            "a watch with no cursor starts now"
        );
        let cursor = start["cursor"].as_str().unwrap().to_owned();

        host.changed(json!({
            "change": "c1a2",
            "sentence": "added \"Buy milk\"",
            "by": "Ada",
            "time_ms": 1_730_000_000_000_i64,
        }));
        host.people(Some(vec![
            tilo_actions::remote::Here {
                who: "person:ada".into(),
                name: "Ada".into(),
                kind: "person".into(),
            },
            tilo_actions::remote::Here {
                who: "agent:claude".into(),
                name: "Claude Code".into(),
                kind: "agent".into(),
            },
        ]));
        let (ran, failed) = answer(
            &server,
            "tilo_do",
            json!({"path":"notes/Todo/body#append-paragraph","args":{"text":"x"}}),
        )
        .await;
        assert!(!failed, "{ran}");

        let (watched, failed) = answer(
            &server,
            "tilo_watch",
            json!({"path":"notes/Todo","cursor":cursor,"wait_ms":50}),
        )
        .await;
        assert!(!failed, "{watched}");
        let events = watched["events"].as_array().unwrap();
        let change = events
            .iter()
            .find(|event| event["kind"] == "change")
            .expect("the change is told");
        assert_eq!(change["path"], "notes/Todo");
        assert_eq!(change["change"], "c1a2");
        assert_eq!(change["sentence"], "added \"Buy milk\"");
        assert_eq!(change["by"], "Ada");
        let run = events
            .iter()
            .find(|event| event["kind"] == "run")
            .unwrap_or_else(|| panic!("the run is told: {watched}"));
        assert_eq!(run["invocation_id"], ran["invocation_id"]);
        assert_eq!(run["path"], "notes/Todo/body#append-paragraph");
        assert_eq!(run["phase"], "succeeded");
        assert_eq!(run["run"], "MCP agent");
        let here = events
            .iter()
            .find(|event| event["kind"] == "presence")
            .expect("who is here is told");
        assert!(
            here["here"]
                .as_array()
                .unwrap()
                .iter()
                .any(|member| member["name"] == "Claude Code" && member["kind"] == "agent"),
            "{here}"
        );
        // Everything it told is told once.
        let (again, failed) = answer(
            &server,
            "tilo_watch",
            json!({"path":"notes/Todo","cursor":watched["cursor"].as_str().unwrap(),"wait_ms":50}),
        )
        .await;
        assert!(!failed, "{again}");
        assert_eq!(again["events"], json!([]), "{again}");
    });
}
