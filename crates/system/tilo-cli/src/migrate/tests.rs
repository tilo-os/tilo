//! `tilo migrate` against a synthetic Thunderbird profile and a workspace that records what it
//! is asked: the person's choice decides exactly what comes over, a second run adds nothing,
//! and a stopped run resumes where it stopped.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::state::{Journal, Place, RunState};
use super::workspace::{Answer, Call, Invocation, Status, Workspace};
use super::{Lander, Stopped, thunderbird};

/// A workspace that keeps every action it is asked, and every upload, and a server that keeps
/// the person's mail accounts as the real one answers them.
#[derive(Default)]
struct Recorded {
    actions: Vec<(String, String, Value)>,
    puts: Vec<Vec<u8>>,
    /// Every account the person asked the server to add, as it was asked.
    added: Vec<Value>,
    /// The accounts the server keeps, as it lists them: never a password.
    kept: Vec<Value>,
    /// The server's refusal of every account added, in its words.
    refuses: Option<String>,
    /// The server answers an account added and then does not list it.
    forgets: bool,
}

impl Workspace for Recorded {
    fn act(&mut self, run: &Invocation, action: &str, args: Value) -> Result<Answer> {
        self.actions.push((run.id.clone(), action.to_owned(), args));
        Ok(Answer::of(None))
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        self.actions.push((String::new(), action.to_owned(), args));
        Ok(Answer::of(None))
    }

    fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        anyhow::ensure!(path == thunderbird::MAIL_ACCOUNTS, "asked {path}");
        match action {
            "list" => Ok(serde_json::json!({"accounts": self.kept})),
            "add" => {
                self.added.push(args.clone());
                if let Some(why) = &self.refuses {
                    return Err(super::workspace::Refused::coded("refused", why.clone()).into());
                }
                let account = serde_json::json!({
                    "id": args["address"], "address": args["address"],
                    "displayName": args["displayName"],
                    "incomingHost": args["incomingHost"], "incomingPort": args["incomingPort"],
                    "outgoingHost": args["outgoingHost"], "outgoingPort": args["outgoingPort"],
                    "hasSecret": args.get("password").is_some(),
                });
                if !self.forgets {
                    self.kept.push(account.clone());
                }
                Ok(account)
            }
            other => anyhow::bail!("no person/mail-accounts#{other}"),
        }
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

/// What the person was told and asked during a migration.
#[derive(Default)]
struct Told {
    lines: Vec<String>,
    asked: Vec<String>,
}

fn run_once(home: &Path, place: &Place, workspace: &mut dyn Workspace) -> Result<Journal> {
    migrate_saying(home, place, workspace, true, &mut Told::default())
}

/// A migration in which the person answers `yes` to every question, and what they were told.
fn migrate_saying(
    home: &Path,
    place: &Place,
    workspace: &mut dyn Workspace,
    yes: bool,
    told: &mut Told,
) -> Result<Journal> {
    let review = place.review(thunderbird::SOURCE)?.expect("reviewed");
    let mut journal = place.journal(thunderbird::SOURCE)?;
    journal.brought.clear();
    journal.already_there = 0;
    let run = Invocation {
        id: "run-1".into(),
        title: thunderbird::summary(&review),
    };
    let lines = std::cell::RefCell::new(Vec::new());
    let asked = std::cell::RefCell::new(Vec::new());
    let mut progress = |line: &str| lines.borrow_mut().push(line.to_owned());
    let mut confirm = |question: &str| {
        asked.borrow_mut().push(question.to_owned());
        Ok(yes)
    };
    let result = {
        let mut lander = Lander {
            workspace,
            opened: std::collections::BTreeSet::new(),
            landing_in: None,
            invocation: &run,
            journal: &mut journal,
            place,
            source: thunderbird::SOURCE,
            progress: &mut progress,
            confirm: &mut confirm,
        };
        thunderbird::land(home, &review, &mut lander)
    };
    told.lines.extend(lines.into_inner());
    told.asked.extend(asked.into_inner());
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
    fn act(&mut self, run: &Invocation, action: &str, args: Value) -> Result<Answer> {
        if action == "mail/mail.messages#deliver-message" {
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

    fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        self.0.person(path, action, args)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        self.0.put(bytes)
    }
}

/// Every server the review asks about answers.
fn answering(_: &str, _: u16) -> bool {
    true
}

/// The sample profile, reviewed with every server answering.
fn sample(dir: &Path) -> (std::path::PathBuf, Place, super::state::Review) {
    let home = dir.join("home");
    migration_sources::thunderbird::fixture::write_sample_profile(&home);
    let place = Place::at(&dir.join("state"));
    let review = thunderbird::review_with(&home, None, &answering).unwrap();
    (home, place, review)
}

/// The sample profile reviewed, and its work account chosen as an agent chooses it, checking
/// what the review lists on the way.
fn work_account_chosen(dir: &Path) -> (std::path::PathBuf, Place) {
    let (home, place, mut review) = sample(dir);

    // The review lists both accounts to connect, and apart from them what Thunderbird keeps of
    // each on this computer, nothing chosen.
    let of_kind = |kind: &str| -> Vec<String> {
        review
            .items
            .iter()
            .filter(|item| item.kind == kind)
            .map(|item| item.key.clone())
            .collect()
    };
    assert_eq!(
        of_kind("account"),
        ["ada@example.com", "ada.home@example.org"],
        "{review:#?}"
    );
    assert_eq!(
        of_kind(thunderbird::CACHE_KIND),
        ["cache/ada@example.com", "cache/ada.home@example.org"]
    );
    assert!(review.chosen().is_empty());
    let cache = review
        .items
        .iter()
        .find(|item| item.key == "cache/ada@example.com")
        .unwrap();
    assert_eq!(
        cache.count, 4,
        "the work account's cached Inbox and Sent hold four messages"
    );
    assert!(
        review.items.iter().all(|item| item.note.is_none()),
        "nothing is suggested while every server answers: {review:#?}"
    );

    // The agent chooses the work account only: connecting it copies nothing of its cache.
    review
        .choose(&["ada@example.com".to_owned()], true, true)
        .unwrap();
    let chosen: Vec<&str> = review
        .chosen()
        .iter()
        .map(|item| item.key.as_str())
        .collect();
    assert_eq!(chosen, ["ada@example.com"]);
    assert!(
        review
            .choose(&["nobody@example.com".to_owned()], true, false)
            .is_err(),
        "a key the review does not list is refused"
    );
    place.keep_review(&review).unwrap();
    assert_eq!(
        thunderbird::summary(&review),
        "From Thunderbird: connect ada@example.com"
    );

    (home, place)
}

#[test]
fn a_live_account_is_connected_and_nothing_is_copied_from_its_cache() {
    let dir = tempfile::tempdir().unwrap();
    let (home, place) = work_account_chosen(dir.path());
    let mut workspace = Recorded::default();
    let mut told = Told::default();
    let journal = migrate_saying(&home, &place, &mut workspace, true, &mut told).unwrap();

    // The account goes to the server with its servers and the password Thunderbird saved,
    // once, as the person's own request; nothing is copied, and no app is asked anything.
    assert_eq!(workspace.added.len(), 1, "{:#?}", workspace.added);
    let added = &workspace.added[0];
    assert_eq!(added["address"], "ada@example.com");
    assert_eq!(added["incomingHost"], "imap.example.com");
    assert_eq!(added["incomingPort"], 993);
    assert_eq!(added["outgoingHost"], "smtp.example.com");
    assert_eq!(added["outgoingPort"], 465);
    assert_eq!(
        added["password"],
        migration_sources::thunderbird::fixture::WORK_PASSWORD
    );
    assert!(
        added.get("identities").is_none(),
        "the work account sends as itself only"
    );
    assert!(workspace.actions.is_empty(), "{:?}", workspace.actions);
    assert!(workspace.puts.is_empty(), "no message is copied");
    assert_eq!(journal.brought.get("accounts"), Some(&1));
    assert_eq!(journal.brought.get("messages"), None);

    // The person was shown the account and its servers before it was connected, and told when
    // it was; never its password.
    assert_eq!(told.asked.len(), 1, "{:?}", told.asked);
    assert!(
        told.asked[0].contains("imap.example.com:993"),
        "{:?}",
        told.asked
    );
    assert!(
        told.lines.iter().any(|line| line.contains("is connected")),
        "{:?}",
        told.lines
    );
    let secret = migration_sources::thunderbird::fixture::WORK_PASSWORD;
    for said in told.asked.iter().chain(&told.lines) {
        assert!(!said.contains(secret), "{said}");
    }
    // Nor does anything this computer keeps of the migration.
    for entry in walk(&dir.path().join("state")) {
        let kept = std::fs::read(&entry).unwrap();
        assert!(
            !kept
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "{} keeps the password",
            entry.display()
        );
    }

    // A second migration finds the account on the server and asks for nothing.
    let journal = run_once(&home, &place, &mut workspace).unwrap();
    assert_eq!(workspace.added.len(), 1, "connected once");
    assert_eq!(journal.already_there, 1);
}

#[test]
fn an_account_that_signs_in_with_google_is_left_to_google_s_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    migration_sources::thunderbird::fixture::write_live_profile(
        &home,
        &migration_sources::thunderbird::fixture::LiveAccount {
            name: "Ada Lovelace".into(),
            email: "ada@gmail.com".into(),
            also_sends_as: None,
            imap_host: "imap.gmail.com".into(),
            imap_port: 993,
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 465,
            password: "a-token-thunderbird-made-for-itself".into(),
        },
    );
    let place = Place::at(&dir.path().join("state"));
    let mut review = thunderbird::review_with(&home, None, &answering).unwrap();
    let account = review
        .items
        .iter_mut()
        .find(|item| item.kind == "account")
        .expect("the account is offered");
    assert!(
        account.title.contains("Google's sign-in"),
        "{}",
        account.title
    );
    account.chosen = true;
    place.keep_review(&review).unwrap();

    // Nothing goes to the server with what Thunderbird keeps, nothing is counted, and the
    // person is told how it is connected; the migration itself is not a failure.
    let mut workspace = Recorded::default();
    let mut told = Told::default();
    let journal = migrate_saying(&home, &place, &mut workspace, true, &mut told).unwrap();
    assert!(workspace.added.is_empty(), "{:#?}", workspace.added);
    assert!(told.asked.is_empty(), "{:?}", told.asked);
    assert_eq!(journal.brought.get("accounts"), None);
    assert!(
        told.lines
            .iter()
            .any(|line| line.contains("not connected") && line.contains("Google")),
        "{:?}",
        told.lines
    );
    for said in &told.lines {
        assert!(
            !said.contains("a-token-thunderbird-made-for-itself"),
            "{said}"
        );
    }
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[test]
fn a_connection_the_server_did_not_keep_is_not_claimed() {
    // The server refuses: its words are the migration's failure, and nothing is counted.
    let dir = tempfile::tempdir().unwrap();
    let (home, place) = work_account_chosen(dir.path());
    let mut refusing = Recorded {
        refuses: Some("the mail server did not take the password".into()),
        ..Recorded::default()
    };
    let error = run_once(&home, &place, &mut refusing).unwrap_err();
    let said = format!("{error:#}");
    assert!(said.contains("was not connected"), "{said}");
    assert!(said.contains("did not take the password"), "{said}");
    let journal = place.journal(thunderbird::SOURCE).unwrap();
    assert_eq!(journal.state, RunState::Failed);
    assert_eq!(journal.brought.get("accounts"), None);

    // The server answers, then does not list it: still not connected.
    let mut forgetting = Recorded {
        forgets: true,
        ..Recorded::default()
    };
    let error = run_once(&home, &place, &mut forgetting).unwrap_err();
    assert!(
        format!("{error:#}").contains("does not list it"),
        "{error:#}"
    );

    // A refused migration tried again connects it: no earlier note of the journal stands in for what
    // the server keeps.
    let mut working = Recorded::default();
    let journal = run_once(&home, &place, &mut working).unwrap();
    assert_eq!(working.added.len(), 1);
    assert_eq!(journal.brought.get("accounts"), Some(&1));
}

#[test]
fn an_account_the_person_says_no_to_is_not_connected() {
    let dir = tempfile::tempdir().unwrap();
    let (home, place) = work_account_chosen(dir.path());
    let mut workspace = Recorded::default();
    let error =
        migrate_saying(&home, &place, &mut workspace, false, &mut Told::default()).unwrap_err();
    assert!(format!("{error:#}").contains("you said no"), "{error:#}");
    assert!(workspace.added.is_empty());
}

#[test]
fn copying_the_cache_is_a_choice_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let (home, place, mut review) = sample(dir.path());
    review
        .choose(&["cache/ada@example.com".to_owned()], true, true)
        .unwrap();
    assert!(
        review.chosen().iter().all(|item| item.kind != "account"),
        "copying connects nothing"
    );
    place.keep_review(&review).unwrap();
    let mut workspace = Recorded::default();
    let journal = run_once(&home, &place, &mut workspace).unwrap();
    assert!(workspace.added.is_empty(), "no account is connected");
    assert_eq!(journal.brought.get("messages"), Some(&4), "{journal:?}");

    // Its four messages, each delivered to Mail in its folder; none of the home account's.
    let delivered = workspace.of("mail/mail.messages#deliver-message");
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

    // A second run asks the workspace for nothing.
    let mut again = Recorded::default();
    let journal = run_once(&home, &place, &mut again).unwrap();
    assert!(again.actions.is_empty(), "{:?}", again.actions);
    assert!(again.puts.is_empty());
    assert!(journal.brought.is_empty());
    assert_eq!(journal.already_there, 4, "its four messages");
}

#[test]
fn copying_is_suggested_only_for_an_account_whose_server_does_not_answer() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    migration_sources::thunderbird::fixture::write_sample_profile(&home);
    // The work account's server answers; the home account's does not.
    let review = thunderbird::review_with(&home, None, &|host: &str, _| {
        host == migration_sources::thunderbird::fixture::WORK_SERVER
    })
    .unwrap();
    let note = |key: &str| {
        review
            .items
            .iter()
            .find(|item| item.key == key)
            .unwrap_or_else(|| panic!("{key} in {review:#?}"))
            .note
            .clone()
    };
    assert_eq!(note("ada@example.com"), None);
    assert_eq!(note("cache/ada@example.com"), None);
    let gone = note("ada.home@example.org").expect("the home account's server is named");
    assert!(
        gone.contains("imap.example.org:993 did not answer"),
        "{gone}"
    );
    assert!(gone.contains("cache/ada.home@example.org"), "{gone}");
    assert!(note("cache/ada.home@example.org").is_some());
    // Suggested, not chosen: the person chooses.
    assert!(review.chosen().is_empty());
}

