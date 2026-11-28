//! A device's listing of a workspace's apps: the packages the workspace's host installed into it,
//! which this device's follower fetched into its own cache, each by the name its manifest gives.
//! A phone that joins a workspace running tilolang Notes, with nothing installed of its own,
//! lists Notes by name and nothing else.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer as _, SigningKey};
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};
use tilo_device_identity::login::{Approval, Approved, Approver, Asked, Entry, Ticket};
use tilo_peer_state::new_device::Keeping;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use crate::in_process::Device;

use tilo_peer_state::fixture::labelled_with;
/// The workspace the person's server keeps for this test.
const WORKSPACE_NAME: &str = "Studio";

/// Ask `read` until it answers `Ok`, up to a minute: what it answered last is the failure.
fn until<T>(what: &str, mut read: impl FnMut() -> Result<T, String>) -> T {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(60))
        .expect("a minute from now is on the clock");
    loop {
        match read() {
            Ok(value) => return value,
            Err(last) if Instant::now() > deadline => panic!("{what}: {last}"),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// A key made from `seed`, so each run's devices are the same devices.
fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// `key`'s certificate, signed by `root`.
fn certified(root: &SigningKey, key: &SigningKey, label: &str, primary: bool) -> DeviceCert {
    labelled_with(
        root,
        key,
        label,
        if primary {
            DeviceCapabilities::primary()
        } else {
            DeviceCapabilities::member()
        },
        tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
        None,
    )
}

/// An endpoint of `key`'s on this machine's loopback.
fn endpoint(runtime: &tokio::runtime::Handle, key: &SigningKey) -> Endpoint {
    let _entered = runtime.enter();
    runtime.block_on(async {
        Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .secret_key(SecretKey::from_bytes(&key.to_bytes()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap()
    })
}

/// The person's server: a host on this machine's loopback keeping the workspace, and the runtime
/// the laptop that prints the phone's ticket runs on.
struct Server {
    runtime: tokio::runtime::Runtime,
    host: test_server::Host,
    person: SigningKey,
    workspace: String,
}

impl Server {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("app-listing-server")
            .enable_all()
            .build()
            .unwrap();
        let person = key(7);
        // A site, as a person's own server is: it keeps their own workspace too, where the device
        // that issues a ticket lists the computer it adds, and which every workspace of theirs
        // reads to admit that computer.
        let host = test_server::Host::site(&person, WORKSPACE_NAME);
        runtime
            .block_on(tilo_workspace_service::workspaces::person_workspace::found(
                host.workspaces(),
            ))
            .expect("the site keeps the person's own workspace");
        let workspace = host.workspace().to_owned();
        Self {
            runtime,
            host,
            person,
            workspace,
        }
    }

    /// Where a device the person's laptop lets in enters: this host's workspace, at this host.
    fn entry(&self) -> Entry {
        self.entry_of(&self.workspace, WORKSPACE_NAME)
    }

    /// Where a device the person's laptop lets in enters `workspace`, named `name`: at this host.
    fn entry_of(&self, workspace: &str, name: &str) -> Entry {
        Entry {
            workspace: workspace.to_owned(),
            workspace_name: Some(name.into()),
            server_cert: self.host.host_hint().host,
            server: tilo_api::daemon::enter::reached_of(&self.host.addr()),
            site: "https://tilo.test/".into(),
            host_origin: "https://server.tilo.test".into(),
        }
    }

    /// The relay this host answers through, which the phone's daemon is told to answer through
    /// too: every device of the flow is found there, on this machine's loopback.
    fn relay_url(&self) -> String {
        self.host
            .addr()
            .relay_urls()
            .next()
            .expect("the host names its packet relay")
            .to_string()
    }

    /// Install the app `tapp` holds in the workspace, as `tilo-app push` does: its file kept by
    /// the workspace's host, and the workspace's record naming it as the version every member
    /// runs.
    fn install(&self, tapp: &Path) {
        use tilo_actions::remote::ActionProvider as _;
        let bytes = std::fs::read(tapp).unwrap();
        let read = tilo_app_package::tapp::read(
            std::io::BufReader::new(bytes.as_slice()),
            &mut tilo_app_package::blobs::DiscardBlobs,
        )
        .unwrap();
        let manifest = read.meta.manifest.package.clone();
        let workspaces = self.host.workspaces();
        let file = workspaces.keep_app_file(&self.workspace, &bytes).unwrap();
        let release = serde_json::json!({
            "app": manifest.id,
            "version": manifest.version,
            "package": hex::encode(read.meta.id.0),
            "publisher": hex::encode(read.meta.root.publisher_identity),
            "file": file,
        });
        let host = &self.host.host_hint().host;
        let caller = tilo_actions::remote::Caller::authenticated(
            host.user_public_key,
            host.device_public_key,
        )
        .in_workspace(&self.workspace);
        let request = tilo_workspace_service::workspaces::fixtures::request(
            &self.workspace,
            "install-app",
            release,
        );
        let receipt = self
            .runtime
            .block_on(workspaces.device_actions().invoke(&caller, request))
            .expect("the install is answered")
            .unwrap_or_else(|error| panic!("the workspace takes {}: {error}", manifest.id));
        assert!(
            matches!(
                receipt.status,
                tilo_actions::remote::ReceiptStatus::Completed { .. }
            ),
            "the workspace runs {}: {receipt:?}",
            manifest.id
        );
    }
}

/// The person's laptop, serving one ticket: a device that logs in with it is certified by the
/// person's root and enters the server's workspace.
struct Issuing {
    ticket: Ticket,
    endpoint: Endpoint,
    router: Router,
    runtime: tokio::runtime::Handle,
}

impl Issuing {
    fn start(
        runtime: &tokio::runtime::Handle,
        person: &SigningKey,
        entry: Entry,
        keeps: Arc<dyn Keeping>,
    ) -> Self {
        let laptop = key(8);
        let cert = certified(person, &laptop, "the person's laptop", true);
        let endpoint = endpoint(runtime, &laptop);
        let ticket = Ticket::new(tilo_api::daemon::enter::reached_of(&endpoint.addr()));
        let router = {
            let _entered = runtime.enter();
            serving(
                &endpoint,
                &laptop,
                ticket.clone(),
                person.verifying_key().to_bytes(),
                entry,
                Arc::new(RootSigns {
                    root: person.clone(),
                    device: laptop.clone(),
                    cert,
                }),
                keeps,
            )
        };
        Self {
            ticket,
            endpoint,
            router,
            runtime: runtime.clone(),
        }
    }

    /// The ticket, as the laptop shows it to the person.
    fn ticket(&self) -> String {
        self.ticket.encode()
    }
}

impl Drop for Issuing {
    fn drop(&mut self) {
        let router = self.router.clone();
        let endpoint = self.endpoint.clone();
        drop(self.runtime.spawn(async move {
            let _ = router.shutdown().await;
            endpoint.close().await;
        }));
    }
}

/// The site's record of the person's devices, where the computer a ticket certifies is listed as
/// the daemon that issues a ticket lists it.
struct SiteKeeps(tilo_workspace_service::workspaces::Workspaces);

impl Keeping for SiteKeeps {
    fn keep(&self, cert: DeviceCert) -> tilo_peer_state::new_device::Kept {
        let workspaces = self.0.clone();
        Box::pin(async move {
            if let Err(error) = tilo_workspace_service::workspaces::person_workspace::devices::keep(
                &workspaces,
                &cert,
            )
            .await
            {
                tracing::warn!(%error, "the added computer is not listed among the person's devices");
            }
        })
    }
}

/// The laptop approving the device that brings its ticket: its person's root certifies it.
struct RootSigns {
    root: SigningKey,
    device: SigningKey,
    cert: DeviceCert,
}

impl Approver for RootSigns {
    fn approve(&self, asked: Asked) -> Approval {
        let mut cert = asked.cert.clone();
        sign_device_cert(&mut cert, &self.root).unwrap();
        Box::pin(std::future::ready(Ok(Approved {
            cert,
            approver: self.cert.clone(),
            signature: self.device.sign(&asked.digest).to_bytes(),
        })))
    }
}

/// The laptop's endpoint serving `ticket` as the device that printed it does. Called inside a
/// runtime context.
fn serving(
    endpoint: &Endpoint,
    key: &SigningKey,
    ticket: Ticket,
    person: [u8; 32],
    entry: Entry,
    approver: Arc<dyn Approver>,
    keeps: Arc<dyn Keeping>,
) -> Router {
    let service = tilo_peer_state::actions::ActionService::new(
        key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    );
    service.admit_uncertified(
        tilo_device_identity::login::SCOPE,
        Arc::new(
            tilo_peer_state::new_device::ServedTicket::new(
                ticket,
                person,
                entry,
                DeviceCapabilities::member(),
                approver,
                tilo_user_identity::RevocationList::new(),
                tilo_clock::unix_now_ns()
                    .saturating_add(tilo_device_identity::login::TICKET_LIFETIME_NS),
            )
            // The computer the ticket certifies is listed among the person's devices as it is
            // certified, as the daemon that issues a ticket lists it.
            .kept_by(keeps),
        ),
    );
    Router::builder(endpoint.clone())
        .accept(tilo_peer_state::actions::ACTIONS_ALPN, service)
        .spawn()
}

/// Notes, `apps/tl/notes`, built into a signed `.tapp` under `scratch`: the file a person installs.
fn notes_tapp(scratch: &Path) -> PathBuf {
    use tilolang_cli::package::{self, BuildOptions};
    let tl = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
    // A build writes each package's ids lock and source catalogue: it builds a copy, never the
    // tree it runs from.
    let source = scratch.join("tl");
    let dirs = tilolang_cli::package_dirs(&tl.join("notes"), std::slice::from_ref(&tl)).unwrap();
    for dir in &dirs {
        copy_tree(dir, &source.join(dir.file_name().unwrap()));
    }
    let publisher = tilolang_cli::identity_at(&scratch.join("publisher.json")).unwrap();
    let built = package::build(
        &BuildOptions {
            dir: source.join("notes"),
            search: vec![source.clone(), tl],
            out: scratch.join("tapps"),
            tilo_runtime: package::DEFAULT_TILO_RUNTIME.to_owned(),
            tilolang_runtime: package::default_tilolang_runtime(),
            version: "0.1.0".to_owned(),
            locked: false,
        },
        &publisher,
    )
    .unwrap_or_else(|error| panic!("Notes builds: {error}"));
    built.last().expect("Notes is built").tapp.clone()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The phone's files, laid out as an Android app's are, and the environment its daemon reads its
/// places from. It is one test in this process: the daemon reads its places once.
fn phone_home() -> (tempfile::TempDir, tilo_test_env::ScopedEnv) {
    let dir = tempfile::Builder::new().prefix("phone").tempdir().unwrap();
    let root = dir.path();
    let files = root.join("files");
    let cache = root.join("cache");
    let set = [
        ("HOME", root.to_path_buf()),
        ("XDG_DATA_HOME", files.join("data")),
        ("XDG_CONFIG_HOME", files.join("config")),
        ("XDG_STATE_HOME", files.join("state")),
        ("XDG_CACHE_HOME", cache.clone()),
        ("XDG_RUNTIME_DIR", cache.join("run")),
        ("TILO_IDENTITY_ROOT", files.join("data/tilo")),
    ];
    for (_, path) in &set {
        std::fs::create_dir_all(path).unwrap();
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    for (name, path) in set {
        env.set(name, path);
    }
    // A phone keeps its identity's secrets in its own files: it has no keyring.
    env.set("TILO_IDENTITY_FILE_SECRETS", "1");
    for inherited in [
        "TILO_RUNTIME_PROFILE",
        "TILO_APPS_DATA_DIR",
        "TILO_APPS_SYSTEM_DIR",
        "TILO_AGENT_SOCKET",
        "TILO_API_KEY",
        "TILO_API_URL",
        "CREDENTIALS_DIRECTORY",
    ] {
        env.remove(inherited);
    }
    (dir, env)
}

/// The app a workspace installs: Notes, by the id every device and view names it.
const NOTES: &str = "so.tilo.notes";

/// A host installs Notes into its workspace, and a phone that joins it by its laptop's ticket
/// lists Notes, by its name, and nothing that is not an app the workspace installed.
#[test]
fn a_phone_that_joins_lists_its_workspaces_installed_apps_by_name_and_no_others() {
    let (_home, mut env) = phone_home();
    let server = Server::start();
    env.set("TILO_RELAY", server.relay_url());
    let scratch = tempfile::tempdir().unwrap();
    // The workspace runs Notes before the phone ever joins it; nothing is installed on the phone.
    server.install(&notes_tapp(scratch.path()));
    let keeps: Arc<dyn Keeping> = Arc::new(SiteKeeps(server.host.workspaces().clone()));
    let laptop = Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry(),
        Arc::clone(&keeps),
    );
    let profile = tilo_machine::account_daemon_profile().expect("the phone keeps a daemon");
    let mut phone = Device::start(&profile).expect("the phone's daemon");
    phone
        .log_in(&laptop.ticket(), "Pixel")
        .expect("the phone redeems the ticket its person's laptop printed");
    until("the phone lists the workspace it joined", || {
        let devices = phone.devices().map_err(|error| format!("{error:#}"))?;
        if devices
            .workspaces
            .iter()
            .any(|workspace| workspace.key == server.workspace)
        {
            Ok(())
        } else {
            Err(format!("{devices:?}"))
        }
    });

    let apps = until(
        "the phone lists Notes, which the workspace runs: its device reads the record its host \
         installed it into",
        || {
            let apps = phone
                .apps(&server.workspace)
                .map_err(|error| format!("{error:#}"))?;
            if apps.iter().any(|app| app.key == NOTES) {
                Ok(apps)
            } else {
                Err(format!("the phone lists {apps:?}"))
            }
        },
    );
    assert_eq!(
        apps.len(),
        1,
        "nothing that is not an app the workspace installed is listed: {apps:?}"
    );
    assert_eq!(apps[0].key, NOTES, "the app the workspace runs: {apps:?}");
    assert_eq!(
        apps[0].name, "Notes",
        "Notes is listed by its name: {apps:?}"
    );

    phone.stop().expect("the phone's daemon stops");
    drop(laptop);
}

/// A device that serves its person now takes the login it is given in as it is made: the
/// workspaces the ticket entered are held here, and its list reads them without the person's part
/// starting again.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one login into a second workspace, and the part that keeps it, read top to bottom"
)]
fn a_running_device_keeps_the_workspace_its_login_enters() {
    use tilo_actions::remote::{
        ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
        RemoteActionTarget,
    };
    use tilo_device_identity::login::{LOG_IN, PROVIDER, RESOURCE};

    let (_home, mut env) = phone_home();
    let server = Server::start();
    env.set("TILO_RELAY", server.relay_url());
    let keeps: Arc<dyn Keeping> = Arc::new(SiteKeeps(server.host.workspaces().clone()));
    let laptop = Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry(),
        Arc::clone(&keeps),
    );
    let profile = tilo_machine::account_daemon_profile().expect("the phone keeps a daemon");
    let mut phone = Device::start(&profile).expect("the phone's daemon");
    phone
        .log_in(&laptop.ticket(), "Pixel")
        .expect("the phone redeems the ticket its person's laptop printed");
    until("the phone lists the workspace it joined", || {
        let devices = phone.devices().map_err(|error| format!("{error:#}"))?;
        if devices
            .workspaces
            .iter()
            .any(|workspace| workspace.key == server.workspace)
        {
            Ok(())
        } else {
            Err(format!("{devices:?}"))
        }
    });

    // A second workspace of the person's, the laptop lets a device into now, while the phone's
    // person's part serves.
    let garage = server.host.create("Garage");
    let issuer = Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry_of(&garage, "Garage"),
        Arc::clone(&keeps),
    );
    let part = phone
        .services(Duration::from_secs(30))
        .expect("the phone's person's part serves");
    let serving = Arc::clone(&part);
    // The login a session's sign-in reaches the device's own actions with, answered by them
    // exactly as `devices#logIn` is.
    let keys = tilo_api::calls::device_keys();
    let key = tilo_device_identity::persist::read_device_key(&keys.device_privkey)
        .expect("the phone's device key, which its daemon took");
    let answers = crate::device::unclaimed::DeviceOnly::at(
        key,
        tilo_device_identity::login::Place::here(keys).expect("the phone's login"),
        profile,
        crate::device::services::this_account_uid(),
    );
    answers.serving(&part);
    let invocation = tilo_actions::InvocationId::new();
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(RESOURCE, LOG_IN),
        },
        argument: InvokeArgument::Json(
            serde_json::json!({"ticket": issuer.ticket(), "agent": "tilo"})
                .as_object()
                .cloned()
                .expect("the login's arguments"),
        ),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
        asks: false,
    };
    let receipt = answers
        .invoke(&Caller::local_system(), request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten();
    assert!(
        matches!(
            receipt,
            Some(Ok(ref receipt)) if matches!(receipt.status, ReceiptStatus::Accepted)
        ),
        "the device's own actions take the login: {receipt:?}"
    );
    // The device's own actions ran the login: what it became, or why it did not.
    let answered = tilo_test_env::wait_for("the login's answer", || {
        match tilo_actions::remote::ActionProvider::observe(
            &answers,
            &Caller::local_system(),
            &invocation,
        ) {
            Some(state)
                if !matches!(
                    state.update,
                    tilo_actions::InvocationUpdate::Accepted { .. }
                ) =>
            {
                Some(state)
            }
            _ => None,
        }
    });
    assert!(
        matches!(
            answered.update,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ),
        "the device's own actions ran the login: {answered:?}"
    );
    until(
        "the person's part lists the workspace its login entered, without starting again",
        || {
            let devices = phone.devices().map_err(|error| format!("{error:#}"))?;
            if devices
                .workspaces
                .iter()
                .any(|workspace| workspace.key == garage)
            {
                Ok(())
            } else {
                let reading = Arc::clone(&serving);
                let held = crate::device::workspaces::block_on_owned(async move {
                    reading.workspaces().ids().await
                });
                Err(format!(
                    "{devices:?}; the part serving {} holds {held:?}",
                    serving.serves_person()
                ))
            }
        },
    );
    let holds = phone
        .services(Duration::from_secs(30))
        .expect("the person's part still serves");
    assert!(
        Arc::ptr_eq(&part, &holds),
        "the person's part did not start again for the login"
    );
    phone.stop().expect("the phone's daemon stops");
    drop(issuer);
}
