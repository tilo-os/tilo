//! A reminder reaches the person from their session, with no window of its app open anywhere,
//! and one of their devices tells them of it once.
//!
//! The session runs the scheduler for every tilolang app installed in each workspace the person
//! is in (`crate::reminders`), over the same records and the same marks a window of the app
//! reads, so a reminder arrives while only the shell is up. What it finds becomes an ordinary
//! notification of the session's: a click opens the item in its app, the one-step actions its
//! type declares are the notice's buttons and run through the app's door, and the mark kept in
//! the workspace's record of the person's own state keeps their other devices from telling them
//! of the same reminder again.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use app_state::{DocStore, FsStore};
use automerge::AutoCommit;
use durable_store::{DurableStore, StorageKey};
use tilo_actions::remote::RemoteActionTarget;
use tilo_actions::{ActionContext, ActionInvoker, DoorError, apply_local_with_context};
use tilo_clock::testing::{OwnProcess, hold_wall_everywhere};
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{TileContent, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_desktop_surface::notifications::{BannerAsk, Notification};
use tilo_resource::{Doc, Resource as _};
use tilo_session::{AppActions, Options, Session};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::personal::{self, Personal};
use tilolang_tilo::runtime::{Principal, Value};
use ui::record::RecordId;

type Arguments = serde_json::Map<String, serde_json::Value>;

/// The package the test person's reminders are in.
const APP: &str = "t.remind";

/// The workspace the desk shows, the id it has on every device.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// 2026-10-27T09:00:00Z: when the person's session starts.
const MORNING: u64 = 1_793_091_600;

/// 2026-10-27T09:30:00Z: the instant the reminder of a task due at ten comes.
const HALF_PAST_NINE: u64 = 1_793_093_400;

/// The person the reminders are for, in the hex everyone here knows them by.
fn person() -> String {
    "ab".repeat(32)
}

/// The wall clock at `epoch`, the instant a session reads.
fn instant(epoch: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(epoch)
}

/// A task the package holds, as the person made it: what it is called, the instant it is due at,
/// and how long before that they are reminded.
struct Task {
    title: &'static str,
    at: &'static str,
    remind: &'static str,
}

/// The reminders the two proofs make: one due at ten, an hour into the person's morning.
const CALL_THE_LANDLORD: Task = Task {
    title: "Call the landlord",
    at: "2026-10-27T10:00:00Z",
    remind: "-PT30M",
};

/// The package the test app is: `tests/remind`, compiled the way `tilolang build` does.
fn bundle() -> &'static Bundle {
    static BUNDLE: OnceLock<Bundle> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/remind");
        let dirs = tilolang_tilo::tilolang::source::dependency_order(&dir, &[])
            .expect("the test package resolves");
        let built = tilolang_tilo::tilolang::source::compile_dirs(
            &dirs,
            &[],
            &mut tilolang_tilo::tilolang::OsEntropy::default(),
        )
        .expect("the test package compiles");
        let (package, used) = built.split_last().expect("the package is built last");
        Bundle::new(&package.compiled, used.iter().map(|used| &used.compiled))
    })
}

