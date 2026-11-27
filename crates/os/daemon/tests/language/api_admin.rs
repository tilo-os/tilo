#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "one scenario in order, on a fixture that is not sent between tasks"
)]
//! What a device of the person's does in the workspace their daemon keeps: it invites people and
//! reads who is in, adds and moves members, and installs a version of an app. A key the workspace
//! does not know is refused all of it.

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, WORKSPACE_GOVERNANCE_PROVIDER, workspace_governance_address, workspace_member,
};
use tilo_actions::{ActionFailure, InvocationUpdate};

use crate::language::host;

/// The action `operation` of `workspace`'s own tree, run by `device`: whether it ran, and its
/// result or its refusal.
async fn workspace_action(
    device: &host::Member,
    workspace: &str,
    operation: &str,
    args: &Value,
) -> (bool, Value) {
    let at = workspace_governance_address(workspace, operation);
    settled(device, at, args).await
}

/// [`workspace_action`] of the person `person`'s row in the workspace's people, with `action`.
async fn person_action(
    device: &host::Member,
    workspace: &str,
    person: &str,
    action: &str,
    args: &Value,
) -> (bool, Value) {
    let at = tilo_actions::ActionAddress::new(workspace_member(workspace, person), action);
    settled(device, at, args).await
}

/// The action at `at` run by `device`: whether it ran, and its result or the refusal.
async fn settled(
    device: &host::Member,
    at: tilo_actions::ActionAddress,
    args: &Value,
) -> (bool, Value) {
    match device.call(WORKSPACE_GOVERNANCE_PROVIDER, at, args).await {
        Ok(InvocationUpdate::Succeeded { output, .. }) => {
            (true, output.value.unwrap_or(Value::Null))
        }
        Ok(InvocationUpdate::Failed {
            error: ActionFailure { code, message, .. },
            ..
        }) => (false, json!({"code": code, "message": message})),
        Ok(other) => panic!("the action settled: {other:?}"),
        Err(ActionCallError::InvalidRequest { reason }) => {
            (false, json!({"code": "invalid_request", "message": reason}))
        }
        Err(ActionCallError::Refused { reason }) => {
            (false, json!({"code": "refused", "message": reason}))
        }
        Err(error) => panic!("the device link answers: {error}"),
    }
}

/// The row of the person `person` among `rows`.
fn row<'a>(rows: &'a [Value], person: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["actor"]["member"]["person"] == person)
        .unwrap_or_else(|| panic!("{person} has a row: {rows:?}"))
}

/// A device of the person's invites a reviewer with a link: the workspace names whom the
/// invitation is for and what part it hands out, and a role above the inviter's is refused.
#[test]
fn a_devices_invitation_links_a_reviewer_with_the_part_it_hands_out() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;

        // No role above the inviter's is handed out.
        let (ran, nowhere) =
            workspace_action(&laptop, &workspace, "invite", &json!({"role": "superuser"})).await;
        assert!(!ran, "{nowhere}");

        // The invitation, and its link carries the signed invitation itself, which opens
        // wherever it is followed.
        let (ran, invited) = workspace_action(
            &laptop,
            &workspace,
            "invite",
            &json!({"role": "editor", "invitee_name": "rosa@example.test"}),
        )
        .await;
        assert!(ran, "{invited}");
        assert_eq!(invited["role"], "editor", "{invited}");
        assert_eq!(invited["workspace"], workspace.as_str(), "{invited}");
        let link = invited["link"].as_str().expect("a link");
        assert!(
            link.contains('#') && link.contains("invite="),
            "a link that carries its invitation: {link}"
        );

        // The workspace keeps it open, named for the person it is for.
        let (ran, open) =
            workspace_action(&laptop, &workspace, "invitation.list", &json!({})).await;
        assert!(ran, "{open}");
        assert!(
            open.as_array().is_some_and(|open| open
                .iter()
                .any(|one| one["invitee_name"] == "rosa@example.test" && one["role"] == "editor")),
            "{open}"
        );
    });
}

