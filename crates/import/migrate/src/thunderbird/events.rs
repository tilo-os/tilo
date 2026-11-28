//! A local calendar's events as iCalendar text, which Tilo Calendar imports whole.
//!
//! The events are read from the calendar as it is on this computer, from its `.ics` file or the
//! store Thunderbird keeps them in, and written in the one format every calendar reads, so the
//! app that keeps them decides what an event means and nothing here copies its shape.

use std::path::Path;

use super::personal_data::{self, Sidecar};
use super::{MAX_SIDECAR_BYTES, find_reviewed, find_sidecar, read_reviewed};

/// The events of one local calendar as iCalendar text.
///
/// `profile` names a profile by its `source_id`, its label or one of its
/// accounts' addresses; `calendar` names a calendar by its `source_id` or its
/// name.
///
/// # Errors
/// The home holds no such profile or calendar, the calendar is not a local
/// file, or it could not be read within its bounds.
pub fn calendar_file(home: &Path, profile: &str, calendar: &str) -> Result<String, String> {
    let scan = super::scan(home);
    let reviewed = find_reviewed(&scan.reviewed, profile)
        .ok_or_else(|| format!("no Thunderbird profile {profile:?}"))?;
    let file = find_sidecar(&reviewed.calendars, calendar)
        .ok_or_else(|| format!("no calendar {calendar:?} in that profile"))?;
    if !file.present || !file.local {
        return Err("that calendar is not a local file".into());
    }
    let relative = file
        .relative
        .as_deref()
        .ok_or_else(|| "the calendar has no file".to_owned())?;
    let bytes =
        read_reviewed(reviewed, relative, MAX_SIDECAR_BYTES).map_err(|error| error.to_string())?;
    personal_data::canonical(Sidecar::Calendar, &bytes)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "that calendar holds no events this reader understands".to_owned())
}
