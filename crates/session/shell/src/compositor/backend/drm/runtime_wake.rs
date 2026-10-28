//! How the session's displays and input come back after a wake: outputs
//! that could not be lit are lit again, and the flight record notes the
//! input the session holds and the first event after the wake.

use smithay::backend::input::InputEvent;
use smithay::backend::libinput::LibinputInputBackend;

use super::super::wake_check::WakeStage;
use super::{DrmDispatchState, ScanoutCommand, device_caps};

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
        // A wake is the person asking for the displays: one that could not
        // be lit gets another attempt.
        self.retry_failed_outputs(reason);
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
    use std::sync::mpsc::channel;

    use super::super::tests::{DGPU, adopt_on, dispatch_with_channel, edid};
    use super::super::{ConnectorSnapshot, OutputSignalStatus, SignalReport};

    #[test]
    fn a_wake_or_the_power_key_lights_an_output_that_could_not_be_lit_again() {
        let (mut state, _rx) = dispatch_with_channel();
        let (report_tx, report_rx) = channel();
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