#[test]
fn a_message_mail_holds_already_counts_as_there() {
    let dir = tempfile::tempdir().unwrap();
    let (home, place, mut review) = sample(dir.path());
    review
        .choose(&["cache/ada@example.com".to_owned()], true, true)
        .unwrap();
    place.keep_review(&review).unwrap();
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
    assert!(more.of("mail/mail.messages#deliver-message").is_empty());
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
    let mut review = thunderbird::review_with(&home, None, &answering).unwrap();
    review
        .choose(&["cache/ada.home@example.org".to_owned()], true, true)
        .unwrap();
    place.keep_review(&review).unwrap();

    // Asked to stop before it starts: nothing lands.
    place.ask_stop(thunderbird::SOURCE).unwrap();
    let mut workspace = Recorded::default();
    let error = run_once(&home, &place, &mut workspace).unwrap_err();
    assert!(error.is::<Stopped>(), "{error:#}");
    assert!(
        workspace
            .of("mail/mail.messages#deliver-message")
            .is_empty()
    );
    assert_eq!(
        place.journal(thunderbird::SOURCE).unwrap().state,
        RunState::Stopped
    );

    let mut workspace = Recorded::default();
    let journal = run_once(&home, &place, &mut workspace).unwrap();
    assert_eq!(journal.brought.get("messages"), Some(&3), "{journal:?}");
    assert!(
        workspace
            .of("mail/mail.messages#deliver-message")
            .iter()
            .all(|message| message.to_string().contains("ada.home@example.org"))
    );
}

