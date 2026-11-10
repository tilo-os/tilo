//! A backend's key brings another person into a customer's workspace through
//! the door: `workspace/members#invite` answers the link a person's desk
//! hands over, the person follows it on a page of their own and is in as
//! the role, and a key they approve themselves reaches the workspace as them.
//! Neither a key minted for the workspace nor the invited person's key
//! invites anyone.
use super::door_on_relay::{accept_invitation, approved_key, door_on_relay};
use super::front_door::{ISSUER, Mcp};
use super::*;
use serde_json::{Value, json};

/// What `tilo_do` of `path` with `args` in `workspace` answered, typed.
async fn members(mcp: &mut Mcp<'_>, workspace: Option<&str>, path: &str, args: Value) -> Value {
    let mut arguments = json!({"path": path, "args": args});
    if let Some(workspace) = workspace {
        arguments["workspace"] = json!(workspace);
    }
    mcp.tilo_do(arguments).await["result"]["structuredContent"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backends_key_invites_a_reviewer_who_enters_with_a_key_of_their_own() {
    let on_relay = door_on_relay(ISSUER).await;
    let door = Arc::clone(&on_relay.door);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Visa backend").await;
    let mut backend = Mcp {
        door: &door,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    let (status, init) = backend.initialize().await;
    assert_eq!(status, 200, "{init}");
    let made = backend
        .did(
            "account/workspaces#create",
            json!({"name": "Applicant: Jane Doe"}),
        )
        .await;
    let workspace = made["workspace"]["id"].as_str().unwrap().to_owned();
    let here = Some(workspace.as_str());

    // It invites as an Editor at most; with an address, the door says it
    // mailed nothing and hands the link over.
    let above = members(
        &mut backend,
        here,
        "workspace/members#invite",
        json!({"role": "moderator"}),
    )
    .await;
    assert_eq!(above["status"], "failed", "{above}");
    let invited = members(
        &mut backend,
        here,
        "workspace/members#invite",
        json!({"role": "editor", "email": "rosa@example.com"}),
    )
    .await;
    assert_eq!(invited["status"], "ok", "{invited}");
    let invited = &invited["result"];
    assert_eq!(invited["sent"], false, "{invited}");
    assert_eq!(invited["email"], "rosa@example.com", "{invited}");
    let link = invited["link"].as_str().unwrap().to_owned();
    assert!(
        link.contains("/i/") && link.contains('#'),
        "the site's short link: {link}"
    );

    // Rosa follows it on a page of hers and is in as an Editor.
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa Ortiz", 1);
    let (rosa_page, joined) = accept_invitation(&on_relay, &link, &rosa, "Rosa Ortiz").await;
    assert_eq!(joined["workspace"], workspace.as_str(), "{joined}");
    assert_eq!(joined["role"], "editor", "{joined}");
    let listed = members(&mut backend, here, "workspace/members#list", json!({})).await;
    let rosa_hex = hex::encode(rosa.verifying_key().to_bytes());
    let rosa_row = listed["result"]["members"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["person"] == rosa_hex.as_str()))
        .cloned();
    assert_eq!(
        rosa_row.map(|row| row["role"].clone()),
        Some(json!("editor")),
        "{listed}"
    );

    // A key she approves herself reaches the workspace as her, and invites
    // nobody into a workspace she does not administer.
    let rosa_key = approved_key(&on_relay, &rosa, &rosa_page, "Rosa's review").await;
    let mut reviewer = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: rosa_key,
        session: None,
    };
    let (status, init) = reviewer.initialize().await;
    assert_eq!(status, 200, "her key reaches the workspace: {init}");
    let (status, listed) = reviewer
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_ls", "arguments": {}}}))
        .await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["result"]["isError"], false, "{listed}");
    let by_rosa = members(
        &mut reviewer,
        None,
        "workspace/members#invite",
        json!({"role": "viewer"}),
    )
    .await;
    assert_eq!(by_rosa["status"], "failed", "{by_rosa}");

    // A key minted for the workspace invites nobody either.
    let minted = backend
        .did("account/keys#mint", json!({"workspace": workspace}))
        .await;
    let mut agent = Mcp {
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted["token"].as_str().unwrap().to_owned(),
        session: None,
    };
    let (status, init) = agent.initialize().await;
    assert_eq!(status, 200, "{init}");
    let by_agent = members(
        &mut agent,
        None,
        "workspace/members#invite",
        json!({"role": "viewer"}),
    )
    .await;
    assert_eq!(by_agent["status"], "failed", "{by_agent}");
}
