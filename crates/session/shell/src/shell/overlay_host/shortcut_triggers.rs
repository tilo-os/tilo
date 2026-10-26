//! Chords, schedules and session events that start published actions.
//!
//! The host keeps the session's [`TriggerHost`] and shows it what it can
//! see: a pressed chord nothing else in the shell took, and on each tick the
//! passing of time, the applications publishing live actions to the broker,
//! the workspaces this device is in, whether Do Not Disturb and the focus
//! schedule are on, the files changed in the folders file-changed triggers
//! watch, and the committed changes to records in rooms where a shortcut
//! watches record changes or incoming messages. Every firing starts through the action
//! broker as the person this device acts for in the shortcut's room, so a
//! run from the keyboard or a schedule meets the same checks as one from
//! the palette, an agent or a peer.
//!
//! The chords of the local person's keyboard triggers are mirrored into the
//! session's keymap ([`ShortcutConfig::actions`]) beside any other action
//! chords, so the shortcut guide, Settings and client pre-routing all read
//! one table and a chord is never offered to an app it would then be taken
//! from.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use chrono::{Datelike as _, Timelike as _};

use settings_store::hardware::{ActionChord, KeyChord};
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget, workspace_provider_room,
};
use tilo_actions::{ActionAddress, InvocationId};
use winit::event::Modifiers;
use winit::keyboard::Key;

use super::ShellOverlayHost;
use super::shortcuts::shortcut_chord_from_event;
use crate::shell::automation_folders::FolderWatch;
use crate::shell::automation_host::{is_shortcut_target, replace_trigger_chords, trigger_chords};
use crate::shell::automation_records::RecordFeed;
use crate::shell::automation_triggers::{
    Armed, Clock, DeviceDirectory, Firing, FocusState, TriggerEvent, TriggerHost, armed, unix_ns,
    watched_folders,
};
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::remote_actions::BrokerActionHost;
use crate::shell::state::RoomId;
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::RecordEvent;

/// How often time is shown to the schedule source.
const TICK_INTERVAL: Duration = Duration::from_millis(500);

pub(super) struct ShortcutTriggerState {
    triggers: TriggerHost,
    broker: Option<AppActionBroker>,
    last_tick: Option<Instant>,
    /// The broker's published-surface generation last read for live apps.
    apps_generation: Option<u64>,
    folders: FolderWatch,
    records: RecordFeed,
}

impl ShortcutTriggerState {
    pub(super) fn new(triggers: TriggerHost) -> Self {
        Self {
            triggers,
            broker: None,
            last_tick: None,
            apps_generation: None,
            folders: FolderWatch::default(),
            records: RecordFeed::default(),
        }
    }

    pub(super) fn attach(&mut self, broker: AppActionBroker) {
        self.broker = Some(broker);
        self.apps_generation = None;
    }

    pub(super) fn broker(&self) -> Option<AppActionBroker> {
        self.broker.clone()
    }

    /// The applications publishing live actions, when that changed since
    /// the last look.
    fn live_apps(&mut self) -> Option<BTreeSet<String>> {
        let broker = self.broker.as_ref()?;
        let generation = broker.surfaces_generation();
        if self.apps_generation.replace(generation) == Some(generation) {
            return None;
        }
        Some(
            broker
                .surfaces()
                .into_iter()
                .filter(|surface| surface.live)
                .map(|surface| surface.app_id)
                .collect(),
        )
    }
}

/// Which focus modes are on at the clock's local time.
fn focus_state(state: &crate::shell::chrome_state::ChromeState, clock: &dyn Clock) -> FocusState {
    let now = clock.now();
    let local = now.with_timezone(&clock.offset_at(now));
    let minute = local
        .hour()
        .checked_mul(60)
        .and_then(|minutes| minutes.checked_add(local.minute()))
        .and_then(|minute| u16::try_from(minute).ok())
        .unwrap_or(0);
    let weekday = u8::try_from(local.weekday().num_days_from_monday()).unwrap_or(0);
    FocusState {
        do_not_disturb: state.dnd(),
        focus: state.focus_schedule_on(minute, weekday),
    }
}

