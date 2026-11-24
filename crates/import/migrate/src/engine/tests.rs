//! `tilo migrate` against a synthetic Thunderbird profile and a workspace that records what it
//! is asked: the person's choice decides exactly what comes over, a second run adds nothing,
//! and a stopped run resumes where it stopped.

use std::path::Path;

use anyhow::Result;

use super::Stopped;
use super::state::{Journal, Place, RunState};
use super::thunderbird;
use super::writer::{Call, Invocation, Status, Workspace};

pub(crate) mod test_env;

use self::test_env::{Answered, Recorded, Told, land_as};

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
    told.yes = yes;
    let invocation = Invocation {
        id: "run-1".into(),
        title: thunderbird::summary(&review),
    };
    land_as(
        place,
        thunderbird::SOURCE,
        &invocation,
        workspace,
        told,
        |lander| thunderbird::land(home, &review, lander),
    )
}

/// The sample profile, reviewed, and where a run keeps its state.
fn sample(dir: &Path) -> (std::path::PathBuf, Place, super::state::Review) {
    let home = dir.join("home");
    crate::thunderbird::fixture::write_sample_profile(&home);
    let place = Place::at(&dir.join("state"));
    let review = thunderbird::review(&home, None).unwrap();
    (home, place, review)
}

/// The sample profile reviewed, and its work account chosen as an agent chooses it, checking
/// what the review lists on the way.
fn work_account_chosen(dir: &Path) -> (std::path::PathBuf, Place) {
    let (home, place, mut review) = sample(dir);

    // The review lists both accounts to connect, nothing chosen.
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
    assert!(review.chosen().is_empty());
    assert!(
        review.items.iter().all(|item| item.note.is_none()),
        "nothing is suggested: {review:#?}"
    );

    // The agent chooses the work account only.
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
        crate::thunderbird::fixture::WORK_PASSWORD
    );
    assert!(
        added.get("identities").is_none(),
        "the work account sends as itself only"
    );
    // Keeping the account is this run's own step, and no app is asked anything.
    assert_eq!(
        workspace.of("workspace/mail-accounts#add").len(),
        1,
        "{:?}",
        workspace.actions
    );
    assert!(
        workspace
            .actions
            .iter()
            .all(|(_, action, _)| action == "workspace/mail-accounts#add"),
        "no app is asked anything: {:?}",
        workspace.actions
    );
    assert_eq!(journal.brought.get("accounts"), Some(&1));
    assert_eq!(journal.brought.get("messages"), None);

    // The person was asked to connect the account before it was, and told when it was; never its
    // password.
    assert!(
        told.lines
            .iter()
            .any(|line| line.contains("connecting ada@example.com asks you first")),
        "{:?}",
        told.lines
    );
    assert!(
        told.lines.iter().any(|line| line.contains("is connected")),
        "{:?}",
        told.lines
    );
    let secret = crate::thunderbird::fixture::WORK_PASSWORD;
    for said in &told.lines {
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
    crate::thunderbird::fixture::write_live_profile(
        &home,
        &crate::thunderbird::fixture::LiveAccount {
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
    let mut review = thunderbird::review(&home, None).unwrap();
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
fn an_account_the_person_says_no_to_is_left_unconnected() {
    let dir = tempfile::tempdir().unwrap();
    let (home, place) = work_account_chosen(dir.path());
    let mut workspace = Recorded {
        answer: Answered::No,
        ..Recorded::default()
    };
    let mut told = Told::default();
    let journal = migrate_saying(&home, &place, &mut workspace, false, &mut told).unwrap();
    // The account was asked for once, and left unconnected: the run goes on and is not a failure.
    assert_eq!(workspace.added.len(), 1, "asked once");
    assert!(workspace.kept.is_empty());
    assert_eq!(journal.brought.get("accounts"), None);
    assert!(
        told.lines.iter().any(|line| line
            .contains("left unconnected")
            && line.contains("ada@example.com")),
        "{:?}",
        told.lines
    );
}

#[test]
fn each_chosen_account_is_asked_once_and_nobody_answering_leaves_it_unconnected() {
    let dir = tempfile::tempdir().unwrap();
    let (home, place, mut review) = sample(dir.path());
    // Both accounts are chosen: each is a decision of its own.
    let accounts: Vec<String> = review
        .items
        .iter()
        .filter(|item| item.kind == "account")
        .map(|item| item.key.clone())
        .collect();
    assert_eq!(accounts.len(), 2, "{review:#?}");
    review.choose(&accounts, true, false).unwrap();
    place.keep_review(&review).unwrap();
    let mut workspace = Recorded {
        answer: Answered::Nobody,
        ..Recorded::default()
    };
    let mut told = Told::default();
    let journal = migrate_saying(&home, &place, &mut workspace, false, &mut told).unwrap();
    // One ask per account, and each is left unconnected.
    assert_eq!(workspace.added.len(), 2, "one ask per account");
    assert!(workspace.kept.is_empty());
    assert_eq!(journal.brought.get("accounts"), None);
    let left: Vec<&String> = told
        .lines
        .iter()
        .filter(|line| line.contains("left unconnected"))
        .collect();
    assert_eq!(left.len(), 2, "{:?}", told.lines);
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
    // The book goes over as one vCard file, which Contacts imports whole.
    let imported = more.of("contacts#import_vcard");
    assert_eq!(imported.len(), 1, "{:?}", more.actions);
    let file = &imported[0]["file"];
    assert!(
        std::path::Path::new(file["name"].as_str().unwrap())
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("vcf")),
        "the file is offered as a vCard: {file}"
    );
    assert!(
        file["content"]
            .as_str()
            .unwrap()
            .starts_with("tilo.content:"),
        "its cards are in the workspace's content: {file}"
    );
    assert_eq!(file["media"], "text/vcard", "{file}");
    assert_eq!(more.of("calendar/calendar.calendar#new-event").len(), 2);
    // The book's two cards, counted as the review counted them.
    assert_eq!(journal.brought.get("contacts"), Some(&2));
}

#[test]
fn a_stopped_run_resumes_where_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    crate::thunderbird::fixture::write_sample_profile(&home);
    let place = Place::at(&dir.path().join("state"));
    let mut review = thunderbird::review(&home, None).unwrap();
    let book = review
        .items
        .iter()
        .find(|item| item.kind == "contacts")
        .expect("the profile has an address book")
        .key
        .clone();
    review.choose(&[book], true, true).unwrap();
    place.keep_review(&review).unwrap();

    // Asked to stop before it starts: nothing lands.
    let mut workspace = Recorded::default();
    let mut stopped = Told {
        stop: true,
        ..Told::default()
    };
    let error = migrate_saying(&home, &place, &mut workspace, true, &mut stopped).unwrap_err();
    assert!(error.is::<Stopped>(), "{error:#}");
    assert!(workspace.actions.is_empty(), "{:?}", workspace.actions);
    assert_eq!(
        place.journal(thunderbird::SOURCE).unwrap().state,
        RunState::Stopped
    );

    // Asked again, the run brings what it was stopped before bringing; a third run adds nothing.
    let mut workspace = Recorded::default();
    let journal = run_once(&home, &place, &mut workspace).unwrap();
    assert_eq!(journal.brought.get("contacts"), Some(&2), "{journal:?}");
    let mut again = Recorded::default();
    let journal = run_once(&home, &place, &mut again).unwrap();
    assert!(again.actions.is_empty(), "{:?}", again.actions);
    assert!(journal.brought.is_empty(), "{journal:?}");
    assert_eq!(
        journal.already_there, 2,
        "its address book's two contacts are there already"
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
        "error": {"code": tilo_actions::codes::NOTE_ALREADY_IMPORTED, "message": "kept"},
    }))
    .unwrap();
    assert_eq!(
        already.error.unwrap().code(),
        tilo_actions::codes::NOTE_ALREADY_IMPORTED,
        "an app's refusal is read by its code, never its words"
    );
    assert!(
        !super::writer::Refused::coded("x", "y").asks_again(),
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
    crate::thunderbird::fixture::write_sample_profile(&home);
    let place = Place::at(&dir.path().join("state"));
    assert!(
        place.review(thunderbird::SOURCE).unwrap().is_none(),
        "nothing reviewed yet"
    );
    let review = test_env::with_cx(&place, &home, |cx| {
        super::reviewed_for_start(cx, thunderbird::SOURCE)
    })
    .unwrap();
    assert!(
        review
            .chosen()
            .iter()
            .any(|item| item.key == "ada@example.com"),
        "the first start reviews the source and takes its accounts: {review:#?}"
    );
    assert!(
        review.items.iter().all(|item| item.chosen),
        "the first start reviews the source and takes everything it holds: {review:#?}"
    );
    // Kept: a second start works from the same review.
    assert_eq!(place.review(thunderbird::SOURCE).unwrap().unwrap(), review);
}

