//! A workspace's people, decided by its governance on the device that keeps it: who may make an
//! administrator, who may unmake one, and who owns the workspace. The changes only its owner
//! makes are refused everyone else, saying why: a key the person's host signs for, and a device
//! of an administrator who is not the owner.

use serde_json::{Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{ActionCallError, RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER};
use tilo_user_identity::UserIdentity;

use crate::language::host;

/// The address the workspace's governance publishes `operation` at, when it is not one person's.
fn at(workspace: &str, operation: &str) -> ActionAddress {
    RemoteActionTarget::workspace_governance(workspace, operation).address
}

/// The address of `action` on `person` among the workspace's people.
fn on(workspace: &str, person: &str, action: &str) -> ActionAddress {
    ActionAddress::new(
        tilo_actions::remote::workspace_member(workspace, person),
        action,
    )
}

/// The rows of an answer that is the workspace's people.
fn people(answered: &Result<InvocationUpdate, ActionCallError>) -> Vec<Value> {
    match answered {
        Ok(InvocationUpdate::Succeeded { output, .. }) => output
            .value
            .as_ref()
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        other => panic!("the workspace answers its people: {other:?}"),
    }
}

/// The row of `person` among them, while the workspace keeps them.
fn row_of<'a>(rows: &'a [Value], person: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|row| row["actor"]["member"]["person"] == person && row["removed"] != true)
}

/// The people the workspace keeps now, as `device`, one of its owner's, reads them.
async fn people_now(device: &host::Member, workspace: &str) -> Vec<Value> {
    let listed = device
        .call(
            WORKSPACE_GOVERNANCE_PROVIDER,
            at(workspace, "member.list"),
            &json!({}),
        )
        .await;
    people(&listed)
}

/// The person's computer makes Grace and Hal administrators of the workspace and unmakes them,
/// hands the workspace on, and each change the workspace's people list follows.
#[test]
fn an_owner_makes_and_unmakes_administrators_and_hands_the_workspace_on() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let computer = host.member("Ada's computer", &workspace).await;
        let (grace, _) = UserIdentity::generate("Grace", 1);
        let grace_hex = hex::encode(grace.verifying_key().to_bytes());
        let (hal, _) = UserIdentity::generate("Hal", 1);
        let hal_hex = hex::encode(hal.verifying_key().to_bytes());

        // Ada's computer makes Grace and Hal administrators, by their keys.
        for (person, name) in [(&grace_hex, "Grace Hopper"), (&hal_hex, "Hal")] {
            let made = computer
                .call(
                    WORKSPACE_GOVERNANCE_PROVIDER,
                    at(&workspace, "member.add"),
                    &json!({"person": person, "role": "administrator", "name": name}),
                )
                .await;
            let rows = people(&made);
            let row = row_of(&rows, person).unwrap_or_else(|| panic!("{name} is listed: {rows:?}"));
            assert_eq!(row["administrator"], true, "{rows:?}");
            assert_eq!(row["name"], name, "{rows:?}");
        }

        // Ada's computer removes Hal and moves Grace to a role.
        let removed = computer
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                on(&workspace, &hal_hex, "remove"),
                &json!({"person": hal_hex.clone()}),
            )
            .await;
        let rows = people(&removed);
        assert!(row_of(&rows, &hal_hex).is_none(), "Hal is gone: {rows:?}");
        let demoted = computer
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                on(&workspace, &grace_hex, "role"),
                &json!({"person": grace_hex.clone(), "role": "editor"}),
            )
            .await;
        let rows = people(&demoted);
        let grace_row = row_of(&rows, &grace_hex).expect("Grace stays");
        assert_eq!(grace_row["administrator"], false, "{rows:?}");
        assert_eq!(grace_row["role"], "editor", "{rows:?}");

        // And hands the workspace to Grace: she owns it, and Ada stays as an administrator.
        let handed = computer
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                on(&workspace, &grace_hex, "role"),
                &json!({"person": grace_hex.clone(), "role": "owner"}),
            )
            .await;
        let rows = people(&handed);
        assert_eq!(
            row_of(&rows, &grace_hex).map(|row| row["owner"].clone()),
            Some(json!(true)),
            "Grace owns it: {rows:?}"
        );
        let ada = host.person().to_owned();
        let ada_row = row_of(&rows, &ada).expect("Ada stays");
        assert_eq!(ada_row["owner"], false, "{rows:?}");
        assert_eq!(ada_row["administrator"], true, "{rows:?}");
    });
}

