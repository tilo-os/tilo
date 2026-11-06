//! Triggers (`tilolang_tilo::triggers`): an action a package declares runs by itself, every so
//! often or at the time a record's field names, for each of its records of a type, and exactly
//! once across the workspace's devices.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use automerge::AutoCommit;
use tilo_render_protocol::patch::local::pair;
use tilolang_runtime::meta::Meta;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{InvokeOutcome, Principal};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::NameHost;

use crate::fixtures::Replica;

const APP: &str = "t.trig";

/// A package whose board counts its ticks every half hour, and whose reminders ring once at the
/// time each holds.
const SOURCE: &str = r#"app t.trig "Trig"

type Board {
    title: Text
    ticks: Int merge counter
}

type Reminder {
    at: Option<Time> = .none
    rings: Int merge counter
}

action tick(b: Board) { b.ticks += 1 }

action ring(r: Reminder) { r.rings += 1 }

action remind(r: Reminder, at: Time) { r.at = .some(at) }

trigger often(b: Board) every 30m: tick(b)

trigger due(r: Reminder) at r.at: ring(r)

root Board
root Reminder

default view board for Board

view board(b: Board) {
    page { text("{b.ticks}") }
}

default view reminder for Reminder

view reminder(r: Reminder) {
    page { text("{r.rings}") }
}
"#;

/// The package's ids, so every replica of the workspace resolves the same ids.
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

/// The package's metadata.
fn meta(replica: &Replica) -> Meta {
    replica.host.package(APP).unwrap().meta
}

/// The id of the type `ty`.
fn type_id(replica: &Replica, ty: &str) -> String {
    meta(replica)
        .types
        .iter()
        .find(|declared| declared.name == ty)
        .map(|declared| declared.id.clone())
        .unwrap()
}

/// The id of the action `name`.
fn action_id(replica: &Replica, name: &str) -> String {
    meta(replica)
        .actions
        .iter()
        .find(|action| action.name == name)
        .map(|action| action.id.clone())
        .unwrap()
}

/// The value of the counter field `field` on the type `ty` of the record `doc`.
fn counted(replica: &Replica, doc: &str, ty: &str, field: &str) -> i64 {
    let id = crate::fixtures::field_id(&meta(replica), ty, field);
    match replica.host.store().record(doc).unwrap().get(&id) {
        Some(tilolang_tilo::record::DynValue::Counter(counter)) => counter.get(),
        other => panic!("not a counter: {other:?}"),
    }
}

/// A replica with the package loaded, its board root made, and a reminder root made.
fn workspace() -> (Replica, String, String) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &packages());
    let board = ada.host.root_of(APP).unwrap().doc;
    let reminder = reminder_root(&ada);
    ada.commit();
    (ada, board, reminder)
}

/// The id of the reminder root, made if it is not there.
fn reminder_root(replica: &Replica) -> String {
    let ty = type_id(replica, "Reminder");
    replica.host.root_record(APP, &ty).unwrap().doc
}

/// Run the triggers and commit what they did, as a window of the app does each turn.
fn turn(replica: &mut Replica, now: i64) -> usize {
    let ran = replica.host.run_triggers(now);
    replica.commit();
    ran
}

/// Invoke the action `name` of the package on `args`, as a person at the window, and commit.
fn run(replica: &mut Replica, name: &str, args: Vec<Value>) {
    let id = action_id(replica, name);
    let done = replica
        .host
        .runtime()
        .invoke_action(&Principal::new(&replica.name), (id.as_str(), args))
        .unwrap();
    assert!(matches!(done, InvokeOutcome::Done(_)), "{name}: {done:?}");
    replica.commit();
}

/// Set the reminder `doc`'s time to `at`, as a person at the window.
fn remind_at(replica: &mut Replica, doc: &str, at: &str) {
    let subject = ResourceRef::root(doc.to_owned(), type_id(replica, "Reminder"));
    run(
        replica,
        "remind",
        vec![Value::resource(&subject), Value::text(at)],
    );
}

/// The start of the half hour a `now` is in, and the half hour before it.
const PERIOD: i64 = 30 * 60;

#[test]
fn a_schedule_runs_once_a_period() {
    let (mut ada, board, _) = workspace();
    let noon = 1_790_000_400 / PERIOD * PERIOD;

    assert_eq!(turn(&mut ada, noon), 1, "the half hour is due");
    assert_eq!(counted(&ada, &board, "Board", "ticks"), 1);

    assert_eq!(turn(&mut ada, noon + 60), 0, "not twice in one half hour");
    assert_eq!(counted(&ada, &board, "Board", "ticks"), 1);

    assert_eq!(turn(&mut ada, noon + PERIOD), 1, "the next half hour");
    assert_eq!(counted(&ada, &board, "Board", "ticks"), 2);
}

