//! Gamma ramps: night light and `zwlr_gamma_control_v1`.
//!
//! An output's display engine maps every pixel it sends through a lookup
//! table per channel after composition, so a ramp tints or corrects
//! everything shown, a fullscreen client's directly scanned-out buffer
//! included, at no cost to drawing. Two things set one:
//!
//! - the session's night light ([`crate::Compositor::set_night_light`]): a
//!   colour temperature, as a ramp that keeps red and lowers green and blue
//!   (a multiply of the encoded signal, so blacks stay black);
//! - a tool speaking `zwlr_gamma_control_v1` (gammastep, wlsunset, a
//!   calibration loader). The protocol gives one control per output
//!   exclusive access, so while a tool holds an output's control its ramp
//!   replaces night light there; destroying the control puts night light
//!   (or no ramp) back. Ramps change every pixel on the screen, so only
//!   clients the session trusts see the global (as with output
//!   management).
//!
//! Backends say which outputs carry a ramp and of what size
//! ([`Gamma::set_size`]); [`Gamma::changes`] says which ramps they have to
//! write, applied after every dispatch ([`crate::Backend::set_gamma`]).

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt as _;

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::{
    zwlr_gamma_control_manager_v1::{self, ZwlrGammaControlManagerV1},
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource as _,
};

use crate::state::{ClientState, State};

/// The protocol version offered.
const VERSION: u32 = 1;

/// The warmest and coolest colour temperatures night light takes, kelvin.
pub const NIGHT_LIGHT_KELVIN: std::ops::RangeInclusive<u32> = 2_000..=6_500;

/// One lookup table per channel, each of the output's ramp size: entry `i`
/// is what a signal of `i / (size - 1)` of full scale is sent as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ramp {
    pub red: Vec<u16>,
    pub green: Vec<u16>,
    pub blue: Vec<u16>,
}

impl Ramp {
    /// The entries per channel.
    pub fn size(&self) -> usize {
        self.red.len()
    }

    /// The ramp of `size` that changes nothing.
    pub fn identity(size: usize) -> Self {
        // Night light at its coolest is no tint at all.
        Self::night_light(size, *NIGHT_LIGHT_KELVIN.end())
    }

    /// A gamma control's table: the red, green and blue ramps one after
    /// the other, native-endian `u16`s. `None` unless it holds exactly
    /// three ramps of `size`.
    fn from_table(table: &[u8], size: usize) -> Option<Self> {
        if size == 0 || table.len() != size.checked_mul(6)? {
            return None;
        }
        let mut values = table
            .chunks_exact(2)
            .filter_map(|pair| Some(u16::from_ne_bytes(*pair.first_chunk::<2>()?)));
        let mut ramp = || values.by_ref().take(size).collect::<Vec<_>>();
        Some(Self {
            red: ramp(),
            green: ramp(),
            blue: ramp(),
        })
    }

    /// Night light at `kelvin` (clamped to [`NIGHT_LIGHT_KELVIN`]) as a
    /// ramp of `size`: red stays at unity while green and blue fall as
    /// warmth rises. The multipliers are deliberately conservative: the
    /// temperature is a comfort target, not a claim that an uncalibrated
    /// panel now shows reference colours. 6500 K changes nothing.
    pub fn night_light(size: usize, kelvin: u32) -> Self {
        let (coolest, warmest) = (*NIGHT_LIGHT_KELVIN.end(), *NIGHT_LIGHT_KELVIN.start());
        let warmth = f64::from(coolest.saturating_sub(kelvin.clamp(warmest, coolest)))
            / f64::from(coolest.saturating_sub(warmest));
        let scaled = |multiplier: f64| -> Vec<u16> {
            let last = size.saturating_sub(1).max(1);
            (0..size)
                .map(|i| {
                    #[allow(
                        clippy::cast_precision_loss,
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "a ramp index is at most a few thousand; the value is clamped to u16"
                    )]
                    let value = ((i as f64 / last as f64) * multiplier * f64::from(u16::MAX))
                        .round()
                        .clamp(0.0, f64::from(u16::MAX)) as u16;
                    value
                })
                .collect()
        };
        Self {
            red: scaled(1.0),
            green: scaled(1.0 - 52.0 / 255.0 * warmth),
            blue: scaled(1.0 - 132.0 / 255.0 * warmth),
        }
    }

    /// Map 8-bit R, G, B, A pixels through the ramp as a display engine
    /// would (alpha untouched).
    pub fn apply(&self, rgba: &mut [u8]) {
        let size = self.size();
        if size < 2 {
            return;
        }
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "a byte times a ramp size of a few thousand, over 255"
        )]
        let look = |ramp: &[u16], value: u8| -> u8 {
            let index = usize::from(value) * (size - 1) / 255;
            ramp.get(index)
                .map_or(value, |entry| entry.to_be_bytes()[0])
        };
        for pixel in rgba.chunks_exact_mut(4) {
            if let [red, green, blue, _] = pixel {
                *red = look(&self.red, *red);
                *green = look(&self.green, *green);
                *blue = look(&self.blue, *blue);
            }
        }
    }
}

