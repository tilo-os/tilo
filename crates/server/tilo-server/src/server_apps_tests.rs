use super::*;
use tilo_app_package::{Manifest, SigningIdentity, pack, tapp};

const APP: &str = "so.tilo.lang.fixture";
const WORKSPACE: &str = "6f1c2a52-8a2b-4c39-9d6b-1f3b0e0c7a10";
/// What the package's module holds: found again in the `.tapp` to tamper with it.
const MODULE: &[u8] = b"\0asm the fixture's compiled module";

/// A tilolang package `id` as `tilolang build` signs it: its entrypoint runs the tilolang runtime,
/// its payload is the module that runtime runs.
fn tilolang_package(id: &str, identity: &SigningIdentity) -> Vec<u8> {
    renamed_package(id, &[], identity)
}

/// [`tilolang_package`], published under the ids `previously` before.
fn renamed_package(id: &str, previously: &[&str], identity: &SigningIdentity) -> Vec<u8> {
    let scratch = tempfile::tempdir().unwrap();
    let payload = scratch.path().join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("app.wasm"), MODULE).unwrap();
    std::fs::write(payload.join("app.ids"), b"# fixture ids\n").unwrap();
    std::fs::write(
        payload.join(package_store::SURFACE_FILE),
        b"format = 2\n\n[[resources]]\nkind = \"fixture.page\"\nlabel = \"Fixture\"\nversion = 1\n",
    )
    .unwrap();
    let manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{id}"
previously = [{}]
kind = "app"
version = "0.1.0"
runtime = "tilo-runtime/2026.11"
optional-runtimes = ["tilolang/0.1.0+tilolang"]

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user", "pid", "mount"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
runtime = "tilolang/0.1.0+tilolang"
bin = "bin/tilolang-app"
type = "gui"
"#,
        previously
            .iter()
            .map(|old| format!("\"{old}\""))
            .collect::<Vec<_>>()
            .join(", "),
        hex::encode(identity.public_key_bytes()),
    ))
    .unwrap();
    let packed = pack(&payload, &manifest, identity).unwrap();
    let source: &dyn tilo_app_package::BlobSource = &packed;
    tapp::to_bytes(&packed.meta, Some(source)).unwrap()
}

/// A server's workspace packages kept in a fresh directory, and a publisher's key.
/// Sockets no service listens on.
#[cfg(feature = "agents")]
fn nowhere() -> Sockets {
    Sockets {
        collab: PathBuf::from("/nonexistent/collab.sock"),
        identity: PathBuf::from("/nonexistent/identity.sock"),
        content: PathBuf::from("/nonexistent/content.sock"),
    }
}

fn packages() -> (tempfile::TempDir, WorkspacePackages, SigningIdentity) {
    let root = tempfile::tempdir().unwrap();
    let packages = WorkspacePackages::new(root.path().to_path_buf());
    (root, packages, SigningIdentity::generate())
}

tilo_test_env::scenario! {
    fn a_workspace_on_a_server_runs_only_the_package_it_installed_as_verified;

    phase an_installed_package_is_verified_as_a_launch_verifies_it;
    {
        // The package the workspace installed runs as the tilolang app its signature vouches for,
        // its module read from the verified payload.
        let (root, packages, identity) = packages();
        let verified = packages
            .verified(WORKSPACE, APP, &tilolang_package(APP, &identity))
            .expect("the installed package verifies");
        assert_eq!(verified.runtime, "tilolang/0.1.0+tilolang");
        assert_eq!(verified.app_runtime, "tilo-runtime/2026.11");
        assert_eq!(verified.publisher, identity.public_key_bytes());
        assert_eq!(verified.module.wasm, MODULE);
        assert!(
            root.path().join(WORKSPACE).is_dir(),
            "the workspace's packages are kept in a store of its own"
        );
    }

    phase a_tampered_package_is_refused;
    {
        // A byte of the module changed after signing: the package no longer holds together, and
        // nothing of it runs.
        let (_root, packages, identity) = packages();
        let mut tampered = tilolang_package(APP, &identity);
        let at = tampered
            .windows(MODULE.len())
            .position(|window| window == MODULE)
            .expect("the package carries its module");
        tampered[at + 1] ^= 0x20;
        let refused = packages
            .verified(WORKSPACE, APP, &tampered)
            .expect_err("a tampered package is refused");
        assert!(refused.contains("was refused"), "{refused}");
    }

    phase a_package_that_is_not_the_installed_app_is_refused;
    {
        let (_root, packages, identity) = packages();
        let other = tilolang_package("so.tilo.lang.other", &identity);
        let refused = packages
            .verified(WORKSPACE, APP, &other)
            .expect_err("another app's package does not run as this one");
        assert!(refused.contains("not so.tilo.lang.fixture"), "{refused}");
    }

    phase a_workspace_is_named_by_its_id_only;
    {
        let (_root, packages, identity) = packages();
        let refused = packages
            .verified("../elsewhere", APP, &tilolang_package(APP, &identity))
            .expect_err("a path is no workspace");
        assert!(refused.contains("no workspace id"), "{refused}");
    }
}