/// A profile copied in from another Thunderbird location reviews; a folder
/// that cannot be read is named in the answer.
#[cfg(unix)]
#[test]
fn a_review_names_what_it_could_not_read() {
    use std::os::unix::ffi::OsStrExt as _;

    let server = crate::thunderbird::fixture::WORK_SERVER;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    crate::thunderbird::fixture::write_sample_profile(&home);
    crate::thunderbird::fixture::write_stale_mail_directories(&home);

    let review = thunderbird::review(&home, None).unwrap();
    assert!(review.unreadable.is_empty(), "{:?}", review.unreadable);

    let subfolder = home.join(format!(
        ".thunderbird/fx8r2l9p.default-release/ImapMail/{server}/Projects.sbd"
    ));
    std::fs::create_dir(&subfolder).unwrap();
    std::fs::write(
        subfolder.join(std::ffi::OsStr::from_bytes(b"2026-\xff")),
        b"",
    )
    .unwrap();

    let review = thunderbird::review(&home, None).unwrap();
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
fn the_terminal_counts_one_thing_in_the_singular() {
    assert_eq!(super::services::counted(1, "issues"), "1 issue");
    assert_eq!(super::services::counted(3, "issues"), "3 issues");
    assert_eq!(super::services::counted(0, "messages"), "0 messages");
    let one = super::sentences(&serde_json::json!({
        "source": "thunderbird", "state": "complete", "brought": {"accounts": 1, "contacts": 2},
    }));
    assert!(one.contains("brought 1 account, 2 contacts"), "{one}");
    let review = super::sentences(&serde_json::json!({
        "items": [{"key": "contacts/Personal", "count": 1, "counts": "contacts"}],
    }));
    assert!(
        review.contains("  1 contact") && !review.contains("1 contacts"),
        "{review}"
    );
}
