#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test says what it needs in a sentence when what it needs is not there"
)]

//! An invitation the Calendar takes in (`take_invitation`, which asks the connector for
//! `caldav.import`), against a calendar server that schedules by itself, as the ones people rent
//! do: it is one entry however often it comes, a later version brings it up to date and its
//! cancellation takes it out, and what the server is sent tells it to write to nobody, so nobody
//! is written to in its person's name.

use tilolang_calendar::test_support::Rig;
use tilolang_calendar::test_support::fake_caldav::Server;
use tilolang_tilo::origin::Origin as _;
use tilolang_tilo::runtime::Delivery;

/// The address her calendar server writes to her at, where her account signs in with it.
const ADA: &str = "ada@example.test";
const PASSWORD: &str = "secret-for-these-tests";

/// A meeting: one entry, with a line this calendar does not model.
fn meeting(uid: &str, summary: &str, start: &str, more: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Server//EN\r\nBEGIN:VEVENT\r\n\
         UID:{uid}\r\nSUMMARY:{summary}\r\nDTSTART;TZID=Europe/Berlin:{start}\r\nDURATION:PT30M\r\n\
         X-SERVER-NOTE:kept as it is\r\n{more}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

/// An entry in which `organizer` invites `invited`, as a calendar holds one.
fn inviting(uid: &str, summary: &str, start: &str, organizer: &str, invited: &[&str]) -> String {
    let people: String = std::iter::once(format!("ORGANIZER;CN=Organiser:mailto:{organizer}\r\n"))
        .chain(invited.iter().map(|address| {
            format!("ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{address}\r\n")
        }))
        .collect();
    meeting(uid, summary, start, &people)
}

/// `entry` as a message carries it, under `method`.
fn carried(method: &str, entry: &str) -> String {
    entry.replace(
        "VERSION:2.0\r\n",
        &format!("VERSION:2.0\r\nMETHOD:{method}\r\n"),
    )
}

/// The lines of the entry `text` that name somebody, each whole.
fn naming_lines(text: &str) -> Vec<String> {
    text.replace("\r\n ", "")
        .lines()
        .filter(|line| line.starts_with("ORGANIZER") || line.starts_with("ATTENDEE"))
        .map(str::to_owned)
        .collect()
}

/// Ada's workspace, its one account signing in to `server` as her address, synced.
fn ada(server: &Server) -> Rig {
    server.add_calendar("work", "Work");
    let mut rig = Rig::new(server, ADA, PASSWORD);
    rig.settle();
    rig
}

/// What the connector answers when the Calendar takes in the invitation `text` to her work
/// calendar: `Ok` once the server took it, the server's or the connector's words where it was
/// refused. The connector is asked as the platform asks it for `take_invitation`'s
/// `caldav.import`, and asked again once it settled, as the platform does while it waits.
fn take_in(rig: &mut Rig, text: &str) -> Result<(), String> {
    let calendar = rig.calendars().remove(0);
    let key = rig.text("Calendar", &calendar, "key");
    let payload = serde_json::json!(["caldav-account", key, text]);
    match rig.origin.deliver("caldav.import", &payload) {
        Delivery::Pending => {}
        Delivery::Rejected(reason) | Delivery::Offline(reason) => return Err(reason),
        Delivery::Delivered | Delivery::DeliveredWith(_) => return Ok(()),
    }
    rig.settle();
    match rig.origin.deliver("caldav.import", &payload) {
        Delivery::Delivered | Delivery::DeliveredWith(_) => {
            rig.settle();
            Ok(())
        }
        Delivery::Rejected(reason) | Delivery::Offline(reason) => Err(reason),
        Delivery::Pending => Err("the connector settled nothing of the invitation".to_owned()),
    }
}

/// Her calendar server wrote to nobody by itself.
fn nobody_was_mailed(server: &Server, what: &str) {
    assert_eq!(
        server.mailed(),
        Vec::<String>::new(),
        "{what}: her calendar server wrote to people in her name"
    );
}

/// The titles of the entries her workspace holds, each with its start.
fn shown(rig: &Rig) -> Vec<String> {
    let mut shown: Vec<String> = rig
        .events()
        .iter()
        .map(|event| {
            format!(
                "{} {}",
                rig.text("Event", event, "title"),
                rig.text("Event", event, "start")
            )
        })
        .collect();
    shown.sort();
    shown
}

/// Her calendar server schedules by itself, as the ones people rent do: an entry in which she
/// invites others has it write to them, unless what it is sent tells it not to. So an entry is
/// stored telling it, whatever the text says of that itself, and nobody is written to in her
/// name: not when it goes in, not when another guest is added, not when it is taken out.
#[test]
fn what_her_calendar_server_is_sent_tells_it_to_write_to_nobody() {
    let server = Server::start(ADA, PASSWORD);
    let mut rig = ada(&server);
    let dinner = |method: &str, invited: &[&str]| {
        carried(
            method,
            &inviting(
                "dinner-3@example.org",
                "Dinner",
                "20261119T190000",
                ADA,
                invited,
            ),
        )
    };
    let _ = server.requests();

    take_in(
        &mut rig,
        &dinner("REQUEST", &["bo@example.test", "eve@elsewhere.test"]),
    )
    .expect("her own entry, by her own word");
    nobody_was_mailed(&server, "an entry in which she invites two people");
    let on_server = server.held("work");
    assert_eq!(on_server.len(), 1);
    assert!(
        !on_server[0].1.contains("METHOD"),
        "an entry a server keeps has no method: {}",
        on_server[0].1
    );
    let naming = naming_lines(&on_server[0].1);
    assert_eq!(naming.len(), 3, "{naming:?}");
    for line in naming {
        assert!(
            line.contains(";SCHEDULE-AGENT=CLIENT"),
            "her calendar server is left to write to whoever this line names: {line}"
        );
    }
    assert_eq!(shown(&rig), ["Dinner 2026-11-19T19:00:00"]);

    // Text that says of itself that the server is the one to write, and to do it now, is not
    // stored saying so.
    let insisting = dinner(
        "REQUEST",
        &["bo@example.test", "eve@elsewhere.test", "cy@example.test"],
    )
    .replace(
        "ATTENDEE;PARTSTAT",
        "ATTENDEE;SCHEDULE-AGENT=SERVER;SCHEDULE-FORCE-SEND=REQUEST;PARTSTAT",
    );
    take_in(&mut rig, &insisting).expect("one more guest");
    nobody_was_mailed(&server, "text that tells the server to write");
    let held = server.held("work");
    assert_eq!(held.len(), 1, "one entry, brought up to date, not two");
    let text = held[0].1.replace("\r\n ", "");
    assert!(
        !text.contains("SCHEDULE-AGENT=SERVER") && !text.contains("SCHEDULE-FORCE-SEND"),
        "{text}"
    );
    assert_eq!(text.matches("SCHEDULE-AGENT=CLIENT").count(), 4, "{text}");
    assert!(
        text.contains("cy@example.test"),
        "the guest she added: {text}"
    );

    // She takes it out again, and nobody is told.
    take_in(
        &mut rig,
        &dinner("CANCEL", &["bo@example.test", "eve@elsewhere.test"]),
    )
    .expect("she takes it out");
    assert!(
        server.held("work").is_empty(),
        "its cancellation took it out"
    );
    let removal = server
        .requests()
        .into_iter()
        .find(|request| request.method == "DELETE")
        .expect("the removal");
    assert_eq!(removal.header("Schedule-Reply"), Some("F"));
    nobody_was_mailed(&server, "an entry taken out");
    assert!(
        rig.events().is_empty(),
        "and her workspace holds it no more"
    );
}

/// An invitation to her is one entry however often it comes: it goes in, the same again
/// changes nothing, its sender's later version brings it up to date, and its cancellation takes
/// it out. What her calendar server is sent tells it to write to nobody: every line that names
/// somebody says the server is not the one to, and the removal says to answer nobody.
#[test]
fn an_invitation_to_her_goes_in_and_out_and_no_server_writes_of_it() {
    let server = Server::start(ADA, PASSWORD);
    let mut rig = ada(&server);
    let offsite = |method: &str, start: &str| {
        carried(
            method,
            &inviting(
                "offsite-9@example.org",
                "Offsite",
                start,
                "bo@example.test",
                &[ADA, "cy@example.test"],
            ),
        )
    };
    // What the server was asked while her account was added does not count.
    let _ = server.requests();

    take_in(&mut rig, &offsite("REQUEST", "20261119T140000")).expect("the invitation goes in");
    assert_eq!(server.held("work").len(), 1);
    assert_eq!(shown(&rig), ["Offsite 2026-11-19T14:00:00"]);
    let first = server.held("work");
    take_in(&mut rig, &offsite("REQUEST", "20261119T140000")).expect("the same again");
    assert_eq!(
        server.held("work"),
        first,
        "the same invitation again is the same one entry, as it was"
    );
    take_in(&mut rig, &offsite("REQUEST", "20261119T160000")).expect("its sender's later version");
    assert_eq!(server.held("work").len(), 1, "one entry, not two");
    assert_eq!(shown(&rig), ["Offsite 2026-11-19T16:00:00"]);

    take_in(&mut rig, &offsite("CANCEL", "20261119T160000")).expect("its sender's cancellation");
    assert!(server.held("work").is_empty());
    assert!(rig.events().is_empty());

    let requests = server.requests();
    let stored: Vec<String> = requests
        .iter()
        .filter(|request| request.method == "PUT")
        .map(test_http::Request::body_text)
        .collect();
    assert_eq!(stored.len(), 2, "the invitation and its later version");
    for text in &stored {
        let naming = naming_lines(text);
        assert_eq!(naming.len(), 3, "{text}");
        for line in naming {
            assert!(
                line.contains(";SCHEDULE-AGENT=CLIENT"),
                "her calendar server is left to write to whoever this line names: {line}"
            );
        }
    }
    let removal = requests
        .iter()
        .find(|request| request.method == "DELETE")
        .expect("the removal");
    assert_eq!(
        removal.header("Schedule-Reply"),
        Some("F"),
        "a removal tells her calendar server to answer nobody"
    );
    nobody_was_mailed(
        &server,
        "an invitation put in, brought up to date and taken out",
    );
}

/// The Calendar puts an invitation in under its UID, in no calendar, as whoever took it in, so
/// the entry is theirs as its history says. Nothing of that is sent to a server: the
/// invitation's import is what is stored, and the server's copy, once it comes, is written over
/// the record that stands for it rather than beside it.
#[test]
fn an_invitation_taken_in_here_is_the_record_its_servers_copy_comes_over() {
    let server = Server::start(ADA, PASSWORD);
    let mut rig = ada(&server);
    let _ = server.requests();
    rig.run(
        "Calendars",
        "put_in",
        None,
        &[
            ("uid", serde_json::json!("offsite-9@example.org")),
            ("title", serde_json::json!("Offsite")),
        ],
    );
    rig.settle();
    let standing = rig.events();
    assert_eq!(standing.len(), 1, "it stands in the workspace at once");
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.method != "PUT"),
        "what stands for the invitation is never sent by itself"
    );

    take_in(
        &mut rig,
        &carried(
            "REQUEST",
            &inviting(
                "offsite-9@example.org",
                "Offsite",
                "20261119T140000",
                "bo@example.test",
                &[ADA],
            ),
        ),
    )
    .expect("the server takes the invitation");
    let held = rig.events();
    assert_eq!(
        held, standing,
        "the server's copy came over the record, not beside it"
    );
    let calendar = rig
        .reference("Event", &held[0], "calendar")
        .expect("it is in the calendar the server keeps it in");
    assert_eq!(rig.text("Calendar", &calendar, "name"), "Work");
    assert_eq!(rig.text("Event", &held[0], "organizer"), "bo@example.test");
    assert_eq!(rig.attendees(&held[0]).len(), 2, "with the people it names");
    assert_eq!(server.held("work").len(), 1, "one entry on the server");
    nobody_was_mailed(&server, "an invitation taken in here");
}
