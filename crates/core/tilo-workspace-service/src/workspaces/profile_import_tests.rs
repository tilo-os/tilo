//! A device whose native apps kept their records and windows' views in files of their own moves
//! them into the workspace once, through the import its action service publishes, and the app
//! reads them there as it reads any record.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{ROOT, ReadDoc as _};
use collaboration::resource::{
    CommandDocuments, DocumentCheckout, DocumentId, DocumentPort, ResourceOwners, ResourceType,
};
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_resource_store::wire::{Command, Reply};
use tilo_user_identity::DeviceCert;

use super::{IMPORT, PROVIDER, device_resource};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{governor, test_host_with_app_files};

const APP: &str = "so.tilo.test.notebook";
const DEVICE: &str = "device-6b0f3c2e-8d1a-4f7e-9a52-3c4d5e6f7a8b";
const WINDOW: u64 = 7;

/// The app's records as its windows read them, through the record commands the service answers
/// an app's broker connection with: `command` of `app` in `workspace`, answered on the service's
/// own workers while the caller waits, as a window's synchronous port does.
fn ask(
    workspaces: &Workspaces,
    runtime: &tokio::runtime::Handle,
    (app, workspace): (&str, &str),
    command: Command,
) -> Result<Reply, String> {
    let (workspaces, runtime) = (workspaces.clone(), runtime.clone());
    let (workspace, app) = (workspace.to_owned(), app.to_owned());
    std::thread::spawn(move || {
        runtime.block_on(async move {
            match command {
                Command::RecordChanges {
                    record: Some(id),
                    after,
                    from,
                } => {
                    workspaces
                        .opened_record_changes(&workspace, (&app, &id), &after, from)
                        .await
                }
                Command::RecordChange {
                    record: Some(id),
                    changes,
                    more: false,
                    ..
                } => workspaces
                    .opened_record_change(&workspace, (&app, &id), &changes.0, None, None)
                    .await
                    .map(|()| Reply::Written),
                Command::Records => workspaces.app_records(&workspace, &app).map(Reply::Ids),
                other => panic!("a window's port sends no {other:?}"),
            }
        })
    })
    .join()
    .expect("the command is answered")
}

/// Ask `workspaces`' device, as the device `cert` certifies, to move what its apps kept in files
/// into `workspace`: the import published on its action service, on its own resource there.
async fn import(
    workspaces: &Workspaces,
    cert: &DeviceCert,
    workspace: &str,
) -> Result<Value, ActionCallError> {
    let device = workspaces.lease_device(workspace).await;
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(device_resource(&device), IMPORT),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let caller = crate::service::local_caller(cert).in_workspace(workspace);
    let receipt = workspaces
        .device_actions()
        .invoke(&caller, request)
        .await
        .expect("the device answers")?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("the import ends as it is asked: {other:?}"),
    }
}

/// The document `app` keeps as `id` in `workspace`, as this device reads it there; `None` while
/// the workspace holds none.
async fn held_document(
    workspaces: &Workspaces,
    workspace: &str,
    app: &str,
    id: &DocumentId,
) -> Option<automerge::AutoCommit> {
    match workspaces
        .own_read(workspace, (app, id.route()), &[], usize::MAX)
        .await
        .expect("the workspace reads here")
    {
        tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. } => {
            Some(automerge::AutoCommit::load(&bytes).expect("a held document loads"))
        }
        tilo_peer_state::workspace_documents::DocumentRead::Absent => None,
        other => panic!("{app}/{} reads whole: {other:?}", id.route()),
    }
}

/// What the files a native app kept of `workspace` held: a record, and the view of the window
/// that showed it.
struct Profile {
    root: PathBuf,
    record: DocumentCheckout,
    files: Vec<(PathBuf, Vec<u8>)>,
}

