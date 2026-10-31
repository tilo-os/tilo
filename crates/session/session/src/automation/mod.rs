//! The shortcuts a person saved, started without anyone invoking them.
//!
//! A shortcut is saved in a workspace ([`tilo_automation::Shortcut`]) with
//! the triggers that start it. This module follows those triggers on the
//! device and starts a run for each firing through the action broker
//! ([`tilo_action_broker::collab_broker::AppActionBroker::start_triggered_shortcut_run`]),
//! which checks again that the shortcut still exists and that its saver may
//! still run it. A run started here is the same run a person's press or an
//! agent's invocation starts, with the same checks.
//!
//! What it follows, each from where the session already reads it:
//!
//! - a record change, from the record documents of this device's rooms,
//!   read on a worker that a tree watch wakes ([`records`]);
//! - the passing of time, a workspace being joined and an application
//!   starting, from the desk the session shows and the launches it made;
//! - the focus modes a shortcut can answer, from the person's notification
//!   policy, which the settings watch keeps current;
//! - a file changing in a folder a shortcut's capability shares, from the
//!   same file watch the session's folders use ([`folders`]);
//! - a keyboard chord, once a press reaches the automation host rather than
//!   a chord of the desktop's own table ([`Automation::press`]);
//! - the revoked devices of a room a shortcut is saved in, from the room's
//!   governance as this device holds it.
//!
//! Only shortcuts saved by the person this device acts for in their room
//! are armed ([`triggers::armed`]), so a collaborator's shortcut never fires
//! on someone else's behalf.

pub(crate) mod folders;
pub(crate) mod records;
pub(crate) mod triggers;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike as _, Timelike as _};
use settings_store::hardware::{ActionChord, KeyChord};
use settings_store::notification_policy::NotificationPolicy;
use tilo_action_broker::automation::chord_target;
use tilo_action_broker::collab_broker::AppActionBroker;
use tilo_actions::remote::Caller;
use tilo_automation::dispatch::RecordEvent;
use tilo_automation::{RunId, ScheduleClaim, TriggerSpec};
use tilo_desktop_model::RoomId;
use tilo_desktop_surface::DesktopHandle;
use tilo_lock::MutexRecover as _;
use tilo_service::Wake;

pub(crate) use triggers::{
    Armed, Firing, FocusState, PersonalDevices, TriggerEvent, TriggerHost, armed, unix_ns,
};

/// The person's certified devices as this session's fabric sees them: which
/// of them this one is, and which are online to run a schedule's fire time.
fn devices_of(fabric: Option<&tilo_device_fabric::link::SharedLink>) -> PersonalDevices {
    let Some(fabric) = fabric else {
        return PersonalDevices::default();
    };
    PersonalDevices {
        local: fabric
            .local_device_fabric_identity()
            .map(|(device, _)| device.as_str().to_string()),
        online: fabric
            .online_device_ids()
            .iter()
            .map(|device| device.as_str().to_string())
            .collect(),
    }
}

pub use tilo_action_broker::automation::FinishedShortcutRun;

/// How often the passing of time is shown to the schedules.
const TICK_INTERVAL: Duration = Duration::from_millis(500);

/// How many finished runs are kept for the history recorder to take.
const KEPT_RUNS: usize = 64;

