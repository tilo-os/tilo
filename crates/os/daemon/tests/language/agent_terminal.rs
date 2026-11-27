//! The workspace's Terminal reached by an agent on the person's computer: a command it names
//! runs in the workspace and is recorded on the workspace's Terminal session, a long run shows
//! what it printed and ends on the agent's cancel, and what it opens on a terminal is recorded
//! as it is typed there. What a command printed stays on its record, the end of a long run kept
//! by the host and named there.

use serde_json::{Value, json};
use tilo_workspace_service::workspaces::terminal::RECORDED_OUTPUT;

use crate::language::agent_fixture::{act, open_terminal_allowed, read_json, said};
use crate::language::host;

/// The agent names a command: the host opens its session, runs it in the workspace's shell, and
/// answers with the run; the same session keeps its place for the next command, and the browser
/// sees both recorded on the session as the agent's run.
#[test]
fn an_agent_runs_a_command_in_its_workspace_in_one_step() {
    host::run_with(&["terminal"], |host| async move {
        let agent = host.agent();
        let (ran, failed) = act(
            &agent,
            "terminal#run",
            json!({"command": "echo hello from the host && mkdir src && echo 'fn main() {}' > src/main.rs && ls src"}),
        )
        .await;
        assert!(
            !failed && ran.contains("hello from the host") && ran.contains("\"exit\":0"),
            "the daemon runs the workspace's Terminal in its own shell: {ran}"
        );
        let (again, failed) =
            act(&agent, "terminal#run", json!({"command": "cd src && pwd"})).await;
        assert!(
            !failed && again.contains("/src"),
            "the same session keeps its place: {again}"
        );
    });
}

/// An agent never runs in the device's own shell: it names this computer's own shell as where
/// its command runs, and the command is refused in words and nothing runs.
#[test]
fn an_agent_is_refused_the_devices_own_shell() {
    host::run_with(&["terminal"], |host| async move {
        let agent = host.agent();
        let (ran, failed) = act(
            &agent,
            "terminal#run",
            json!({"command": "echo never", "target": "host"}),
        )
        .await;
        assert!(
            failed,
            "an agent's command in the device's own shell is refused: {ran}"
        );
        assert!(
            ran.contains("an agent never runs commands in the device's own shell"),
            "the refusal says why: {ran}"
        );
    });
}

