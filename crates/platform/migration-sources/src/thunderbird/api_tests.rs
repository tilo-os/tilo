//! The laptop-facing reads over the sample profile.

use std::ops::ControlFlow;

use super::fixture::{HOME_EMAIL, HOME_HOST, WORK_EMAIL, WORK_HOST, write_sample_profile};
use super::*;

fn sample() -> (tempfile::TempDir, ThunderbirdProfile) {
    let home = tempfile::tempdir().unwrap();
    write_sample_profile(home.path());
    let profiles = discover(home.path()).unwrap();
    assert_eq!(profiles.len(), 1);
    (home, profiles.into_iter().next().unwrap())
}

#[test]
fn discovery_lists_both_accounts_their_folders_books_and_calendars() {
    let (_home, profile) = sample();
    assert_eq!(profile.accounts.len(), 2);
    let work = profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(WORK_EMAIL))
        .expect("the Work account");
    assert_eq!(work.kind, "imap");
    assert_eq!(work.display_name, "Work");
    assert_eq!(work.identities.len(), 1);
    assert_eq!(work.identities[0].email, WORK_EMAIL);
    let inbox = work
        .folders
        .iter()
        .find(|folder| folder.hierarchy == ["INBOX"])
        .expect("the Work inbox");
    assert_eq!(inbox.kind, FolderKind::Inbox);
    assert_eq!(inbox.format, "mbox");
    assert_eq!(inbox.messages, Some(3));
    let sent = work
        .folders
        .iter()
        .find(|folder| folder.hierarchy == ["Sent"])
        .expect("the Work sent folder");
    assert_eq!(sent.kind, FolderKind::Sent);

    let home = profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(HOME_EMAIL))
        .expect("the Home account");
    assert_eq!(home.identities.len(), 2, "both of Home's identities");
    assert_eq!(
        home.folders
            .iter()
            .find(|folder| folder.hierarchy == ["INBOX"])
            .and_then(|folder| folder.messages),
        Some(2)
    );

    assert_eq!(profile.address_books.len(), 1);
    assert_eq!(profile.address_books[0].name, "Personal address book");
    assert_eq!(profile.address_books[0].contacts, Some(2));
    assert_eq!(profile.calendars.len(), 1);
    assert_eq!(profile.calendars[0].events, Some(2));
}

#[test]
fn account_settings_carry_the_imap_and_smtp_endpoints_without_a_password() {
    let (_home, profile) = sample();
    let work = profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(WORK_EMAIL))
        .unwrap();
    let settings = account_settings(&profile, &work.source_id).expect("settings");
    assert_eq!(settings.hostname.as_deref(), Some(WORK_HOST));
    let candidate = settings.candidate.expect("an IMAP candidate");
    assert_eq!(candidate.incoming.host, WORK_HOST);
    assert_eq!(candidate.incoming.port, 993);
    assert_eq!(candidate.incoming.username, WORK_EMAIL);
    assert_eq!(candidate.outgoing.host, "smtp.example.com");
    assert_eq!(candidate.outgoing.port, 465);
    let serialized = serde_json::to_string(&profile).unwrap();
    assert!(
        !serialized.contains("ImapMail"),
        "the pinned source paths stay out of a status record"
    );
    assert!(!serialized.contains("\"settings\""));
}

#[test]
fn messages_come_one_bounded_file_at_a_time_with_their_flags() {
    let (home, profile) = sample();
    let work = profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(WORK_EMAIL))
        .unwrap();
    let inbox = work
        .folders
        .iter()
        .find(|folder| folder.hierarchy == ["INBOX"])
        .unwrap();
    let mut messages: Vec<StoredMessage> = Vec::new();
    super::messages(
        home.path(),
        &profile.source_id,
        &inbox.source_id,
        &mut |message| {
            messages.push(message);
            ControlFlow::Continue(())
        },
    )
    .unwrap();
    assert_eq!(messages.len(), 3);
    assert!(messages[0].read);
    assert!(!messages[1].read);
    assert!(messages[2].starred);
    assert_eq!(
        messages[0].message_id.as_deref(),
        Some("<work-1@example.test>")
    );
    assert!(messages[2].bytes.starts_with(b"From "));
    let stopped = super::messages(
        home.path(),
        &profile.source_id,
        &inbox.source_id,
        &mut |_| ControlFlow::Break(()),
    );
    assert!(stopped.is_ok(), "the caller can stop the read");

    let home_account = profile
        .accounts
        .iter()
        .find(|account| account.email.as_deref() == Some(HOME_EMAIL))
        .unwrap();
    let sent = home_account
        .folders
        .iter()
        .find(|folder| folder.hierarchy == ["Sent"])
        .unwrap();
    let mut count = 0;
    super::messages(home.path(), HOME_EMAIL, &sent.source_id, &mut |_| {
        count += 1;
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(count, 1);
    assert!(
        super::messages(home.path(), HOME_EMAIL, "Missing", &mut |_| {
            ControlFlow::Continue(())
        })
        .is_err()
    );
    assert_ne!(HOME_HOST, WORK_HOST);
}

#[test]
fn contacts_take_the_shape_the_contacts_app_reads_and_keep_their_ids() {
    let (home, profile) = sample();
    let book = &profile.address_books[0];
    let contacts = super::contacts(home.path(), &profile.source_id, &book.source_id).unwrap();
    assert_eq!(contacts.len(), 2);
    let ada = contacts
        .iter()
        .find(|contact| contact["first"] == "Ada")
        .expect("Ada's card");
    assert_eq!(ada["last"], "Lovelace");
    assert_eq!(ada["org"], "Analytical Engines");
    assert_eq!(ada["birthday"]["Full"]["year"], 1815);
    assert_eq!(ada["phones"][0]["label"], "cell");
    assert!(uuid::Uuid::parse_str(ada["id"].as_str().unwrap()).is_ok());
    let again = super::contacts(home.path(), &profile.source_id, &book.source_id).unwrap();
    assert_eq!(contacts, again, "the same card names the same contact");
}

#[test]
fn events_take_the_shape_the_calendar_app_reads() {
    let (home, profile) = sample();
    let calendar = &profile.calendars[0];
    let events = super::events(home.path(), &profile.source_id, &calendar.source_id).unwrap();
    assert_eq!(events.len(), 2);
    let review = events
        .iter()
        .find(|event| event["title"] == "Design review")
        .expect("the review event");
    assert_eq!(review["start"], "2026-11-05T09:00");
    assert_eq!(review["end"], "2026-11-05T10:00");
    assert_eq!(review["timezone"], "Europe/Berlin");
    assert_eq!(review["rrule"], "FREQ=WEEKLY;COUNT=4");
    let holiday = events
        .iter()
        .find(|event| event["title"] == "Company holiday")
        .expect("the holiday event");
    assert_eq!(holiday["all_day"], true);
    assert_eq!(holiday["start"], "2026-12-24T00:00");
    assert!(uuid::Uuid::parse_str(review["id"].as_str().unwrap()).is_ok());
}
