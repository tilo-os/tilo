//! A listing is served while the search index reads a document, never after it.
//!
//! A write marks the document it landed in for search to read again, and a notebook's entries
//! cost seconds: reading them on the thread the service answers requests on held every
//! request of its share for as long as the reading took, so a listing right after a write cost
//! a second more than the same listing before it. The index reads on a thread of its own now,
//! and this holds the service to it by counting the requests it serves while that thread reads
//! ([`WorkspaceActions::search_served_beside_reading`]).
use super::hosted_agent_turns::{TASKS, file_issue, make_agent};
use super::*;
use serde_json::json;
use tilo_actions::remote::{ActionProvider as _, Caller, CatalogueQuery};
use tilo_server::workspace_agents::WorkspaceActionProvider;

/// The app that keeps notes.
const NOTES: &str = "so.tilo.notes";

/// How long a note is: a notebook whose entries take a while to read for search, as the one a
/// person's workspace holds does.
const NOTE_WORDS: usize = 3000;

/// What a note's body says, in `NOTE_WORDS` words.
fn body() -> String {
    (0..NOTE_WORDS)
        .map(|word| format!("word{word}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A caller as `root`'s person at `browser`'s device, in `workspace`.
fn person(root: &SigningKey, browser: &crate::support::Device, workspace: &str) -> Caller {
    Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_listing_after_a_write_is_served_while_the_index_reads() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (serving, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = &serving.workspaces().unwrap().clone();
    let workspace = &workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&serving, &browser, &server, workspace).await;
    let owner = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, workspaces, workspace, &owner, None).await;
    // The tracker the listing is of, and one issue in it.
    let issues_part = file_issue(&actions, workspaces, workspace, &root, (&server, &browser)).await;
    let catalogue = crate::common::catalogue();
    let id = WorkspaceId::parse(workspace).unwrap();
    let provider = WorkspaceActionProvider::new(actions.clone(), id);
    let person = person(&root, &browser, workspace);

    // A notebook of notes with real bodies: the app whose write marks a document for the index
    // is not the app the listing is of, as an agent writes a note and then lists its issues.
    let notebook = tilo_server::workspace_bots::workspace_resource(&catalogue, NOTES);
    workspaces
        .prepare_application(workspace, &server.cert, &catalogue, NOTES, &notebook)
        .await
        .unwrap();
    let notes = DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.clone(),
        document: notebook.clone(),
    };
    let pages = notes::note_list::chain_resource();
    let note = |title: &str| {
        tilo_server::workspace_agents::invoke_named(
            &provider,
            &person,
            &notes,
            &pages,
            "create-page",
            json!({"title": title, "markdown": body()})
                .as_object()
                .unwrap()
                .clone(),
        )
    };
    note("Note").await.expect("a note is written");

    // The listing an agent asks for, as `tilo_ls tasks/Issues` asks it of the tracker's document.
    let tracker = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: tilo_server::workspace_bots::workspace_resource(&catalogue, TASKS),
    };
    let listing = || CatalogueQuery {
        provider: Some(resource_provider(&tracker)),
        max_results: 128,
        ..CatalogueQuery::default()
    };
    let served = actions.clone();
    let list = || async {
        provider
            .query(&person, listing())
            .await
            .expect("the listing is answered")
            .expect("the tracker is listed")
    };
    // Its first answer, and the index read past what the notes' writes landed: a listing
    // costs the index nothing once it holds the document.
    let first = list().await;
    assert!(
        !first.rows.is_empty(),
        "the tracker lists the issue it holds: {issues_part}"
    );
    let deadline = tilo_test_env::hang_deadline();
    while served.search_documents_warmed() < 1 {
        assert!(Instant::now() < deadline, "the note is read for the index");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    note("Written before the listing")
        .await
        .expect("a note is written");
    // The write marked the notebook for search to read again. The first listing asks the
    // index for it; a listing after it is served while the index reads it, and one of them
    // must be: none is while the reading is done on the thread they are answered on.
    for asking in 0..200 {
        let beside = served.search_served_beside_reading();
        let listed = list().await;
        assert!(
            !listed.rows.is_empty(),
            "the tracker lists its issue while the index reads (ask {asking})"
        );
        let served_beside = served.search_served_beside_reading() - beside;
        if served_beside > 0 {
            eprintln!("listing {asking} was served while the index read the note");
            return;
        }
    }
    panic!(
        "no listing was served while the index read the note that was written: {} read \
         for the index",
        served.search_documents_warmed()
    );
}