/// A window of another device opened the workspace's Terminal session once and closed: the
/// session still names that device, and the agent's command still lands in the workspace's shell
/// and is recorded on that session.
#[test]
fn an_agents_command_lands_after_another_devices_terminal_closed() {
    host::run_with(&["terminal"], |host| async move {
        // The person's other device opened the Terminal once, as a window of theirs does.
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let opened = laptop
            .call(
                tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
                tilo_actions::remote::workspace_governance_address(&workspace, "invitation.list"),
                &json!({}),
            )
            .await;
        assert!(
            matches!(opened, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "the person's laptop reaches the workspace: {opened:?}"
        );

        let agent = host.agent();
        let (ran, failed) = act(
            &agent,
            "terminal#run",
            json!({"command": "echo \"ran on the workspace's shell\""}),
        )
        .await;
        assert!(
            !failed && ran.contains("ran on the workspace's shell") && ran.contains("\"exit\":0"),
            "the command runs on the workspace's shell after the other device's Terminal closed: \
             {ran}"
        );
        let recorded = recorded_command(&agent, "echo \"ran on the workspace's shell\"").await;
        assert_eq!(
            recorded["exit"],
            json!(0),
            "the command is recorded on the session: {recorded}"
        );
    });
}

/// A build that prints and then takes minutes is not silent: the call answers pending with what
/// it printed, `tilo_status` shows it as it stands, and the agent's cancel ends it, recording
/// the run it stopped.
#[test]
fn an_agents_long_run_shows_its_output_and_its_cancel_ends_it() {
    host::run_with(&["terminal"], |host| async move {
        let agent = host.agent();
        let invocation = "3f6b8c0e-0000-4000-8000-000000000009";
        let (ran, failed) = said(
            &agent
                .tool(
                    "tilo_do",
                    &json!({"path": "terminal#run",
                            "args": {"command": "echo building && sleep 300 && echo never"},
                            "invocation_id": invocation,
                            "wait_ms": 2_000}),
                )
                .await,
        );
        assert!(failed, "a run still going is no success: {ran}");
        assert!(
            ran.contains("pending") && ran.contains("building") && !ran.contains("never"),
            "the pending answer shows what the run printed: {ran}"
        );
        let (status, failed) = said(
            &agent
                .tool("tilo_status", &json!({"invocation_id": invocation}))
                .await,
        );
        assert!(
            failed && status.contains("building") && status.contains("still running"),
            "the pending run says what it printed: {status}"
        );
        let _ = said(
            &agent
                .tool(
                    "tilo_status",
                    &json!({"invocation_id": invocation, "cancel": true}),
                )
                .await,
        );
        let (status, _) = said(
            &agent
                .tool("tilo_status", &json!({"invocation_id": invocation}))
                .await,
        );
        assert_eq!(
            status.trim(),
            "cancelled",
            "the agent's cancel ends it: {status}"
        );
    });
}

/// The agent opens the workspace's Terminal session on a terminal: what is typed there on its
/// computer is recorded on the session by the host, as the commands it runs are, once each.
#[test]
fn a_command_typed_on_a_terminal_the_agent_opened_is_recorded_on_the_session() {
    host::run_with(&["terminal"], |host| async move {
        let agent = host.agent();
        let (ran, failed) = act(&agent, "terminal#run", json!({"command": "echo typed-42"})).await;
        assert!(
            !failed && ran.contains("typed-42"),
            "the command runs: {ran}"
        );
        // A terminal reaches beyond the workspace's records: its person allows it first.
        open_terminal_allowed(
            host.as_ref(),
            &agent,
            "3f6b8c0e-0000-4000-8000-00000000000b",
            json!({"cols": 100, "rows": 30}),
        )
        .await;
        // The command its shell ran reaches the workspace's shell, and the host records it on
        // the session once, as the agent's.
        let recorded = recorded_command(&agent, "echo typed-42").await;
        assert!(
            names_an_agent(&recorded["actor"]),
            "the session records the command as the agent's: {recorded}"
        );
        let rows = command_rows(&agent, "echo typed-42").await;
        assert_eq!(rows.len(), 1, "the command is recorded once: {rows:?}");
    });
}

/// Whether `actor`, an actor as a record names it, is an agent's.
fn names_an_agent(actor: &Value) -> bool {
    match actor {
        Value::Object(fields) => {
            fields.contains_key("agent") || fields.values().any(names_an_agent)
        }
        _ => false,
    }
}

/// The value of the command recorded as `command` in `read`, a record as `tilo_read` answers
/// it: the first object anywhere in it whose `command` is that line.
fn recorded<'a>(read: &'a Value, command: &str) -> Option<&'a Value> {
    match read {
        Value::Object(fields) if fields.get("command").and_then(Value::as_str) == Some(command) => {
            Some(read)
        }
        Value::Object(fields) => fields.values().find_map(|value| recorded(value, command)),
        Value::Array(values) => values.iter().find_map(|value| recorded(value, command)),
        _ => None,
    }
}

/// The rows of the workspace's Terminal session that ran `command`, as `tilo_ls` lists them.
async fn command_rows(agent: &host::Agent, command: &str) -> Vec<Value> {
    let listed = agent
        .tool_structured(
            "tilo_ls",
            &json!({"path": "terminal/Terminal", "kind": "resources"}),
        )
        .await;
    assert_ne!(
        listed["isError"], true,
        "the agent lists the session: {listed}"
    );
    listed["structuredContent"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["label"] == command)
        .cloned()
        .collect()
}

