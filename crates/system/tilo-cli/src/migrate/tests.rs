//! `tilo migrate` against a synthetic Thunderbird profile and a workspace that records what it
//! is asked: the person's choice decides exactly what comes over, a second run adds nothing,
//! and a stopped run resumes where it stopped.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::state::{Journal, Place, RunState};
use super::workspace::{Run, Workspace};
use super::{Lander, Stopped, thunderbird};

/// A workspace that keeps every action it is asked, and every upload.
#[derive(Default)]
struct Recorded {
    actions: Vec<(String, String, Value)>,
    puts: Vec<Vec<u8>>,
}

impl Workspace for Recorded {
    fn act(&mut self, run: &Run, action: &str, args: Value) -> Result<String> {
        self.actions.push((run.id.clone(), action.to_owned(), args));
        Ok("ok".into())
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        self.puts.push(bytes.to_vec());
        Ok(format!("tilo.content:{}", blake3::hash(bytes).to_hex()))
    }
}

impl Recorded {
    fn of(&self, action: &str) -> Vec<&Value> {
        self.actions
            .iter()
            .filter(|(_, name, _)| name == action)
            .map(|(_, _, args)| args)
            .collect()
    }
}

fn run_once(home: &Path, place: &Place, workspace: &mut Recorded) -> Result<Journal> {
    let review = place.review(thunderbird::SOURCE)?.expect("reviewed");
    let mut journal = place.journal(thunderbird::SOURCE)?;
    journal.brought.clear();
    journal.already_there = 0;
    let run = Run {
        id: "run-1".into(),
        title: thunderbird::summary(&review),
    };
    let mut progress = |_: &str| {};
    let result = {
        let mut lander = Lander {
            workspace,
            opened: std::collections::BTreeSet::new(),
            landing_in: None,
            run: &run,
            journal: &mut journal,
            place,
            source: thunderbird::SOURCE,
            progress: &mut progress,
        };
        thunderbird::land(home, &review, &mut lander)
    };
    journal.state = match &result {
        Ok(()) => RunState::Complete,
        Err(error) if error.is::<Stopped>() => RunState::Stopped,
        Err(_) => RunState::Failed,
    };
    place.keep_journal(thunderbird::SOURCE, &journal)?;
    result.map(|()| journal)
}

/// The sample profile reviewed, and its work account chosen as an agent chooses it, checking
/// what the review lists on the way.
fn work_account_chosen(dir: &Path) -> (std::path::PathBuf, Place) {
    let home = dir.join("home");
    migration_sources::thunderbird::fixture::write_sample_profile(&home);
    let place = Place::at(&dir.join("state"));

    // The review lists both accounts with their folders and counts, nothing chosen.
    let mut review = thunderbird::review(&home, None).unwrap();
    let accounts: Vec<&str> = review
        .items
        .iter()
        .filter(|item| item.kind == "account")
        .map(|item| item.key.as_str())
        .collect();
    assert_eq!(
        accounts,
        ["ada@example.com", "ada.home@example.org"],
        "{review:#?}"
    );
    assert!(review.chosen().is_empty());
    let work = review
        .items
        .iter()
        .find(|item| item.key == "ada@example.com")
        .unwrap();
    assert_eq!(
        work.count, 4,
        "the work account's Inbox and Sent hold four messages"
    );

    // The agent chooses the work account only; its folders come with it.
    review
        .choose(&["ada@example.com".to_owned()], true, true)
        .unwrap();
    let chosen: Vec<&str> = review
        .chosen()
        .iter()
        .map(|item| item.key.as_str())
        .collect();
    assert!(chosen.contains(&"ada@example.com"));
    assert!(
        chosen.iter().all(|key| key.starts_with("ada@example.com")),
        "{chosen:?}"
    );
    assert!(
        review
            .choose(&["nobody@example.com".to_owned()], true, false)
            .is_err(),
        "a key the review does not list is refused"
    );
    place.keep_review(&review).unwrap();
    assert_eq!(
        thunderbird::summary(&review),
        "From Thunderbird: ada@example.com (4 messages)"
    );

    (home, place)
}

