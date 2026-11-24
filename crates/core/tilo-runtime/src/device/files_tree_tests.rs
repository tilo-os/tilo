//! The daemon is the one server of the workspace trees' socket. A mount that connects to it is
//! told the file access list of the workspaces it holds before anything of the tree, and puts it
//! in force as the files daemon does (its capability file, read by every open of the mount); a
//! role changed while the mount runs is in force at its next open. Two devices' mounts of one
//! workspace sync through their daemons. Where a mount's folder stands is the daemon's resource
//! `tilo.folders`, which this device's own person reads in their own workspace and nobody else
//! reads.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; the server and the devices stay up \
              until it ends"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{
    ActionCallError, Caller, ResourceAddress, ResourceCopy, ResourceReads as _,
};
use tilo_fs::Access;
use tilo_server_protocol::workspaces::Role;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
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

/// A fresh idempotency key for a governance request.
fn fresh_id() -> [u8; 32] {
    SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng))
        .verifying_key()
        .to_bytes()
}

/// The server keeping the workspace, and the device it governs it from.
struct Site {
    server: tilo_server::daemon::Server,
    cert: DeviceCert,
    workspace: String,
}

/// A computer of a member as its daemon runs it: its workspace service, holding the workspace,
/// and the trees' socket it serves.
struct Laptop {
    own: Arc<ServiceWorkspaces>,
    _served: super::files_tree::FilesTree,
    socket: PathBuf,
    person: [u8; 32],
    device: [u8; 32],
}

