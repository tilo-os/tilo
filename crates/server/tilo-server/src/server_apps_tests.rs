use super::*;
use tilo_app_package::{Manifest, SigningIdentity, pack, tapp};

const APP: &str = "so.tilo.lang.fixture";
const WORKSPACE: &str = "6f1c2a52-8a2b-4c39-9d6b-1f3b0e0c7a10";
/// What the package's module holds: found again in the `.tapp` to tamper with it.
const MODULE: &[u8] = b"\0asm the fixture's compiled module";

/// A tilolang package `id` as `tilolang build` signs it: its entrypoint runs the tilolang runtime,
/// its payload is the module that runtime runs.
fn tilolang_package(id: &str, identity: &SigningIdentity) -> Vec<u8> {
    let scratch = tempfile::tempdir().unwrap();
    let payload = scratch.path().join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("app.wasm"), MODULE).unwrap();
    std::fs::write(payload.join("app.ids"), b"# fixture ids\n").unwrap();
    let manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{id}"
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
        hex::encode(identity.public_key_bytes()),
    ))
    .unwrap();
    let packed = pack(&payload, &manifest, identity).unwrap();
    let source: &dyn tilo_app_package::BlobSource = &packed;
    tapp::to_bytes(&packed.meta, Some(source)).unwrap()
}

/// A server's workspace packages kept in a fresh directory, and a publisher's key.
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
            PathBuf::from("/nonexistent/collab.sock"),
            "server".to_owned(),
            Err("no launcher".to_owned()),
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

    phase a_call_runs_only_in_its_own_workspace_and_only_through_the_launcher;
    {
        use tilo_broker_core::services::LauncherServices;
        let launcher = ServerLauncher::new(
            WORKSPACE.to_owned(),
            PathBuf::from("/nonexistent/collab.sock"),
            "server".to_owned(),
            Err("no tilo-launcher is installed on this server".to_owned()),
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
            PathBuf::from("/nonexistent/collab.sock"),
            "server".to_owned(),
            Err("no launcher".to_owned()),
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
#[expect(
    clippy::significant_drop_tightening,
    reason = "the host serves its workspaces until the test ends"
)]
async fn a_server_runs_one_broker_for_each_workspace_while_it_keeps_it() {
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
