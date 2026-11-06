//! The person's reminders, wherever their session runs: what any of the workspaces they are in
//! reminds them of, shown as the platform's notifications by the session itself.
//!
//! A type that reminds says so by its fields' roles, and [`tilolang_tilo::reminders`] reads what
//! the workspace's records hold of them for one person. This is what runs that scheduler for the
//! whole session: over every tilolang app installed in each workspace the person is in, whether
//! or not a window of it is open, so a reminder reaches them while only the shell is up. What it
//! finds becomes an ordinary notification of the session's ([`crate::notices::Notice`]): its
//! title names the item, a click opens the item in its app — which starts if it was closed —
//! and the one-step actions its type declares are the notification's buttons, run through the
//! app's door as any action of it is.
//!
//! One reminder tells one person once, on one of their devices: whichever session shows it keeps
//! a mark in the workspace's record of each person's own state ([`tilolang_tilo::personal`]),
//! and their other devices read the mark before they would show it. The mark is kept as the
//! reminder is found due, before the notice goes out, so a session that ends in between tells
//! nobody twice.
//!
//! Reading the records happens on a worker of its own, never on the loop: a record is a file and
//! a tilolang host over it, which the thread that draws frames must not wait for. The worker
//! follows the directories the records are in ([`tilo_files::native::Watcher`]) and reads again
//! when they change, and the loop asks it to read again at the instant the next reminder comes;
//! nothing polls.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use app_state::DocStore;
use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone as _, Utc};
use durable_store::{DurableStore, StorageKey};
use settings_store::system_sounds::SystemSoundEvent;
use tilo_actions::{ActionContext, ActionInvoker};
use tilo_desktop_search::{InvokeTarget, Target};
use tilo_files::native::{ChangeWatch, Watcher};
use tilo_resource::{Doc, Resource as _};
use tilolang_tilo::authority::holding_nothing;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::{Host, Opening};
use tilolang_tilo::personal::{self, Keep, Personal, PersonalState};
use tilolang_tilo::reminders::{self, Alarm};
use tilolang_tilo::runtime::Principal;
use ui::record::RecordId;

use crate::Shared;
use crate::i18n::t;
use crate::notices::Notice;
use crate::notifications::{SessionNotifications, Tell};

/// The action a notice offers that opens what it is about; a click on the notice is this one.
const OPEN: &str = "default";

/// How long past its instant a reminder still reads as coming, rather than as late: the loop
/// wakes a moment after the instant it asked for.
const GRACE_SECS: i64 = 60;

/// What the loop asks of the worker.
enum Order {
    /// Read what the person's reminders are in these workspaces, at `now`.
    Look {
        now: DateTime<Utc>,
        workspaces: Vec<String>,
    },
    /// The session is ending.
    Stop,
}

/// What the worker says to the loop.
enum Signal {
    /// A directory the worker follows changed: read again when the loop can.
    Ask,
    /// What the worker found when it was last asked.
    Reading(Reading),
}

/// What one look found.
#[derive(Clone, Debug, Default)]
struct Reading {
    /// The reminders due, each with the workspace it is in.
    due: Vec<(String, Alarm)>,
    /// The reminders the person was told of already that still stand.
    heard: Vec<(String, Alarm)>,
    /// The earliest instant later than the one asked at at which any of them comes.
    next: Option<DateTime<Utc>>,
}

/// A notice of a reminder, and what it is about.
struct Shown {
    tell: Tell,
    alarm: Alarm,
}

