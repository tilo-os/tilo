//! Adding this computer to the person's devices with the code another of
//! their devices printed.
//!
//! `tilo device add` on a device of the person's prints a one-use code for ten
//! minutes. This computer's daemon answers that code with its `devices#logIn`
//! action, the same action and the same call `tilo login` makes: the daemon
//! dials the device that printed the code, that device's root certifies this
//! computer's one device key, and the answer is kept as this computer's login.
//! Setup then takes the login up as this account's identity, exactly as a
//! daemon starting with one does, and hands off to the desktop.
//!
//! The call waits on the network and on the person approving this computer on
//! their other device, so it runs on a thread of its own; its outcome wakes
//! the compositor's loop, which never reads the disk for it.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::json;
use tilo_api::client::{Client, Reach};
use tilo_device_identity::IdentityPaths;
use tilo_device_identity::login::{LOG_IN, Place, Ticket};
use tilo_onboarding::{t, t_args};

use crate::identity::MintedIdentity;

/// The resource the daemon's device actions act on.
const RESOURCE: &str = "devices";
/// The agent this computer logs in as. The desktop is the device itself, not
/// an agent of the person's, so it does not borrow one of their agents' names.
const AGENT: &str = "Tilo";
/// How long this computer's daemon has to answer after it is asked to start.
const DAEMON_START_WITHIN: Duration = Duration::from_secs(15);

/// Why adding this computer stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NearbyCause {
    /// The code is not one this computer can use: mistyped, or not a ticket.
    Wrong,
    /// The code was already spent.
    Used,
    /// The code is past its ten minutes.
    Expired,
    /// The device that printed the code could not be reached.
    Unreached,
    /// This computer could not save the login or the identity it brought.
    Storage,
    /// This computer's daemon did not start and answer.
    NotStarted,
    /// Setup here cannot add a computer at all.
    Unavailable,
    /// Nothing more is known.
    Unknown,
}

fn cause_keys(cause: NearbyCause) -> (&'static str, &'static str) {
    match cause {
        NearbyCause::Wrong => ("onboarding-nearby-wrong-title", "onboarding-nearby-wrong"),
        NearbyCause::Used => ("onboarding-nearby-used-title", "onboarding-nearby-used"),
        NearbyCause::Expired => (
            "onboarding-nearby-expired-title",
            "onboarding-nearby-expired",
        ),
        NearbyCause::Unreached => (
            "onboarding-nearby-unreachable-title",
            "onboarding-nearby-unreachable",
        ),
        NearbyCause::Storage => (
            "onboarding-nearby-storage-title",
            "onboarding-nearby-storage",
        ),
        NearbyCause::NotStarted => (
            "onboarding-nearby-not-started-title",
            "onboarding-nearby-not-started",
        ),
        NearbyCause::Unavailable => (
            "onboarding-nearby-unavailable-title",
            "onboarding-nearby-unavailable",
        ),
        NearbyCause::Unknown => (
            "onboarding-nearby-failed-title",
            "onboarding-nearby-unknown",
        ),
    }
}

/// Why adding this computer stopped, and this computer's own words when it
/// was not one of the reasons a person can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearbyFailure {
    pub cause: NearbyCause,
    pub detail: Option<String>,
}

impl NearbyFailure {
    pub fn new(cause: NearbyCause) -> Self {
        Self {
            cause,
            detail: None,
        }
    }

    /// What went wrong, in plain words: the banner's title.
    pub fn title(&self) -> String {
        t(cause_keys(self.cause).0)
    }

    /// What to do next, followed by this computer's own words when it gave
    /// any a person could act on.
    pub fn message(&self) -> String {
        let next = t(cause_keys(self.cause).1);
        match &self.detail {
            Some(detail) => format!(
                "{next} {}",
                t_args(
                    "onboarding-nearby-detail",
                    &[("detail", detail.clone().into())]
                )
            ),
            None => next,
        }
    }