tilo_test_env::scenario! {
    fn a_server_runs_apps_only_through_its_launcher;

    phase a_server_without_a_launcher_runs_no_app;
    {
        let root = tempfile::tempdir().unwrap();
        let missing = [root.path().join("tilo-launcher")];
        let refused = tilo_launcher(&missing).expect_err("no launcher, no app");
        assert!(refused.contains("no tilo-launcher is installed"), "{refused}");
        assert!(
            refused.contains(&missing[0].display().to_string()),
            "it says where it looked: {refused}"
        );
    }

    phase the_first_installed_launcher_is_the_one;
    {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("tilo-launcher");
        let installed = root.path().join("installed");
        std::fs::write(&installed, b"").unwrap();
        let candidates = [missing, installed.clone()];
        assert_eq!(tilo_launcher(&candidates).unwrap(), installed);
    }
}

tilo_test_env::scenario! {
    fn the_servers_apps_keep_content_in_its_one_store;

    phase an_app_reaches_the_store_through_the_launchers_scoped_connection;
    {
        // The launcher's own resolution connects an app in a workspace to the server's store, and
        // what the app keeps there reads back through that connection.
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("blobs.sock");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let _content = runtime
            .block_on(ContentService::start(&root.path().join("content"), &socket))
            .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(launcher::services::BLOB_SOCKET_ENV, &socket);
        let connection = launcher::services::resolve_blob_bind(APP, Some(WORKSPACE), false)
            .unwrap()
            .expect("the store is running");
        let app = tilo_fs_client::blobs::BlobClient::from_app_stream(
            std::os::unix::net::UnixStream::from(connection.host),
        )
        .unwrap();
        let kept = app.put(b"a picture the person chose", "image/png").unwrap();
        assert_eq!(
            app.read_all(&kept.hash).unwrap(),
            tilo_fs_client::blobs::ReadOutcome::Ready {
                bytes: b"a picture the person chose".to_vec(),
                total_len: 26,
            }
        );
    }
}

#[cfg(feature = "agents")]
tilo_test_env::scenario! {
    fn a_servers_launcher_starts_only_what_it_may;

    phase a_launch_with_no_page_to_show_it_starts_nothing;
    {
        use tilo_broker_core::services::{LaunchPlace, LauncherServices};
        let launcher = ServerLauncher::new(
            WORKSPACE.to_owned(),
            nowhere(),
            "server".to_owned(),
            Err("no launcher".to_owned()),
            std::sync::Arc::new(WorkspacePackages::new(PathBuf::from("/nonexistent/packages"))),
        );
        let workspace = tilo_desktop_model::Workspace::new(
            tilo_desktop_model::WorkspaceId::parse(WORKSPACE).unwrap(),
            "Team".to_owned(),
        );
        let refused = launcher
            .launch(APP, &workspace, None, &LaunchPlace::Focus)
            .unwrap_err();
        assert!(refused.contains(app_launcher::NO_WINDOW), "{refused}");
    }

    phase a_call_runs_only_in_its_workspace_and_only_through_the_launcher;
    {
        use tilo_broker_core::services::LauncherServices;
        let launcher = ServerLauncher::new(
            WORKSPACE.to_owned(),
            nowhere(),
            "server".to_owned(),
            Err("no tilo-launcher is installed on this server".to_owned()),
            std::sync::Arc::new(WorkspacePackages::new(PathBuf::from("/nonexistent/packages"))),
        );
        let other = "0b1c2a52-8a2b-4c39-9d6b-1f3b0e0c7a10";
        let elsewhere = launcher.windowless(APP, other).unwrap_err();
        assert!(elsewhere.contains("runs here only in workspace"), "{elsewhere}");
        let unconfined = launcher.windowless(APP, WORKSPACE).unwrap_err();
        assert_eq!(unconfined, "no tilo-launcher is installed on this server");
    }

    phase a_connection_this_process_did_not_make_is_no_app;
    {
        use tilo_broker_core::services::LauncherServices;
        let launcher = ServerLauncher::new(
            WORKSPACE.to_owned(),
            nowhere(),
            "server".to_owned(),
            Err("no launcher".to_owned()),
            std::sync::Arc::new(WorkspacePackages::new(PathBuf::from("/nonexistent/packages"))),
        );
        assert!(!launcher.attest_process(std::process::id(), APP, WORKSPACE));
        assert!(!launcher.attest_process(1, APP, WORKSPACE));
        assert!(
            launcher
                .authenticated_launch(std::process::id(), APP, WORKSPACE, Some(1))
                .unwrap()
                .is_none()
        );
    }
}

