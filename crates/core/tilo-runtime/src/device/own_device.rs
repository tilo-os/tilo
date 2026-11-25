//! An application's action on another of the person's own devices, carried from this daemon.
//!
//! The broker admits an own-device invocation for the person at this device
//! ([`OwnDeviceActions`]); this is where the other device is reached: the request is sent there
//! as an ordinary invocation over the device action service (`tilo-actions/1`), under the
//! original invocation's id and signed by this device for its person, and followed until that
//! device answers, so the app observes the outcome as its own invocation's.
//!
//! A request from another of the person's devices is them using this computer: it holds suspend
//! off for a lease after each, so nothing falls asleep mid-task while they are away from it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::{EndpointAddr, EndpointId};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest, Pending, own_device, pending};
use tilo_actions::{ActionFailure, InvocationUpdate};
use tilo_broker_core::services::{OwnDeviceActions, OwnDeviceOutcome};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::FOLLOW_DEADLINE;

use super::workspaces::ServiceWorkspaces;

/// How long one request from another of the person's devices keeps this machine from
/// idle-suspending.
const REMOTE_USE_LEASE: Duration = Duration::from_secs(600);

/// This device's own-device actions, carried over the one action service its workspaces run.
pub(crate) struct AnotherDevice {
    workspaces: Arc<ServiceWorkspaces>,
    remote_use: RemoteUse,
}

impl AnotherDevice {
    /// The person's own-device actions on `workspaces`: the devices are reached over the action
    /// service the daemon already serves, and each request from another of their devices holds
    /// this machine awake for a lease.
    pub(crate) fn over(workspaces: &Arc<ServiceWorkspaces>) -> Self {
        Self {
            workspaces: Arc::clone(workspaces),
            remote_use: RemoteUse::default(),
        }
    }

    /// Where the device `device` of the person's is reached: what its key resolves to on this
    /// device's network.
    fn addr(device: [u8; 32]) -> Result<EndpointAddr, ActionCallError> {
        EndpointId::from_bytes(&device)
            .map(EndpointAddr::new)
            .map_err(|_| ActionCallError::InvalidRequest {
                reason: "that is not a device key".into(),
            })
    }
}

impl OwnDeviceActions for AnotherDevice {
    fn invoke(
        &self,
        caller: &Caller,
        checked: &own_device::Checked,
        request: &InvokeRequest,
    ) -> Result<Pending<OwnDeviceOutcome>, ActionCallError> {
        // A request from another of the person's devices is them using this one.
        self.remote_use
            .note(caller, self.workspaces.device_cert().user_public_key);
        // The workspace the two devices are to share: the one the app acts in, which is where
        // the other device decides whether this one may act at all.
        let Some(workspace) = caller.workspace() else {
            return Err(ActionCallError::NotPermitted);
        };
        let addr = Self::addr(checked.device)?;
        let actions = self.workspaces.copies().action_service();
        let device = Arc::new(actions.remote(&self.workspaces.endpoint(), &addr, workspace));
        // The device signs for its person, whose call this is: the other device decides it as
        // theirs from the signature, and never meets a forward it cannot check.
        let mut forwarded = checked.forwarded(workspace, request);
        let signed = tilo_server_protocol::invocation::sign_invocation_now(
            workspace,
            &forwarded,
            &self.workspaces.device_cert(),
            self.workspaces.signer().as_ref(),
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| ActionCallError::Unavailable {
            reason: format!("the call could not be signed for its person: {error}"),
        })?;
        forwarded.signed = Some(Box::new(signed));
        let (complete, outcome) = pending();
        device
            .invoke_until_settled(forwarded, FOLLOW_DEADLINE)
            .then(move |answer| {
                complete.complete(outcome_of(answer));
            });
        Ok(outcome)
    }
}

/// What the other device's answer comes to: the outcome the app on this device observes as its
/// own invocation's.
fn outcome_of(answer: Option<Result<InvocationUpdate, ActionCallError>>) -> OwnDeviceOutcome {
    let failure = |code: &str, message: String| {
        Box::new(ActionFailure {
            code: code.to_owned(),
            message,
            details: None,
            retryable: true,
        })
    };
    match answer {
        Some(Ok(InvocationUpdate::Succeeded { output, .. })) => Ok(output),
        Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(Box::new(error)),
        Some(Ok(InvocationUpdate::Cancelled { message, .. })) => Err(failure(
            "cancelled",
            message.unwrap_or_else(|| "the action was cancelled on that device".into()),
        )),
        // Following ends on a terminal update: anything else is a device answering out of turn.
        Some(Ok(update)) => Err(failure(
            "unsettled",
            format!("the action on that device did not settle: {update:?}"),
        )),
        Some(Err(error)) => Err(failure("own_device_refused", error.to_string())),
        None => Err(failure(
            "own_device_abandoned",
            "the device's answer was dropped before it arrived".into(),
        )),
    }
}