    fn unavailable(error: impl std::fmt::Display) -> Self {
        Self {
            cause: NearbyCause::Unavailable,
            detail: Some(error.to_string()),
        }
    }

    fn storage(error: impl std::fmt::Display) -> Self {
        Self {
            cause: NearbyCause::Storage,
            detail: Some(error.to_string()),
        }
    }

    fn not_started(error: impl std::fmt::Display) -> Self {
        Self {
            cause: NearbyCause::NotStarted,
            detail: Some(error.to_string()),
        }
    }
}

/// The daemon's refusal, read as one of the reasons a person can act on. The
/// details are kept only where nothing more is known: the designed reasons
/// say what to do without the daemon's machinery.
fn refused(error: &anyhow::Error) -> NearbyFailure {
    let said = format!("{error:#}");
    let cause = if said.contains("was used already") {
        NearbyCause::Used
    } else if said.contains("ran out") {
        NearbyCause::Expired
    } else if said.contains("is not the ticket's secret") || said.contains("not a Tilo ticket") {
        NearbyCause::Wrong
    } else if said.contains("can't be reached from here") || said.contains("did not take it") {
        NearbyCause::Unreached
    } else if said.contains("reaching Tilo on this computer") || said.contains("didn't start") {
        NearbyCause::NotStarted
    } else if said.contains("serves no workspace") {
        NearbyCause::Unavailable
    } else {
        NearbyCause::Unknown
    };
    NearbyFailure {
        cause,
        detail: (cause == NearbyCause::Unknown).then_some(said),
    }
}

/// What setup should show after one [`NearbyJoin::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NearbyEvent {
    /// Nothing changed.
    Idle,
    /// This computer joined the person's identity. Adding it is over.
    Joined(MintedIdentity),
    /// Adding this computer ended without joining, and why.
    Failed(NearbyFailure),
}

/// One code being redeemed. Dropping it stops setup from taking the result
/// up; the call itself is bounded by the code's lifetime on the issuing
/// device.
pub struct NearbyJoin {
    outcome: mpsc::Receiver<NearbyEvent>,
    /// Set when setup gives the attempt up, so its login is not taken up here.
    cancelled: Arc<AtomicBool>,
}

impl NearbyJoin {
    /// Redeem `ticket` and take the login up as this account's identity.
    ///
    /// # Errors
    /// The thread that redeems it cannot be started.
    pub fn start(
        ticket: String,
        identity_dir: &Path,
        paths: &IdentityPaths,
        label: &str,
    ) -> Result<Self, NearbyFailure> {
        let (sender, outcome) = mpsc::channel();
        // Made on the loop's thread, so the outcome wakes the loop it is for.
        let wake = crate::services::loop_waker();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancelled);
        let identity_dir = identity_dir.to_path_buf();
        let paths = paths.clone();
        let label = label.to_owned();
        std::thread::Builder::new()
            .name("tilo-device-join".into())
            .spawn(move || {
                let event = match join(&ticket, &identity_dir, &paths, &label, &stop) {
                    Ok(minted) => NearbyEvent::Joined(minted),
                    Err(failure) => NearbyEvent::Failed(failure),
                };
                let _ = sender.send(event);
                wake.wake_by_ref();
            })
            .map_err(NearbyFailure::not_started)?;
        Ok(Self { outcome, cancelled })
    }

    /// Take in the thread's outcome. Never reads the disk: everything it needs
    /// arrived through the channel.
    pub fn poll(&mut self, _now: Instant) -> NearbyEvent {
        match self.outcome.try_recv() {
            Ok(event) => event,
            Err(mpsc::TryRecvError::Empty) => NearbyEvent::Idle,
            // The thread ended without an outcome, which only a panic does.
            Err(mpsc::TryRecvError::Disconnected) => {
                NearbyEvent::Failed(NearbyFailure::new(NearbyCause::Unknown))
            }
        }
    }
}