/// A key the person approved does not make an owner-only change through the host it acts for:
/// only the owner's own device does, and the workspace is unchanged.
#[test]
fn a_key_the_person_approved_makes_no_owner_only_change() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let computer = host.member("Ada's computer", &workspace).await;
        let (hal, _) = UserIdentity::generate("Hal", 1);
        let hal_hex = hex::encode(hal.verifying_key().to_bytes());
        let made = computer
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                at(&workspace, "member.add"),
                &json!({"person": hal_hex.clone(), "role": "administrator"}),
            )
            .await;
        assert_eq!(
            row_of(&people(&made), &hal_hex).map(|row| row["administrator"].clone()),
            Some(json!(true)),
            "Hal administers the workspace"
        );

        // The API names the workspace's governance as its listing does: `workspace/members/…`.
        let removing = format!(
            "/v1/workspaces/{workspace}/resources/workspace/members/{hal_hex}/actions/remove"
        );
        let (status, by_key) = host.key("Ada's backend").post(&removing, &json!({})).await;
        assert_ne!(
            status, 200,
            "the key does not remove an administrator: {by_key}"
        );
        // The workspace offers a key none of the governance it may not change, so the answer is
        // its own refusal of the change, not one the call made and lost.
        assert_eq!(by_key["error"]["code"], "not_permitted", "{by_key}");
        assert!(
            by_key["error"]["message"]
                .as_str()
                .is_some_and(|said| said.contains("only the workspace's owner may")),
            "{by_key}"
        );
        let rows = people_now(&computer, &workspace).await;
        assert!(
            row_of(&rows, &hal_hex).is_some_and(|hal| hal["administrator"] == true),
            "none of the refused changes took: {rows:?}"
        );
    });
}

/// A page in the workspace signs a change the workspace takes. An administrator who is not its
/// owner is refused the owner's changes with why, and the workspace is unchanged.
#[test]
fn an_administrators_page_may_not_unmake_an_administrator_nor_take_the_workspace() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let page = host.member("Ada's page", &workspace).await;
        let (grace, _) = UserIdentity::generate("Grace", 1);
        let grace_hex = hex::encode(grace.verifying_key().to_bytes());
        let (hal, _) = UserIdentity::generate("Hal", 1);
        let hal_hex = hex::encode(hal.verifying_key().to_bytes());

        // Ada's page makes Grace and Hal administrators, and the workspace lists them.
        for person in [&grace_hex, &hal_hex] {
            let made = page
                .call(
                    WORKSPACE_GOVERNANCE_PROVIDER,
                    at(&workspace, "member.add"),
                    &json!({"person": person, "role": "administrator"}),
                )
                .await
                .expect("the owner's page makes an administrator");
            assert!(
                matches!(made, InvocationUpdate::Succeeded { .. }),
                "{made:?}"
            );
        }
        let rows = people_now(&page, &workspace).await;
        for person in [&grace_hex, &hal_hex] {
            assert_eq!(
                row_of(&rows, person).map(|row| row["administrator"].clone()),
                Some(json!(true)),
                "{rows:?}"
            );
        }

        // Ada's page hands the workspace to Grace: Ada administers it now, and owns nothing.
        let handed = page
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                on(&workspace, &grace_hex, "role"),
                &json!({"person": grace_hex.clone(), "role": "owner"}),
            )
            .await;
        assert_eq!(
            row_of(&people(&handed), &grace_hex).map(|row| row["owner"].clone()),
            Some(json!(true)),
            "{handed:?}"
        );

        // Now Ada's page, an administrator and not the owner, may not remove another
        // administrator, nor take the workspace back.
        let refused = page
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                on(&workspace, &hal_hex, "remove"),
                &json!({"person": hal_hex.clone()}),
            )
            .await
            .expect_err("an administrator does not remove another");
        assert!(
            format!("{refused:?}").contains("only the workspace's owner may"),
            "the workspace says it refused: {refused:?}"
        );
        let taking = page
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                on(&workspace, &grace_hex, "role"),
                &json!({"person": grace_hex.clone(), "role": "owner"}),
            )
            .await
            .expect_err("an administrator does not take the workspace");
        assert!(
            format!("{taking:?}")
                .contains("only the workspace's owner hands the workspace to someone else"),
            "{taking:?}"
        );
        let rows = people_now(&page, &workspace).await;
        assert!(row_of(&rows, &hal_hex).is_some(), "Hal stays: {rows:?}");
        assert_eq!(
            row_of(&rows, &grace_hex).map(|row| row["owner"].clone()),
            Some(json!(true)),
            "Grace still owns it: {rows:?}"
        );
    });
}

