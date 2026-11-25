//! A person's calendar kept by the calendar bridge: the account Settings keeps in the
//! workspace's accounts is the one the bridge signs in with, and what its server holds shows in
//! the calendar and comes back to the server when its person edits an entry.
//!
//! The bridge is the app's own connector, run by the daemon beside its instances: its protocol
//! code is the connector crate's, over the same calendar server a person rents.

use serde_json::json;
use tilolang_calendar::test_support::Rig;
use tilolang_calendar::test_support::fake_caldav::Server;

const ADA: &str = "ada@example.test";
const PASSWORD: &str = "the-password-her-vault-keeps";

/// A workspace with a calendar account kept in its accounts record, as Settings keeps one when
/// its person connects their server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connected_calendar_account_is_kept_where_the_bridge_reads_it() {
    let device = super::Device::start("Calendar", None).await;
    let workspace = device.workspace.clone();
    let acting = tilo_action_policy::Acting::person(device.person());

    let server = "https://calendar.example.test/dav/";
    let mut account = accounts::Account::new(
        accounts::AccountId::from("caldav:ada@calendar.example.test"),
        accounts::calendar::TYPE_ID,
    );
    account.display_name = "Work".into();
    account.config = json!({
        "display_name": "Work",
        "server_url": server,
        "username": ADA,
        "calendars": [],
        "collapsed": false,
    });
    device
        .workspaces()
        .keep_workspace_account((&workspace, &acting, None), &account)
        .await
        .expect("the account is kept");

    // What the bridge's account source reads: the workspace's calendar accounts, each with the
    // server and user name its sign-in is released for.
    let held = device
        .workspaces()
        .workspace_accounts(&workspace)
        .await
        .expect("the workspace's accounts are read");
    let kept = held
        .of_type(accounts::calendar::TYPE_ID)
        .into_iter()
        .find(|kept| kept.id == account.id)
        .expect("the calendar account is listed");
    assert_eq!(kept.display_name, "Work");
    assert_eq!(
        kept.config.get("server_url").and_then(|v| v.as_str()),
        Some(server)
    );
    assert_eq!(
        kept.config.get("username").and_then(|v| v.as_str()),
        Some(ADA)
    );

    device.shutdown().await;
}

/// An entry as a calendar server holds it: `start` and `end` as `DTSTART`/`DTEND` say them.
fn review_ics() -> String {
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:review@example.test\r\n\
     SUMMARY:Review\r\nDTSTART:20261119T190000Z\r\nDTEND:20261119T200000Z\r\n\
     END:VEVENT\r\nEND:VCALENDAR\r\n"
        .to_owned()
}

/// The bridge shows the entries its server holds, with their times as the server wrote them, and
/// an edit its person makes in the calendar reaches the server.
#[test]
fn the_calendar_shows_its_servers_entries_and_writes_an_edit_back() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put("work", "review.ics", &review_ics());
    let mut rig = Rig::new(&server, ADA, PASSWORD);
    rig.settle();

    // The person sees her calendar and the entry on it.
    let calendars = rig.calendars();
    assert_eq!(calendars.len(), 1, "her one calendar");
    assert_eq!(rig.text("Calendar", &calendars[0], "name"), "Work");
    let events = rig.events();
    assert_eq!(events.len(), 1, "its one entry");
    assert_eq!(rig.text("Event", &events[0], "title"), "Review");
    assert_eq!(
        rig.text("Event", &events[0], "start"),
        "2026-11-19T19:00:00"
    );

    // She renames it; the change is written to the server, and her calendar shows it.
    let event = events[0].clone();
    rig.run(
        "Event",
        "rename_event",
        Some(&event),
        &[("title", json!("Review, moved"))],
    );
    rig.settle();
    let written = server
        .held("work")
        .into_iter()
        .next()
        .map(|(_, text)| text)
        .expect("the server holds the entry");
    assert!(
        written.contains("Review") && written.contains("moved"),
        "the server took the edit: {written}"
    );
    assert_eq!(rig.text("Event", &event, "title"), "Review, moved");
}

/// A calendar server that refuses the sign-in is said so on its account, and nothing of what it
/// holds is kept: the person sees why, and the calendar shows no entry of that server.
#[test]
fn a_calendar_server_that_refuses_the_password_keeps_no_entry() {
    let server = Server::start(ADA, PASSWORD);
    server.add_calendar("work", "Work");
    server.put("work", "review.ics", &review_ics());
    let mut rig = Rig::new(&server, ADA, "not-what-the-server-knows");
    rig.settle();

    let account = rig.items("accounts").remove(0);
    assert!(
        !rig.text("Account", &account, "trouble").is_empty(),
        "the refusal is said in the account's words"
    );
    assert!(
        rig.events().is_empty(),
        "nothing of the server's calendar is kept"
    );
}
