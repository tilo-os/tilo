//! A device whose native apps kept their records in files of their own moves them into the
//! workspace's store once, through the import its action service publishes: each record founded
//! there holding what its file held.

use std::path::{Path, PathBuf};

use automerge::transaction::Transactable as _;
use automerge::{ROOT, ReadDoc as _};
use collab_types::document::DocumentId;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_user_identity::DeviceCert;

use super::{IMPORT, PROVIDER, device_resource};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{TestHost, hosting_device_with, test_host_with_app_files};

const APP: &str = "so.tilo.test.notebook";
const DEVICE: &str = "device-6b0f3c2e-8d1a-4f7e-9a52-3c4d5e6f7a8b";
const WINDOW: u64 = 7;
const KIND: &str = "so.tilo.test.notebook.page";
/// The notebook's surface: one kind, whose content is the page.
const SURFACE: &str = r#"{"format":2,"resources":[{"fields":[{"path":"title","type":"string"},{"path":"body","type":"string"}],"kind":"so.tilo.test.notebook.page","label":"Page","version":1}]}"#;

/// A device whose native apps kept their files below `home`, writing as the person's device
/// with the notebook's surface known here, and a workspace of theirs it holds.
async fn device(scratch: &Path, home: &Path) -> (TestHost, DeviceCert, String) {
    let (key, cert) = hosting_device_with(tilo_user_identity::DeviceCapabilities::member());
    let host =
        test_host_with_app_files(&scratch.join("service"), &key, &cert, vec![home.to_owned()])
            .await;
    let workspaces = host.workspaces();
    crate::workspaces::keep_surface(&workspaces.inner.host.surfaces, APP, SURFACE.as_bytes())
        .unwrap();
    workspaces.set_member_device(std::sync::Arc::new(crate::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: std::sync::Arc::new(key),
    }));
    workspaces
        .offer_own_workspaces(crate::workspaces::OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let workspace = workspaces.create("Notes").await.unwrap().workspace;
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    workspaces
        .own_route_keys(&route, true)
        .await
        .unwrap()
        .unwrap();
    (host, cert, workspace)
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

/// What the page `id` holds at `key` in `workspace`'s store; `None` when the store keeps no
/// such page.
async fn held(workspaces: &Workspaces, workspace: &str, id: &str, key: &str) -> Option<String> {
    let state = workspaces.workspace(workspace).await.unwrap();
    let mut hosted = state.lock().await;
    let mut store = workspaces.store_of(&mut hosted);
    held_field(&mut store, id, key)
}

/// What the page `id` holds at `key` in `store`, as [`held`] reads it.
fn held_field(store: &mut tilo_resource_store::Store, id: &str, key: &str) -> Option<String> {
    store.index().get(KIND, id)?;
    let doc = store.content_document(KIND, id).unwrap()?;
    doc.get(ROOT, key)
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok())
}

/// The saved view of `workspace` the store keeps for the window `WINDOW`, and who reads it.
async fn held_view(workspaces: &Workspaces, workspace: &str) -> Option<(Vec<u8>, String)> {
    let view = DocumentId::window_view(DEVICE, WINDOW);
    let state = workspaces.workspace(workspace).await.unwrap();
    let mut hosted = state.lock().await;
    let mut store = workspaces.store_of(&mut hosted);
    view_of(&mut store, view.route())
}

/// The saved view of the window at `route` in `store`, and who reads it, as [`held_view`] reads
/// it.
fn view_of(store: &mut tilo_resource_store::Store, route: &str) -> Option<(Vec<u8>, String)> {
    let doc = store.index().get(KIND, route)?.doc.clone();
    let readers = store.index().readers(&doc).unwrap().to_owned();
    let held = store.content_document(KIND, route).unwrap()?;
    let bytes = match held.get(ROOT, "view").unwrap()? {
        (automerge::Value::Scalar(scalar), _) => match scalar.as_ref() {
            automerge::ScalarValue::Bytes(bytes) => bytes.clone(),
            _ => return None,
        },
        _ => return None,
    };
    Some((bytes, readers))
}

/// Lay out the view `APP`'s window `WINDOW` kept of `workspace` below `home`, as it kept it: its
/// bytes.
fn kept_view(home: &Path, workspace: &str) -> Vec<u8> {
    let bytes = postcard::to_stdvec(&(1_u32, APP, DEVICE, workspace, WINDOW, "scrolled")).unwrap();
    write(
        &launched(home, workspace)
            .join("instance-views")
            .join(hex::encode(DEVICE))
            .join(hex::encode(workspace))
            .join(format!("{WINDOW}.postcard")),
        &bytes,
    );
    bytes
}

/// The data directory the launcher gave `APP` launched into `workspace`, below `home`.
fn launched(home: &Path, workspace: &str) -> PathBuf {
    home.join("tilo-workspaces")
        .join(workspace)
        .join(APP)
        .join(".local/share")
}

