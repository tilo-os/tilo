//! A trigger at a time a resource holds, and one on mail arriving, read the app's records as the
//! app publishes them: the Calendar app's entries with every occurrence of a series among them,
//! and the Mail app's messages as its connector brings them in. Both are written here through the
//! device's own store, as the apps' writers write them, and the shortcut is kept signed and armed
//! as a person's is.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion"
)]

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use collab_types::store::wire::Command;
use collab_types::store::{Cell, Resource, Value};
use serde_json::json;
use tilo_automation::Shortcut;
use tilo_resource_store::Stamp;

use super::{Order, Runner, StartRun};
use crate::test_support::Started;
use tilo_workspace_service::workspaces::MemberDevice;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::fixtures::{
    declare_app, hosting_device, save_signed, test_host as test_server,
};

/// The Calendar app's entries, with the fields a moment reads.
const CALENDAR_SURFACE: &str = r#"format = 2

[[resources]]
kind = "tl.so.tilo.calendar.Event"
label = "Event"
version = 1
fields = [
  { path = "key", type = "text" },
  { path = "title", type = "text" },
  { path = "start", type = "text" },
  { path = "end", type = "text" },
  { path = "timezone", type = "text" },
  { path = "all_day", type = "bool" },
  { path = "recurrence", type = "text" },
  { path = "recurrence_every", type = "int" },
  { path = "recurrence_count", type = "int" },
  { path = "exdates", type = { set = "text" } },
  { path = "categories", type = { set = "text" } },
  { path = "cancelled", type = "bool" },
  { path = "recurrence_of", type = "text" },
  { path = "original_start", type = "text" },
]
verbs = [
  { id = "$origin", label = "Keep in step with its service", kind = "undoable", origin = "caldav.event" },
]

[[resources]]
kind = "tl.so.tilo.calendar.Account"
label = "Account"
version = 1
fields = [
  { path = "key", type = "text" },
  { path = "address", type = "text" },
  { path = "name", type = "text" },
  { path = "events", type = { collection = { resource = "tl.so.tilo.calendar.Event" } }, stored = "collection" },
]
verbs = [
  { id = "$origin", label = "Keep in step with its service", kind = "undoable", origin = "caldav.account" },
]
"#;

/// The Mail app's messages, with the fields a trigger on mail reads.
const MAIL_SURFACE: &str = r#"format = 2

[[resources]]
kind = "tl.so.tilo.mail.Message"
label = "Message"
version = 1
fields = [
  { path = "key", type = "text" },
  { path = "account", type = "text" },
  { path = "mailbox", type = "text" },
  { path = "from", type = "text" },
  { path = "from_name", type = "text" },
  { path = "to", type = "text" },
  { path = "cc", type = "text" },
  { path = "subject", type = "text" },
  { path = "received", type = "text" },
  { path = "snippet", type = "text" },
  { path = "message_id", type = "text" },
  { path = "unread", type = "bool" },
]
verbs = [
  { id = "$origin", label = "Keep in step with its service", kind = "undoable", origin = "imap.message" },
]

[[resources]]
kind = "tl.so.tilo.mail.Thread"
label = "Thread"
version = 1
fields = [
  { path = "key", type = "text" },
  { path = "subject", type = "text" },
  { path = "messages", type = { collection = { resource = "tl.so.tilo.mail.Message" } }, stored = "collection" },
]
verbs = [
  { id = "$origin", label = "Keep in step with its service", kind = "undoable", origin = "imap.thread" },
]

[[resources]]
kind = "tl.so.tilo.mail.Account"
label = "Account"
version = 1
fields = [
  { path = "key", type = "text" },
  { path = "address", type = "text" },
  { path = "name", type = "text" },
  { path = "threads", type = { collection = { resource = "tl.so.tilo.mail.Thread" } }, stored = "collection" },
]
verbs = [
  { id = "$origin", label = "Keep in step with its service", kind = "undoable", origin = "imap.account" },
]
"#;

/// The moment `text` names, ms since the epoch.
fn moment(text: &str) -> i64 {
    text.parse::<jiff::Timestamp>().unwrap().as_millisecond()
}

/// [`moment`], as a pinned wall clock takes it.
fn pinned_at(text: &str) -> u64 {
    u64::try_from(moment(text)).unwrap()
}

/// A test's right to pin its thread's wall clock, which the runner reads.
struct Pinned(tilo_clock::testing::SharedWall);