/// An agent's grant is the agent and no more: a key the person's root certified for an agent
/// asks its person before letting someone in, which the person may do, and until they say yes
/// the workspace invites no one.
#[test]
fn an_agents_invitation_waits_for_its_persons_yes() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent_key("Reviewer's agent");
        let inviting =
            format!("/v1/workspaces/{workspace}/resources/workspace/invitations/actions/invite");
        let (_, asked) = agent
            .request(
                "POST",
                &inviting,
                &[
                    ("content-type", "application/json"),
                    ("tilo-invocation", "invite"),
                ],
                json!({"role": "editor", "invitee_name": "Gooseberry jam"})
                    .to_string()
                    .as_bytes(),
            )
            .await;
        assert_eq!(
            asked["status"], "pending",
            "the agent's invitation asks: {asked}"
        );
        assert!(
            asked.get("ask").is_some(),
            "it waits for its person: {asked}"
        );

        // The person's own computer lists no invitation the agent asked for.
        let computer = host.member("Ada's computer", &workspace).await;
        let open = computer
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                at(&workspace, "invitation.list"),
                &json!({}),
            )
            .await;
        let Ok(InvocationUpdate::Succeeded { output, .. }) = &open else {
            panic!("the computer lists the invitations: {open:?}");
        };
        assert!(
            !output
                .value
                .as_ref()
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|invited| invited["invitee_name"] == "Gooseberry jam"),
            "the workspace invites no one before the person's yes: {output:?}"
        );

        // The same invitation, by a device of the person's, is the workspace's to take at once.
        let invited = computer
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                at(&workspace, "invite"),
                &json!({"role": "editor", "invitee_name": "Gooseberry jam"}),
            )
            .await;
        assert!(
            matches!(invited, Ok(InvocationUpdate::Succeeded { .. })),
            "the person's own computer invites: {invited:?}"
        );
    });
}

/// A key is refused the workspace itself beyond the administration: it renames nothing, which a
/// device of its person's does, and its person's rename is the one the workspace takes.
#[test]
fn a_key_the_person_approved_renames_no_workspace() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Ada's backend");
        let renaming = format!("/v1/workspaces/{workspace}/resources/workspace/actions/rename");
        let (status, refused) = key
            .post(&renaming, &json!({"name": "Named by a key"}))
            .await;
        assert_ne!(status, 200, "the key renames no workspace: {refused}");
        assert_eq!(
            refused["error"]["code"], "unknown_action",
            "the workspace offers a key no rename: {refused}"
        );

        // The person's own computer renames it, so the refusal is the key's and not the
        // workspace's.
        let computer = host.member("Ada's computer", &workspace).await;
        let renamed = computer
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                at(&workspace, "rename"),
                &json!({"name": "Named by its owner"}),
            )
            .await;
        match renamed {
            Ok(InvocationUpdate::Succeeded { output, .. }) => assert_eq!(
                output
                    .value
                    .as_ref()
                    .and_then(|answer| answer["name"].as_str()),
                Some("Named by its owner"),
                "the person's own computer renames it"
            ),
            other => panic!("the person's own computer renames it: {other:?}"),
        }
    });
}
