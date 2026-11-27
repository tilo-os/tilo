//! A keeping server's web listener answers the app pages its daemon serves.
//!
//! A machine configured as a server (`TILO_SERVER_CONFIG`), signed in by a ticket from a device
//! of its person, keeps their workspaces and serves its own web UI. A browser that opens
//! `/w/<workspace>/<app>` there reaches the daemon's pages (`Daemon::pages`, what the page
//! session calls after the browser's device proves itself), and the app's own window lays the
//! view out, as on a computer that founded its person.

#![cfg(target_os = "linux")]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::Pending;
use tilo_runtime::RuntimeServices as _;
use tilo_runtime::device::services::Daemon;
use tilo_server_protocol::page::PageAddress;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use crate::language::host;
use crate::language::host::button_labelled;

/// Names the test the process [`run`] starts runs, and where its profile is.
const OWN: &str = "TILO_KEEPING_TEST";
const PROFILE: &str = "TILO_KEEPING_TEST_PROFILE";

/// Run `body` against a keeping server of its own, in a process of its own.
///
/// # Panics
///
/// When the server does not start, or `body` fails.
fn run<F, Fut>(apps: &[&str], body: F)
where
    F: FnOnce(Arc<Keeping>) -> Fut,
    Fut: Future<Output = ()>,
{
    let test = std::thread::current()
        .name()
        .expect("libtest names a test's thread after the test")
        .to_owned();
    if std::env::var(OWN).ok().as_deref() != Some(test.as_str()) {
        own_process(&test, apps);
        return;
    }
    host::say_what_went_wrong();
    let profile = PathBuf::from(std::env::var_os(PROFILE).expect("the profile is named"));
    let keeping = Arc::new(Keeping::start(&profile));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(body(Arc::clone(&keeping)));
    drop(runtime);
    drop(keeping);
}

/// Start this test again, alone, in a process whose home, identity and profile are a new
/// directory's, with the machine's server configuration naming a keeping server.
fn own_process(test: &str, apps: &[&str]) {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::Builder::new()
        .prefix("tilo-keep-")
        .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let place = |name: &str| {
        let dir = root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    let (config, data) = (place("c"), place("d"));
    let (relay, relay_url) = test_server::loopback_relay();
    let installed = host::apps::install(&place("a"), &data, apps);
    let server = keep_config(root.path());
    let log = root.path().join("out");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .envs(installed)
        .args(["--exact", test, "--nocapture", "--test-threads", "1"])
        .env(OWN, test)
        .env(PROFILE, place("p"))
        .env("HOME", place("h"))
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CACHE_HOME", place("k"))
        .env("XDG_STATE_HOME", place("s"))
        .env("TILO_IDENTITY_ROOT", place("i"))
        .env(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
        .env(relay, relay_url)
        .env_remove(runtime_bundle::RUNTIME_DIR_ENV)
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env("TILO_SERVER_CONFIG", server)
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(Stdio::inherit())
        .status_locked()
        .unwrap();
    let out = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        status.success() && out.contains("1 passed"),
        "{test} on its own keeping server: {status}\n{out}"
    );
}

/// The configuration of a machine that keeps its person's workspaces, with a loopback endpoint
/// and a browser listener on a port the machine picks.
fn keep_config(root: &Path) -> PathBuf {
    let state = root.join("server");
    std::fs::create_dir_all(&state).unwrap();
    let config = root.join("server.toml");
    std::fs::write(
        &config,
        format!(
            "state_root = {:?}\niroh_listen = \"127.0.0.1:0\"\nweb_listen = \"127.0.0.1:0\"\n",
            state.display().to_string()
        ),
    )
    .unwrap();
    config
}

/// The keeping server a test runs against.
struct Keeping {
    daemon: Arc<Daemon>,
    /// The person's browser, as its root certified it: what a page's link verifies.
    browser: DeviceCert,
}

impl Keeping {
    /// Sign this machine in with a member certificate its person's root signed, as a ticket
    /// leaves one, and start the daemon: it keeps the person's workspaces and serves its web UI,
    /// which its browser listener reaches its pages through.
    fn start(profile: &Path) -> Self {
        let root = SigningKey::from_bytes(&[0x5c; 32]);
        let key = SigningKey::from_bytes(&[0x5d; 32]);
        let now = tilo_clock::unix_now_ns();
        let certified = |key: &SigningKey, label: &str| {
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: key.verifying_key().to_bytes(),
                device_label: label.into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: now.saturating_sub(60_000_000_000),
                expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
            cert
        };
        let cert = certified(&key, "the-server");
        let browser = certified(&SigningKey::from_bytes(&[0x5e; 32]), "Ada's browser");
        let place =
            tilo_device_identity::login::Place::here(tilo_api::calls::device_keys()).unwrap();
        tilo_device_identity::persist::adopt_device_key(place.keys(), &key.to_bytes()).unwrap();
        tilo_device_identity::login::file_certificate(&place, &cert).unwrap();
        tilo_api::daemon::enter::adopt(&place, &cert).unwrap();
        let daemon = Arc::new(Daemon::start_reporting(profile, &mut |_| {}).unwrap());
        daemon.hand_apps_their_lines();
        Self { daemon, browser }
    }
}

/// What a page answers, waited on: the page session waits as long before it says the page is
/// unavailable.
fn settled<T>(answer: Pending<T>) -> T {
    answer
        .wait_timeout(std::time::Duration::from_secs(60))
        .ok()
        .flatten()
        .expect("the page answers")
}

/// A browser at a keeping server's `/w/<workspace>/calculator` reaches the daemon's page of the
/// app: the calculator's view, laid out by its own window, and a press on one of its keys comes
/// back as the display that changed.
#[test]
fn a_keeping_server_answers_a_page_of_a_tl_app_it_serves() {
    run(&["calculator"], |keeping| async move {
        // A workspace the server keeps, as one its person's device places on it.
        let workspace = keeping
            .daemon
            .workspaces()
            .create("Work")
            .await
            .unwrap()
            .workspace;
        let workspace_id = tilo_desktop_model::WorkspaceId::parse(&workspace).unwrap();
        let cert = keeping.browser.clone();
        let caller =
            tilo_workspace_service::service::local_caller(&cert).in_workspace(workspace.as_str());
        let daemon = Arc::clone(&keeping.daemon);
        let (opened, pressed) = tokio::task::spawn_blocking(move || {
            let pages = daemon
                .pages()
                .expect("a keeping server serves the pages of the workspaces it keeps");
            let opened = settled(pages.open(
                workspace_id,
                (&caller, &cert),
                PageAddress {
                    workspace: workspace.clone(),
                    app: "calculator".to_owned(),
                    show: None,
                },
            ))
            .expect("the calculator's page opens");
            let seven = button_labelled(&opened.body, "7")
                .unwrap_or_else(|| panic!("the page shows the calculator's keys: {}", opened.body));
            let pressed = settled(pages.act(
                workspace_id,
                opened.page,
                Some(tilo_workspace_service::app_serving::PageAct {
                    id: seven,
                    target: None,
                    action: tilo_server_protocol::page::PageAction::Activate,
                    signed: None,
                }),
            ))
            .expect("a press on the page comes back as what changed");
            (opened, pressed)
        })
        .await
        .unwrap();
        assert!(
            opened.body.contains("<button"),
            "the app's own window laid its view out: {}",
            opened.body
        );
        assert!(
            !pressed.is_empty(),
            "pressing 7 changed the display: {pressed:?}"
        );
    });
}