/// The person's reminders as the session tells them: what the worker read, and what the notices
/// the session showed are about.
pub(crate) struct Reminders {
    orders: Sender<Order>,
    /// The worker is gone — it only ends if its channel does, which is a bug — so nothing more is
    /// asked of it.
    gone: Cell<bool>,
    signals: Receiver<Signal>,
    /// The notices shown and not taken back, by workspace, app and the reminder each tells of.
    shown: RefCell<BTreeMap<(String, String, String), Shown>>,
    /// The instant the next reminder comes, as the worker last read.
    next: Cell<Option<DateTime<Utc>>>,
    /// The workspaces the worker was last told about.
    asked: RefCell<Vec<String>>,
    /// Whether the worker must read again: the workspaces moved, an instant came, or a record
    /// changed.
    owed: Cell<bool>,
    /// Whether the worker was asked and has not answered: one look at a time.
    asking: Cell<bool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Reminders {
    /// The reminders held in every workspace `root` keeps this device's records of, waking the
    /// loop through `wake` when what the worker read is ready.
    pub(crate) fn start(root: PathBuf, person: &str, wake: tilo_service::Wake) -> Self {
        let (orders, work) = mpsc::channel();
        let (signals, readings) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("tilo-reminders".into())
            .spawn({
                let person = person.to_owned();
                move || Worker::new(root, person).run(&work, &signals, wake)
            })
            .inspect_err(|error| tracing::warn!(%error, "the reminders are not followed"))
            .ok();
        Self {
            orders,
            gone: Cell::new(false),
            signals: readings,
            shown: RefCell::default(),
            next: Cell::default(),
            asked: RefCell::default(),
            owed: Cell::new(true),
            asking: Cell::new(false),
            worker,
        }
    }

    /// Take what the worker read, show what is due, take back the notices of reminders that no
    /// longer stand, and ask it to read again where it must: when the person's workspaces moved,
    /// when a record changed, and at the instant the next reminder comes.
    pub(crate) fn follow(
        &self,
        now: DateTime<Utc>,
        workspaces: &[String],
        notifications: &SessionNotifications,
    ) {
        while let Ok(signal) = self.signals.try_recv() {
            match signal {
                Signal::Ask => self.owed.set(true),
                Signal::Reading(reading) => {
                    self.asking.set(false);
                    self.apply(reading, now, notifications);
                }
            }
        }
        {
            let mut asked = self.asked.borrow_mut();
            if *asked != workspaces {
                asked.clear();
                asked.extend_from_slice(workspaces);
                self.owed.set(true);
            }
        }
        if self.next.get().is_some_and(|next| next <= now) {
            self.owed.set(true);
        }
        if !self.owed.get() || self.asking.get() || self.gone.get() {
            return;
        }
        let order = Order::Look {
            now,
            workspaces: workspaces.to_vec(),
        };
        if self.orders.send(order).is_ok() {
            self.owed.set(false);
            self.asking.set(true);
        } else {
            tracing::warn!("the worker that reads the reminders ended");
            self.gone.set(true);
        }
    }

    /// When the loop must pass next for the reminder coming soonest: the instant it comes.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        let ahead = self
            .next
            .get()?
            .timestamp_millis()
            .saturating_sub(ui::time::unix_now_ms_i64())
            .max(0);
        let ahead = u64::try_from(ahead).ok()?;
        ui::time::now().checked_add(Duration::from_millis(ahead))
    }

    /// What the notice the person acted on at `tell` is about, once it is theirs: the workspace
    /// it is in and the reminder it told of. Nothing when the session shows no such notice.
    pub(crate) fn take(&self, tell: Tell) -> Option<(String, Alarm)> {
        let mut shown = self.shown.borrow_mut();
        let key = shown
            .iter()
            .find(|(_, shown)| shown.tell == tell)
            .map(|(key, _)| key.clone())?;
        let shown = shown.remove(&key)?;
        Some((key.0, shown.alarm))
    }

    /// Show what `reading` says is due and is not shown yet, take back the notices of reminders
    /// in neither of its lists — no longer standing — and keep when the next one comes.
    fn apply(&self, reading: Reading, now: DateTime<Utc>, notifications: &SessionNotifications) {
        self.next.set(reading.next);
        let standing: BTreeSet<(String, String, String)> = reading
            .due
            .iter()
            .chain(reading.heard.iter())
            .map(|(workspace, alarm)| (workspace.clone(), alarm.app.clone(), alarm.key.clone()))
            .collect();
        let gone: Vec<(String, String, String)> = self
            .shown
            .borrow()
            .keys()
            .filter(|key| !standing.contains(*key))
            .cloned()
            .collect();
        for key in gone {
            if let Some(shown) = self.shown.borrow_mut().remove(&key) {
                notifications.withdraw_own(shown.tell);
            }
        }
        for (workspace, alarm) in reading.due {
            let key = (workspace, alarm.app.clone(), alarm.key.clone());
            if self.shown.borrow().contains_key(&key) {
                continue;
            }
            let tell = notifications.tell_own(notice(&alarm, now));
            self.shown.borrow_mut().insert(key, Shown { tell, alarm });
        }
    }
}

