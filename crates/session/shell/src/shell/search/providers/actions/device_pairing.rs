//! `identity.device.pair.discover`, `.approve` and `.reject`: pairing a
//! nearby device through the palette, the control socket and agents.
//!
//! The steps run in [`crate::shell::device_pairing`], which the broker's
//! door for Settings shares, so a device discovered through one door is
//! approved or rejected through another, under one contract. Discovery and
//! approval answer through a pending ticket that the worker's completion
//! wakes; the loop never waits on the LAN or on the administrator's
//! authentication.
//!
//! Every step is refused unless the caller administers the active
//! workspace's identity; approval also needs explicit confirmation, and the
//! helper it runs asks polkit for an administrator.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::remote::{Pending, PendingPoll};

use crate::shell::chrome_state::Chrome;
use crate::shell::device_pairing::{self as engine, Answer, Pairing, Started};
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam};
use crate::shell::search::{ActionReceipt, InvocationDisposition, PendingInvocation};

pub(super) const DISCOVER_ID: &str = "identity.device.pair.discover";
pub(super) const APPROVE_ID: &str = "identity.device.pair.approve";
pub(super) const REJECT_ID: &str = "identity.device.pair.reject";

pub(super) const DISCOVER_PARAMS: &[ActionParam] = engine::DISCOVER_PARAMS;
pub(super) const APPROVE_PARAMS: &[ActionParam] = engine::APPROVE_PARAMS;
pub(super) const REJECT_PARAMS: &[ActionParam] = engine::REJECT_PARAMS;

/// The engine's step behind shell action `id`.
fn step(id: &str) -> Option<&'static str> {
    match id {
        DISCOVER_ID => Some(engine::DISCOVER),
        APPROVE_ID => Some(engine::APPROVE),
        REJECT_ID => Some(engine::REJECT),
        _ => None,
    }
}

/// Whether `id` is one of the pairing steps.
pub(super) fn is_pair_action(id: &str) -> bool {
    step(id).is_some()
}

/// The step's contract: the one the broker's door offers too.
pub(super) fn contract(id: &str) -> tilo_actions::ActionContract {
    engine::contract(step(id).unwrap_or(engine::REJECT))
}

/// Start step `id` for whoever invoked it from the active workspace: only
/// someone who administers that workspace's identity pairs a device.
pub(super) fn begin(
    pairing: &Pairing,
    chrome: &Chrome,
    id: &str,
    args: &ActionArgs,
) -> Result<InvocationDisposition, ActionError> {
    let step =
        step(id).ok_or_else(|| ActionError::Refused(format!("{id} is not a pairing step")))?;
    {
        let state = chrome.borrow();
        if !super::current_shell_action_authorized(&state, id) {
            return Err(ActionError::Refused(format!(
                "current workspace authority no longer permits {id}"
            )));
        }
        super::active_identity(&state)?;
    }
    match pairing.start(step, args).map_err(ActionError::Refused)? {
        Started::Answered(value) => Ok(InvocationDisposition::Accepted(
            ActionReceipt::completed_with(value),
        )),
        Started::Later(answer) => Ok(InvocationDisposition::Pending(ticket(answer))),
    }
}

