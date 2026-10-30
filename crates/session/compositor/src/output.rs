//! Outputs: the screens the compositor shows the desktop on.
//!
//! An output has a mode in physical pixels and a scale; its place in the
//! logical layout is its mode over its scale, which is what clients see
//! (`wl_output`, `xdg_output`) and the pointer moves in.

use std::time::Duration;

use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::utils::{Logical, Point, Transform};

use crate::state::State;

/// The refresh interval assumed when a mode reports none (60 Hz).
const FALLBACK_INTERVAL: Duration = Duration::from_micros(16_667);

/// The time between frames at `refresh_mhz` millihertz; 60 Hz when the
/// mode reports no rate.
pub(crate) fn refresh_interval(refresh_mhz: i32) -> Duration {
    u64::try_from(refresh_mhz)
        .ok()
        .and_then(|mhz| 1_000_000_000_000_u64.checked_div(mhz))
        .map_or(FALLBACK_INTERVAL, Duration::from_nanos)
}

/// A mode a configuration asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeRequest {
    /// Physical pixels.
    pub size: (i32, i32),
    /// Millihertz; `None` takes the fastest at that size.
    pub refresh_mhz: Option<i32>,
}

/// An output as the session sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    /// The connector name (`eDP-1`).
    pub name: String,
    /// Whether it is on.
    pub enabled: bool,
    /// Its logical rectangle `(x, y, w, h)` while it is on.
    pub rect: Option<(i32, i32, i32, i32)>,
}

/// How a configuration (a display tool, Settings) changes one output.
/// Unset fields keep what the output has.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputChange {
    /// The output's connector name.
    pub name: String,
    /// Whether the output stays on.
    pub enabled: bool,
    pub mode: Option<ModeRequest>,
    /// The top-left corner in the logical layout.
    pub position: Option<(i32, i32)>,
    pub scale: Option<f64>,
    pub transform: Option<Transform>,
}

impl OutputChange {
    /// Keep `name` on, changing nothing yet.
    pub fn enable(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            mode: None,
            position: None,
            scale: None,
            transform: None,
        }
    }

    /// Turn `name` off.
    pub fn disable(name: impl Into<String>) -> Self {
        Self {
            enabled: false,
            ..Self::enable(name)
        }
    }
}

/// Why a configuration cannot be applied, before any backend looks at it:
/// an unknown output, an unusable scale, or every output turned off.
/// `outputs` are every known output's name and whether it is on now.
pub(crate) fn check_changes(
    changes: &[OutputChange],
    outputs: &[(String, bool)],
) -> Result<(), String> {
    for change in changes {
        if !outputs.iter().any(|(name, _)| name == &change.name) {
            return Err(format!("{}: no such output", change.name));
        }
        if change
            .scale
            .is_some_and(|scale| !(scale.is_finite() && scale > 0.0))
        {
            return Err(format!("{}: unusable scale", change.name));
        }
    }
    let all_off = outputs.iter().all(|(name, on)| {
        changes
            .iter()
            .find(|change| &change.name == name)
            .map_or(!on, |change| !change.enabled)
    });
    if all_off && !outputs.is_empty() {
        return Err("every output would be off".into());
    }
    Ok(())
}

/// The mode of `modes` a request names: that size at that refresh (within
/// half a hertz), or the fastest at that size when it names no refresh.
pub(crate) fn find_mode(modes: &[Mode], request: ModeRequest) -> Option<Mode> {
    let at_size = modes
        .iter()
        .filter(|mode| (mode.size.w, mode.size.h) == request.size);
    match request.refresh_mhz {
        Some(refresh) => at_size
            .filter(|mode| mode.refresh.abs_diff(refresh) <= 500)
            .min_by_key(|mode| mode.refresh.abs_diff(refresh))
            .copied(),
        None => at_size.max_by_key(|mode| mode.refresh).copied(),
    }
}

