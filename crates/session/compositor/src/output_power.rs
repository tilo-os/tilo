//! Output power: whether each panel is lit.
//!
//! Two things switch a panel off: idle blanking (the whole session, until
//! input wakes it) and `zwlr_output_power_manager_v1` (one output, for tools
//! such as `wlopm`, until the tool turns it on again). Blanking does not
//! replace a tool's per-output intent: a panel a tool turned off stays off
//! when input wakes the others. The global switches displays off, so only
//! clients the session trusts see it (as with output management).
//!
//! [`OutputPower::changes`] says which outputs the backend has to switch;
//! the compositor applies them after every dispatch
//! ([`crate::Backend::set_power`]).

use std::collections::{BTreeMap, BTreeSet};

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::{
    zwlr_output_power_manager_v1::{self, ZwlrOutputPowerManagerV1},
    zwlr_output_power_v1::{self, ZwlrOutputPowerV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource as _, WEnum,
};

use crate::state::{ClientState, State};

/// The protocol version offered.
const VERSION: u32 = 1;

/// Who wants which panel lit, and what the backend was last told.
#[derive(Default)]
pub(crate) struct OutputPower {
    /// Power objects and the output each controls.
    objects: Vec<(ZwlrOutputPowerV1, String)>,
    /// Outputs a tool switched off.
    off: BTreeSet<String>,
    /// Idle blanking holds every panel off.
    blanked: bool,
    /// What the backend was last told per output; absent means lit.
    applied: BTreeMap<String, bool>,
}

impl OutputPower {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        display.create_global::<State, ZwlrOutputPowerManagerV1, ()>(VERSION, ());
        Self::default()
    }

    /// Whether `name` should be lit.
    pub(crate) fn wanted(&self, name: &str) -> bool {
        !self.blanked && !self.off.contains(name)
    }

    /// Hold every panel off (idle blanking) or let them light again.
    pub(crate) fn set_blanked(&mut self, blanked: bool) {
        self.blanked = blanked;
    }

    /// The switches the backend has to make so `outputs` (each known
    /// output's name and whether a configuration keeps it on) show what is
    /// wanted, each output once. Forgets outputs that are gone.
    pub(crate) fn changes(&mut self, outputs: &[(String, bool)]) -> Vec<(String, bool)> {
        let known = |name: &String| outputs.iter().any(|(known, _)| known == name);
        self.applied.retain(|name, _| known(name));
        self.off.retain(|name| known(name));
        let changes: Vec<(String, bool)> = outputs
            .iter()
            .filter_map(|(name, enabled)| {
                let wanted = *enabled && self.wanted(name);
                let applied = self.applied.get(name).copied().unwrap_or(true);
                (wanted != applied).then(|| (name.clone(), wanted))
            })
            .collect();
        changes
    }

    /// The backend switched `name` (or failed to: then `on` is what the
    /// panel still shows). Tools are told the panel's state.
    pub(crate) fn applied(&mut self, name: &str, on: bool) {
        self.applied.insert(name.to_owned(), on);
        for (object, _) in self.objects.iter().filter(|(_, output)| output == name) {
            object.mode(mode(on));
        }
    }

    /// Whether the backend was last told to light `name`.
    pub(crate) fn lit(&self, name: &str) -> bool {
        self.applied.get(name).copied().unwrap_or(true)
    }

    /// Fail power objects whose output went away.
    pub(crate) fn retain_outputs(&mut self, outputs: &[String]) {
        self.objects.retain(|(object, name)| {
            let alive = outputs.contains(name);
            if !alive {
                object.failed();
            }
            alive
        });
    }
}

const fn mode(on: bool) -> zwlr_output_power_v1::Mode {
    if on {
        zwlr_output_power_v1::Mode::On
    } else {
        zwlr_output_power_v1::Mode::Off
    }
}

/// The output a power object controls; `None` when it named no output of
/// ours (the object failed at once and stays inert).
pub struct PowerData {
    output: Option<String>,
}

impl GlobalDispatch<ZwlrOutputPowerManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputPowerManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _data: &()) -> bool {
        client
            .get_data::<ClientState>()
            .is_some_and(ClientState::trusted)
    }
}