/// A ticket the loop polls for `answer`, which wakes the loop when it
/// arrives from the worker.
pub(super) fn ticket(answer: Pending<Answer>) -> PendingInvocation {
    let answer = RefCell::new(crate::shell::search::invocation_wake::watch(answer));
    Rc::new(move || {
        let waker = crate::shell::search::invocation_wake::waker();
        match crate::shell::search::invocation_wake::poll(&mut answer.borrow_mut(), &waker) {
            PendingPoll::Ready(Ok(value)) => Ok(Some(ActionReceipt::completed_with(value))),
            PendingPoll::Ready(Err(reason)) => Err(ActionError::Refused(reason)),
            PendingPoll::Waiting => Ok(None),
            PendingPoll::Abandoned => Err(ActionError::Refused(
                "the host step stopped without an answer".into(),
            )),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::device_pairing::{Finished, HostCommand, PairRole, Runner};
    use crate::shell::search::action_api::ArgValue;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const KEY: &str = "cc00000000000000000000000000000000000000000000000000000000000003";
    const SERVICE: &str = "dd00000000000000000000000000000000000000000000000000000000000004";

    #[test]
    fn identity_device_pair_actions_are_listed_and_contracted() {
        for id in [DISCOVER_ID, APPROVE_ID, REJECT_ID] {
            assert!(is_pair_action(id));
            assert!(
                super::super::Action::ALL
                    .iter()
                    .any(|action| action.id == id),
                "{id} is in the catalogue"
            );
            let contract = super::super::shell_action_contract(id);
            assert_eq!(
                contract,
                self::contract(id),
                "{id} has its own contract arm"
            );
            assert_eq!(contract.kind, tilo_actions::ActionKind::Outside);
            assert!(
                contract
                    .tags
                    .iter()
                    .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY),
                "{id} runs only for the person at this device"
            );
        }
        let approve = super::super::shell_action_contract(APPROVE_ID);
        assert_eq!(approve.risk, tilo_actions::RiskLevel::Privileged);
        assert_eq!(
            approve.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            APPROVE_PARAMS
                .iter()
                .any(|param| param.name.as_ref() == "confirm"),
            "approval carries its confirmation"
        );
        assert_eq!(
            super::super::shell_action_contract(DISCOVER_ID).confirmation,
            tilo_actions::ConfirmationPolicy::None,
            "looking around asks nothing"
        );
        for (id, value) in [
            (
                DISCOVER_ID,
                serde_json::json!({"role": "home-peer", "device": KEY, "service": SERVICE,
                                   "label": "NAS", "verification_code": "1"}),
            ),
            (
                APPROVE_ID,
                serde_json::json!({"role": "home-peer", "device": KEY, "label": "NAS"}),
            ),
            (
                REJECT_ID,
                serde_json::json!({"device": KEY, "forgotten": true, "stopped": false}),
            ),
        ] {
            contract(id)
                .result
                .validate(&tilo_actions::ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                })
                .unwrap_or_else(|error| panic!("{id} answers its advertised result: {error:?}"));
        }
    }

    #[test]
    fn discover_answers_the_parsed_candidate_through_a_ticket_its_worker_wakes() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        let _installed = crate::shell::search::invocation_wake::install(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = std::sync::Mutex::new(gate);
        let runner: Runner = Arc::new(move |command: &HostCommand, _| {
            assert_eq!(
                command.program,
                std::path::Path::new("/usr/bin/tilo-enroll")
            );
            gate.lock().expect("gate").recv().expect("released");
            Ok(Finished {
                success: true,
                stdout: format!(
                    "{{\"device_node_id\":\"{KEY}\",\"relay_service_node_id\":\"{SERVICE}\",\
                     \"label\":\"Laptop\",\"verification_code\":\"042-917\"}}"
                )
                .into_bytes(),
                stderr: Vec::new(),
            })
        });
        let pairing = Pairing::with_runner(runner);
        let answer = pairing.discover(PairRole::PersonalDevice).expect("starts");
        let ticket = ticket(answer);
        assert!(
            matches!(ticket(), Ok(None)),
            "nothing is answered before the device is found"
        );
        let before = wakes.load(Ordering::SeqCst);
        release.send(()).expect("released");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while wakes.load(Ordering::SeqCst) == before {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker's answer wakes the loop"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let receipt = ticket().expect("answered").expect("ready once woken");
        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
            panic!("discovery succeeded");
        };
        assert_eq!(
            output.value,
            Some(serde_json::json!({
                "role": "personal-device",
                "device": KEY,
                "service": SERVICE,
                "label": "Laptop",
                "verification_code": "042-917",
            }))
        );
    }

    #[test]
    fn a_non_admin_is_refused_before_any_pairing_step_runs() {
        use crate::shell::state::FreshId as _;
        use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        let mut workspace = Workspace::new(id, "Someone else's workspace");
        workspace.set_owner("a-different-person");
        tree.desktop.workspaces.push(workspace);
        tree.desktop.active = id;
        let chrome =
            ChromeState::new_with_handle(crate::shell::state::WorkspaceTreeHandle::new(tree));
        let ran = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&ran);
        let pairing = Pairing::with_runner(Arc::new(move |_: &HostCommand, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(Finished::default())
        }));
        for (id, params, values) in [
            (
                DISCOVER_ID,
                DISCOVER_PARAMS,
                vec![("role", ArgValue::Text("home-peer".into()))],
            ),
            (
                APPROVE_ID,
                APPROVE_PARAMS,
                vec![
                    ("role", ArgValue::Text("home-peer".into())),
                    ("device", ArgValue::Text(KEY.into())),
                    ("service", ArgValue::Text(SERVICE.into())),
                    ("confirm", ArgValue::Boolean(true)),
                ],
            ),
            (
                REJECT_ID,
                REJECT_PARAMS,
                vec![("device", ArgValue::Text(KEY.into()))],
            ),
        ] {
            let args = ActionArgs::bind(
                params,
                values
                    .into_iter()
                    .map(|(name, value)| (name.to_string(), value)),
            )
            .expect("arguments bind");
            let refused =
                begin(&pairing, &chrome, id, &args).expect_err("a non-admin may not pair a device");
            // Refused by the workspace's authority over this Outside
            // action, or by the identity administration check behind it.
            assert!(
                matches!(&refused, ActionError::Refused(message)
                    if message.contains("may not administer devices")
                        || message.contains("no longer permits")),
                "unexpected refusal for {id}: {refused:?}"
            );
        }
        assert_eq!(ran.load(Ordering::SeqCst), 0, "nothing ran for a non-admin");
    }

    #[test]
    fn approval_arguments_are_checked_before_anything_runs() {
        let bind = |values: Vec<(&str, ArgValue)>| {
            ActionArgs::bind(
                APPROVE_PARAMS,
                values
                    .into_iter()
                    .map(|(name, value)| (name.to_string(), value)),
            )
            .expect("binds")
        };
        let base = || {
            vec![
                ("role", ArgValue::Text("home-peer".into())),
                ("device", ArgValue::Text(KEY.to_ascii_uppercase())),
                ("service", ArgValue::Text(SERVICE.into())),
                ("confirm", ArgValue::Boolean(true)),
            ]
        };
        let plain = engine::approval(&bind(base())).expect("valid");
        assert_eq!(plain.device, KEY);
        assert_eq!(plain.workspace_ids, None);
        let mut with_ids = base();
        with_ids.push((
            "workspace_ids",
            ArgValue::from_json(&serde_json::json!([3, 5])).expect("array"),
        ));
        assert_eq!(
            engine::approval(&bind(with_ids))
                .expect("valid")
                .workspace_ids,
            Some(vec![3, 5])
        );
        let mut negative = base();
        negative.push((
            "workspace_ids",
            ArgValue::from_json(&serde_json::json!([-1])).expect("array"),
        ));
        assert!(engine::approval(&bind(negative)).is_err());
        let mut short = base();
        short[1] = ("device", ArgValue::Text("abc".into()));
        assert!(engine::approval(&bind(short)).is_err());

        let mut unconfirmed = base();
        unconfirmed[3] = ("confirm", ArgValue::Boolean(false));
        let pairing = Pairing::with_runner(Arc::new(|_: &HostCommand, _| {
            panic!("an unconfirmed approval runs nothing")
        }));
        assert!(
            pairing.start(engine::APPROVE, &bind(unconfirmed)).is_err(),
            "approval needs its explicit confirmation"
        );
    }
}