impl Pinned {
    fn at(now_ms: u64) -> Self {
        let wall = tilo_clock::testing::SharedWall::default();
        wall.follow();
        let pinned = Self(wall);
        pinned.set(now_ms);
        pinned
    }

    fn set(&self, now_ms: u64) {
        let Some(now) = UNIX_EPOCH.checked_add(std::time::Duration::from_millis(now_ms)) else {
            unreachable!("a near time");
        };
        self.0.pin(now);
    }
}

impl Drop for Pinned {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Set the pinned clock to `when` and let `runner` look once.
async fn pass_at(clock: &Pinned, runner: &mut Runner, when: &str) {
    clock.set(pinned_at(when));
    runner.pass(Vec::new()).await;
}

/// The text `value` holds, as a record keeps one.
fn text(value: &str) -> Cell {
    Cell::One(Value::Text(value.to_owned()))
}

/// The members of a set, as a record keeps them.
fn set(members: &[&str]) -> Cell {
    Cell::Set(
        members
            .iter()
            .map(|one| Value::Text((*one).to_owned()))
            .collect(),
    )
}

/// Keep `fields` as the record `id` of `kind`, nested in `parent`, written by `app` as its origin
/// writes its records.
async fn keep(
    workspaces: &Workspaces,
    workspace: &str,
    app: &str,
    kind: &str,
    id: &str,
    parent: Option<&str>,
    fields: Vec<(&str, Cell)>,
) {
    workspaces
        .own_resource_command(
            workspace,
            app,
            Command::Put {
                kind: kind.into(),
                id: id.into(),
                resource: Resource {
                    readers: "*".into(),
                    parent: parent.map(str::to_owned),
                    order: None,
                    fields: fields
                        .into_iter()
                        .map(|(name, cell)| (name.to_owned(), cell))
                        .collect(),
                },
                stamp: Stamp {
                    verb: "$origin".into(),
                    ..Stamp::default()
                },
            },
            Vec::new(),
        )
        .await
        .unwrap();
}

/// Take the record `id` of `kind` off, as the app's origin does once its server no longer holds it.
async fn forget(workspaces: &Workspaces, workspace: &str, app: &str, kind: &str, id: &str) {
    workspaces
        .own_resource_command(
            workspace,
            app,
            Command::Remove {
                kind: kind.into(),
                id: id.into(),
                stamp: Stamp {
                    verb: "$origin".into(),
                    ..Stamp::default()
                },
            },
            Vec::new(),
        )
        .await
        .unwrap();
}

/// A shortcut `id` whose one trigger is `trigger`, taking the trigger's payload as its inputs.
fn shortcut(id: &str, trigger: tilo_automation::TriggerSpec) -> Shortcut {
    Shortcut {
        id: id.into(),
        name: id.into(),
        inputs: trigger.payload().to_vec(),
        steps: Vec::new(),
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
        saved_proof: None,
        paused: false,
    }
}

/// The filter `object` names, as a trigger holds it.
fn filter(object: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    object.as_object().cloned().unwrap_or_default()
}

/// A shortcut that fires a quarter of an hour before each Calendar entry filed under Board.
fn before_board_entries() -> Shortcut {
    shortcut(
        "board",
        tilo_automation::TriggerSpec::ResourceTime {
            kind: "tl.so.tilo.calendar.Event".into(),
            field: "start".into(),
            offset_secs: -900,
            filter: filter(&json!({ "categories": "Board" })),
        },
    )
}

/// What the moments fired so far handed over: for each, the date its occurrence has in its
/// series and the time the occurrence starts.
fn handed_moments(started: &Started) -> Vec<(String, i64)> {
    started
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|firing| {
            let Some(event) = firing.inputs.get(tilo_automation::EVENT_INPUT) else {
                unreachable!("a fired moment carries its event");
            };
            let held_by = event["item"].as_str().unwrap();
            (
                held_by.rsplit('/').next().unwrap().to_owned(),
                event["at_ms"].as_i64().unwrap(),
            )
        })
        .collect()
}

