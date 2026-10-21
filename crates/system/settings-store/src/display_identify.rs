//! "Which screen is which?" — the Identify handshake between Settings and
//! the compositor, and the one numbering both sides show.
//!
//! Settings writes an [`IdentifyRequest`]; the compositor paints each
//! display's number and name on that display for [`IDENTIFY_MS`], or, for a
//! request naming one display (the one a person points at or picks in
//! Settings), on that display alone for [`IDENTIFY_FLASH_MS`]. The number
//! comes from [`display_numbers`] and the name from [`display_label`] on
//! both sides, so "2" in the arrangement canvas is the "2" on the physical
//! screen.

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now_ms;

use crate::hardware::{DisplayOutputSnapshot, MonitorIdentity, OutputDescriptor};

/// How long each display shows its number.
pub const IDENTIFY_MS: u64 = 3_000;
/// How long one display shows its number when a person points at it or
/// picks it in Settings: a glance, not a reading.
pub const IDENTIFY_FLASH_MS: u64 = 1_200;
/// How old a request may be when the compositor first sees it and still
/// answer it. The request is stamped before its write, which a loaded disk
/// can hold up for seconds; a restarted compositor must not replay it.
pub const IDENTIFY_FRESH_MS: u64 = 10_000;

/// One request to show display numbers. The id is the request time, so a
/// compositor that restarts never replays an old request as new.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", default)]
pub struct IdentifyRequest {
    pub requested_unix_ms: u64,
    /// The one display to flash its number on, by output key; every
    /// display when `None`. Always written, `null` included: a request is
    /// merged into its section, and a flash must not outlive itself into
    /// the next request for every display.
    pub display: Option<String>,
}

crate::read_through_json_value!(IdentifyRequest);

impl IdentifyRequest {
    const SECTION: &'static str = "display_identify";

    /// # Errors
    ///
    /// Returns an error if the settings section can't be written.
    pub fn request() -> std::io::Result<Self> {
        let request = Self {
            requested_unix_ms: unix_now_ms(),
            display: None,
        };
        let value = serde_json::to_value(&request).map_err(std::io::Error::other)?;
        super::update_section(
            Self::SECTION,
            value.as_object().cloned().unwrap_or_default(),
        )?;
        Ok(request)
    }

    /// Write the same identify handshake through the session-owned source.
    ///
    /// # Errors
    /// Returns a persistence error without installing a global app writer.
    pub fn request_in(source: &dyn crate::SettingsSource, request: Self) -> std::io::Result<Self> {
        let value = serde_json::to_value(&request).map_err(std::io::Error::other)?;
        source.update_section(
            Self::SECTION,
            value.as_object().cloned().unwrap_or_default(),
        )?;
        Ok(request)
    }

    pub fn load() -> Option<Self> {
        let section = super::load_section(Self::SECTION);
        if !section.contains_key("requested_unix_ms") {
            return None;
        }
        serde_json::from_value(serde_json::Value::Object(section)).ok()
    }

    /// How long the numbers show: [`IDENTIFY_MS`] on every display, a
    /// shorter [`IDENTIFY_FLASH_MS`] on the one display a request names.
    pub fn duration_ms(&self) -> u64 {
        if self.display.is_some() {
            IDENTIFY_FLASH_MS
        } else {
            IDENTIFY_MS
        }
    }

    /// Whether a compositor seeing this request first at `now_unix_ms`
    /// should still answer it (for its whole [`Self::duration_ms`]).
    pub fn is_fresh(&self, now_unix_ms: u64) -> bool {
        now_unix_ms >= self.requested_unix_ms
            && now_unix_ms.saturating_sub(self.requested_unix_ms) <= IDENTIFY_FRESH_MS
    }

    /// Milliseconds of the identify window left at `now_unix_ms`, if any.
    pub fn remaining_ms(&self, now_unix_ms: u64) -> Option<u64> {
        let end = self.requested_unix_ms.saturating_add(self.duration_ms());
        (now_unix_ms >= self.requested_unix_ms && now_unix_ms < end)
            .then(|| end.saturating_sub(now_unix_ms))
    }
}