impl Drop for Reminders {
    fn drop(&mut self) {
        let _ = self.orders.send(Order::Stop);
        if let Some(worker) = self.worker.take() {
            // The worker finishes the look it is on at most before it ends, so a session that
            // closes neither leaves a thread reading records behind it nor loses a mark.
            let _ = worker.join();
        }
    }
}

impl Shared {
    /// Show the reminders the person's workspaces hold for them, and carry out what they asked of
    /// those the session showed: a click opens the item in its app, and a button runs the action
    /// the item's type offers, through the app's door — starting the app when it is closed.
    pub(crate) fn follow_reminders(&self) {
        let workspaces = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|held| held.id().to_string())
                .collect::<Vec<String>>()
        });
        self.reminders
            .follow(wall(), &workspaces, &self.notifications.borrow());
        for (tell, action) in self.notifications.borrow().take_answered() {
            let Some((workspace, alarm)) = self.reminders.take(tell) else {
                continue;
            };
            let target = match action.as_deref() {
                None | Some(OPEN) => {
                    InvokeTarget::open(alarm.app.clone(), workspace.clone(), alarm.resource())
                }
                Some(chosen) => InvokeTarget::action(
                    alarm.app.clone(),
                    workspace.clone(),
                    alarm.resource(),
                    chosen,
                ),
            };
            let carried =
                self.search
                    .perform(&workspace, Target::Invoke(Box::new(target)), &mut |own| {
                        self.doors
                            .invoke(
                                &own.address,
                                &own.arguments,
                                &mut tilo_surface_host::Look::none(),
                            )
                            .map(drop)
                    });
            if let Err(error) = carried {
                // The reminder's row went with the person's answer: what they
                // asked for did not happen, so the notice says why.
                self.answer_failed(&alarm.title, &error);
            }
        }
    }
}

/// The notice telling the person of `alarm` at `now`: what it is about, when it is due, which app
/// it is from, and what they can do about it there.
fn notice(alarm: &Alarm, now: DateTime<Utc>) -> Notice {
    let mut notice = Notice::new(
        alarm.app_title.clone(),
        1,
        alarm.title.clone(),
        body(alarm, now),
    );
    notice.actions.push((OPEN.to_owned(), t("reminder-open")));
    for offer in alarm.actions.iter().take(ui::notify::MAX_BUTTONS) {
        notice.actions.push((offer.id.clone(), offer.label.clone()));
    }
    notice.sound = Some(SystemSoundEvent::Notification);
    notice
}

/// What the notice says under its title, one fact a line: when the item is due — or, for a
/// reminder found after its instant, a device that was off past it, that it is late and the time
/// it was due — then the app it is from.
fn body(alarm: &Alarm, now: DateTime<Utc>) -> String {
    let late = now
        .timestamp()
        .checked_sub(alarm.at.timestamp())
        .is_some_and(|past| past > GRACE_SECS);
    let when = if late {
        ui::clock::time_of_day(&alarm.deadline.with_timezone(&Local))
    } else {
        let ahead = alarm.deadline.timestamp().saturating_sub(now.timestamp());
        ui::format::relative_ahead(ahead)
    };
    let mut lines = Vec::new();
    if late {
        lines.push(t("reminder-late"));
    }
    lines.push(when);
    lines.push(alarm.app_title.clone());
    lines.join("\n")
}

/// The platform's wall clock, which a test pins.
fn wall() -> DateTime<Utc> {
    DateTime::from(ui::time::wall())
}

/// The instant a day, and a time of day on it, is in the person's own zone: what a reminder's day
/// is read in.
fn zone(day: NaiveDate, time: Option<NaiveTime>) -> Option<DateTime<Utc>> {
    let at = day.and_time(time.unwrap_or(NaiveTime::MIN));
    Local
        .from_local_datetime(&at)
        .earliest()
        .map(|at| at.with_timezone(&Utc))
}

