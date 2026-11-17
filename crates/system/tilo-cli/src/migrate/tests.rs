//! `tilo migrate` against a synthetic Thunderbird profile and a workspace that records what it
//! is asked: the person's choice decides exactly what comes over, a second run adds nothing,
//! and a stopped run resumes where it stopped.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::state::{Journal, Place, RunState};
use super::workspace::{Answer, Call, Run, Status, Workspace};
use super::{Lander, Stopped, thunderbird};

/// A workspace that keeps every action it is asked, and every upload.
#[derive(Default)]
struct Recorded {
    actions: Vec<(String, String, Value)>,
    puts: Vec<Vec<u8>>,
}

impl Workspace for Recorded {
    fn act(&mut self, run: &Run, action: &str, args: Value) -> Result<Answer> {
        self.actions.push((run.id.clone(), action.to_owned(), args));
        Ok(Answer::of(None, Vec::new()))
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        self.actions.push((String::new(), action.to_owned(), args));
        Ok(Answer::of(None, Vec::new()))
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

fn run_once(home: &Path, place: &Place, workspace: &mut dyn Workspace) -> Result<Journal> {
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

/// A workspace whose Mail holds every message delivered already, as an earlier run's would.
struct HoldsMail(Recorded);

impl Workspace for HoldsMail {
    fn act(&mut self, run: &Run, action: &str, args: Value) -> Result<Answer> {
        if action == "mail/mail.mailbox#deliver-message" {
            self.0
                .actions
                .push((run.id.clone(), action.to_owned(), args));
            return Err(super::workspace::Refused::coded(
                tilo_actions::codes::MESSAGE_ALREADY_HELD,
                "this message is in the mailbox already",
            )
            .into());
        }
        self.0.act(run, action, args)
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        self.0.ask(action, args)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        self.0.put(bytes)
    }
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
    // Each message travels as it was sent, its bytes kept as content the delivery names.
    assert_eq!(
        workspace.puts.len(),
        4,
        "each message's bytes are kept whole"
    );
    assert!(
        delivered.iter().all(|message| message["original"]
            .as_str()
            .is_some_and(|original| original.starts_with("tilo.content:"))),
        "{delivered:#?}"
    );
    assert!(
        delivered
            .iter()
            .all(|message| message["message_id"].as_str().is_some()),
        "a message keeps its own Message-ID"
    );
    assert_eq!(workspace.of("mail#open").len(), 1, "Mail is started once");
    // Contacts and calendars were not chosen.
    assert!(
        workspace
            .of("contacts/contacts.contacts#add-contacts")
            .is_empty()
    );
    assert!(
        workspace
            .of("calendar/calendar.calendar#new-event")
            .is_empty()
    );
    // The account joins the person's accounts with its servers, the other address it sends
    // as and the password Thunderbird saved; the home account does not. The password travels
    // with the account, as a secret argument, into the store that keeps the account.
    let added = workspace.of("mail/mail.mailbox#add-account");
    assert_eq!(added.len(), 1, "{added:#?}");
    assert_eq!(added[0]["email"], "ada@example.com");
    assert_eq!(added[0]["imap_host"], "imap.example.com");
    assert_eq!(added[0]["smtp_host"], "smtp.example.com");
    assert_eq!(
        added[0]["password"],
        migration_sources::thunderbird::fixture::WORK_PASSWORD,
        "the saved sign-in travels once, with the account"
    );
    assert!(
        added[0].get("identities").is_none(),
        "the work account sends as itself only"
    );
    // No vault of this computer's keeps a second copy beside it.
    let vault = accounts::SecretVault::open_read_only_in(&place.path("vault", ""));
    assert!(
        vault
            .get(&accounts::AccountId::new("ada@example.com"))
            .is_none(),
        "the sign-in is not sealed into a second vault"
    );
    assert_eq!(journal.brought.get("accounts"), Some(&1));
    // Every action is a step of the one run.
    assert!(workspace.actions.iter().all(|(run, _, _)| run == "run-1"));

    // A second run asks the workspace for nothing.
    let mut again = Recorded::default();
    let journal = run_once(&home, &place, &mut again).unwrap();
    assert!(again.actions.is_empty(), "{:?}", again.actions);
    assert!(again.puts.is_empty());
    assert!(journal.brought.is_empty());
    assert_eq!(
        journal.already_there, 5,
        "its four messages and the account"
    );
}

#[test]
fn a_message_mail_holds_already_counts_as_there() {
    let dir = tempfile::tempdir().unwrap();
    let (home, place) = work_account_chosen(dir.path());
    let mut workspace = HoldsMail(Recorded::default());
    let journal = run_once(&home, &place, &mut workspace).unwrap();
    assert_eq!(
        journal.brought.get("messages"),
        None,
        "no message was brought: Mail holds each already"
    );
    assert_eq!(
        journal.already_there, 4,
        "its four messages are told by the app's code, not by its words"
    );
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
        .of("contacts/contacts.contacts#add-contacts")
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

#[test]
fn a_runs_change_in_effect_is_found_by_the_run_id_history_keeps() {
    let of = |change: &str, run: &str| serde_json::json!({"change": change, "run": "From Linear: Engineering", "actor": {"id": "agent:tilo", "run_id": run}});
    let history = serde_json::json!({"versions": [
        of("a1", "migrate-linear-1"),
        of("a2", "migrate-linear-1"),
        {"change": "b1", "actor": {"id": "person"}},
        of("c1", "migrate-linear-2"),
        {"undone": true, "change": "c2", "run": "From Linear: Engineering", "actor": {"id": "agent:tilo", "run_id": "migrate-linear-2"}},
    ]});
    assert_eq!(
        super::run_in_history(Some(&history), "migrate-linear-1"),
        super::RunInHistory::InEffect("a2".to_owned()),
        "two runs under one title are told apart by their ids"
    );
    assert_eq!(
        super::run_in_history(Some(&history), "migrate-linear-2"),
        super::RunInHistory::InEffect("c1".to_owned()),
        "a change undone already is not taken back again"
    );
    assert_eq!(
        super::run_in_history(Some(&history), "migrate-linear-3"),
        super::RunInHistory::Absent
    );
    let undone = serde_json::json!({"versions": [
        {"undone": true, "change": "d1", "run": "From Linear: Engineering", "actor": {"id": "agent:tilo", "run_id": "migrate-linear-4"}},
    ]});
    assert_eq!(
        super::run_in_history(Some(&undone), "migrate-linear-4"),
        super::RunInHistory::Undone,
        "a run someone undid already is there, with nothing left to take back"
    );
    assert_eq!(
        super::run_in_history(None, "migrate-linear-5"),
        super::RunInHistory::Absent,
        "a record that answered no history holds no change of any run"
    );
}

#[test]
fn a_run_counts_one_thing_in_the_singular() {
    assert_eq!(super::services::counted(1, "issues"), "1 issue");
    assert_eq!(super::services::counted(3, "issues"), "3 issues");
    assert_eq!(super::services::counted(0, "messages"), "0 messages");
}

#[test]
fn a_step_that_waits_for_its_person_answers_pending_not_failed() {
    let pending = Call::of(&serde_json::json!({
        "status": "pending", "invocation_id": "9c19",
        "waiting_for": "the person to allow it",
    }))
    .unwrap();
    assert_eq!(pending.status, Status::Pending);
    assert_eq!(pending.result, None);
    assert!(pending.error.is_none(), "waiting is no refusal");

    let waiting = Call::of(&serde_json::json!({
        "status": "failed",
        "error": {"code": "rate_limited", "message": "busy", "retry_after_ms": 40},
    }))
    .unwrap();
    assert_eq!(waiting.status, Status::Failed);
    let error = waiting.error.unwrap();
    assert_eq!(error.code(), "rate_limited");
    assert!(error.asks_again(), "a busy workspace is asked again");

    let already = Call::of(&serde_json::json!({
        "status": "failed",
        "error": {"code": tilo_actions::codes::MESSAGE_ALREADY_HELD, "message": "held"},
    }))
    .unwrap();
    assert_eq!(
        already.error.unwrap().code(),
        tilo_actions::codes::MESSAGE_ALREADY_HELD,
        "an app's refusal is read by its code, never its words"
    );
    assert!(
        !super::workspace::Refused::coded("x", "y").asks_again(),
        "an app's own refusal is not asked again"
    );
}

#[test]
fn a_call_that_ran_answers_its_value_and_the_records_it_names() {
    let ran = Call::of(&serde_json::json!({
        "status": "ok",
        "result": {"already": true, "id": "abc"},
        "resources": ["tasks.tracker/issues/abc"],
    }))
    .unwrap();
    assert_eq!(ran.status, Status::Ok);
    let answer = ran.answer();
    assert!(answer.said("already"));
    assert_eq!(
        answer.result().and_then(|value| value.get("id")),
        Some(&serde_json::json!("abc"))
    );
    assert_eq!(
        answer.resources().collect::<Vec<_>>(),
        ["tasks.tracker/issues/abc"]
    );
}

#[test]
fn starting_without_a_review_reviews_the_source_and_takes_everything() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    migration_sources::thunderbird::fixture::write_sample_profile(&home);
    let place = Place::at(&dir.path().join("state"));
    assert!(
        place.review(thunderbird::SOURCE).unwrap().is_none(),
        "nothing reviewed yet"
    );
    let review = super::reviewed_for_start(&place, &home, thunderbird::SOURCE).unwrap();
    assert!(
        review
            .chosen()
            .iter()
            .any(|item| item.key == "ada@example.com"),
        "the first start reviews the source and takes its accounts: {review:#?}"
    );
    // Kept: a second start works from the same review.
    assert_eq!(place.review(thunderbird::SOURCE).unwrap().unwrap(), review);
}
