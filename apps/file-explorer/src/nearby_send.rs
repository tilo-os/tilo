//! Files-side state for the shell-owned Nearby Send service.
//!
//! This module owns no transport and never reads a file. It emits bounded
//! commands onto the toolkit's already-authenticated collaboration lane, then
//! projects the shell's certified-device/status reply into app-local picker,
//! consent and progress state. The shell remains responsible for validating
//! paths, consent and the existing resumable transfer.
//!
//! It never asks on a timer. The shell announces each change
//! ([`CollabEvent::NearbyTransferChanged`], which names nothing) and Files
//! then asks for one snapshot; the first announcement arrives when the route
//! joins. An idle desktop with Files open therefore makes no Nearby request
//! and no wakeup at all.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use ui::time::{Duration, Instant};

use ui::collaboration::{
    CollabEvent, CollabRequest, NearbyDestinationHandle, NearbyTargetHandle, NearbyTransferCommand,
    NearbyTransferDestination, NearbyTransferHandle, NearbyTransferPhase, NearbyTransferReply,
    NearbyTransferSnapshot, NearbyTransferStatus, NearbyTransferTarget,
};

const APP_ID: &str = "so.tilo.file-explorer";
/// How long a background snapshot may go unanswered before the next
/// announced change asks again regardless: a request sent while the route
/// was going down is never answered, and waiting on it would stop every
/// later refresh.
const UNANSWERED_AFTER: Duration = Duration::from_secs(5);
const MAX_VISIBLE_HISTORY: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Pending {
    Snapshot { open_picker: bool },
    AirDropAvailability,
    Send,
    Decide,
    Cancel,
    Retry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NearbyUiEffect {
    AvailabilityChanged,
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
    /// When the background snapshot now awaiting its reply was asked for.
    refresh_sent: Option<Instant>,
    /// The shell announced another change while that snapshot was out: ask
    /// once more when it is answered, so the last change is never missed.
    refresh_again: bool,
    shown_offer: Option<NearbyTransferHandle>,
    observed_phases: BTreeMap<NearbyTransferHandle, NearbyTransferPhase>,
    history: VecDeque<NearbyTransferStatus>,
    airdrop_available: bool,
}

impl Default for NearbySendState {
    fn default() -> Self {
        Self {
            next_request_id: 1,
            pending: BTreeMap::new(),
            requests: VecDeque::new(),
            snapshot: NearbyTransferSnapshot::default(),
            refresh_sent: None,
            refresh_again: false,
            shown_offer: None,
            observed_phases: BTreeMap::new(),
            history: VecDeque::new(),
            airdrop_available: false,
        }
    }
}

impl NearbySendState {
    pub fn airdrop_available(&self) -> bool {
        self.airdrop_available
    }

    /// The app submits no certificate/trust assertion. Those are collected
    /// on the compositor-owned verification sheet for this one attempt.
    ///
    /// # Errors
    /// Returns a localized error if lab availability, Unix source-path
    /// representation or the bounded literal endpoint is invalid.
    pub fn send_airdrop(&mut self, path: &Path, address: String, port: u16) -> Result<(), String> {
        if !self.airdrop_available {
            return Err(crate::i18n::t("files-airdrop-unavailable"));
        }
        let Some(path) = host_fs::names::unix_path_bytes(path) else {
            return Err(crate::i18n::t("files-share-not-available"));
        };
        let command = NearbyTransferCommand::SendAirDrop {
            source_path: path.to_vec(),
            address,
            port,
        };
        if !command.valid() {
            return Err(crate::i18n::t("files-airdrop-endpoint-invalid"));
        }
        self.queue(command, Pending::Send);
        Ok(())
    }
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

    pub fn cancel(&mut self, handle: NearbyTransferHandle) {
        self.queue(NearbyTransferCommand::Cancel { handle }, Pending::Cancel);
    }

    pub fn retry(&mut self, handle: NearbyTransferHandle) {
        self.queue(NearbyTransferCommand::Retry { handle }, Pending::Retry);
    }

    /// The shell announced that the view may have changed: ask for one
    /// snapshot, and whether `AirDrop` is on offer. While an earlier ask is
    /// still out, remember to ask again once it is answered instead; bursts
    /// of changes (a transfer's progress) cost one snapshot at a time.
    fn refresh(&mut self, now: Instant) {
        if self
            .refresh_sent
            .is_some_and(|sent| now.saturating_duration_since(sent) < UNANSWERED_AFTER)
        {
            self.refresh_again = true;
            return;
        }
        // Nothing is out, or what is out will never be answered.
        self.pending.retain(|_, pending| {
            !matches!(
                pending,
                Pending::Snapshot { open_picker: false } | Pending::AirDropAvailability
            )
        });
        self.refresh_sent = Some(now);
        self.refresh_again = false;
        self.queue(
            NearbyTransferCommand::Snapshot,
            Pending::Snapshot { open_picker: false },
        );
        self.queue(
            NearbyTransferCommand::AirDropAvailability,
            Pending::AirDropAvailability,
        );
    }

    /// The background snapshot was answered, either way.
    fn refreshed(&mut self, now: Instant) {
        self.refresh_sent = None;
        if std::mem::take(&mut self.refresh_again) {
            self.refresh(now);
        }
    }

    /// Requests are queued and the app must be pumped to send them. They are
    /// queued while handling an event, after this turn's requests were taken,
    /// so the host wakes for them at once; with none queued this is no
    /// deadline at all.
    pub fn has_requests(&self) -> bool {
        !self.requests.is_empty()
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
        if matches!(event, CollabEvent::NearbyTransferChanged) {
            self.refresh(ui::time::now());
            return Vec::new();
        }
        let CollabEvent::NearbyTransferCommandResult { request_id, result } = event else {
            return Vec::new();
        };
        let Some(pending) = self.pending.remove(request_id) else {
            return Vec::new();
        };
        if pending == (Pending::Snapshot { open_picker: false }) {
            self.refreshed(ui::time::now());
        }
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                if pending == (Pending::Snapshot { open_picker: false }) {
                    // Discovery may be disabled by the person's privacy settings.
                    // Withdraw old targets without interrupting unrelated work;
                    // an explicit picker/send still reports its own refusal.
                    let changed = self.snapshot != NearbyTransferSnapshot::default();
                    self.apply_snapshot(NearbyTransferSnapshot::default(), false);
                    return if changed {
                        vec![NearbyUiEffect::AvailabilityChanged]
                    } else {
                        Vec::new()
                    };
                }
                if pending == Pending::AirDropAvailability {
                    if self.airdrop_available {
                        self.airdrop_available = false;
                        return vec![NearbyUiEffect::AvailabilityChanged];
                    }
                    return Vec::new();
                }
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
            (
                Pending::AirDropAvailability,
                NearbyTransferReply::AirDropAvailability { available },
            ) => {
                if self.airdrop_available == *available {
                    return Vec::new();
                }
                self.airdrop_available = *available;
                vec![NearbyUiEffect::AvailabilityChanged]
            }
            (Pending::AirDropAvailability, _) => {
                if self.airdrop_available {
                    self.airdrop_available = false;
                    vec![NearbyUiEffect::AvailabilityChanged]
                } else {
                    Vec::new()
                }
            }
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
        // A change the shell announced may leave the view as it was (a
        // handle renewed, progress inside one percent). An identical roster
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
        if transfer.direction == ui::collaboration::NearbyTransferDirection::Receiving
            && matches!(
                transfer.phase,
                NearbyTransferPhase::AwaitingConsent | NearbyTransferPhase::AwaitingDestination
            )
            && self.shown_offer != Some(transfer.handle)
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

    /// The shell announces a change; Files asks for one snapshot and for
    /// `AirDrop`'s availability, and nothing else. Returns the snapshot's id.
    fn announce(state: &mut NearbySendState) -> u64 {
        assert!(
            state
                .handle_event(&CollabEvent::NearbyTransferChanged)
                .is_empty(),
            "an announcement names nothing and shows nothing by itself"
        );
        assert!(state.has_requests(), "the host wakes to send the ask");
        let requests = state.take_requests();
        assert_eq!(requests.len(), 2, "one snapshot and one availability ask");
        requests
            .iter()
            .find_map(|request| match request {
                CollabRequest::NearbyTransferCommand {
                    request_id,
                    command: NearbyTransferCommand::Snapshot,
                    ..
                } => Some(*request_id),
                _ => None,
            })
            .expect("an announcement asks for a snapshot")
    }

    fn answer(
        state: &mut NearbySendState,
        request_id: u64,
        snapshot: NearbyTransferSnapshot,
    ) -> Vec<NearbyUiEffect> {
        state.handle_event(&CollabEvent::NearbyTransferCommandResult {
            request_id,
            result: Ok(NearbyTransferReply::Snapshot { snapshot }),
        })
    }

    fn sending(phase: NearbyTransferPhase, transferred_bytes: u64) -> NearbyTransferSnapshot {
        NearbyTransferSnapshot {
            transfers: vec![NearbyTransferStatus {
                handle: NearbyTransferHandle::from_issued_bytes([31; 16]),
                name: "report.pdf".into(),
                device_label: "Desk".into(),
                direction: NearbyTransferDirection::Sending,
                phase,
                transferred_bytes,
                total_bytes: 200,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn an_idle_files_window_asks_nothing_and_needs_no_wakeup() {
        let mut state = NearbySendState::default();
        assert!(
            !state.has_requests(),
            "nothing is asked before the shell says so"
        );
        assert!(state.take_requests().is_empty());
        let request_id = announce(&mut state);
        assert!(answer(&mut state, request_id, NearbyTransferSnapshot::default()).is_empty());
        assert!(
            !state.has_requests() && state.take_requests().is_empty(),
            "an answered view waits for the next announcement, not a timer"
        );
    }

    #[test]
    fn an_offer_announced_by_the_shell_surfaces_without_a_timer() {
        let mut state = NearbySendState::default();
        let request_id = announce(&mut state);
        assert!(answer(&mut state, request_id, NearbyTransferSnapshot::default()).is_empty());

        // A device offers a file: the shell announces it, nothing else runs.
        let offer = NearbyTransferStatus {
            handle: NearbyTransferHandle::from_issued_bytes([12; 16]),
            name: "Photos".into(),
            device_label: "Travel laptop".into(),
            direction: NearbyTransferDirection::Receiving,
            phase: NearbyTransferPhase::AwaitingConsent,
            transferred_bytes: 0,
            total_bytes: 42,
        };
        let destination = NearbyTransferDestination {
            handle: NearbyDestinationHandle::from_issued_bytes([13; 16]),
            label: "Workspace files".into(),
            remembered_for_session: false,
        };
        let request_id = announce(&mut state);
        let effects = answer(
            &mut state,
            request_id,
            NearbyTransferSnapshot {
                destinations: vec![destination.clone()],
                transfers: vec![offer.clone()],
                ..Default::default()
            },
        );
        assert_eq!(
            effects,
            vec![NearbyUiEffect::Offer {
                transfer: offer.clone(),
                destinations: vec![destination],
            }]
        );
        assert_eq!(
            state.transfers(),
            &[offer],
            "the action surface lists the offer for a person, script or agent to answer"
        );
    }

    #[test]
    fn a_transfer_start_progress_and_end_reach_files_through_announcements() {
        let mut state = NearbySendState::default();

        let request_id = announce(&mut state);
        assert_eq!(
            answer(
                &mut state,
                request_id,
                sending(NearbyTransferPhase::Queued, 0)
            ),
            vec![NearbyUiEffect::Status("Queued report.pdf for Desk".into())],
            "a transfer that starts is shown"
        );

        let request_id = announce(&mut state);
        assert_eq!(
            answer(
                &mut state,
                request_id,
                sending(NearbyTransferPhase::Transferring, 100)
            ),
            vec![NearbyUiEffect::Status("Sending report.pdf (50%)".into())]
        );
        let request_id = announce(&mut state);
        assert!(
            answer(
                &mut state,
                request_id,
                sending(NearbyTransferPhase::Transferring, 150)
            )
            .is_empty(),
            "progress inside a phase is no new status line"
        );
        assert_eq!(
            state.transfers()[0].transferred_bytes,
            150,
            "but the transfer Files publishes moves on"
        );

        let request_id = announce(&mut state);
        assert_eq!(
            answer(
                &mut state,
                request_id,
                sending(NearbyTransferPhase::Completed, 200)
            ),
            vec![NearbyUiEffect::Status(
                "Nearby Send completed: report.pdf".into()
            )],
            "a transfer that ends is shown"
        );
        assert_eq!(
            state.history.front().map(|entry| entry.phase),
            Some(NearbyTransferPhase::Completed)
        );

        let mut failing = NearbySendState::default();
        let request_id = announce(&mut failing);
        let effects = answer(
            &mut failing,
            request_id,
            sending(NearbyTransferPhase::Failed, 40),
        );
        assert!(
            matches!(
                effects.first(),
                Some(NearbyUiEffect::Failed(transfer)) if transfer.name == "report.pdf"
            ),
            "a failed transfer offers its retry: {effects:?}"
        );
    }

    #[test]
    fn announcements_during_an_unanswered_snapshot_become_one_more_ask() {
        let mut state = NearbySendState::default();
        let first = announce(&mut state);
        for _ in 0..3 {
            assert!(
                state
                    .handle_event(&CollabEvent::NearbyTransferChanged)
                    .is_empty()
            );
            assert!(
                state.take_requests().is_empty(),
                "one snapshot is out at a time"
            );
        }
        assert!(answer(&mut state, first, NearbyTransferSnapshot::default()).is_empty());
        let requests = state.take_requests();
        assert!(
            matches!(
                requests.as_slice(),
                [
                    CollabRequest::NearbyTransferCommand {
                        command: NearbyTransferCommand::Snapshot,
                        ..
                    },
                    CollabRequest::NearbyTransferCommand {
                        command: NearbyTransferCommand::AirDropAvailability,
                        ..
                    }
                ]
            ),
            "the changes announced meanwhile are asked about once: {requests:?}"
        );
    }

    #[test]
    fn an_ask_lost_with_its_route_does_not_block_later_announcements() {
        let mut state = NearbySendState::default();
        let lost = announce(&mut state);
        let sent = state.refresh_sent.expect("the ask is out");
        // The reply never comes; the route went down with it. A change
        // announced after the rejoin asks again rather than waiting forever.
        state.refresh(sent + UNANSWERED_AFTER);
        let requests = state.take_requests();
        let Some(CollabRequest::NearbyTransferCommand {
            request_id,
            command: NearbyTransferCommand::Snapshot,
            ..
        }) = requests.first()
        else {
            panic!("a fresh snapshot: {requests:?}");
        };
        let request_id = *request_id;
        assert_ne!(request_id, lost);
        assert!(
            answer(&mut state, lost, sending(NearbyTransferPhase::Queued, 0)).is_empty(),
            "a late answer to the forgotten ask is ignored"
        );
        assert_eq!(
            answer(
                &mut state,
                request_id,
                sending(NearbyTransferPhase::Queued, 0)
            ),
            vec![NearbyUiEffect::Status("Queued report.pdf for Desk".into())]
        );
    }

    #[test]
    fn airdrop_availability_is_typed_and_failure_withdraws_the_action() {
        let mut state = NearbySendState::default();
        assert!(!state.airdrop_available());
        assert!(
            state
                .send_airdrop(Path::new("/fixture"), "127.0.0.1".into(), 443)
                .is_err()
        );
        state.handle_event(&CollabEvent::NearbyTransferChanged);
        let requests = state.take_requests();
        let request_id = requests
            .iter()
            .find_map(|request| match request {
                CollabRequest::NearbyTransferCommand {
                    request_id,
                    command: NearbyTransferCommand::AirDropAvailability,
                    ..
                } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            state.handle_event(&CollabEvent::NearbyTransferCommandResult {
                request_id,
                result: Ok(NearbyTransferReply::AirDropAvailability { available: true }),
            }),
            vec![NearbyUiEffect::AvailabilityChanged]
        );
        assert!(state.airdrop_available());
        state.queue(
            NearbyTransferCommand::AirDropAvailability,
            Pending::AirDropAvailability,
        );
        let CollabRequest::NearbyTransferCommand { request_id, .. } =
            state.take_requests().pop().unwrap()
        else {
            panic!("request")
        };
        assert_eq!(
            state.handle_event(&CollabEvent::NearbyTransferCommandResult {
                request_id,
                result: Err("host disconnected".into()),
            }),
            vec![NearbyUiEffect::AvailabilityChanged]
        );
        assert!(!state.airdrop_available());
        assert!(
            state
                .send_airdrop(Path::new("/fixture"), "127.0.0.1".into(), 443)
                .is_err()
        );
    }

    #[test]
    fn background_discovery_refusal_withdraws_targets_and_explicit_picker_reports_failure() {
        for had_targets in [false, true] {
            let mut state = NearbySendState::default();
            if had_targets {
                state.snapshot.targets.push(NearbyTransferTarget {
                    handle: NearbyTargetHandle::from_issued_bytes([4; 16]),
                    label: "Desk".into(),
                    online: true,
                });
            }
            state.handle_event(&CollabEvent::NearbyTransferChanged);
            let request_id = state
                .take_requests()
                .into_iter()
                .find_map(|request| match request {
                    CollabRequest::NearbyTransferCommand {
                        request_id,
                        command: NearbyTransferCommand::Snapshot,
                        ..
                    } => Some(request_id),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                state.handle_event(&CollabEvent::NearbyTransferCommandResult {
                    request_id,
                    result: Err("privacy refusal".into()),
                }),
                if had_targets {
                    vec![NearbyUiEffect::AvailabilityChanged]
                } else {
                    Vec::new()
                }
            );
            assert!(state.targets().is_empty());
            state.open_picker();
            let CollabRequest::NearbyTransferCommand { request_id, .. } =
                state.take_requests().pop().unwrap()
            else {
                panic!("picker request")
            };
            assert_eq!(
                state.handle_event(&CollabEvent::NearbyTransferCommandResult {
                    request_id,
                    result: Err("privacy refusal".into()),
                }),
                vec![NearbyUiEffect::Status(
                    "Nearby Send unavailable: privacy refusal".into()
                )]
            );
        }
    }

    #[test]
    fn optional_availability_mismatch_is_silent_and_withdraws_old_support() {
        let mut state = NearbySendState::default();
        for previously_available in [false, true] {
            state.airdrop_available = previously_available;
            state.queue(
                NearbyTransferCommand::AirDropAvailability,
                Pending::AirDropAvailability,
            );
            let CollabRequest::NearbyTransferCommand { request_id, .. } =
                state.take_requests().pop().unwrap()
            else {
                panic!("availability request")
            };
            let effects = state.handle_event(&CollabEvent::NearbyTransferCommandResult {
                request_id,
                result: Ok(NearbyTransferReply::Snapshot {
                    snapshot: NearbyTransferSnapshot::default(),
                }),
            });
            assert_eq!(
                effects,
                if previously_available {
                    vec![NearbyUiEffect::AvailabilityChanged]
                } else {
                    Vec::new()
                }
            );
            assert!(!state.airdrop_available());
        }
    }

    #[test]
    fn outgoing_airdrop_consent_never_opens_a_receive_offer() {
        let mut state = NearbySendState::default();
        let snapshot = NearbyTransferSnapshot {
            transfers: vec![NearbyTransferStatus {
                handle: NearbyTransferHandle::from_issued_bytes([19; 16]),
                name: "report.txt".into(),
                device_label: "AirDrop · 127.0.0.1:443".into(),
                direction: NearbyTransferDirection::Sending,
                phase: NearbyTransferPhase::AwaitingConsent,
                transferred_bytes: 0,
                total_bytes: 8,
            }],
            ..Default::default()
        };
        assert!(
            !state
                .apply_snapshot(snapshot, false)
                .iter()
                .any(|effect| matches!(effect, NearbyUiEffect::Offer { .. }))
        );
    }

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
        let request_id = announce(&mut state);
        let idle = NearbyTransferSnapshot::default();
        assert!(
            answer(&mut state, request_id, idle.clone()).is_empty(),
            "an empty idle roster must not open chrome"
        );
        // A renewed handle or progress inside one percent is announced too.
        let request_id = announce(&mut state);
        assert!(
            answer(&mut state, request_id, idle).is_empty(),
            "an unchanged Nearby view must not keep requesting frames"
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
