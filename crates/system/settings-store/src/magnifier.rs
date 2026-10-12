//! The desktop magnifier's persisted state in the `accessibility` section.
//!
//! Settings, the compositor's keyboard shortcuts and the frame that paints the magnifier all
//! read and write the same keys; the zoom steps, their labels and the toggle's memory live
//! here so the three cannot disagree about what "3×" or "zoom in" means.

use serde_json::{Map, Value};

/// The section holding the magnifier keys.
pub const SECTION: &str = "accessibility";
/// Current zoom factor; [`OFF`] means the magnifier is off.
pub const ZOOM_KEY: &str = "magnifier_zoom";
/// The zoom the toggle turns back on to.
pub const LAST_ZOOM_KEY: &str = "magnifier_last_zoom";
/// [`MagnifierView::as_str`] of the chosen view.
pub const VIEW_KEY: &str = "magnifier_view";

/// Zoom factor meaning "off".
pub const OFF: u8 = 1;
/// The zoom a first toggle turns on at.
pub const DEFAULT_ZOOM: u8 = 2;
/// The strongest zoom offered.
pub const MAX_ZOOM: u8 = 6;

/// The label Settings shows for each zoom level, weakest first; [`OFF`] leads.
pub const ZOOM_LABELS: [&str; 6] = ["Off", "2×", "3×", "4×", "5×", "6×"];

/// Every zoom level, weakest first; [`OFF`] leads.
pub fn zoom_levels() -> impl Iterator<Item = u8> {
    OFF..=MAX_ZOOM
}

/// The label Settings shows for `zoom`: "Off" or "3×".
pub fn zoom_label(zoom: u8) -> &'static str {
    let position = usize::from(zoom.clamp(OFF, MAX_ZOOM).saturating_sub(OFF));
    ZOOM_LABELS.get(position).copied().unwrap_or("Off")
}

/// Parse a [`zoom_label`] back to its zoom.
pub fn zoom_from_label(label: &str) -> Option<u8> {
    zoom_levels().find(|zoom| zoom_label(*zoom) == label)
}

/// How the magnified image is shown. Persisted as [`Self::as_str`]; an unknown value reads
/// as the lens.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "String", into = "&'static str")]
pub enum MagnifierView {
    /// A lens around the point of interest; the rest of the screen stays as it is.
    #[default]
    Lens,
    /// The whole screen magnified, panning with the point of interest.
    FullScreen,
}

impl MagnifierView {
    pub const ALL: [Self; 2] = [Self::Lens, Self::FullScreen];

    /// The persisted value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lens => "lens",
            Self::FullScreen => "full_screen",
        }
    }

    /// Parse [`Self::as_str`]; anything else is the lens.
    pub fn parse(value: &str) -> Self {
        match value {
            "full_screen" => Self::FullScreen,
            _ => Self::Lens,
        }
    }

    /// The label Settings shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Lens => "Lens",
            Self::FullScreen => "Full screen",
        }
    }

    /// Parse [`Self::label`].
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|view| view.label() == label)
    }
}

impl From<String> for MagnifierView {
    fn from(value: String) -> Self {
        Self::parse(&value)
    }
}

impl From<MagnifierView> for &'static str {
    fn from(view: MagnifierView) -> Self {
        view.as_str()
    }
}

/// The magnifier state the frame paints from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MagnifierState {
    pub zoom: u8,
    pub view: MagnifierView,
}

impl MagnifierState {
    /// Read the state from an `accessibility` section; missing or malformed values are off.
    pub fn from_section(section: &Map<String, Value>) -> Self {
        Self {
            zoom: zoom_in_section(section, ZOOM_KEY).unwrap_or(OFF),
            view: section
                .get(VIEW_KEY)
                .and_then(Value::as_str)
                .map(MagnifierView::parse)
                .unwrap_or_default(),
        }
    }

    pub fn is_on(self) -> bool {
        self.zoom > OFF
    }
}

fn zoom_in_section(section: &Map<String, Value>, key: &str) -> Option<u8> {
    section
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|v| u8::try_from(v).ok())
        .map(|zoom| zoom.clamp(OFF, MAX_ZOOM))
}

/// A magnifier shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MagnifierStep {
    /// Off turns on at the last zoom used; on turns off.
    Toggle,
    /// One level stronger; from off, turns on at the weakest magnification.
    ZoomIn,
    /// One level weaker; the weakest magnification steps to off.
    ZoomOut,
}

