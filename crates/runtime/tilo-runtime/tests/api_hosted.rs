#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "one scenario in order, on a fixture that is not sent between tasks"
)]
//! What an API server in front of people's workspaces answers a key: a webhook posted to its
//! receiver signed; a call reaching the server that keeps the workspace, though another
//! answers; the same key working at every API server and after the one it used goes away; a
//! typed refusal when nothing that can read the workspace is online; a computer's one
//! session; its agent's grants; and what the workspace keeps for its members.

use serde_json::{Value, json};
use tilo_actions::remote::{person_models, person_vault};

use crate::host;

/// A hosted API posts a workspace's event to a webhook, signed with the secret its maker was
/// answered once, and lists it as one that is delivered.
#[test]
fn a_hosted_apis_webhook_reaches_its_receiver_signed() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        // The receiver is a server this test starts on this machine; the API is handed it to
        // stand in for the public internet.
        let (status, hook) = key
            .post(
                "/v1/resources/account/webhooks/actions/create",
                &json!({"workspace": workspace,
                        "url": "http://127.0.0.1:9/hook",
                        "events": ["invocation"]}),
            )
            .await;
        assert_eq!(status, 200, "the webhook is kept: {hook}");
        let (opened, body) = key
            .post(
                &format!("/v1/workspaces/{workspace}/resources/pdf-viewer/actions/open"),
                &json!({}),
            )
            .await;
        assert_eq!(opened, 200, "{body}");
        panic!(
            "the daemon keeps no account webhooks and posts no deliveries: its person's \
             programs reach the workspace they keep, and there is no hosted API in front of it"
        );
    });
}

/// A key's call reaches the server that keeps the person's workspace: a device of theirs asks
/// the workspace's own tree, and the person's workspace is listed where it is kept.
#[test]
fn a_call_reaches_the_host_that_keeps_the_workspace() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let laptop = host.member("Ada's laptop", &workspace).await;
        let listed = laptop
            .call(
                tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
                tilo_actions::remote::workspace_governance_address(&workspace, "server.list"),
                &json!({}),
            )
            .await;
        assert!(
            matches!(listed, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "a call for the workspace reaches the one that keeps it: {listed:?}"
        );
        let (status, kept) = key
            .post("/v1/resources/account/workspaces/actions/list", &json!({}))
            .await;
        assert_eq!(status, 200, "{kept}");
        assert!(
            kept["result"]["workspaces"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == workspace.as_str())),
            "her workspaces are listed where they are kept: {kept}"
        );
    });
}

/// The same key is served wherever its person's workspaces are: two keys of the person's,
/// made at different times, read the same workspace and list the same workspaces.
#[test]
fn a_key_works_at_every_api_server() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let first = host.key("Visa backend");
        let elsewhere = host.key("Visa backend, again");
        for key in [&first, &elsewhere] {
            let (status, listed) = key
                .post("/v1/resources/account/workspaces/actions/list", &json!({}))
                .await;
            assert_eq!(status, 200, "{listed}");
            let (status, read) = key
                .get(&format!("/v1/workspaces/{workspace}/resources"))
                .await;
            assert_eq!(status, 200, "{read}");
        }
    });
}

/// The server a call used goes away; another server of the person's serves the same key,
/// because the key is the grant and every server admits it.
#[test]
fn a_call_is_served_after_the_host_it_used_goes_away() {
    host::run(|host| async move {
        let key = host.key("Visa backend");
        let (status, listed) = key
            .post("/v1/resources/account/workspaces/actions/list", &json!({}))
            .await;
        assert_eq!(status, 200, "{listed}");
        panic!(
            "the daemon-host fixture starts one device per test and has no restart, and a \
             device's API keeps no list of servers to fail over to"
        );
    });
}

/// Nothing that can read the workspace is online: the call is refused in plain words, with
/// what to do about it.
#[test]
fn no_host_that_can_read_the_workspace_is_a_typed_refusal() {
    host::run(|_host| async move {
        panic!(
            "the daemon-host fixture serves the workspace from its own process; there is no \
             list of servers to name one that does not answer"
        );
    });
}

/// A computer `tilo login` made a device of keeps that device's one session with its server,
/// and its commands are calls to the API the computer serves over that session.
#[test]
fn a_logged_in_computers_commands_go_over_its_one_session() {
    host::run(|_host| async move {
        panic!(
            "the fixture's daemon has no login and has no session with a server; its person's \
             programs reach the API served over the daemon's own workspace"
        );
    });
}

/// The computer's agent, over the session its computer keeps, reads a workspace's people and
/// invites someone; its person is asked first, and their answer stands.
#[test]
fn a_logged_in_computers_agent_reads_and_invites_people_as_its_grants_let_it() {
    host::run(|host| async move {
        let agent = host.agent();
        let (listed, failed) = said(
            &agent
                .tool("tilo_do", &json!({"path": "workspace/members#list"}))
                .await,
        );
        assert!(!failed, "the agent lists the workspace's people: {listed}");
        assert!(listed.contains(host.person()), "{listed}");

        // Inviting someone asks the person first: allowed, the link comes back.
        let invited = agent
            .tool_structured(
                "tilo_do",
                &json!({"path": "workspace/invitations#invite", "args": {"role": "editor"}}),
            )
            .await;
        assert_eq!(
            invited["structuredContent"]["status"], "pending",
            "its person is asked first: {invited}"
        );
    });
}

/// The text of an MCP tool's result, and whether it failed.
fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, result["isError"].as_bool().unwrap_or(false))
}

/// What the workspace keeps (what its members sign in with, their model keys) answers its
/// members on devices of theirs, and a key a person made for a script as their agent there.
#[test]
fn a_key_made_for_a_script_reaches_the_workspaces_stores_as_its_persons_agent() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let list = person_vault::target(person_vault::LIST);
        let kept = laptop
            .call(&list.provider, list.address.clone(), &json!({}))
            .await;
        assert!(
            matches!(kept, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "her own device lists the workspace's vault: {kept:?}"
        );

        let with_key = host.member("Nightly build", &workspace).await;
        let listed = with_key
            .call(&list.provider, list.address.clone(), &json!({}))
            .await;
        assert!(
            matches!(listed, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "her key, as her agent, lists the workspace's vault: {listed:?}"
        );
        for (what, target, argument) in [
            (
                "adds to the vault",
                person_vault::target(person_vault::ADD),
                json!({"kind": "login",
                       "belongsTo": ["https://mail.example.test"],
                       "username": "ada@example.test",
                       "secret": "correct horse battery staple"}),
            ),
            (
                "sets a model key",
                person_models::target(person_models::SET_KEY),
                json!({"provider": "a-provider", "key": "sk-test-not-a-key"}),
            ),
        ] {
            let answered = with_key
                .call(&target.provider, target.address, &argument)
                .await;
            assert!(
                !matches!(
                    answered,
                    Err(tilo_actions::remote::ActionCallError::UnknownAction)
                ),
                "her key, as her agent, {what}: {answered:?}"
            );
        }
    });
}