/// Install the package in this test's app store, as the launcher's landing leaves it: the
/// payload the session reads, the manifest that names it, and the registry row that lists it.
fn install(home: &mut tilo_test_env::TestHome) {
    let store = home.data().join("tilo-apps");
    let payload = store.join("payload").join(APP);
    std::fs::create_dir_all(payload.join("bin")).expect("the app's payload");
    bundle().write(&payload).expect("the app's package");
    let manifest = payload.join("manifest.toml");
    std::fs::write(
        &manifest,
        format!(
            "[package]\n\
             id = \"{APP}\"\n\
             version = \"1.0.0\"\n\
             kind = \"app\"\n\
             name = \"Remind\"\n\
             \n\
             [publisher]\n\
             identity = \"ed25519:{key}\"\n\
             signature = \"\"\n\
             \n\
             [sandbox]\n\
             namespaces = [\"user\"]\n\
             seccomp = \"tilo-default\"\n\
             landlock = \"strict\"\n\
             \n\
             [sandbox.net]\n\
             mode = \"off\"\n\
             \n\
             [entrypoint]\n\
             bin = \"bin/app\"\n\
             type = \"gui\"\n",
            key = "11".repeat(32),
        ),
    )
    .expect("the app's manifest");
    std::fs::write(
        store.join("registry.toml"),
        format!(
            "[[entry]]\nid = \"{APP}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .expect("the store's registry");
    let system = home.data().join("system-apps");
    std::fs::create_dir_all(&system).expect("an empty system store");
    home.env().set("TILO_APPS_DATA_DIR", &store);
    home.env().set("TILO_APPS_SYSTEM_DIR", &system);
}

/// This device's records, read and written as every part of the session reads them.
fn store(data: &Path) -> Rc<dyn DurableStore> {
    Rc::new(FsStore::new(data))
}

/// The workspace's record of each person's own state, as the apps keep it.
fn personal_key() -> StorageKey {
    let id = collab_types::workspace_record_id(personal::KIND);
    let record = RecordId::parse(&id)
        .expect("the personal record's id")
        .storage_key();
    StorageKey::record(personal::OWNER, WORKSPACE, &record)
}

/// What the workspace's record of the person's own state says they were told of, by the reminder
/// each mark names.
fn told(store: &Rc<dyn DurableStore>) -> Vec<String> {
    let Some(stored) = store.read(&personal_key()).expect("the record is read") else {
        return Vec::new();
    };
    let doc = AutoCommit::load(&stored.bytes).expect("the record loads");
    Personal::read(&doc)
        .expect("it is the record of a person's own state")
        .of(&person())
        .told
        .into_iter()
        .map(|told| told.reminder)
        .collect()
}

/// `record` with each of `tasks` made in it, through the app's own action as its window makes
/// them.
fn make_tasks(record: &mut Doc, tasks: &[&Task]) {
    let me = person();
    let host = Host::new(record, Principal::new(&me), || None).expect("a host over the record");
    host.load_bundle(bundle()).expect("the app loads");
    host.root_of(APP).expect("the app has a root");
    apply_committed(record, &host);
    let new_task = bundle()
        .meta
        .actions
        .iter()
        .find(|action| action.name == "new_task")
        .expect("the app declares how a task is made")
        .id
        .clone();
    for task in tasks {
        let root = Value::resource(&host.root_of(APP).expect("the app has a root"));
        host.runtime()
            .invoke_action(
                &Principal::new(&me),
                (
                    new_task.as_str(),
                    vec![
                        root,
                        Value::text(task.title),
                        Value::text("2026-10-27"),
                        Value::some(Value::text(task.at)),
                        Value::some(Value::Principal(Principal::new(&me))),
                        Value::text(task.remind),
                    ],
                ),
            )
            .expect("the task is made");
        apply_committed(record, &host);
    }
}

/// Apply what the runtime committed on its own, as a window does: the change is recorded as the
/// person's step in the record.
fn apply_committed(record: &mut Doc, host: &Host) {
    let me = person();
    for committed in host.take_committed() {
        let action = &committed.action;
        let context = ActionContext::for_resource("workspace", &action.kind, &action.kind)
            .with_app("so.tilo.lang", "tilolang")
            .with_invoker(ActionInvoker::actor(me.clone()));
        let applied = host.with_schemas(|| {
            apply_local_with_context(
                record,
                action,
                action.encode().expect("the action encodes"),
                Some(me.as_bytes()),
                context,
            )
        });
        record.commit();
        host.sync_from(record);
        applied.expect("a committed change applies");
    }
}

/// The workspace's record of the app, as this device keeps it.
fn app_record(store: &Rc<dyn DurableStore>) -> Vec<u8> {
    store
        .read(&StorageKey::workspace_index(APP, WORKSPACE))
        .expect("the record is read")
        .expect("the record is kept")
        .bytes
}

/// Keep `record` as the app's record of the workspace on this device.
fn keep(store: &Rc<dyn DurableStore>, record: &mut Doc) {
    let mut kept = DocStore::shared(
        Rc::clone(store),
        StorageKey::workspace_index(APP, WORKSPACE),
    )
    .expect("the app's record is joined");
    kept.save(record).expect("the record is kept");
}

/// Put a workspace for [`WORKSPACE`] on the session's desk: the workspace this person is in, and
/// no window of any app in it.
fn show(session: &Session) {
    let id = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let mut tree = session.handle().read(Clone::clone);
    tree.workspaces.retain(|held| held.id() != id);
    tree.workspaces.push(Workspace::in_workspace(
        WorkspaceSlot::of_workspace(id),
        id,
        "Desk",
    ));
    session.handle().replace_tree(tree);
}

/// Whether the session has a window of the app `app` open anywhere.
fn open_anywhere(session: &Session, app: &str) -> bool {
    session.handle().read(|tree| {
        tree.workspaces.iter().any(|held| {
            held.strips.iter().any(|strip| {
                strip.window_groups.iter().any(|group| {
                    group.tabs.iter().any(|tab| {
                        matches!(&tab.tile, TileContent::LocalApp { app_id, .. } if app_id == app)
                    })
                })
            })
        })
    })
}

/// A headless session on this device, telling `apps` what the person asks of their apps.
fn session_with(apps: Rc<dyn AppActions>) -> Session {
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: person(),
            collab: false,
            apps: Some(apps),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    show(&session);
    session
}

/// What the session told the person, as the desk's notifications stand.
fn notices(session: &Session) -> Vec<Notification> {
    session.notifications().log().active().to_vec()
}

/// The person's second device, brought up on what the first left in the workspace: its app
/// record (`record`) as the first saved it, with `also` made there, and the marks of their own
/// state (`marks`) as the first kept them — what their other device reads of both over the
/// workspace.
fn second_device(
    record: &[u8],
    marks: &[u8],
    also: &Task,
) -> (
    tilo_test_env::TestHome,
    Rc<dyn DurableStore>,
    Rc<HostApps>,
    Session,
) {
    let mut record = AutoCommit::load(record).expect("the record loads");
    make_tasks(&mut record, &[also]);
    let mut home = tilo_test_env::TestHome::new().expect("their second device");
    install(&mut home);
    let store = store(home.data());
    keep(&store, &mut record);
    let mut marks = AutoCommit::load(marks).expect("the marks load");
    DocStore::shared(Rc::clone(&store), personal_key())
        .expect("the record of their own state is joined")
        .save(&mut marks)
        .expect("the marks are kept");
    let apps = Rc::new(HostApps::default());
    let host: Rc<dyn AppActions> = apps.clone();
    let session = session_with(host);
    (home, store, apps, session)
}

/// `session`, stepped until `probe` says what it was waiting for.
#[track_caller]
fn wait_for<T>(
    session: &mut Session,
    what: &str,
    mut probe: impl FnMut(&mut Session) -> Option<T>,
) -> T {
    let until = Instant::now() + tilo_test_env::hang_bound();
    loop {
        if let Some(answer) = probe(session) {
            return answer;
        }
        assert!(Instant::now() < until, "gave up waiting for {what}");
        session.step(Duration::from_millis(2)).expect("a step");
    }
}

/// An action host that keeps what it was asked to run, and starts nothing.
#[derive(Default)]
struct HostApps {
    invoked: RefCell<Vec<(RemoteActionTarget, Arguments)>>,
}

impl HostApps {
    fn invoked(&self) -> Vec<(RemoteActionTarget, Arguments)> {
        self.invoked.borrow().clone()
    }
}

impl AppActions for HostApps {
    fn published(&self, _: &str) -> Vec<PublishedActions> {
        Vec::new()
    }

    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }

    fn invoke(&self, target: RemoteActionTarget, arguments: Arguments) -> Result<(), DoorError> {
        self.invoked.borrow_mut().push((target, arguments));
        Ok(())
    }

    fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), DoorError> {
        Ok(())
    }

    fn open_uri(&self, _: &str, _: &str) -> Result<(), DoorError> {
        Ok(())
    }

    fn start_host_app(&self, _: &str) -> Result<(), DoorError> {
        Ok(())
    }
}