/// The daemon of the device `key`, `cert`, logged in to `site`'s workspace, keeping what it keeps
/// under `dir`, serving the trees once its record content runs.
async fn laptop(dir: &Path, (key, cert): (&SigningKey, &DeviceCert), site: &Site) -> Laptop {
    let login = tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: site.workspace.clone(),
        workspace_name: Some("Studio".into()),
        agent: "tilo".into(),
        cert: cert.clone(),
        server_cert: site.cert.clone(),
        server_address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: site.server.node_addr(),
            relay_url: None,
            web: None,
        }),
    };
    let own = ServiceWorkspaces::open_for_test(
        &dir.join("own"),
        key,
        cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the laptop's own service opens");
    let entered = tilo_test_env::within(
        "the login enters its workspace",
        tilo_api::daemon::enter::connect(key, &login, Some(own.lent())),
    )
    .await
    .expect("the login enters");
    tilo_test_env::within(
        "the device's service keeps the workspace",
        own.hold(&entered),
    )
    .await;
    let daemons = Arc::new(tilo_workspace_service::files::socket::Daemons::default());
    super::content::document_content(&own, &dir.join("blob.sock"), Arc::clone(&daemons));
    let socket = dir.join("run").join("files-tree.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let (person, device) = (cert.user_public_key, cert.device_public_key);
    let served = super::files_tree::serve(
        &socket,
        &own,
        daemons,
        dir.join("identity.sock"),
        (hex::encode(person), device),
    )
    .expect("the daemon serves the trees once its record content runs");
    Laptop {
        own,
        _served: served,
        socket,
        person,
        device,
    }
}

/// A workspace's tree mounted on a laptop as its files daemon mounts it: synced over the
/// daemon's socket, the access list it is told put in force through its capability file, and
/// every open decided by the mount's own check.
struct Mount {
    tree: tilo_fs::tree_sync::TreeSync,
    _service: tilo_fs::tree_sync::remote::Service,
    fs: tilo_fs::QuickFS,
    /// The list in force, as the files daemon keeps it.
    caps: Arc<tilo_fs::CapabilityFile>,
    _follow: tokio::task::JoinHandle<()>,
}

async fn mount(laptop: &Laptop, dir: &Path, workspace: &str) -> Mount {
    std::fs::create_dir_all(dir).unwrap();
    let content = Arc::new(
        tilo_fs::ContentStore::open(dir.join("content"))
            .await
            .unwrap(),
    );
    let (tree, service) = tilo_fs::tree_sync::remote::start(
        laptop.socket.clone(),
        workspace.to_owned(),
        (laptop.device, laptop.person),
        dir.join("namespace"),
        Arc::clone(&content),
        dir.join("mount"),
    )
    .await
    .expect("the mount's tree opens");
    // Nothing is granted until the daemon says what is.
    let caps_path = dir.join("caps.json");
    std::fs::write(&caps_path, br#"{"capabilities":[]}"#).unwrap();
    let caps = Arc::new(
        tilo_fs::CapabilityFile::load(&caps_path, laptop.person, Vec::new())
            .expect("an empty capability list binds"),
    );
    let follow = {
        let (caps, mut told) = (Arc::clone(&caps), service.access());
        tokio::spawn(async move {
            loop {
                let said = told.borrow_and_update().clone();
                if let Some(json) = said {
                    caps.keep(&json).expect("the daemon's access list is one");
                }
                if told.changed().await.is_err() {
                    return;
                }
            }
        })
    };
    let hydrator = Arc::new(tilo_fs::Hydrator::new(content));
    let heuristics = Arc::new(tilo_fs::Heuristics::new(Arc::clone(&hydrator)));
    let fs = tilo_fs::QuickFS::new(Arc::clone(&tree.namespace), hydrator, heuristics)
        .with_acl(caps.handle(), laptop.person);
    Mount {
        tree,
        _service: service,
        fs,
        caps,
        _follow: follow,
    }
}

impl Mount {
    /// The file at `path` (folders by `/`), made with the folders it is in.
    fn file(&self, path: &str) -> u64 {
        let mut namespace = self.tree.namespace.lock().unwrap();
        let (folders, name) = path.rsplit_once('/').unwrap_or(("", path));
        let mut parent = tilo_fs::ROOT_INO;
        let mut walked = String::new();
        for folder in folders.split('/').filter(|folder| !folder.is_empty()) {
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(folder);
            parent = match namespace.resolve_path_ino(&walked) {
                Some(ino) => ino,
                None => namespace
                    .create_dir(parent, folder, 1_000)
                    .expect("a folder"),
            };
        }
        namespace
            .create_file(parent, name, None, 0, 1_000)
            .expect("a file")
    }

    /// Whether the mount's tree holds `path`.
    fn holds(&self, path: &str) -> bool {
        self.tree
            .namespace
            .lock()
            .unwrap()
            .resolve_path_ino(path)
            .is_some()
    }

    /// What the mount answers an open of `ino` for `access`: `Ok`, or the errno it refuses with.
    fn open(&self, ino: u64, access: Access) -> Result<(), i32> {
        self.fs.check_access(ino, access)
    }

    /// Wait until an open of `ino` for `access` is answered `answer`.
    async fn until(&self, what: &str, ino: u64, access: Access, answer: Result<(), i32>) {
        tilo_test_env::within(what, async {
            while self.open(ino, access) != answer {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }
}

/// Where the mount keeps `workspace`'s own files: the tree under its id.
fn tree_of(workspace: &str) -> String {
    format!("{}/{workspace}", tilo_paths::WORKSPACE_TREES_DIR)
}

/// The daemon's folders, as `caller` reads them in `workspace` on this device.
fn folders(
    own: &ServiceWorkspaces,
    caller: &Caller,
    workspace: &str,
) -> Result<FolderList, ActionCallError> {
    let read = own.copies().resource_reads().read(
        caller,
        &ResourceAddress {
            workspace: workspace.to_owned(),
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
async fn mounts_on_the_daemons_sync_and_enforce_what_the_workspace_grants_now() {
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (bob, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let (server_key, server_cert) = enrol(&ada, "home-server", DeviceCapabilities::member());
    let (ada_key, ada_cert) = enrol(&ada, "Ada's laptop", DeviceCapabilities::primary());
    let (bob_key, bob_cert) = enrol(&bob, "Bob's laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig::loopback(state.path().join("server")),
        tilo_server::daemon::ServerIdentity {
            user_public_key: ada.verifying_key().to_bytes(),
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
        .add_member(
            &workspace,
            &server_cert,
            tilo_workspace_service::workspaces::AddMemberRequest {
                request_id: fresh_id(),
                user_public_key: bob_cert.user_public_key,
                place: tilo_workspace_service::workspaces::MemberPlace::Role(Role::Editor),
                name: Some("Bob".into()),
            },
        )
        .await
        .expect("the owner adds Bob as an editor");
    for cert in [&ada_cert, &bob_cert] {
        hosted
            .enter(&workspace, cert)
            .await
            .expect("a member's device enters the workspace");
    }
    let site = Site {
        server,
        cert: server_cert.clone(),
        workspace: workspace.clone(),
    };
    let ada_laptop = laptop(&state.path().join("ada"), (&ada_key, &ada_cert), &site).await;
    let bob_laptop = laptop(&state.path().join("bob"), (&bob_key, &bob_cert), &site).await;
    let ada_mount = mount(&ada_laptop, &state.path().join("ada-mount"), &workspace).await;
    let bob_mount = mount(&bob_laptop, &state.path().join("bob-mount"), &workspace).await;

    // Each mount's tree syncs through its own daemon to the other's.
    let tree = tree_of(&workspace);
    let plan = format!("{tree}/brief/plan.md");
    ada_mount.file(&plan);
    tilo_test_env::within("Bob's mount holds the file Ada's made", async {
        while !bob_mount.holds(&plan) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let answer = format!("{tree}/brief/answer.md");
    bob_mount.file(&answer);
    tilo_test_env::within("Ada's mount holds the file Bob's made", async {
        while !ada_mount.holds(&answer) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    // An editor writes the workspace's files.
    let note = bob_mount
        .tree
        .namespace
        .lock()
        .unwrap()
        .resolve_path_ino(&plan)
        .unwrap();
    bob_mount
        .until(
            "Bob's mount lets the editor write",
            note,
            Access::Write,
            Ok(()),
        )
        .await;

    // A path outside what is granted is refused: another workspace's tree, and the device's
    // own files beside the trees.
    let elsewhere = tilo_desktop_model::WorkspaceId::new().to_string();
    let theirs = bob_mount.file(&format!("{}/plan.md", tree_of(&elsewhere)));
    assert_eq!(bob_mount.open(theirs, Access::Read), Err(libc::ENOENT));
    assert_eq!(bob_mount.open(theirs, Access::Write), Err(libc::ENOENT));
    let beside = bob_mount.file("vault.txt");
    assert_eq!(bob_mount.open(beside, Access::Read), Err(libc::ENOENT));

    // A symbolic link is a file of the tree like any other: the mount presents it as one, so
    // nothing follows it out of the granted folder, and opening it is decided by where the link
    // itself is.
    let link = bob_mount.file(&format!("{tree}/escape"));
    let outside_link = bob_mount.file("escape-in");
    {
        let mut namespace = bob_mount.tree.namespace.lock().unwrap();
        for ino in [link, outside_link] {
            let record = namespace.document_id_for(ino).unwrap();
            assert!(namespace.put_posix_attrs(
                record,
                &tilo_fs::namespace::PosixAttrs {
                    mode: 0o777,
                    symlink: true,
                }
            ));
            assert_eq!(
                namespace.getattr(ino).unwrap().kind,
                tilo_fs::node::FileType::RegularFile,
                "the mount never hands the kernel a link to follow"
            );
        }
    }
    assert_eq!(bob_mount.open(link, Access::Write), Ok(()));
    assert_eq!(
        bob_mount.open(outside_link, Access::Read),
        Err(libc::ENOENT)
    );
    assert_eq!(
        bob_mount.open(outside_link, Access::Write),
        Err(libc::ENOENT)
    );

    // `..` never leaves the granted folder: no name in the tree is one, and a path that has one
    // is refused by the mount's check, whoever asks.
    {
        let mut namespace = bob_mount.tree.namespace.lock().unwrap();
        let folder = namespace.resolve_path_ino(&tree).unwrap();
        assert!(
            namespace
                .create_file(folder, "..", None, 0, 1_000)
                .is_none()
        );
        assert!(namespace.create_dir(folder, "..", 1_000).is_none());
    }
    let acl = bob_mount.caps.handle();
    assert!(acl.permits(&bob_laptop.person, &format!("/{plan}"), Access::Read));
    for climbing in [
        format!("/{tree}/../{elsewhere}/plan.md"),
        format!("/{tree}/brief/../../../vault.txt"),
        format!("/{tree}/./brief/plan.md"),
    ] {
        assert!(
            !acl.permits(&bob_laptop.person, &climbing, Access::Read),
            "{climbing} is refused"
        );
        assert!(!acl.permits(&bob_laptop.person, &climbing, Access::Write));
        assert!(!acl.visible(&bob_laptop.person, &climbing));
    }

    // Bob's role is lowered while his mount runs: his next write is refused, his reads go on,
    // and nothing is mounted again.
    hosted
        .set_member_role(
            &workspace,
            &server_cert,
            tilo_workspace_service::workspaces::SetMemberRoleRequest {
                request_id: fresh_id(),
                user_public_key: bob_cert.user_public_key,
                role: Role::Viewer,
            },
        )
        .await
        .expect("the owner lowers Bob to a viewer");
    bob_mount
        .until(
            "Bob's mount refuses the viewer's write",
            note,
            Access::Write,
            Err(libc::EROFS),
        )
        .await;
    assert_eq!(bob_mount.open(note, Access::Read), Ok(()));

    // Raised again, he writes again on the same mount.
    hosted
        .set_member_role(
            &workspace,
            &server_cert,
            tilo_workspace_service::workspaces::SetMemberRoleRequest {
                request_id: fresh_id(),
                user_public_key: bob_cert.user_public_key,
                role: Role::Editor,
            },
        )
        .await
        .expect("the owner makes Bob an editor again");
    bob_mount
        .until(
            "Bob's mount lets the editor write again",
            note,
            Access::Write,
            Ok(()),
        )
        .await;

    // Where Ada's folder stands is her daemon's own resource, read in her own workspace.
    let (person, device) = (ada_laptop.person, ada_laptop.device);
    let own_workspace = tilo_server_protocol::servers::person_workspace_id(&person);
    let me = Caller::authenticated(person, device);
    let shown = tilo_test_env::within("the folder's status is the daemon's resource", async {
        loop {
            let list = folders(&ada_laptop.own, &me, &own_workspace)
                .expect("the person reads their device's folders");
            if let Some(folder) = list
                .folders
                .into_iter()
                .find(|folder| folder.workspace == workspace)
            {
                return folder;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(shown.workspace, workspace);
    assert!(
        matches!(
            folders(&ada_laptop.own, &me, &workspace),
            Err(ActionCallError::Refused { .. })
        ),
        "the folders are read in the person's own workspace, not in another"
    );
    let (_, phone) = enrol(&ada, "Ada's phone", DeviceCapabilities::member());
    assert!(
        matches!(
            folders(
                &ada_laptop.own,
                &Caller::authenticated(person, phone.device_public_key),
                &own_workspace
            ),
            Err(ActionCallError::Refused { .. })
        ),
        "another device, even the person's own, does not read this device's folders"
    );
    assert!(
        matches!(
            folders(
                &ada_laptop.own,
                &Caller::local_agent("helper"),
                &own_workspace
            ),
            Err(ActionCallError::Refused { .. })
        ),
        "an agent does not read them"
    );
    assert!(
        matches!(
            folders(
                &bob_laptop.own,
                &Caller::authenticated(person, device),
                &own_workspace
            ),
            Err(ActionCallError::Refused { .. })
        ),
        "another person's daemon does not answer for Ada"
    );
}