/// A server runs one broker for each workspace it keeps, from when it finds the workspace until
/// the workspace goes, each on its own socket.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_serves_each_workspace_it_keeps_on_its_own_socket() {
    use std::collections::{BTreeMap, BTreeSet};
    use tilo_desktop_model::WorkspaceId;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = crate::workspaces::fixtures::governor();
    let host =
        crate::workspaces::fixtures::test_host(&dir.path().join("server"), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    let id = |summary: &crate::workspaces::WorkspaceSummary| {
        WorkspaceId::parse(&summary.workspace).unwrap()
    };
    let team = id(&workspaces.create("Team").await.unwrap());
    let other = id(&workspaces.create("Other").await.unwrap());
    let apps = ServerApps::new(
        ServerWorkspaces::new(workspaces.clone(), tokio::runtime::Handle::current()),
        dir.path().join("sockets"),
        dir.path().join("journals"),
        cert.device_public_key,
        Err("no tilo-launcher is installed on this server".to_owned()),
        Shipped {
            packages: Vec::new(),
            kept: std::sync::Arc::new(WorkspacePackages::new(dir.path().join("packages"))),
        },
    );
    assert_eq!(apps.reconcile().await, BTreeMap::new());
    assert_eq!(apps.workspaces(), BTreeSet::from([team, other]));
    for workspace in [team, other] {
        assert!(
            apps.socket(workspace).exists(),
            "{workspace} has its broker"
        );
    }
    // Finding the same workspaces again starts nothing twice.
    assert_eq!(apps.reconcile().await, BTreeMap::new());
    assert_eq!(apps.workspaces(), BTreeSet::from([team, other]));

    workspaces.delete(&team.to_string()).await.unwrap();
    assert_eq!(apps.reconcile().await, BTreeMap::new());
    assert_eq!(apps.workspaces(), BTreeSet::from([other]));
}

