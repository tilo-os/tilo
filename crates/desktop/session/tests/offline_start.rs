//! A desktop's first frame never waits on the network: a device with no relay it can reach (an
//! offline laptop, a captive portal) shows its desktop, and says it is usable, as fast as one
//! online does. The session here is a person's own, with an identity, its peer network and its own
//! workspace service, given a relay that never answers.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use tilo_session::{AppSocket, Options};

use crate::common::{PERSON, wait_for};

/// The session starts with an identity and its collaboration on: its first frame, and the ready
/// file the live update waits for, come in less time than a session waits on a network at all.
#[test]
fn a_desktop_with_no_reachable_relay_shows_and_says_it_is_ready_without_waiting_for_one() {
    use std::os::unix::fs::PermissionsExt as _;
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    // The account's own store below the identity directory, where every reader looks, and the
    // person's devices a certificate is checked against before anything signs with it.
    tilo_identity::sources::test_trust::install();
    let identity = tilo_identity::store::account_identity_dir(&home.config().join("tilo/identity"));
    std::fs::create_dir_all(&identity).expect("the identity's directory");
    let system = home.root().join("system");
    let secrets = home.root().join("secrets");
    std::fs::create_dir_all(&secrets).expect("the secret store's directory");
    home.env()
        .set("TILO_IDENTITY_ROOT", &system)
        .set("TILO_SECRETS_DIR", &secrets);
    // This process stands in for the machine's daemon: it takes the device key first, so the
    // identity is sealed with it.
    crate::common::take_device_key(&tilo_identity::provisioning::collaboration_identity_paths());
    tilo_identity::provisioning::ensure_throwaway_test_identity(
        &identity,
        &tilo_identity::provisioning::collaboration_identity_paths(),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("the throwaway identity");
    // The login's own directory, as the wrapper hands it: where the session keeps its lock state.
    let login = home.root().join("login");
    std::fs::create_dir_all(&login).expect("the login's directory");
    std::fs::set_permissions(&login, std::fs::Permissions::from_mode(0o700))
        .expect("the login's directory is this user's");
    let recovery = tilo_session::recovery::Recovery::begin(&login.join("logout-request"))
        .expect("the lock state");
    let ready = tilo_paths::login::session_dir(Some(home.runtime().as_os_str().to_owned()))
        .expect("the login's session directory")
        .join(tilo_paths::login::DESKTOP_READY_FILE);
    let started = Instant::now();
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: true,
        state: Some(home.state().to_path_buf()),
        recovery: Some(recovery),
        app_socket: AppSocket::At(home.runtime().join("app_wire.sock")),
        ..Options::default()
    });
    let mut first_frame = None;
    let usable = wait_for(&mut session, "the desktop says it is usable", |session| {
        if first_frame.is_none() && session.compositor().output_has_presented("HEADLESS-1") {
            first_frame = Some(started.elapsed());
        }
        ready.exists().then(|| started.elapsed())
    });
    let first_frame = first_frame.unwrap_or(usable);
    eprintln!("first frame after {first_frame:?}, usable after {usable:?}");
    let bound: Duration = tilo_endpoint_reachability::RELAY_WAIT;
    assert!(
        first_frame < bound,
        "the first frame came after {first_frame:?}: it waited on the network"
    );
    assert!(
        usable < bound,
        "the desktop said it is usable after {usable:?}: it waited on the network"
    );
}
