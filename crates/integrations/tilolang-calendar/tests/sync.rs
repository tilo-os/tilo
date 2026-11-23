#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test says what it needs in a sentence when what it needs is not there"
)]

//! The calendar origin against a calendar server kept on loopback: what a sync writes into a
//! package's records, what a person's change sends, and what a server's refusal puts back.

#[path = "support/mod.rs"]
mod support;

use support::fake_caldav::Server;
use support::{Rig, at};
use tilolang_tilo::origin::Origin as _;

const ADA: &str = "ada@example.test";
const PASSWORD: &str = "secret-for-these-tests";

/// An entry a server holds: `start` and `end` as `DTSTART`/`DTEND` say them.
fn entry(uid: &str, title: &str, start: &str, end: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{title}\r\n\
         DTSTART:{start}\r\nDTEND:{end}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

/// The ics the one entry of `slug` holds.
fn held(server: &Server, slug: &str) -> String {
    server
        .held(slug)
        .into_iter()
        .next()
        .map(|(_, text)| text)
        .expect("the calendar holds an entry")
}

/// A workspace whose account holds `server`'s one calendar, `work`.
fn rig(server: &Server) -> Rig {
    let mut rig = Rig::new(server, ADA, PASSWORD);
    rig.settle();
    rig
}

#[test]
fn an_accounts_calendars_and_entries_sync_into_records() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put(
        "work",
        "a.ics",
        &entry(
            "a@example.test",
            "Review",
            "20261119T190000Z",
            "20261119T200000Z",
        ),
    );
    let rig = rig(&server);
    let calendars = rig.calendars();
    assert_eq!(calendars.len(), 1, "her one calendar");
    assert_eq!(rig.text("Calendar", &calendars[0], "name"), "Work");
    assert_eq!(
        rig.text("Calendar", &calendars[0], "account"),
        "caldav-account"
    );
    assert!(rig.flag("Calendar", &calendars[0], "writable"));
    let events = rig.events();
    assert_eq!(events.len(), 1, "its one entry");
    assert_eq!(rig.text("Event", &events[0], "title"), "Review");
    assert_eq!(
        rig.text("Event", &events[0], "start"),
        "2026-11-19T19:00:00"
    );
    assert_eq!(rig.text("Event", &events[0], "timezone"), "UTC");
    assert_eq!(rig.text("Event", &events[0], "day"), "2026-11-19");
    assert_eq!(rig.text("Event", &events[0], "end_day"), "2026-11-19");
    assert_eq!(
        rig.reference("Event", &events[0], "calendar").as_ref(),
        Some(&calendars[0])
    );
    assert_eq!(
        rig.text("Event", &events[0], "key"),
        "a@example.test",
        "the server's UID names it"
    );
}

#[test]
fn an_edited_entry_reaches_the_server() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put(
        "work",
        "a.ics",
        &entry(
            "a@example.test",
            "Review",
            "20261119T190000Z",
            "20261119T200000Z",
        ),
    );
    let mut rig = rig(&server);
    let event = rig.events().remove(0);
    rig.run(
        "Event",
        "rename_event",
        Some(&event),
        &[("title", serde_json::json!("Review, moved"))],
    );
    rig.settle();
    let text = held(&server, "work");
    assert!(text.contains("Review") && text.contains("moved"), "{text}");
    assert_eq!(rig.text("Event", &event, "title"), "Review, moved");
}

#[test]
fn a_servers_refusal_puts_the_field_back_and_says_why_on_the_account() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put(
        "work",
        "a.ics",
        &entry(
            "a@example.test",
            "Review",
            "20261119T190000Z",
            "20261119T200000Z",
        ),
    );
    let mut rig = rig(&server);
    let event = rig.events().remove(0);
    // Another client writes the entry: the version the calendar saw is no longer the server's.
    server.put(
        "work",
        "a.ics",
        &entry(
            "a@example.test",
            "Review, by somebody else",
            "20261119T190000Z",
            "20261119T200000Z",
        ),
    );
    rig.run(
        "Event",
        "rename_event",
        Some(&event),
        &[("title", serde_json::json!("Mine now"))],
    );
    rig.settle();
    assert_eq!(
        rig.text("Event", &event, "title"),
        "Review",
        "the server's version is put back"
    );
    assert!(held(&server, "work").contains("by somebody else"));
}