#[test]
fn a_reminder_reaches_the_person_from_their_session_with_no_window_of_the_app_open() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_reminder_reaches_the_person_from_their_session_with_no_window_of_the_app_open",
        || {
            let _morning =
                hold_wall_everywhere(OwnProcess::of_a_test_in_its_own_process(), instant(MORNING));
            let mut home = tilo_test_env::TestHome::new().expect("a home");
            install(&mut home);
            let store = store(home.data());
            let mut record = Doc::default();
            make_tasks(&mut record, &[&CALL_THE_LANDLORD]);
            keep(&store, &mut record);

            let apps = Rc::new(HostApps::default());
            let action_host: Rc<dyn AppActions> = apps.clone();
            let mut session = session_with(action_host);
            assert!(
                !open_anywhere(&session, APP),
                "nothing of the app is open: only the shell is up"
            );
            // Half an hour before its instant, the session says nothing of it.
            for _ in 0..8 {
                session.step(Duration::from_millis(2)).expect("a step");
            }
            assert!(
                notices(&session).is_empty(),
                "half an hour early: {:?}",
                notices(&session)
            );

            // At the instant, it comes, from the session: no window of the app was ever open.
            let _half_past_nine = hold_wall_everywhere(
                OwnProcess::of_a_test_in_its_own_process(),
                instant(HALF_PAST_NINE),
            );
            let notice = wait_for(
                &mut session,
                "the reminder to reach the person",
                |session| notices(session).into_iter().next(),
            );
            assert_eq!(notice.title, "Call the landlord");
            assert_eq!(
                notice.body.lines().last(),
                Some("Remind"),
                "from the app: {:?}",
                notice.body
            );
            assert_eq!(
                notice.app_name.as_deref(),
                Some("Remind"),
                "the notice says which app it is from"
            );
            let offered: Vec<&str> = notice
                .actions
                .iter()
                .map(|(_, label)| label.as_str())
                .collect();
            assert_eq!(
                offered,
                vec!["Open", "Done"],
                "it opens the item, and offers the one step its type declares"
            );

            // Told once, however many frames pass while the reminder still stands.
            for _ in 0..16 {
                session.step(Duration::from_millis(2)).expect("a step");
            }
            assert_eq!(
                notices(&session).len(),
                1,
                "told once: {:?}",
                notices(&session)
            );
            // The mark is kept in the workspace's record of the person's own state, which their
            // other devices read: the reminder's record and instant, and no other.
            let told = told(&store);
            assert_eq!(told.len(), 1, "one mark: {told:?}");
            assert!(
                told[0].ends_with("@1793093400"),
                "the instant it came, 2026-10-27T09:30:00Z: {told:?}"
            );

            // A click on the notice opens the item in its app: the session asks the action host
            // for the app's `open` at the item, which starts the app when it is closed.
            session
                .notifications()
                .answer(BannerAsk::Activate(notice.id));
            let (target, arguments) = wait_for(&mut session, "the click to reach the app", |_| {
                apps.invoked().into_iter().next()
            });
            assert_eq!(target.provider, format!("application:{WORKSPACE}:{APP}"));
            assert_eq!(target.address.action.as_str(), "open");
            assert!(
                target.address.resource.as_str().starts_with("tl:t.remind:"),
                "the item: {}",
                target.address.resource
            );
            assert_eq!(
                arguments.get("anchor").and_then(serde_json::Value::as_str),
                Some(target.address.resource.as_str()),
                "the anchor is the item the notice is about"
            );
        },
    );
}