/// The command recorded as `command` on the workspace's Terminal session, once it is: its own
/// record, read where the session lists it.
async fn recorded_command(agent: &host::Agent, command: &str) -> Value {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let rows = command_rows(agent, command).await;
        if let Some(path) = rows.first().and_then(|row| row["path"].as_str()) {
            let read = read_json(agent, path).await;
            return recorded(&read, command)
                .unwrap_or_else(|| panic!("the record is of {command}: {read}"))
                .clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{command} is never recorded on the session"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// What a command printed is on its record, for history, search and the workspace's other
/// members, not only in the answer to whoever ran it: a short output in the record itself, and
/// the end of a run that printed more than a record keeps kept by the host the record names it
/// on, which its members are answered with.
#[test]
fn a_commands_output_is_kept_on_its_record() {
    host::run_with(&["terminal"], |host| async move {
        let agent = host.agent();
        let short = "echo test result: ok. 1 passed";
        let (ran, failed) = act(&agent, "terminal#run", json!({"command": short})).await;
        assert!(
            !failed && ran.contains("\"exit\":0"),
            "the command runs: {ran}"
        );
        let command = recorded_command(&agent, short).await;
        assert_eq!(
            command["output"]["text"], "test result: ok. 1 passed\n",
            "a short output is in the record: {command}"
        );
        assert_eq!(command["truncated"], json!(false), "{command}");

        // A run that prints past what is kept of it: its record keeps the end, where the
        // summary is.
        let long = format!(
            "head -c {} /dev/zero | tr '\\0' y && echo && echo the summary at the end",
            RECORDED_OUTPUT + 4096
        );
        let (ran, failed) = act(&agent, "terminal#run", json!({"command": long})).await;
        assert!(
            !failed && ran.contains("the summary at the end"),
            "the answer carries the end of the run: {ran}"
        );
        let command = recorded_command(&agent, &long).await;
        assert_eq!(command["truncated"], json!(true), "{command}");
        let len = command["output"]["bytes"]
            .as_u64()
            .and_then(|len| usize::try_from(len).ok())
            .unwrap_or_else(|| {
                panic!("the record says how much of the output it keeps: {command}")
            });
        assert!(
            len > 64 * 1024 && len <= RECORDED_OUTPUT,
            "the record keeps a bounded end of the run: {len} bytes"
        );
        // What it keeps is the end of what the run printed, kept by the host the record names
        // it on, which the workspace's members are answered with.
        let mut printed = vec![b'y'; RECORDED_OUTPUT + 4096];
        printed.extend_from_slice(b"\nthe summary at the end\n");
        let end = &printed[printed.len() - len..];
        let workspace =
            tilo_desktop_model::WorkspaceId::parse(host.workspace()).expect("a workspace id");
        let kept = host
            .daemon()
            .workspaces()
            .kept_record_content(workspace, ui::apps::TERMINAL, &blake3::hash(end).to_hex())
            .await
            .unwrap_or_else(|| panic!("the host keeps the end of the output: {command}"));
        let (ys, summary) = kept.split_at(kept.len() - "\nthe summary at the end\n".len());
        assert_eq!(summary, b"\nthe summary at the end\n");
        assert!(
            ys.iter().all(|byte| *byte == b'y'),
            "only the end of the run is kept"
        );
    });
}

/// An agent's command is kept as the app wrote it with no window open: the app answers the
/// action, its save of the change follows the answer, and the session records the command as the
/// agent's.
#[test]
fn an_agents_call_to_the_terminal_app_with_no_window_keeps_what_it_wrote() {
    host::run_with(&["terminal"], |host| async move {
        let agent = host.agent();
        let command = "echo kept after the app answered";
        let (ran, failed) = act(&agent, "terminal#run", json!({"command": command})).await;
        assert!(
            !failed && ran.contains("\"exit\":0"),
            "the command runs: {ran}"
        );
        let recorded = recorded_command(&agent, command).await;
        assert_eq!(
            recorded["exit"],
            json!(0),
            "the session keeps the command the app wrote: {recorded}"
        );
        assert!(
            names_an_agent(&recorded["actor"]),
            "the session records the agent that asked: {recorded}"
        );
    });
}
