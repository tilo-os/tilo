//! An agent a relay runs for Ada asks before an action her grants leave to
//! her. Her laptop — a device holding her root, connected to the relay as
//! its host — tells her with a notification; its Answer shows the question
//! on its own confirmation sheet ("… wants to act for you", Allow once or
//! Deny) and the relay hands the agent the witnessed answer for that one
//! invocation.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, Confirmation, InvokeArgument, InvokeRequest,
    RemoteActionTarget,
};
use tilo_relay_protocol::delegation::{DelegateScope, Delegation};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::{HostIdentity, Reach, ShellNetwork};
use crate::shell::app_action_consent::set_session_attended;
use crate::shell::portal_dialog::{DialogView, PortalDialogController, PortalDialogQueue};
use crate::shell::state::WorkspaceTree;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

fn certify(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    caps: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: caps,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// Clearing the calculator's history, as the agent asks about it: the
/// record's own words and risk for it.
fn clear_history() -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: "record".into(),
            address: tilo_actions::door::ActionAddress::new("calculator.document", "clear-history"),
        },
        argument: InvokeArgument::None,
        context: Some(
            tilo_actions::ActionContext::for_resource(
                "calculator.document",
                "calculator.document",
                "Calculator",
            )
            .with_app("so.tilo.calculator", "so.tilo.calculator")
            .with_purpose("Remove every history entry")
            .with_facet(tilo_actions::ActionFacet::new(
                tilo_actions::RISK_FACET,
                tilo_actions::RiskLevel::Destructive.name(),
            )),
        ),
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
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

/// Ada answers the notice the question arrives as; until she does, no
/// sheet takes her screen.
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
            assert!(sheet.view().is_none(), "the agent never raises the sheet");
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
    answer: &mpsc::Receiver<Result<Confirmation, ActionHostError>>,
) -> Result<Confirmation, ActionHostError> {
    answer
        .recv_timeout(Duration::from_secs(30))
        .expect("the relay hands the agent the person's answer")
}

#[test]
#[serial_test::serial]
fn the_persons_device_answers_a_relay_agents_question_on_its_sheet() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = key(1);
    let node_key = key(2);
    let node = certify(&root, &node_key, "Home server", DeviceCapabilities::host());
    let relay = runtime
        .block_on(tilo_relay::daemon::Relay::start(
            tilo_relay::RelayConfig {
                state_root: dir.path().join("relay"),
                ..tilo_relay::RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: node_key,
                device_cert: node,
                revocations: RevocationList::new(),
            },
        ))
        .unwrap();
    home.env().set(
        "TILO_COLLAB_HOST",
        serde_json::to_string(&relay.node_addr()).unwrap(),
    );

    // The agent the relay runs for Ada, and the relay's side of asking her.
    let agent_key = key(3);
    let now = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &root,
        agent_key.verifying_key().to_bytes(),
        tilo_relay_protocol::delegation::Delegate::Agent {
            agent_id: "careful".into(),
        },
        "Careful agent",
        [0x7c, 0x3a, 0xed],
        DelegateScope {
            rooms: vec![ROOM.into()],
            apps: vec!["so.tilo.calculator".into()],
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    let witness = {
        let _runtime = runtime.enter();
        tilo_agent_participant::AgentHostLink::witness(&relay.agent_link(), &delegation)
            .expect("a relay witnesses for its agents")
    };
    let agent = Caller::delegated_agent(
        "careful",
        root.verifying_key().to_bytes(),
        agent_key.verifying_key().to_bytes(),
    )
    .in_room(ROOM.to_owned());
    let ask = || {
        let (given, answer) = mpsc::channel();
        witness
            .confirm(&agent, &clear_history())
            .then(move |result| {
                let _ = given.send(result.unwrap_or(Err(ActionHostError::ConfirmationRequired)));
            });
        answer
    };

    // Ada's laptop holds her root and connects to the relay as its host.
    set_session_attended(true);
    let session_dir = dir.path().join("session");
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(session_dir.join("state.json"));
    let _net = ShellNetwork::spawn_with(
        tree.shared(),
        &session_dir.join("device-private/keypair.bin"),
        &session_dir.join("peer-sync"),
        Reach::Loopback,
        move |secret, _| {
            let key = SigningKey::from_bytes(&secret.to_bytes());
            let cert = certify(&root, &key, "Ada's laptop", DeviceCapabilities::primary());
            Some(HostIdentity {
                device_cert: cert,
                device_signing_key: key,
                workspace_device_certs: std::collections::HashMap::new(),
                workspace_revocations: std::collections::HashMap::new(),
                identity_paths: None,
            })
        },
    )
    .unwrap();
    let mut sheet = PortalDialogController::new(PortalDialogQueue::global());
    let bounds = vello::kurbo::Rect::new(0.0, 0.0, 1600.0, 900.0);

    // Asked: the laptop shows the question on its confirmation sheet, and
    // Allow once is the only allowing answer.
    let answer = ask();
    answer_the_notice(&mut sheet);
    let view = sheet_shows(&mut sheet);
    assert!(
        view.title().contains("Careful agent wants to act for you"),
        "{view:?}"
    );
    assert_eq!(view.app_name(), "Calculator · Remove every history entry");
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
    assert!(
        matches!(answered(&answer), Ok(Confirmation::Witnessed(_))),
        "allowed, the agent holds the person's word for this one call"
    );

    // Asked again, it is a new question; denied, the agent is refused.
    let answer = ask();
    answer_the_notice(&mut sheet);
    sheet_shows(&mut sheet);
    let layout = sheet.layout(bounds).unwrap();
    assert!(sheet.click(layout.cancel.center(), &layout), "Deny answers");
    assert_eq!(answered(&answer), Err(ActionHostError::NotPermitted));

    set_session_attended(false);
    runtime.block_on(relay.shutdown());
}