#[test]
fn one_reminder_tells_one_person_once_across_their_devices() {
    tilo_test_env::in_own_process(
        module_path!(),
        "one_reminder_tells_one_person_once_across_their_devices",
        || {
            let _morning =
                hold_wall_everywhere(OwnProcess::of_a_test_in_its_own_process(), instant(MORNING));
            // The person's first device, with their one task, due at ten.
            let mut first_home = tilo_test_env::TestHome::new().expect("a home");
            install(&mut first_home);
            let first_store = store(first_home.data());
            let mut record = Doc::default();
            make_tasks(&mut record, &[&CALL_THE_LANDLORD]);
            keep(&first_store, &mut record);
            let mut first = session_with(Rc::new(HostApps::default()));
            // At half past nine the first device tells them, and marks it in the record both
            // devices read.
            let _half_past_nine = hold_wall_everywhere(
                OwnProcess::of_a_test_in_its_own_process(),
                instant(HALF_PAST_NINE),
            );
            let notice = wait_for(&mut first, "the first device to tell them", |session| {
                notices(session).into_iter().next()
            });
            assert_eq!(notice.title, "Call the landlord");
            assert_eq!(
                told(&first_store).len(),
                1,
                "kept before the notice goes out"
            );

            // Their second device comes up a minute later, with the records as the first left
            // them and a second task whose reminder comes now. What it reads of their marks is
            // what any change of theirs arriving over the workspace brings here.
            let now_reminded = Task {
                title: "Post the letter",
                at: "2026-10-27T10:01:00Z",
                remind: "-PT30M",
            };
            let record = app_record(&first_store);
            let marks = first_store
                .read(&personal_key())
                .expect("the record is read")
                .expect("the mark is kept")
                .bytes;
            drop(first);
            drop(first_home);
            let (_second_home, second_store, apps, mut second) =
                second_device(&record, &marks, &now_reminded);
            let _a_minute_later = hold_wall_everywhere(
                OwnProcess::of_a_test_in_its_own_process(),
                instant(HALF_PAST_NINE + 60),
            );

            // The reminder they were told of on the other device is not told again; the one
            // that comes now is.
            let notice = wait_for(&mut second, "the second device to tell them", |session| {
                notices(session).into_iter().next()
            });
            assert_eq!(
                notice.title, "Post the letter",
                "the reminder they were told of is not told again"
            );
            for _ in 0..16 {
                second.step(Duration::from_millis(2)).expect("a step");
            }
            let standing: Vec<String> = notices(&second)
                .into_iter()
                .map(|notice| notice.title)
                .collect();
            assert_eq!(
                standing,
                vec!["Post the letter".to_owned()],
                "one notice for the one reminder that came here"
            );
            let told = told(&second_store);
            assert_eq!(told.len(), 2, "a mark for each: {told:?}");
            assert!(
                told.iter().any(|mark| mark.ends_with("@1793093400")),
                "the reminder the first device told: {told:?}"
            );
            assert!(
                told.iter().any(|mark| mark.ends_with("@1793093460")),
                "the reminder this device told: {told:?}"
            );
            // The action the notice offers runs on the item, through the app's door, as any
            // action of the app is run.
            let done = done_label(&notice);
            press_the_types_action(&mut second, &notice, &apps, &done);
        },
    );
}