#[test]
fn a_runs_change_in_effect_is_found_by_the_id_history_keeps() {
    let of = |change: &str, run: &str| serde_json::json!({"change": change, "run": "From Linear: Engineering", "actor": {"id": "agent:tilo", "declared": run}});
    let history = serde_json::json!({"versions": [
        of("a1", "migrate-linear-1"),
        of("a2", "migrate-linear-1"),
        {"change": "b1", "actor": {"id": "person"}},
        of("c1", "migrate-linear-2"),
        {"undone": true, "change": "c2", "run": "From Linear: Engineering", "actor": {"id": "agent:tilo", "declared": "migrate-linear-2"}},
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
        {"undone": true, "change": "d1", "run": "From Linear: Engineering", "actor": {"id": "agent:tilo", "declared": "migrate-linear-4"}},
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
        "error": {"code": "not_ready", "message": "the workspace is not serving yet"},
    }))
    .unwrap();
    assert_eq!(waiting.status, Status::Failed);
    let error = waiting.error.unwrap();
    assert_eq!(error.code(), "not_ready");
    assert!(
        error.asks_again(),
        "a workspace not serving yet is asked again"
    );

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
    assert!(
        review
            .chosen()
            .iter()
            .all(|item| item.kind != thunderbird::CACHE_KIND && item.kind != "folder"),
        "nothing of the cache is copied unless the person chooses it: {review:#?}"
    );
    // Kept: a second start works from the same review.
    assert_eq!(place.review(thunderbird::SOURCE).unwrap().unwrap(), review);
}

