//! A workspace that ran the native Notes app opens the tilolang Notes, hosted as a session starts
//! it: under the id it took from the native app, its documents kept under the id it was published
//! as before. The session grants the process, read-only, what the native app kept of the workspace
//! in the store it wrote; its one-time conversion reads the notebook there and carries every page
//! over once, each with its title and its body.

use super::*;

/// The id Notes was published as before it took the native app's: its own documents stay there.
const PUBLISHED_BEFORE: &str = "so.tilo.lang.notes";

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

/// What the native Notes kept of `workspace` on the device, in the store it wrote at `root` (its
/// data home), as the native app stored a document: by a name made from its id, the id stamped in
/// it. A workspace's notebook record names its own id; the native app's test harness wrote it as
/// the package's primary document instead, so it is named here as a workspace names it.
fn native_notes_kept(root: &std::path::Path, workspace: uuid::Uuid) {
    let store: Rc<dyn durable_store::DurableStore> = Rc::new(app_state::FsStore::new(root));
    let workspace = workspace.to_string();
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
        collaboration::document::save::resource_document(
            &store,
            NOTES,
            &workspace,
            &id.storage_key(),
        )
        .unwrap()
        .save(&mut doc)
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
    // The session starts Notes under its id with its documents under the id it had before, and
    // the host honours that earlier id, as it does for a launch the installed catalogue names.
    tilolang_tilo::schema::register_previously(NOTES, &[PUBLISHED_BEFORE.to_owned()]);
    let route = tilolang_process::route_under(&bundle.meta, PUBLISHED_BEFORE, workspace, ADA);
    assert_eq!(
        route.app_id, PUBLISHED_BEFORE,
        "Notes' documents stay where it kept them"
    );
    let broker = Broker::new(route.clone());
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
    let record = InstanceDocument::new(automerge::AutoCommit::new(), manager, route, ADA)
        .keeping_content_in(content.clone());
    assert!(record.is_origin(), "this device is the workspace's origin");
    // The native app's data home, of which the session grants the process the store it wrote;
    // the process keeps its own storage beside it.
    let device = tempfile::tempdir().unwrap();
    let native = device.path().join("native");
    native_notes_kept(&native, workspace);
    let mut host =
        TilolangProcess::new(Some(device.path().join("storage")), wake()).reading_earlier(native);
    let kept = tilolang_process::hosted::Kept {
        content: Some(content),
        ..host.kept(PUBLISHED_BEFORE).unwrap()
    };
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::with_document(
        Spec::new(NOTES, workspace.to_string(), 1, ADA),
        record,
        Arc::new(bundle),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        &Rc::new(Phrases::new("en-US")),
        host.formats(),
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

/// Notes as a server runs it for one of its workspaces: its process reads the workspace through
/// the server's broker, so the record is not the document's origin by itself.
/// `for_server` makes it the run that carries the package's migrations; `record` is the workspace's document
/// as the broker holds it. The server granted the process what the native Notes kept, at `native`.
/// Answers the host once it settled, and what the run handed the workspace.
fn run_on_the_server(
    workspace: uuid::Uuid,
    record: automerge::AutoCommit,
    native: &std::path::Path,
    storage: &std::path::Path,
    for_server: bool,
) -> (TilolangProcess, Sent) {
    let bundle = notes();
    tilolang_tilo::schema::register_previously(NOTES, &[PUBLISHED_BEFORE.to_owned()]);
    let route = tilolang_process::route_under(&bundle.meta, PUBLISHED_BEFORE, workspace, ADA);
    let broker = Broker::new(route.clone());
    let sent = broker.sent();
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        resource_type: None,
        handle: Some(ADA.to_owned()),
    }));
    let _ = manager.poll();
    let record = InstanceDocument::new(record, manager, route, ADA)
        .keeping_content_in(tilo_content::ContentStore::memory())
        .read_through_a_broker();
    let mut host =
        TilolangProcess::new(Some(storage.to_path_buf()), wake()).reading_earlier(native.into());
    let kept = host.kept(PUBLISHED_BEFORE).unwrap();
    let spec = Spec::new(NOTES, workspace.to_string(), 1, ADA);
    let spec = if for_server {
        spec.carrying_migrations()
    } else {
        spec.showing(Shows::Nothing)
    };
    let hosted = Hosted::with_document(
        spec,
        record,
        Arc::new(bundle),
        &kept,
        Box::new(tilolang_process::hosted::NoView),
        &Rc::new(Phrases::new("en-US")),
        host.formats(),
        wake(),
    );
    host.add(hosted, None);
    settle_all(&mut host, &mut [], |_| false);
    (host, sent)
}

