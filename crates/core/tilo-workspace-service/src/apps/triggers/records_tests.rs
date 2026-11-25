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

use serde_json::json;
use tilo_automation::Shortcut;
use tilo_resource_store::wire::Command;
use tilo_resource_store::{Cell, Resource, Stamp, Value};

use super::{Order, Runner, StartRun};
use crate::workspaces::MemberDevice;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{
    Started, declare_app, governor, save_signed, test_host as test_server,
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
  { id = "$origin", label = "Keep in step with its service", kind = "undoable" },
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
  { id = "$origin", label = "Keep in step with its service", kind = "undoable" },
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
        self.0
            .pin(UNIX_EPOCH + std::time::Duration::from_millis(now_ms));
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

/// Keep `fields` as the record `id` of `kind`, written by `app` as its origin writes its records.
async fn keep(
    workspaces: &Workspaces,
    workspace: &str,
    app: &str,
    kind: &str,
    id: &str,
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
                    parent: None,
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
fn filter(object: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
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
            filter: filter(json!({ "categories": "Board" })),
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
            let event = &firing.inputs[tilo_automation::EVENT_INPUT];
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
    let (key, cert) = governor();
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
            filter: filter(json!({ "from": "bo@example.test" })),
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
            let event = &firing.inputs[tilo_automation::EVENT_INPUT];
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

/// A message arriving after the trigger was set up fires it once, a message the filter leaves
/// out fires nothing, one the person sent is no arrival, and a restart hears nothing twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_message_fires_the_shortcut_watching_mail_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
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
    let keep_message = async |id: &str,
                              mailbox: &str,
                              from: &str,
                              from_name: &str,
                              subject: &str,
                              received: &str| {
        keep(
            &workspaces,
            &workspace,
            "so.tilo.mail",
            "tl.so.tilo.mail.Message",
            id,
            vec![
                ("key", text(id)),
                ("account", text("ada@example.test")),
                ("mailbox", text(mailbox)),
                ("from", text(from)),
                ("from_name", text(from_name)),
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
    };

    // What its inbox held before the trigger was set up is never new.
    keep_message(
        "old",
        "inbox",
        "bo@example.test",
        "Bo",
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
        keep_message(id, "inbox", from, from_name, subject, received).await;
    }
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
        "sent-1",
        "sent",
        "ada@example.test",
        "Ada",
        "Re: Friday",
        "2026-11-23T08:14:00Z",
    )
    .await;
    pass_at(&clock, &mut runner, "2026-11-23T08:15:00Z").await;
    assert_eq!(heard_mail(&started).len(), 2, "nothing fires twice");

    // The device starts over: what it heard is kept, and the next message comes once.
    drop(runner);
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    pass_at(&clock, &mut runner, "2026-11-23T08:20:00Z").await;
    assert_eq!(
        heard_mail(&started).len(),
        2,
        "a restart hears nothing twice"
    );
    keep_message(
        "new-3",
        "inbox",
        "bo@example.test",
        "Bo",
        "Monday",
        "2026-11-23T08:21:00Z",
    )
    .await;
    pass_at(&clock, &mut runner, "2026-11-23T08:22:00Z").await;
    assert_eq!(heard_mail(&started).len(), 3);
    assert_eq!(heard_mail(&started)[2].1, "Monday");
    host.shutdown().await;
}