/// A request from another of the person's own devices is use of this machine: suspend is held off
/// for a lease after it, and renewed in place while they keep asking.
///
/// A lease is held the way the machine understands one: on Linux, as the daemon's program holds
/// suspend off ([`super::capabilities::Capabilities::stay_awake`]), and nothing on a machine that
/// suspends another way.
#[derive(Clone, Default)]
pub(crate) struct RemoteUse(Arc<Mutex<Option<Lease>>>);

#[derive(Clone, Copy)]
struct Lease {
    #[cfg_attr(
        not(any(test, target_os = "linux")),
        expect(
            dead_code,
            reason = "only Linux holds suspend off until a lease ends; elsewhere a lease is only noted"
        )
    )]
    until: Instant,
}

impl RemoteUse {
    /// A request from another of the person's own devices is use of this one: take or renew the
    /// lease.
    fn note(&self, caller: &Caller, user: [u8; 32]) {
        if caller.is_foreign_device()
            || caller.user_public_key() != Some(&user)
            || caller.device_public_key().is_none()
        {
            return;
        }
        let Some(until) = Instant::now().checked_add(REMOTE_USE_LEASE) else {
            return;
        };
        let mut lease = self.0.lock_recover();
        let was_held = lease.is_some();
        *lease = Some(Lease { until });
        drop(lease);
        if !was_held {
            hold(self.clone());
        }
    }

    /// When the lease taken now ends, while one is held.
    #[cfg(test)]
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.0.lock_recover().map(|lease| lease.until)
    }

    /// Whether a lease is held now.
    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn held(&self) -> bool {
        self.0.lock_recover().is_some()
    }

    /// The lease `lease` holds now, dropped once it ended.
    #[cfg(target_os = "linux")]
    fn take_ended(&self) -> Option<Lease> {
        let mut held = self.0.lock_recover();
        let lease = (*held)?;
        if Instant::now() >= lease.until {
            *held = None;
            drop(held);
            return Some(lease);
        }
        None
    }
}

/// Keep this machine awake while the lease holds, and let it sleep once it ended: one hold at a
/// time, renewed in place, dropped at its deadline.
#[cfg(target_os = "linux")]
fn hold(remote: RemoteUse) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(async move {
        // The hold itself, kept alive while the lease is; re-taken if nothing could hold the
        // machine when a request first arrived.
        let mut inhibitor: Option<super::capabilities::Awake> = None;
        loop {
            let lease = {
                let held = remote.0.lock_recover();
                held.as_ref().copied()
            };
            let Some(lease) = lease else {
                return;
            };
            if inhibitor.is_none() {
                inhibitor = super::capabilities::stay_awake().await;
            }
            let now = Instant::now();
            if now >= lease.until {
                if remote.take_ended().is_some() {
                    inhibitor = None;
                }
                if !remote.held() {
                    return;
                }
                continue;
            }
            tokio::time::sleep(lease.until.saturating_duration_since(now)).await;
        }
    });
}

/// A machine that suspends another way keeps no lease: nothing here can hold it.
#[cfg(not(target_os = "linux"))]
fn hold(remote: RemoteUse) {
    drop(remote);
}

#[cfg(test)]
mod tests {
    //! A request from another of the person's own devices holds suspend off for a lease, renewed
    //! in place, and nothing else does.

    use std::time::Duration;

    use super::{REMOTE_USE_LEASE, RemoteUse};
    use tilo_actions::remote::Caller;

    /// When a lease taken at `now` lapses.
    fn lease_ends(now: std::time::Instant) -> std::time::Instant {
        now.checked_add(REMOTE_USE_LEASE).expect("a lease deadline")
    }

