//! How the session's displays and input come back after a wake: outputs
//! that could not be lit are lit again, and the flight record notes the
//! input the session holds and the first event after the wake.

use smithay::backend::input::InputEvent;
use smithay::backend::libinput::LibinputInputBackend;

use super::super::wake_check::WakeStage;
use super::{DrmDispatchState, DrmOutputKey, ScanoutCommand, device_caps};

/// How long after a wake an output's failure is still the wake's: a
/// modeset whose link never comes up completes only after the driver's
/// commit timeout, seconds later on some DP sinks.
const WAKE_RELIGHT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

impl DrmDispatchState {
    /// Input arrived: the first event after a wake goes into the flight
    /// record, with how long after the wake it came.
    pub(super) fn note_input_after_wake(&mut self, event: &InputEvent<LibinputInputBackend>) {
        if let Some(kind) = libinput_event_kind(event)
            && let Some((wake, at)) = self.input_wake.take()
        {
            eprintln!(
                "tilo compositor: DRM input: first event after wake={wake} after_ms={} kind={kind}",
                at.elapsed().as_millis()
            );
        }
    }

    /// Light every output that could not be lit again: a wake, the power
    /// key or a resume is the person asking for the displays, as a display
    /// settings change is. Each output is modeset once more, nonblocking;
    /// nothing retries on its own. Whether there was one to light.
    pub fn retry_failed_outputs(&mut self, reason: &str) -> bool {
        if self.link_failures.is_empty() {
            return false;
        }
        let outputs: Vec<&str> = self
            .link_failures
            .values()
            .map(|record| record.config_key.as_str())
            .collect();
        eprintln!(
            "tilo compositor: DRM display: {reason}: lighting {} again; it could not be lit before",
            outputs.join(",")
        );
        self.link_failures.clear();
        self.reapply_outputs_from_cache(reason);
        true
    }

    /// The display woke (`reason`): light again an output that could not
    /// be lit, record which input devices the session holds and whether its
    /// seat is active, and record the first input event after the wake.
    pub fn note_wake(&mut self, reason: &'static str) {
        let devices: Vec<String> = self
            .devices
            .iter()
            .map(|device| format!("{:?}({:?})", device.name(), device_caps(device).kind()))
            .collect();
        let seat = match self.session.as_ref() {
            Some(session) if session.is_active() => "active",
            Some(_) => "paused",
            None => "none",
        };
        eprintln!(
            "tilo compositor: DRM input: wake={reason} seat_session={seat} devices={} [{}]",
            devices.len(),
            devices.join(", ")
        );
        self.input_wake = Some((reason, std::time::Instant::now()));
        // Every output lit before the blank must light again, on every
        // card: one whose wake fails is brought up once more from scratch
        // ([`Self::take_wake_relight`]).
        self.wake_relight = Some((
            std::time::Instant::now(),
            self.live.iter().map(|entry| entry.key).collect(),
        ));
        // A card that lost DRM master meanwhile lights nothing, whatever is
        // retried on it: every card takes master back first.
        self.recover_card_masters(reason);
        // A wake is the person asking for the displays: one that could not
        // be lit gets another attempt.
        self.retry_failed_outputs(reason);
    }

    /// Take DRM master back on every card (`reason` says what asked), and
    /// reopen through the seat each card that cannot get it back. While
    /// another session holds the seat nothing is asked: master belongs to it.
    pub(super) fn recover_card_masters(&mut self, reason: &str) {
        if self
            .session
            .as_ref()
            .is_some_and(|session| !session.is_active())
        {
            return;
        }
        let lost = super::cards_without_master(self.cards.iter());
        self.cards_to_reopen.extend(lost);
        self.reopen_cards_without_master(reason);
    }

    /// Reopen each card found without DRM master while the session is
    /// active. The card's fd is no longer the one the seat manager makes
    /// master, so it is retired as an unplugged card would be and opened
    /// through the seat again once the presenter let go of it: the seat
    /// grants the new fd master, and its connectors are lit as a hotplugged
    /// card's are. Whether one was reopened.
    pub(super) fn reopen_cards_without_master(&mut self, reason: &str) -> bool {
        let cards = std::mem::take(&mut self.cards_to_reopen);
        let mut reopened = false;
        for device_id in cards {
            let Some(path) = self
                .cards
                .get(&device_id)
                .map(|card| card.path().to_path_buf())
            else {
                continue;
            };
            eprintln!(
                "tilo compositor: DRM card: {} does not hold DRM master ({reason}); reopening it through the seat",
                path.display()
            );
            self.retire_card(device_id);
            // The old fd is still open: the re-add waits for its close.
            self.add_card(&path, device_id);
            reopened = true;
        }
        reopened
    }

    /// `key` failed while its wake still owes it another bring-up: take
    /// that bring-up, once. A wake's failure shows within
    /// [`WAKE_RELIGHT_WINDOW`]; a link lost later is no longer the wake's.
    pub(super) fn take_wake_relight(&mut self, key: DrmOutputKey) -> bool {
        let Some((at, owed)) = self.wake_relight.as_mut() else {
            return false;
        };
        if at.elapsed() > WAKE_RELIGHT_WINDOW {
            self.wake_relight = None;
            return false;
        }
        owed.remove(&key)
    }

