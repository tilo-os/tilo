//! How the session's displays and input come back after a wake: outputs
//! that could not be lit are lit again, and the flight record notes the
//! input the session holds and the first event after the wake.

use smithay::backend::input::InputEvent;
use smithay::backend::libinput::LibinputInputBackend;

use super::super::wake_check::WakeStage;
use super::{DrmDispatchState, ScanoutCommand, device_caps};

impl DrmDispatchState {
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
pub(super) fn libinput_event_kind(
    event: &InputEvent<LibinputInputBackend>,
) -> Option<&'static str> {
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