/// The workspace's document as it holds `before` and what a run handed it since.
fn handed(before: &automerge::AutoCommit, sent: &Sent) -> automerge::AutoCommit {
    let mut held = before.clone();
    tilo_automerge::ApplyIncoming::merge_incoming(&mut held, &mut workspace_copy(sent)).unwrap();
    held
}

/// Each note the workspace's document `held` holds, as a reader of it is shown them.
fn notes_held(
    workspace: uuid::Uuid,
    held: automerge::AutoCommit,
    native: &std::path::Path,
) -> Vec<(String, String, Vec<String>)> {
    let device = tempfile::tempdir().unwrap();
    let (reader, _) = run_on_the_server(workspace, held, native, device.path(), false);
    let held = reader
        .instances()
        .next()
        .unwrap()
        .instance()
        .host()
        .unwrap();
    let Ok(notebook) = held.root_of(NOTES) else {
        return Vec::new();
    };
    let Value::List(notes) = held.runtime().read(&notebook, "notes").unwrap() else {
        panic!("the notes read as a list");
    };
    if notes.is_empty() {
        return Vec::new();
    }
    notes_shown(&reader)
}

#[test]
fn the_run_that_carries_migrations_carries_the_native_notebook_once_and_ends() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let workspace = uuid::Uuid::new_v4();
    let device = tempfile::tempdir().unwrap();
    let native = device.path().join("native");
    native_notes_kept(&native, workspace);
    let empty = automerge::AutoCommit::new();

    // A run that is not the server's, reading the workspace through the broker, carries nothing:
    // it is no origin.
    assert_eq!(
        notes_held(workspace, empty.clone(), &native),
        [],
        "a run that carries no migrations carries nothing of what the native app kept"
    );

    let (mut first, sent) = run_on_the_server(
        workspace,
        empty.clone(),
        &native,
        &device.path().join("server"),
        true,
    );
    assert!(
        first.is_empty() && first.take_ended() == [1],
        "the server's run ends once it carried what the native app kept"
    );
    assert!(
        documents_sent(&sent) > 0,
        "the run handed the workspace what it carried"
    );
    let held = handed(&empty, &sent);
    let carried = notes_held(workspace, held.clone(), &native);
    let titles: Vec<&str> = carried.iter().map(|(_, title, _)| title.as_str()).collect();
    assert_eq!(
        titles,
        ["Handbook", "Setup", "Laptop", "Security", "Inbox"],
        "the server's run carries every page of the native notebook"
    );
    assert_eq!(
        carried
            .iter()
            .find(|(_, title, _)| title == "Setup")
            .map(|(_, _, body)| body.clone()),
        Some(vec!["Accounts".into(), "Mail".into(), "Chat".into()]),
        "each with the body its page had"
    );

    // The server starts again over what the workspace now holds: nothing is carried twice, and
    // the run ends again.
    let (mut again, sent) = run_on_the_server(
        workspace,
        held.clone(),
        &native,
        &device.path().join("server"),
        true,
    );
    assert!(
        again.is_empty() && again.take_ended() == [1],
        "the second run ends too"
    );
    assert_eq!(
        notes_held(workspace, handed(&held, &sent), &native),
        carried,
        "a second start carries the notebook no second time"
    );
}