/// A moment before each entry of a category fires once, follows the calendar as another client
/// changes it, and answers nothing once the entry is off the calendar.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn a_moment_before_each_board_entry_fires_once_and_follows_the_records() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_server(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    declare_app(
        &host,
        "so.tilo.calendar",
        tilo_surface_compile::compile(CALENDAR_SURFACE)
            .expect("the surface compiles")
            .as_bytes(),
    );
    let workspace = workspaces.create("Calendars").await.unwrap().workspace;
    let clock = Pinned::at(pinned_at("2026-10-19T06:00:00Z"));
    // The account whose server keeps the entries: what the calendar service's own writes stand
    // in, so each entry is nested under the account that holds it.
    keep(
        &workspaces,
        &workspace,
        "so.tilo.calendar",
        "tl.so.tilo.calendar.Account",
        "calendar-account",
        None,
        vec![
            ("key", text("account-1")),
            ("address", text("ada@example.test")),
            ("name", text("Ada")),
        ],
    )
    .await;
    let keep_event = async |id: &str,
                            key: &str,
                            times: (&str, &str),
                            categories: &[&str],
                            recurrence: &str,
                            count: Option<i64>,
                            exdates: &[&str],
                            apart: Option<(&str, &str)>| {
        let (start, end) = times;
        let mut fields = vec![
            ("key", text(key)),
            ("title", text(key)),
            ("start", text(start)),
            ("end", text(end)),
            ("timezone", text("Europe/Berlin")),
            ("cancelled", Cell::One(Value::Bool(false))),
            ("categories", set(categories)),
            ("exdates", set(exdates)),
            ("recurrence", text(recurrence)),
        ];
        if let Some(count) = count {
            fields.push(("recurrence_count", Cell::One(Value::Int(count))));
        }
        if let Some((series, original)) = apart {
            fields.push((
                "recurrence_of",
                text(&format!("tl:so.tilo.calendar:{series}")),
            ));
            fields.push(("original_start", text(original)));
        }
        keep(
            &workspaces,
            &workspace,
            "so.tilo.calendar",
            "tl.so.tilo.calendar.Event",
            id,
            Some("calendar-account"),
            fields,
        )
        .await;
    };

    // Every day at 09:30 in Berlin from Monday 19 October 2026, over the end of summer time on
    // the 25th: Wednesday is left out and Thursday is moved to 14:00. Lunch is filed under
    // nothing. Friday is also a whole day filed under Board.
    keep_event(
        "standup-1",
        "standup-1@example.org",
        ("2026-10-19T09:30:00", "2026-10-19T10:00:00"),
        &["Board"],
        "FREQ=DAILY",
        Some(9),
        &["2026-10-21"],
        None,
    )
    .await;
    keep_event(
        "standup-1-2026-10-22",
        "standup-1@example.org",
        ("2026-10-22T14:00:00", "2026-10-22T14:30:00"),
        &["Board"],
        "",
        None,
        &[],
        Some(("standup-1", "2026-10-22T09:30:00")),
    )
    .await;
    keep_event(
        "lunch-1",
        "lunch-1@example.org",
        ("2026-10-19T12:00:00", "2026-10-19T12:30:00"),
        &[],
        "",
        None,
        &[],
        None,
    )
    .await;
    keep_event(
        "offsite-1",
        "offsite-1@example.org",
        ("2026-10-23T00:00:00", "2026-10-24T00:00:00"),
        &["Board"],
        "",
        None,
        &[],
        None,
    )
    .await;

    save_signed(
        &workspaces,
        &workspace,
        before_board_entries(),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut expected: Vec<(String, i64)> = Vec::new();

    // Nothing before its moment, and its moment once, however often the runner looks.
    pass_at(&clock, &mut runner, "2026-10-19T07:14:59Z").await;
    assert_eq!(handed_moments(&started), expected);
    pass_at(&clock, &mut runner, "2026-10-19T07:15:00Z").await;
    expected.push(("2026-10-19".into(), moment("2026-10-19T07:30:00Z")));
    assert_eq!(handed_moments(&started), expected);
    pass_at(&clock, &mut runner, "2026-10-19T07:20:00Z").await;
    // An entry of no category is nobody's moment.
    pass_at(&clock, &mut runner, "2026-10-19T09:50:00Z").await;
    assert_eq!(handed_moments(&started), expected);

    // The next occurrence of the series is its own time.
    pass_at(&clock, &mut runner, "2026-10-20T07:15:00Z").await;
    expected.push(("2026-10-20".into(), moment("2026-10-20T07:30:00Z")));
    assert_eq!(handed_moments(&started), expected);

    // The day the series leaves out, and the hour an occurrence was moved away from: nothing.
    // The moved one fires before where it was moved to.
    pass_at(&clock, &mut runner, "2026-10-21T07:40:00Z").await;
    pass_at(&clock, &mut runner, "2026-10-22T07:40:00Z").await;
    assert_eq!(handed_moments(&started), expected);
    pass_at(&clock, &mut runner, "2026-10-22T11:45:00Z").await;
    expected.push(("2026-10-22".into(), moment("2026-10-22T12:00:00Z")));
    assert_eq!(handed_moments(&started), expected);

    // A whole day begins at its person's midnight in its own zone, 22:00 UTC the evening before
    // in summer time.
    pass_at(&clock, &mut runner, "2026-10-22T21:45:00Z").await;
    expected.push(("2026-10-23".into(), moment("2026-10-22T22:00:00Z")));
    assert_eq!(handed_moments(&started), expected);

    // The device starts over: what it answered is kept done in the workspace, so it answers
    // nothing twice, and the next moment still comes.
    drop(runner);
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    pass_at(&clock, &mut runner, "2026-10-23T07:00:00Z").await;
    assert_eq!(handed_moments(&started), expected);
    pass_at(&clock, &mut runner, "2026-10-23T07:15:00Z").await;
    expected.push(("2026-10-23".into(), moment("2026-10-23T07:30:00Z")));
    assert_eq!(handed_moments(&started), expected);

    // Down over Saturday's moment and back before the entry starts: late, and still ahead of it.
    pass_at(&clock, &mut runner, "2026-10-24T07:25:00Z").await;
    expected.push(("2026-10-24".into(), moment("2026-10-24T07:30:00Z")));
    assert_eq!(handed_moments(&started), expected);

    // Down over Sunday's moment and back once the entry has begun: nothing, then or later.
    // Summer time ended that night, so it began at 08:30 UTC.
    pass_at(&clock, &mut runner, "2026-10-25T08:31:00Z").await;
    pass_at(&clock, &mut runner, "2026-10-25T12:00:00Z").await;
    assert_eq!(handed_moments(&started), expected);

    // Monday is an hour later by UTC than a week before: 09:30 in Berlin stayed 09:30.
    pass_at(&clock, &mut runner, "2026-10-26T07:15:00Z").await;
    assert_eq!(handed_moments(&started), expected);
    pass_at(&clock, &mut runner, "2026-10-26T08:15:00Z").await;
    expected.push(("2026-10-26".into(), moment("2026-10-26T08:30:00Z")));
    assert_eq!(handed_moments(&started), expected);

    // Another client moves that occurrence to 11:00 after its moment was answered: it holds
    // another time, whose moment comes too.
    keep_event(
        "standup-1-2026-10-26",
        "standup-1@example.org",
        ("2026-10-26T11:00:00", "2026-10-26T11:30:00"),
        &["Board"],
        "",
        None,
        &[],
        Some(("standup-1", "2026-10-26T09:30:00")),
    )
    .await;
    pass_at(&clock, &mut runner, "2026-10-26T08:20:00Z").await;
    assert_eq!(handed_moments(&started), expected);
    pass_at(&clock, &mut runner, "2026-10-26T09:45:00Z").await;
    expected.push(("2026-10-26".into(), moment("2026-10-26T10:00:00Z")));
    assert_eq!(handed_moments(&started), expected);

    // The series is taken off the calendar before Tuesday's moment: nothing is held any more,
    // and nothing fires.
    for id in ["standup-1", "standup-1-2026-10-26", "standup-1-2026-10-22"] {
        forget(
            &workspaces,
            &workspace,
            "so.tilo.calendar",
            "tl.so.tilo.calendar.Event",
            id,
        )
        .await;
    }
    pass_at(&clock, &mut runner, "2026-10-27T08:15:00Z").await;
    pass_at(&clock, &mut runner, "2026-10-27T09:00:00Z").await;
    assert_eq!(handed_moments(&started), expected);
    host.shutdown().await;
}