#[test]
fn a_trigger_at_a_records_time_fires_and_rearms_when_it_changes() {
    let (mut ada, _, reminder) = workspace();
    let ten = tilolang::calendar::instant("2026-10-27T10:00:00Z").unwrap();
    let eleven = tilolang::calendar::instant("2026-10-27T11:00:00Z").unwrap();
    let rings = |ada: &Replica| counted(ada, &reminder, "Reminder", "rings");

    remind_at(&mut ada, &reminder, "2026-10-27T10:00:00Z");
    turn(&mut ada, ten - 1);
    assert_eq!(rings(&ada), 0, "not before its time");

    turn(&mut ada, ten);
    assert_eq!(rings(&ada), 1, "at its time");
    turn(&mut ada, ten);
    assert_eq!(rings(&ada), 1, "once, not every turn");

    remind_at(&mut ada, &reminder, "2026-10-27T11:00:00Z");
    turn(&mut ada, ten + 60);
    assert_eq!(rings(&ada), 1, "the old time rang already");

    turn(&mut ada, eleven);
    assert_eq!(rings(&ada), 2, "the new time comes due");
}

#[test]
fn a_run_missed_while_every_device_was_off_runs_once_when_one_comes_up() {
    let (mut ada, board, reminder) = workspace();
    let ten = tilolang::calendar::instant("2026-10-27T10:00:00Z").unwrap();
    remind_at(&mut ada, &reminder, "2026-10-27T10:00:00Z");

    // Nothing ran for five hours: the board's half hours and the reminder's time all passed.
    let five_past = ten + 5 * 60 * 60;
    assert_eq!(turn(&mut ada, five_past), 2, "each trigger runs once");
    assert_eq!(counted(&ada, &board, "Board", "ticks"), 1, "one, not ten");
    assert_eq!(counted(&ada, &reminder, "Reminder", "rings"), 1);

    assert_eq!(turn(&mut ada, five_past + 1), 0, "and nothing again");
    assert_eq!(counted(&ada, &board, "Board", "ticks"), 1);
    assert_eq!(counted(&ada, &reminder, "Reminder", "rings"), 1);
}

#[test]
fn two_devices_run_a_trigger_once() {
    let (mut ada, board, _) = workspace();
    let now = 1_790_000_400 / PERIOD * PERIOD;
    let mut bo = ada.fork("bo", &packages());

    assert_eq!(ada.host.run_triggers(now), 1, "the origin runs it");
    let done = ada.host.take_committed();
    assert_eq!(done.len(), 1, "one change: {done:#?}");
    // The app acting by itself, not the person at the window.
    assert_eq!(done[0].action.authorship.invoker, "trigger:t.trig");
    for change in done {
        ada.apply(&change.action);
    }

    bo.sync_with(&mut ada);
    assert_eq!(turn(&mut bo, now), 0, "the other device finds it ran");
    assert_eq!(counted(&bo, &board, "Board", "ticks"), 1);
    assert_eq!(counted(&ada, &board, "Board", "ticks"), 1);
}

/// What a window's instance asks the platform: the package, the clock, and the words.
struct Trig {
    now: RefCell<String>,
}

impl Platform for Trig {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SOURCE, ids()).unwrap();
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        self.now.borrow().clone()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

/// The names a window without a roster shows.
struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Principal) -> String {
        who.0.clone()
    }
}

/// A window of the app on a record of its own: no broker, so nothing of it is sent anywhere.
fn window(now: &str, origin: bool) -> (Instance, AutoCommit) {
    let doc = AutoCommit::new();
    let (app_end, _window) = pair();
    let platform = Rc::new(Trig {
        now: RefCell::new(now.to_owned()),
    });
    let mut instance = Instance::new(
        Config {
            app: APP.to_owned(),
            me: "ada".to_owned(),
            mode: Mode::Live,
            device: None,
            index_dir: None,
            record_loader: None,
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
        },
        platform,
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.set_origin(origin);
    (instance, doc)
}

#[test]
fn only_the_origin_runs_triggers_from_its_frame() {
    let noon = "2026-10-27T12:00:00Z";

    let (mut origin, doc) = window(noon, true);
    origin.sync(Some(&doc));
    origin.sync(Some(&doc));
    let ran: Vec<String> = origin
        .host()
        .unwrap()
        .take_committed()
        .into_iter()
        .map(|done| done.action.authorship.invoker)
        .filter(|invoker| invoker.starts_with("trigger:"))
        .collect();
    assert_eq!(ran, vec!["trigger:t.trig".to_owned()]);

    let (mut reader, doc) = window(noon, false);
    reader.sync(Some(&doc));
    reader.sync(Some(&doc));
    assert!(
        reader
            .host()
            .unwrap()
            .take_committed()
            .iter()
            .all(|done| !done.action.authorship.invoker.starts_with("trigger:")),
        "a replica that does not govern the workspace runs none"
    );
}
