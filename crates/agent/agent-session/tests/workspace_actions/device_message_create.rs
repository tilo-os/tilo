//! A computer's agent posting through its held device session to a
//! workspace's server, as `tilo migrate` does: the post answers the message it
//! made, with the address it is published under, and the message reads back
//! there — whether the messages part was already held or the write founded it.

use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::Caller;
use tilo_workspace_service::agents::{WorkspaceActionProvider, invoke_named};
use ui::tilo_resource::{Id, Resource as _};

const CHAT: &str = "so.tilo.chat";
/// The chat's channel the posts go in.
const CHANNEL: &str = "c_chat";

/// The message a post answered, by its id and address.
fn made(answer: &Value) -> (String, String) {
    let made = answer["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("a post answers what it made: {answer}"));
    assert_eq!(made.len(), 1, "one message made: {answer}");
    let address = made[0].as_str().unwrap().to_owned();
    let id = address.rsplit('/').next().unwrap().to_owned();
    (id, address)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_post_through_the_device_session_answers_the_message_it_made() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (served, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let _page = Browser::open_at(&served, &browser, &server, &workspace).await;
    let catalogue = crate::common::catalogue();
    let record = tilo_workspace_service::agents::workspace_resource(&catalogue, CHAT);
    workspaces
        .prepare_application(&workspace, &server.cert, &catalogue, CHAT, &record)
        .await
        .unwrap();
    let service =
        WorkspaceActionProvider::new(actions.clone(), WorkspaceId::parse(&workspace).unwrap());
    let person = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(&workspace);
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.clone(),
        document: record.clone(),
    };
    let messages = ui::resource::chain_resource_id(&chat::parts::chain());
    invoke_named(
        &service,
        &person,
        &route,
        "chat.workspace",
        "create-channel-with-id",
        json!({"id": CHANNEL, "name": "chat", "category": null,
            "kind": null, "topic": null, "members": null})
        .as_object()
        .unwrap()
        .clone(),
    )
    .await
    .expect("the chat takes a channel");
    let post = |text: &str| {
        json!({"channel": CHANNEL, "text": text})
            .as_object()
            .unwrap()
            .clone()
    };

    // The first message: the messages part is not held yet, and the post founds it.
    assert!(
        workspaces
            .resource_document(&workspace, CHAT, chat::parts::part(0).as_str())
            .await
            .unwrap()
            .is_none(),
        "the workspace holds no messages yet"
    );
    let answer = invoke_named(
        &service,
        &person,
        &route,
        &messages,
        "post",
        post("Plan the launch"),
    )
    .await
    .expect("the post is made");
    let (id, address) = made(&answer);
    assert!(
        address.starts_with(
            &format!(
                "{}/messages/",
                <chat::parts::ChatMessages as ui::tilo_resource::Resource>::KIND
            )
        ),
        "the answer names the message's address: {address}"
    );

    // A second, with the part held now: it answers its own id.
    let answer = invoke_named(
        &service,
        &person,
        &route,
        &messages,
        "post",
        post("Ship it"),
    )
    .await
    .expect("the post is made");
    let (second, _) = made(&answer);
    assert_ne!(second, id, "each post answers the message it made");

    // Both read back where their answers said they are.
    let part = message_part(&workspaces, &workspace).await;
    let bytes = workspaces
        .resource_document(&workspace, CHAT, part.as_str())
        .await
        .unwrap()
        .unwrap();
    let held = chat::messages::read_part(&automerge::AutoCommit::load(&bytes).unwrap());
    for made in [&id, &second] {
        assert!(
            held.messages.contains(&Id::from_raw(made.as_str())),
            "the message answered as {made} reads back"
        );
    }
}

/// The messages part the chat holds now, as the workspace keeps it.
async fn message_part(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> ui::resource::DocumentId {
    for index in 0..64 {
        let part = chat::parts::part(index);
        if workspaces
            .resource_document(workspace, CHAT, part.as_str())
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return part;
        }
    }
    panic!("the post founded no part");
}
