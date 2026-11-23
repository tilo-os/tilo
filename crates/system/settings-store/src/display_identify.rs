//! "Which screen is which?" — the Identify handshake between Settings and
//! the compositor, and the one numbering both sides show.
//!
//! Settings writes an [`IdentifyRequest`] when the person presses Identify;
//! the compositor paints each display's number and name in a corner of that
//! display for [`IDENTIFY_MS`]. The number
//! comes from [`display_numbers`] and the name from [`display_names`] on
//! both sides, so "2" in the placement canvas is the "2" on the physical
//! screen.

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now_ms;

use crate::hardware::{DisplayOutputSnapshot, MonitorIdentity, OutputDescriptor};

pub const IDENTIFY_MS: u64 = 3_000;
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
}

crate::read_through_json_value!(IdentifyRequest);

impl IdentifyRequest {
    const SECTION: &'static str = "display_identify";

    pub fn request() -> std::io::Result<Self> {
        let request = Self {
            requested_unix_ms: unix_now_ms(),
        };
        let value = serde_json::to_value(&request).map_err(std::io::Error::other)?;
        super::update_section(
            Self::SECTION,
            value.as_object().cloned().unwrap_or_default(),
        )?;
        Ok(request)
    }

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

    pub fn is_fresh(&self, now_unix_ms: u64) -> bool {
        now_unix_ms >= self.requested_unix_ms
            && now_unix_ms.saturating_sub(self.requested_unix_ms) <= IDENTIFY_FRESH_MS
    }

    pub fn remaining_ms(&self, now_unix_ms: u64) -> Option<u64> {
        let end = self.requested_unix_ms.saturating_add(IDENTIFY_MS);
        (now_unix_ms >= self.requested_unix_ms && now_unix_ms < end)
            .then(|| end.saturating_sub(now_unix_ms))
    }
}

/// The name a person knows a display by before it is told apart from the
/// others: `Built-in display` for the laptop's panel (callers translate it),
/// else the monitor's model from its EDID, else `External display`. Ports
/// and cards are implementation details and never part of a name; use
/// [`display_names`] for a set of displays, which tells identical ones apart.
pub fn display_name(descriptor: &OutputDescriptor, builtin: bool) -> String {
    let monitor_name = descriptor
        .monitor
        .as_ref()
        .map(MonitorIdentity::display_name)
        .filter(|name| !name.is_empty());
    match (builtin, monitor_name) {
        (true, _) => "Built-in display".to_string(),
        (false, Some(name)) => name,
        (false, None) => "External display".to_string(),
    }
}

pub fn display_names(outputs: &[DisplayOutputSnapshot]) -> Vec<String> {
    let names: Vec<String> = outputs
        .iter()
        .map(|output| display_name(&output.descriptor, output.builtin))
        .collect();
    name_apart(&names, &display_numbers(outputs))
}

/// `names` with each one more than one display shares followed by that
/// display's number from `numbers` (same order).
pub fn name_apart(names: &[String], numbers: &[usize]) -> Vec<String> {
    let shared = |name: &String| names.iter().filter(|other| *other == name).count() > 1;
    names
        .iter()
        .zip(numbers)
        .map(|(name, number)| {
            if shared(name) {
                format!("{name} ({number})")
            } else {
                name.clone()
            }
        })
        .collect()
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
