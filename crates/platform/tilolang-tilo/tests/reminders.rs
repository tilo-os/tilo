//! Reminders as the workspace holds them (`tilolang_tilo::reminders`): a type whose field roles
//! say it reminds, the reminders of the workspace's records that have come due for one person,
//! and the mark in the person's own state that keeps their devices from telling them twice.

use std::sync::OnceLock;

use automerge::AutoCommit;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use tilo_actions::ActionContext;
use tilo_resource::Resource as _;
use tilolang_runtime::{InvokeOutcome, Principal, Value};
use tilolang_tilo::personal::{self, Keep, Personal, PersonalState};
use tilolang_tilo::reminders::{Alarm, Look};

use crate::fixtures::Replica;

const APP: &str = "t.remind";

/// A package with a type that reminds: a task due on a day, at a moment, for a person, and how
/// long before that it remembers them.
const SOURCE: &str = r#"app t.remind "Remind"

type Task {
    title: Text (title)
    done: Bool (done)
    due: Option<Date> = .none
    at: Option<Time> = .none
    who: Option<Principal> = .none
    remind: Option<std.Reminder> (reminds: due, at, who: who) = .none
}

type Board {
    title: Text
    tasks: Collection<Task>
}

primary action new_task(
    b: Board,
    title: Text,
    due: Date,
    at: Option<Time>,
    who: Option<Principal>,
    remind: std.Reminder,
) -> Task (label: "New task") {
    b.tasks.insert(Task(title: title, due: .some(due), at: at, who: who, remind: .some(remind)))
}

action finish(t: Task) (label: "Done") { t.done = true }

action postpone(t: Task, days: Int) (label: "Postpone") { t.due = .none }

default view main for Board

view main(b: Board) {
    page {
        list(b.tasks) { t -> row { text(t.title) } }
    }
}

root Board
"#;

/// The package's ids, so two replicas resolve the same type and field ids.
fn ids() -> &'static str {
    static IDS: OnceLock<String> = OnceLock::new();
    IDS.get_or_init(|| {
        tilolang::compile_package_with(
            SOURCE,
            &tilolang::IdsLock::new(),
            &[],
            &mut tilolang::SeededEntropy(7),
        )
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
        .ids
        .to_text()
    })
}

fn packages() -> Vec<(&'static str, &'static str)> {
    vec![(SOURCE, ids())]
}

/// A day, and a time of day on it, in UTC: the zone these tests read a reminder in.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature the scheduler reads a person's zone in"
)]
fn utc(day: NaiveDate, time: Option<NaiveTime>) -> Option<DateTime<Utc>> {
    Some(day.and_time(time.unwrap_or(NaiveTime::MIN)).and_utc())
}

/// The instant of the 27th at `hour`:`minute`, in UTC.
fn at(hour: u32, minute: u32) -> DateTime<Utc> {
    NaiveDate::from_ymd_opt(2026, 10, 27)
        .unwrap()
        .and_hms_opt(hour, minute, 0)
        .unwrap()
        .and_utc()
}

/// Ada's replica of the workspace at noon the day before, with a task she gave Bo: due 10:00 on
/// the 27th, reminding him half an hour before.
fn workspace() -> (Replica, Value) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &packages());
    ada.host
        .runtime()
        .set_clock(|| "2026-10-26T12:00:00Z".to_owned());
    let root = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let action = ada
        .host
        .package(APP)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|action| action.name == "new_task")
        .unwrap()
        .id
        .clone();
    let made = ada
        .host
        .runtime()
        .invoke_action(
            &Principal::new("ada"),
            (
                action.as_str(),
                vec![
                    root,
                    Value::text("Call the landlord"),
                    Value::text("2026-10-27"),
                    Value::some(Value::text("2026-10-27T10:00")),
                    Value::some(Value::Principal(Principal::new("bo"))),
                    Value::text("-PT30M"),
                ],
            ),
        )
        .unwrap();
    let InvokeOutcome::Done(task) = made else {
        panic!("the task is made: {made:?}");
    };
    ada.commit();
    (ada, task)
}

/// The person's own state of the workspace, as their record holds it.
fn personal_of(replica: &Replica) -> PersonalState {
    Personal::read(&replica.doc).expect("the personal record reads")
}

/// What the workspace `replica` holds for `person` at `now`.
fn look(replica: &Replica, person: &str, now: DateTime<Utc>) -> Look {
    tilolang_tilo::reminders::look(&replica.host, person, now, &utc, &personal_of(replica))
}

/// Bo's replica of the workspace `ada` made.
fn bo(ada: &mut Replica) -> Replica {
    ada.fork("bo", &packages())
}

/// The one alarm `look` finds, or a panic naming what was found instead.
fn only(found: &Look) -> &Alarm {
    assert_eq!(found.due.len(), 1, "one reminder: {found:#?}");
    &found.due[0]
}

/// The resource of `alarm`'s item, as the app's action door hands it an action.
fn item(alarm: &Alarm) -> Value {
    let target = tilolang_runtime::value::ResourceRef::parse_id(&alarm.path, alarm.ty.as_str())
        .expect("the item reads");
    Value::resource(&target)
}

