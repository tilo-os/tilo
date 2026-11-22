//! A backend's key brings another person into a customer's workspace through
//! the action API: `workspace/invitations#invite` answers the link a person's
//! layout hands over, the person follows it on a page of their own and is in as
//! the role, and a key they approve themselves reaches the workspace as them.
//! Neither a key minted for the workspace nor the invited person's key
//! invites anyone.
use super::api::{ISSUER, Mcp};
use super::api_on_server::{accept_invitation, api_on_server, approved_key};
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
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &ada, &page, "Visa backend").await;
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

    // It invites to one of the workspace's places, and names whom an invitation is for: the
    // workspace's open invitations say so.
    let nowhere = members(
        &mut backend,
        here,
        "workspace/invitations#invite",
        json!({"role": "superuser"}),
    )
    .await;
    assert_eq!(nowhere["status"], "failed", "{nowhere}");
    let invited = members(
        &mut backend,
        here,
        "workspace/invitations#invite",
        json!({"role": "editor", "invitee_name": "rosa@example.com"}),
    )
    .await;
    assert_eq!(invited["status"], "ok", "{invited}");
    let invited = &invited["result"];
    assert_eq!(invited["role"], "editor", "{invited}");
    assert_eq!(invited["workspace"], workspace.as_str(), "{invited}");
    let link = invited["link"].as_str().unwrap().to_owned();
    // The link carries the signed invitation itself, which opens wherever it is followed.
    assert!(
        link.contains('#') && link.contains("invite="),
        "a link that carries its invitation: {link}"
    );
    let open = members(&mut backend, here, "workspace/invitations#list", json!({})).await;
    assert!(
        open["result"].as_array().is_some_and(|open| open
            .iter()
            .any(|one| one["invitee_name"] == "rosa@example.com" && one["role"] == "editor")),
        "{open}"
    );

    // Ada's own page follows an invitation too, as a person's layout joins the
    // workspace their backend made for them.
    let theirs = members(
        &mut backend,
        here,
        "workspace/invitations#invite",
        json!({"role": "editor"}),
    )
    .await;
    assert_eq!(theirs["status"], "ok", "{theirs}");
    let theirs = theirs["result"]["link"].as_str().unwrap().to_owned();
    let (_ada_page, joined) = accept_invitation(&on_server, &theirs, &ada, "Ada").await;
    assert_eq!(joined["role"], "editor", "{joined}");

    // Rosa follows her link on a page of hers and is in as an Editor.
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa Ortiz", 1);
    let (rosa_page, joined) = accept_invitation(&on_server, &link, &rosa, "Rosa Ortiz").await;
    assert_eq!(joined["workspace"], workspace.as_str(), "{joined}");
    assert_eq!(joined["role"], "editor", "{joined}");
    let listed = members(&mut backend, here, "workspace/members#list", json!({})).await;
    let rows = listed["result"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 2, "the two people, and no one else: {listed}");
    let ada_hex = hex::encode(ada.verifying_key().to_bytes());
    let rosa_hex = hex::encode(rosa.verifying_key().to_bytes());
    let row = |person: &str| {
        rows.iter()
            .find(|row| row["member"]["person"] == person)
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
            .is_some_and(|name| name.contains("Visa backend") || name == "server")),
        "no key of Ada's and not the server that runs the workspace names a person: {listed}"
    );

    // A key she approves herself reaches the workspace as her, and invites
    // nobody into a workspace she does not administer.
    let rosa_key = approved_key(&on_server, &rosa, &rosa_page, "Rosa's review").await;
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
        "workspace/invitations#invite",
        json!({"role": "viewer"}),
    )
    .await;
    assert_eq!(by_rosa["status"], "failed", "{by_rosa}");

    // A key minted for the workspace enters it as Ada, at her role: a key of
    // hers for that workspace, so it invites as she does, and is no person of
    // the workspace itself.
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
        "workspace/invitations#invite",
        json!({"role": "viewer"}),
    )
    .await;
    assert_eq!(by_agent["status"], "ok", "{by_agent}");
    let after = members(&mut backend, here, "workspace/members#list", json!({})).await;
    let after = after["result"].as_array().unwrap().clone();
    assert_eq!(
        after.len(),
        2,
        "her agent's key is not a person of the workspace: {after:?}"
    );
    assert_eq!(
        after
            .iter()
            .find(|row| row["member"]["person"] == ada_hex)
            .map(|row| row["name"].clone()),
        Some(json!("Ada")),
        "it does not rename her after its key: {after:?}"
    );
}

