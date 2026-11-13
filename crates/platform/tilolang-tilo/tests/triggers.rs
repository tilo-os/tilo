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

use crate::fixtures::{Replica, action_id};

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

/// A bell that rings at a time, held in a rack: a trigger runs for the items of a type wherever
/// they are, so what a large collection reads is the same as a small one.
type Bell {
    at: Option<Time> = .none
    chimes: Int merge counter
}

type Rack {
    bells: Collection<Bell>
}

action tick(b: Board) { b.ticks += 1 }

action chime(bell: Bell) { bell.chimes += 1 }

action ring(r: Reminder) { r.rings += 1 }

action remind(r: Reminder, at: Time) { r.at = .some(at) }

trigger often(b: Board) every 30m: tick(b)

trigger due(r: Reminder) at r.at: ring(r)

trigger bell(b: Bell) at b.at: chime(b)

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

/// The value of the counter field `field` on the type `ty` of the record `doc`.
fn counted(replica: &Replica, doc: &str, ty: &str, field: &str) -> i64 {
    let id = crate::fixtures::field_id(&meta(replica), ty, field);
    match replica.host.store().record(doc).unwrap().get(&id) {
        Some(tilolang_tilo::record::DynValue::Counter(counter)) => counter.get(),
        other => panic!("not a counter: {other:?}"),
    }
}