impl Dispatch<ZwlrOutputPowerManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrOutputPowerManagerV1,
        request: zwlr_output_power_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_output_power_manager_v1::Request::GetOutputPower { id, output } = request {
            let name = Output::from_resource(&output)
                .map(|output| output.name())
                .filter(|name| state.space.outputs().any(|output| &output.name() == name));
            let object = data_init.init(
                id,
                PowerData {
                    output: name.clone(),
                },
            );
            match name {
                Some(name) => {
                    object.mode(mode(state.output_power.lit(&name)));
                    state.output_power.objects.push((object, name));
                }
                None => object.failed(),
            }
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, PowerData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        object: &ZwlrOutputPowerV1,
        request: zwlr_output_power_v1::Request,
        data: &PowerData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let zwlr_output_power_v1::Request::SetMode { mode } = request else {
            return;
        };
        let on = match mode {
            WEnum::Value(zwlr_output_power_v1::Mode::On) => true,
            WEnum::Value(zwlr_output_power_v1::Mode::Off) => false,
            _ => {
                object.post_error(
                    zwlr_output_power_v1::Error::InvalidMode,
                    "unknown power mode",
                );
                return;
            }
        };
        let power = &mut state.output_power;
        let Some(name) = data
            .output
            .as_ref()
            .filter(|_| power.objects.iter().any(|(bound, _)| bound == object))
        else {
            return;
        };
        if on {
            power.off.remove(name);
        } else {
            power.off.insert(name.clone());
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, object: &ZwlrOutputPowerV1, _: &PowerData) {
        state
            .output_power
            .objects
            .retain(|(bound, _)| bound != object);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<(String, bool)> {
        list.iter().map(|name| ((*name).to_owned(), true)).collect()
    }

    #[test]
    fn blanking_switches_every_lit_panel_once_and_waking_lights_them_again() {
        let outputs = names(&["eDP-1", "DP-1"]);
        let mut power = OutputPower::default();
        assert!(power.changes(&outputs).is_empty(), "panels start lit");
        power.set_blanked(true);
        let off = power.changes(&outputs);
        assert_eq!(off, vec![("eDP-1".into(), false), ("DP-1".into(), false)]);
        for (name, on) in off {
            power.applied(&name, on);
        }
        assert!(
            power.changes(&outputs).is_empty(),
            "nothing is switched twice"
        );
        power.set_blanked(false);
        assert_eq!(
            power.changes(&outputs),
            vec![("eDP-1".into(), true), ("DP-1".into(), true)]
        );
    }

    #[test]
    fn a_panel_a_tool_turned_off_stays_off_when_input_wakes_the_others() {
        let outputs = names(&["eDP-1", "DP-1"]);
        let mut power = OutputPower::default();
        power.off.insert("DP-1".into());
        power.applied("DP-1", false);
        power.set_blanked(true);
        for (name, on) in power.changes(&outputs) {
            power.applied(&name, on);
        }
        power.set_blanked(false);
        assert_eq!(power.changes(&outputs), vec![("eDP-1".into(), true)]);
        assert!(!power.wanted("DP-1"));
    }

    #[test]
    fn unplugged_outputs_are_forgotten() {
        let mut power = OutputPower::default();
        power.off.insert("DP-1".into());
        power.applied("DP-1", false);
        assert!(power.changes(&names(&["eDP-1"])).is_empty());
        // Plugged back in, it lights like a new display.
        assert!(power.changes(&names(&["eDP-1", "DP-1"])).is_empty());
        assert!(power.lit("DP-1"));
    }

    #[test]
    fn an_output_turned_off_by_a_configuration_goes_dark_and_comes_back_lit() {
        let mut power = OutputPower::default();
        let off = vec![("eDP-1".to_owned(), true), ("DP-1".to_owned(), false)];
        assert_eq!(power.changes(&off), vec![("DP-1".into(), false)]);
        power.applied("DP-1", false);
        assert_eq!(
            power.changes(&names(&["eDP-1", "DP-1"])),
            vec![("DP-1".into(), true)]
        );
    }
}
