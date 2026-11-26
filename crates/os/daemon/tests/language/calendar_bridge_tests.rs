//! A person's calendar kept by the calendar bridge: the account Settings keeps in the
//! workspace's accounts is the one the bridge signs in with, and what its server holds shows in
//! the calendar and comes back to the server when its person edits an entry.
//!
//! The bridge is the app's own connector, run by the daemon beside its instances: its protocol
//! code is the connector crate's, over the same calendar server a person rents.

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_workspace_service::workspaces::MemberDevice;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, hosting_device, test_host_holding,
};
use tilolang_calendar::test_support::Rig;
use tilolang_calendar::test_support::fake_caldav::Server;

const ADA: &str = "ada@example.test";
const PASSWORD: &str = "the-password-her-vault-keeps";

/// A workspace with a calendar account kept in its accounts record, as Settings keeps one when
/// its person connects their server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connected_calendar_account_is_kept_where_the_bridge_reads_it() {
    // The workspace's own store, with no window and no app's package: what the account is kept
    // in is the one record, and the bridge reads it there.
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default())).await;
    let workspaces = host.workspaces().clone();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Calendar").await.unwrap().workspace;
    let acting = tilo_action_policy::Acting::person(hex::encode(cert.user_public_key));

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
    tilo_person_services::PersonServices::of(&workspaces)
        .keep_workspace_account((&workspace, &acting, None), &account)
        .await
        .expect("the account is kept");

    // What the bridge's account source reads: the workspace's calendar accounts, each with the
    // server and user name its sign-in is released for.
    let held = tilo_person_services::PersonServices::of(&workspaces)
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

    host.shutdown().await;
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

/// An iCloud account its person asks for calendars keeps them beside its mail, with the app
/// password it was added with, where the bridge reads the workspace's calendar accounts: the
/// bridge signs in there as the account's address, and the calendar lists what iCloud holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_icloud_account_asked_for_calendars_lists_them_in_the_calendar() {
    use tilo_actions::remote::{
        ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    };
    use tilo_workspace_records::accounts as workspace_accounts;
    use tilolang_calendar::engine::CalendarAccount;

    const ADDRESS: &str = "ada@icloud.example";
    const APP_PASSWORD: &str = "made-up-app-password-for-icloud";
    // A calendar server on this machine stands in for iCloud's.
    let icloud = Server::start(ADDRESS, APP_PASSWORD);
    icloud.add_calendar("personal", "Personal");
    icloud.add_calendar("family", "Family");

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default())).await;
    let workspaces = host.workspaces().clone();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    tilo_person_services::PersonServices::of(&workspaces).sign_in_accounts_through(
        |_, password| {
            if password == APP_PASSWORD {
                Ok("signed in".into())
            } else {
                Err("IMAP server rejected the credential".into())
            }
        },
    );
    tilo_person_services::PersonServices::of(&workspaces).keep_icloud_calendars_at(&icloud.url());
    let workspace = workspaces.create("Calendar").await.unwrap().workspace;

    // She adds her iCloud mail, asking for its calendars too.
    let Value::Object(adding) = json!({
        "address": ADDRESS,
        "displayName": "Ada",
        "incomingHost": "imap.mail.me.com",
        "outgoingHost": "smtp.mail.me.com",
        "outgoingPort": 587,
        "password": APP_PASSWORD,
        "services": {"mail": true, "calendar": true},
    }) else {
        unreachable!("arguments are an object")
    };
    let herself = Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_workspace(&workspace);
    let receipt = workspaces
        .device_actions()
        .invoke(
            &herself,
            InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: workspace_accounts::target(workspace_accounts::ADD_MAIL),
                argument: InvokeArgument::Json(adding),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        )
        .await
        .expect("the device's dispatch answers")
        .expect("her iCloud account is added");
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("her own add is answered at once: {:?}", receipt.status);
    };
    let added = output.value.unwrap_or_default();
    assert_eq!(added["provider"], "icloud", "{added}");
    assert_eq!(
        added["services"]["calendar"], true,
        "one connect feeds both: {added}"
    );

    // The calendar account kept beside it, as the bridge's account source reads it.
    let held = tilo_person_services::PersonServices::of(&workspaces)
        .workspace_accounts(&workspace)
        .await
        .expect("the workspace's accounts are read");
    let calendars = held.of_type(accounts::calendar::TYPE_ID);
    assert_eq!(calendars.len(), 1, "one calendar account beside the mail");
    let kept = &calendars[0];
    let said = |name: &str| {
        kept.config
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(
        said("server_url"),
        icloud.url(),
        "kept where iCloud keeps calendars"
    );
    assert_eq!(
        said("username"),
        ADDRESS,
        "signed in as the account's address"
    );
    assert!(
        !serde_json::to_string(kept).unwrap().contains(APP_PASSWORD),
        "a kept account carries no password"
    );
    let account = CalendarAccount {
        id: kept.id.to_string(),
        name: kept.display_name.clone(),
        server: said("server_url"),
        username: said("username"),
        password: APP_PASSWORD.to_owned(),
        refused: None,
        read_only: false,
    };
    host.shutdown().await;

    // The calendar, with the bridge signed in as that account, lists iCloud's calendars.
    let names = tokio::task::spawn_blocking(move || {
        let mut rig = Rig::with_accounts(vec![account]);
        rig.settle();
        let mut names: Vec<String> = rig
            .calendars()
            .iter()
            .map(|calendar| rig.text("Calendar", calendar, "name"))
            .collect();
        names.sort();
        names
    })
    .await
    .unwrap();
    assert_eq!(names, ["Family", "Personal"]);
}