/// How many times the rack's `i`th bell has rung.
fn chimes(replica: &Replica, i: usize) -> i64 {
    use tilolang_runtime::meta::FieldKind;
    use tilolang_runtime::store::{FieldSpec, FieldValue, Store as _};

    let spec = FieldSpec {
        id: crate::fixtures::field_id(&meta(replica), "Bell", "chimes"),
        kind: FieldKind::Counter,
    };
    let at = ResourceRef::root("rack".to_owned(), type_id(replica, "Rack"))
        .item(&format!("bell-{i:05}"), type_id(replica, "Bell"));
    match replica.host.store().read(&at, &spec).unwrap() {
        FieldValue::Counter(rung) => rung,
        other => panic!("a bell is {other:?}"),
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

/// The id of a rack holding `n` bells, its `first` bell due at `now` and every other an hour or
/// more later, all made in one change as a workspace is seeded.
fn rack(replica: &mut Replica, n: usize, first: usize, now: i64) -> String {
    use serde_json::json;
    use tilo_resource::{Collection, Id, Model as _};
    use tilolang_tilo::action::{DynAction, DynEdit};
    use tilolang_tilo::record::DynRecord;

    let meta = meta(replica);
    let rack_ty = meta.types.iter().find(|t| t.name == "Rack").unwrap();
    let bell_ty = meta.types.iter().find(|t| t.name == "Bell").unwrap();
    let schema = tilolang_tilo::schema::lookup(&bell_ty.id).unwrap();
    let mut bells = Collection::default();
    for i in 0..n {
        let mut bell = DynRecord::empty(schema).unwrap();
        let at = if i == first {
            now
        } else {
            // An hour later, and each the minute after the one before it.
            now.saturating_add(3600)
                .saturating_add(i64::try_from(i).unwrap_or(i64::MAX).saturating_mul(60))
        };
        if let Some(tilolang_tilo::record::DynValue::Register(slot)) =
            bell.get_mut(&crate::fixtures::field_id(&meta, "Bell", "at"))
        {
            *slot = json!({ "some": tilolang_tilo::host::iso_utc(at.cast_unsigned()) });
        }
        bells.push(Id::from_raw(format!("bell-{i:05}")), bell);
    }
    let mut record = DynRecord::empty(tilolang_tilo::schema::lookup(&rack_ty.id).unwrap()).unwrap();
    if let Some(tilolang_tilo::record::DynValue::Collection(slot)) =
        record.get_mut(&crate::fixtures::field_id(&meta, "Rack", "bells"))
    {
        *slot = bells;
    }
    let create = DynAction {
        app: APP.to_owned(),
        kind: "tl.t.trig.seed".to_owned(),
        action_id: "$create".to_owned(),
        args: serde_json::Value::Null,
        authorship: tilolang_tilo::Authorship::default(),
        edits: vec![DynEdit::Create {
            id: "rack".to_owned(),
            record: record.to_json(),
        }],
    };
    replica.apply(&create);
    replica.commit();
    "rack".to_owned()
}

/// Run the triggers and commit what they did, as a window of the app does each turn.
fn turn(replica: &mut Replica, now: i64) -> usize {
    let ran = replica.host.run_triggers(now);
    replica.commit();
    ran
}

/// Invoke the action `name` of the package on `args`, as a person at the window, and commit.
fn run(replica: &mut Replica, name: &str, args: Vec<Value>) {
    let id = action_id(&meta(replica), name);
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
fn a_wake_comes_at_the_next_instant_a_trigger_is_due() {
    let (mut ada, _, reminder) = workspace();
    let ten = tilolang::calendar::instant("2026-10-27T10:07:30Z").unwrap();
    let now = ten - 60;

    // No reminder time to come first: the board's next half hour.
    let next = ada.host.next_trigger(now).unwrap();
    assert!(next > now && next <= now + PERIOD, "{next} after {now}");
    assert_eq!(next % PERIOD, 0, "a period's start: {next}");

    // A time still to come comes first.
    remind_at(&mut ada, &reminder, "2026-10-27T10:07:30Z");
    assert_eq!(ada.host.next_trigger(now), Some(ten), "the reminder's time");

    // Once it rang, the board's half hour is what wakes the app again.
    turn(&mut ada, ten);
    let next = ada.host.next_trigger(ten).unwrap();
    assert!(next > ten && next <= ten + PERIOD, "{next} after {ten}");
    assert_eq!(next % PERIOD, 0);
}

#[test]
fn a_large_collection_is_read_at_its_next_instant_not_whole() {
    let now = tilolang::calendar::instant("2026-10-27T10:07:30Z").unwrap();
    let period = now.div_euclid(PERIOD) * PERIOD;

    // What one turn costs at a rack of `n` bells: the bell due at `now` rings, and nothing reads
    // the bells that are not.
    let work = |n: usize| {
        let (mut ada, _, _) = workspace();
        rack(&mut ada, n, 0, now);
        // The board's half hour ticks first, in the turn that starts its period: the turn read
        // below is the bells' alone.
        assert_eq!(turn(&mut ada, period), 1, "the board ticks, no bell is due");
        let before = ada.host.store().read_stats();
        let next = ada.host.next_trigger(now - 1);
        let ran = ada.host.run_triggers(now);
        let after = ada.host.store().read_stats();
        ada.commit();
        (
            ran,
            next,
            after.doc.entries - before.doc.entries,
            after.evaluated - before.evaluated,
            after.index.pages - before.index.pages,
            chimes(&ada, 0),
            chimes(&ada, 1),
            ada.host.next_trigger(now),
        )
    };

    let (ran, next, small_entries, small_evaluated, small_pages, first, second, after_small) =
        work(20);
    assert_eq!(next, Some(now), "the bell due is the next instant");
    assert_eq!(ran, 1, "the one bell due rings");
    assert_eq!((first, second), (1, 0), "and no other");
    assert_eq!(
        after_small,
        Some(period + PERIOD),
        "the board's next half hour, the bells' times all past it"
    );

    let (ran, next, entries, evaluated, pages, first, second, after_large) = work(10_000);
    assert_eq!(next, Some(now), "the bell due is the next instant");
    assert_eq!(ran, 1, "the one bell due rings");
    assert_eq!((first, second), (1, 0), "and no other");
    assert!(
        entries <= small_entries && entries < 64,
        "reading {entries} record entries, against {small_entries} of twenty"
    );
    assert_eq!(
        (evaluated, small_evaluated),
        (0, 0),
        "no bell checked one by one"
    );
    assert!(
        pages <= small_pages + 8 && pages < 64,
        "reading {pages} index pages, against {small_pages} of twenty"
    );
    assert_eq!(
        after_large, after_small,
        "and what it wakes for is the same"
    );
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
    // Driven by hand, this window has no frames to take in the index its load builds off
    // them: it waits for it, as a running window's frames would.
    origin.host().expect("the window opened").wait_index();
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
    reader.host().expect("the window opened").wait_index();
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