#[test]
fn one_thunderbird_account_of_two_comes_over_once() {
    let dir = tempfile::tempdir().unwrap();
    let (home, place) = work_account_chosen(dir.path());
    let mut workspace = Recorded::default();
    let journal = run_once(&home, &place, &mut workspace).unwrap();
    assert_eq!(journal.brought.get("messages"), Some(&4), "{journal:?}");

    // Its four messages, each delivered to Mail in its folder; none of the home account's.
    let delivered = workspace.of("mail/mail.mailbox#deliver-message");
    assert_eq!(delivered.len(), 4, "{delivered:#?}");
    assert!(delivered.iter().any(|message| message["folder"] == "sent"));
    assert_eq!(
        delivered
            .iter()
            .filter(|message| message["folder"] == "inbox")
            .count(),
        3
    );
    assert!(
        delivered
            .iter()
            .all(|message| !message.to_string().contains("ada.home@example.org")),
        "nothing of the home account's mail is read into the workspace"
    );
    assert!(
        workspace.puts.is_empty(),
        "a message's words travel in its action"
    );
    assert_eq!(workspace.of("mail#open").len(), 1, "Mail is started once");
    // Contacts and calendars were not chosen.
    assert!(
        workspace
            .of("contacts/contacts.book#add-contacts")
            .is_empty()
    );
    assert!(
        workspace
            .of("calendar/calendar.calendar#new-event")
            .is_empty()
    );
    // Every action is a step of the one run.
    assert!(workspace.actions.iter().all(|(run, _, _)| run == "run-1"));

    // A second run asks the workspace for nothing.
    let mut again = Recorded::default();
    let journal = run_once(&home, &place, &mut again).unwrap();
    assert!(again.actions.is_empty(), "{:?}", again.actions);
    assert!(again.puts.is_empty());
    assert!(journal.brought.is_empty());
    assert_eq!(journal.already_there, 4);
}

#[test]
fn an_address_book_and_calendar_chosen_later_come_over_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (home, place) = work_account_chosen(dir.path());
    run_once(&home, &place, &mut Recorded::default()).unwrap();
    // Choosing the address book and calendar later brings only them.
    let mut review = place.review(thunderbird::SOURCE).unwrap().unwrap();
    let extra: Vec<String> = review
        .items
        .iter()
        .filter(|item| item.kind == "contacts" || item.kind == "calendar")
        .map(|item| item.key.clone())
        .collect();
    assert_eq!(extra.len(), 2, "{review:#?}");
    review.choose(&extra, true, false).unwrap();
    place.keep_review(&review).unwrap();
    let mut more = Recorded::default();
    let journal = run_once(&home, &place, &mut more).unwrap();
    assert!(more.of("mail/mail.mailbox#deliver-message").is_empty());
    let contacts = more
        .of("contacts/contacts.book#add-contacts")
        .into_iter()
        .flat_map(|args| args["contacts"].as_array().unwrap())
        .count();
    assert_eq!(contacts, 2);
    assert_eq!(more.of("calendar/calendar.calendar#new-event").len(), 2);
    assert_eq!(journal.brought.get("contacts"), Some(&2));
}

#[test]
fn a_stopped_run_resumes_where_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    migration_sources::thunderbird::fixture::write_sample_profile(&home);
    let place = Place::at(&dir.path().join("state"));
    let mut review = thunderbird::review(&home, None).unwrap();
    review
        .choose(&["ada.home@example.org".to_owned()], true, true)
        .unwrap();
    place.keep_review(&review).unwrap();

    // Asked to stop before it starts: nothing lands.
    place.ask_stop(thunderbird::SOURCE).unwrap();
    let mut workspace = Recorded::default();
    let error = run_once(&home, &place, &mut workspace).unwrap_err();
    assert!(error.is::<Stopped>(), "{error:#}");
    assert!(workspace.of("mail/mail.mailbox#deliver-message").is_empty());
    assert_eq!(
        place.journal(thunderbird::SOURCE).unwrap().state,
        RunState::Stopped
    );

    let mut workspace = Recorded::default();
    let journal = run_once(&home, &place, &mut workspace).unwrap();
    assert_eq!(journal.brought.get("messages"), Some(&3), "{journal:?}");
    assert!(
        workspace
            .of("mail/mail.mailbox#deliver-message")
            .iter()
            .all(|message| message.to_string().contains("ada.home@example.org"))
    );
}