/// The session's trigger host: the shortcuts it arms, what it shows them,
/// and the runs it starts.
pub struct Automation {
    triggers: TriggerHost,
    feed: records::RecordFeed,
    /// What a person saves through the action door, applied on the loop.
    saved: tokio::sync::mpsc::UnboundedReceiver<crate::plane::StoreRequest>,
    /// The rooms whose records are followed now, so the worker is told only
    /// when that changed.
    followed: BTreeSet<String>,
    /// The apps' broker, while the session serves one: every firing starts
    /// through it.
    broker: Option<Arc<AppActionBroker>>,
    /// The person's devices, refreshed each tick from `fabric`: a schedule
    /// fires on one of them, and each waits its turn for the others.
    devices: Arc<triggers::FixedDevices>,
    /// This session's device fabric, while the person certified its device.
    fabric: Option<tilo_device_fabric::link::SharedLink>,
    /// The apps this session started, which an application-opened trigger
    /// follows.
    launches: Arc<crate::launches::Launches>,
    /// The folders file-changed triggers watch, and what changed in them.
    folders: folders::FolderWatch,
    /// The person's notification policy, as the session keeps it current:
    /// the focus modes a shortcut can answer.
    policy: Arc<std::sync::Mutex<NotificationPolicy>>,
    /// What each of this device's rooms revokes, as the room's governance
    /// records it.
    revocations: crate::engine::Revocations,
    handle: DesktopHandle,
    /// The person at this device, as the session's own identity names them:
    /// a shortcut saved by anyone else is never armed.
    person: String,
    /// The rooms with a shortcut armed now: the ones the desk is published
    /// for, because a firing there has to find its shortcut.
    armed_rooms: BTreeSet<String>,
    /// The shortcuts armed now, as the last look at the desk found them: the
    /// chords the keys answer are theirs.
    armed_now: Vec<Armed>,
    /// The desk revision the broker's copy was last brought level with.
    published: Option<u64>,
    /// The desk revision whose shortcuts were last armed: a desk that moved
    /// arms again on the next pass, so a shortcut a person just saved
    /// follows its room's records from the moment it is shown rather than
    /// from the next tick.
    armed_at: Option<u64>,
    /// When the passing of time was last shown to the schedules.
    last_tick: RefCell<Option<Instant>>,
    /// Runs that finished and no history recorder took yet.
    finished: RefCell<Vec<FinishedShortcutRun>>,
    /// Firings that came while the apps' broker was still starting: they
    /// start once it serves, rather than being lost.
    waiting: RefCell<Vec<Firing>>,
}

/// What the session shows the trigger host besides its desk: the apps it
/// starts, the person's notification policy, what each of its rooms revokes,
/// and which of the person's devices are online to run a schedule.
pub struct Feeds {
    pub fabric: Option<tilo_device_fabric::link::SharedLink>,
    pub launches: Arc<crate::launches::Launches>,
    pub policy: Arc<std::sync::Mutex<NotificationPolicy>>,
    pub revocations: crate::engine::Revocations,
}

impl Automation {
    /// The trigger host of a session whose desk is `handle`, keeping its
    /// schedule ledger under `state` in the file the shell kept it in.
    #[must_use]
    pub fn new(
        state: Option<&Path>,
        wake: Wake,
        handle: DesktopHandle,
        person: String,
        saved: tokio::sync::mpsc::UnboundedReceiver<crate::plane::StoreRequest>,
        feeds: Feeds,
    ) -> Self {
        let Feeds {
            fabric,
            launches,
            policy,
            revocations,
        } = feeds;
        let root = cli::data_home();
        let devices = Arc::new(triggers::FixedDevices::new(devices_of(fabric.as_ref())));
        let mut triggers = TriggerHost::for_session(state, wake.clone());
        triggers.set_devices(Arc::clone(&devices) as Arc<dyn triggers::DeviceDirectory>);
        Self {
            triggers,
            feed: records::RecordFeed::start(root, BTreeSet::new(), wake),
            saved,
            followed: BTreeSet::new(),
            broker: None,
            devices,
            fabric,
            launches,
            folders: folders::FolderWatch::default(),
            policy,
            revocations,
            handle,
            person,
            armed_rooms: BTreeSet::new(),
            armed_now: Vec::new(),
            published: None,
            armed_at: None,
            last_tick: RefCell::new(None),
            finished: RefCell::new(Vec::new()),
            waiting: RefCell::new(Vec::new()),
        }
    }

    /// Start every firing through `broker`.
    pub fn attach(&mut self, broker: Arc<AppActionBroker>) {
        self.broker = Some(broker);
    }

    /// Whether the broker a firing starts on is missing still.
    #[must_use]
    pub fn needs_broker(&self) -> bool {
        self.broker.is_none()
    }

    /// The broker's copy of the desk has to follow the desk the loop shows:
    /// a firing starts on the broker's thread, which cannot read this one.
    /// Nothing is published while no shortcut is armed.
    ///
    /// The desk belongs to the loop ([`DesktopHandle`] hands it out here
    /// only) and `desk` reads it on the broker's thread, so the loop hands
    /// over a copy when the desk moved.
    fn publish_desk(&mut self, desk: Option<&crate::plane::DeskAuthority>) {
        let Some(desk) = desk else {
            return;
        };
        if self.armed_rooms.is_empty() {
            return;
        }
        let revision = self.handle.revision();
        if self.published == Some(revision) {
            return;
        }
        desk.publish(self.handle.read(Clone::clone));
        self.published = Some(revision);
    }

