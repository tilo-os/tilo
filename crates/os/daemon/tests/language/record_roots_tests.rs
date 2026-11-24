//! A built-in app's root, made on a device that writes its record with no window of the app
//! open: the language runtime the daemon links makes it, through the package the build carries.

use std::sync::Arc;

use tilo_actions::MemberId;
use tilo_actions::remote::CatalogueQuery;
use tilo_resource_store::wire::Command;
use tilo_resource_store::{Cell, Resource, Stamp, Value};
use tilo_workspace_service::workspaces::MemberDevice;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, declare_app, governor_with, test_host_holding,
};
use tilolang_workspace::records::{built_in, member, with_tl_record};

use super::mount;

/// Tasks, the built-in app whose record this test roots.
const APP: &str = "so.tilo.tasks";
const KIND: &str = "tl.so.tilo.tasks.Task";

/// Tasks' root, made in a workspace that runs a version of the app — as every workspace that
/// installed the catalogue's apps runs one: a record this device writes names that version's
/// surface, so the write is merged instead of held for a version nobody names, and the root is
/// made once rather than attempted again every time a page is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_built_in_apps_root_is_made_where_the_workspace_runs_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    mount(&workspaces);
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Tasks").await.unwrap().workspace;
    let surface = built_in(APP)
        .unwrap()
        .expect("Tasks is built into this build")
        .surface(APP)
        .expect("Tasks declares its surface");
    // Tasks' types are declared so this device's write of a task is judged by them.
    declare_app(&server, APP, &surface);
    let (_, canonical) =
        collab_types::surface::SurfaceFile::kept(&surface).expect("the built-in surface reads");
    // The version the workspace runs names its surface by the hash of the file it keeps, as an
    // installed release does.
    let release = tilo_peer_state::workspace_membership::AppRelease {
        app: APP.into(),
        version: "1.0.0".into(),
        surface: blake3::hash(canonical.as_bytes()).to_hex().to_string(),
        package: "00".repeat(32),
        publisher: "00".repeat(32),
        file: None,
        dev: true,
        pinned: false,
        payload: tilo_peer_state::workspace_membership::AppPayload::Tapp,
    };
    workspaces
        .commit_own_membership(
            &workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::InstallApp(release),
            None,
        )
        .await
        .unwrap();
    // The record's channel keys are here before it is written.
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    assert!(
        workspaces
            .own_route_keys(&route, true)
            .await
            .expect("the workspace answers for its keys")
            .is_ok(),
        "the record's channel keys are here"
    );
    // One task through this device's own store, so the record holds something to root.
    let person = hex::encode(cert.user_public_key);
    workspaces
        .own_resource_command(
            &workspace,
            APP,
            Command::Put {
                kind: KIND.into(),
                id: "t-0".into(),
                resource: Resource {
                    readers: "*".into(),
                    parent: None,
                    order: None,
                    fields: [("title".to_owned(), Cell::One(Value::Text("Task 0".into())))].into(),
                },
                stamp: Stamp {
                    verb: "$create".into(),
                    invoker: Stamp::by(&tilo_action_policy::Actor::Member(MemberId::person(
                        person.clone(),
                    ))),
                    invocation: "t-0".into(),
                    ..Stamp::default()
                },
            },
            Vec::new(),
        )
        .await
        .expect("this device writes the task");
    let packages = built_in(APP)
        .unwrap()
        .expect("Tasks is built into this build");
    let viewer = member(MemberId::person(person.clone()));
    let root = with_tl_record(
        &workspaces,
        &workspace,
        &packages,
        (viewer, &person),
        |opened| {
            opened
                .host()
                .root_of(APP)
                .map(|root| root.id_string())
                .map_err(|error| error.to_string())
        },
    )
    .await
    .expect("the root is made");
    // The root the launcher shows a window on: read back through the record as a page reads it.
    assert!(
        root.starts_with("root-"),
        "the record names a root resource: {root}"
    );
    assert!(
        workspaces
            .record_page(&workspace, &route, &person, &CatalogueQuery::default())
            .is_some(),
        "the record reads back once its root is made"
    );
    drop(workspaces);
    server.shutdown().await;
}