/// A tilolang app installed on this device: what the desktop calls it, and the package it runs.
struct App {
    /// The app's id: what a click on its reminder opens, and the name its records are kept under.
    id: String,
    payload: PathBuf,
    bundle: Arc<Bundle>,
}

/// One app's record of one workspace, and the host over it.
struct Held {
    host: Host,
    /// The record's bytes as the host holds them, so an unchanged record is not decoded again.
    bytes: Vec<u8>,
}

impl Held {
    /// A host over the record of `bundle`'s app, with the app loaded and its index ready.
    fn open(bundle: &Bundle, person: &str) -> Option<Self> {
        let empty = Doc::default();
        let host = Host::open(
            Opening {
                record: Some(&empty),
                index_dir: None,
                device: None,
            },
            Principal::new(person),
            // A reminder is read here, never acted on.
            holding_nothing(person),
        )
        .inspect_err(|error| tracing::warn!(%error, "a record's host did not come up"))
        .ok()?;
        if let Err(error) = host.load_bundle(bundle) {
            tracing::warn!(app = bundle.meta.app, %error, "an app of the reminders did not load");
            return None;
        }
        host.wait_index();
        Some(Self {
            host,
            bytes: Vec::new(),
        })
    }

    /// Bring the host up to `bytes`, the record as it stands: whether there is a record to read.
    fn sync(&mut self, bytes: Vec<u8>) -> bool {
        if self.bytes == bytes {
            return true;
        }
        let Ok(doc) = AutoCommit::load_with_options(
            &bytes,
            LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
        ) else {
            tracing::warn!("a workspace's record does not load");
            return false;
        };
        self.host.sync_from(&doc);
        self.host.wait_index();
        self.bytes = bytes;
        true
    }
}

/// A directory the worker follows, and the watch that says it changed.
struct Followed {
    _directory: PathBuf,
    _watch: ChangeWatch,
}

/// What tells the loop the records moved.
#[derive(Clone)]
struct Announce {
    signals: Sender<Signal>,
    wake: tilo_service::Wake,
}

impl Announce {
    fn now(&self) {
        let _ = self.signals.send(Signal::Ask);
        (self.wake)();
    }
}

/// The thread that reads the records: a host over each app's record in each workspace, the
/// person's own state of each workspace, and what it follows.
struct Worker {
    /// Where this device keeps its apps' records.
    root: PathBuf,
    person: String,
    /// One store for the whole device, as every part of it reads its records through.
    store: Rc<dyn DurableStore>,
    /// The person's workspaces, as the loop last said.
    workspaces: Vec<String>,
    /// The tilolang apps installed now.
    apps: Vec<App>,
    /// Per app and workspace, the record it keeps there and the host over it.
    held: BTreeMap<(String, String), Held>,
    /// Per workspace, the record of this person's own state of it.
    personal: BTreeMap<String, DocStore>,
    /// The directories the worker followed at its last read.
    followed: Vec<PathBuf>,
    watched: Vec<Followed>,
}

impl Worker {
    fn new(root: PathBuf, person: String) -> Self {
        Self {
            store: Rc::new(app_state::FsStore::new(&root)),
            apps: Vec::new(),
            held: BTreeMap::new(),
            personal: BTreeMap::new(),
            followed: Vec::new(),
            watched: Vec::new(),
            workspaces: Vec::new(),
            root,
            person,
        }
    }

    /// Read every record the person's reminders are in until the session ends, answering each
    /// look the loop asks for.
    fn run(mut self, orders: &Receiver<Order>, signals: &Sender<Signal>, wake: tilo_service::Wake) {
        let announce = Announce {
            signals: signals.clone(),
            wake,
        };
        let mut asked: Option<(DateTime<Utc>, Vec<String>)>;
        loop {
            let Ok(order) = orders.recv() else { return };
            match order {
                Order::Stop => return,
                Order::Look { now, workspaces } => asked = Some((now, workspaces)),
            }
            // What the loop asked while this look ran is one look, at the latest instant.
            while let Ok(order) = orders.try_recv() {
                match order {
                    Order::Stop => return,
                    Order::Look { now, workspaces } => asked = Some((now, workspaces)),
                }
            }
            let Some((now, workspaces)) = asked.take() else {
                continue;
            };
            self.workspaces = workspaces;
            self.install();
            self.watch(&announce);
            let reading = self.look(now);
            if signals.send(Signal::Reading(reading)).is_err() {
                return;
            }
            (announce.wake)();
        }
    }