impl Drop for NearbyJoin {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Redeem `ticket` on this computer's daemon and take the login it answers up
/// as the identity under `identity_dir`.
fn join(
    ticket: &str,
    identity_dir: &Path,
    paths: &IdentityPaths,
    label: &str,
    cancelled: &AtomicBool,
) -> Result<MintedIdentity, NearbyFailure> {
    let ticket = Ticket::parse(ticket).map_err(|_| NearbyFailure::new(NearbyCause::Wrong))?;
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(paths)
        .map_err(NearbyFailure::storage)?;
    let place = Place::here(paths.clone()).map_err(NearbyFailure::unavailable)?;
    make_login_dir(place.dir())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(NearbyFailure::not_started)?;
    // A computer no person's certificate names yet logs in itself, as `tilo login` does: its
    // daemon serves the device key alone until then, and has no API to ask.
    if !tilo_device_identity::login::claimed(&place, &key.verifying_key().to_bytes()) {
        runtime
            .block_on(tilo_api::daemon::enter::log_in_here(
                &key,
                &place,
                &ticket.encode(),
                AGENT,
                Some(label),
            ))
            .map_err(|error| refused(&error))?;
        return taken_up(&place, identity_dir, paths, cancelled);
    }
    ensure_daemon()?;
    runtime
        .block_on(async {
            let client = Client::open(Reach {
                api: None,
                key_file: None,
                json: false,
            })?;
            let workspace = client.founded_workspace().await?;
            client
                .act(
                    &workspace,
                    RESOURCE,
                    LOG_IN,
                    &json!({"ticket": ticket.encode(), "agent": AGENT, "label": label}),
                    "Log in",
                )
                .await?;
            Ok::<(), anyhow::Error>(())
        })
        .map_err(|error| refused(&error))?;
    taken_up(&place, identity_dir, paths, cancelled)
}

/// This account's identity, from the login kept in `place`, unless setup gave the attempt up.
fn taken_up(
    place: &Place,
    identity_dir: &Path,
    paths: &IdentityPaths,
    cancelled: &AtomicBool,
) -> Result<MintedIdentity, NearbyFailure> {
    // Setup gave the attempt up: the login stays kept for a later daemon to
    // take up, and nothing is written into this account's identity here.
    if cancelled.load(Ordering::Acquire) {
        return Err(NearbyFailure::new(NearbyCause::Unknown));
    }
    let device = tilo_device_identity::login::load(place)
        .map_err(NearbyFailure::storage)?
        .ok_or_else(|| NearbyFailure::storage("the login was not kept"))?
        .1;
    // What a daemon starting with this login does for itself, done here so the
    // identity is this account's before the desktop opens.
    let now = tilo_clock::unix_now_ns();
    tilo_device_identity::persist::install_device_cert(paths, &device.cert, now)
        .map_err(NearbyFailure::storage)?;
    tilo_identity::provisioning::complete_enrolled_device(identity_dir, paths, now)
        .map_err(NearbyFailure::storage)?;
    let active = tilo_identity::store::resolve_active_identity(identity_dir)
        .ok_or_else(|| NearbyFailure::storage("the identity record was not written"))?;
    crate::identity::existing_identity_at(&active).map_err(|why| NearbyFailure {
        cause: NearbyCause::Unknown,
        detail: Some(why),
    })
}

/// The login directory the device's daemon and login live in, made private
/// here when this computer has none yet.
fn make_login_dir(dir: &Path) -> Result<(), NearbyFailure> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(NearbyFailure::storage)
}

/// Wait for this computer's daemon to serve its API, its service started when
/// none does ([`tilo_api::daemon::service::start`]).
fn ensure_daemon() -> Result<(), NearbyFailure> {
    tilo_api::daemon::this_computer()
        .and_then(|place| place.wait_for_api(DAEMON_START_WITHIN))
        .map_err(NearbyFailure::not_started)
}

#[cfg(test)]
pub(crate) mod testing {
    //! A stand-in for this computer's daemon at the one place setup reaches
    //! it: the socket the real daemon serves Tilo's API on. It answers the two
    //! calls `tilo login` makes, `account/workspaces#list` and the `devices#logIn`
    //! action, and keeps a login the real daemon would keep.