/// Lay out the files `APP` kept below `home`, launched into `workspace`, as it kept them: the
/// record saved whole with a later change appended, and the window's view beside it.
fn kept_files(home: &Path, workspace: &str) -> Profile {
    let root = launched(home, workspace);
    let record = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("notebook"));

    let store = ResourceOwners::in_memory().open(APP, workspace).unwrap();
    store.create_resource(&record).unwrap();
    let (whole, appended) = store
        .with(&record.id, |doc| {
            doc.put(ROOT, "title", "Field notes").unwrap();
            doc.commit();
            let whole = doc.save();
            let heads = doc.get_heads();
            doc.put(ROOT, "body", "Saw a heron at the lake.").unwrap();
            doc.commit();
            (whole, doc.save_after(&heads))
        })
        .unwrap();
    let mut document = whole;
    document.extend(appended);

    let view = ui::application_view::SavedInstanceView {
        version: ui::application_view::SavedInstanceView::VERSION,
        app_id: APP.to_owned(),
        device_id: DEVICE.to_owned(),
        workspace_id: workspace.to_owned(),
        instance_id: WINDOW,
        document_id: record.id.as_str().to_owned(),
        resource_type: record.kind.as_str().to_owned(),
        view: ui::view::View {
            scroll: vec![0.0, 412.0],
            ..ui::view::View::default()
        },
        application: None,
        shows: None,
    };
    let files = vec![
        (
            root.join(".tilo-device-id"),
            format!("{DEVICE}\n").into_bytes(),
        ),
        (record_file(&root, workspace, &record.id), document),
        (
            root.join("instance-views")
                .join(hex::encode(DEVICE))
                .join(hex::encode(workspace))
                .join(format!("{WINDOW}.postcard")),
            postcard::to_stdvec(&view).unwrap(),
        ),
    ];
    for (path, bytes) in &files {
        write(path, bytes);
    }
    Profile {
        root,
        record,
        files,
    }
}

/// The data directory the launcher gave `APP` launched into `workspace`, below `home`.
fn launched(home: &Path, workspace: &str) -> PathBuf {
    home.join("tilo-workspaces")
        .join(workspace)
        .join(APP)
        .join(".local/share")
}

