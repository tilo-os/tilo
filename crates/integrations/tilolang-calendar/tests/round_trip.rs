#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test says what it needs in a sentence when what it needs is not there"
)]

//! What a server's entry says, written back: the three ways iCalendar writes a time, the days a
//! repeat is left out on, and a reminder's alarm.

use tilolang_calendar::engine::{Engine, text_of};

const CALENDAR: &str = "https://calendar.example/dav/w/";
const HREF: &str = "https://calendar.example/dav/w/a.ics";

/// An entry a server holds, `start` and `end` as its `DTSTART`/`DTEND` lines write them.
fn held(start: &str, end: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a@example.test\r\n\
         SUMMARY:Standup\r\nDTSTART{start}\r\nDTEND{end}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

#[test]
fn a_wall_time_written_nowhere_in_particular_goes_back_the_same() {
    let text = held(":20261124T090000", ":20261124T093000");
    let entry = Engine::entry_of(&text, CALENDAR, HREF).expect("the entry reads");
    assert_eq!(entry.timezone, "floating");
    assert_eq!(entry.start, "2026-11-24T09:00:00");
    let written = text_of(&entry);
    assert!(written.contains("DTSTART:20261124T090000\r\n"), "{written}");
    assert!(!written.contains("DTSTART;TZID"), "{written}");
    assert!(!written.contains("20261124T090000Z"), "{written}");
}

#[test]
fn a_time_read_in_a_zone_goes_back_in_that_zone() {
    let text = held(
        ";TZID=Europe/Berlin:20261124T090000",
        ";TZID=Europe/Berlin:20261124T093000",
    );
    let entry = Engine::entry_of(&text, CALENDAR, HREF).expect("the entry reads");
    assert_eq!(entry.timezone, "Europe/Berlin");
    assert_eq!(entry.start, "2026-11-24T09:00:00");
    let written = text_of(&entry);
    assert!(
        written.contains("DTSTART;TZID=Europe/Berlin:20261124T090000"),
        "{written}"
    );
}

#[test]
fn a_time_written_in_utc_goes_back_in_utc() {
    let text = held(":20261124T090000Z", ":20261124T093000Z");
    let entry = Engine::entry_of(&text, CALENDAR, HREF).expect("the entry reads");
    assert_eq!(entry.timezone, "UTC");
    assert_eq!(entry.start, "2026-11-24T09:00:00");
    let written = text_of(&entry);
    assert!(written.contains("DTSTART:20261124T090000Z"), "{written}");
}

#[test]
fn a_day_left_out_goes_back_as_an_exdate() {
    let text = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a@example.test\r\n\
         SUMMARY:Standup\r\nDTSTART:20261124T090000Z\r\nDTEND:20261124T093000Z\r\n\
         RRULE:FREQ=DAILY\r\nEXDATE:20261125T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let entry = Engine::entry_of(text, CALENDAR, HREF).expect("the entry reads");
    assert_eq!(entry.exdates, ["2026-11-25"]);
    assert_eq!(entry.recurrence, "FREQ=DAILY");
    let written = text_of(&entry);
    assert!(written.contains("EXDATE:20261125T090000Z"), "{written}");
}

#[test]
fn a_reminder_goes_back_as_its_alarm() {
    let text = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a@example.test\r\n\
         SUMMARY:Standup\r\nDTSTART:20261124T090000Z\r\nDTEND:20261124T093000Z\r\n\
         BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n";
    let entry = Engine::entry_of(text, CALENDAR, HREF).expect("the entry reads");
    assert_eq!(entry.remind, "2026-11-24T08:50:00Z");
    let written = text_of(&entry);
    assert!(written.contains("TRIGGER:-PT10M"), "{written}");
}

#[test]
fn a_cancelled_instance_goes_back_as_a_day_left_out() {
    let text = concat!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a@example.test\r\n",
        "SUMMARY:Standup\r\nDTSTART:20261124T090000Z\r\nDTEND:20261124T093000Z\r\n",
        "RRULE:FREQ=DAILY\r\nEND:VEVENT\r\n",
        "BEGIN:VEVENT\r\nUID:a@example.test\r\nRECURRENCE-ID:20261125T090000Z\r\n",
        "DTSTART:20261125T090000Z\r\nDTEND:20261125T093000Z\r\nSTATUS:CANCELLED\r\n",
        "END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let entry = Engine::entry_of(text, CALENDAR, HREF).expect("the entry reads");
    assert!(
        entry
            .overrides
            .iter()
            .any(|occurrence| occurrence.cancelled),
        "{:?}",
        entry.overrides
    );
    let written = text_of(&entry);
    assert!(
        written.contains("RECURRENCE-ID:20261125T090000Z"),
        "{written}"
    );
    assert!(written.contains("STATUS:CANCELLED"), "{written}");
}
