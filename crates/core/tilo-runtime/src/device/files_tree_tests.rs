//! The daemon is the one server of the workspace trees' socket: a mount that connects to it is
//! told the file access list of the workspaces it holds before anything of the tree, and where
//! the mount's folder stands becomes the daemon's resource `tilo.folders`, which this device's own
//! person reads and nobody else does.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the server and the device stay up until it ends"
)]

use std::sync::Arc;
use std::time::Duration;

use collab_types::caps::Access;
use ed25519_dalek::SigningKey;
use tilo_actions::remote::{
    ActionCallError, Caller, ResourceAddress, ResourceCopy, ResourceReads as _,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use tilo_workspace_service::files::access::WORKSPACE_FILES;
use tilo_workspace_service::files::{FOLDERS_APP, FolderList};

use super::workspaces::ServiceWorkspaces;

/// A device `root` certified, and its key.
fn enrol(
    root: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// A file of `workspace`, where the mount keeps that workspace's tree: the paths its members'
/// access ranges over.
fn note(workspace: &str) -> String {
    format!(
        "{WORKSPACE_FILES}/{}/{workspace}/brief/plan.md",
        tilo_paths::WORKSPACE_TREES_DIR
    )
}

/// Can the key `who` reach `path` with `access` in `list`? The decision every Files open asks.
fn may(list: &collab_types::FsAuthority, who: [u8; 32], path: &str, access: Access) -> bool {
    collab_types::decide_capability(
        &tilo_action_policy::Acting::person(hex::encode(who)),
        &collab_types::ResourceRef::FsPath(path.into()),
        access,
        &list.capabilities,
        Some(0),
    ) == collab_types::CapDecision::Allow
}

/// The daemon's folders, as `caller` reads them on this device.
fn folders(own: &ServiceWorkspaces, caller: &Caller) -> Result<FolderList, ActionCallError> {
    let read = own.copies().resource_reads().read(
        caller,
        &ResourceAddress {
            workspace: String::new(),
            app: FOLDERS_APP.into(),
            record: String::new(),
        },
    )?;
    let ResourceCopy::Reading { value, .. } = read else {
        panic!("the folders read as a reading, not {read:?}");
    };
    Ok(serde_json::from_value(value).expect("the folders' reading is a list of them"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mount_on_the_daemon_is_told_its_access_and_its_folder_is_the_daemons_resource() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig::loopback(state.path().join("server")),
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server has workspaces")
        .clone();
    let workspace = hosted.create("Studio").await.unwrap().workspace;
    hosted
        .enter(&workspace, &laptop_cert)
        .await
        .expect("a device of the person enters the workspace");
    let login = tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: workspace.clone(),
        workspace_name: Some("Studio".into()),
        agent: "tilo".into(),
        cert: laptop_cert.clone(),
        server_cert: server_cert.clone(),
        server_address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
    };
    let own = ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the laptop's own service opens");
    let entered = tilo_test_env::within(
        "the login enters its workspace",
        tilo_api::daemon::enter::connect(&laptop_key, &login, Some(own.lent())),
    )
    .await
    .expect("the login enters");
    tilo_test_env::within(
        "the device's service keeps the workspace",
        own.hold(&entered),
    )
    .await;

    // The daemon serves the trees, as it does once its record content runs.
    let daemons = Arc::new(tilo_workspace_service::files::socket::Daemons::default());
    super::content::document_content(&own, &state.path().join("blob.sock"), Arc::clone(&daemons));
    let socket = state.path().join("run").join("files-tree.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let (person, device) = (laptop_cert.user_public_key, laptop_cert.device_public_key);
    let _served = super::files_tree::serve(
        &socket,
        &own,
        daemons,
        state.path().join("identity.sock"),
        (hex::encode(person), device),
    )
    .expect("the daemon serves the trees once its record content runs");

    // A mount connects as the files daemon does.
    let kept = state.path().join("mount");
    let content = Arc::new(
        tilo_fs::ContentStore::open(kept.join("content"))
            .await
            .unwrap(),
    );
    let (_tree, service) = tilo_fs::tree_sync::remote::start(
        socket,
        workspace.clone(),
        (device, person),
        kept.join("namespace"),
        content,
    )
    .await
    .expect("the mount's tree opens");

    // It is told who may read and write the files, and the list holds the workspace's members.
    let mut access = service.access();
    let told = tilo_test_env::within("the mount is told its access list", async {
        loop {
            let held = access
                .borrow_and_update()
                .clone()
                .map(|json| serde_json::from_str::<collab_types::FsAuthority>(&json).unwrap())
                .filter(|list| may(list, person, &note(&workspace), Access::Write));
            if let Some(list) = held {
                return list;
            }
            access.changed().await.unwrap();
        }
    })
    .await;
    assert!(
        may(&told, device, &note(&workspace), Access::Write),
        "the device key the mount acts as holds its person's access"
    );
    let stranger = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng))
        .verifying_key()
        .to_bytes();
    assert!(
        !may(&told, stranger, &note(&workspace), Access::Read),
        "someone outside the workspace reads none of its files"
    );
    let elsewhere = tilo_desktop_model::WorkspaceId::new().to_string();
    assert!(
        !may(&told, person, &note(&elsewhere), Access::Read),
        "a workspace this device does not hold grants nothing"
    );

    // Where the folder stands is the daemon's own resource, which its person reads here.
    let me = Caller::authenticated(person, device);
    let shown = tilo_test_env::within("the folder's status is the daemon's resource", async {
        loop {
            let list = folders(&own, &me).expect("the person reads their device's folders");
            if let Some(folder) = list
                .folders
                .into_iter()
                .find(|folder| folder.workspace == workspace)
            {
                return folder;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        shown.name, "Studio",
        "the folder is named by its workspace's name"
    );
    let (_, phone) = enrol(&root, "phone", DeviceCapabilities::member());
    assert!(
        matches!(
            folders(
                &own,
                &Caller::authenticated(person, phone.device_public_key)
            ),
            Err(ActionCallError::Refused { .. })
        ),
        "another device, even the person's own, does not read this device's folders"
    );
    assert!(
        matches!(
            folders(&own, &Caller::local_agent("helper")),
            Err(ActionCallError::Refused { .. })
        ),
        "an agent does not read them"
    );
}