    /// Take what the broker finished, show the schedules the passing of
    /// time, and start whatever fired. Once per pass.
    pub fn settle(&mut self, desk: Option<&crate::plane::DeskAuthority>) {
        self.keep_finished();
        self.apply_saved();
        // What fired while the broker was starting starts now that it serves.
        if self.broker.is_some() {
            let waiting = std::mem::take(&mut *self.waiting.borrow_mut());
            self.start(waiting);
        }
        // A schedule firing waits for the ledger that records it: the write
        // wakes the loop, and its firings start then rather than a tick
        // later.
        let settled = self.triggers.take_settled_firings();
        if !settled.is_empty() {
            self.start(settled);
        }
        let now = Instant::now();
        let tick = self
            .last_tick
            .borrow()
            .is_none_or(|last| now.duration_since(last) >= TICK_INTERVAL);
        // The desk can change on any pass: a shortcut shown now follows its
        // records from this pass on, so a change made right after it appears
        // is one it fires on.
        if !tick && self.armed_at == Some(self.handle.revision()) {
            // The broker reads the desk only when a shortcut is armed, and
            // then only when it moved.
            self.publish_desk(desk);
            return;
        }
        if tick {
            *self.last_tick.borrow_mut() = Some(now);
        }
        self.observe(tick);
        self.publish_desk(desk);
    }

    /// A chord was pressed that nothing else of the desktop's answers: it
    /// starts every shortcut whose keyboard trigger names it, once.
    pub fn press(&mut self, chord: &KeyChord) {
        let now_ns = unix_ns(self.triggers.clock().now());
        let armed = self.armed(now_ns);
        let firings = self.triggers.observe(TriggerEvent::Key(chord), &armed);
        self.start(firings);
    }

    /// The chords the armed shortcuts' keyboard triggers answer, as the
    /// desktop's shortcut table lists them: the key surface claims these,
    /// and the guide shows what each runs.
    #[must_use]
    pub fn trigger_chords(&self) -> Vec<ActionChord> {
        self.armed_now
            .iter()
            .flat_map(|armed| {
                armed.shortcut.triggers.iter().filter_map(move |trigger| {
                    let TriggerSpec::Keyboard { chord } = trigger else {
                        return None;
                    };
                    Some(ActionChord {
                        chord: KeyChord::parse(chord).ok()?,
                        target: chord_target(&armed.room, &armed.shortcut.id),
                        label: format!("Run {}", armed.shortcut.name),
                    })
                })
            })
            .collect()
    }

