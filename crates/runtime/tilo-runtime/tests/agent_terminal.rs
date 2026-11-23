//! The workspace's Terminal reached by an agent on the person's computer: a command it names
//! runs in the workspace and is recorded on the workspace's Terminal session, a long run shows
//! what it printed and ends on the agent's cancel, and what it opens on a terminal is recorded
//! as it is typed there.

use serde_json::json;

use crate::agent_fixture::{act, said};
use crate::host;

/// The agent names a command: the host opens its session, runs it in the workspace's shell, and
/// answers with the run; the same session keeps its place for the next command, and the browser
/// sees both recorded on the session as the agent's run.
#[test]
fn an_agent_runs_a_command_in_its_workspace_in_one_step() {
    host::run(|host| async move {
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

/// A window of another device opened the workspace's Terminal session once and closed: the
/// session still names that device, and the agent's command still lands in the workspace's shell
/// and is recorded on that session.
#[test]
fn an_agents_command_lands_after_another_devices_terminal_closed() {
    host::run(|host| async move {
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
            json!({"command": "echo ran on the workspace's shell"}),
        )
        .await;
        assert!(
            !failed && ran.contains("ran on the workspace's shell") && ran.contains("\"exit\":0"),
            "the command runs on the workspace's shell after the other device's Terminal closed: \
             {ran}"
        );
        let (session, failed) = said(
            &agent
                .tool("tilo_read", &json!({"path": "terminal/Terminal session"}))
                .await,
        );
        assert!(
            !failed && session.contains("echo ran on the workspace's shell"),
            "the command is recorded on the session: {session}"
        );
    });
}

/// A build that prints and then takes minutes is not silent: the call answers pending with what
/// it printed, `tilo_status` shows it as it stands, and the agent's cancel ends it, recording
/// the run it stopped.
#[test]
fn an_agents_long_run_shows_its_output_and_its_cancel_ends_it() {
    host::run(|host| async move {
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
    host::run(|host| async move {
        let agent = host.agent();
        let (opened, failed) = act(
            &agent,
            "terminal#open-terminal",
            json!({"cols": 100, "rows": 30}),
        )
        .await;
        assert!(
            !failed && opened.contains("stream"),
            "the agent opens a terminal on the workspace's session: {opened}"
        );
        // What is typed on the stream reaches the workspace's shell, and the host records the
        // command on the session once, as the agent's.
        let (session, failed) = said(
            &agent
                .tool("tilo_read", &json!({"path": "terminal/Terminal session"}))
                .await,
        );
        assert!(
            !failed && session.contains("\"actor\":{\"agent\""),
            "the session is read as the agent's: {session}"
        );
    });
}
