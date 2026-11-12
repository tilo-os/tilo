//! An agent working through the door hands a person a link to what it did:
//! a call that changed a record answers the record's link, and `tilo_link`
//! answers the link to any path. It is the record link a person copies in
//! the app, under the site the door and the relay announce, and carries no
//! secret: someone the workspace does not admit is told nothing.
use super::api::{ISSUER, Mcp, upload};
use super::api_on_relay::{api_on_relay, approved_key};
use super::hosted_documents::application_pdf;
use super::*;
use serde_json::{Value, json};
use tilo_reference::link::{Link, Mode, Scope, WebEntries, WebEntry};

const FORM: &str = "pdf-viewer/application.pdf";

/// `tilo_link` of `path` (in `workspace`, for an account's session): the
/// HTTP status and the reply.
async fn link_of(mcp: &mut Mcp<'_>, workspace: Option<&str>, path: &str) -> (u16, Value) {
    let mut arguments = json!({"path": path});
    if let Some(workspace) = workspace {
        arguments["workspace"] = json!(workspace);
    }
    mcp.post(json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                    "params": {"name": "tilo_link", "arguments": arguments,
                               "_meta": {"so.tilo.mcp": {"structured": true}}}}))
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_hands_over_the_link_to_the_record_it_changed() {
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
    assert_eq!(backend.initialize().await.0, 200);
    let made = backend
        .did(
            "account/workspaces#create",
            json!({"name": "Applicant: Jane Doe"}),
        )
        .await;
    let workspace = made["workspace"]["id"].as_str().unwrap().to_owned();
    let minted = backend
        .did("account/keys#mint", json!({"workspace": workspace}))
        .await;
    let mut agent = Mcp {
        api: &api,
        path: format!("/w/{workspace}/mcp"),
        token: minted["token"].as_str().unwrap().to_owned(),
        session: None,
    };
    assert_eq!(agent.initialize().await.0, 200);
    let (status, put) = upload(&agent, &workspace, &application_pdf()).await;
    assert_eq!(status, 201, "{put}");
    let form = put["content"].as_str().unwrap().to_owned();
    agent.did("pdf-viewer#open", json!({})).await;
    agent
        .did(
            "pdf-viewer#import-pdf-content",
            json!({"content": form, "name": "application.pdf"}),
        )
        .await;

    // A read changed nothing to show: it answers no link.
    let read = agent
        .tilo_do(json!({"path": "pdf-viewer#read-form-fields"}))
        .await;
    let read = &read["result"]["structuredContent"];
    assert_eq!(read["status"], "ok", "{read}");
    assert!(read.get("link").is_none(), "{read}");
    let surname = read["result"]["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|field| field["name"] == "surname")
        .unwrap_or_else(|| panic!("the form has a surname: {read}"))
        .clone();

    // A change answers the link to the record it changed.
    let edit = json!({"path": surname["path"], "op": "set",
                      "value": {"kind": "text", "value": "Okafor"}});
    let changed = agent
        .tilo_do(json!({"path": format!("{FORM}#field-edit"), "args": {"edit": edit}}))
        .await;
    let changed = &changed["result"]["structuredContent"];
    assert_eq!(changed["status"], "ok", "{changed}");
    let url = changed["link"]
        .as_str()
        .unwrap_or_else(|| panic!("a change answers its record's link: {changed}"))
        .to_owned();

    // It is the record link a person copies in the app: the workspace on the
    // relay, the app and the record, under the site, and no secret.
    let entries = WebEntries::new(vec![WebEntry::new(&on_relay.web_entry).unwrap()]);
    let link = Link::parse(&url, &entries).unwrap_or_else(|error| panic!("{url}: {error}"));
    assert_eq!(link.mode, Mode::App, "{url}");
    assert!(link.secrets.is_empty(), "a link carries no secret: {url}");
    let Scope::Room { host, id, .. } = &link.target.scope else {
        panic!("a link names its workspace: {url}");
    };
    assert_eq!(id.to_string(), workspace, "{url}");
    assert_eq!(host.as_str(), on_relay.host_origin, "{url}");
    assert_eq!(
        link.target
            .app
            .as_ref()
            .map(tilo_reference::link::AppId::as_str),
        Some("so.tilo.pdf-viewer"),
        "{url}"
    );
    assert!(link.target.record.is_some(), "{url}");
    assert_eq!(link.target.resource, None, "it opens the record: {url}");

    // `tilo_link` answers the same link for the record's path.
    let (status, asked) = link_of(&mut agent, None, FORM).await;
    assert_eq!(status, 200, "{asked}");
    assert_eq!(asked["result"]["isError"], false, "{asked}");
    assert_eq!(
        asked["result"]["structuredContent"]["link"],
        url.as_str(),
        "{asked}"
    );

    // Someone the workspace does not admit is told nothing about it.
    let (bob, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let bobs_page = enrol(&bob, "Bob's browser", DeviceCapabilities::member());
    let bobs_key = approved_key(&on_relay, &bob, &bobs_page, "Bob's backend").await;
    let mut stranger = Mcp {
        api: &api,
        path: "/mcp".into(),
        token: bobs_key,
        session: None,
    };
    assert_eq!(stranger.initialize().await.0, 200);
    let (status, refused) = link_of(&mut stranger, Some(&workspace), FORM).await;
    assert!(
        status != 200 || refused["result"]["isError"] == true || refused.get("error").is_some(),
        "{refused}"
    );
    assert!(!refused.to_string().contains(&url), "{refused}");
}
