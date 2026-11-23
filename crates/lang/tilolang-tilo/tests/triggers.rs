//! Triggers (`tilolang_tilo::triggers`): an action a package declares runs by itself, every so
//! often or at the time a document's field names, for each of its documents of a type, and exactly
//! once across the workspace's devices.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use crate::fixtures::Workspace;
use tilo_render_protocol::patch::local::pair;
use tilolang_runtime::meta::Meta;
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Actor, InvokeOutcome};
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::resources::Turn;
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

/// Rings the rack's newest bell once for each bell the rack holds.
action ring_newest(r: Rack) {
    if let b = r.bells.last() { b.chimes += r.bells.count() }
}

/// Adds a bell due at `at`.
action add_bell(r: Rack, at: Option<Time>) { r.bells.insert(Bell(at: at)) }

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

/// The value the counter field `field` of `at` holds.
fn counted_at(replica: &Replica, at: &ResourceRef, field: &str) -> i64 {
    match replica.host.runtime().read(at, field) {
        Ok(Value::Int(value)) => value,
        other => panic!("not a counter: {other:?}"),
    }
}

/// The value of the counter field `field` on the type `ty` of the root `doc`.
fn counted(replica: &Replica, doc: &str, ty: &str, field: &str) -> i64 {
    let at = ResourceRef::root(doc.to_owned(), type_id(replica, ty));
    counted_at(replica, &at, field)
}

/// A replica with the package loaded, its board root made, and a reminder root made.
fn workspace() -> (Replica, String, String) {
    let (ada, _) = Replica::new("ada", Workspace::new(), &packages());
    let board = ada.host.root_of(APP).unwrap().doc;
    let reminder = reminder_root(&ada);
    ada.commit();
    (ada, board, reminder)
}

/// The id of the reminder root, made if it is not there.
fn reminder_root(replica: &Replica) -> String {
    let ty = type_id(replica, "Reminder");
    replica.host.root_resource(APP, &ty).unwrap().doc
}

/// Run the triggers and commit what they did, as a window of the app does each turn.
fn turn(replica: &Replica, now: i64) -> usize {
    let ran = replica.host.run_triggers(now);
    replica.commit();
    ran
}

/// Invoke the action `name` of the package on `args`, as a person at the window, and commit.
fn run(replica: &Replica, name: &str, args: Vec<Value>) {
    let id = action_id(&meta(replica), name);
    let done = replica
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(&replica.name)),
            (id.as_str(), args),
        )
        .unwrap();
    assert!(matches!(done, InvokeOutcome::Done(_)), "{name}: {done:?}");
    replica.commit();
}

/// Set the reminder `doc`'s time to `at`, as a person at the window.
fn remind_at(replica: &Replica, doc: &str, at: &str) {
    let target = ResourceRef::root(doc.to_owned(), type_id(replica, "Reminder"));
    run(
        replica,
        "remind",
        vec![Value::resource(&target), Value::text(at)],
    );
}

/// The start of the half hour a `now` is in, and the half hour before it.
const PERIOD: i64 = 30 * 60;