/// Where `APP`, whose data directory is `root`, kept its record `id` of `workspace`.
fn record_file(root: &Path, workspace: &str, id: &DocumentId) -> PathBuf {
    root.join(APP)
        .join("workspaces")
        .join(workspace)
        .join("documents")
        .join(id.storage_key())
        .join("state.automerge")
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A document with `key` set to `value`, saved whole.
fn document_with(key: &str, value: &str) -> Vec<u8> {
    let mut document = automerge::AutoCommit::new();
    document.put(ROOT, key, value).unwrap();
    document.commit();
    document.save()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_apps_files_go_into_its_workspace_once_and_the_app_reads_them_there() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let home = scratch.path().join("data");
    let server = test_host_with_app_files(
        &scratch.path().join("service"),
        &key,
        &cert,
        vec![home.clone()],
    )
    .await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Notes").await.unwrap().workspace;
    let profile = kept_files(&home, &workspace);

    let moved = import(&workspaces, &cert, &workspace)
        .await
        .expect("the device moves its files");
    assert_eq!(
        moved,
        json!({
            "device": hex::encode(workspaces.lease_device(&workspace).await),
            "apps": { APP: 2 },
        }),
        "the record and the window's view go in, said by app"
    );

    // The app, opened afterwards on this device, reads the record from the workspace, and its
    // window comes back on the view it had.
    let runtime = tokio::runtime::Handle::current();
    let reached = workspaces.clone();
    let port: Rc<dyn DocumentPort> = Rc::new(CommandDocuments(
        move |app: &str, workspace: &str, command| {
            ask(&reached, &runtime, (app, workspace), command)
        },
    ));
    let store = ResourceOwners::reading(Rc::clone(&port))
        .open(APP, &workspace)
        .unwrap();
    store
        .open_existing_resource_checked(&profile.record)
        .expect("the record opens from the workspace");
    let text = |key: &str| {
        store
            .read_checked(&profile.record.id, |doc| {
                doc.get(ROOT, key)
                    .unwrap()
                    .and_then(|(value, _)| value.into_string().ok())
            })
            .unwrap()
    };
    assert_eq!(text("title").as_deref(), Some("Field notes"));
    assert_eq!(
        text("body").as_deref(),
        Some("Saw a heron at the lake."),
        "the change appended after the record's save came with it"
    );
    let window = ui::application::NativeProfile::open(
        &profile.root,
        Some(&workspace),
        APP,
        Some(Rc::clone(&port)),
    )
    .unwrap();
    assert_eq!(window.device_id(), DEVICE);
    let view = window
        .saved_view(WINDOW)
        .unwrap()
        .expect("the window's view is in the workspace");
    assert_eq!(view.document_id, profile.record.id.as_str());
    assert_eq!(view.view.scroll, [0.0, 412.0]);

    // Moved once: a file that turns up after it is not read, and asking again, as anyone or as
    // the next start, is answered with what the first move moved.
    let late = DocumentId::minted();
    write(
        &record_file(&profile.root, &workspace, &late),
        &document_with("title", "Too late"),
    );
    assert_eq!(
        import(&workspaces, &cert, &workspace).await.unwrap(),
        moved,
        "a second ask moves nothing and answers the first answer"
    );
    let started = workspaces.import_held_app_files().await;
    assert_eq!(
        started.get(&workspace).cloned(),
        Some(Ok(moved.clone())),
        "a start asks for the turn and is answered the same"
    );
    assert!(
        held_document(&workspaces, &workspace, APP, &late)
            .await
            .is_none(),
        "nothing goes in after the move ran"
    );
    for (path, bytes) in &profile.files {
        assert_eq!(
            &std::fs::read(path).unwrap(),
            bytes,
            "{} is left",
            path.display()
        );
    }

    drop((store, window));
    server.shutdown().await;
}

/// The files of a workspace this device does not hold wait for it, and nothing an app kept
/// under a name that is no workspace goes anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_of_a_workspace_not_held_here_wait_for_it() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let home = scratch.path().join("data");
    let server = test_host_with_app_files(
        &scratch.path().join("service"),
        &key,
        &cert,
        vec![home.clone()],
    )
    .await;
    let workspaces = server.workspaces().clone();
    let elsewhere = uuid::Uuid::new_v4().to_string();
    kept_files(&home, &elsewhere);
    kept_files(&home, "not-a-workspace");

    assert!(
        workspaces.import_held_app_files().await.is_empty(),
        "a start moves nothing of a workspace it does not hold"
    );
    let refused = import(&workspaces, &cert, &elsewhere).await;
    assert!(
        matches!(refused, Err(ActionCallError::Refused { .. })),
        "a workspace not held here takes nothing in until it is: {refused:?}"
    );
    server.shutdown().await;
}

/// What an app kept before it kept anything per workspace goes into the workspace the launcher
/// gave its data directory, once, as the app's primary record there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_apps_global_document_goes_in_once_as_its_primary_record() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let home = scratch.path().join("data");
    let server = test_host_with_app_files(
        &scratch.path().join("service"),
        &key,
        &cert,
        vec![home.clone()],
    )
    .await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Notes").await.unwrap().workspace;
    let global = launched(&home, &workspace)
        .join(APP)
        .join("state.automerge");
    write(&global, &document_with("theme", "dusk"));

    let moved = import(&workspaces, &cert, &workspace).await.unwrap();
    assert_eq!(moved["apps"], json!({ APP: 1 }));
    let primary = held_document(&workspaces, &workspace, APP, &DocumentId::package_primary())
        .await
        .expect("the app's primary record is in the workspace");
    assert_eq!(
        primary
            .get(ROOT, "theme")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
            .as_deref(),
        Some("dusk")
    );

    write(&global, &document_with("theme", "noon"));
    assert_eq!(
        import(&workspaces, &cert, &workspace).await.unwrap(),
        moved,
        "it goes in once"
    );
    let primary = held_document(&workspaces, &workspace, APP, &DocumentId::package_primary())
        .await
        .unwrap();
    assert_eq!(
        primary
            .get(ROOT, "theme")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
            .as_deref(),
        Some("dusk"),
        "what the workspace holds is not overwritten"
    );
    server.shutdown().await;
}