/// A profile copied in from another Thunderbird location reviews with its
/// mail; a folder that cannot be read is named in the answer, and the folders
/// beside it are still listed.
#[cfg(unix)]
#[test]
fn a_review_names_what_it_could_not_read_and_still_lists_the_rest() {
    use std::os::unix::ffi::OsStrExt as _;

    let server = migration_sources::thunderbird::fixture::WORK_SERVER;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    migration_sources::thunderbird::fixture::write_sample_profile(&home);
    migration_sources::thunderbird::fixture::write_stale_mail_directories(&home);

    let review = thunderbird::review_with(&home, None, &answering).unwrap();
    assert!(review.unreadable.is_empty(), "{:?}", review.unreadable);
    assert!(
        review
            .items
            .iter()
            .any(|item| item.key == "cache/ada@example.com/INBOX")
    );

    let subfolder = home.join(format!(
        ".thunderbird/fx8r2l9p.default-release/ImapMail/{server}/Projects.sbd"
    ));
    std::fs::create_dir(&subfolder).unwrap();
    std::fs::write(
        subfolder.join(std::ffi::OsStr::from_bytes(b"2026-\xff")),
        b"",
    )
    .unwrap();

    let review = thunderbird::review_with(&home, None, &answering).unwrap();
    assert!(
        review
            .items
            .iter()
            .any(|item| item.key == "cache/ada@example.com/INBOX"),
        "the folders beside the unreadable one are still listed"
    );
    assert!(
        review
            .items
            .iter()
            .any(|item| item.key == "cache/ada@example.com/Sent"),
        "the folders beside it are still listed"
    );
    let relative = format!("ImapMail/{server}/Projects.sbd");
    let problem = review
        .unreadable
        .first()
        .expect("the review says what it could not read");
    assert!(problem.contains(&relative), "{problem}");
    assert!(problem.contains("unreadable data"), "{problem}");

    let answer = super::review_answer(&review, &Journal::default());
    let text = super::sentences(&answer);
    assert!(
        text.contains("incomplete:") && text.contains(&relative),
        "{text}"
    );
}