    /// Read the apps installed now, and the bundles they run, keeping the records of the ones
    /// that stayed installed as they were.
    fn install(&mut self) {
        let Ok(installed) = tilo_installed_apps::read_from_env() else {
            // The app store could not be read: what was installed is still what is.
            return;
        };
        let apps: Vec<App> = installed
            .into_iter()
            .filter_map(|app| {
                let bundle = Bundle::read(&app.payload_root).ok()?;
                Some(App {
                    id: bundle.meta.app.clone(),
                    payload: app.payload_root,
                    bundle: Arc::new(bundle),
                })
            })
            .collect();
        let before: BTreeMap<String, PathBuf> = self
            .apps
            .iter()
            .map(|app| (app.id.clone(), app.payload.clone()))
            .collect();
        let after: BTreeMap<String, PathBuf> = apps
            .iter()
            .map(|app| (app.id.clone(), app.payload.clone()))
            .collect();
        if before == after {
            return;
        }
        self.held
            .retain(|(app, _), _| match (before.get(app), after.get(app)) {
                (Some(was), Some(is)) => was == is,
                _ => false,
            });
        self.apps = apps;
    }

    /// Follow what the records are kept in: each app's own, the person's own, and the app store
    /// an install changes. Followed once per set of directories, never per read.
    fn watch(&mut self, announce: &Announce) {
        let mut roots = vec![(self.root.join(personal::OWNER), true)];
        for app in &self.apps {
            roots.push((self.root.join(&app.id), true));
        }
        for store in tilo_installed_apps::store_directories() {
            roots.push((store, false));
        }
        roots.sort_by(|left, right| left.0.cmp(&right.0));
        roots.dedup_by(|left, right| left.0 == right.0);
        let directories: Vec<PathBuf> = roots.iter().map(|(root, _)| root.clone()).collect();
        if directories == self.followed {
            return;
        }
        self.watched.clear();
        for (directory, deep) in &roots {
            let announce = announce.clone();
            if let Some(watch) = follow(directory, *deep, move || announce.now()) {
                self.watched.push(Followed {
                    _directory: directory.clone(),
                    _watch: watch,
                });
            } else {
                tracing::debug!(
                    root = %directory.display(),
                    "a directory the reminders are kept in is not followed"
                );
            }
        }
        self.followed = directories;
    }

    /// What the person's reminders are in every workspace they are in, at `now`.
    fn look(&mut self, now: DateTime<Utc>) -> Reading {
        let mut reading = Reading::default();
        for workspace in self.workspaces.clone() {
            let told = self.told(&workspace);
            let apps: Vec<(String, Arc<Bundle>)> = self
                .apps
                .iter()
                .map(|app| (app.id.clone(), Arc::clone(&app.bundle)))
                .collect();
            for (id, bundle) in apps {
                let Some(bytes) = self.read(&id, &workspace) else {
                    // The app keeps nothing in this workspace.
                    self.held.remove(&(id, workspace.clone()));
                    continue;
                };
                let key = (id.clone(), workspace.clone());
                if !self.held.contains_key(&key) {
                    let Some(held) = Held::open(&bundle, &self.person) else {
                        continue;
                    };
                    self.held.insert(key.clone(), held);
                }
                let Some(held) = self.held.get_mut(&key) else {
                    continue;
                };
                if !held.sync(bytes) {
                    self.held.remove(&key);
                    continue;
                }
                let found = reminders::look(&held.host, &self.person, now, &zone, &told);
                for alarm in &found.due {
                    self.remember(&workspace, alarm);
                }
                reading.due.extend(
                    found
                        .due
                        .into_iter()
                        .map(|alarm| (workspace.clone(), alarm)),
                );
                reading.heard.extend(
                    found
                        .heard
                        .into_iter()
                        .map(|alarm| (workspace.clone(), alarm)),
                );
                reading.next = match (reading.next, found.next) {
                    (Some(next), Some(found)) => Some(next.min(found)),
                    (next, found) => next.or(found),
                };
            }
        }
        reading
    }

