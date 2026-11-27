#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "one scenario in order, on a fixture that is not sent between tasks"
)]
//! A person has one list of workspaces: the computer their device serves lists it, and a key of
//! theirs for all their workspaces reads the same list through `/v1`. That key makes a workspace
//! as its person, and a key for one workspace makes no other.

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
/// reads the key's workspaces through the device link; the key, whose grant reaches all of its
/// person's workspaces, makes one as they would, and both list it.
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
        let read = tower.members(host.workspace()).await;
        assert!(
            read.is_ok_and(|people| !people.is_empty()),
            "the tower reads the workspace the list names"
        );

        // The person signed the key's grant for all their workspaces, and that signature is
        // their yes to making one: the key makes it here, and the one list names it for both.
        let (status, made) = key
            .post(CREATE, &json!({"name": "Made by the backend"}))
            .await;
        assert_eq!(status, 200, "{made}");
        let id = made["result"]["workspace"]["id"]
            .as_str()
            .expect("the made workspace");
        let (_, listed) = key.post(LIST, &json!({})).await;
        let now =
            tilo_api::session::Here::workspaces(daemon.as_ref(), daemon.workspaces().device_cert())
                .into_iter()
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>();
        assert_eq!(ids(&listed["result"]), now, "{listed}");
        assert!(now.iter().any(|listed| listed == id), "{listed}");
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

/// A key the person made for one workspace makes no other: it is told it works in one, and
/// nothing is made.
#[test]
fn a_one_workspace_key_makes_no_other_workspace() {
    host::run(|host| async move {
        let key = host.key_reaching(host.workspace(), "Visa backend, one workspace");
        let (_, before) = host.key("Lister").post(LIST, &json!({})).await;
        let (status, refused) = key
            .post(CREATE, &json!({"name": "Made while Ada was away"}))
            .await;
        assert_eq!(status, 403, "{refused}");
        assert!(
            refused
                .to_string()
                .contains("this key only works in one workspace"),
            "the key is told why: {refused}"
        );
        let (_, after) = host.key("Lister").post(LIST, &json!({})).await;
        assert_eq!(
            ids(&after["result"]),
            ids(&before["result"]),
            "nothing was made"
        );
    });
}