/// A shortcut that fires when a message from Bo arrives in the workspace's mail.
fn from_bo() -> Shortcut {
    shortcut(
        "watch-bo",
        tilo_automation::TriggerSpec::SourceChanged {
            kind: "tilo.accounts.mail".into(),
            at: String::new(),
            every_secs: None,
            filter: filter(&json!({ "from": "bo@example.test" })),
        },
    )
}

/// What the mail heard so far was made from: for each, who wrote and what about, and the message
/// itself as the run is made from it.
fn heard_mail(started: &Started) -> Vec<(String, String, String, String)> {
    let mut heard: Vec<(String, String, String, String)> = started
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|firing| {
            let Some(event) = firing.inputs.get(tilo_automation::EVENT_INPUT) else {
                unreachable!("a fired moment carries its event");
            };
            let facts = &event["facts"];
            let from = &event["$from"];
            (
                facts["from"].as_str().unwrap_or_default().to_owned(),
                facts["subject"].as_str().unwrap_or_default().to_owned(),
                from["ref"].as_str().unwrap_or_default().to_owned(),
                from["ty"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    // The firings' order is the store's, which is no part of what a trigger answers.
    heard.sort();
    heard
}

/// The Mail app's account `id` of `address`, kept as the mail connector keeps it: the place its
/// conversations, and their messages, stand in.
async fn keep_account(workspaces: &Workspaces, workspace: &str, id: &str, address: &str) {
    keep(
        workspaces,
        workspace,
        "so.tilo.mail",
        "tl.so.tilo.mail.Account",
        id,
        None,
        vec![
            ("key", text(id)),
            ("address", text(address)),
            ("name", text("Ada")),
        ],
    )
    .await;
}

/// The conversation `id` of `account`, kept as the mail connector keeps it: the place its
/// messages stand in.
async fn keep_thread(
    workspaces: &Workspaces,
    workspace: &str,
    id: &str,
    account: &str,
    subject: &str,
) {
    keep(
        workspaces,
        workspace,
        "so.tilo.mail",
        "tl.so.tilo.mail.Thread",
        id,
        Some(account),
        vec![("key", text(id)), ("subject", text(subject))],
    )
    .await;
}

/// One message `id` in `thread`, in `mailbox`, from `from` (`from_name`) about `subject`, received
/// at `received`, kept in `workspace`'s Mail as the app keeps it.
async fn keep_message(
    workspaces: &Workspaces,
    workspace: &str,
    thread: &str,
    id: &str,
    mailbox: &str,
    from: (&str, &str),
    subject: &str,
    received: &str,
) {
    keep(
        workspaces,
        workspace,
        "so.tilo.mail",
        "tl.so.tilo.mail.Message",
        id,
        Some(thread),
        vec![
            ("key", text(id)),
            ("account", text("ada@example.test")),
            ("mailbox", text(mailbox)),
            ("from", text(from.0)),
            ("from_name", text(from.1)),
            ("to", text("ada@example.test")),
            ("cc", text("")),
            ("subject", text(subject)),
            ("received", text(received)),
            ("snippet", text("Could we meet on Friday?")),
            ("message_id", text(id)),
            ("unread", Cell::One(Value::Bool(true))),
        ],
    )
    .await;
}

/// The messages that arrive after the trigger was set up: two from Bo, and one from someone else,
/// in the workspace's inbox conversation.
async fn messages_after_setup(workspaces: &Workspaces, workspace: &str) {
    for (id, from, from_name, subject, received) in [
        (
            "new-1",
            "bo@example.test",
            "Bo",
            "Friday",
            "2026-11-23T08:10:00Z",
        ),
        (
            "new-2",
            "bo@example.test",
            "Bo",
            "Lunch",
            "2026-11-23T08:11:00Z",
        ),
        (
            "other-1",
            "cy@example.test",
            "Cy",
            "Elsewhere",
            "2026-11-23T08:12:00Z",
        ),
    ] {
        keep_message(
            workspaces,
            workspace,
            "thread-1",
            id,
            "inbox",
            (from, from_name),
            subject,
            received,
        )
        .await;
    }
}

/// The runner a device starts again, armed and passed at `at`.
async fn restarted(
    workspaces: &Workspaces,
    started: &Arc<Started>,
    clock: &Pinned,
    at: &str,
) -> Runner {
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    pass_at(clock, &mut runner, at).await;
    runner
}

/// A message arriving after the trigger was set up fires it once, a message the filter leaves
/// out fires nothing, one the person sent is no arrival, and a restart hears nothing twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_message_fires_the_shortcut_watching_mail_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_server(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    declare_app(
        &host,
        "so.tilo.mail",
        tilo_surface_compile::compile(MAIL_SURFACE)
            .expect("the surface compiles")
            .as_bytes(),
    );
    let workspace = workspaces.create("Mail").await.unwrap().workspace;
    let clock = Pinned::at(pinned_at("2026-11-23T08:00:00Z"));
    keep_account(&workspaces, &workspace, "account-ada", "ada@example.test").await;
    keep_thread(&workspaces, &workspace, "thread-1", "account-ada", "Friday").await;

    // What its inbox held before the trigger was set up is never new.
    keep_message(
        &workspaces,
        &workspace,
        "thread-1",
        "old",
        "inbox",
        ("bo@example.test", "Bo"),
        "Before",
        "2026-11-23T07:00:00Z",
    )
    .await;
    save_signed(&workspaces, &workspace, from_bo(), (&key, &cert)).await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    pass_at(&clock, &mut runner, "2026-11-23T08:00:00Z").await;
    assert!(
        heard_mail(&started).is_empty(),
        "what was there is not news"
    );

    // Two messages from Bo arrive, and one from someone else: the two fire, once each.
    messages_after_setup(&workspaces, &workspace).await;
    pass_at(&clock, &mut runner, "2026-11-23T08:13:00Z").await;
    assert_eq!(
        heard_mail(&started),
        [
            (
                "bo@example.test".to_owned(),
                "Friday".to_owned(),
                "tl:so.tilo.mail:new-1".to_owned(),
                "tl.so.tilo.mail.Message".to_owned(),
            ),
            (
                "bo@example.test".to_owned(),
                "Lunch".to_owned(),
                "tl:so.tilo.mail:new-2".to_owned(),
                "tl.so.tilo.mail.Message".to_owned(),
            ),
        ],
        "each run is made from the message itself"
    );

    // Looking again, and a message the person sent rather than received, fires nothing more.
    keep_message(
        &workspaces,
        &workspace,
        "thread-1",
        "sent-1",
        "sent",
        ("ada@example.test", "Ada"),
        "Re: Friday",
        "2026-11-23T08:14:00Z",
    )
    .await;
    pass_at(&clock, &mut runner, "2026-11-23T08:15:00Z").await;
    assert_eq!(heard_mail(&started).len(), 2, "nothing fires twice");

    // The device starts over: what it heard is kept, and the next message comes once.
    drop(runner);
    let mut runner = restarted(&workspaces, &started, &clock, "2026-11-23T08:20:00Z").await;
    assert_eq!(
        heard_mail(&started).len(),
        2,
        "a restart hears nothing twice"
    );
    keep_message(
        &workspaces,
        &workspace,
        "thread-1",
        "new-3",
        "inbox",
        ("bo@example.test", "Bo"),
        "Monday",
        "2026-11-23T08:21:00Z",
    )
    .await;
    pass_at(&clock, &mut runner, "2026-11-23T08:22:00Z").await;
    assert_eq!(heard_mail(&started).len(), 3);
    assert_eq!(heard_mail(&started)[2].1, "Monday");
    host.shutdown().await;
}

/// A shortcut kept to one of the workspace's accounts hears that account's mail and no other's:
/// a message arriving in the workspace's other account, its shared mailbox, fires nothing of
/// hers, and either account's message fires once and not again on the next look.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shortcut_kept_to_her_account_hears_her_mail_and_no_other_account() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_server(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    declare_app(
        &host,
        "so.tilo.mail",
        tilo_surface_compile::compile(MAIL_SURFACE)
            .expect("the surface compiles")
            .as_bytes(),
    );
    let workspace = workspaces.create("Mail").await.unwrap().workspace;
    let clock = Pinned::at(pinned_at("2026-11-23T08:00:00Z"));
    // The workspace's two accounts, each with its own conversation.
    for (id, address, thread) in [
        ("account-ada", "ada@example.test", "thread-ada"),
        ("account-team", "team@example.test", "thread-team"),
    ] {
        keep_account(&workspaces, &workspace, id, address).await;
        keep_thread(&workspaces, &workspace, thread, id, address).await;
    }
    let keep_message = async |id: &str, account: &str, received: &str| {
        let thread = if account == "ada@example.test" {
            "thread-ada"
        } else {
            "thread-team"
        };
        keep(
            &workspaces,
            &workspace,
            "so.tilo.mail",
            "tl.so.tilo.mail.Message",
            id,
            Some(thread),
            vec![
                ("key", text(id)),
                ("account", text(account)),
                ("mailbox", text("inbox")),
                ("from", text("bo@example.test")),
                ("from_name", text("Bo")),
                ("to", text(account)),
                ("cc", text("")),
                ("subject", text(id)),
                ("received", text(received)),
                ("snippet", text("Friday?")),
                ("message_id", text(id)),
                ("unread", Cell::One(Value::Bool(true))),
            ],
        )
        .await;
    };
    let kept = shortcut(
        "my-mail",
        tilo_automation::TriggerSpec::SourceChanged {
            kind: "tilo.accounts.mail".into(),
            at: "ada@example.test".into(),
            every_secs: None,
            filter: filter(&json!({})),
        },
    );
    save_signed(&workspaces, &workspace, kept, (&key, &cert)).await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    pass_at(&clock, &mut runner, "2026-11-23T08:00:00Z").await;

    // Her own account's message fires it; the workspace's other account is no source for it.
    keep_message("ada-1", "ada@example.test", "2026-11-23T08:10:00Z").await;
    keep_message("team-1", "team@example.test", "2026-11-23T08:11:00Z").await;
    pass_at(&clock, &mut runner, "2026-11-23T08:12:00Z").await;
    let heard = heard_mail(&started);
    assert_eq!(heard.len(), 1, "only her own account: {heard:?}");
    assert_eq!(heard[0].2, "tl:so.tilo.mail:ada-1");

    // Looking again fires nothing twice, and what arrives next fires once.
    pass_at(&clock, &mut runner, "2026-11-23T08:13:00Z").await;
    assert_eq!(heard_mail(&started).len(), 1, "nothing fires twice");
    keep_message("ada-2", "ada@example.test", "2026-11-23T08:14:00Z").await;
    pass_at(&clock, &mut runner, "2026-11-23T08:15:00Z").await;
    let heard = heard_mail(&started);
    assert_eq!(heard.len(), 2, "{heard:?}");
    assert_eq!(heard[1].2, "tl:so.tilo.mail:ada-2");
    host.shutdown().await;
}