    use std::io::{Read as _, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use ed25519_dalek::SigningKey;
    use serde_json::json;
    use tilo_device_identity::IdentityPaths;
    use tilo_device_identity::login::{Device, Place};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    use super::make_login_dir;

    pub(crate) const WORKSPACE: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";

    /// What the stand-in daemon answers the login with.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Answer {
        /// Certify this computer, as the device that printed the code does.
        Certify,
        /// Refuse it with `message`, as the device of a spent code does.
        Refuse(&'static str),
        /// Wait a while before certifying it, as a device a person has to
        /// approve by hand does.
        Hold,
    }

    /// The stand-in daemon, listening where this computer's own would.
    pub(crate) struct FakeDaemon {
        api: PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeDaemon {
        pub(crate) fn start(paths: &IdentityPaths, answer: Answer) -> Self {
            let place = Place::here(paths.clone()).expect("this account's login place");
            make_login_dir(place.dir()).expect("the login directory");
            let key = tilo_device_identity::persist::load_device_key(paths).expect("a device key");
            let daemon = tilo_api::daemon::this_computer().expect("the daemon's place");
            // A daemon serving a computer with no login is this computer's
            // own device already, certified as such: that certificate is
            // what names the socket `tilo` commands reach it on.
            let root = SigningKey::from_bytes(&[0x42; 32]);
            let cert = cert_for(&root, &key, "this computer");
            tilo_device_identity::persist::write_device_cert(&paths.device_cert, &cert)
                .expect("the daemon's certificate");
            let listener = UnixListener::bind(daemon.api()).expect("the daemon's socket");
            listener
                .set_nonblocking(true)
                .expect("a socket that is looked at");
            let stop = Arc::new(AtomicBool::new(false));
            let running = Arc::clone(&stop);
            let paths = paths.clone();
            let thread = std::thread::Builder::new()
                .name("fake-tilo-daemon".into())
                .spawn(move || {
                    while !running.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((stream, _)) => serve(stream, &paths, answer),
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(std::time::Duration::from_millis(5));
                            }
                            Err(_) => return,
                        }
                    }
                })
                .expect("the stand-in's thread");
            Self {
                api: daemon.api().to_path_buf(),
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = std::fs::remove_file(&self.api);
        }
    }

    /// One request of the client, answered.
    fn serve(mut stream: UnixStream, paths: &IdentityPaths, answer: Answer) {
        let Ok((head, body)) = request(&mut stream) else {
            return;
        };
        let (status, answer) = if head.starts_with("POST /v1/resources/account")
            && head.contains("workspaces/actions/list")
        {
            (
                200,
                json!({"result": {"workspaces": [{"id": WORKSPACE, "name": "Studio"}]}}),
            )
        } else if head.contains("/resources/devices/actions/logIn") {
            if matches!(answer, Answer::Hold) {
                std::thread::sleep(std::time::Duration::from_millis(750));
            }
            let asked: serde_json::Value =
                serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            match answer {
                Answer::Certify | Answer::Hold => {
                    let ticket = asked.get("ticket").and_then(|value| value.as_str());
                    assert!(
                        ticket.is_some_and(|ticket| ticket.starts_with("tilo1")),
                        "the login carries the code: {asked}"
                    );
                    assert!(
                        asked
                            .get("agent")
                            .and_then(|value| value.as_str())
                            .is_some_and(|agent| !agent.is_empty()),
                        "the login names the agent here: {asked}"
                    );
                    keep_login(paths);
                    (200, json!({"result": {}}))
                }
                Answer::Refuse(message) => (
                    409,
                    json!({"error": {"code": "device-action", "message": message}}),
                ),
            }
        } else {
            (
                404,
                json!({"error": {"code": "not-found", "message": "no such route"}}),
            )
        };
        let body = answer.to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
    }