#[test]
fn a_read_only_account_refuses_a_write_without_asking_the_server() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put(
        "work",
        "a.ics",
        &entry(
            "a@example.test",
            "Review",
            "20261119T190000Z",
            "20261119T200000Z",
        ),
    );
    let mut rig = Rig::new(&server, ADA, PASSWORD);
    rig.accounts.lock().unwrap()[0].read_only = true;
    rig.settle();
    let event = rig.events().remove(0);
    let before = server.requests().len();
    rig.run(
        "Event",
        "rename_event",
        Some(&event),
        &[("title", serde_json::json!("Mine now"))],
    );
    rig.settle();
    assert_eq!(rig.text("Event", &event, "title"), "Review");
    let wrote = server
        .requests()
        .into_iter()
        .skip(before)
        .filter(|request| request.method == "PUT")
        .count();
    assert_eq!(wrote, 0, "the read-only account asked its server nothing");
}

#[test]
fn check_asks_the_server_now_and_brings_in_what_it_holds() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    let mut rig = rig(&server);
    assert!(rig.events().is_empty());
    server.put(
        "work",
        "b.ics",
        &entry(
            "b@example.test",
            "Later",
            "20261120T090000Z",
            "20261120T100000Z",
        ),
    );
    let delivered = rig
        .origin
        .deliver("caldav.check", &serde_json::json!("caldav-account"));
    assert!(matches!(
        delivered,
        tilolang_tilo::runtime::Delivery::Delivered
    ));
    rig.settle();
    assert_eq!(rig.events().len(), 1, "the new entry is in the workspace");
}

#[test]
fn an_answer_tells_the_organiser_and_writes_the_persons_own_status() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put(
        "work",
        "invite.ics",
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:invite@example.test\r\n\
         SUMMARY:Dinner\r\nDTSTART:20261120T190000Z\r\nDTEND:20261120T200000Z\r\n\
         ORGANIZER:mailto:bo@example.test\r\nATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:\
         mailto:ada@example.test\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    let mut rig = rig(&server);
    let event = rig.events().remove(0);
    let delivered = rig.origin.deliver(
        "caldav.answer",
        &serde_json::json!(["caldav-account", "invite@example.test", ADA, "accepted"]),
    );
    assert!(matches!(
        delivered,
        tilolang_tilo::runtime::Delivery::Pending
    ));
    rig.settle();
    let text = held(&server, "work");
    assert!(text.contains("PARTSTAT=ACCEPTED"), "{text}");
    let guest = rig.attendees(&event).remove(0);
    assert_eq!(rig.text("Attendee", &guest, "status"), "accepted");
}

#[test]
fn an_imported_invitation_becomes_an_entry_of_the_calendar() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    let mut rig = rig(&server);
    let calendar = rig.calendars().remove(0);
    let key = rig.text("Calendar", &calendar, "key");
    let delivered = rig.origin.deliver(
        "caldav.import",
        &serde_json::json!([
            "caldav-account",
            key,
            entry(
                "offsite@example.test",
                "Offsite",
                "20261201T090000Z",
                "20261201T170000Z"
            )
        ]),
    );
    assert!(matches!(
        delivered,
        tilolang_tilo::runtime::Delivery::Pending
    ));
    rig.settle();
    assert_eq!(server.held("work").len(), 1, "the server took it");
    assert_eq!(rig.events().len(), 1, "the workspace took it in");
}

#[test]
fn a_second_sync_at_the_same_token_brings_nothing_in() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put(
        "work",
        "a.ics",
        &entry(
            "a@example.test",
            "Review",
            "20261119T190000Z",
            "20261119T200000Z",
        ),
    );
    let mut rig = rig(&server);
    let before = rig.events();
    let _ = server.requests();
    assert!(matches!(
        rig.origin
            .deliver("caldav.check", &serde_json::json!("caldav-account")),
        tilolang_tilo::runtime::Delivery::Delivered
    ));
    rig.settle();
    let reports: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|request| request.method == "REPORT")
        .collect();
    assert!(!reports.is_empty(), "the server was asked");
    assert!(
        reports
            .iter()
            .any(|request| request.body_text().contains("urn:test:sync:")),
        "a second sync asks from the token it ended at"
    );
    assert_eq!(rig.events(), before, "nothing changed in the workspace");
}