/// Chat's messages, with the rich body a mention lives in.
const CHAT_SURFACE: &str = r#"format = 2

[[resources]]
kind = "tl.so.tilo.chat.Message"
label = "Message"
version = 1
fields = [
  { path = "body", type = "rich_text", stored = "value" },
]

[[resources.verbs]]
id = "send"
kind = "undoable"
params = []
"#;

/// A mention written the way Chat's window writes one — through the app's certified route, as
/// the person at it — fires the agent's trigger for it: the store hands the trigger the
/// message's rich body as the write left it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn a_mention_written_through_an_apps_route_fires_the_agents_trigger() {
    use collab_types::store::wire::{Command, Edit, Reply};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_server(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    declare_app(
        &host,
        "so.tilo.chat",
        tilo_surface_compile::compile(CHAT_SURFACE)
            .expect("the surface compiles")
            .as_bytes(),
    );
    let workspace = workspaces.create("Chat").await.unwrap().workspace;
    let person = hex::encode(cert.user_public_key);
    save_signed(
        &workspaces,
        &workspace,
        tilo_workspace_service::agent_shortcuts::triggers("scout", &person, "Scout"),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();

    // What Chat's window writes for a message that mentions the person's assistant: the item,
    // and its rich body with the mention's mark, as `MemberId::mark_value` writes it.
    let stamp = Stamp {
        // Chat's own `send`, the verb its surface declares for a message it writes.
        verb: "send".into(),
        invoker: Stamp::by(&tilo_action_policy::Actor::Member(
            tilo_action_policy::MemberId::person(person.clone()),
        )),
        invocation: "i-1".into(),
        at: jiff::Timestamp::now().to_string(),
        ..Stamp::default()
    };
    let member = tilo_action_policy::MemberId::agent("scout", person.clone()).mark_value();
    let body = json!({"blocks": [{
        "text": "@Scout Remind me in a moment",
        "marks": [{"start": 0, "end": 6, "name": "mention", "value": {"Str": member}}],
    }]});
    let wrote = workspaces
        .own_resource_command(
            &workspace,
            "so.tilo.chat",
            Command::Batch(vec![
                Command::Put {
                    kind: "tl.so.tilo.chat.Message".into(),
                    id: "m-1".into(),
                    resource: Resource {
                        readers: "*".into(),
                        parent: None,
                        order: None,
                        fields: std::collections::BTreeMap::new(),
                    },
                    stamp: stamp.clone(),
                },
                Command::Edit {
                    kind: "tl.so.tilo.chat.Message".into(),
                    id: "m-1".into(),
                    edits: vec![Edit::Set {
                        field: "body".into(),
                        value: body,
                    }],
                    stamp,
                },
            ]),
            Vec::new(),
        )
        .await
        .expect("the app's write lands");
    assert!(
        matches!(wrote, Reply::Written),
        "the write landed: {wrote:?}"
    );

    let mut since = Vec::new();
    while let Ok(write) = writes.try_recv() {
        since.push(write);
    }
    runner.pass(since).await;
    assert_eq!(
        started.0.lock().unwrap().len(),
        1,
        "the mention fires the agent's trigger"
    );
    host.shutdown().await;
}

/// Chat's messages as the tilolang Chat keeps them, with the files a message carries nested in
/// it: what a send with attachments writes.
const CHAT_WITH_FILES_SURFACE: &str = r#"format = 2

[[resources]]
kind = "tl.so.tilo.chat.Message"
label = "Message"
version = 1
fields = [
  { path = "body", type = "rich_text", stored = "value" },
  { path = "attachments", type = { collection = { resource = "tl.so.tilo.chat.Attached" } }, stored = "collection" },
]

[[resources.verbs]]
id = "send"
kind = "undoable"
params = []

[[resources]]
kind = "tl.so.tilo.chat.Attached"
label = "Attached"
version = 1
fields = [
  { path = "name", type = "text" },
  { path = "blob", type = "text" },
]
"#;

/// A message sent with a file carried along and a mention of the agent is one write of the
/// message and its file: the trigger on the message's body fires, as it does for a message that
/// carries none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn a_message_that_carries_a_file_still_fires_its_mention() {
    use collab_types::store::wire::{Command, Edit, Reply};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_server(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    declare_app(
        &host,
        "so.tilo.chat",
        tilo_surface_compile::compile(CHAT_WITH_FILES_SURFACE)
            .expect("the surface compiles")
            .as_bytes(),
    );
    let workspace = workspaces.create("Chat").await.unwrap().workspace;
    let person = hex::encode(cert.user_public_key);
    save_signed(
        &workspaces,
        &workspace,
        tilo_workspace_service::agent_shortcuts::triggers("scout", &person, "Scout"),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();

    // What Chat's `send` writes: the message with its rich body, then the file it carries
    // nested in it. All of it is the one write, stamped as the person's own `send`.
    let stamp = Stamp {
        verb: "send".into(),
        invoker: Stamp::by(&tilo_action_policy::Actor::Member(
            tilo_action_policy::MemberId::person(person.clone()),
        )),
        invocation: "i-1".into(),
        at: jiff::Timestamp::now().to_string(),
        ..Stamp::default()
    };
    let member = tilo_action_policy::MemberId::agent("scout", person.clone()).mark_value();
    let body = json!({"blocks": [{
        "text": "@Scout look at this",
        "marks": [{"start": 0, "end": 6, "name": "mention", "value": {"Str": member}}],
    }]});
    let wrote = workspaces
        .own_resource_command(
            &workspace,
            "so.tilo.chat",
            Command::Batch(vec![
                Command::Put {
                    kind: "tl.so.tilo.chat.Message".into(),
                    id: "m-1".into(),
                    resource: Resource {
                        readers: "*".into(),
                        parent: None,
                        order: None,
                        fields: std::collections::BTreeMap::new(),
                    },
                    stamp: stamp.clone(),
                },
                Command::Edit {
                    kind: "tl.so.tilo.chat.Message".into(),
                    id: "m-1".into(),
                    edits: vec![Edit::Set {
                        field: "body".into(),
                        value: body,
                    }],
                    stamp: stamp.clone(),
                },
                Command::Put {
                    kind: "tl.so.tilo.chat.Attached".into(),
                    id: "a-1".into(),
                    resource: Resource {
                        readers: "*".into(),
                        parent: Some("m-1".into()),
                        order: Some("a".into()),
                        fields: std::collections::BTreeMap::from([(
                            "name".into(),
                            text("plan.txt"),
                        )]),
                    },
                    stamp,
                },
            ]),
            Vec::new(),
        )
        .await
        .expect("the app's write lands");
    assert!(
        matches!(wrote, Reply::Written),
        "the write landed: {wrote:?}"
    );

    let mut since = Vec::new();
    while let Ok(write) = writes.try_recv() {
        since.push(write);
    }
    let events = runner.read(since).await;
    let message = events
        .iter()
        .find(|event| event.record == "m-1")
        .expect("the message's own event");
    assert!(
        message
            .after
            .as_ref()
            .and_then(|after| after.pointer("/body"))
            .is_some_and(|body| body.to_string().contains("scout")),
        "the message's event holds the body it was sent with: {message:?}"
    );
    assert!(
        message
            .after
            .as_ref()
            .and_then(|after| after.pointer("/$held"))
            .is_some_and(|held| held.to_string().contains("plan.txt")),
        "the event carries the file the message holds: {message:?}"
    );
    let fired = super::dispatched(&runner.armed, super::Event::Record(message), 0);
    assert!(
        fired
            .iter()
            .any(|waiting| waiting.firing.shortcut == "agent-scout"),
        "the mention fires the agent's trigger though the message carries a file"
    );
    host.shutdown().await;
}
