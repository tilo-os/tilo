#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "one scenario in order, on a fixture that is not sent between tasks"
)]
//! A person has one list of workspaces: the computer their device serves lists it, and a key of
//! theirs for all their workspaces reads the same list through `/v1`. Making a workspace is the
//! computer's own change, and a command whose grants ask the person first waits for them.

use serde_json::{Value, json};

use crate::language::host;

/// Where a key lists a person's workspaces: the account's action.
const LIST: &str = "/v1/resources/account/workspaces/actions/list";
/// Where a key makes one.
const CREATE: &str = "/v1/resources/account/workspaces/actions/create";
/// Where a desktop hands its workspaces to the device it runs on.
const IMPORT: &str = "/v1/resources/account/workspaces/actions/import";

/// The ids the `account/workspaces#list` answer lists.
fn ids(listed: &Value) -> Vec<String> {
    listed["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|workspace| workspace["id"].as_str().map(str::to_owned))
        .collect()
}

/// The computer's own list and the key's are one list, and the other computer of the person's
/// reads the key's workspaces through the device link; making a workspace is the computer's own
/// change, which no key makes through the API.
#[test]
fn a_computer_and_a_key_share_the_persons_workspace_list() {
    host::run(|host| async move {
        let key = host.key("Visa backend");
        let daemon = host.daemon();
        let computer =
            tilo_api::session::Here::workspaces(daemon.as_ref(), daemon.workspaces().device_cert())
                .into_iter()
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>();

        let (status, listed) = key.post(LIST, &json!({})).await;
        assert_eq!(status, 200, "{listed}");
        assert_eq!(ids(&listed["result"]), computer, "{listed}");

        // Another computer of hers reads the workspace the list names as she does.
        let tower = host.member("Ada's tower", host.workspace()).await;
        let provider = tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER;
        let at = |operation| {
            tilo_actions::remote::workspace_governance_address(host.workspace(), operation)
        };
        let read = tower.call(provider, at("member.list"), &json!({})).await;
        assert!(
            matches!(read, Ok(tilo_actions::InvocationUpdate::Succeeded { .. })),
            "the tower reads the workspace the list names: {read:?}"
        );

        // A key makes no workspace: making one is the computer's own change, and the computer
        // answers for it in Tilo itself.
        let (status, refused) = key
            .post(CREATE, &json!({"name": "Made by the backend"}))
            .await;
        assert_eq!(status, 403, "{refused}");
        assert!(
            refused.to_string().contains("Tilo"),
            "the key is told where a workspace is made: {refused}"
        );
        let (_, listed) = key.post(LIST, &json!({})).await;
        assert_eq!(
            ids(&listed["result"]),
            computer,
            "nothing was made: {listed}"
        );
    });
}

/// A desktop hands its person's workspaces to the device it runs on: the device that keeps them
/// answers, and it takes them from its own person's device and no key of theirs.
#[test]
fn importing_a_desktops_workspaces_reaches_the_device_that_takes_them_in() {
    host::run(|host| async move {
        let key = host.key("Claude Code");
        let (status, refused) = key.post(IMPORT, &json!({})).await;
        assert_eq!(status, 422, "{refused}");
        assert!(
            refused
                .to_string()
                .contains("only this device's own person takes its desktop's workspaces in"),
            "the device that keeps them answers, not a host without them: {refused}"
        );
    });
}

/// A command on a computer that does not keep the person's root, whose grants ask the person
/// first, answers that it waits for their approval; once they allow it where they keep the
/// root, what it asked is done.
#[test]
fn a_computer_without_the_root_says_it_waits_for_the_persons_approval() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        // The computer's agent asks the person to make a workspace; the question reaches them
        // on a device that keeps their root.
        let agent = host.agent();
        let asked = agent
            .tool_structured(
                "tilo_do",
                &json!({"workspace": workspace, "path": "account/workspaces#create",
                        "args": {"name": "Made while Ada was away"}}),
            )
            .await;
        assert_eq!(
            asked["structuredContent"]["status"], "pending",
            "the command waits for the person's approval: {asked}"
        );
    });
}
