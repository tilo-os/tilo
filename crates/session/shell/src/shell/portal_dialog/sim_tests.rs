//! The SIM's PIN and PUK, asked in the network secrets sheet: the PIN
//! goes back to NetworkManager, the PUK and a new PIN to the modem.

use super::tests::type_key;
use super::*;
use ui::widget::{FieldRole, SecretKind};

fn sim_request(
    handle: &str,
    step: SimUnlockStep,
    attempts_left: Option<u32>,
) -> NetworkSecretsRequest {
    NetworkSecretsRequest {
        handle: handle.into(),
        title: "SIM PIN required".into(),
        detail: "Enter the PIN to unlock the SIM for “Carrier”.".into(),
        fields: vec![SecretFieldSpec {
            key: "pin".into(),
            label: "SIM PIN".into(),
            masked: true,
            required: true,
            initial: String::new(),
            rule: SecretFieldRule::Any,
        }],
        remember: None,
        sim: Some(SimUnlock {
            step,
            attempts_left,
            sim_path: Some("/org/freedesktop/ModemManager1/SIM/0".into()),
        }),
    }
}

#[test]
fn a_sim_pin_is_asked_with_the_pin_prompt_and_answers_only_digits() {
    use winit::keyboard::Key;

    let queue = PortalDialogQueue::default();
    let receiver = queue
        .enqueue_network_secrets(sim_request("nm:sim", SimUnlockStep::Pin, Some(2)))
        .expect("queued");
    let mut controller = PortalDialogController::new(queue);
    assert!(controller.pump());
    let prompt = controller.prompt().expect("the PIN prompt");
    assert_eq!(
        prompt.kind(),
        SecretKind::SimPin {
            attempts_left: Some(2)
        }
    );
    // The SIM keeps its PIN: nothing offers to remember it.
    assert_eq!(prompt.option_checked(), None);
    let notice = prompt
        .notice_line()
        .map(|(line, _)| line)
        .unwrap_or_default();
    assert!(notice.contains('2'), "the attempts left are said: {notice}");
    assert_eq!(
        controller.view().map(|view| view.confirm_label()),
        Some(t("portal-sim-unlock"))
    );
    let state = controller.state_json();
    assert_eq!(state["sim"]["step"], "pin");
    assert_eq!(state["sim"]["attempts_left"], 2);
    assert_eq!(state["remember"], serde_json::Value::Null);
    for key in ["1", "2", "x", "3", "4"] {
        assert!(type_key(&mut controller, Key::Character(key.into())));
    }
    assert!(controller.confirm());
    match receiver.blocking_recv() {
        Ok(NetworkSecretsDecision::Answered { values, remember }) => {
            assert!(!remember);
            assert_eq!(
                values
                    .iter()
                    .map(|value| value.as_str())
                    .collect::<Vec<_>>(),
                ["1234"]
            );
        }
        other => panic!("expected the PIN, got {other:?}"),
    }
}

#[test]
fn a_pin_networkmanager_asks_again_for_is_said_wrong_in_place() {
    let queue = PortalDialogQueue::default();
    let mut request = sim_request("nm:sim-again", SimUnlockStep::Pin, Some(1));
    request.detail = format!("{}\n{}", t("portal-network-retry"), request.detail);
    let _receiver = queue.enqueue_network_secrets(request).expect("queued");
    let mut controller = PortalDialogController::new(queue);
    assert!(controller.pump());
    let prompt = controller.prompt().expect("the PIN prompt");
    assert_eq!(prompt.error(), Some(t("portal-sim-pin-wrong").as_str()));
    assert_eq!(prompt.focused_field(), Some(FieldRole::Pin));
    assert_eq!(
        prompt.resource(),
        "Enter the PIN to unlock the SIM for “Carrier”."
    );
}

/// Answers PUKs from a script and keeps what it was handed.
#[derive(Default)]
struct ScriptedUnlocker {
    answers: std::sync::Mutex<VecDeque<Result<(), PukRefusal>>>,
    sent: std::sync::Mutex<Vec<(String, String, String)>>,
}