/// Who wants which ramp on which output, and what the backend holds.
#[derive(Default)]
pub(crate) struct Gamma {
    /// The ramp size of each output that carries a ramp.
    sizes: BTreeMap<String, usize>,
    /// Live controls and the output each holds.
    controls: Vec<(ZwlrGammaControlV1, String)>,
    /// The ramp each control set, by output.
    set: BTreeMap<String, Ramp>,
    /// Night light's colour temperature, while it is on.
    night_light: Option<u32>,
    /// What the backend was last told per output (`None`: no ramp);
    /// absent when unknown (new, or after another session had the device),
    /// so it is written.
    applied: BTreeMap<String, Option<Ramp>>,
}

impl Gamma {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        display.create_global::<State, ZwlrGammaControlManagerV1, ()>(VERSION, ());
        Self::default()
    }

    /// `name` carries ramps of `size` entries (`None`: none at all).
    pub(crate) fn set_size(&mut self, name: &str, size: Option<usize>) {
        if let Some(size) = size.filter(|size| *size >= 2) {
            if self.sizes.insert(name.to_owned(), size) != Some(size) {
                // A ramp of another size no longer fits.
                self.fail_controls(name);
            }
        } else {
            self.sizes.remove(name);
            self.fail_controls(name);
        }
    }

    /// Turn night light on at `kelvin`, or off.
    pub(crate) fn set_night_light(&mut self, kelvin: Option<u32>) {
        self.night_light = kelvin;
    }

    pub(crate) const fn night_light(&self) -> Option<u32> {
        self.night_light
    }

    /// The ramp `name` should show: a tool's, else night light's, else
    /// none.
    pub(crate) fn wanted(&self, name: &str) -> Option<Ramp> {
        if let Some(ramp) = self.set.get(name) {
            return Some(ramp.clone());
        }
        let size = *self.sizes.get(name)?;
        self.night_light
            .map(|kelvin| Ramp::night_light(size, kelvin))
    }

    /// The ramps the backend has to write so the outputs named in
    /// `outputs` show what is wanted (`None`: take the ramp off). Forgets
    /// outputs that are gone and fails their controls.
    pub(crate) fn changes(&mut self, outputs: &[String]) -> Vec<(String, Option<Ramp>)> {
        let gone: Vec<String> = self
            .sizes
            .keys()
            .filter(|name| !outputs.contains(name))
            .cloned()
            .collect();
        for name in gone {
            self.set_size(&name, None);
        }
        self.applied.retain(|name, _| self.sizes.contains_key(name));
        self.sizes
            .keys()
            .filter_map(|name| {
                let wanted = self.wanted(name);
                (self.applied.get(name) != Some(&wanted)).then(|| (name.clone(), wanted))
            })
            .collect()
    }

    /// The backend wrote `ramp` to `name`.
    pub(crate) fn applied(&mut self, name: &str, ramp: Option<Ramp>) {
        self.applied.insert(name.to_owned(), ramp);
    }

    /// The backend could not write `name`'s ramp: a tool's control fails
    /// (it is no longer in effect) and the output is not tried again until
    /// what is wanted changes.
    pub(crate) fn refused(&mut self, name: &str) {
        if self.set.remove(name).is_some() {
            self.fail_controls(name);
        }
        let wanted = self.wanted(name);
        self.applied(name, wanted);
    }

    /// What the device holds is unknown (another session had the display):
    /// every ramp is written again.
    pub(crate) fn forget_applied(&mut self) {
        self.applied.clear();
    }

    fn fail_controls(&mut self, name: &str) {
        self.set.remove(name);
        self.controls.retain(|(control, output)| {
            let keep = output != name;
            if !keep {
                control.failed();
            }
            keep
        });
    }
}