/// The records held in each room where an armed shortcut watches record
/// changes or incoming messages: only those are read each tick.
fn watched_record_rooms(
    held: &std::collections::HashMap<RoomId, BTreeMap<String, String>>,
    armed: &[Armed<'_>],
) -> Vec<(String, BTreeMap<String, String>)> {
    held.iter()
        .map(|(room, records)| (room.to_string(), records))
        .filter(|(room, _)| {
            armed.iter().any(|armed| {
                armed.room == *room
                    && armed.shortcut.triggers.iter().any(|trigger| {
                        matches!(
                            trigger,
                            TriggerSpec::RecordChanged { .. } | TriggerSpec::IncomingMessage { .. }
                        )
                    })
            })
        })
        .map(|(room, records)| (room, records.clone()))
        .collect()
}

impl ShellOverlayHost {
    /// Read the person's devices from `devices`, so each of their
    /// schedules runs on one of them.
    pub fn set_personal_devices(&mut self, devices: std::sync::Arc<dyn DeviceDirectory>) {
        self.shortcut_triggers.triggers.set_devices(devices);
    }

    /// Show the passing time to the trigger sources, start whatever fired,
    /// and bring the keymap's shortcut chords up to date. Runs each frame;
    /// does real work at most every `TICK_INTERVAL`. Whether a run was
    /// started.
    pub fn tick_shortcut_triggers(&mut self) -> bool {
        let now = Instant::now();
        if self
            .shortcut_triggers
            .last_tick
            .is_some_and(|last| now.saturating_duration_since(last) < TICK_INTERVAL)
        {
            // A schedule firing held for its ledger write runs as soon as the
            // write lands (which wakes the loop), not a whole tick later.
            let settled = self.shortcut_triggers.triggers.take_settled_firings();
            return self.start_firings(settled);
        }
        self.shortcut_triggers.last_tick = Some(now);
        self.observe_shortcut_triggers(TriggerEvent::Tick)
    }

    /// Every chord bound to a published action, as the guide lists them.
    #[must_use]
    pub fn action_chords(&self) -> &[ActionChord] {
        &self.shortcut_config.actions
    }

    /// When an idle loop must look again for a schedule firing that waits
    /// for its ledger write. The write wakes the loop itself; this is the
    /// backstop for a loop no wake reaches.
    pub(super) fn held_firings_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.shortcut_triggers
            .triggers
            .holds_firings()
            .then(|| now.checked_add(TICK_INTERVAL))
            .flatten()
    }

    /// [`Self::tick_shortcut_triggers`] without the frame throttle.
    pub fn tick_shortcut_triggers_now(&mut self) -> bool {
        self.shortcut_triggers.last_tick = Some(Instant::now());
        self.observe_shortcut_triggers(TriggerEvent::Tick)
    }

    fn observe_shortcut_triggers(&mut self, event: TriggerEvent<'_>) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let ticking = matches!(event, TriggerEvent::Tick);
        let apps = if ticking {
            self.shortcut_triggers.live_apps()
        } else {
            None
        };
        let now_ns = unix_ns(self.shortcut_triggers.triggers.clock().now());
        let (firings, chords) = {
            let state = chrome.borrow();
            let tree = state.tree();
            let armed = armed(
                &tree.desktop.workspaces,
                |workspace| state.local_user_for_room(&workspace.room()),
                |workspace| tree.room_revocations(workspace.room()),
                now_ns,
            );
            let chords = trigger_chords(&armed);
            let mut firings = self.shortcut_triggers.triggers.observe(event, &armed);
            if ticking {
                if let Some(apps) = &apps {
                    firings.extend(
                        self.shortcut_triggers
                            .triggers
                            .observe(TriggerEvent::Apps(apps), &armed),
                    );
                }
                let workspaces: BTreeMap<String, String> = tree
                    .desktop
                    .workspaces
                    .iter()
                    .map(|workspace| (workspace.room().to_string(), workspace.name.clone()))
                    .collect();
                firings.extend(
                    self.shortcut_triggers
                        .triggers
                        .observe(TriggerEvent::Workspaces(&workspaces), &armed),
                );
                let focus = focus_state(&state, self.shortcut_triggers.triggers.clock());
                firings.extend(
                    self.shortcut_triggers
                        .triggers
                        .observe(TriggerEvent::Focus(focus), &armed),
                );
                let rooms = watched_record_rooms(&tree.desktop.room_records_held, &armed);
                let events = self.shortcut_triggers.records.poll(&rooms, state.records());
                if !events.is_empty() {
                    // A run's own first change is its claim: a firing another
                    // device already started is not held for this one.
                    let started = events.iter().filter_map(RecordEvent::started_run).collect();
                    self.shortcut_triggers.triggers.forget_started(&started);
                }
                firings.extend(
                    self.shortcut_triggers
                        .triggers
                        .observe(TriggerEvent::Records(&events), &armed),
                );
                self.shortcut_triggers
                    .folders
                    .watch(watched_folders(&tree.desktop.workspaces, &armed));
                let changes = self.shortcut_triggers.folders.changes();
                if !changes.is_empty() {
                    firings.extend(
                        self.shortcut_triggers
                            .triggers
                            .observe(TriggerEvent::Files(&changes), &armed),
                    );
                }
            }
            drop(tree);
            (firings, chords)
        };
        self.sync_trigger_chords(chords);
        self.start_firings(firings)
    }

    /// Mirror the local person's keyboard triggers into the keymap,
    /// persisting it when it follows the settings file.
    fn sync_trigger_chords(&mut self, chords: Vec<ActionChord>) {
        let mut keymap = self.shortcut_config.clone();
        if !replace_trigger_chords(&mut keymap, chords) {
            return;
        }
        if !self.desktop_controls.is_explicit()
            && let Err(error) = keymap.save()
        {
            tracing::warn!(%error, "shortcut chords were not saved to settings");
        }
        self.shortcut_config = keymap;
        self.refresh_nested_shortcuts();
    }

    fn start_firings(&self, firings: Vec<Firing>) -> bool {
        if firings.is_empty() {
            return false;
        }
        let (Some(chrome), Some(broker)) =
            (self.chrome.clone(), self.shortcut_triggers.broker.clone())
        else {
            return false;
        };
        let mut started = false;
        for firing in firings {
            let person = {
                let state = chrome.borrow();
                let tree = state.tree();
                tree.desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room().to_string() == firing.room)
                    .map(|workspace| state.local_user_for_room(&workspace.room()))
            };
            let Some(person) = person else {
                continue;
            };
            // Taken before the run starts, so the person's other devices
            // leave this fire time even if the run is slow to finish.
            if let Some(claim) = &firing.claim
                && let Some(room) = crate::shell::state::RoomId::parse(&firing.room)
            {
                chrome.borrow().tree_mut().claim_schedule(room, claim);
            }
            let caller = Caller::local_person(person).in_room(firing.room.clone());
            match broker.start_triggered_shortcut_run(
                &caller,
                &firing.room,
                &firing.shortcut,
                &firing.trigger,
                firing.run,
                firing.inputs.clone(),
            ) {
                Ok(_) => started = true,
                Err(error) => {
                    tracing::info!(
                        shortcut = %firing.shortcut,
                        trigger = firing.trigger.kind(),
                        %error,
                        "a shortcut trigger fired but the shortcut did not start"
                    );
                    if matches!(
                        firing.trigger,
                        tilo_automation::TriggerSpec::Keyboard { .. }
                    ) {
                        chrome.borrow_mut().push_warning_notification(
                            crate::shell::i18n::t("shortcut-trigger-failed"),
                            error.to_string(),
                        );
                    }
                }
            }
        }
        started
    }

    /// Whether the chord of this press runs a published action, so the
    /// compositor keeps it from a focused client.
    pub(crate) fn owns_action_chord(&self, logical_key: &Key, modifiers: Modifiers) -> bool {
        shortcut_chord_from_event(logical_key, modifiers)
            .is_some_and(|chord| self.shortcut_config.action_for(&chord).is_some())
    }

    /// Whether a press belongs to the shell rather than a focused client:
    /// a configurable desktop action or a chord bound to a published action.
    pub(crate) fn owns_configured_chord(&self, logical_key: &Key, modifiers: Modifiers) -> bool {
        self.configured_shortcut_action(logical_key, modifiers)
            .is_some()
            || self.owns_action_chord(logical_key, modifiers)
    }

    /// Run whatever this press's chord is bound to: the keyboard trigger of
    /// a saved shortcut, or any other published action in the keymap. A
    /// held key repeats nothing, but stays the shell's. Whether the chord
    /// was bound.
    pub(super) fn handle_action_chord(
        &mut self,
        logical_key: &Key,
        modifiers: Modifiers,
        repeat: bool,
    ) -> bool {
        let Some(chord) = shortcut_chord_from_event(logical_key, modifiers) else {
            return false;
        };
        if repeat {
            return self.shortcut_config.action_for(&chord).is_some();
        }
        if self.fire_keyboard_triggers(&chord) {
            return true;
        }
        let Some(bound) = self.shortcut_config.action_for(&chord).cloned() else {
            return false;
        };
        if !is_shortcut_target(&bound.target) {
            self.invoke_action_chord(&bound);
        }
        // A shortcut's chord that no longer fires (its shortcut was deleted
        // or changed elsewhere) is still the shell's until the keymap
        // catches up on the next tick.
        true
    }

    fn fire_keyboard_triggers(&mut self, chord: &KeyChord) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        // Every typed key lands here. With no trigger saved anywhere there is
        // nothing to arm, and arming resolves each workspace's identity
        // (a registry stat and copy per room) only to find that out.
        let any_trigger = chrome
            .borrow()
            .tree()
            .desktop
            .workspaces
            .iter()
            .any(|workspace| {
                workspace
                    .shortcuts
                    .iter()
                    .any(|shortcut| !shortcut.triggers.is_empty())
            });
        if !any_trigger {
            return false;
        }
        let now_ns = unix_ns(self.shortcut_triggers.triggers.clock().now());
        let firings = {
            let state = chrome.borrow();
            let tree = state.tree();
            let armed = armed(
                &tree.desktop.workspaces,
                |workspace| state.local_user_for_room(&workspace.room()),
                |workspace| tree.room_revocations(workspace.room()),
                now_ns,
            );
            let firings = self
                .shortcut_triggers
                .triggers
                .observe(TriggerEvent::Key(chord), &armed);
            drop(tree);
            firings
        };
        if firings.is_empty() {
            return false;
        }
        self.start_firings(firings);
        true
    }

    /// Invoke the published action a chord is bound to, as the person this
    /// device acts for in the action's workspace (the active one for an
    /// application's action).
    fn invoke_action_chord(&self, bound: &ActionChord) {
        let (Some(chrome), Some(broker)) =
            (self.chrome.clone(), self.shortcut_triggers.broker.clone())
        else {
            return;
        };
        let caller = {
            let state = chrome.borrow();
            let tree = state.tree();
            let workspace = match workspace_provider_room(&bound.target.provider) {
                Some(room) => tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room().to_string() == room),
                None => tree.desktop.active_workspace(),
            };
            let caller = workspace.map(|workspace| {
                Caller::local_person(state.local_user_for_room(&workspace.room()))
                    .in_room(workspace.room().to_string())
            });
            drop(tree);
            caller
        };
        let Some(caller) = caller else {
            return;
        };
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: bound.target.provider.clone(),
                address: ActionAddress::new(
                    bound.target.resource.clone(),
                    bound.target.action.clone(),
                ),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        let outcome = BrokerActionHost::new(broker)
            .invoke_job(&caller, request)
            .poll_ready();
        let refusal = match outcome {
            PendingPoll::Ready(Ok(_)) => None,
            PendingPoll::Ready(Err(error)) => Some(error.to_string()),
            PendingPoll::Waiting | PendingPoll::Abandoned => {
                Some("the action broker did not admit it".to_string())
            }
        };
        if let Some(reason) = refusal {
            chrome
                .borrow_mut()
                .push_warning_notification(format!("“{}” didn't run", bound.label), reason);
        }
    }

    /// The session's application action broker, once it has bound.
    pub(super) fn action_broker(&self) -> Option<AppActionBroker> {
        self.shortcut_triggers.broker.clone()
    }

    pub(super) fn attach_shortcut_trigger_broker(&mut self, broker: AppActionBroker) {
        self.shortcut_triggers.attach(broker);
    }
}