/// Run the action `name` of `replica`'s package on `args`, as the app runs what a notification
/// offers, and commit what it changed.
fn run(replica: &mut Replica, action: &str, args: Vec<Value>) {
    let done = replica
        .host
        .runtime()
        .invoke_action(&Principal::new(&replica.name), (action, args))
        .unwrap();
    assert!(matches!(done, InvokeOutcome::Done(_)), "{action}: {done:?}");
    replica.commit();
}

#[test]
fn a_reminder_comes_at_its_instant_for_the_person_it_names_only() {
    let (mut ada, _) = workspace();
    let bo = bo(&mut ada);
    assert!(
        look(&ada, "bo", at(9, 0)).due.is_empty(),
        "half an hour early"
    );
    assert_eq!(
        look(&ada, "bo", at(9, 0)).next,
        Some(at(9, 30)),
        "the window knows when to wake"
    );
    let ada_look = look(&ada, "ada", at(9, 30));
    assert!(
        ada_look.due.is_empty() && ada_look.next.is_none(),
        "the task is Bo's, not Ada's: {ada_look:#?}"
    );
    let found = look(&bo, "bo", at(9, 30));
    let alarm = only(&found);
    assert_eq!(alarm.title, "Call the landlord");
    assert_eq!(alarm.app_title, "Remind");
    assert_eq!(alarm.app, APP);
    assert_eq!(alarm.person, "bo");
    assert_eq!(alarm.at, at(9, 30));
    assert_eq!(alarm.deadline, at(10, 0));
    assert_eq!(alarm.resource(), format!("tl:{APP}:{}", alarm.path));
    assert_eq!(alarm.tag().len(), 15, "the shortest tag a platform takes");
    let offered: Vec<&str> = alarm
        .actions
        .iter()
        .map(|offer| offer.label.as_str())
        .collect();
    assert_eq!(
        offered,
        vec!["Done"],
        "only the actions that ask nothing can be a button"
    );
    assert_eq!(found.next, None, "nothing else of Bo's is coming");
}

#[test]
fn a_done_item_reminds_nobody() {
    let (mut ada, _) = workspace();
    let mut bo = bo(&mut ada);
    let alarm = only(&look(&bo, "bo", at(9, 30))).clone();
    run(&mut bo, &alarm.actions[0].id, vec![item(&alarm)]);
    let found = look(&bo, "bo", at(9, 30));
    assert!(
        found.due.is_empty(),
        "a done task reminds nobody: {found:#?}"
    );
    assert_eq!(found.next, None);
}

#[test]
fn a_reminder_whose_instant_passed_comes_when_a_device_comes_up_marked_late() {
    let (mut ada, _) = workspace();
    // None of Bo's devices ran at 9:30: this one starts at 10:05.
    let late = bo(&mut ada);
    let now = at(10, 5);
    let found = look(&late, "bo", now);
    let alarm = only(&found);
    assert_eq!(alarm.at, at(9, 30), "the instant it should have come");
    assert!(
        alarm.at < now,
        "delivered after its instant, which is what makes it late"
    );
}

#[test]
fn a_reminder_a_person_was_told_reaches_none_of_their_devices_again() {
    let (mut ada, _) = workspace();
    let mut first = bo(&mut ada);
    let mut second = bo(&mut ada);
    let alarm = only(&look(&first, "bo", at(9, 30))).clone();
    // The device that showed it keeps the mark, as the window does after showing.
    let keep = Keep::Reminded {
        person: "bo".to_owned(),
        reminder: alarm.key,
    };
    let encoded = tilo_resource::encode::<Personal>(&keep).expect("the mark encodes");
    let context = ActionContext::for_resource(personal::KIND, personal::KIND, Personal::LABEL)
        .with_invoker(tilo_actions::ActionInvoker::actor("bo"));
    tilo_actions::apply_local_with_context(&mut first.doc, &keep, encoded, None, context)
        .expect("the mark applies");
    first.doc.commit();
    first.host.sync_from(&first.doc);
    assert_eq!(
        personal_of(&first).of("bo").told.len(),
        1,
        "bo was told once"
    );
    assert!(
        look(&first, "bo", at(9, 31)).due.is_empty(),
        "the device that told him does not tell him again"
    );
    let kept = look(&first, "bo", at(9, 31));
    assert_eq!(
        kept.heard.len(),
        1,
        "the reminder still stands, so the one he was shown stays: {kept:#?}"
    );
    // The other device reads the same mark once the record reaches it.
    first.sync_with(&mut second);
    let found = look(&second, "bo", at(9, 31));
    assert!(
        found.due.is_empty(),
        "one notification for one person: {found:#?}"
    );
    assert_eq!(found.heard.len(), 1, "and it hears of what he was told");
    // Before the mark arrived, the second device would have shown it: the mark is the only thing
    // that keeps the two apart.
    let (mut ada, _) = workspace();
    let other = bo(&mut ada);
    assert_eq!(look(&other, "bo", at(9, 31)).due.len(), 1);
}

#[test]
fn the_action_a_notification_offers_runs_on_the_item_it_is_about() {
    let (mut ada, _) = workspace();
    let mut bo = bo(&mut ada);
    let alarm = only(&look(&bo, "bo", at(9, 30))).clone();
    let done = alarm
        .actions
        .iter()
        .find(|offer| offer.label == "Done")
        .expect("the type offers it")
        .id
        .clone();
    run(&mut bo, &done, vec![item(&alarm)]);
    assert!(
        look(&bo, "bo", at(9, 31)).due.is_empty(),
        "the action the notification offered did what the app does"
    );
}