impl SimUnlocker for ScriptedUnlocker {
    fn send_puk(
        &self,
        sim_path: &str,
        puk: zeroize::Zeroizing<String>,
        new_pin: zeroize::Zeroizing<String>,
    ) -> Result<(), PukRefusal> {
        self.sent.lock().expect("sent").push((
            sim_path.to_owned(),
            puk.as_str().to_owned(),
            new_pin.as_str().to_owned(),
        ));
        self.answers
            .lock()
            .expect("answers")
            .pop_front()
            .unwrap_or(Ok(()))
    }
}

fn fill_puk(controller: &mut PortalDialogController, puk: &str, pin: &str, confirm: &str) {
    let prompt = controller.prompt_mut().expect("the PUK prompt");
    for (role, value) in [
        (FieldRole::Puk, puk),
        (FieldRole::NewPin, pin),
        (FieldRole::ConfirmPin, confirm),
    ] {
        prompt.field_mut(role).expect("field").set_text(value);
    }
}

#[test]
fn a_sim_locked_to_its_puk_is_unlocked_with_the_puk_and_a_new_pin() {
    let queue = PortalDialogQueue::default();
    let mut receiver = queue
        .enqueue_network_secrets(sim_request("nm:puk", SimUnlockStep::Puk, Some(10)))
        .expect("queued");
    let unlocker = Arc::new(ScriptedUnlocker::default());
    unlocker
        .answers
        .lock()
        .expect("answers")
        .push_back(Err(PukRefusal::Wrong {
            attempts_left: Some(9),
        }));
    let mut controller = PortalDialogController::new(queue)
        .with_sim_unlocker(Arc::clone(&unlocker) as Arc<dyn SimUnlocker>);
    assert!(controller.pump());
    let prompt = controller.prompt().expect("the PUK prompt");
    assert_eq!(
        prompt.kind(),
        SecretKind::SimPuk {
            attempts_left: Some(10)
        }
    );
    assert_eq!(
        prompt.fields().map(|(role, _)| role).collect::<Vec<_>>(),
        [FieldRole::Puk, FieldRole::NewPin, FieldRole::ConfirmPin]
    );
    assert_eq!(prompt.focused_field(), Some(FieldRole::Puk));
    assert_eq!(controller.state_json()["sim"]["step"], "puk");

    // New PINs that differ never reach the modem.
    fill_puk(&mut controller, "12345678", "4321", "4322");
    assert!(!controller.confirm());
    assert!(!controller.wait_for_puk_attempt());
    assert!(unlocker.sent.lock().expect("sent").is_empty());

    // A wrong PUK: the sheet stays, says so on the PUK and counts down.
    fill_puk(&mut controller, "12345678", "4321", "4321");
    assert!(!controller.confirm(), "the sheet waits for the modem");
    assert!(
        controller.state_json()["sim"]["busy"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(controller.wait_for_puk_attempt());
    let prompt = controller.prompt().expect("still asking");
    assert_eq!(prompt.error(), Some(t("portal-sim-puk-wrong").as_str()));
    assert_eq!(prompt.focused_field(), Some(FieldRole::Puk));
    assert_eq!(
        prompt.kind(),
        SecretKind::SimPuk {
            attempts_left: Some(9)
        }
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    // The right one unlocks the SIM and closes the sheet.
    fill_puk(&mut controller, "87654321", "4321", "4321");
    assert!(!controller.confirm());
    assert!(controller.wait_for_puk_attempt());
    assert!(controller.view().is_none(), "the sheet closed");
    assert!(matches!(
        receiver.blocking_recv(),
        Ok(NetworkSecretsDecision::UnlockedWithPuk)
    ));
    assert_eq!(
        unlocker.sent.lock().expect("sent").as_slice(),
        [
            (
                "/org/freedesktop/ModemManager1/SIM/0".to_owned(),
                "12345678".to_owned(),
                "4321".to_owned()
            ),
            (
                "/org/freedesktop/ModemManager1/SIM/0".to_owned(),
                "87654321".to_owned(),
                "4321".to_owned()
            ),
        ]
    );
}
