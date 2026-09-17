//! Files-side state for the shell-owned Nearby Send service.
//!
//! This module owns no transport and never reads a file. It emits bounded
//! commands onto the toolkit's already-authenticated collaboration lane, then
//! projects the shell's certified-device/status reply into app-local picker,
//! consent and progress state. The shell remains responsible for validating
//! paths, consent and the existing resumable transfer.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use ui::collaboration::{
    CollabEvent, CollabRequest, NearbyTargetHandle, NearbyTransferCommand, NearbyTransferPhase,
    NearbyTransferReply, NearbyTransferSnapshot, NearbyTransferStatus, NearbyTransferTarget,
};

const APP_ID: &str = "dev.tilo.file-explorer";
const POLL_INTERVAL: Duration = Duration::from_secs(2);

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
    Offer(NearbyTransferStatus),
    Failed(NearbyTransferStatus),
    Status(String),
    SendResult(Result<[u8; 16], String>),
}

pub struct NearbySendState {
    next_request_id: u64,
    pending: BTreeMap<u64, Pending>,
    requests: VecDeque<CollabRequest>,
    snapshot: NearbyTransferSnapshot,
    next_poll: Instant,
    shown_offer: Option<[u8; 16]>,
    observed_phases: BTreeMap<[u8; 16], NearbyTransferPhase>,
}

impl Default for NearbySendState {
    fn default() -> Self {
        Self {
            next_request_id: 1,
            pending: BTreeMap::new(),
            requests: VecDeque::new(),
            snapshot: NearbyTransferSnapshot::default(),
            next_poll: Instant::now(),
            shown_offer: None,
            observed_phases: BTreeMap::new(),
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

    pub fn send(&mut self, path: &Path, target: NearbyTargetHandle) {
        use std::os::unix::ffi::OsStrExt as _;
        self.queue(
            NearbyTransferCommand::Send {
                target,
                source_path: path.as_os_str().as_bytes().to_vec(),
            },
            Pending::Send,
        );
    }

    pub fn decide(&mut self, transfer_id: [u8; 16], accept: bool) {
        self.shown_offer = None;
        self.queue(
            NearbyTransferCommand::Decide {
                transfer_id,
                accept,
            },
            Pending::Decide,
        );
    }

    pub fn cancel(&mut self, transfer_id: [u8; 16]) {
        self.queue(
            NearbyTransferCommand::Cancel { transfer_id },
            Pending::Cancel,
        );
    }

    pub fn retry(&mut self, transfer_id: [u8; 16]) {
        self.queue(NearbyTransferCommand::Retry { transfer_id }, Pending::Retry);
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
        match (pending, reply) {
            (Pending::Snapshot { open_picker }, NearbyTransferReply::Snapshot { snapshot }) => {
                self.snapshot = snapshot.clone();
                let mut effects = Vec::new();
                if open_picker {
                    effects.push(NearbyUiEffect::OpenPicker(self.snapshot.targets.clone()));
                }
                for transfer in &self.snapshot.transfers {
                    if transfer.phase == NearbyTransferPhase::AwaitingConsent
                        && self.shown_offer != Some(transfer.transfer_id)
                    {
                        self.shown_offer = Some(transfer.transfer_id);
                        effects.push(NearbyUiEffect::Offer(transfer.clone()));
                    }
                    let old = self
                        .observed_phases
                        .insert(transfer.transfer_id, transfer.phase);
                    if old == Some(transfer.phase) {
                        continue;
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
                            Some(format!("Sending {} · {percent}%", transfer.name))
                        }
                        NearbyTransferPhase::Completed => {
                            Some(format!("Nearby Send completed: {}", transfer.name))
                        }
                        NearbyTransferPhase::Failed => {
                            effects.push(NearbyUiEffect::Failed(transfer.clone()));
                            Some(format!(
                                "Nearby Send failed: {} · retry available",
                                transfer.name
                            ))
                        }
                        NearbyTransferPhase::Cancelled => {
                            Some(format!("Nearby Send cancelled: {}", transfer.name))
                        }
                        NearbyTransferPhase::AwaitingConsent => None,
                    };
                    if let Some(message) = message {
                        effects.push(NearbyUiEffect::Status(message));
                    }
                }
                effects
            }
            (Pending::Send, NearbyTransferReply::Queued { transfer_id }) => {
                vec![
                    NearbyUiEffect::SendResult(Ok(*transfer_id)),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::collaboration::{NearbyTransferDirection, NearbyTransferPhase, NearbyTransferReply};

    #[test]
    fn picker_and_receiver_consent_are_driven_by_typed_broker_results() {
        let mut state = NearbySendState::default();
        state.open_picker();
        let request = state.take_requests().pop().expect("snapshot request");
        let CollabRequest::NearbyTransferCommand { request_id, .. } = request else {
            panic!("wrong request");
        };
        let offer = NearbyTransferStatus {
            transfer_id: [8; 16],
            name: "Photos".into(),
            device_label: "Travel laptop".into(),
            direction: NearbyTransferDirection::Receiving,
            phase: NearbyTransferPhase::AwaitingConsent,
            transferred_bytes: 0,
            total_bytes: 42,
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
                    transfers: vec![offer.clone()],
                },
            }),
        });
        assert!(matches!(
            effects.first(),
            Some(NearbyUiEffect::OpenPicker(_))
        ));
        assert!(effects.contains(&NearbyUiEffect::Offer(offer)));
    }

    #[test]
    fn non_utf8_source_path_stays_exact_in_send_command() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let path = std::path::PathBuf::from(OsString::from_vec(vec![b'/', b'a', 0xff]));
        let mut state = NearbySendState::default();
        state.send(&path, NearbyTargetHandle::from_issued_bytes([9; 16]));
        let request = state.take_requests().pop().expect("send request");
        let CollabRequest::NearbyTransferCommand { command, .. } = request else {
            panic!("wrong request");
        };
        let NearbyTransferCommand::Send { source_path, .. } = command else {
            panic!("wrong command");
        };
        assert_eq!(source_path, path.as_os_str().as_bytes());
    }
}