/// The name a person knows a display by, the same in Settings and on the
/// display itself: `Built-in display` for the laptop's panel (callers
/// translate it), else the monitor's EDID name with its port (two identical
/// monitors differ only there), else the port. A connector label two cards
/// share is told apart by its card, so every name is one display's.
pub fn display_label(descriptor: &OutputDescriptor, builtin: bool) -> String {
    let monitor_name = descriptor
        .monitor
        .as_ref()
        .map(MonitorIdentity::display_name)
        .filter(|name| !name.is_empty());
    let base = match (builtin, monitor_name) {
        (true, _) => "Built-in display".to_string(),
        (false, Some(name)) => format!("{name} on {}", descriptor.label()),
        (false, None) => descriptor.label().to_string(),
    };
    descriptor
        .card_qualifier()
        .map_or_else(|| base.clone(), |card| format!("{base} on {card}"))
}

/// The number each attached display is known by, starting at 1: the
/// built-in panel first, then external displays by connector name. Returned
/// in the same order as `outputs`.
pub fn display_numbers(outputs: &[DisplayOutputSnapshot]) -> Vec<usize> {
    number_displays(outputs.iter().map(|output| {
        (
            output.builtin,
            output.descriptor.label(),
            output.descriptor.connector.as_str(),
        )
    }))
}

/// [`display_numbers`] over `(built-in, connector label, connector key)`
/// triples, for callers that hold their own view of the outputs.
pub fn number_displays<'a>(
    outputs: impl IntoIterator<Item = (bool, &'a str, &'a str)>,
) -> Vec<usize> {
    let keys: Vec<(bool, &str, &str)> = outputs.into_iter().collect();
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by(|&a, &b| match (keys.get(a), keys.get(b)) {
        (Some(a), Some(b)) => {
            b.0.cmp(&a.0)
                .then_with(|| a.1.cmp(b.1))
                .then_with(|| a.2.cmp(b.2))
        }
        _ => std::cmp::Ordering::Equal,
    });
    let mut numbers = vec![0; keys.len()];
    for (rank, index) in order.into_iter().enumerate() {
        if let Some(slot) = numbers.get_mut(index) {
            *slot = rank.saturating_add(1);
        }
    }
    numbers
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::hardware::{OutputDescriptor, OutputMode};

    fn output(connector: &str, builtin: bool) -> DisplayOutputSnapshot {
        let mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.into(),
                connector_label: connector.into(),
                modes: vec![mode],
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: None,
            },
            current_mode: mode,
            scale: 1.0,
            builtin,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: BTreeMap::new(),
        }
    }

    #[test]
    fn the_builtin_panel_is_display_one_then_connectors_in_order() {
        let outputs = [
            output("HDMI-A-1", false),
            output("DP-2", false),
            output("eDP-1", true),
        ];
        assert_eq!(display_numbers(&outputs), vec![3, 2, 1]);
    }

    #[test]
    fn a_request_lasts_three_seconds() {
        let request = IdentifyRequest {
            requested_unix_ms: 10_000,
            display: None,
        };
        assert_eq!(request.remaining_ms(9_999), None);
        assert_eq!(request.remaining_ms(10_000), Some(IDENTIFY_MS));
        assert_eq!(request.remaining_ms(12_500), Some(500));
        assert_eq!(request.remaining_ms(13_000), None);
        assert!(
            request.is_fresh(13_000),
            "a request whose write was slow is still answered"
        );
        assert!(request.is_fresh(10_000 + IDENTIFY_FRESH_MS));
        assert!(
            !request.is_fresh(10_001 + IDENTIFY_FRESH_MS),
            "nor replayed after a restart"
        );
        assert!(!request.is_fresh(9_999));
    }

    #[test]
    fn a_request_naming_one_display_is_a_short_flash_on_it() {
        let request = IdentifyRequest {
            requested_unix_ms: 10_000,
            display: Some("DP-2".into()),
        };
        assert_eq!(request.remaining_ms(10_000), Some(IDENTIFY_FLASH_MS));
        assert_eq!(request.remaining_ms(10_000 + IDENTIFY_FLASH_MS), None);
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["display"], "DP-2");
        let every = serde_json::to_value(&IdentifyRequest {
            requested_unix_ms: 1,
            display: None,
        })
        .unwrap();
        assert!(
            every["display"].is_null(),
            "a request for every display clears the last flash's display"
        );
    }
}
