//! `tilo machine add` at a profile whose identity `tilo login --new-identity`
//! made: the person's own action reaches the session, writes the machine's
//! plan into their Servers workspace, and answers — the identity the command
//! and the session read is one.

#![cfg(target_os = "linux")]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tilo_device_identity::IdentityPaths;
use tilo_session::{AppSocket, Options};

use crate::common::PERSON;

/// How long the person's action may take before the session is taken to have
/// hung on it: every step of planning a machine answers within this.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

/// `tilo login --new-identity` on a fresh profile: the person's root in the
/// one identity directory the session reads, and the device key, certificate
/// and fleet entry that admit this computer. Returns the device key this
/// process took as the daemon's, which the session signs with from here.
fn login(identity_dir: &Path) -> tilo_device_identity::persist::DeviceKeyLock {
    let paths = IdentityPaths::under(identity_dir);
    let taken_key = tilo_identity::provisioning::device_key_taken_as_daemon(&paths)
        .expect("this computer's device key");
    tilo_identity::provisioning::create_person(
        identity_dir,
        &paths,
        "plan cycle owner",
        "plan cycle owner",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    taken_key
}

/// The plan `tilo machine add --os server --name cupboard --disk
/// serial:tilo-install-target --network dhcp --unlock owner-device --updates
/// stable --no-workspaces` writes, as the command hands it to the session.
fn plan() -> String {
    serde_json::json!({
        "schema": 1,
        "plan": "0123456789abcdef0123456789abcdef",
        "revision": 1,
        "destination": "tilo-server",
        "name": "cupboard",
        "network": { "addressing": "dhcp" },
        "disk": { "kind": "serial", "identity": "tilo-install-target" },
        "unlock": "owner-device",
        "updates": "stable",
        "hosting": [],
        "open": [],
        "token": { "nonce": "ab".repeat(16), "expires_at_ns": u64::MAX },
    })
    .to_string()
}

/// Send `request` on `socket` as `tilo-control` does, and answer what came
/// back: `None` when nothing did within `within`.
fn ask(socket: &Path, request: &serde_json::Value, within: Duration) -> Option<serde_json::Value> {
    let mut stream = UnixStream::connect(socket).expect("the test-control socket");
    stream.set_read_timeout(Some(within)).unwrap();
    stream
        .write_all(format!("{request}\n").as_bytes())
        .expect("the socket takes the request");
    let mut line = String::new();
    let _ = BufReader::new(stream).read_line(&mut line);
    (!line.trim().is_empty()).then(|| serde_json::from_str(&line).expect("the answer is JSON"))
}

/// Ask `request` on `socket` while the session's loop is stepped, and answer
/// what came back: `None` when nothing did within [`ANSWER_WITHIN`].
fn ask_once(
    session: &mut tilo_session::Session,
    socket: &Path,
    request: serde_json::Value,
) -> Option<serde_json::Value> {
    let answered: Arc<OnceLock<Option<serde_json::Value>>> = Arc::default();
    {
        let answered = Arc::clone(&answered);
        let socket = socket.to_path_buf();
        std::thread::spawn(move || {
            let _ = answered.set(ask(&socket, &request, ANSWER_WITHIN));
        });
    }
    step_until(session, ANSWER_WITHIN + Duration::from_secs(5), || {
        answered.get().is_some()
    });
    answered.get().cloned().flatten()
}

/// Step the session until `done` answers, or `within` has passed.
fn step_until(
    session: &mut tilo_session::Session,
    within: Duration,
    mut done: impl FnMut() -> bool,
) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    done()
}

#[test]
fn a_person_on_a_fresh_profile_plans_a_machine_and_is_answered() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let identity = home.config().join("tilo/identity");
    std::fs::create_dir_all(&identity).expect("the identity's directory");
    let secrets = home.root().join("secrets");
    // One tree for the person and their device, as a disposable profile has
    // it: the directory the session reads is where the command wrote.
    // The session is Tilo OS's, whose devices are native.
    home.env()
        .set("TILO_IDENTITY_ROOT", &identity)
        .set("TILO_SECRETS_DIR", &secrets)
        .set(
            tilo_identity::provisioning::TEST_PLATFORM_ENV,
            tilo_identity::provisioning::TILO_OS_PLATFORM,
        );
    let _taken_key = login(&identity);
    let socket = home.runtime().join("test-control.sock");
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: true,
        state: Some(home.state().to_path_buf()),
        app_socket: AppSocket::At(home.runtime().join("app_wire.sock")),
        test_control: Some(socket.clone()),
        ..Options::default()
    });
    // The command as `tilo machine add` asks it, once the session answers a
    // step of it: the command asks the same cheap one until the person's own
    // workspace is the one in front, then plans.
    let asking = |step: &'static str, args: serde_json::Value| {
        let target = tilo_actions::remote::RemoteActionTarget {
            provider: "tilo.machines".to_owned(),
            address: tilo_actions::ActionAddress::new("machines", step),
        };
        crate::common::action_frame(tilo_actions::remote::RemoteActionMessage::Invoke(
            crate::common::invocation(&target, &args, None, Some(5_000)),
        ))
    };
    let sent = Instant::now();
    let mut listed = None;
    let mut last = None;
    while sent.elapsed() < ANSWER_WITHIN {
        let answer = ask_once(
            &mut session,
            &socket,
            asking("machine.list", serde_json::json!({})),
        );
        let refused = answer
            .as_ref()
            .is_none_or(|answer| crate::common::refused(answer).is_some());
        if !refused {
            listed = answer;
            break;
        }
        last = answer;
        session.step(Duration::from_millis(50)).expect("a frame");
    }
    assert!(
        listed.is_some(),
        "the session never answered listing machines; its last answer was {last:?}"
    );
    let answer = ask_once(
        &mut session,
        &socket,
        asking(
            "machine.plan",
            serde_json::json!({
                "plan": plan(),
                "label": "cupboard",
                "token_file": "/tmp/cupboard.token",
                "installer_url": "https://tilo.sh",
            }),
        ),
    );
    let answer = answer.expect("the action answered");
    assert_eq!(
        crate::common::refused(&answer),
        None,
        "the person's own action is not refused after {:?}: {answer}",
        sent.elapsed()
    );
}