/// The output a control holds; `None` when it never held one (it failed at
/// once and stays inert).
pub struct ControlData {
    output: Option<String>,
}

impl GlobalDispatch<ZwlrGammaControlManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrGammaControlManagerV1>,
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

impl Dispatch<ZwlrGammaControlManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrGammaControlManagerV1,
        request: zwlr_gamma_control_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let zwlr_gamma_control_manager_v1::Request::GetGammaControl { id, output } = request else {
            return;
        };
        let gamma = &mut state.gamma;
        // One control per output: a second one fails at once.
        let held = Output::from_resource(&output)
            .map(|output| output.name())
            .filter(|name| !gamma.controls.iter().any(|(_, held)| held == name))
            .and_then(|name| Some((gamma.sizes.get(&name).copied()?, name)));
        let control = data_init.init(
            id,
            ControlData {
                output: held.as_ref().map(|(_, name)| name.clone()),
            },
        );
        match held {
            Some((size, name)) => {
                control.gamma_size(u32::try_from(size).unwrap_or(u32::MAX));
                gamma.controls.push((control, name));
            }
            None => control.failed(),
        }
    }
}

impl Dispatch<ZwlrGammaControlV1, ControlData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        control: &ZwlrGammaControlV1,
        request: zwlr_gamma_control_v1::Request,
        data: &ControlData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let zwlr_gamma_control_v1::Request::SetGamma { fd } = request else {
            return;
        };
        let gamma = &mut state.gamma;
        let Some(name) = data
            .output
            .as_ref()
            .filter(|_| gamma.controls.iter().any(|(held, _)| held == control))
        else {
            return;
        };
        let Some(size) = gamma.sizes.get(name).copied() else {
            return;
        };
        match read_table(fd, size) {
            Some(ramp) => {
                gamma.set.insert(name.clone(), ramp);
            }
            None => control.post_error(
                zwlr_gamma_control_v1::Error::InvalidGamma,
                "the gamma table is not three ramps of the gamma size",
            ),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        control: &ZwlrGammaControlV1,
        data: &ControlData,
    ) {
        let gamma = &mut state.gamma;
        let held = gamma.controls.iter().any(|(bound, _)| bound == control);
        gamma.controls.retain(|(bound, _)| bound != control);
        // The table it set goes with it.
        if held && let Some(name) = &data.output {
            gamma.set.remove(name);
        }
    }
}