    /// Send the wake fault `stage` to whoever owns the scanouts: the
    /// presenter, or the backend's own poll before the presenter split off.
    pub fn inject_wake_fault(&self, stage: WakeStage) -> Result<(), String> {
        let sent = self
            .commands
            .as_ref()
            .is_some_and(|commands| commands.send(ScanoutCommand::WakeFault(stage)).is_ok());
        if sent {
            Ok(())
        } else {
            Err("the scanouts no longer take commands".to_owned())
        }
    }
}

/// How soon a seat found without input devices is reopened, and the longest
/// wait between later attempts.
const INPUT_RECOVERY_FIRST: std::time::Duration = std::time::Duration::from_secs(2);
const INPUT_RECOVERY_LONGEST: std::time::Duration = std::time::Duration::from_secs(60);

/// When to reopen a seat whose input devices all vanished while the session
/// is active. After a suspend the seat manager revoked every input device
/// and libinput never got them back: the lock screen came up with no
/// keyboard and no pointer, and only a restart helped. An active seat with
/// no input device at all is never right on a machine with a lid, a power
/// button or a keyboard, so libinput is suspended and resumed, which opens
/// every device on the seat again; the attempts back off, never stop.
#[derive(Debug, Default)]
pub(super) struct InputRecovery {
    next_at: Option<std::time::Instant>,
    attempts: u32,
}

impl InputRecovery {
    /// Whether to reopen the seat now. `missing`: the session is active and
    /// libinput holds no device. Devices coming back resets the backoff.
    pub(super) fn due(&mut self, missing: bool, now: std::time::Instant) -> Option<u32> {
        if !missing {
            self.next_at = None;
            self.attempts = 0;
            return None;
        }
        let next = *self
            .next_at
            .get_or_insert_with(|| now.checked_add(INPUT_RECOVERY_FIRST).unwrap_or(now));
        if now < next {
            return None;
        }
        self.attempts = self.attempts.saturating_add(1);
        let wait = INPUT_RECOVERY_FIRST
            .saturating_mul(1 << self.attempts.min(5))
            .min(INPUT_RECOVERY_LONGEST);
        self.next_at = Some(now.checked_add(wait).unwrap_or(now));
        Some(self.attempts)
    }
}