/// Lay out the page `id` `APP` kept of `workspace` below `home`, as it kept it: named in the
/// workspace's index, saved whole with a later change appended. Its files, with their bytes.
fn kept_page(home: &Path, workspace: &str, id: &DocumentId) -> Vec<(PathBuf, Vec<u8>)> {
    let scope = launched(home, workspace)
        .join(APP)
        .join("workspaces")
        .join(workspace);
    let mut index = automerge::AutoCommit::new();
    let records = index
        .put_object(ROOT, "tilo:records", automerge::ObjType::Map)
        .unwrap();
    index.put(&records, id.as_str(), true).unwrap();
    index.commit();

    let mut page = automerge::AutoCommit::new();
    page.put(ROOT, "title", "Field notes").unwrap();
    page.commit();
    let mut document = page.save();
    let heads = page.get_heads();
    page.put(ROOT, "body", "Saw a heron at the lake.").unwrap();
    page.commit();
    document.extend(page.save_after(&heads));

    let files = vec![
        (scope.join("state.automerge"), index.save()),
        (
            scope
                .join("documents")
                .join(id.storage_key())
                .join("state.automerge"),
            document,
        ),
    ];
    for (path, bytes) in &files {
        write(path, bytes);
    }
    files
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// An app's page kept in a file goes into the workspace's store once, holding what the file held
/// with the change appended after its save; asking again moves nothing more, and the files stay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_apps_files_go_into_its_workspace_store_once() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("data");
    let (host, cert, workspace) = device(scratch.path(), &home).await;
    let workspaces = host.workspaces().clone();
    let page = DocumentId::minted();
    let files = kept_page(&home, &workspace, &page);
    let view = kept_view(&home, &workspace);

    let moved = import(&workspaces, &cert, &workspace)
        .await
        .expect("the device moves its files");
    assert_eq!(
        moved,
        json!({
            "device": hex::encode(workspaces.lease_device(&workspace).await),
            "apps": { APP: 2 },
        }),
        "the page and the window's view go in, said by app"
    );
    assert_eq!(
        held_view(&workspaces, &workspace).await,
        Some((
            view,
            collab_types::Readers::people([cert.user_public_key])
                .unwrap()
                .text()
        )),
        "the window's view is its person's alone"
    );
    assert_eq!(
        held(&workspaces, &workspace, page.as_str(), "title")
            .await
            .as_deref(),
        Some("Field notes")
    );
    assert_eq!(
        held(&workspaces, &workspace, page.as_str(), "body")
            .await
            .as_deref(),
        Some("Saw a heron at the lake."),
        "the change appended after the page's save came with it"
    );

    for (path, bytes) in &files {
        assert_eq!(
            &std::fs::read(path).unwrap(),
            bytes,
            "{} is left",
            path.display()
        );
    }

    // Moved once: a file that turns up after it is not read, and asking again, as anyone or as
    // the next start, is answered with what the first move moved.
    let late = DocumentId::minted();
    kept_page(&home, &workspace, &late);
    assert_eq!(import(&workspaces, &cert, &workspace).await.unwrap(), moved);
    assert_eq!(
        workspaces
            .import_held_app_files()
            .await
            .get(&workspace)
            .cloned(),
        Some(Ok(moved.clone())),
        "a start asks for the turn and is answered the same"
    );
    assert_eq!(
        held(&workspaces, &workspace, late.as_str(), "title").await,
        None
    );
    drop(workspaces);
    host.shutdown().await;
}

/// The files of a workspace this device does not hold wait for it, and nothing an app kept
/// under a name that is no workspace goes anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_of_a_workspace_not_held_here_wait_for_it() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("data");
    let (host, cert, _) = device(scratch.path(), &home).await;
    let workspaces = host.workspaces().clone();
    let elsewhere = uuid::Uuid::new_v4().to_string();
    kept_page(&home, &elsewhere, &DocumentId::minted());
    kept_page(&home, "not-a-workspace", &DocumentId::minted());

    assert!(
        workspaces.import_held_app_files().await.is_empty(),
        "a start moves nothing of a workspace it does not hold"
    );
    let refused = import(&workspaces, &cert, &elsewhere).await;
    assert!(
        matches!(refused, Err(ActionCallError::Refused { .. })),
        "a workspace not held here takes nothing in until it is: {refused:?}"
    );
    drop(workspaces);
    host.shutdown().await;
}

/// What an app kept before it kept anything per workspace goes into the workspace the launcher
/// gave its data directory, once, as the app's primary record there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_apps_global_document_goes_in_once_as_its_primary_record() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("data");
    let (host, cert, workspace) = device(scratch.path(), &home).await;
    let workspaces = host.workspaces().clone();
    let mut global = automerge::AutoCommit::new();
    global.put(ROOT, "title", "Dusk").unwrap();
    global.commit();
    write(
        &launched(&home, &workspace)
            .join(APP)
            .join("state.automerge"),
        &global.save(),
    );

    let moved = import(&workspaces, &cert, &workspace).await.unwrap();
    assert_eq!(moved["apps"], json!({ APP: 1 }));
    let primary = DocumentId::package_primary();
    assert_eq!(
        held(&workspaces, &workspace, primary.route(), "title")
            .await
            .as_deref(),
        Some("Dusk")
    );
    drop(workspaces);
    host.shutdown().await;
}
