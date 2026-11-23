//! A person has one list of workspaces: a key of theirs for all their workspaces (over `/v1`)
//! lists the one list the daemon keeps for the person, reads what it lists, and makes one the
//! computer it runs on answers for.

use serde_json::{Value, json};

use crate::host;

/// Where a key makes a workspace and lists them: the account's actions.
const CREATE: &str = "/v1/resources/account/workspaces/actions/create";
const LIST: &str = "/v1/resources/account/workspaces/actions/list";
/// The ids of the workspaces an `account/workspaces#list` answer lists.
fn ids(listed: &Value) -> Vec<String> {
    listed["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|workspace| workspace["id"].as_str().map(str::to_owned))
        .collect()
}

/// The key lists what the computer lists, reads it, and makes one, which the computer holds.
#[test]
fn a_key_lists_reads_and_makes_the_workspaces_its_computer_holds() {
    host::run(|host| async move {
        let key = host.key("Visa backend");
        // The computer's own list, as `tilo` reads it on it.
        let (_, own) = host.certify(
            "this computer's tilo",
            tilo_user_identity::DeviceCapabilities::agent_grant(
                tilo_user_identity::AgentScope::AllWorkspaces,
            ),
        );
        let listed_here = tilo_api::session::Here::workspaces(host.daemon().as_ref(), &own)
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        assert_eq!(listed_here, [host.workspace()]);

        let (status, listed) = key.post(LIST, &json!({})).await;
        assert_eq!(status, 200, "{listed}");
        assert_eq!(ids(&listed["result"]), listed_here, "{listed}");
        let (status, read) = key
            .get(&format!("/v1/workspaces/{}/resources", host.workspace()))
            .await;
        assert_eq!(
            status, 200,
            "the key reads the computer's workspace: {read}"
        );

        let (status, made) = key
            .post(CREATE, &json!({"name": "Made by the backend"}))
            .await;
        assert_eq!(status, 200, "the computer makes it: {made}");
        let made = made["result"]["workspace"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("a create answers the workspace it made: {made}"))
            .to_owned();
        let (_, listed) = key.post(LIST, &json!({})).await;
        let listed = ids(&listed["result"]);
        assert!(listed.contains(&made), "the list holds it: {listed:?}");
        assert!(listed.contains(&listed_here[0]), "{listed:?}");
        let (status, read) = key.get(&format!("/v1/workspaces/{made}/resources")).await;
        assert_eq!(status, 200, "the computer serves it at once: {read}");
    });
}

/// A key someone else's root certified reaches nothing on this computer: it lists no workspace
/// and reads none, and nothing of the person's is entered as it.
#[test]
fn a_key_of_a_root_this_computer_does_not_know_reaches_nothing() {
    host::run(|host| async move {
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[0x5a; 32]);
        let key = host.key_of(&stranger, "Someone's backend");
        let (status, listed) = key.post(LIST, &json!({})).await;
        assert!(
            status != 200 || ids(&listed["result"]).is_empty(),
            "a stranger's key lists nothing: {status} {listed}"
        );
        let (status, read) = key
            .get(&format!("/v1/workspaces/{}/resources", host.workspace()))
            .await;
        assert!(
            matches!(status, 403 | 404),
            "a stranger's key reads nothing of the workspace: {status} {read}"
        );
    });
}
