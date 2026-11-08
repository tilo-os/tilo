//! Settings' Identify: "which screen is which?" answered on the screens themselves.
//! Settings writes an [`IdentifyRequest`](settings_store::display_identify::IdentifyRequest) when
//! the person presses Identify. The session answers it once per press by handing the desk each
//! display's number and name — the same number its tile on Settings' canvas carries, and the same
//! name ([`display_names`](settings_store::display_identify::display_names)), read off the
//! compositor's own display snapshot — and the desk says each on the display it names for a few
//! seconds ([`tilo_desktop::display_identify`]). Nothing here counts down: the desk holds the cards
//! for their moment and asks for the frame that takes them away.

use std::cell::RefCell;
use std::rc::Rc;

use settings_store::display_identify::{IdentifyRequest, display_names, display_numbers};
use settings_store::hardware::DisplayHardwareSnapshot;
use tilo_compositor::OutputInfo;
use tilo_desktop::display_identify::{Ask, Label};

/// The Identify answers this session has given, and the desk's end of them: what the bar reads each
/// pass.
#[derive(Default)]
pub(crate) struct Numbers {
    /// The cards the desk shows now, as the bar reads them.
    ask: Rc<RefCell<Option<Ask>>>,
    /// The request answered last, so one press of Identify shows the cards once.
    answered: Option<u64>,
}

impl Numbers {
    /// What the bar reads: the cards the desk shows, if any.
    pub(crate) fn ask(&self) -> Rc<RefCell<Option<Ask>>> {
        Rc::clone(&self.ask)
    }

    /// Answer Settings' Identify request, if one came since the last pass: each display's number
    /// and name, keyed by the monitor the desk follows it by. A request older than the shell
    /// answers — a restarted session seeing one written minutes ago — is left alone.
    pub(crate) fn follow(
        &mut self,
        snapshot: Option<&DisplayHardwareSnapshot>,
        outputs: &[OutputInfo],
    ) {
        let Some(request) = IdentifyRequest::load() else {
            *self.ask.borrow_mut() = None;
            return;
        };
        if self.answered == Some(request.requested_unix_ms) {
            return;
        }
        if !request.is_fresh(tilo_clock::unix_now_ms()) {
            return;
        }
        self.answered = Some(request.requested_unix_ms);
        let labels = labels_of(snapshot, outputs);
        if labels.is_empty() {
            return;
        }
        *self.ask.borrow_mut() = Some(Ask {
            id: request.requested_unix_ms,
            labels,
        });
    }
}

/// Each display the session shows something on, with the number and name Settings gives it: the
/// numbers and names come from the compositor's own snapshot, so the canvas tile and the physical
/// screen say the same thing.
fn labels_of(snapshot: Option<&DisplayHardwareSnapshot>, outputs: &[OutputInfo]) -> Vec<Label> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let numbers = display_numbers(&snapshot.outputs);
    let names = display_names(&snapshot.outputs);
    snapshot
        .outputs
        .iter()
        .zip(numbers)
        .zip(names)
        .filter_map(|((output, number), name)| {
            let monitor = outputs
                .iter()
                .find(|shown| shown.enabled && shown.name == output.descriptor.connector)?
                .monitor
                .clone();
            Some(Label {
                monitor,
                number,
                name,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use settings_store::hardware::{DisplayOutputSnapshot, OutputDescriptor, OutputMode};

    fn hardware(connector: &str, builtin: bool) -> DisplayOutputSnapshot {
        DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.to_owned(),
                connector_label: connector.to_owned(),
                modes: vec![OutputMode {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: 60_000,
                }],
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: None,
            },
            current_mode: OutputMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            },
            scale: 1.0,
            builtin,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: std::collections::BTreeMap::default(),
        }
    }

    fn snapshot(outputs: Vec<DisplayOutputSnapshot>) -> DisplayHardwareSnapshot {
        DisplayHardwareSnapshot {
            generated_unix_ms: 0,
            backend: "headless".to_owned(),
            publisher_pid: 1,
            publisher_start_ticks: 0,
            outputs,
            layout: Vec::new(),
        }
    }

    fn shown(connector: &str, monitor: &str, enabled: bool) -> OutputInfo {
        OutputInfo {
            name: connector.to_owned(),
            monitor: monitor.to_owned(),
            label: "A display".to_owned(),
            enabled,
            rect: enabled.then_some((0, 0, 1920, 1080)),
            scale: 1.0,
            description: None,
            colour: tilo_color_pipeline::hdr_display::DisplayCapabilities::sdr_default(),
        }
    }

    #[test]
    fn settings_number_and_name_reach_the_display_that_carries_them() {
        let hardware = snapshot(vec![hardware("DP-2", false), hardware("eDP-1", true)]);
        // The desk follows a display by its monitor, not its connector.
        let outputs = vec![
            shown("eDP-1", "edp-monitor", true),
            shown("DP-2", "dp-monitor", true),
            shown("HDMI-A-1", "hdmi-monitor", true),
        ];
        let labels = labels_of(Some(&hardware), &outputs);
        assert_eq!(
            labels.len(),
            2,
            "one card per display the compositor published: {labels:?}"
        );
        let builtin = labels
            .iter()
            .find(|label| label.monitor == "edp-monitor")
            .expect("the panel says which one it is");
        assert_eq!(
            (builtin.number, builtin.name.as_str()),
            (1, "Built-in display"),
            "the panel is one and by the name Settings gives it"
        );
        let external = labels
            .iter()
            .find(|label| label.monitor == "dp-monitor")
            .expect("the monitor says which one it is");
        assert_eq!(
            (external.number, external.name.as_str()),
            (2, "External display"),
            "the external display is two, as on the canvas"
        );
        assert!(
            !labels.iter().any(|label| label.monitor == "hdmi-monitor"),
            "a monitor only the desk knows gets no card"
        );
    }

    #[test]
    fn a_display_the_desk_shows_nothing_on_gets_no_card() {
        let hardware = snapshot(vec![hardware("eDP-1", true), hardware("DP-2", false)]);
        let outputs = vec![
            shown("eDP-1", "edp-monitor", true),
            shown("DP-2", "dp-monitor", false),
        ];
        let labels = labels_of(Some(&hardware), &outputs);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].monitor, "edp-monitor");
    }

    #[test]
    fn nothing_is_named_before_the_compositor_published_its_displays() {
        let outputs = vec![shown("eDP-1", "edp-monitor", true)];
        assert!(labels_of(None, &outputs).is_empty());
        assert!(labels_of(None, &[]).is_empty());
    }
}