/// What a libinput event is, for the flight record; `None` for a device
/// arriving or leaving, which is no input from the person.
fn libinput_event_kind(event: &InputEvent<LibinputInputBackend>) -> Option<&'static str> {
    Some(match event {
        InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } => return None,
        InputEvent::Keyboard { .. } => "keyboard",
        InputEvent::PointerMotion { .. }
        | InputEvent::PointerMotionAbsolute { .. }
        | InputEvent::PointerButton { .. }
        | InputEvent::PointerAxis { .. } => "pointer",
        InputEvent::TouchDown { .. }
        | InputEvent::TouchMotion { .. }
        | InputEvent::TouchUp { .. }
        | InputEvent::TouchCancel { .. }
        | InputEvent::TouchFrame { .. } => "touch",
        InputEvent::SwitchToggle { .. } => "switch",
        _ => "other",
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::tests::{DGPU, IGPU, adopt_on, dispatch_with_channel, edid};
    use super::super::{
        ConnectorSnapshot, DrmDispatchState, DrmOutputKey, OutputSignalStatus, SignalReport,
    };
    use crate::compositor::backend::OutputId;

    fn link_down(output: OutputId) -> SignalReport {
        SignalReport {
            output,
            status: OutputSignalStatus {
                link_failure: Some(settings_store::hardware::OutputUnavailable::LinkDown),
                ..OutputSignalStatus::default()
            },
        }
    }

    const PANEL: DrmOutputKey = DrmOutputKey {
        device: IGPU,
        connector: 126,
    };

    /// The owner's hybrid laptop: HDMI on the discrete card, the built-in
    /// panel on the integrated one, both lit.
    fn hybrid_laptop(state: &mut DrmDispatchState) -> (OutputId, OutputId) {
        let hdmi = adopt_on(state, DGPU, 835, "HDMI-A-1", (3840, 2160), (600, 340));
        let panel = adopt_on(state, IGPU, 126, "eDP-1", (1920, 1080), (340, 190));
        state.connector_probes = BTreeMap::from([
            (
                DGPU,
                vec![ConnectorSnapshot::fake(835, "HDMI-A-1", Some(edid("H")))],
            ),
            (
                IGPU,
                vec![ConnectorSnapshot::fake(126, "eDP-1", Some(edid("P")))],
            ),
        ]);
        (hdmi, panel)
    }

    #[test]
    fn a_wake_brings_up_again_an_output_on_the_other_card_whose_wake_failed() {
        let (mut state, _rx) = dispatch_with_channel();
        let (report_tx, report_rx) = std::sync::mpsc::channel();
        state.signal_reports = Some(report_rx);
        let (_hdmi, panel) = hybrid_laptop(&mut state);

        // The blank leaves both live; the wake owes each a re-light.
        state.note_wake("woke");
        // The panel's power-on commit on the integrated card failed.
        report_tx.send(link_down(panel)).expect("report");
        assert!(state.drain_signal_reports());
        assert!(
            state.link_failures.is_empty(),
            "the panel is not held dark: it is brought up again from scratch"
        );
        assert!(state.held_dark_lines(&BTreeMap::new()).is_empty());

        // The retry failed too: now it is held dark, and the journal says
        // so beside the topology on every pass, whichever card it is on.
        let panel = adopt_on(&mut state, IGPU, 126, "eDP-1", (1920, 1080), (340, 190));
        report_tx.send(link_down(panel)).expect("report");
        assert!(state.drain_signal_reports());
        assert!(state.link_failures.contains_key(&PANEL));
        assert_eq!(
            state.held_dark_lines(&BTreeMap::new()),
            [format!(
                "tilo compositor: DRM connector attempt card=dev:{IGPU} connector=eDP-1 mode=0x0 result=failed reason=\"held dark: it could not be lit (LinkDown); tried again on a replug, a display settings change, a wake or the power key\""
            )]
        );
    }

    #[test]
    fn an_output_failing_outside_a_wake_is_held_dark() {
        let (mut state, _rx) = dispatch_with_channel();
        let (report_tx, report_rx) = std::sync::mpsc::channel();
        state.signal_reports = Some(report_rx);
        let (_hdmi, panel) = hybrid_laptop(&mut state);
        report_tx.send(link_down(panel)).expect("report");
        assert!(state.drain_signal_reports());
        assert!(
            state.link_failures.contains_key(&PANEL),
            "without a wake there is no bring-up owed"
        );
    }

    #[test]
    fn a_display_change_on_one_card_retries_an_output_held_dark_on_the_other() {
        let (mut state, _rx) = dispatch_with_channel();
        let (report_tx, report_rx) = std::sync::mpsc::channel();
        state.signal_reports = Some(report_rx);
        let (_hdmi, panel) = hybrid_laptop(&mut state);
        report_tx.send(link_down(panel)).expect("report");
        assert!(state.drain_signal_reports());
        assert!(state.link_failures.contains_key(&PANEL));

        // HDMI on the discrete card is replugged: the set of connected
        // displays the last pass saw is no longer the one there now.
        state.display_set =
            settings_store::hardware::DisplaySet::of([settings_store::hardware::AttachedOutput {
                connector: "HDMI-A-1",
                connector_label: "HDMI-A-1",
                monitor: None,
            }]);
        state.reapply_outputs_from_cache("udev change on card0");
        assert!(
            state.link_failures.is_empty(),
            "the panel on the integrated card is reconsidered with the HDMI change"
        );
    }

    #[test]
    fn an_active_seat_without_input_devices_is_reopened_until_they_return() {
        use super::InputRecovery;
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut recovery = InputRecovery::default();
        // The resume left libinput with no device: a grace period first,
        // for devices whose re-add is still queued.
        assert_eq!(recovery.due(true, t0), None);
        assert_eq!(recovery.due(true, t0 + Duration::from_secs(1)), None);
        assert_eq!(recovery.due(true, t0 + Duration::from_secs(2)), Some(1));
        // Still none: tried again, later each time, but never given up.
        assert_eq!(recovery.due(true, t0 + Duration::from_secs(3)), None);
        assert_eq!(recovery.due(true, t0 + Duration::from_secs(6)), Some(2));
        let mut at = t0 + Duration::from_secs(6);
        for attempt in 3..12 {
            at += Duration::from_secs(60);
            assert_eq!(recovery.due(true, at), Some(attempt));
        }
        // The keyboard and pointer are back: nothing more is done, and a
        // later loss starts over with the short grace.
        assert_eq!(recovery.due(false, at), None);
        assert_eq!(recovery.due(true, at), None);
        assert_eq!(recovery.due(true, at + Duration::from_secs(2)), Some(1));
    }

    #[test]
    fn a_wake_or_the_power_key_lights_an_output_that_could_not_be_lit_again() {
        let (mut state, _rx) = dispatch_with_channel();
        let (report_tx, report_rx) = std::sync::mpsc::channel();
        state.signal_reports = Some(report_rx);
        let panel = adopt_on(&mut state, DGPU, 43, "eDP-1", (2560, 1600), (300, 190));
        state.connector_probes.insert(
            DGPU,
            vec![ConnectorSnapshot::fake(43, "eDP-1", Some(edid("A")))],
        );
        report_tx
            .send(SignalReport {
                output: panel,
                status: OutputSignalStatus {
                    link_failure: Some(settings_store::hardware::OutputUnavailable::LinkDown),
                    ..OutputSignalStatus::default()
                },
            })
            .expect("report");
        assert!(state.drain_signal_reports());
        assert!(state.live.is_empty(), "the failed panel left the desktop");

        assert!(state.retry_failed_outputs("woke from sleep"));
        assert!(
            state.link_failures.is_empty(),
            "the panel is no longer held off the desktop"
        );
        assert!(
            !state.retry_failed_outputs("power key"),
            "nothing is left to light"
        );
    }
}