/// Read a control's table from the start of `fd`: three ramps of `size`.
fn read_table(fd: OwnedFd, size: usize) -> Option<Ramp> {
    let file = std::fs::File::from(fd);
    let mut table = vec![0_u8; size.checked_mul(6)?];
    file.read_exact_at(&mut table, 0).ok()?;
    // A longer table is as wrong as a shorter one.
    let mut extra = [0_u8; 1];
    if file
        .read_at(&mut extra, u64::try_from(table.len()).ok()?)
        .ok()?
        != 0
    {
        return None;
    }
    Ramp::from_table(&table, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn night_light_keeps_red_and_lowers_green_then_blue_most() {
        let ramp = Ramp::night_light(256, 2_000);
        assert_eq!(ramp.red[255], u16::MAX);
        assert_eq!(ramp.red[0], 0);
        assert_eq!(ramp.green[255] >> 8, 203);
        assert_eq!(ramp.blue[255] >> 8, 123);
        let neutral = Ramp::night_light(256, 6_500);
        assert_eq!(neutral.red, neutral.blue, "6500 K changes nothing");
        assert_eq!(neutral.green[128], 128 * 257);
        assert_eq!(Ramp::night_light(256, 100), ramp, "clamped to 2000 K");
    }

    #[test]
    fn a_table_is_exactly_three_ramps_of_the_size() {
        let values: Vec<u8> = (0_u16..6).flat_map(u16::to_ne_bytes).collect();
        let ramp = Ramp::from_table(&values, 2).unwrap();
        assert_eq!(
            (ramp.red, ramp.green, ramp.blue),
            (vec![0, 1], vec![2, 3], vec![4, 5])
        );
        assert!(Ramp::from_table(&values, 3).is_none());
        assert!(Ramp::from_table(&values[..10], 2).is_none());
        assert!(Ramp::from_table(&[], 0).is_none());
    }

    #[test]
    fn pixels_go_through_the_ramp_and_alpha_does_not() {
        let mut pixels = [255, 255, 255, 200, 0, 0, 0, 255];
        Ramp::night_light(1024, 2_000).apply(&mut pixels);
        assert_eq!(pixels, [255, 203, 123, 200, 0, 0, 0, 255]);
    }

    #[test]
    fn night_light_is_written_once_per_output_and_taken_off_when_turned_off() {
        let outputs = names(&["eDP-1", "DP-1"]);
        let mut gamma = Gamma::default();
        gamma.set_size("eDP-1", Some(1024));
        assert_eq!(
            gamma.changes(&outputs),
            vec![("eDP-1".to_owned(), None)],
            "a new output is told it has no ramp, whatever it held before"
        );
        gamma.applied("eDP-1", None);
        gamma.set_night_light(Some(3_700));
        let on = gamma.changes(&outputs);
        assert_eq!(on.len(), 1, "only the output that carries a ramp");
        assert_eq!(on[0].1.as_ref().map(Ramp::size), Some(1024));
        for (name, ramp) in on {
            gamma.applied(&name, ramp);
        }
        assert!(gamma.changes(&outputs).is_empty(), "nothing written twice");
        gamma.set_night_light(None);
        assert_eq!(gamma.changes(&outputs), vec![("eDP-1".to_owned(), None)]);
    }

    #[test]
    fn a_tools_ramp_replaces_night_light_until_it_lets_go() {
        let outputs = names(&["eDP-1"]);
        let mut gamma = Gamma::default();
        gamma.set_size("eDP-1", Some(4));
        gamma.set_night_light(Some(2_000));
        gamma.applied("eDP-1", None);
        let tool = Ramp {
            red: vec![0; 4],
            green: vec![1; 4],
            blue: vec![2; 4],
        };
        gamma.set.insert("eDP-1".into(), tool.clone());
        assert_eq!(gamma.changes(&outputs), vec![("eDP-1".into(), Some(tool))]);
        gamma.set.remove("eDP-1");
        assert_eq!(
            gamma.changes(&outputs),
            vec![("eDP-1".into(), Some(Ramp::night_light(4, 2_000)))]
        );
    }

    #[test]
    fn unplugged_outputs_are_forgotten_and_a_resume_writes_every_ramp_again() {
        let mut gamma = Gamma::default();
        gamma.set_size("eDP-1", Some(4));
        gamma.set_size("DP-1", Some(4));
        gamma.applied("eDP-1", None);
        gamma.applied("DP-1", None);
        assert!(gamma.changes(&names(&["eDP-1"])).is_empty());
        assert!(!gamma.sizes.contains_key("DP-1"));
        gamma.forget_applied();
        assert_eq!(
            gamma.changes(&names(&["eDP-1"])),
            vec![("eDP-1".into(), None)]
        );
    }
}