tilo_test_env::scenario! {
    fn a_trigger_runs_once_when_it_is_due;

    phase a_schedule_runs_once_a_period;
    {
        let (ada, board, _) = workspace();
        let noon = 1_790_000_400 / PERIOD * PERIOD;

        assert_eq!(turn(&ada, noon), 1, "the half hour is due");
        assert_eq!(counted(&ada, &board, "Board", "ticks"), 1);

        assert_eq!(turn(&ada, noon + 60), 0, "not twice in one half hour");
        assert_eq!(counted(&ada, &board, "Board", "ticks"), 1);

        assert_eq!(turn(&ada, noon + PERIOD), 1, "the next half hour");
        assert_eq!(counted(&ada, &board, "Board", "ticks"), 2);
    }

    phase a_trigger_at_a_documents_time_fires_and_rearms_when_it_changes;
    {
        let (ada, _, reminder) = workspace();
        let ten = tilolang::calendar::instant("2026-10-27T10:00:00Z").unwrap();
        let eleven = tilolang::calendar::instant("2026-10-27T11:00:00Z").unwrap();
        let rings = |ada: &Replica| counted(ada, &reminder, "Reminder", "rings");

        remind_at(&ada, &reminder, "2026-10-27T10:00:00Z");
        turn(&ada, ten - 1);
        assert_eq!(rings(&ada), 0, "not before its time");

        turn(&ada, ten);
        assert_eq!(rings(&ada), 1, "at its time");
        turn(&ada, ten);
        assert_eq!(rings(&ada), 1, "once, not every turn");

        remind_at(&ada, &reminder, "2026-10-27T11:00:00Z");
        turn(&ada, ten + 60);
        assert_eq!(rings(&ada), 1, "the old time rang already");

        turn(&ada, eleven);
        assert_eq!(rings(&ada), 2, "the new time comes due");
    }

    phase a_run_missed_while_every_device_was_off_runs_once_when_one_comes_up;
    {
        let (ada, board, reminder) = workspace();
        let ten = tilolang::calendar::instant("2026-10-27T10:00:00Z").unwrap();
        remind_at(&ada, &reminder, "2026-10-27T10:00:00Z");

        // Nothing ran for five hours: the board's half hours and the reminder's time all passed.
        let five_past = ten + 5 * 60 * 60;
        assert_eq!(turn(&ada, five_past), 2, "each trigger runs once");
        assert_eq!(counted(&ada, &board, "Board", "ticks"), 1, "one, not ten");
        assert_eq!(counted(&ada, &reminder, "Reminder", "rings"), 1);

        assert_eq!(turn(&ada, five_past + 1), 0, "and nothing again");
        assert_eq!(counted(&ada, &board, "Board", "ticks"), 1);
        assert_eq!(counted(&ada, &reminder, "Reminder", "rings"), 1);
    }

    phase a_wake_comes_at_the_next_instant_a_trigger_is_due;
    {
        let (ada, _, reminder) = workspace();
        let ten = tilolang::calendar::instant("2026-10-27T10:07:30Z").unwrap();
        let now = ten - 60;

        // No reminder time to come first: the board's next half hour.
        let next = ada.host.next_trigger(now).unwrap();
        assert!(next > now && next <= now + PERIOD, "{next} after {now}");
        assert_eq!(next % PERIOD, 0, "a period's start: {next}");

        // A time still to come comes first.
        remind_at(&ada, &reminder, "2026-10-27T10:07:30Z");
        assert_eq!(ada.host.next_trigger(now), Some(ten), "the reminder's time");

        // Once it rang, the board's half hour is what wakes the app again.
        turn(&ada, ten);
        let next = ada.host.next_trigger(ten).unwrap();
        assert!(next > ten && next <= ten + PERIOD, "{next} after {ten}");
        assert_eq!(next % PERIOD, 0);
    }

    phase two_devices_run_a_trigger_once;
    {
        let (ada, board, _) = workspace();
        let now = 1_790_000_400 / PERIOD * PERIOD;
        let bo = ada.fork("bo", &packages());
        let at = ResourceRef::root(board.clone(), type_id(&ada, "Board"));

        assert_eq!(ada.host.run_triggers(now), 1, "the first device runs it");
        ada.commit();
        let tick = action_id(&meta(&ada), "tick");
        // Its run is one change of the board, after the one that made it.
        let versions: Vec<_> = ada
            .versions(&at)
            .into_iter()
            .filter(|version| version.kind != tilolang_runtime::store::VersionKind::Created)
            .collect();
        assert_eq!(versions.len(), 1, "one change: {versions:#?}");
        // The app acting by itself, not the person at the window.
        assert_eq!(
            versions[0].who,
            Some(tilolang_tilo::runtime::Actor::App("t.trig".into()))
        );
        assert_eq!(versions[0].action, tick);

        bo.sync_with(&ada);
        assert_eq!(turn(&bo, now), 0, "the other device finds it ran");
        assert_eq!(counted(&bo, &board, "Board", "ticks"), 1);
        assert_eq!(counted(&ada, &board, "Board", "ticks"), 1);
    }

    phase a_window_runs_a_trigger_only_when_its_device_takes_the_turn;
    {
        let noon = "2026-10-27T12:00:00Z";

        let (mut origin, board) = window(noon, Turn::Take);
        origin.sync();
        origin.sync();
        let at = ResourceRef::root(board, type_id_of(&origin, "Board"));
        let tick = action_id(&origin.host().unwrap().package(APP).unwrap().meta, "tick");
        let versions = origin.host().unwrap().store().versions(&at).unwrap();
        assert_eq!(
            versions.iter().filter(|version| version.action == tick).count(),
            1,
            "the device whose turn it is runs the trigger: {versions:#?}"
        );
        assert_eq!(
            versions
                .iter()
                .find(|version| version.action == tick)
                .and_then(|version| version.who.clone()),
            Some(tilolang_tilo::runtime::Actor::App("t.trig".into()))
        );

        let (mut reader, board) = window(noon, Turn::Later);
        reader.sync();
        reader.sync();
        let at = ResourceRef::root(board, type_id_of(&reader, "Board"));
        let versions = reader.host().unwrap().store().versions(&at).unwrap();
        assert!(
            versions.iter().all(|version| version.action != tick),
            "a device whose turn it is not runs none: {versions:#?}"
        );
        assert!(
            reader.wakes_in().is_some_and(|wait| wait.as_secs() <= 5),
            "and asks again a little later"
        );

        let (mut late, board) = window(noon, Turn::Done(serde_json::Value::Null));
        late.sync();
        let at = ResourceRef::root(board, type_id_of(&late, "Board"));
        let versions = late.host().unwrap().store().versions(&at).unwrap();
        assert!(
            versions.iter().all(|version| version.action != tick),
            "nor one that finds it ran on another device: {versions:#?}"
        );
    }

    phase frames_over_a_store_that_did_not_move_take_nothing_in;
    {
        // Frames over a store that did not move read nothing new: the window's view draws again
        // without the store moving.
        let (mut window, _board) = window("2026-10-27T12:00:00Z", Turn::Take);
        window.sync();
        let watch = window.host().expect("the window opened").store().watch();
        let _ = watch.take();
        for _ in 0..10 {
            window.sync();
        }
        assert!(watch.take().is_empty(), "an idle window saw nothing move");
    }
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
    fn name(&self, who: &Actor) -> String {
        who.id().to_owned()
    }
}