    /// The record `app` keeps in `workspace`, as it stands: none when nothing is kept there.
    fn read(&self, app: &str, workspace: &str) -> Option<Vec<u8>> {
        let key = StorageKey::workspace_index(app, workspace);
        match self.store.read(&key) {
            Ok(Some(stored)) if !stored.bytes.is_empty() => Some(stored.bytes),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, app, %workspace, "a workspace's record is not read");
                None
            }
        }
    }

    /// The person's own state of `workspace`, as their record holds it.
    fn told(&mut self, workspace: &str) -> PersonalState {
        let Some(store) = self.personal(workspace) else {
            return PersonalState::default();
        };
        let loaded = store.load();
        let doc = match loaded {
            Ok(Some(doc)) => doc,
            Ok(None) => return PersonalState::default(),
            Err(error) => {
                tracing::warn!(%error, "the person's own state does not read");
                return PersonalState::default();
            }
        };
        match Personal::read(&doc) {
            Ok(state) => state.of(&self.person),
            Err(error) => {
                tracing::warn!(%error, "the person's own state does not read");
                PersonalState::default()
            }
        }
    }

    /// The record of this person's own state of `workspace`, joined the first time it is needed.
    fn personal(&mut self, workspace: &str) -> Option<&mut DocStore> {
        if !self.personal.contains_key(workspace) {
            let record = personal_record()?;
            let key = StorageKey::record(personal::OWNER, workspace, &record);
            let store = DocStore::shared(Rc::clone(&self.store), key)
                .inspect_err(|error| tracing::warn!(%error, "the person's own state is not joined"))
                .ok()?;
            self.personal.insert(workspace.to_owned(), store);
        }
        self.personal.get_mut(workspace)
    }

    /// Keep `alarm` as one the person was told of, in the workspace's record of their own state,
    /// so none of their devices tells them again.
    fn remember(&mut self, workspace: &str, alarm: &Alarm) {
        let person = self.person.clone();
        let keep = Keep::Reminded {
            person: person.clone(),
            reminder: alarm.key.clone(),
        };
        let Ok(encoded) = tilo_resource::encode::<Personal>(&keep) else {
            tracing::warn!(reminder = %alarm.key, "a reminder's mark does not encode");
            return;
        };
        let context = ActionContext::for_resource(personal::KIND, personal::KIND, Personal::LABEL)
            .with_invoker(ActionInvoker::actor(person));
        let Some(store) = self.personal(workspace) else {
            return;
        };
        let mut doc = store.load_or_new();
        match tilo_actions::apply_local_with_context(&mut doc, &keep, encoded, None, context) {
            Ok(_) => {
                doc.commit();
                if let Err(error) = store.append(&mut doc) {
                    tracing::warn!(%error, "a reminder's mark is not kept");
                }
            }
            Err(error) => tracing::warn!(%error, "a reminder's mark is not kept"),
        }
    }
}

/// The record of each person's own state of a workspace, as the apps open it.
fn personal_record() -> Option<String> {
    let id = collab_types::workspace_record_id(personal::KIND);
    RecordId::parse(&id).map(|id| id.storage_key())
}

/// Follow `root`: its whole tree once it is there, and the entry that becomes it until then, so a
/// directory made later is followed from the moment it is.
fn follow(root: &Path, deep: bool, changed: impl Fn() + Send + 'static) -> Option<ChangeWatch> {
    let through = root.ancestors().find(|path| path.is_dir())?;
    let watch = if through == root {
        if deep {
            Watcher::tree(root)
        } else {
            Watcher::entries(root)
        }
        .on_change(|_| true, changed)
    } else {
        let wanted = root.to_path_buf();
        Watcher::entries(through)
            .presence()
            .on_change(move |path| path == wanted, changed)
    };
    watch
        .inspect_err(|error| tracing::warn!(%error, root = %root.display(), "not followed"))
        .ok()
}
