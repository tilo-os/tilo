//! How the displays are used together: side by side, showing the same, or
//! only the built-in or only the other ones. The display key steps through
//! them ([`DisplayMode::Next`]); Settings and a script set one by name.

use serde::{Deserialize, Serialize};
use tilo_compositor::{OutputChange, OutputInfo};

/// A way of using the displays together, or the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayMode {
    /// The one after the current, in [`ARRANGEMENTS`] order.
    Next,
    /// Every display on, side by side: one desktop across them.
    Extend,
    /// Every display on, each showing the same.
    Mirror,
    /// Only the built-in display.
    BuiltIn,
    /// Only the other displays.
    External,
}

/// The arrangements, in the order the display key steps through them.
pub const ARRANGEMENTS: [DisplayMode; 4] = [
    DisplayMode::Extend,
    DisplayMode::Mirror,
    DisplayMode::BuiltIn,
    DisplayMode::External,
];

/// The names the `set-mode` action takes, in [`ARRANGEMENTS`] order.
pub const NAMES: [&str; 4] = ["extend", "mirror", "built-in", "external"];

impl DisplayMode {
    /// The arrangement named `name` ([`NAMES`]).
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        NAMES
            .iter()
            .zip(ARRANGEMENTS)
            .find_map(|(known, mode)| (*known == name).then_some(mode))
    }
}

/// Whether `name` is a built-in panel's connector.
fn built_in(name: &str) -> bool {
    ["eDP", "LVDS", "DSI"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The built-in display: the first panel connector, else the first output.
fn internal(outputs: &[OutputInfo]) -> Option<&str> {
    outputs
        .iter()
        .find(|output| built_in(&output.name))
        .or_else(|| outputs.first())
        .map(|output| output.name.as_str())
}

/// The arrangement `outputs` show now.
#[must_use]
pub fn current(outputs: &[OutputInfo]) -> DisplayMode {
    let internal = internal(outputs);
    let on: Vec<&OutputInfo> = outputs.iter().filter(|output| output.enabled).collect();
    if on.len() < outputs.len() {
        return if on
            .iter()
            .any(|output| Some(output.name.as_str()) == internal)
        {
            DisplayMode::BuiltIn
        } else {
            DisplayMode::External
        };
    }
    let corner = |output: &&OutputInfo| output.rect.map(|(x, y, _, _)| (x, y));
    let first = on.first().and_then(corner);
    if on.len() > 1 && on.iter().all(|output| corner(output) == first) {
        DisplayMode::Mirror
    } else {
        DisplayMode::Extend
    }
}

/// The changes that arrange `outputs` as `mode`; none with a single
/// display, where there is nothing to arrange.
#[must_use]
pub fn arrange(mode: DisplayMode, outputs: &[OutputInfo]) -> Vec<OutputChange> {
    if outputs.len() < 2 {
        return Vec::new();
    }
    let mode = match mode {
        DisplayMode::Next => {
            let now = current(outputs);
            ARRANGEMENTS
                .iter()
                .cycle()
                .skip_while(|mode| **mode != now)
                .nth(1)
                .copied()
                .unwrap_or(DisplayMode::Extend)
        }
        mode => mode,
    };
    let internal = internal(outputs).map(str::to_owned);
    let is_internal = |output: &OutputInfo| Some(&output.name) == internal.as_ref();
    match mode {
        DisplayMode::Next | DisplayMode::Extend => {
            // Left to right: the displays on now in the order they are
            // known, then those off, whose size is not known yet.
            let mut x = 0;
            let (lit, dark): (Vec<&OutputInfo>, Vec<&OutputInfo>) =
                outputs.iter().partition(|output| output.rect.is_some());
            lit.into_iter()
                .chain(dark)
                .map(|output| {
                    let change = OutputChange {
                        position: Some((x, 0)),
                        ..OutputChange::enable(output.name.clone())
                    };
                    if let Some((_, _, width, _)) = output.rect {
                        x = x.saturating_add(width);
                    }
                    change
                })
                .collect()
        }
        DisplayMode::Mirror => outputs
            .iter()
            .map(|output| OutputChange {
                position: Some((0, 0)),
                ..OutputChange::enable(output.name.clone())
            })
            .collect(),
        DisplayMode::BuiltIn | DisplayMode::External => {
            let keep = mode == DisplayMode::BuiltIn;
            outputs
                .iter()
                .map(|output| {
                    if is_internal(output) == keep {
                        OutputChange::enable(output.name.clone())
                    } else {
                        OutputChange::disable(output.name.clone())
                    }
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(name: &str, x: i32) -> OutputInfo {
        OutputInfo {
            name: name.into(),
            enabled: true,
            rect: Some((x, 0, 1280, 800)),
        }
    }

    #[test]
    fn the_display_key_steps_from_side_by_side_to_the_same_to_one_and_the_other() {
        let mut outputs = vec![on("HDMI-A-1", 1280), on("eDP-1", 0)];
        assert_eq!(current(&outputs), DisplayMode::Extend);
        let mirror = arrange(DisplayMode::Next, &outputs);
        assert!(mirror.iter().all(|change| change.position == Some((0, 0))));
        outputs = vec![on("HDMI-A-1", 0), on("eDP-1", 0)];
        assert_eq!(current(&outputs), DisplayMode::Mirror);
        let built_in = arrange(DisplayMode::Next, &outputs);
        assert_eq!(
            built_in
                .iter()
                .map(|change| (change.name.as_str(), change.enabled))
                .collect::<Vec<_>>(),
            [("HDMI-A-1", false), ("eDP-1", true)],
            "the panel is the built-in display wherever it is listed"
        );
        outputs[0].enabled = false;
        outputs[0].rect = None;
        assert_eq!(current(&outputs), DisplayMode::BuiltIn);
        assert!(!arrange(DisplayMode::Next, &outputs)[1].enabled);
        assert!(arrange(DisplayMode::Next, &outputs[1..]).is_empty());
        assert_eq!(DisplayMode::named("built-in"), Some(DisplayMode::BuiltIn));
    }
}
