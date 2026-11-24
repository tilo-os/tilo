//! A workspace that ran the native Notes app opens the tilolang Notes, hosted as a session starts
//! it: under the id it took from the native app. Its one-time conversion reads what the native app
//! kept of the workspace through the instance's own connection, where the workspace holds it now,
//! and carries every page over once, each with its title and its body.

use super::*;
use tilo_resource_store::wire::Command;

/// Where a workspace keeps the native notebook's record. The native app's test harness wrote it
/// under the package's own primary id instead (`@tilo:package-primary`).
const RECORD: &str = "derived:default@tab:0000000000000000";

/// The native app's own notebook documents, each by its document id in a workspace.
fn native_documents() -> Vec<(String, automerge::AutoCommit)> {
    include_str!("../../../tilolang-tilo/tests/legacy/rust_notes.hex")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (id, hex) = line.split_once('\t').expect("an id and the document");
            let bytes: Vec<u8> = hex
                .as_bytes()
                .chunks(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            let id = if id == "@tilo:package-primary" {
                RECORD
            } else {
                id
            };
            (id.to_owned(), automerge::AutoCommit::load(&bytes).unwrap())
        })
        .collect()
}

/// What the native Notes kept of the workspace, as the workspace holds it now: each document
/// whole, by the id the native app stored it under, with the id stamped in it. A workspace's
/// notebook record names its own id; the native app's test harness wrote it as the package's
/// primary document instead, so it is named here as a workspace names it.
fn native_notes_kept(workspace: &tilolang_process::testing::Workspace) {
    let documents = native_documents();
    let kind = documents
        .iter()
        .find_map(|(_, doc)| collab_types::document::identity::identity(doc))
        .map(|held| held.kind)
        .expect("the native pages name their resource type");
    for (id, mut doc) in documents {
        let id = collab_types::document::DocumentId::parse(&id).unwrap();
        if collab_types::document::identity::identity(&doc).is_none() {
            use automerge::transaction::Transactable as _;
            let meta = collab_types::document::identity::meta_obj(&mut doc).unwrap();
            doc.put(&meta, "id", id.as_str()).unwrap();
            doc.put(&meta, "kind", kind.as_str()).unwrap();
            doc.commit();
        }
        assert_eq!(
            collab_types::document::identity::identity(&doc).map(|held| held.id),
            Some(id.clone()),
            "each document names its id"
        );
        workspace
            .command(
                Command::RecordChange {
                    record: Some(id.as_str().to_owned()),
                    changes: tilo_resource_store::wire::Bytes(doc.save()),
                    more: false,
                    commit: None,
                },
                ADA,
            )
            .unwrap();
    }
}

fn text(
    host: &tilolang_tilo::host::Host,
    at: &tilolang_tilo::runtime::ResourceRef,
    field: &str,
) -> String {
    match host.runtime().read(at, field).unwrap() {
        Value::Text(text) => text,
        other => panic!("{field} reads {other:?}"),
    }
}

/// Every paragraph's words of the note's body.
fn body(
    host: &tilolang_tilo::host::Host,
    note: &tilolang_tilo::runtime::ResourceRef,
) -> Vec<String> {
    match host.runtime().read(note, "body").unwrap() {
        Value::Rich(rich) => rich.blocks.iter().map(|block| block.text.clone()).collect(),
        other => panic!("a body reads {other:?}"),
    }
}

/// Each note of the notebook, as its id, title and the words of its body.
fn notes_shown(host: &TilolangProcess) -> Vec<(String, String, Vec<String>)> {
    let runtime_host = host.instances().next().unwrap().instance().host().unwrap();
    let notebook = runtime_host.root_of(NOTES).expect("the notebook");
    let Value::List(notes) = runtime_host.runtime().read(&notebook, "notes").unwrap() else {
        panic!("the notes read as a list");
    };
    notes
        .iter()
        .map(|note| {
            let Value::Ref(note) = note else {
                panic!("a note is a resource: {note:?}");
            };
            let note = &*note.target;
            (
                note.item.clone().expect("a note is an item"),
                text(&runtime_host, note, "title"),
                body(&runtime_host, note),
            )
        })
        .collect()
}