/// Apply the scale, rotation and place of `change` to `output` (the mode is
/// the backend's), moving it in the layout. The caller re-arranges windows
/// once every output is done.
pub(crate) fn apply_placement(state: &mut State, output: &Output, change: &OutputChange) {
    let position = change.position.map(Point::<i32, Logical>::from);
    output.change_current_state(
        None,
        change.transform,
        change.scale.map(Scale::Fractional),
        position,
    );
    if let Some(position) = position {
        state.space.map_output(output, position);
    }
}

/// Where outputs go in the logical layout, in the order given: an output
/// named in `fixed` at its place, the others packed left to right (top
/// edge at 0) after every fixed one, so outputs that come and go leave no
/// holes. `outputs` are `(name, logical width)`.
pub(crate) fn arrange(
    outputs: &[(String, i32)],
    fixed: &std::collections::HashMap<String, (i32, i32)>,
) -> Vec<(i32, i32)> {
    let mut next_x = outputs
        .iter()
        .filter_map(|(name, width)| {
            fixed
                .get(name)
                .map(|(x, _)| x.saturating_add((*width).max(0)))
        })
        .max()
        .unwrap_or(0);
    outputs
        .iter()
        .map(|(name, width)| {
            fixed.get(name).copied().unwrap_or_else(|| {
                let at = (next_x, 0);
                next_x = next_x.saturating_add((*width).max(0));
                at
            })
        })
        .collect()
}

/// One output as a backend describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputConfig {
    /// The connector name (`eDP-1`, `HEADLESS-1`), unique per session.
    pub name: String,
    /// The mode, in physical pixels.
    pub size: (i32, i32),
    /// The scale clients render at (`1.0`, `1.5`, `2.0`).
    pub scale: f64,
    /// The top-left corner in the logical layout.
    pub position: (i32, i32),
    /// The refresh rate in millihertz.
    pub refresh_mhz: i32,
}

impl OutputConfig {
    /// A 60 Hz output at scale 1 at the origin.
    pub fn new(name: impl Into<String>, width: i32, height: i32) -> Self {
        Self {
            name: name.into(),
            size: (width, height),
            scale: 1.0,
            position: (0, 0),
            refresh_mhz: 60_000,
        }
    }

    /// The same output at another scale.
    #[must_use]
    pub const fn with_scale(mut self, scale: f64) -> Self {
        self.scale = scale;
        self
    }

    /// The same output at another place in the logical layout.
    #[must_use]
    pub const fn at(mut self, x: i32, y: i32) -> Self {
        self.position = (x, y);
        self
    }

    pub(crate) fn location(&self) -> Point<i32, Logical> {
        Point::from(self.position)
    }

    /// The output, advertised to clients.
    pub(crate) fn create(&self, display: &DisplayHandle, model: &str) -> Output {
        let output = Output::new(
            self.name.clone(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Tilo".into(),
                model: model.into(),
            },
        );
        let mode = Mode {
            size: self.size.into(),
            refresh: self.refresh_mhz,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Fractional(self.scale)),
            Some(self.location()),
        );
        output.set_preferred(mode);
        advertise(&output, display);
        output
    }
}

/// An output's `wl_output` global while clients see it.
struct Global(std::cell::RefCell<Option<GlobalId>>);

/// Show `output` to clients (`wl_output`); once, however often called.
pub(crate) fn advertise(output: &Output, display: &DisplayHandle) {
    output
        .user_data()
        .insert_if_missing(|| Global(std::cell::RefCell::new(None)));
    if let Some(global) = output.user_data().get::<Global>() {
        let mut global = global.0.borrow_mut();
        if global.is_none() {
            *global = Some(output.create_global::<State>(display));
        }
    }
}

