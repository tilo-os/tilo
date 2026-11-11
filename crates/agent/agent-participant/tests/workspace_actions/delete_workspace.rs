//! A backend running one workspace per customer deletes a customer's
//! workspace when they leave, through the front door on the relay: the
//! workspace leaves every listing, what the relay kept of it goes from its
//! disk and from its person's storage, the key minted for it is revoked and
//! says so, and a member's page open in it is told it was deleted. Only its
//! owner's key reaching all their workspaces deletes it.
use super::door_on_relay::{OnRelay, accept_invitation, approved_key, door_on_relay};
use super::front_door::{ISSUER, Mcp, upload};
use super::hosted_documents::application_pdf;
use super::*;
use serde_json::{Value, json};
use std::path::PathBuf;

/// Every path under `root` that is `workspace`'s, as the relay names what it
/// keeps of one: its id, first in a file's name, or its relay workspace.
fn traces(root: &Path, workspace: &str, workspace_id: u64) -> Vec<PathBuf> {
    let names = [workspace_id.to_string(), format!("{workspace_id:016x}")];
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(workspace) || names.contains(&name) {
                found.push(path.clone());
            }
            if path.is_dir() {
                pending.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The rows of `account/usage`, as `account` reads it.
async fn usage(account: &mut Mcp<'_>) -> Vec<Value> {
    let (status, read) = account
        .post(json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                     "params": {"name": "tilo_read", "arguments": {"path": "account/usage"},
                                "_meta": {"so.tilo.mcp": {"structured": true}}}}))
        .await;
    assert_eq!(status, 200, "{read}");
    read["result"]["structuredContent"]["fields"]["standing"]["usage"]["workspaces"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The ids `account/workspaces#list` names, as `account` lists them.
async fn listed(account: &mut Mcp<'_>) -> Vec<String> {
    account
        .did("account/workspaces#list", json!({}))
        .await
        .get("workspaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect()
}

/// A person's server key on `on_relay`, approved on a page of theirs.
async fn server_key(on_relay: &OnRelay, name: &str) -> (SigningKey, String) {
    let (root, _) = tilo_user_identity::UserIdentity::generate(name, 1);
    let page = enrol(
        &root,
        &format!("{name}'s browser"),
        DeviceCapabilities::member(),
    );
    let key = approved_key(on_relay, &root, &page, "Visa backend").await;
    (root, key)
}

/// `account` invites a person into `workspace`, who enters on a page of
/// theirs as `root`: their device.
async fn invite(
    account: &mut Mcp<'_>,
    on_relay: &OnRelay,
    workspace: &str,
    root: &SigningKey,
) -> Device {
    let invited = account
        .tilo_do(
            json!({"path": "workspace/members#invite", "workspace": workspace,
                        "args": {"role": "editor"}}),
        )
        .await;
    let link = invited["result"]["structuredContent"]["result"]["link"]
        .as_str()
        .unwrap_or_else(|| panic!("an invitation's link: {invited}"))
        .to_owned();
    accept_invitation(on_relay, &link, root, "Rosa Ortiz")
        .await
        .0
}

/// Wait until `page` heard that `workspace_id` was deleted, and stopped
/// holding it: it says `workspace-deleted`, never that it was not admitted.
async fn heard_deleted(page: &tilo_session_host::session::PeerHandle, workspace_id: u64) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let status = page.status();
        assert!(
            !status
                .failures
                .iter()
                .any(|failure| failure.code == "not-admitted"),
            "a deleted workspace is not one that turned the page away: {status:?}"
        );
        if status
            .failures
            .iter()
            .any(|failure| failure.code == "workspace-deleted")
        {
            assert!(!status.workspaces.contains_key(&workspace_id), "{status:?}");
            assert_eq!(
                status.phase,
                tilo_session_host::session::TransportPhase::Failed,
                "a page holding only it is done: {status:?}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the page never heard the workspace was deleted: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `page`, a member of only `workspace` (`workspace_id`), comes back after it
/// was deleted: its host no longer lets the device in, and says nothing to it
/// at the handshake, but its address says the workspace was deleted, so the
/// device forgets it.
async fn only_member_hears_deleted(
    on_relay: &OnRelay,
    page: &Device,
    workspace: &str,
    workspace_id: u64,
) {
    let back = start_peer(
        on_relay.relay_host(),
        page.cert.clone(),
        on_relay.host.cert.clone(),
        page.key.clone(),
    );
    back.join(workspace_id).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while !back
        .status()
        .failures
        .iter()
        .any(|failure| failure.code == "not-admitted")
    {
        assert!(
            Instant::now() < deadline,
            "the host never turned the device away: {:?}",
            back.status()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    back.close();
    let client = tilo_net::Client::builder().build().unwrap();
    let host = tilo_relay_client::https::Origin::new(&client, &on_relay.relay);
    assert!(
        tilo_session_host::workspace_deleted(&host, workspace)
            .await
            .unwrap(),
        "the host's address says the workspace was deleted"
    );
    // The device settles what it remembered by that answer: forgotten.
    let host_device = hex::encode(on_relay.host.cert.device_public_key);
    let place = tilo_session_host::entered::Place {
        origin: on_relay.relay.clone(),
        room: workspace.to_owned(),
    };
    let remembered = tilo_session_host::entered::remember(
        Vec::new(),
        tilo_session_host::entered::Entered {
            origin: place.origin.clone(),
            room: place.room.clone(),
            host_device: host_device.clone(),
            name: Some("Applicant: Jane Doe".into()),
            unavailable: false,
        },
    );
    let (changed, settled) = tilo_session_host::entered::settle_failed(
        remembered.entered,
        &place,
        Some(&host_device),
        true,
    );
    assert_eq!(settled, tilo_session_host::entered::Settled::Deleted);
    assert!(changed.entered.is_empty(), "{changed:?}");
    assert_eq!(changed.unchoose, [place]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one customer's workspace, from its making to everything it kept being gone"
)]
async fn a_server_key_deletes_a_customers_workspace_and_everything_it_kept_goes() {
    let on_relay = door_on_relay(ISSUER).await;
    let door = Arc::clone(&on_relay.door);
    let (_, key) = server_key(&on_relay, "Ada").await;
    let mut account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    assert_eq!(account.initialize().await.0, 200);
    let made = account
        .did(
            "account/workspaces#create",
            json!({"name": "Applicant: Jane Doe"}),
        )
        .await;
    let workspace = made["workspace"]["id"].as_str().unwrap().to_owned();
    let workspace_id = tilo_relay_protocol::room_workspace_id(&workspace).unwrap();
    let kept = account
        .did(
            "account/workspaces#create",
            json!({"name": "Applicant: John Roe"}),
        )
        .await["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // An agent works in it with a key minted for it: a form goes in.
    let minted = account
        .did("account/keys#mint", json!({"workspace": workspace}))
        .await;
    let minted_key = minted["token"].as_str().unwrap().to_owned();
    let mut agent = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted_key.clone(),
        session: None,
    };
    assert_eq!(agent.initialize().await.0, 200);
    let (status, put) = upload(&agent, &workspace, &application_pdf()).await;
    assert_eq!(status, 201, "{put}");
    agent
        .did(
            "pdf-viewer#import-pdf-content",
            json!({"content": put["content"], "name": "application.pdf"}),
        )
        .await;

    // A reviewer has it open on a page of theirs.
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa Ortiz", 1);
    let rosa_page = invite(&mut account, &on_relay, &workspace, &rosa).await;
    let page = start_peer(
        on_relay.relay_host(),
        rosa_page.cert.clone(),
        on_relay.host.cert.clone(),
        rosa_page.key.clone(),
    );
    page.join(workspace_id).unwrap();
    ready(&[&page]).await;
    // Another reviewer, who works in the other customer's workspace too,
    // had it open earlier and is away while it is deleted.
    let (sam, _) = tilo_user_identity::UserIdentity::generate("Sam Lee", 1);
    let sam_page = invite(&mut account, &on_relay, &workspace, &sam).await;
    let _ = invite(&mut account, &on_relay, &kept, &sam).await;
    let away = start_peer(
        on_relay.relay_host(),
        sam_page.cert.clone(),
        on_relay.host.cert.clone(),
        sam_page.key.clone(),
    );
    away.join(workspace_id).unwrap();
    ready(&[&away]).await;
    away.close();
    // A customer, invited to only this workspace, had it open earlier too.
    let (lin, _) = tilo_user_identity::UserIdentity::generate("Lin Park", 1);
    let lin_page = invite(&mut account, &on_relay, &workspace, &lin).await;
    let lin_away = start_peer(
        on_relay.relay_host(),
        lin_page.cert.clone(),
        on_relay.host.cert.clone(),
        lin_page.key.clone(),
    );
    lin_away.join(workspace_id).unwrap();
    ready(&[&lin_away]).await;
    lin_away.close();

    let state = on_relay.state.path();
    let before = traces(state, &workspace, workspace_id);
    assert!(
        before
            .iter()
            .any(|path| path.extension().is_some_and(|ext| ext == "room")),
        "the relay keeps the workspace's record: {before:?}"
    );
    assert!(
        usage(&mut account)
            .await
            .iter()
            .any(|row| row["workspace"] == workspace.as_str()),
        "what it keeps counts against its person"
    );

    // The customer leaves: the backend deletes their workspace.
    let deleted = account
        .did("account/workspaces#delete", json!({"workspace": workspace}))
        .await;
    assert_eq!(deleted["deleted"], workspace.as_str(), "{deleted}");
    assert_eq!(
        deleted["revoked_keys"],
        json!([minted["grant"]]),
        "the key minted for it is revoked with it: {deleted}"
    );

    // It is in no listing; the other customer's workspace stays.
    let ids = listed(&mut account).await;
    assert!(!ids.contains(&workspace), "{ids:?}");
    assert!(ids.contains(&kept), "{ids:?}");
    // Its key is told it was revoked, not that it never was a key.
    let mut minted_again = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted_key,
        session: None,
    };
    let (status, refused) = minted_again.initialize().await;
    assert_eq!(status, 401, "{refused}");
    assert_eq!(refused["error"]["code"], "key_revoked", "{refused}");
    assert!(
        refused["error"]["revoked_at_ms"].is_u64(),
        "it says when: {refused}"
    );
    // What the relay kept of it is gone from its disk, but the mark that it
    // was deleted (its content goes as the relay hears it was deleted), and
    // from what its person keeps.
    let marker = format!("{workspace}.deleted");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let after = traces(state, &workspace, workspace_id);
        if after.len() == 1 && after[0].ends_with(&marker) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "nothing of it stays: {after:?}, kept before {before:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !usage(&mut account)
            .await
            .iter()
            .any(|row| row["workspace"] == workspace.as_str()),
        "its storage is released"
    );
    // The reviewer's page heard it was deleted, and stopped holding it.
    heard_deleted(&page, workspace_id).await;
    // The reviewer who was away comes back, and hears it was deleted too.
    let back = start_peer(
        on_relay.relay_host(),
        sam_page.cert.clone(),
        on_relay.host.cert.clone(),
        sam_page.key.clone(),
    );
    back.join(workspace_id).unwrap();
    heard_deleted(&back, workspace_id).await;
    only_member_hears_deleted(&on_relay, &lin_page, &workspace, workspace_id).await;
    // Deleting it again finds no such workspace.
    let again = account
        .tilo_do(json!({"path": "account/workspaces#delete", "args": {"workspace": workspace}}))
        .await;
    assert_eq!(
        again["result"]["structuredContent"]["error"]["code"], "not_found",
        "{again}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn neither_a_minted_key_nor_a_member_deletes_a_workspace() {
    let on_relay = door_on_relay(ISSUER).await;
    let door = Arc::clone(&on_relay.door);
    let (_, key) = server_key(&on_relay, "Ada").await;
    let mut account = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    assert_eq!(account.initialize().await.0, 200);
    let workspace = account
        .did(
            "account/workspaces#create",
            json!({"name": "Applicant: Jane Doe"}),
        )
        .await["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // A key minted for it reaches no account, so deletes nothing.
    let minted = account
        .did("account/keys#mint", json!({"workspace": workspace}))
        .await;
    let mut agent = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: minted["token"].as_str().unwrap().to_owned(),
        session: None,
    };
    assert_eq!(agent.initialize().await.0, 200);
    let refused = agent
        .tilo_do(json!({"path": "account/workspaces#delete", "args": {"workspace": workspace}}))
        .await;
    assert_eq!(
        refused["result"]["structuredContent"]["error"]["code"], "forbidden",
        "{refused}"
    );

    // A member's own server key, who was invited in, finds no workspace of
    // theirs to delete.
    let (rosa, rosa_key) = server_key(&on_relay, "Rosa").await;
    let _ = invite(&mut account, &on_relay, &workspace, &rosa).await;
    let mut member = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: rosa_key,
        session: None,
    };
    assert_eq!(member.initialize().await.0, 200);
    let refused = member
        .tilo_do(json!({"path": "account/workspaces#delete", "args": {"workspace": workspace}}))
        .await;
    assert_eq!(
        refused["result"]["structuredContent"]["error"]["code"], "not_found",
        "{refused}"
    );

    // It stays, and the minted key still works there.
    assert!(listed(&mut account).await.contains(&workspace));
    let mut in_workspace = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted["token"].as_str().unwrap().to_owned(),
        session: None,
    };
    assert_eq!(in_workspace.initialize().await.0, 200);
}