/// The owner's device lets a person in by their key, as an administrator; a key the workspace
/// does not know adds nobody.
#[test]
fn a_devices_key_adds_an_administrator_and_a_stranger_adds_nobody() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let (grace, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
        let grace_hex = hex::encode(grace.verifying_key().to_bytes());

        // A name alone is no person's key.
        let (ran, bad) = workspace_action(
            &laptop,
            &workspace,
            "member.add",
            &json!({"person": "Grace", "role": "editor"}),
        )
        .await;
        assert!(!ran, "{bad}");

        // An administrator, by their key, is let in.
        let (ran, added) = workspace_action(
            &laptop,
            &workspace,
            "member.add",
            &json!({"person": grace_hex, "role": "administrator", "name": "Grace Hopper"}),
        )
        .await;
        assert!(ran, "{added}");
        let rows = added.as_array().cloned().unwrap_or_default();
        let grace_row = row(&rows, &grace_hex);
        assert_eq!(grace_row["owner"], false, "{added}");
        assert_eq!(grace_row["administrator"], true, "{added}");
        assert_eq!(grace_row["name"], "Grace Hopper", "{added}");

        // The wait list reads, empty: this workspace's entry keeps none.
        let (ran, waiting) =
            workspace_action(&laptop, &workspace, "wait-list.list", &json!({})).await;
        assert!(ran, "{waiting}");
        assert_eq!(waiting["entries"], json!([]), "{waiting}");

        // A key the workspace holds no certificate for adds nobody.
        let visitor = host.visitor(&workspace).await;
        let refused = visitor
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                workspace_governance_address(&workspace, "member.add"),
                &json!({"person": grace_hex, "role": "editor"}),
            )
            .await;
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "a key the workspace does not know adds nobody: {refused:?}"
        );
    });
}

/// The owner's device moves a member to another role; a key the workspace does not know removes
/// nobody.
#[test]
fn a_devices_key_moves_a_member_and_a_stranger_removes_nobody() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's laptop", &workspace).await;
        let (grace, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
        let (hal, _) = tilo_user_identity::UserIdentity::generate("Hal", 1);
        let (grace_hex, hal_hex) = (
            hex::encode(grace.verifying_key().to_bytes()),
            hex::encode(hal.verifying_key().to_bytes()),
        );
        for person in [&grace_hex, &hal_hex] {
            let (ran, added) = workspace_action(
                &laptop,
                &workspace,
                "member.add",
                &json!({"person": person, "role": "editor"}),
            )
            .await;
            assert!(ran, "{added}");
        }

        let (ran, moved) = person_action(
            &laptop,
            &workspace,
            &hal_hex,
            "role",
            &json!({"role": "viewer"}),
        )
        .await;
        assert!(ran, "{moved}");
        let rows = moved.as_array().cloned().unwrap_or_default();
        let hal_row = row(&rows, &hal_hex);
        assert_eq!(hal_row["role"], "viewer", "{moved}");

        // A key the workspace holds no certificate for removes nobody.
        let visitor = host.visitor(&workspace).await;
        let refused = visitor
            .call(
                WORKSPACE_GOVERNANCE_PROVIDER,
                workspace_governance_address(&workspace, "remove-member"),
                &json!({"person": hal_hex}),
            )
            .await;
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "a key the workspace does not know removes nobody: {refused:?}"
        );
    });
}

/// The bytes of a package file one `upload-app-file` piece carries.
const PIECE: usize = 256 * 1024;

/// A device hands the workspace an app version in pieces, makes it the one the workspace uses,
/// and takes the version back: the same path `tilolang push` takes.
#[test]
fn a_device_installs_an_app_version_and_takes_it_back() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let laptop = host.member("Ada's builds", &workspace).await;
        let package = b"a developer's own build of the notes app".repeat(20000);
        let file = blake3::hash(&package).to_hex().to_string();
        let total = package.len() as u64;
        let mut offset = 0_u64;
        for piece in package.chunks(PIECE) {
            let (ran, answer) = workspace_action(
                &laptop,
                &workspace,
                "upload-app-file",
                &json!({
                    "file": file,
                    "offset": offset,
                    "total": total,
                    "data": hex::encode(piece),
                }),
            )
            .await;
            assert!(ran, "{answer}");
            offset += piece.len() as u64;
        }
        let (ran, installed) = workspace_action(
            &laptop,
            &workspace,
            "install-app",
            &json!({
                "app": "so.tilo.notes",
                "version": "2.0.0-dev.1",
                "surface": hex::encode([2_u8; 32]),
                "package": hex::encode([3_u8; 32]),
                "publisher": host.person(),
                "dev": true,
                "file": file,
            }),
        )
        .await;
        assert!(ran, "{installed}");
        assert_eq!(
            installed["running"]["release"]["version"], "2.0.0-dev.1",
            "{installed}"
        );

        // The version is taken back where the workspace publishes that app: its own resource.
        let app = tilo_actions::remote::workspace_app(&workspace, "so.tilo.notes");
        let at = tilo_actions::ActionAddress::new(app, "uninstall");
        let (ran, removed) = settled(&laptop, at, &json!({"app": "so.tilo.notes"})).await;
        assert!(ran, "{removed}");
    });
}

/// A key of the person's reads the workspace's mail accounts at `workspace/mail-accounts`.
#[test]
fn a_keys_read_of_the_workspaces_mail_accounts() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Mail reader");
        let (status, listed) = key
            .get(&format!(
                "/v1/workspaces/{workspace}/resources/workspace/mail-accounts"
            ))
            .await;
        assert_eq!(status, 200, "{listed}");
        assert!(
            listed.get("fields").is_some() || listed.get("children").is_some(),
            "the key reaches the workspace's mail: {listed}"
        );
    });
}