/// An owner's key lets a person in by their key, as an administrator,
/// through the action API: no invitation to hand over. That person's own key
/// then reaches the workspace and brings someone in themself. Handing the
/// workspace on is its owner's own change, signed on a device of theirs: the
/// API refuses it in words that say so, for the owner's key and for a key
/// minted for the workspace alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owners_key_adds_an_administrator_by_key_whose_key_brings_people_in() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &ada, &page, "Ada's laptop").await;
    let mut ada_mcp = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    let (status, init) = ada_mcp.initialize().await;
    assert_eq!(status, 200, "{init}");
    let made = ada_mcp
        .did("account/workspaces#create", json!({"name": "Lounge"}))
        .await;
    let workspace = made["workspace"]["id"].as_str().unwrap().to_owned();
    let here = Some(workspace.as_str());

    let (grace, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let grace_hex = hex::encode(grace.verifying_key().to_bytes());
    let bad = members(
        &mut ada_mcp,
        here,
        "workspace/members#add",
        json!({"person": "Grace", "role": "owner"}),
    )
    .await;
    assert_eq!(bad["status"], "failed", "{bad}");
    let handed = members(
        &mut ada_mcp,
        here,
        "workspace/members#add",
        json!({"person": grace_hex, "role": "owner", "name": "Grace Hopper"}),
    )
    .await;
    assert_eq!(handed["status"], "failed", "{handed}");
    assert!(
        handed["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(
                "only the workspace's owner makes this change, and from a device of their own"
            )),
        "{handed}"
    );
    let added = members(
        &mut ada_mcp,
        here,
        "workspace/members#add",
        json!({"person": grace_hex, "role": "administrator", "name": "Grace Hopper"}),
    )
    .await;
    assert_eq!(added["status"], "ok", "{added}");
    let rows = added["result"].as_array().unwrap().clone();
    let grace_row = rows
        .iter()
        .find(|row| row["member"]["person"] == grace_hex)
        .cloned()
        .unwrap_or_else(|| panic!("Grace has a row in {added}"));
    assert_eq!(grace_row["owner"], false, "{added}");
    assert_eq!(grace_row["administrator"], true, "{added}");
    assert_eq!(grace_row["name"], "Grace Hopper", "{added}");

    // Grace approves a key of her own, which reaches the workspace as hers,
    // and lets Hal in as an editor.
    let grace_page = enrol(&grace, "Grace's browser", DeviceCapabilities::member());
    let grace_key = approved_key(&on_server, &grace, &grace_page, "Grace's laptop").await;
    let mut grace_mcp = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: grace_key,
        session: None,
    };
    let (status, init) = grace_mcp.initialize().await;
    assert_eq!(status, 200, "{init}");
    let (hal, _) = tilo_user_identity::UserIdentity::generate("Hal", 1);
    let hal_hex = hex::encode(hal.verifying_key().to_bytes());
    let by_grace = members(
        &mut grace_mcp,
        here,
        "workspace/members#add",
        json!({"person": hal_hex, "role": "editor"}),
    )
    .await;
    assert_eq!(by_grace["status"], "ok", "{by_grace}");
    let hal_row = by_grace["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["member"]["person"] == hal_hex)
        .cloned()
        .unwrap_or_else(|| panic!("Hal has a row in {by_grace}"));
    assert_eq!(hal_row["role"], "editor", "{by_grace}");
    assert_eq!(hal_row["owner"], false, "{by_grace}");

    // The wait list reads, empty: this workspace's entry keeps none.
    let waiting = members(&mut grace_mcp, here, "workspace/waiting#list", json!({})).await;
    assert_eq!(waiting["status"], "ok", "{waiting}");
    assert_eq!(waiting["result"]["entries"], json!([]), "{waiting}");

    // A key minted for the workspace is Ada's own there: it makes an
    // administrator as she does, and hands the workspace to nobody.
    let minted = ada_mcp
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
    let (ivy, _) = tilo_user_identity::UserIdentity::generate("Ivy", 1);
    let ivy_hex = hex::encode(ivy.verifying_key().to_bytes());
    let by_agent = members(
        &mut agent,
        None,
        "workspace/members#add",
        json!({"person": ivy_hex, "role": "owner"}),
    )
    .await;
    assert_eq!(by_agent["status"], "failed", "{by_agent}");
    let by_agent = members(
        &mut agent,
        None,
        "workspace/members#add",
        json!({"person": ivy_hex, "role": "administrator"}),
    )
    .await;
    assert_eq!(by_agent["status"], "ok", "{by_agent}");
}

/// An owner's key moves a member to another role through the action API,
/// and an editor's key removes nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owners_key_moves_a_member_and_an_editors_removes_nobody() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_server, &ada, &page, "Ada's laptop").await;
    let mut ada_mcp = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: key,
        session: None,
    };
    let (status, init) = ada_mcp.initialize().await;
    assert_eq!(status, 200, "{init}");
    let made = ada_mcp
        .did("account/workspaces#create", json!({"name": "Studio"}))
        .await;
    let workspace = made["workspace"]["id"].as_str().unwrap().to_owned();
    let here = Some(workspace.as_str());
    let row = |answer: &Value, person: &str| {
        answer["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["member"]["person"] == person)
            .cloned()
    };

    let (grace, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let grace_hex = hex::encode(grace.verifying_key().to_bytes());
    let (hal, _) = tilo_user_identity::UserIdentity::generate("Hal", 1);
    let hal_hex = hex::encode(hal.verifying_key().to_bytes());
    for person in [&grace_hex, &hal_hex] {
        let added = members(
            &mut ada_mcp,
            here,
            "workspace/members#add",
            json!({"person": person, "role": "editor"}),
        )
        .await;
        assert_eq!(added["status"], "ok", "{added}");
    }

    let moved = members(
        &mut ada_mcp,
        here,
        &format!("workspace/members/{hal_hex}#role"),
        json!({"role": "viewer"}),
    )
    .await;
    assert_eq!(moved["status"], "ok", "{moved}");
    let hal_row = row(&moved, &hal_hex).unwrap_or_else(|| panic!("Hal has a row in {moved}"));
    assert_eq!(hal_row["role"], "viewer", "{moved}");

    // An editor removes nobody.
    let grace_page = enrol(&grace, "Grace's browser", DeviceCapabilities::member());
    let grace_key = approved_key(&on_server, &grace, &grace_page, "Grace's laptop").await;
    let mut grace_mcp = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: grace_key,
        session: None,
    };
    let (status, init) = grace_mcp.initialize().await;
    assert_eq!(status, 200, "{init}");
    let refused = members(
        &mut grace_mcp,
        here,
        &format!("workspace/members/{hal_hex}#remove"),
        json!({}),
    )
    .await;
    assert_eq!(refused["status"], "failed", "{refused}");
}