/// The system's apps a server ships are installed in every workspace it serves, the
/// one it was started for or not, once: a later start installs nothing again.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_installs_the_apps_it_ships_in_every_workspace_it_serves() {
    use tilo_desktop_model::WorkspaceId;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = crate::workspaces::fixtures::governor();
    let host =
        crate::workspaces::fixtures::test_host(&dir.path().join("server"), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    let team = workspaces.create("Team").await.unwrap().workspace;
    let identity = SigningIdentity::generate();
    let shipped = vec![tilolang_package(APP, &identity)];
    let start = || {
        ServerApps::new(
            ServerWorkspaces::new(workspaces.clone(), tokio::runtime::Handle::current()),
            dir.path().join("sockets"),
            dir.path().join("journals"),
            cert.device_public_key,
            Err("no tilo-launcher is installed on this server".to_owned()),
            Shipped {
                packages: shipped.clone(),
                kept: std::sync::Arc::new(WorkspacePackages::new(dir.path().join("packages"))),
            },
        )
    };
    let runs = |workspace: String| {
        let workspaces = workspaces.clone();
        async move {
            workspaces
                .app_versions(&workspace)
                .await
                .unwrap()
                .into_iter()
                .map(|installed| installed.release.app)
                .collect::<Vec<_>>()
        }
    };
    let apps = start();
    apps.reconcile().await;
    assert_eq!(runs(team.clone()).await, [APP]);
    // A workspace the running server starts serving gets them too.
    let later = workspaces.create("Later").await.unwrap().workspace;
    apps.reconcile().await;
    assert_eq!(runs(later.clone()).await, [APP]);
    assert!(
        apps.workspaces()
            .contains(&WorkspaceId::parse(&later).unwrap())
    );
    drop(apps);
    // The next start finds them installed, and installs nothing again.
    let mut changes = workspaces.governance_changes();
    let again = start();
    again.reconcile().await;
    assert_eq!(runs(team).await, [APP]);
    assert!(
        matches!(
            changes.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "a start that finds the system's apps installed changes nothing"
    );
}

/// An app the server ships keeps the records of the id its publisher published it under before,
/// as an app an image ships does on a device; an id a package of a workspace's own claims is
/// believed only as the store checked it.
#[cfg(feature = "agents")]
#[test]
fn a_shipped_apps_earlier_id_is_its_own() {
    use tilo_broker_core::services::LauncherServices;
    let dir = tempfile::tempdir().unwrap();
    let identity = SigningIdentity::generate();
    let notes = renamed_package("so.tilo.notes", &["so.tilo.lang.notes"], &identity);
    let packages = WorkspacePackages::new(dir.path().join("packages"))
        .shipping(&dir.path().join("shipped"), &[notes])
        .unwrap();
    assert_eq!(
        packages.earlier_ids(WORKSPACE).of("so.tilo.notes"),
        ["so.tilo.lang.notes"].map(str::to_owned)
    );
    // A package only the workspace installed names an id nothing here was installed under.
    let claim = renamed_package(APP, &["so.tilo.lang.someone"], &SigningIdentity::generate());
    packages.verified(WORKSPACE, APP, &claim).unwrap();
    assert!(packages.earlier_ids(WORKSPACE).of(APP).is_empty());
    let launcher = ServerLauncher::new(
        WORKSPACE.to_owned(),
        nowhere(),
        "server".to_owned(),
        Err("no launcher".to_owned()),
        std::sync::Arc::new(packages),
    );
    assert_eq!(
        launcher.earlier_ids().of("so.tilo.notes"),
        ["so.tilo.lang.notes"].map(str::to_owned)
    );
}

/// A page asking the server's identity service for its keys, on the connection it was opened
/// with: decided as whoever its call names. Whether the keys were handed.
#[cfg(feature = "agents")]
fn page_keys(socket: &std::path::Path, workspace: &str, call: [u8; 16]) -> bool {
    use tilo_identity_client::wire::{
        FRAMING, HANDSHAKE_FRAMING, Handshake, PresentationHandshake, Request, Response,
    };
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    let handshake = serde_json::to_vec(&PresentationHandshake {
        handshake: Handshake {
            workspace_id: workspace.to_owned(),
            app_id: "so.tilo.notes".to_owned(),
            surface: None,
            call: Some(call),
        },
        presentation_version: 0,
    })
    .unwrap();
    HANDSHAKE_FRAMING.write(&mut stream, &handshake).unwrap();
    let ask = Request::GroupKeys {
        workspace_id: workspace.to_owned(),
        app_id: "so.tilo.notes".to_owned(),
        document_id: String::new(),
        known_generation: 0,
    };
    FRAMING.write_value(&mut stream, &ask).unwrap();
    match FRAMING.read_value(&mut stream).unwrap().unwrap() {
        Response::GroupKeys { .. } => true,
        Response::Denied(_) => false,
        other => panic!("the service answered {other:?}"),
    }
}

/// A person `workspace` admits by an invitation its owner `owner` made, as their device.
#[cfg(feature = "agents")]
async fn admitted_reader(
    workspaces: &crate::workspaces::Workspaces,
    owner: &tilo_user_identity::DeviceCert,
    workspace: &str,
) -> tilo_user_identity::DeviceCert {
    let token = [3; 8];
    let issued = workspaces
        .pre_admit(
            owner,
            crate::workspaces::PreAdmitRequest {
                request_id: [3; 32],
                grant: crate::workspaces::fixtures::signed_pre_admission(workspace, &token),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let (_, reader) = crate::workspaces::fixtures::device_cert(
        &ed25519_dalek::SigningKey::from_bytes(&[3; 32]),
        "reader",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let admitted = workspaces
        .admit_proven(
            issued.signed_invitation,
            reader.clone(),
            rand::random(),
            "Reader",
            Some(token.as_slice()),
        )
        .await;
    assert!(
        matches!(
            admitted,
            tilo_server_protocol::workspaces::JoinReply::Admitted { .. }
        ),
        "{admitted:?}"
    );
    reader
}

#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_is_handed_keys_only_while_a_reader_who_may_read_it_reads() {
    use tilo_workspace_service::apps::caller_signer::{CallerSigner, Callers, VerifiedCaller};
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = crate::workspaces::fixtures::governor();
    let host =
        crate::workspaces::fixtures::test_host(&dir.path().join("server"), &key, &owner).await;
    let workspaces = host.workspaces().clone();
    let summary = workspaces.create("Team").await.unwrap();
    let (workspace, id) = (summary.workspace.clone(), summary.workspace_id);
    // A person the workspace admits, who may read what it keeps.
    let reader = admitted_reader(&workspaces, &owner, &workspace).await;
    let (_, outsider) = crate::workspaces::fixtures::device_cert(
        &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
        "outsider",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let verified = |cert: &tilo_user_identity::DeviceCert| VerifiedCaller {
        device_public_key: cert.device_public_key,
        user_public_key: cert.user_public_key,
    };

    let paths = tilo_device_identity::persist::IdentityPaths::under(dir.path().join("identity"));
    std::fs::create_dir_all(paths.device_privkey.parent().unwrap()).unwrap();
    tilo_device_identity::persist::write_device_key(&paths.device_privkey, &key).unwrap();
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &owner).unwrap();
    let readers = std::sync::Arc::new(Readers::default());
    let runtime = tokio::runtime::Handle::current();
    let socket = dir.path().join("identity.sock");
    let _identity = tilo_identity::broker::spawn_signing(
        socket.clone(),
        std::sync::Arc::new(ServerWorkspaces::new(workspaces.clone(), runtime.clone())),
        // The server's own key service, as it runs beside its workspaces.
        std::sync::Arc::new(
            crate::workspace_keys::open(&dir.path().join("keys"), key.clone())
                .unwrap()
                .keys(),
        ),
        std::sync::Arc::new(()),
        std::sync::Arc::new(CallerSigner::new(
            workspaces.clone(),
            runtime,
            paths,
            owner.user_public_key,
            std::sync::Arc::clone(&readers) as std::sync::Arc<dyn Callers>,
        )),
    )
    .unwrap();
    let keys = |call: [u8; 16]| {
        let (socket, workspace) = (socket.clone(), workspace.clone());
        async move {
            tokio::task::spawn_blocking(move || page_keys(&socket, &workspace, call))
                .await
                .unwrap()
        }
    };

    // A reader the workspace lets read is handed the route's keys while they read.
    let reading = readers.reading(id, "so.tilo.notes", verified(&reader));
    assert!(keys(reading.call()).await);
    // Someone the workspace does not have is handed none, page open or not.
    let outside = readers.reading(id, "so.tilo.notes", verified(&outsider));
    assert!(!keys(outside.call()).await);
    // A page of another app names nobody here.
    let elsewhere = readers.reading(id, "so.tilo.mail", verified(&reader));
    assert!(!keys(elsewhere.call()).await);
    // Once the last reader has left, the page's call names nobody: nothing more is handed.
    let left = reading.call();
    drop(reading);
    assert!(!keys(left).await);

    // A reader the workspace removes while they read is refused on the page's next ask.
    let reading = readers.reading(id, "so.tilo.notes", verified(&reader));
    assert!(keys(reading.call()).await);
    workspaces
        .remove_member(
            &workspace,
            &owner,
            tilo_server_session::workspaces::RemoveMemberRequest {
                request_id: [41; 32],
                user_public_key: reader.user_public_key,
            },
        )
        .await
        .unwrap();
    assert!(!keys(reading.call()).await);
}

/// A page of an app is shown only to someone the workspace lets read it: an outsider is refused
/// as one, and a member the workspace keeps from the app is told no such app is served, with
/// nothing of it shown. A member who may read it gets as far as the app's process.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_shows_nothing_to_someone_who_may_not_read_its_app() {
    use crate::app_serving::HostedPages as _;
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    use tilo_server_protocol::page::PageRefusal;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = crate::workspaces::fixtures::governor();
    let host =
        crate::workspaces::fixtures::test_host(&dir.path().join("server"), &key, &owner).await;
    let workspaces = host.workspaces().clone();
    let summary = workspaces.create("Team").await.unwrap();
    let (workspace, id) = (summary.workspace.clone(), summary.workspace_id);
    let identity = SigningIdentity::generate();
    workspaces
        .install_package(&workspace, &owner, &tilolang_package(APP, &identity))
        .await
        .unwrap();
    let reader = admitted_reader(&workspaces, &owner, &workspace).await;
    let (_, outsider) = crate::workspaces::fixtures::device_cert(
        &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
        "outsider",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let apps = ServerApps::new(
        ServerWorkspaces::new(workspaces.clone(), tokio::runtime::Handle::current()),
        dir.path().join("sockets"),
        dir.path().join("journals"),
        owner.device_public_key,
        Err("no tilo-launcher is installed on this server".to_owned()),
        Shipped {
            packages: Vec::new(),
            kept: std::sync::Arc::new(WorkspacePackages::new(dir.path().join("packages"))),
        },
    );
    apps.reconcile().await;
    let (_root, packages, _) = packages();
    let pages = ServerPages::new(
        apps,
        std::sync::Arc::new(packages),
        std::sync::Arc::new(Readers::default()),
    );
    assert!(
        pages.apps(id).iter().any(|(app, _)| app == APP),
        "the installed app is offered"
    );
    let open = |cert: &tilo_user_identity::DeviceCert| {
        let caller = tilo_actions::remote::Caller::authenticated(
            cert.user_public_key,
            cert.device_public_key,
        );
        let opened = pages.open(id, APP, &caller, None);
        async move {
            tokio::task::spawn_blocking(move || {
                opened
                    .wait_timeout(std::time::Duration::from_secs(30))
                    .unwrap_or_else(|_| panic!("the page is answered in time"))
                    .expect("the page is answered")
                    .map(|opened| opened.body)
            })
            .await
            .unwrap()
        }
    };

    assert_eq!(open(&outsider).await, Err(PageRefusal::NotMember));
    // The member may read it: the page gets as far as the process that would show it, which
    // this server cannot start.
    assert!(
        matches!(open(&reader).await, Err(PageRefusal::Unavailable(why)) if why.contains("tilo-launcher")),
        "a reader reaches the app's process"
    );
    let who = hex::encode(reader.user_public_key);
    workspaces
        .put_workspace_grants(
            &workspace,
            &owner,
            [5; 32],
            vec![Grant::new(
                format!("keep-out:{who}"),
                Grantee::person(who.clone()),
                TreePath::workspace(&workspace).app(APP),
                KindSet::READ,
                Answer::Deny,
                Grantee::person(hex::encode(owner.user_public_key)),
            )],
        )
        .await
        .unwrap();
    assert_eq!(
        open(&reader).await,
        Err(PageRefusal::NoSuchApp),
        "a member kept from the app is shown none of it"
    );
}

/// A runtime directory as short as a login's (`/run/user/<uid>`), where a test's own scratch
/// directory may lie too deep for a socket.
#[cfg(feature = "agents")]
fn short_runtime_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("run")
        .tempdir_in("/tmp")
        .unwrap()
}

/// The apps of a server whose state lies deep listen where a run reaches them, and a carry that
/// failed when the package was installed runs when the server starts again: once, through the
/// sockets, the identity service and the process start a real carry goes through.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one carry through an install, a restart and the run that ends it"
)]
async fn a_carry_that_failed_at_its_install_runs_at_the_next_start() {
    use tilo_peer_state::workspace_documents::{DeviceWrites as _, DocumentWriting};
    let scratch = tempfile::tempdir().unwrap();
    let runtime = short_runtime_dir();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    env.set("XDG_DATA_HOME", scratch.path().join("data"));
    // As deep as a deployment's volume may put it: far deeper than a socket's path may be.
    let state_root = scratch.path().join("state".repeat(32));
    std::fs::create_dir_all(&state_root).unwrap();
    assert!(state_root.as_os_str().len() > 150);
    let (key, cert) = crate::workspaces::fixtures::governor();
    let host =
        crate::workspaces::fixtures::test_host(&state_root.join("server"), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    let team = workspaces.create("Team").await.unwrap().workspace;

    // What the native app of the package's id kept in the workspace.
    let record = "uuid:0b6f2a1e-4c2d-4e8a-9f1b-6d3c2e1a0f4b";
    let sealed = workspaces
        .own_route_keys(
            &tilo_peer_state::document_frame::DocumentRoute {
                app_id: APP.into(),
                cluster_id: team.clone(),
                document: record.into(),
            },
            true,
        )
        .await
        .and_then(Result::ok);
    let mut kept = automerge::AutoCommit::new();
    automerge::transaction::Transactable::put(&mut kept, automerge::ROOT, "title", "Kept").unwrap();
    tilo_workspace_service::device_documents::ServiceWrites::of(workspaces.clone())
        .write(
            &team,
            &kept.save(),
            DocumentWriting {
                app: APP,
                document: record,
                readers: None,
                founding: true,
                snapshot: true,
                sealed: sealed.as_ref(),
            },
        )
        .await
        .expect("the native app's record is kept");

    let shipped = scratch.path().join("shipped");
    std::fs::create_dir_all(&shipped).unwrap();
    std::fs::write(
        shipped.join("fixture.tapp"),
        tilolang_package(APP, &SigningIdentity::generate()),
    )
    .unwrap();
    // Each process the server starts for the package: started as the launcher would start it.
    let starting = |started: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
                    program: &'static str| {
        let started = std::sync::Arc::clone(started);
        let starts: Box<tilo_app_launch::tilolang_processes::Starts> =
            Box::new(move |_, _, _, _| {
                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::process::Command::new(program)
            });
        starts
    };
    // A test's process outlives the server it stops, and that server's content store stays
    // open in it: the next start keeps its own apps' state beside the first's, over the same
    // workspaces.
    let again_root = scratch.path().join("again".repeat(32));
    let start = |root: PathBuf, starts: Box<tilo_app_launch::tilolang_processes::Starts>| {
        let (workspaces, cert, shipped) = (workspaces.clone(), cert.clone(), shipped.clone());
        let identity_root = scratch.path().join("identity");
        async move {
            start_through(
                workspaces,
                Device {
                    identity_root,
                    cert,
                    keys: None,
                },
                (&root, Some(&shipped)),
                None,
                starts,
            )
            .await
        }
    };

    // The first start installs the package, and its carry fails: its process cannot start.
    let failed = std::sync::Arc::default();
    let first = start(
        state_root.clone(),
        starting(&failed, "/nonexistent/tilo-launcher"),
    )
    .await
    .expect("the apps start");
    first.apps.reconcile().await;
    drop(first);
    assert_eq!(
        failed.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the carry was tried"
    );

    // The next start carries it, and only once while it runs.
    let carried = std::sync::Arc::default();
    let again = start(again_root, starting(&carried, "true"))
        .await
        .expect("the apps start again");
    again.apps.reconcile().await;
    again.apps.reconcile().await;
    assert_eq!(
        carried.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the carry ran once"
    );
    drop(again);
}

/// A server's apps listen in its login's runtime directory, never under its state; a login with
/// none, or one too deep for a socket in it, starts no app.
#[cfg(feature = "agents")]
#[test]
fn a_server_without_a_runtime_directory_starts_no_app() {
    let scratch = tempfile::tempdir().unwrap();
    let state = scratch.path().join("state");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_RUNTIME_DIR", scratch.path().join("gone"));
    let refused = app_sockets(&state).expect_err("no runtime directory, no sockets");
    assert!(refused.contains("set XDG_RUNTIME_DIR"), "{refused}");
    let deep = scratch.path().join("run".repeat(40));
    std::fs::create_dir_all(&deep).unwrap();
    env.set("XDG_RUNTIME_DIR", &deep);
    let refused = app_sockets(&state).expect_err("too deep for a socket");
    assert!(refused.contains("too deep"), "{refused}");
    let runtime = short_runtime_dir();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let sockets = app_sockets(&state).unwrap();
    assert!(sockets.starts_with(runtime.path()) && !sockets.starts_with(&state));
    assert_ne!(
        sockets,
        app_sockets(&scratch.path().join("other")).unwrap(),
        "two servers keep apart"
    );
}
