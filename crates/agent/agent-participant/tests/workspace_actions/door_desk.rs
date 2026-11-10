//! What an agent working through the door changes opens in front of the
//! workspace's people: each record it makes or changes is a window on the
//! workspace's shared desk, said to be its run's, which a person entering
//! the workspace finds open. What it only reads opens nothing.
use super::door_on_relay::{accept_invitation, approved_key, door_on_relay};
use super::front_door::{ISSUER, Mcp, upload};
use super::hosted_documents::{WRITER, application_pdf};
use super::*;
use serde_json::json;
use tilo_desktop_model::ownership::SharedDesktop;

const PDF: &str = "so.tilo.pdf-viewer";

/// The windows of `desktop` that show an `app` record, by tab id.
fn windows_of(desktop: &SharedDesktop, app: &str) -> Vec<u64> {
    desktop
        .records
        .iter()
        .filter(|(tab, _)| {
            desktop.tree.tab(*tab).is_some_and(|shown| {
                matches!(&shown.tile, tilo_desktop_model::TileContent::LocalApp { app_id, .. }
                    if app_id == app)
            })
        })
        .map(|(tab, _)| *tab)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_door_agent_changes_opens_on_the_workspaces_desk() {
    let on_relay = door_on_relay(ISSUER).await;
    let door = Arc::clone(&on_relay.door);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page_device = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page_device, "Visa backend").await;
    let mut backend = Mcp {
        door: &door,
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
        door: &door,
        path: format!("/w/{workspace}/mcp"),
        token: minted["token"].as_str().unwrap().to_owned(),
        session: None,
    };
    assert_eq!(agent.initialize().await.0, 200);

    // The agent uploads the applicant's form, fills a field and writes a
    // letter, exported as Word; nobody's page is open meanwhile.
    let (status, put) = upload(&agent, &workspace, &application_pdf()).await;
    assert_eq!(status, 201, "{put}");
    let form = put["content"].as_str().unwrap().to_owned();
    agent
        .did(
            "pdf-viewer#import-pdf-content",
            json!({"content": form, "name": "application.pdf"}),
        )
        .await;
    let read = agent
        .tilo_do(json!({"path": "pdf-viewer#read-form-fields"}))
        .await;
    let read = &read["result"]["structuredContent"];
    let surname = read["result"]["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|field| field["name"] == "surname")
        .unwrap_or_else(|| panic!("the form has a surname: {read}"))
        .clone();
    let edit = json!({"path": surname["path"], "op": "set",
                      "value": {"kind": "text", "value": "Okafor"}});
    agent
        .did(
            "pdf-viewer/application.pdf#field-edit",
            json!({"edit": edit}),
        )
        .await;
    agent
        .did(
            "word-processor#append-paragraph",
            json!({"text": "Dear Visa Officer,"}),
        )
        .await;
    agent
        .did("word-processor#export-content", json!({"format": "docx"}))
        .await;

    // Her reviewer enters the workspace on a page of hers: the form and the
    // letter are open there, once each, as windows of Ada's that her agent's
    // run opened.
    let invited = backend
        .tilo_do(
            json!({"path": "workspace/members#invite", "workspace": workspace,
                        "args": {"role": "editor"}}),
        )
        .await;
    let link = invited["result"]["structuredContent"]["result"]["link"]
        .as_str()
        .unwrap_or_else(|| panic!("an invitation's link: {invited}"))
        .to_owned();
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa Ortiz", 1);
    let (rosa_page, _) = accept_invitation(&on_relay, &link, &rosa, "Rosa Ortiz").await;
    let mut page = super::resource_support::open_admitted_peer(
        &on_relay.hosted,
        &rosa_page,
        &on_relay.host,
        &workspace,
    )
    .await;
    let deadline = tilo_test_env::hang_deadline();
    let desktop = loop {
        let desktop = page.desk();
        if let Some(desktop) = &desktop
            && !windows_of(desktop, PDF).is_empty()
            && !windows_of(desktop, WRITER).is_empty()
        {
            break desktop.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the agent's records never opened on the desk: {desktop:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let (form_windows, letter_windows) = (windows_of(&desktop, PDF), windows_of(&desktop, WRITER));
    assert_eq!(
        form_windows.len(),
        1,
        "one window shows the form: {desktop:?}"
    );
    assert_eq!(
        letter_windows.len(),
        1,
        "one window shows the letter: {desktop:?}"
    );
    let person = hex::encode(ada.verifying_key().to_bytes());
    for tab in form_windows.iter().chain(&letter_windows) {
        assert!(
            desktop
                .authors
                .get(tab)
                .is_some_and(|author| author.contains(&person)),
            "the window is Ada's: {desktop:?}"
        );
        let opened = desktop
            .opened_by
            .get(tab)
            .unwrap_or_else(|| panic!("the window says which run opened it: {desktop:?}"));
        assert!(opened.run.starts_with("agent-run:"), "{opened:?}");
        assert!(!opened.label.is_empty(), "{opened:?}");
    }
}