#[test]
fn a_message_larger_than_one_upload_comes_over_with_its_parts_apart() {
    use migration_sources::thunderbird::fixture::{
        LARGE_FILES, LARGE_MESSAGE_ID, append_large_message,
    };
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    migration_sources::thunderbird::fixture::write_sample_profile(&home);
    let sent = append_large_message(&home);
    assert!(sent.len() > 40 * 1024 * 1024, "{} bytes", sent.len());
    let place = Place::at(&dir.path().join("state"));
    let mut review = thunderbird::review_with(&home, None, &answering).unwrap();
    let work = review
        .items
        .iter()
        .find(|item| item.key == "cache/ada@example.com")
        .unwrap();
    assert_eq!(work.count, 5, "the review counts the large message too");
    review
        .choose(&["cache/ada@example.com".to_owned()], true, true)
        .unwrap();
    place.keep_review(&review).unwrap();

    let mut workspace = Recorded::default();
    let journal = run_once(&home, &place, &mut workspace).unwrap();
    assert_eq!(journal.brought.get("messages"), Some(&5), "{journal:?}");
    let largest = workspace.puts.iter().map(Vec::len).max().unwrap();
    assert!(
        largest <= tilo_mail_parts::PIECE_BYTES,
        "no one upload carries the message: the largest is {largest} bytes"
    );

    // Delivered as the message kept and its parts apart, which put back together are the
    // message as the mbox holds it, byte for byte, without the line that separates it there.
    let delivery = workspace
        .of("mail/mail.messages#deliver-message")
        .into_iter()
        .find(|delivery| delivery["message_id"] == format!("<{LARGE_MESSAGE_ID}>"))
        .cloned()
        .unwrap();
    let uploaded = |reference: &Value| -> Vec<u8> {
        let hash = reference
            .as_str()
            .unwrap()
            .strip_prefix("tilo.content:")
            .unwrap();
        workspace
            .puts
            .iter()
            .find(|bytes| blake3::hash(bytes).to_hex().as_str() == hash)
            .cloned()
            .unwrap()
    };
    let rest = uploaded(&delivery["original"]);
    let parts = delivery["parts"].as_array().unwrap();
    assert_eq!(parts.len(), LARGE_FILES.len(), "{delivery}");
    let bodies: Vec<(u64, Vec<u8>)> = parts
        .iter()
        .map(|part| {
            let body = part["pieces"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(uploaded)
                .collect();
            (part["at"].as_u64().unwrap(), body)
        })
        .collect();
    let sizes: Vec<u64> = parts
        .iter()
        .map(|part| part["size"].as_u64().unwrap())
        .collect();
    assert_eq!(sizes, LARGE_FILES.map(|(_, size)| size as u64));
    let bodies: Vec<(u64, &[u8])> = bodies
        .iter()
        .map(|(at, body)| (*at, body.as_slice()))
        .collect();
    let separator = sent.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    assert!(sent.starts_with(b"From - "));
    let rejoined = tilo_mail_parts::join(&rest, &bodies).unwrap();
    assert!(
        rejoined == sent[separator..],
        "{} of {} bytes",
        rejoined.len(),
        sent.len() - separator
    );
}