#[test]
fn notes_started_as_a_session_does_carries_the_native_notebook_over_once() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let workspace = uuid::Uuid::new_v4();
    let bundle = notes();
    // Notes runs under its own id; what the native app of that id left in the store is read by
    // the package's own conversion, wherever it wrote.
    let route = tilolang_process::route_of(&bundle.meta, workspace, ADA);
    assert_eq!(
        route.app_id, NOTES,
        "Notes runs in the document of its own id"
    );
    let bus = Rc::new(Bus::default());
    let broker = Broker::on(&bus, route.clone(), "");
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        resource_type: None,
        handle: Some(ADA.to_owned()),
    }));
    let _ = manager.poll();
    // The device's content store, which the session opens for the app: the pictures of the
    // pages are carried into it.
    let content = tilo_content::ContentStore::memory();
    let connection = Connection::new(manager).expect("the connection joined its route");
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    // The notebook the native app kept of this workspace, in the workspace's documents: the
    // package's one-time conversion reads it there.
    native_notes_kept(&bus.workspace());
    let mut host = TilolangProcess::new(wake());
    let kept = tilolang_process::hosted::Kept {
        content: Some(content.clone()),
        chooser: None,
        clipboard: None,
        thumbnails: None,
        tools: None,
    };
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::new(
        Spec::new(NOTES, workspace.to_string(), 1, ADA),
        connection,
        (store, content),
        Arc::new(bundle),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake(),
    );
    host.add(hosted, Some(watched));
    let mut window = Window::over(ChannelTransport::new(theirs).unwrap());
    settle(&mut host, &mut window);

    let shown = notes_shown(&host);
    let pages: Vec<(&str, &str)> = shown
        .iter()
        .map(|(id, title, _)| (id.as_str(), title.as_str()))
        .collect();
    assert_eq!(
        pages,
        [
            ("00000000-0000-4000-8000-000000000001", "Handbook"),
            ("00000000-0000-4000-8000-000000000002", "Setup"),
            ("00000000-0000-4000-8000-000000000003", "Laptop"),
            ("00000000-0000-4000-8000-000000000004", "Security"),
            ("00000000-0000-4000-8000-000000000005", "Inbox"),
        ],
        "every page of the native notebook is a note, each once, keeping its id and title"
    );
    let body_of = |title: &str| {
        shown
            .iter()
            .find(|(_, shown, _)| shown == title)
            .map(|(_, _, body)| body.clone())
            .unwrap()
    };
    assert_eq!(
        body_of("Setup"),
        ["Accounts", "Mail", "Chat"],
        "each with the body its page had"
    );
    assert_eq!(
        body_of("Handbook").first().map(String::as_str),
        Some("Getting started")
    );
    assert_eq!(
        body_of("Laptop").first().map(String::as_str),
        Some("Pick one up at the desk. See Inbox.")
    );

    // Later frames carry nothing over again.
    settle(&mut host, &mut window);
    assert_eq!(
        notes_shown(&host),
        shown,
        "the notebook is carried over once"
    );
}

/// An app's route is its own id, whatever its manifest claimed it was published as before: a
/// `previously:` claim renames nothing, so a device that never installed the earlier app routes
/// and keeps the app's own records, and the broker admits it as its own worker.
#[test]
fn a_route_is_the_apps_own_whatever_the_manifest_claims_before() {
    let mut meta = notes().meta;
    // An app id of this test's own: its registration is no other test's.
    meta.app = "so.tilo.lang.route-test".into();
    let workspace = uuid::Uuid::new_v4();
    tilolang_tilo::schema::register_previously(
        &meta.app,
        std::slice::from_ref(&"so.tilo.lang.route-test-before".to_owned()),
    );
    assert_eq!(
        tilolang_process::route_of(&meta, workspace, ADA).app_id,
        meta.app,
        "the route is the app's own, whatever was claimed before"
    );
    tilolang_tilo::schema::register_previously(&meta.app, &[]);
}