/// Stop showing `output` to clients (it was turned off or unplugged).
pub(crate) fn withdraw(output: &Output, display: &DisplayHandle) {
    if let Some(id) = output
        .user_data()
        .get::<Global>()
        .and_then(|global| global.0.borrow_mut().take())
    {
        display.remove_global::<State>(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(w: i32, h: i32, refresh: i32) -> Mode {
        Mode {
            size: (w, h).into(),
            refresh,
        }
    }

    #[test]
    fn requested_modes_match_size_and_refresh_or_take_the_fastest() {
        let modes = [
            mode(1920, 1080, 60_000),
            mode(1920, 1080, 143_981),
            mode(1280, 720, 60_000),
        ];
        let request = |size, refresh_mhz| ModeRequest { size, refresh_mhz };
        assert_eq!(
            find_mode(&modes, request((1920, 1080), None)),
            Some(modes[1])
        );
        assert_eq!(
            find_mode(&modes, request((1920, 1080), Some(144_000))),
            Some(modes[1])
        );
        assert_eq!(
            find_mode(&modes, request((1920, 1080), Some(59_940))),
            Some(modes[0])
        );
        assert_eq!(find_mode(&modes, request((1920, 1080), Some(75_000))), None);
        assert_eq!(find_mode(&modes, request((800, 600), None)), None);
    }

    #[test]
    fn configurations_name_real_outputs_keep_one_on_and_use_real_scales() {
        let outputs = vec![("eDP-1".to_owned(), true), ("DP-1".to_owned(), true)];
        let scaled = OutputChange {
            scale: Some(1.5),
            ..OutputChange::enable("DP-1")
        };
        assert_eq!(
            check_changes(&[scaled, OutputChange::disable("eDP-1")], &outputs),
            Ok(())
        );
        assert!(check_changes(&[OutputChange::enable("HDMI-A-9")], &outputs).is_err());
        let zero = OutputChange {
            scale: Some(0.0),
            ..OutputChange::enable("DP-1")
        };
        assert!(check_changes(&[zero], &outputs).is_err());
        assert!(
            check_changes(
                &[
                    OutputChange::disable("eDP-1"),
                    OutputChange::disable("DP-1")
                ],
                &outputs
            )
            .is_err()
        );
        // An output already off counts as off; turning it back on is fine.
        let one_off = vec![("eDP-1".to_owned(), true), ("DP-1".to_owned(), false)];
        assert!(check_changes(&[OutputChange::disable("eDP-1")], &one_off).is_err());
        assert_eq!(
            check_changes(
                &[OutputChange::disable("eDP-1"), OutputChange::enable("DP-1")],
                &one_off
            ),
            Ok(())
        );
    }

    #[test]
    fn unplaced_outputs_pack_left_to_right_after_placed_ones() {
        let outputs = |list: &[(&str, i32)]| -> Vec<(String, i32)> {
            list.iter()
                .map(|(name, w)| ((*name).to_owned(), *w))
                .collect()
        };
        let none = std::collections::HashMap::new();
        // Pulling the middle display closes the hole it left.
        assert_eq!(
            arrange(
                &outputs(&[("eDP-1", 1280), ("DP-1", 1920), ("DP-2", 2560)]),
                &none
            ),
            vec![(0, 0), (1280, 0), (3200, 0)]
        );
        assert_eq!(
            arrange(&outputs(&[("eDP-1", 1280), ("DP-2", 2560)]), &none),
            vec![(0, 0), (1280, 0)]
        );
        let fixed = std::collections::HashMap::from([("DP-1".to_owned(), (0, -1080))]);
        assert_eq!(
            arrange(&outputs(&[("eDP-1", 1280), ("DP-1", 1920)]), &fixed),
            vec![(1920, 0), (0, -1080)]
        );
    }

    #[test]
    fn refresh_intervals_follow_the_mode_and_fall_back_to_60_hz() {
        assert_eq!(refresh_interval(60_000), Duration::from_nanos(16_666_666));
        assert_eq!(refresh_interval(120_000), Duration::from_nanos(8_333_333));
        assert_eq!(refresh_interval(0), FALLBACK_INTERVAL);
        assert_eq!(refresh_interval(-1), FALLBACK_INTERVAL);
    }
}
