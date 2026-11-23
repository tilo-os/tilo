//! The calendar origin against a calendar server kept on loopback: what a sync writes into a
//! package's records, what a person's change sends, and what a server's refusal puts back.

#[path = "support/fake_caldav.rs"]
#[allow(dead_code, reason = "the harness holds more than one test asks it yet")]
mod fake_caldav;

/// The server a test asks holds what a client wrote, and answers what it was asked.
#[test]
fn the_server_holds_what_a_client_wrote() {
    let server = fake_caldav::Server::start("ada@example.test", "secret");
    let work = server.add_calendar("work", "Work");
    server.put(
        "work",
        "a.ics",
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261119T190000Z\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    assert_eq!(server.held("work").len(), 1);
    assert!(work.ends_with("/work/"));
}
