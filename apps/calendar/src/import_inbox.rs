//! Import the canonical calendars a same-computer handoff left for this app.
//!
//! The source calendar is retained byte for byte and its events are published
//! in this app's own data area. Opening the calendar parses them through the
//! same iCalendar reader as a chosen file and applies one ordinary resource
//! change, so repeated opens do not duplicate events.

use ui::application::HostContext;
use ui::tilo_resource::Replica;

use crate::model::Calendar;

const INBOX: &str = "personal-data-incoming/calendars";
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

/// Import the calendars waiting in this app's import inbox.
pub(crate) fn import(context: &HostContext) -> Result<(), String> {
    let files = context.files();
    let data = files.data().map_err(|error| error.to_string())?;
    let Ok(inbox) = tilo_files::VPath::new(INBOX) else {
        return Ok(());
    };
    let Ok(entries) = tilo_files::blocking::list(&data, &inbox) else {
        return Ok(());
    };
    let mut imports = Vec::new();
    for entry in entries {
        if entry.kind != tilo_files::Kind::File
            || !std::path::Path::new(&entry.name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("ics"))
        {
            continue;
        }
        let Ok(name) = tilo_files::VPath::new(&entry.name) else {
            continue;
        };
        let Ok(Some(bytes)) =
            tilo_files::blocking::read_bounded(&data, &inbox.join(&name), MAX_FILE_BYTES)
        else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if let Some(import) = crate::resource::import(text) {
            imports.push(import);
        }
    }
    if imports.is_empty() {
        return Ok(());
    }
    let author = context.author().0.clone();
    context
        .migrate_record(move |doc| apply(doc, imports, &author).map(|()| true))
        .map(drop)
}

fn apply(
    doc: &mut automerge::AutoCommit,
    imports: Vec<ui::tilo_resource::Invoke<Calendar>>,
    author: &str,
) -> Result<(), String> {
    let mut replica = Replica::<Calendar>::from_doc(doc.clone()).as_author(author);
    for import in imports {
        replica
            .apply_confirmed(import)
            .map_err(|error| error.to_string())?;
    }
    *doc = replica.doc_mut().clone();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_calendar_items_become_events_in_the_target_record() {
        let import = crate::resource::import(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:kept@example.test\r\nSUMMARY:Kept\r\nDTSTART:20260102T090000Z\r\nDTEND:20260102T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .unwrap();
        let mut replica = Replica::<Calendar>::new();
        let mut doc = replica.doc_mut().clone();
        apply(&mut doc, vec![import], "firstboot").unwrap();
        let held = <Calendar as ui::tilo_resource::Resource>::read(&doc).unwrap();
        assert_eq!(held.events.len(), 1);
        assert_eq!(held.events[0].title, "Kept");
    }
}