    /// Arm the shortcuts the desk shows, show every source what this device
    /// sees now, and start what fired. `tick` says the passing of time came
    /// round, which the schedules alone are shown.
    fn observe(&mut self, tick: bool) {
        self.devices.set(devices_of(self.fabric.as_ref()));
        let now_ns = unix_ns(self.triggers.clock().now());
        let armed = self.armed(now_ns);
        self.armed_rooms = armed.iter().map(|armed| armed.room.clone()).collect();
        self.armed_now.clone_from(&armed);
        self.follow_records(&armed);
        self.armed_at = Some(self.handle.revision());
        let mut firings = if tick {
            let mut firings = self.triggers.observe(TriggerEvent::Tick, &armed);
            let running = self.launches.running_apps();
            firings.extend(self.triggers.observe(TriggerEvent::Apps(&running), &armed));
            let workspaces: BTreeMap<String, String> = self.handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .map(|workspace| (workspace.room().to_string(), workspace.name.clone()))
                    .collect()
            });
            firings.extend(
                self.triggers
                    .observe(TriggerEvent::Workspaces(&workspaces), &armed),
            );
            let focus = self.focus_state();
            firings.extend(self.triggers.observe(TriggerEvent::Focus(focus), &armed));
            firings
        } else {
            Vec::new()
        };
        let events = self.feed.take();
        if !events.is_empty() {
            // A run's own first change is its claim: a firing another device
            // already started is not held for this one.
            let started: BTreeSet<RunId> =
                events.iter().filter_map(RecordEvent::started_run).collect();
            self.triggers.forget_started(&started);
            firings.extend(
                self.triggers
                    .observe(TriggerEvent::Records(&events), &armed),
            );
        }
        if tick {
            self.follow_folders(&armed);
            let changes = self.folders.changes();
            if !changes.is_empty() {
                firings.extend(self.triggers.observe(TriggerEvent::Files(&changes), &armed));
            }
        }
        self.start(firings);
    }

    /// Watch exactly the folders the armed shortcuts' file-changed triggers
    /// name, for the changes the next look reports.
    fn follow_folders(&mut self, armed: &[Armed]) {
        let folders = self
            .handle
            .read(|tree| triggers::watched_folders(&tree.workspaces, armed));
        self.folders.watch(folders);
    }

    /// Which focus modes are on now, as the person's notification policy
    /// keeps them: their switch, and the schedule at this device's local
    /// time.
    fn focus_state(&self) -> FocusState {
        let now = self.triggers.clock().now();
        let local = now.with_timezone(&self.triggers.clock().offset_at(now));
        let minute = local
            .hour()
            .checked_mul(60)
            .and_then(|minutes| minutes.checked_add(local.minute()))
            .and_then(|minute| u16::try_from(minute).ok())
            .unwrap_or(0);
        let weekday = u8::try_from(local.weekday().num_days_from_monday()).unwrap_or(0);
        let policy = self.policy.lock_recover();
        FocusState {
            do_not_disturb: policy.do_not_disturb,
            focus: policy.focus.active_until(minute, weekday).is_some(),
        }
    }

    /// The shortcuts this device may start: those the person it acts for in
    /// their room saved with a device of theirs, with the fire times of that
    /// room's schedules already claimed.
    fn armed(&self, now_ns: u64) -> Vec<Armed> {
        let revocations = &self.revocations;
        self.handle.read(|tree| {
            armed(
                &tree.workspaces,
                |workspace| self.person_in(&workspace.room().to_string()),
                // A room this device holds arms against what its governance
                // revokes. One it does not hold is a workspace on the desk
                // whose room lives elsewhere: nothing of it is known here,
                // and the run it starts is refused by the broker unless the
                // room it holds says otherwise.
                |workspace| Some(revocations(&workspace.room().to_string()).unwrap_or_default()),
                now_ns,
            )
        })
    }

    /// The rooms where an armed shortcut watches record changes or incoming
    /// messages: only their records are read.
    fn follow_records(&mut self, armed: &[Armed]) {
        let rooms: BTreeSet<String> = armed
            .iter()
            .filter(|armed| {
                armed.shortcut.triggers.iter().any(|trigger| {
                    matches!(
                        trigger,
                        TriggerSpec::RecordChanged { .. } | TriggerSpec::IncomingMessage { .. }
                    )
                })
            })
            .map(|armed| armed.room.clone())
            .collect();
        if rooms != self.followed {
            self.feed.follow(rooms.clone());
            self.followed = rooms;
        }
    }

    /// Who this device acts for in `room`: the identity the registry
    /// presents there, else the session's own person.
    fn person_in(&self, room: &str) -> String {
        tilo_identity_host::registry::selected_principal_id_prompt(room)
            .ok()
            .flatten()
            .unwrap_or_else(|| self.person.clone())
    }

    /// Start every firing as the person this device acts for in its room,
    /// claiming a schedule's fire time first so the person's other devices
    /// leave it alone.
    fn start(&self, firings: Vec<Firing>) {
        let Some(broker) = self.broker.clone() else {
            // The broker may still be starting: what fired waits for it, and
            // starts from the pass that finds it.
            self.waiting.borrow_mut().extend(firings);
            return;
        };
        for firing in firings {
            if let Some(claim) = &firing.claim {
                self.claim(&firing.room, claim);
            }
            let caller = Caller::local_person(self.person_in(&firing.room)).in_room(&firing.room);
            match broker.start_triggered_shortcut_run(
                &caller,
                &firing.room,
                &firing.shortcut,
                &firing.trigger,
                firing.run,
                firing.inputs.clone(),
            ) {
                Ok(_) => {}
                Err(error) => {
                    // A shortcut deleted or changed elsewhere, or one whose
                    // saver may not run it here: nothing started, and the
                    // next arming drops it.
                    tracing::info!(
                        shortcut = %firing.shortcut,
                        trigger = firing.trigger.kind(),
                        %error,
                        "a shortcut trigger fired but the shortcut did not start"
                    );
                }
            }
        }
    }

    /// Store every shortcut change the action door took since the last pass,
    /// and answer the apps that asked: the desk belongs to this thread.
    fn apply_saved(&mut self) {
        while let Ok(request) = self.saved.try_recv() {
            let result = self.save(
                request.room.as_str(),
                request.actor.as_str(),
                &request.change,
            );
            if let Err(refusal) = &result {
                tracing::info!(
                    room = %request.room,
                    actor = %request.actor,
                    %refusal,
                    "a saved shortcut was refused"
                );
            }
            let _ = request.done.send(result);
        }
    }

    /// `change` stored in the workspace of `room`, as `actor` asked: a saved
    /// shortcut carries this device's proof of who saved it, which is what
    /// arms its triggers on the saver's devices.
    fn save(
        &self,
        room: &str,
        actor: &str,
        change: &tilo_automation::ShortcutChange,
    ) -> Result<(), String> {
        let (key, device) = tilo_identity_host::broker::trusted_current_device(room)
            .map_err(|error| format!("this device cannot sign in the workspace: {error}"))?;
        if actor != hex::encode(device.user_public_key) {
            return Err("a shortcut is saved from its saver's own device".into());
        }
        let change = match change {
            tilo_automation::ShortcutChange::Save(shortcut) => {
                let mut shortcut = shortcut.as_ref().clone();
                // A trigger this device cannot watch would be saved into a
                // shortcut that never runs: the person is told now.
                for trigger in &shortcut.triggers {
                    triggers::validate_trigger(trigger)?;
                }
                tilo_peer_state::room::sign_saved_shortcut(room, &mut shortcut, &device, &key)?;
                tilo_automation::ShortcutChange::Save(Box::new(shortcut))
            }
            delete @ tilo_automation::ShortcutChange::Delete(_) => delete.clone(),
        };
        let mut tree = self.handle.read(Clone::clone);
        let workspace = tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room().to_string() == room)
            .ok_or("this device holds no such workspace")?;
        match change {
            tilo_automation::ShortcutChange::Save(shortcut) => {
                let saved = tilo_automation::SavedShortcut::encode(&shortcut);
                match workspace
                    .shortcuts
                    .iter_mut()
                    .find(|held| held.id() == saved.id())
                {
                    Some(held) => *held = saved,
                    None => workspace.shortcuts.push(saved),
                }
                workspace
                    .shortcuts
                    .sort_by(|left, right| left.id().cmp(right.id()));
            }
            tilo_automation::ShortcutChange::Delete(id) => {
                let before = workspace.shortcuts.len();
                workspace.shortcuts.retain(|held| held.id() != id);
                if workspace.shortcuts.len() == before {
                    return Err(format!("there is no shortcut `{id}`"));
                }
            }
        }
        self.handle.replace_tree(tree);
        Ok(())
    }

    /// Record `claim` in the workspace of `room`: the fire time this device
    /// took, which its other devices read back and leave alone.
    fn claim(&self, room: &str, claim: &ScheduleClaim) {
        let Some(room) = RoomId::parse(room) else {
            return;
        };
        let mut tree = self.handle.read(Clone::clone);
        let Some(workspace) = tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room() == room)
        else {
            return;
        };
        // A fire time of a shortcut no longer saved is no claim: it would
        // keep the person's other devices off a shortcut that is gone.
        if !workspace
            .shortcuts
            .iter()
            .any(|shortcut| shortcut.id() == claim.shortcut)
        {
            return;
        }
        let shortcuts = workspace.shortcuts.clone();
        workspace.schedule_claims.retain(|held| {
            shortcuts
                .iter()
                .any(|shortcut| shortcut.id() == held.shortcut)
        });
        if workspace
            .schedule_claims
            .iter()
            .any(|held| held.key() == claim.key())
        {
            return;
        }
        workspace.schedule_claims.push(claim.clone());
        self.handle.replace_tree(tree);
    }

    /// Keep the runs the broker finished until the history recorder takes
    /// them.
    fn keep_finished(&self) {
        let Some(broker) = &self.broker else {
            return;
        };
        let mut finished = self.finished.borrow_mut();
        finished.extend(broker.take_finished_shortcut_runs());
        if finished.len() > KEPT_RUNS {
            let excess = finished.len().saturating_sub(KEPT_RUNS);
            finished.drain(..excess);
        }
    }

    /// The shortcut runs that finished and no history recorder took yet,
    /// oldest first.
    pub fn take_finished(&self) -> Vec<FinishedShortcutRun> {
        std::mem::take(&mut *self.finished.borrow_mut())
    }
}