/// A window of the app over a store of its own: no broker, so nothing of it is sent anywhere.
/// A store whose device's turn to run an action that runs once is always `turn`.
struct Turns {
    store: tilolang_tilo::resources::LocalPort,
    turn: Turn,
}

impl tilolang_tilo::resources::Port for Turns {
    fn command(
        &self,
        command: tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        match command {
            tilo_resource_store::wire::Command::Turn { answer: None, .. } => {
                Ok(tilo_resource_store::wire::Reply::Turn(self.turn.clone()))
            }
            other => self.store.command(other),
        }
    }
}

/// A window of the board app, its clock at `now`, on a device whose turn to run a trigger is
/// `turn`.
fn window(now: &str, turn: Turn) -> (Instance, String) {
    let (app_end, _window) = pair();
    let platform = Rc::new(Trig {
        now: RefCell::new(now.to_owned()),
    });
    let instance = Instance::new(
        Config {
            app: APP.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: tilolang_tilo::resources::ResourceStore::new(Rc::new(Turns {
                store: tilolang_tilo::resources::LocalPort::in_memory(),
                turn,
            })),
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        platform,
        Rc::new(Alone),
        Box::new(app_end),
    );
    (instance, "root-".to_owned() + &type_id_board())
}

/// The type id the package gives its board.
fn type_id_board() -> String {
    let compiled =
        tilolang::compile_package(SOURCE, &tilolang::IdsLock::parse(ids()).unwrap(), &[])
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    compiled
        .meta
        .types
        .iter()
        .find(|ty| ty.name == "Board")
        .unwrap()
        .id
        .clone()
}

/// The type id the package `host` runs gives its type `ty`.
fn type_id_of(instance: &Instance, ty: &str) -> String {
    let meta = &instance
        .host()
        .expect("the window opened")
        .package(APP)
        .unwrap()
        .meta;
    meta.types
        .iter()
        .find(|declared| declared.name == ty)
        .unwrap()
        .id
        .clone()
}
