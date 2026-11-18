//! A member whose role the workspace's own state holds, asked through the action service of
//! the server's device: what a commenter's comment and a moderator's new channel are answered.

use super::super::*;
use super::{CHAT, Membership, TASKS, invoked, json, listed};
use tilo_actions::remote::Caller;
use ui::permissions::Role;

/// The provider that names `document`, a document of `app`.
fn provider(app: &str, document: &DocumentRoute) -> String {
    format!(
        "{}:{app}:{}",
        tilo_actions::remote::RESOURCE_PROVIDER,
        document.document
    )
}

/// A commenter comments and a moderator makes a new channel, each holding their role in the
/// workspace's own governance, given by the owner's server device, and not in this fixture's
/// word alone: both calls are answered done.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commenter_comments_and_a_moderator_makes_a_channel_by_the_role_the_workspace_holds() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let device = enrol(&root, "home-server", DeviceCapabilities::member());
    let page_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = start(&root, &device, state.path()).await;
    let mut page = Browser::open(&server, &page_device, &device).await;
    let tasks = provider(TASKS, &page.create(TASKS));
    let chat = provider(CHAT, &page.create(CHAT));
    page.peer.barrier().unwrap().recv().unwrap().unwrap();
    // The owner files an issue for a member to comment on.
    let issues = ui::resource::chain_resource_id(&ui::resource::DocumentId::derived(
        "issues",
        &ui::resource::DocumentId::workspace_default(),
    ));
    let filed = page
        .invoke(request(
            &tasks,
            ActionAddress::new(issues, "create"),
            json(serde_json::json!({"title": "Ship the launch"})),
        ))
        .await;
    assert!(
        matches!(
            filed.status,
            ReceiptStatus::Completed { .. } | ReceiptStatus::Accepted
        ),
        "{filed:?}"
    );

    let membership = Membership::of(
        &server,
        &device,
        &guest_device,
        state.path().join("member-content"),
    );
    let actions = tilo_agent_session::WorkspaceActions::new(Arc::new(crate::common::catalogue));
    actions.attach(membership.clone()).unwrap();
    // The guest is a member of the workspace, a viewer until they are made more.
    super::super::resource_support::admit_person(
        &server,
        &device,
        WORKSPACE,
        &guest_device,
        tilo_peer_state::join::Role::Viewer,
    )
    .await;
    let workspace = page.workspace;
    let member = Caller::authenticated(
        guest_device.cert.user_public_key,
        guest_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    let mut wrong = Vec::new();

    // A commenter, by the workspace's governance: their comment on the issue.
    membership.becomes(Role::Commenter).await;
    let comment = listed(&actions, workspace, &member, &tasks)
        .into_iter()
        .find(|action| {
            action.address.action == "comment"
                && !action.params.iter().any(|param| param.name == "issue")
        });
    match comment {
        Some(comment) => {
            let answered = invoked(
                &actions,
                workspace,
                &member,
                &request(
                    &tasks,
                    comment.address.clone(),
                    json(serde_json::json!({"text": "Looks good"})),
                ),
            );
            if !matches!(answered, ReceiptStatus::Completed { .. }) {
                wrong.push(format!("a commenter's comment is not saved: {answered:?}"));
            }
        }
        None => wrong.push("a commenter is not listed Comment on the issue".to_owned()),
    }

    // A moderator, by the workspace's governance: a new channel in Chat.
    membership.becomes(Role::Moderator).await;
    let listing = listed(&actions, workspace, &member, &chat);
    match listing
        .iter()
        .find(|action| action.address.action == "create-channel")
    {
        Some(new_channel) => {
            let answered = invoked(
                &actions,
                workspace,
                &member,
                &request(
                    &chat,
                    new_channel.address.clone(),
                    json(serde_json::json!({"name": "launch"})),
                ),
            );
            if !matches!(answered, ReceiptStatus::Completed { .. }) {
                wrong.push(format!(
                    "a moderator's new channel is not saved: {answered:?}"
                ));
            }
        }
        None => {
            let names: Vec<String> = listing
                .iter()
                .map(|action| action.address.action.to_string())
                .collect();
            wrong.push(format!(
                "a moderator is not listed a new channel; listed: {names:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