/// The button `notice` offers, by its label.
fn done_label(notice: &Notification) -> String {
    notice
        .actions
        .iter()
        .find(|(_, label)| label == "Done")
        .expect("the type's one-step action is a button of the notice")
        .0
        .clone()
}

/// Press `done`, the notice's one-step action: it runs on the item, through the app's door, as
/// any action of the app is run.
fn press_the_types_action(
    session: &mut Session,
    notice: &Notification,
    apps: &Rc<HostApps>,
    done: &str,
) {
    session
        .notifications()
        .answer(BannerAsk::Invoke(notice.id, done.to_owned()));
    let (target, arguments) = wait_for(session, "the button to reach the app", |_| {
        apps.invoked().into_iter().next()
    });
    assert_eq!(target.provider, format!("application:{WORKSPACE}:{APP}"));
    assert_eq!(target.address.action, done);
    assert!(
        target.address.resource.as_str().starts_with("tl:t.remind:"),
        "run on the item: {}",
        target.address.resource
    );
    assert!(
        arguments.is_empty(),
        "an action that asks nothing: {arguments:?}"
    );
    for _ in 0..8 {
        session.step(Duration::from_millis(2)).expect("a step");
    }
    assert!(apps.invoked().len() == 1, "run once: {:?}", apps.invoked());
}
