//! A device of the person's own is asked what a device that is not theirs
//! may do. The question arrives as a notification, its Answer puts it on this
//! device's confirmation sheet ("… wants to act for you", Allow once or
//! Deny), and the answer is what this device reports back — nothing about the
//! person's grants changes.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{ActionHostError, Caller, ConfirmationQuestion, ConfirmationReply};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::{HostIdentity, Reach, ShellNetwork};
use crate::shell::app_action_consent::set_session_attended;
use crate::shell::foreign_ask::ask_here;
use crate::shell::portal_dialog::{DialogView, PortalDialogController, PortalDialogQueue};
use crate::shell::state::WorkspaceTree;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn certify(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// The question as the foreign device puts it: its own words for the action,
/// and the room the invocation runs in.
fn question(room: &str) -> ConfirmationQuestion {
    ConfirmationQuestion {
        id: [0x2b; 32],
        device_label: "3f9a1c02b7d4e586".into(),
        room: room.to_owned(),
        resource_label: "Identities".into(),
        action_label: "Remove".into(),
        risk: tilo_actions::RiskLevel::Destructive.name().to_owned(),
    }
}

/// The sheet once it shows, pumped as the compositor does each tick.
fn sheet_shows(sheet: &mut PortalDialogController) -> DialogView {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        sheet.pump();
        if let Some(view) = sheet.view() {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "the question never reached the sheet"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The person answers the notice the question arrives as; until they do, no
/// sheet takes their screen.
fn answer_the_notice(sheet: &mut PortalDialogController) {
    use crate::shell::app_action_consent::{Notice, answer_notice, drain_notices};
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let posted = drain_notices().into_iter().find_map(|notice| match notice {
            Notice::Post { handle, .. } => Some(handle),
            Notice::Withdraw { .. } => None,
        });
        if let Some(handle) = posted {
            sheet.pump();
            assert!(sheet.view().is_none(), "the asked device raises its sheet");
            assert!(answer_notice(&handle));
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the question never arrived as a notice"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn answered(
    answer: &mpsc::Receiver<Option<Result<ConfirmationReply, ActionHostError>>>,
) -> ConfirmationReply {
    answer
        .recv_timeout(Duration::from_secs(30))
        .expect("this device reports what its person chose")
        .expect("the question is answered rather than dropped")
        .expect("the sheet answers rather than failing")
}

/// A device of the person's own: it holds their root, and one workspace.
struct OwnDevice {
    runtime: tokio::runtime::Runtime,
    _home: tilo_test_env::TestHome,
    _dir: tempfile::TempDir,
    net: ShellNetwork,
    room: String,
}

impl OwnDevice {
    fn start(root: &SigningKey) -> Self {
        use std::collections::HashMap;

        let home = tilo_test_env::TestHome::new().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("session");
        let mut tree = WorkspaceTree::bootstrap_default();
        let workspace_room = tree.desktop.workspaces[0].room().to_string();
        tree.persist_path = Some(session_dir.join("state.json"));
        let owner = root.clone();
        let net = ShellNetwork::spawn_with(
            tree.shared(),
            &session_dir.join("device-private/keypair.bin"),
            &session_dir.join("peer-sync"),
            Reach::Loopback,
            move |secret, _| {
                let key = SigningKey::from_bytes(&secret.to_bytes());
                let cert = certify(&owner, &key, "Ada's laptop");
                Some(HostIdentity {
                    device_cert: cert,
                    device_signing_key: key,
                    workspace_device_certs: HashMap::new(),
                    workspace_revocations: HashMap::new(),
                    identity_paths: None,
                })
            },
        )
        .unwrap();
        Self {
            runtime,
            _home: home,
            _dir: dir,
            net,
            room: workspace_room,
        }
    }
}

/// The device of the person's that is not theirs: it acts for their root.
fn foreign_caller(root: &SigningKey) -> Caller {
    Caller::authenticated(
        root.verifying_key().to_bytes(),
        key(9).verifying_key().to_bytes(),
    )
    .from_foreign_device(true)
}

#[test]
#[serial_test::serial]
fn the_foreign_devices_question_is_allowed_once_on_this_devices_sheet() {
    let root = key(1);
    let own = OwnDevice::start(&root);
    let question = question(&own.room);
    let caller = foreign_caller(&root);
    let (given, answer) = mpsc::channel();
    let mut sheet = PortalDialogController::new(PortalDialogQueue::global());
    let bounds = vello::kurbo::Rect::new(0.0, 0.0, 1600.0, 900.0);
    set_session_attended(true);

    // The question is put from the network's own runtime, as the actor does.
    let waiting = {
        let _on_the_network = own.runtime.enter();
        ask_here(&own.net, &caller, &question).expect("the person is asked here")
    };
    waiting.then(move |chosen| {
        let _ = given.send(chosen);
    });

    answer_the_notice(&mut sheet);
    let view = sheet_shows(&mut sheet);
    assert!(view.title().contains("wants to act for you"), "{view:?}");
    assert_eq!(view.app_name(), "Identities · Remove", "{view:?}");
    assert_eq!(
        view.choices(),
        ["Allow once"],
        "confirming grants nothing beyond this call"
    );
    assert!(
        view.detail().starts_with("Destructive"),
        "{}",
        view.detail()
    );
    let layout = sheet.layout(bounds).unwrap();
    assert!(sheet.click(layout.choices[0].center(), &layout));
    assert!(
        sheet.click(layout.confirm.center(), &layout),
        "Allow answers"
    );
    assert_eq!(answered(&answer), ConfirmationReply::AllowOnce);

    drop(own);
    set_session_attended(false);
}

#[test]
#[serial_test::serial]
fn a_denied_question_is_refused_for_the_foreign_device() {
    let root = key(1);
    let own = OwnDevice::start(&root);
    let question = question(&own.room);
    let caller = foreign_caller(&root);
    let (given, answer) = mpsc::channel();
    let mut sheet = PortalDialogController::new(PortalDialogQueue::global());
    let bounds = vello::kurbo::Rect::new(0.0, 0.0, 1600.0, 900.0);
    set_session_attended(true);

    // The question is put from the network's own runtime, as the actor does.
    let waiting = {
        let _on_the_network = own.runtime.enter();
        ask_here(&own.net, &caller, &question).expect("the person is asked here")
    };
    waiting.then(move |chosen| {
        let _ = given.send(chosen);
    });

    answer_the_notice(&mut sheet);
    sheet_shows(&mut sheet);
    let layout = sheet.layout(bounds).unwrap();
    assert!(sheet.click(layout.cancel.center(), &layout), "Deny answers");
    assert_eq!(answered(&answer), ConfirmationReply::Deny);

    drop(own);
    set_session_attended(false);
}

#[test]
#[serial_test::serial]
fn nobody_elses_device_asks_this_ones_person() {
    let root = key(1);
    let own = OwnDevice::start(&root);
    let question = question(&own.room);
    // A person whose root this device does not act for: their device's
    // question is not this device's person's to answer.
    let stranger = foreign_caller(&key(5));
    assert!(
        ask_here(&own.net, &stranger, &question).is_none(),
        "another person's question is not asked here"
    );
    drop(own);
}

#[test]
#[serial_test::serial]
fn a_room_this_device_does_not_hold_is_not_asked_about() {
    let root = key(1);
    let own = OwnDevice::start(&root);
    let elsewhere = question("11111111-2222-3333-4444-555555555555");
    let caller = foreign_caller(&root);
    assert!(
        ask_here(&own.net, &caller, &elsewhere).is_none(),
        "a room this device does not hold is nobody's to confirm"
    );
    drop(own);
}