    #[test]
    fn a_request_from_another_of_the_persons_devices_takes_a_lease_and_renews_it() {
        let user = [0x51; 32];
        let remote = RemoteUse::default();
        let theirs = Caller::authenticated(user, [0x53; 32]);
        let now = std::time::Instant::now();
        remote.note(&theirs, user);
        let first = remote
            .deadline()
            .expect("the person using this machine from elsewhere holds it");
        assert!(first >= lease_ends(now), "the lease is long enough");
        assert!(
            first
                <= lease_ends(now)
                    .checked_add(Duration::from_secs(1))
                    .expect("just after the lease"),
            "and no longer than one lease"
        );

        // The next request through the same device renews that one lease.
        std::thread::sleep(Duration::from_millis(5));
        let later = std::time::Instant::now();
        remote.note(&theirs, user);
        let renewed = remote.deadline().expect("still held");
        assert!(
            renewed >= lease_ends(later),
            "the renewal counts from the later request"
        );
        assert!(renewed > first, "one lease, moved forward");
    }

    /// A failure this device names for an answer of the other device's that brought no outcome of
    /// the action's own: one the app may try again.
    fn ours(code: &str, message: &str) -> super::OwnDeviceOutcome {
        Err(Box::new(tilo_actions::ActionFailure {
            code: code.to_owned(),
            message: message.to_owned(),
            details: None,
            retryable: true,
        }))
    }

    /// What the app observes for each way the other device's answer can end: the action's output
    /// or its own failure exactly as that device gave them, and a failure this device names for an
    /// answer that was cancelled, came out of turn, was refused, or never arrived.
    #[test]
    fn every_answer_of_the_other_device_is_the_outcome_the_app_observes() {
        use tilo_actions::remote::ActionCallError;
        use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};

        let invocation = InvocationId::new();
        let output = ActionOutput {
            value: Some(serde_json::json!({"appended": 3})),
            resources: vec!["notes.note:1".to_owned()],
        };
        let theirs = ActionFailure {
            code: "note_locked".to_owned(),
            message: "the note is locked there".to_owned(),
            details: Some(serde_json::json!({"note": "1"})),
            retryable: false,
        };
        let stopped = Some("the person stopped it".to_owned());
        let accepted = InvocationUpdate::Accepted { invocation };
        let item = InvocationUpdate::Item {
            invocation,
            sequence: 1,
            value: serde_json::json!("a line"),
        };
        let unavailable = ActionCallError::Unavailable {
            reason: "this device is not serving its actions".to_owned(),
        };
        let rows = [
            (
                "succeeded",
                Some(Ok(InvocationUpdate::Succeeded {
                    invocation,
                    output: output.clone(),
                })),
                Ok(output),
            ),
            (
                "failed there",
                Some(Ok(InvocationUpdate::Failed {
                    invocation,
                    error: theirs.clone(),
                })),
                Err(Box::new(theirs)),
            ),
            (
                "cancelled with a word",
                Some(Ok(InvocationUpdate::Cancelled {
                    invocation,
                    message: stopped,
                })),
                ours("cancelled", "the person stopped it"),
            ),
            (
                "cancelled without one",
                Some(Ok(InvocationUpdate::Cancelled {
                    invocation,
                    message: None,
                })),
                ours("cancelled", "the action was cancelled on that device"),
            ),
            (
                "accepted, and no more",
                Some(Ok(accepted.clone())),
                ours(
                    "unsettled",
                    &format!("the action on that device did not settle: {accepted:?}"),
                ),
            ),
            (
                "an item, and no more",
                Some(Ok(item.clone())),
                ours(
                    "unsettled",
                    &format!("the action on that device did not settle: {item:?}"),
                ),
            ),
            (
                "not permitted there",
                Some(Err(ActionCallError::NotPermitted)),
                ours(
                    "own_device_refused",
                    "the caller may not invoke this action",
                ),
            ),
            (
                "not serving there",
                Some(Err(unavailable)),
                ours(
                    "own_device_refused",
                    "remote actions are unavailable: this device is not serving its actions",
                ),
            ),
            (
                "never answered",
                None,
                ours(
                    "own_device_abandoned",
                    "the device's answer was dropped before it arrived",
                ),
            ),
        ];
        for (row, answer, expected) in rows {
            assert_eq!(super::outcome_of(answer), expected, "{row}");
        }
    }

    #[test]
    fn only_the_person_using_this_machine_from_another_of_their_devices_holds_it() {
        let user = [0x51; 32];
        for (what, caller) in [
            (
                "someone else's device",
                Caller::authenticated([0x54; 32], [0x55; 32]),
            ),
            (
                "a foreign device",
                Caller::authenticated(user, [0x56; 32]).from_foreign_device(true),
            ),
            ("a caller on this device", Caller::local_person("person")),
        ] {
            let remote = RemoteUse::default();
            remote.note(&caller, user);
            assert!(
                !remote.held(),
                "{what} is not the person using this machine"
            );
            assert!(remote.deadline().is_none());
        }
    }
}