    /// The request line and headers, then its body.
    fn request(stream: &mut UnixStream) -> std::io::Result<(String, Vec<u8>)> {
        let mut data = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Err(std::io::Error::other("the client ended first"));
            }
            data.extend_from_slice(chunk.get(..read).unwrap_or_default());
            let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&data[..end]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            let body = end.checked_add(4).unwrap_or(end);
            while data.len() < body.saturating_add(length) {
                let read = stream.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                data.extend_from_slice(chunk.get(..read).unwrap_or_default());
            }
            let body_bytes = data
                .get(body..body.saturating_add(length))
                .unwrap_or_default();
            return Ok((head, body_bytes.to_vec()));
        }
    }

    /// The login a real daemon would keep for the computer that asked.
    fn keep_login(paths: &IdentityPaths) {
        let root = SigningKey::from_bytes(&[0x42; 32]);
        let key = tilo_device_identity::persist::load_device_key(paths).expect("a device key");
        let cert = cert_for(&root, &key, "the kitchen computer");
        let server_cert = cert_for(&root, &root, "the server");
        let place = Place::here(paths.clone()).expect("a place");
        let device = Device {
            site: "https://tilo.test/".into(),
            host_origin: "https://eu1.tilo.test".into(),
            workspace: WORKSPACE.into(),
            workspace_name: Some("Studio".into()),
            agent: "Tilo".into(),
            cert,
            server_cert,
            server_address: None,
        };
        tilo_device_identity::login::store(&place, &device).expect("the login kept");
    }

    /// A member certificate `root` signs for `key`.
    pub(crate) fn cert_for(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(60_000_000_000),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).expect("the person's signature");
        cert
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::testing::{Answer, FakeDaemon, WORKSPACE};
    use super::*;
    use ed25519_dalek::SigningKey;
    use std::path::PathBuf;

    /// An account of its own, logged out: private runtime, state, config and
    /// identity directories, and this computer's one device key, all under a
    /// temporary home so nothing here reads or writes the real one.
    pub(crate) struct Account {
        pub dir: tempfile::TempDir,
        pub paths: IdentityPaths,
        /// Held for the test's whole run: the environment above is the
        /// process's.
        _env: tilo_test_env::ScopedEnv,
    }

    impl Account {
        pub(crate) fn new() -> Self {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = tempfile::tempdir().expect("tempdir");
            let private = |name: &str| {
                let path = dir.path().join(name);
                std::fs::create_dir_all(&path).expect("a directory");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                    .expect("a private directory");
                path
            };
            let runtime = private("run");
            let state = private("state");
            let config = private("config");
            let root = dir.path().join("identity-root");
            std::fs::create_dir_all(&root).expect("an identity root");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("HOME", dir.path())
                .set("XDG_RUNTIME_DIR", &runtime)
                .set("XDG_STATE_HOME", &state)
                .set("XDG_CONFIG_HOME", &config)
                .set("TILO_IDENTITY_ROOT", &root);
            let paths = tilo_identity::provisioning::collaboration_identity_paths();
            let key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
            tilo_device_identity::persist::adopt_device_key(&paths, &key.to_bytes())
                .expect("this computer's device key");
            Self {
                dir,
                paths,
                _env: env,
            }
        }

        /// The identity directory setup writes this account's identity into.
        pub(crate) fn identity_dir(&self) -> PathBuf {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = self.dir.path().join("config/tilo/identity");
            std::fs::create_dir_all(&dir).expect("an identity directory");
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .expect("a private identity directory");
            dir
        }
    }

    /// A code in the form `tilo device add` prints it.
    fn code() -> String {
        let issuer = tilo_device_identity::login::Reached {
            endpoint: *SigningKey::from_bytes(&[9; 32]).verifying_key().as_bytes(),
            relay: None,
            direct: Vec::new(),
        };
        tilo_device_identity::login::Ticket::new(issuer).encode()
    }

    fn wait_for_outcome(join: &mut NearbyJoin) -> NearbyEvent {
        tilo_test_env::wait_for("the code to be redeemed", || {
            match join.poll(Instant::now()) {
                NearbyEvent::Idle => None,
                event => Some(event),
            }
        })
    }

    tilo_test_env::scenario! {
        fn a_code_another_of_the_persons_devices_printed_adds_this_computer;

        phase a_code_printed_elsewhere_certifies_this_computer_and_is_taken_up;
        {
            let account = Account::new();
            let identity_dir = account.identity_dir();
            let _daemon = FakeDaemon::start(&account.paths, Answer::Certify);
            let mut join = NearbyJoin::start(
                code(),
                &identity_dir,
                &account.paths,
                "the kitchen computer",
            )
            .expect("the join starts");
            let NearbyEvent::Joined(minted) = wait_for_outcome(&mut join) else {
                panic!("the stand-in daemon certified this computer");
            };
            // The login is kept, the device's certificate is this computer's,
            // and the person is this account's identity, held as a public
            // record: the root stayed on the device that printed the code.
            let place = Place::here(account.paths.clone()).expect("a place");
            let device = tilo_device_identity::login::load(&place)
                .expect("the kept login reads")
                .expect("a login is kept")
                .1;
            assert_eq!(device.workspace, WORKSPACE);
            assert_eq!(device.label(), "the kitchen computer");
            let kept = tilo_device_identity::persist::read_device_cert(&account.paths.device_cert)
                .expect("this device's certificate");
            assert_eq!(kept, device.cert);
            assert_eq!(minted.fingerprint, hex::encode(device.cert.user_public_key));
            assert!(
                crate::identity::holds_no_identity_secret(&minted.path),
                "the joined device holds no secret of the identity"
            );
            assert_eq!(
                tilo_identity::store::resolve_active_identity(&identity_dir),
                Some(minted.path),
                "the joined identity is this account's"
            );
        }

        phase a_device_that_cannot_be_reached_says_so;
        {
            let account = Account::new();
            let identity_dir = account.identity_dir();
            // The stand-in answers the action as a device that never heard of
            // the code does when the network cannot carry it.
            let _daemon = FakeDaemon::start(
                &account.paths,
                Answer::Refuse("The device that made this ticket can't be reached from here: it may be asleep."),
            );
            let mut join = NearbyJoin::start(
                code(),
                &identity_dir,
                &account.paths,
                "the kitchen computer",
            )
            .expect("the join starts");
            let NearbyEvent::Failed(failure) = wait_for_outcome(&mut join) else {
                panic!("an unreached code adds nothing");
            };
            assert_eq!(failure.cause, NearbyCause::Unreached);
            assert!(failure.message().contains("connected"), "{}", failure.message());
        }
    }

    /// The person's device that printed a code, as it serves it: its root signs the certificate
    /// of the computer that brings the code.
    struct RootSigns {
        root: SigningKey,
        device: SigningKey,
        cert: tilo_user_identity::DeviceCert,
    }

    impl tilo_device_identity::login::Approver for RootSigns {
        fn approve(
            &self,
            asked: tilo_device_identity::login::Asked,
        ) -> tilo_device_identity::login::Approval {
            use ed25519_dalek::Signer as _;
            let mut cert = asked.cert.clone();
            tilo_user_identity::sign_device_cert(&mut cert, &self.root).expect("the root signs");
            Box::pin(std::future::ready(Ok(
                tilo_device_identity::login::Approved {
                    cert,
                    approver: self.cert.clone(),
                    signature: self.device.sign(&asked.digest).to_bytes(),
                },
            )))
        }
    }

    /// A computer no person's certificate names yet, whose daemon serves its device key alone,
    /// joins with a code itself: the device that printed it, reached over iroh, has the person's
    /// root certify this computer's key, and the person is this account's identity.
    #[test]
    fn a_computer_nobody_certified_yet_joins_with_a_code_itself() {
        use tilo_device_identity::login::{Entry, Ticket};
        let account = Account::new();
        let identity_dir = account.identity_dir();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let root = SigningKey::from_bytes(&[0x42; 32]);
        let device = SigningKey::from_bytes(&[0x43; 32]);
        let cert = super::testing::cert_for(&root, &device, "the person's laptop");
        let endpoint = runtime.block_on(async {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .secret_key(iroh::SecretKey::from_bytes(&device.to_bytes()))
                .relay_mode(iroh::RelayMode::Disabled)
                .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
                .expect("a loopback address")
                .bind()
                .await
                .expect("the laptop's endpoint")
        });
        let reached = tilo_api::daemon::enter::reached_of(&endpoint.addr());
        let ticket = Ticket::new(reached.clone());
        let router = {
            let _entered = runtime.enter();
            tilo_api::daemon::enter::issuing_for_test(
                &endpoint,
                &device,
                ticket.clone(),
                root.verifying_key().to_bytes(),
                Entry {
                    workspace: WORKSPACE.into(),
                    workspace_name: Some("Studio".into()),
                    server_cert: cert.clone(),
                    server: reached,
                    site: "https://tilo.test/".into(),
                    host_origin: "https://eu1.tilo.test".into(),
                },
                Arc::new(RootSigns {
                    root: root.clone(),
                    device: device.clone(),
                    cert,
                }),
            )
        };
        let key = tilo_device_identity::persist::load_device_key(&account.paths).expect("a key");
        let place = Place::here(account.paths.clone()).expect("a place");
        assert!(
            !tilo_device_identity::login::claimed(&place, &key.verifying_key().to_bytes()),
            "no person's certificate names this computer yet"
        );

        let mut join = NearbyJoin::start(
            ticket.encode(),
            &identity_dir,
            &account.paths,
            "the kitchen computer",
        )
        .expect("the join starts");
        let outcome = wait_for_outcome(&mut join);
        let NearbyEvent::Joined(minted) = outcome else {
            panic!("the device that printed the code certified this computer: {outcome:?}");
        };
        let person = root.verifying_key().to_bytes();
        let joined = tilo_device_identity::login::load(&place)
            .expect("the kept login reads")
            .expect("a login is kept")
            .1;
        assert_eq!(joined.cert.user_public_key, person);
        assert_eq!(
            joined.cert.device_public_key,
            key.verifying_key().to_bytes()
        );
        assert_eq!(joined.label(), "the kitchen computer");
        assert_eq!(
            tilo_device_identity::persist::read_device_cert(
                &account.paths.device_cert_for_user(&person)
            )
            .expect("the certificate filed under the person"),
            joined.cert
        );
        assert_eq!(minted.fingerprint, hex::encode(person));
        runtime.block_on(async {
            let _ = router.shutdown().await;
            endpoint.close().await;
        });
    }

    #[test]
    fn every_reason_a_person_can_act_on_has_its_own_words() {
        let general = NearbyFailure::new(NearbyCause::Unknown).title();
        let specific = [
            NearbyCause::Wrong,
            NearbyCause::Used,
            NearbyCause::Expired,
            NearbyCause::Unreached,
            NearbyCause::Storage,
            NearbyCause::NotStarted,
            NearbyCause::Unavailable,
        ];
        let mut titles = std::collections::BTreeSet::new();
        for cause in specific {
            let title = NearbyFailure::new(cause).title();
            assert_ne!(title, general, "{cause:?} is reported generically");
            assert!(
                titles.insert(title),
                "{cause:?} shares another cause's title"
            );
        }
        // The daemon's own words are kept only where nothing more is known.
        let expired = refused(&anyhow::anyhow!(
            "Your device didn't take this ticket: this ticket ran out: ask for a new one."
        ));
        assert_eq!(expired.cause, NearbyCause::Expired);
        assert_eq!(expired.detail, None);
        let unknown = refused(&anyhow::anyhow!("something else entirely"));
        assert_eq!(unknown.cause, NearbyCause::Unknown);
        assert_eq!(unknown.detail.as_deref(), Some("something else entirely"));
        assert!(
            unknown.message().contains("something else entirely"),
            "the words stay available: {}",
            unknown.message()
        );
    }
}
