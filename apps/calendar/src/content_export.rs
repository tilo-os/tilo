//! The calendar as an iCalendar file kept as content, for whoever acts
//! without a file chooser: an agent hands the `.ics` on (attaches it to a
//! mail, shares it) rather than writing on this device's disk, which the
//! person's own Export ICS does through the chooser.

use ui::tilo_resource::{Id, Keyed};

use crate::model::Calendar;

/// Keep the calendar `calendar` holds — every event and task, or only the
/// event `event` — as an iCalendar file in the window's content store, and
/// answer its address, how many events and tasks it holds and its size.
///
/// # Errors
/// Returns that the event is gone, that there is nothing to export, or why
/// the content store refused the file.
pub fn ics(calendar: &Calendar, event: Option<uuid::Uuid>) -> Result<serde_json::Value, String> {
    let chosen = match event {
        Some(id) => {
            let key = Id::from_raw(id.to_string());
            let found = calendar
                .events
                .find(&key)
                .ok_or("that event is gone; read the calendar for the events it holds")?;
            let mut events = Keyed::new();
            events.push(found.clone());
            let mut notes = std::collections::BTreeMap::new();
            if let Some(note) = calendar.notes.get(&id.to_string()) {
                notes.insert(id.to_string(), note.clone());
            }
            Calendar {
                events,
                notes,
                ..Calendar::default()
            }
        }
        None => calendar.clone(),
    };
    let (events, tasks) = (chosen.events.len(), chosen.tasks.len());
    if events == 0 && tasks == 0 {
        return Err("there are no events or tasks to export".into());
    }
    let text = crate::ics::export(&chosen);
    let store = ui::tilo_resource::content::current()
        .ok_or("this window keeps no content to export into")?;
    let hash = store
        .put(text.as_bytes())
        .map_err(|error| format!("the export could not be kept: {error}"))?;
    let content =
        tilo_actions::content_resource(&hash).ok_or("the export has no content address")?;
    Ok(serde_json::json!({
        "content": content,
        "format": "ics",
        "media_type": "text/calendar",
        "events": events,
        "tasks": tasks,
        "bytes": text.len(),
    }))
}