#[test]
fn an_entry_of_the_workspaces_own_calendar_is_never_sent_anywhere() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    let mut rig = rig(&server);
    let _ = server.requests();
    let root = rig.root();
    rig.run(
        "Calendars",
        "add_calendar",
        None,
        &[("name", serde_json::json!("Home"))],
    );
    rig.settle();
    let home = rig
        .calendars()
        .into_iter()
        .find(|calendar| rig.text("Calendar", calendar, "name") == "Home")
        .expect("the calendar of her own");
    let _ = root;
    rig.run(
        "Calendars",
        "add_event",
        None,
        &[
            ("calendar", serde_json::json!(home.id_string())),
            ("title", serde_json::json!("Family dinner")),
            ("start", at("2026-11-20T19:00:00")),
            ("end", at("2026-11-20T21:00:00")),
        ],
    );
    rig.settle();
    assert_eq!(rig.events().len(), 1);
    let wrote = server
        .requests()
        .into_iter()
        .filter(|request| request.method == "PUT" || request.method == "DELETE")
        .count();
    assert_eq!(wrote, 0, "an own calendar's entry is never sent");
}

#[test]
fn moving_an_entry_into_an_accounts_calendar_writes_it_and_out_of_it_takes_it_out() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    let mut rig = rig(&server);
    let work = rig.calendars().remove(0);
    rig.run(
        "Calendars",
        "add_calendar",
        None,
        &[("name", serde_json::json!("Home"))],
    );
    let home = rig
        .calendars()
        .into_iter()
        .find(|calendar| rig.text("Calendar", calendar, "name") == "Home")
        .expect("her own calendar");
    rig.run(
        "Calendars",
        "add_event",
        None,
        &[
            ("calendar", serde_json::json!(home.id_string())),
            ("title", serde_json::json!("Dinner")),
            ("start", at("2026-11-20T19:00:00")),
            ("end", at("2026-11-20T21:00:00")),
        ],
    );
    rig.settle();
    let event = rig.events().remove(0);
    // Into the account's calendar: a PUT to its server.
    rig.run(
        "Event",
        "move_event",
        Some(&event),
        &[("calendar", serde_json::json!(work.id_string()))],
    );
    rig.settle();
    assert_eq!(server.held("work").len(), 1, "the server holds it now");
    assert!(!rig.text("Event", &event, "href").is_empty());
    // And out again: a DELETE there.
    rig.run(
        "Event",
        "move_event",
        Some(&event),
        &[("calendar", serde_json::json!(home.id_string()))],
    );
    rig.settle();
    assert!(server.held("work").is_empty(), "the server let it go");
    assert!(rig.text("Event", &event, "href").is_empty());
}

#[test]
fn a_colour_reaches_the_server_and_a_refused_one_puts_the_servers_back() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put(
        "work",
        "a.ics",
        &entry(
            "a@example.test",
            "Review",
            "20261119T190000Z",
            "20261119T200000Z",
        ),
    );
    let mut rig = rig(&server);
    let event = rig.events().remove(0);
    rig.run(
        "Event",
        "recolor_event",
        Some(&event),
        &[("color", serde_json::json!("#ff0000"))],
    );
    rig.settle();
    assert!(
        held(&server, "work").contains("COLOR:#ff0000"),
        "{}",
        held(&server, "work")
    );
    let calendar = rig.calendars().remove(0);
    rig.run(
        "Calendar",
        "recolor_calendar",
        Some(&calendar),
        &[("color", serde_json::json!("#00ff00"))],
    );
    rig.settle();
    assert_eq!(server.color("work"), "#00ff00");
}

#[test]
fn a_calendar_colour_a_server_refuses_puts_the_servers_back() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    let mut rig = rig(&server);
    let calendar = rig.calendars().remove(0);
    server.make_read_only("work");
    rig.run(
        "Calendar",
        "recolor_calendar",
        Some(&calendar),
        &[("color", serde_json::json!("#00ff00"))],
    );
    rig.settle();
    assert_eq!(server.color("work"), "", "the server kept its own colour");
    assert_eq!(
        rig.text("Calendar", &calendar, "color"),
        "",
        "the server's colour is put back"
    );
}

#[test]
fn a_restarted_connector_asks_from_where_the_last_pull_ended() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put(
        "work",
        "a.ics",
        &entry(
            "a@example.test",
            "Review",
            "20261119T190000Z",
            "20261119T200000Z",
        ),
    );
    let mut rig = rig(&server);
    let calendar = rig.calendars().remove(0);
    let token = rig.text("Calendar", &calendar, "sync_token");
    assert!(token.starts_with("urn:test:sync:"), "{token}");
    let before = rig.events();
    let _ = server.requests();
    rig.restart();
    rig.settle();
    let reports: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|request| request.method == "REPORT")
        .collect();
    assert!(
        reports
            .iter()
            .any(|request| request.body_text().contains(&token)),
        "a restarted connector asks from where the last pull ended"
    );
    assert_eq!(rig.events(), before, "and brings in nothing again");
}
