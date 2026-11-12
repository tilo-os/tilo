//! A backend's key brings another person into a customer's workspace through
//! the door: `workspace/members#invite` answers the link a person's desk
//! hands over, the person follows it on a page of their own and is in as
//! the role, and a key they approve themselves reaches the workspace as them.
//! Neither a key minted for the workspace nor the invited person's key
//! invites anyone.
use super::api::{ISSUER, Mcp};
use super::api_on_relay::{accept_invitation, api_on_relay, approved_key};
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
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Visa backend").await;
    let mut backend = Mcp {
        api: &api,
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

    // Ada's own page follows an invitation too, as a person's desk joins the
    // workspace their backend made for them.
    let theirs = members(
        &mut backend,
        here,
        "workspace/members#invite",
        json!({"role": "editor"}),
    )
    .await;
    assert_eq!(theirs["status"], "ok", "{theirs}");
    let theirs = theirs["result"]["link"].as_str().unwrap().to_owned();
    let (_ada_page, joined) = accept_invitation(&on_relay, &theirs, &ada, "Ada").await;
    assert_eq!(joined["role"], "editor", "{joined}");

    // Rosa follows her link on a page of hers and is in as an Editor.
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa Ortiz", 1);
    let (rosa_page, joined) = accept_invitation(&on_relay, &link, &rosa, "Rosa Ortiz").await;
    assert_eq!(joined["workspace"], workspace.as_str(), "{joined}");
    assert_eq!(joined["role"], "editor", "{joined}");
    let listed = members(&mut backend, here, "workspace/members#list", json!({})).await;
    let rows = listed["result"]["members"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 2, "the two people, and no one else: {listed}");
    let ada_hex = hex::encode(ada.verifying_key().to_bytes());
    let rosa_hex = hex::encode(rosa.verifying_key().to_bytes());
    let row = |person: &str| {
        rows.iter()
            .find(|row| row["person"] == person)
            .cloned()
            .unwrap_or_else(|| panic!("{person} has a row in {listed}"))
    };
    let ada_row = row(&ada_hex);
    assert_eq!(ada_row["name"], "Ada", "{listed}");
    assert_eq!(ada_row["owner"], true, "{listed}");
    assert_eq!(ada_row["administrator"], true, "{listed}");
    let rosa_row = row(&rosa_hex);
    assert_eq!(rosa_row["name"], "Rosa Ortiz", "{listed}");
    assert_eq!(rosa_row["role"], "editor", "{listed}");
    assert_eq!(rosa_row["owner"], false, "{listed}");
    assert!(
        !rows.iter().any(|row| row["name"]
            .as_str()
            .is_some_and(|name| name.contains("Visa backend") || name == "relay")),
        "no key of Ada's and not the relay that runs the workspace names a person: {listed}"
    );

    // A key she approves herself reaches the workspace as her, and invites
    // nobody into a workspace she does not administer.
    let rosa_key = approved_key(&on_relay, &rosa, &rosa_page, "Rosa's review").await;
    // Her account lists the workspace she was invited into, with her role
    // there, so she reaches it by its name.
    let mut reviewer_account = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: rosa_key.clone(),
        session: None,
    };
    let (status, init) = reviewer_account.initialize().await;
    assert_eq!(status, 200, "{init}");
    let listed_for_her = reviewer_account
        .did("account/workspaces#list", json!({}))
        .await;
    let invited_into = listed_for_her["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|one| one["id"] == workspace.as_str())
        .cloned();
    assert_eq!(
        invited_into.map(|one| (one["name"].clone(), one["role"].clone())),
        Some((json!("Applicant: Jane Doe"), json!("editor"))),
        "the workspace she was invited into, with the role she holds there: {listed_for_her}"
    );

    let mut reviewer = Mcp {
        api: &api,
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

    // A key minted for the workspace enters it as Ada, at her role, and
    // invites nobody either: it is a key of hers, not a person of the
    // workspace.
    let minted = backend
        .did("account/keys#mint", json!({"workspace": workspace}))
        .await;
    let mut agent = Mcp {
        api: &api,
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
    let after = members(&mut backend, here, "workspace/members#list", json!({})).await;
    let after = after["result"]["members"].as_array().unwrap().clone();
    assert_eq!(
        after.len(),
        2,
        "her agent's key is not a person of the workspace: {after:?}"
    );
    assert_eq!(
        after
            .iter()
            .find(|row| row["person"] == ada_hex)
            .map(|row| row["name"].clone()),
        Some(json!("Ada")),
        "it does not rename her after its key: {after:?}"
    );
}
