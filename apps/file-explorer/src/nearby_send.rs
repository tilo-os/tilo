//! Files-side state for the shell-owned Nearby Send service.
//!
//! This module owns no transport and never reads a file. It emits bounded
//! commands onto the toolkit's already-authenticated collaboration lane, then
//! projects the shell's certified-device/status reply into app-local picker,
//! consent and progress state. The shell remains responsible for validating
//! paths, consent and the existing resumable transfer.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use ui::time::{Duration, Instant};

use ui::collaboration::{
    CollabEvent, CollabRequest, NearbyDestinationHandle, NearbyTargetHandle, NearbyTransferCommand,
    NearbyTransferDestination, NearbyTransferHandle, NearbyTransferPhase, NearbyTransferReply,
    NearbyTransferSnapshot, NearbyTransferStatus, NearbyTransferTarget,
};

const APP_ID: &str = "so.tilo.file-explorer";
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_VISIBLE_HISTORY: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Pending {
    Snapshot { open_picker: bool },
    Send,
    Decide,
    Cancel,
    Retry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NearbyUiEffect {
    OpenPicker(Vec<NearbyTransferTarget>),
    Offer {
        transfer: NearbyTransferStatus,
        destinations: Vec<NearbyTransferDestination>,
    },
    Failed(NearbyTransferStatus),
    Status(String),
    SendResult(Result<NearbyTransferHandle, String>),
}

pub struct NearbySendState {
    next_request_id: u64,
    pending: BTreeMap<u64, Pending>,
    requests: VecDeque<CollabRequest>,
    snapshot: NearbyTransferSnapshot,
    next_poll: Instant,
    shown_offer: Option<NearbyTransferHandle>,
    observed_phases: BTreeMap<NearbyTransferHandle, NearbyTransferPhase>,
    history: VecDeque<NearbyTransferStatus>,
}

impl Default for NearbySendState {
    fn default() -> Self {
        Self {
            next_request_id: 1,
            pending: BTreeMap::new(),
            requests: VecDeque::new(),
            snapshot: NearbyTransferSnapshot::default(),
            next_poll: ui::time::now(),
            shown_offer: None,
            observed_phases: BTreeMap::new(),
            history: VecDeque::new(),
        }
    }
}

impl NearbySendState {
    fn queue(&mut self, command: NearbyTransferCommand, pending: Pending) {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        self.pending.insert(request_id, pending);
        self.requests
            .push_back(CollabRequest::NearbyTransferCommand {
                request_id,
                app_id: APP_ID.into(),
                command,
            });
    }

    pub fn open_picker(&mut self) {
        self.queue(
            NearbyTransferCommand::Snapshot,
            Pending::Snapshot { open_picker: true },
        );
    }

    /// Queue an exact Unix path for the shell-owned transfer service.
    ///
    /// # Errors
    /// Hosts whose paths are not Unix byte strings cannot represent local
    /// paths in this service protocol; nothing is queued.
    pub fn send(&mut self, path: &Path, target: NearbyTargetHandle) -> Result<(), String> {
        let Some(source_path) = host_fs::names::unix_path_bytes(path) else {
            return Err(crate::i18n::t("files-share-not-available"));
        };
        self.queue(
            NearbyTransferCommand::Send {
                target,
                source_path: source_path.to_vec(),
            },
            Pending::Send,
        );
        Ok(())
    }

    pub fn decide(
        &mut self,
        handle: NearbyTransferHandle,
        destination: Option<NearbyDestinationHandle>,
        remember_for_session: bool,
    ) {
        self.shown_offer = None;
        self.queue(
            NearbyTransferCommand::Decide {
                handle,
                accept: destination.is_some(),
                destination,
                remember_for_session,
            },
            Pending::Decide,
        );
    }

    pub fn decline(&mut self, handle: NearbyTransferHandle) {
        self.decide(handle, None, false);
    }

    pub fn destinations(&self) -> &[NearbyTransferDestination] {
        &self.snapshot.destinations
    }

    pub fn history(&self) -> &VecDeque<NearbyTransferStatus> {
        &self.history
    }

    pub fn cancel(&mut self, handle: NearbyTransferHandle) {
        self.queue(NearbyTransferCommand::Cancel { handle }, Pending::Cancel);
    }

    pub fn retry(&mut self, handle: NearbyTransferHandle) {
        self.queue(NearbyTransferCommand::Retry { handle }, Pending::Retry);
    }

    pub fn poll_if_due(&mut self, now: Instant) {
        if now < self.next_poll
            || self
                .pending
                .values()
                .any(|pending| matches!(pending, Pending::Snapshot { open_picker: false }))
        {
            return;
        }
        self.next_poll = now + POLL_INTERVAL;
        self.queue(
            NearbyTransferCommand::Snapshot,
            Pending::Snapshot { open_picker: false },
        );
    }

    pub fn next_poll(&self) -> Instant {
        self.next_poll
    }

    pub fn targets(&self) -> &[NearbyTransferTarget] {
        &self.snapshot.targets
    }

    /// Path-free, broker-issued transfer projections used to publish the
    /// same receiver decisions through Files' semantic action door.
    pub fn transfers(&self) -> &[NearbyTransferStatus] {
        &self.snapshot.transfers
    }

    #[cfg(test)]
    pub(crate) fn install_snapshot_for_test(&mut self, snapshot: NearbyTransferSnapshot) {
        self.snapshot = snapshot;
    }

    pub fn take_requests(&mut self) -> Vec<CollabRequest> {
        self.requests.drain(..).collect()
    }

    pub fn handle_event(&mut self, event: &CollabEvent) -> Vec<NearbyUiEffect> {
        let CollabEvent::NearbyTransferCommandResult { request_id, result } = event else {
            return Vec::new();
        };
        let Some(pending) = self.pending.remove(request_id) else {
            return Vec::new();
        };
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                if pending == Pending::Send {
                    return vec![
                        NearbyUiEffect::SendResult(Err(error.clone())),
                        NearbyUiEffect::Status(format!("Nearby Send unavailable: {error}")),
                    ];
                }
                return vec![NearbyUiEffect::Status(format!(
                    "Nearby Send unavailable: {error}"
                ))];
            }
        };
        self.handle_reply(pending, reply)
    }

    fn handle_reply(
        &mut self,
        pending: Pending,
        reply: &NearbyTransferReply,
    ) -> Vec<NearbyUiEffect> {
        match (pending, reply) {
            (Pending::Snapshot { open_picker }, NearbyTransferReply::Snapshot { snapshot }) => {
                self.apply_snapshot(snapshot.clone(), open_picker)
            }
            (Pending::Send, NearbyTransferReply::Queued { handle }) => {
                vec![
                    NearbyUiEffect::SendResult(Ok(*handle)),
                    NearbyUiEffect::Status("Nearby Send queued securely".into()),
                ]
            }
            (Pending::Decide | Pending::Cancel | Pending::Retry, NearbyTransferReply::Accepted) => {
                Vec::new()
            }
            (Pending::Send, NearbyTransferReply::Rejected { reason }) => vec![
                NearbyUiEffect::SendResult(Err(reason.clone())),
                NearbyUiEffect::Status(format!("Nearby Send: {reason}")),
            ],
            (_, NearbyTransferReply::Rejected { reason }) => {
                vec![NearbyUiEffect::Status(format!("Nearby Send: {reason}"))]
            }
            _ => vec![NearbyUiEffect::Status(
                "Nearby Send returned an unexpected response".into(),
            )],
        }
    }

    fn apply_snapshot(
        &mut self,
        snapshot: NearbyTransferSnapshot,
        open_picker: bool,
    ) -> Vec<NearbyUiEffect> {
        // Background Snapshot polls keep the 2s wake. An identical roster
        // must not look like a frame: the runner used to commit every reply.
        if !open_picker && snapshot == self.snapshot {
            return Vec::new();
        }
        self.snapshot = snapshot;
        self.observed_phases.retain(|handle, _| {
            self.snapshot
                .transfers
                .iter()
                .any(|transfer| transfer.handle == *handle)
        });
        if self.shown_offer.is_some_and(|handle| {
            !self
                .snapshot
                .transfers
                .iter()
                .any(|transfer| transfer.handle == handle)
        }) {
            self.shown_offer = None;
        }
        let mut effects = Vec::new();
        if open_picker {
            effects.push(NearbyUiEffect::OpenPicker(self.snapshot.targets.clone()));
        }
        for transfer in self.snapshot.transfers.clone() {
            self.append_transfer_effects(&transfer, &mut effects);
        }
        effects
    }

    fn append_transfer_effects(
        &mut self,
        transfer: &NearbyTransferStatus,
        effects: &mut Vec<NearbyUiEffect>,
    ) {
        if matches!(
            transfer.phase,
            NearbyTransferPhase::AwaitingConsent | NearbyTransferPhase::AwaitingDestination
        ) && self.shown_offer != Some(transfer.handle)
        {
            self.shown_offer = Some(transfer.handle);
            effects.push(NearbyUiEffect::Offer {
                transfer: transfer.clone(),
                destinations: self.snapshot.destinations.clone(),
            });
        }
        let old = self.observed_phases.insert(transfer.handle, transfer.phase);
        if old == Some(transfer.phase) {
            return;
        }
        if matches!(
            transfer.phase,
            NearbyTransferPhase::Completed
                | NearbyTransferPhase::Failed
                | NearbyTransferPhase::Cancelled
        ) {
            self.history.retain(|entry| entry.handle != transfer.handle);
            self.history.push_front(transfer.clone());
            self.history.truncate(MAX_VISIBLE_HISTORY);
        }
        let message = match transfer.phase {
            NearbyTransferPhase::Queued => Some(format!(
                "Queued {} for {}",
                transfer.name, transfer.device_label
            )),
            NearbyTransferPhase::Transferring => {
                let percent = transfer
                    .transferred_bytes
                    .saturating_mul(100)
                    .checked_div(transfer.total_bytes)
                    .unwrap_or(0);
                Some(format!("Sending {} ({percent}%)", transfer.name))
            }
            NearbyTransferPhase::Completed => {
                Some(format!("Nearby Send completed: {}", transfer.name))
            }
            NearbyTransferPhase::Failed => {
                effects.push(NearbyUiEffect::Failed(transfer.clone()));
                Some(format!(
                    "Nearby Send failed: {}. Retry is available",
                    transfer.name
                ))
            }
            NearbyTransferPhase::Cancelled => {
                Some(format!("Nearby Send cancelled: {}", transfer.name))
            }
            NearbyTransferPhase::AwaitingConsent | NearbyTransferPhase::AwaitingDestination => None,
        };
        if let Some(message) = message {
            effects.push(NearbyUiEffect::Status(message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::collaboration::{NearbyTransferDirection, NearbyTransferPhase, NearbyTransferReply};

    /// Picker and receiver consent follow typed broker results and the receiver decision uses only opaque destination authority.
    #[test]
    fn nearby_send_consent_is_driven_by_typed_opaque_broker_results() {
        // --- phase: picker_and_receiver_consent_are_driven_by_typed_broker_results ---
        {
            let mut state = NearbySendState::default();
            state.open_picker();
            let request = state.take_requests().pop().expect("snapshot request");
            let CollabRequest::NearbyTransferCommand { request_id, .. } = request else {
                panic!("wrong request");
            };
            let offer = NearbyTransferStatus {
                handle: NearbyTransferHandle::from_issued_bytes([8; 16]),
                name: "Photos".into(),
                device_label: "Travel laptop".into(),
                direction: NearbyTransferDirection::Receiving,
                phase: NearbyTransferPhase::AwaitingConsent,
                transferred_bytes: 0,
                total_bytes: 42,
            };
            let destination = NearbyTransferDestination {
                handle: NearbyDestinationHandle::from_issued_bytes([6; 16]),
                label: "Workspace files".into(),
                remembered_for_session: false,
            };
            let effects = state.handle_event(&CollabEvent::NearbyTransferCommandResult {
                request_id,
                result: Ok(NearbyTransferReply::Snapshot {
                    snapshot: NearbyTransferSnapshot {
                        targets: vec![NearbyTransferTarget {
                            handle: NearbyTargetHandle::from_issued_bytes([4; 16]),
                            label: "Desk".into(),
                            online: true,
                        }],
                        destinations: vec![destination.clone()],
                        transfers: vec![offer.clone()],
                    },
                }),
            });
            assert!(matches!(
                effects.first(),
                Some(NearbyUiEffect::OpenPicker(_))
            ));
            assert!(effects.contains(&NearbyUiEffect::Offer {
                transfer: offer,
                destinations: vec![destination],
            }));
        }

        // --- phase: receiver_decision_uses_only_opaque_destination_authority ---
        {
            let mut state = NearbySendState::default();
            let transfer = NearbyTransferHandle::from_issued_bytes([8; 16]);
            let destination = NearbyDestinationHandle::from_issued_bytes([9; 16]);
            state.decide(transfer, Some(destination), true);
            let request = state.take_requests().pop().expect("decision request");
            let CollabRequest::NearbyTransferCommand { command, .. } = request else {
                panic!("wrong request");
            };
            assert_eq!(
                command,
                NearbyTransferCommand::Decide {
                    handle: transfer,
                    accept: true,
                    destination: Some(destination),
                    remember_for_session: true,
                }
            );
        }
    }

    #[test]
    fn identical_background_snapshot_is_not_a_ui_effect() {
        let mut state = NearbySendState::default();
        state.poll_if_due(ui::time::now());
        let request = state.take_requests().pop().expect("snapshot request");
        let CollabRequest::NearbyTransferCommand { request_id, .. } = request else {
            panic!("wrong request");
        };
        let idle = NearbyTransferSnapshot::default();
        let first = state.handle_event(&CollabEvent::NearbyTransferCommandResult {
            request_id,
            result: Ok(NearbyTransferReply::Snapshot {
                snapshot: idle.clone(),
            }),
        });
        assert!(
            first.is_empty(),
            "an empty idle roster must not open chrome"
        );
        state.poll_if_due(ui::time::now() + super::POLL_INTERVAL);
        let request = state.take_requests().pop().expect("second snapshot");
        let CollabRequest::NearbyTransferCommand { request_id, .. } = request else {
            panic!("wrong request");
        };
        let second = state.handle_event(&CollabEvent::NearbyTransferCommandResult {
            request_id,
            result: Ok(NearbyTransferReply::Snapshot { snapshot: idle }),
        });
        assert!(
            second.is_empty(),
            "an unchanged Nearby poll must not keep requesting frames"
        );
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_source_path_stays_exact_in_send_command() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let path = std::path::PathBuf::from(OsString::from_vec(vec![b'/', b'a', 0xff]));
        let mut state = NearbySendState::default();
        state
            .send(&path, NearbyTargetHandle::from_issued_bytes([9; 16]))
            .unwrap();
        let request = state.take_requests().pop().expect("send request");
        let CollabRequest::NearbyTransferCommand { command, .. } = request else {
            panic!("wrong request");
        };
        let NearbyTransferCommand::Send { source_path, .. } = command else {
            panic!("wrong command");
        };
        assert_eq!(source_path, path.as_os_str().as_bytes());
    }

    #[test]
    #[cfg(windows)]
    fn native_windows_source_is_rejected_without_queuing_unix_path_bytes() {
        let mut state = NearbySendState::default();
        assert!(
            state
                .send(
                    Path::new(r"C:\Users\owner\Documents\report.txt"),
                    NearbyTargetHandle::from_issued_bytes([9; 16]),
                )
                .is_err()
        );
        assert!(state.take_requests().is_empty());
        assert!(state.pending.is_empty());
    }
}