/// Apply `step` to the current `zoom`, given the zoom the toggle remembers. Returns the new zoom
/// and the new remembered zoom.
pub fn step(zoom: u8, remembered: u8, step: MagnifierStep) -> (u8, u8) {
    let zoom = zoom.clamp(OFF, MAX_ZOOM);
    let remembered = if remembered > OFF {
        remembered.min(MAX_ZOOM)
    } else {
        DEFAULT_ZOOM
    };
    let next = match step {
        MagnifierStep::Toggle if zoom > OFF => OFF,
        MagnifierStep::Toggle => remembered,
        MagnifierStep::ZoomIn => zoom.saturating_add(1).min(MAX_ZOOM),
        MagnifierStep::ZoomOut => zoom.saturating_sub(1).max(OFF),
    };
    // Remember the last magnification actually in use, so turning off and on again returns to it.
    let remembered = if next > OFF {
        next
    } else {
        remembered.max(zoom)
    };
    (next, remembered)
}

/// Apply `step` to the session's settings ([`crate::update_section`]) and
/// return the new zoom.
///
/// # Errors
///
/// Returns the I/O error when the updated section cannot be written.
pub fn apply_step(magnifier_step: MagnifierStep) -> std::io::Result<u8> {
    let (values, next) = stepped(&crate::load_section(SECTION), magnifier_step);
    crate::update_section(SECTION, values)?;
    Ok(next)
}

/// The keys `step` writes into the accessibility `section`, and the new zoom.
fn stepped(
    section: &Map<String, Value>,
    magnifier_step: MagnifierStep,
) -> (Map<String, Value>, u8) {
    let zoom = zoom_in_section(section, ZOOM_KEY).unwrap_or(OFF);
    let remembered = zoom_in_section(section, LAST_ZOOM_KEY).unwrap_or(DEFAULT_ZOOM);
    let (next, remembered) = step(zoom, remembered, magnifier_step);
    let mut values = Map::new();
    values.insert(ZOOM_KEY.into(), Value::from(next));
    values.insert(LAST_ZOOM_KEY.into(), Value::from(remembered));
    (values, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_returns_to_the_last_zoom_in_use() {
        let (on, remembered) = step(OFF, OFF, MagnifierStep::Toggle);
        assert_eq!(on, DEFAULT_ZOOM, "a first toggle turns on at 2×");
        let (zoomed, remembered) = step(on, remembered, MagnifierStep::ZoomIn);
        assert_eq!(zoomed, 3);
        let (off, remembered) = step(zoomed, remembered, MagnifierStep::Toggle);
        assert_eq!(off, OFF);
        assert_eq!(step(off, remembered, MagnifierStep::Toggle).0, 3);
    }

    #[test]
    fn zoom_steps_stop_at_off_and_at_the_strongest_level() {
        assert_eq!(step(OFF, OFF, MagnifierStep::ZoomIn).0, 2);
        assert_eq!(step(2, 2, MagnifierStep::ZoomOut).0, OFF);
        assert_eq!(step(OFF, 4, MagnifierStep::ZoomOut), (OFF, 4));
        assert_eq!(step(MAX_ZOOM, MAX_ZOOM, MagnifierStep::ZoomIn).0, MAX_ZOOM);
    }

    #[test]
    fn labels_round_trip_every_level() {
        for zoom in zoom_levels() {
            assert_eq!(zoom_from_label(zoom_label(zoom)), Some(zoom));
        }
        assert_eq!(ZOOM_LABELS.len(), zoom_levels().count());
        assert_eq!(zoom_label(OFF), "Off");
        assert_eq!(zoom_label(3), "3×");
        assert_eq!(zoom_from_label("nope"), None);
        for view in MagnifierView::ALL {
            assert_eq!(MagnifierView::from_label(view.label()), Some(view));
            assert_eq!(MagnifierView::parse(view.as_str()), view);
        }
    }

    #[test]
    fn a_step_writes_the_zoom_and_what_the_toggle_remembers() {
        let mut section = Map::new();
        section.insert(ZOOM_KEY.into(), Value::from(4));
        let mut apply = |magnifier_step| {
            let (values, next) = stepped(&section, magnifier_step);
            section.extend(values);
            next
        };
        assert_eq!(apply(MagnifierStep::Toggle), OFF);
        assert_eq!(apply(MagnifierStep::Toggle), 4);
        assert_eq!(apply(MagnifierStep::ZoomOut), 3);
        assert_eq!(MagnifierState::from_section(&section).zoom, 3);
        assert_eq!(section.get(LAST_ZOOM_KEY), Some(&Value::from(3)));
    }

    #[test]
    fn malformed_state_reads_as_off_in_a_lens() {
        let mut section = Map::new();
        section.insert(ZOOM_KEY.into(), Value::from("big"));
        section.insert(VIEW_KEY.into(), Value::from(7));
        assert_eq!(
            MagnifierState::from_section(&section),
            MagnifierState {
                zoom: OFF,
                view: MagnifierView::Lens
            }
        );
        section.insert(ZOOM_KEY.into(), Value::from(40));
        assert_eq!(MagnifierState::from_section(&section).zoom, MAX_ZOOM);
    }
}
