//! The shortcuts a person saved that a trigger of this device starts: a chord pressed at its
//! keyboard, an app it started, a focus mode switching on it. These happen on this device only,
//! so it starts them itself, through the apps' broker, which checks again that the shortcut still
//! exists and that its saver may still run it. A trigger every device of the workspace can see (a
//! schedule, a record changing, a folder, a workspace joined) is the device's daemon's to watch
//! ([`tilo_workspace_service::apps::triggers`]), which places each firing on one device.
//! Only the person this device acts for has armed shortcuts
//! ([`tilo_workspace_service::apps::triggers::armed`]).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use settings_store::hardware::{ActionChord, KeyChord};
use settings_store::notification_policy::NotificationPolicy;
use tilo_automation::{FOCUS_DO_NOT_DISTURB, FOCUS_SCHEDULE, FocusTransition, TriggerSpec};
use tilo_desktop_surface::DesktopHandle;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::automation::chord_target;
use tilo_workspace_service::apps::collab_broker::AppActionBroker;
use tilo_workspace_service::apps::triggers::{Armed, Firing, StartRun as _, armed, seen_here};

pub use tilo_workspace_service::apps::automation::FinishedInvocation;

/// How often the apps running and the focus modes are looked at.
const TICK_INTERVAL: Duration = Duration::from_millis(500);

/// How many finished runs are kept for the history recorder to take.
const KEPT_RUNS: usize = 64;

/// Whether each focus mode is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusState {
    pub do_not_disturb: bool,
    pub focus: bool,
}

impl FocusState {
    /// Whether `mode` ([`FOCUS_DO_NOT_DISTURB`], [`FOCUS_SCHEDULE`]) is on.
    #[must_use]
    pub fn is_on(self, mode: &str) -> Option<bool> {
        match mode {
            FOCUS_DO_NOT_DISTURB => Some(self.do_not_disturb),
            FOCUS_SCHEDULE => Some(self.focus),
            _ => None,
        }
    }
}

/// The session's own triggers: the shortcuts it arms, what it shows them, and the runs it starts.
pub struct Automation {
    /// The apps' broker, while the session serves one: every firing starts through it.
    broker: Option<Arc<AppActionBroker>>,
    /// The apps this session started, which an application-opened trigger follows.
    launches: Arc<crate::launches::Launches>,
    /// The person's notification policy, as the session keeps it current: the focus modes a
    /// shortcut can answer.
    policy: Arc<std::sync::Mutex<NotificationPolicy>>,
    /// What each of this device's workspaces revokes, as its governance records it.
    revocations: crate::engine::Revocations,
    handle: DesktopHandle,
    /// The person at this device, as the session's own identity names them: a shortcut saved by
    /// anyone else is never armed.
    person: String,
    /// The shortcuts armed now, as the last look at the shell found them.
    armed_now: Vec<Armed>,
    /// The shell revision whose shortcuts were last armed.
    armed_at: Option<u64>,
    /// The shell revision the broker's copy was last brought level with.
    published: Option<u64>,
    /// The apps running at the last look; `None` before the first.
    running: Option<BTreeSet<String>>,
    /// The focus modes at the last look; `None` before the first.
    focus: Option<FocusState>,
    /// When the apps and focus modes were last looked at.
    last_tick: RefCell<Option<Instant>>,
    /// Runs that finished and no history recorder took yet.
    finished: RefCell<Vec<FinishedInvocation>>,
    /// Firings that came while the apps' broker was still starting: they start once it serves,
    /// rather than being lost.
    waiting: RefCell<Vec<Firing>>,
}

/// What the session shows its triggers besides its shell: the apps it starts, the person's
/// notification policy, and what each of its workspaces revokes. The workspaces' own triggers
/// fire in the device's daemon, which keeps the workspaces.
pub struct Feeds {
    pub launches: Arc<crate::launches::Launches>,
    pub policy: Arc<std::sync::Mutex<NotificationPolicy>>,
    pub revocations: crate::engine::Revocations,
}

impl Automation {
    /// The triggers of a session whose shell is `handle`.
    #[must_use]
    pub fn new(handle: DesktopHandle, person: String, feeds: Feeds) -> Self {
        let Feeds {
            launches,
            policy,
            revocations,
        } = feeds;
        Self {
            broker: None,
            launches,
            policy,
            revocations,
            handle,
            person,
            armed_now: Vec::new(),
            armed_at: None,
            published: None,
            running: None,
            focus: None,
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

    /// The broker's copy of the shell has to follow the shell the loop shows: a firing starts on
    /// the broker's thread, which checks the shortcut against that copy.
    pub(crate) fn publish_layout(&mut self, shell: Option<&crate::app_wire::LayoutAuthority>) {
        let Some(shell) = shell else {
            return;
        };
        let revision = self.handle.revision();
        if self.published == Some(revision) {
            return;
        }
        shell.publish(self.handle.read(Clone::clone));
        self.published = Some(revision);
    }

    /// Take what the broker finished, look at the apps and focus modes when it is time, and start
    /// whatever fired. Once per pass.
    pub fn settle(&mut self, shell: Option<&crate::app_wire::LayoutAuthority>) {
        self.keep_finished();
        if self.broker.is_some() {
            let waiting = std::mem::take(&mut *self.waiting.borrow_mut());
            self.start(waiting);
        }
        if self.armed_at != Some(self.handle.revision()) {
            self.arm();
            self.publish_layout(shell);
        }
        let now = Instant::now();
        let tick = self
            .last_tick
            .borrow()
            .is_none_or(|last| now.duration_since(last) >= TICK_INTERVAL);
        if !tick {
            return;
        }
        *self.last_tick.borrow_mut() = Some(now);
        let running = self.launches.running_apps();
        let mut firings = apps_opened(
            &self.armed_now,
            self.running.replace(running.clone()),
            &running,
        );
        let focus = self.focus_state();
        firings.extend(focus_switched(
            &self.armed_now,
            self.focus.replace(focus),
            focus,
        ));
        self.start(firings);
    }

    /// When this session next looks at the apps and focus modes: only while a shortcut is armed
    /// with a trigger that answers them.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let looks = self.armed_now.iter().any(|armed| {
            armed.shortcut.triggers.iter().any(|trigger| {
                matches!(
                    trigger,
                    TriggerSpec::AppOpened { .. } | TriggerSpec::FocusMode { .. }
                )
            })
        });
        if !looks {
            return None;
        }
        self.last_tick
            .borrow()
            .and_then(|last| last.checked_add(TICK_INTERVAL))
    }

    /// A chord was pressed that nothing else of the desktop's answers: it starts every shortcut
    /// whose keyboard trigger names it, once.
    pub fn press(&self, chord: &KeyChord) {
        let firings = chord_pressed(&self.armed_now, chord);
        self.start(firings);
    }

    /// The chords the armed shortcuts' keyboard triggers answer, as the desktop's shortcut table
    /// lists them: the key surface claims these, and the guide shows what each runs.
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
                        target: chord_target(&armed.workspace, &armed.shortcut.id),
                        label: format!("Run {}", armed.shortcut.name),
                    })
                })
            })
            .collect()
    }

    /// Arm the shortcuts the shell shows that the person this device acts for saved.
    fn arm(&mut self) {
        let revocations = &self.revocations;
        let person = &self.person;
        self.armed_now = self.handle.read(|tree| {
            armed(
                &tree.workspaces,
                // A workspace this device keeps arms against what its governance revokes. One it
                // does not keep is a workspace on the shell whose records live elsewhere: nothing
                // of it is known here, and the run it starts is refused by the broker unless the
                // workspace it keeps says otherwise.
                |workspace| Some(revocations.of(workspace).unwrap_or_default()),
                |workspace| crate::identity::person_in(workspace, person),
                tilo_clock::unix_now_ns(),
            )
        });
        self.armed_at = Some(self.handle.revision());
    }

    /// Which focus modes are on now, as the person's notification policy keeps them: their switch,
    /// and the schedule at this device's local time.
    fn focus_state(&self) -> FocusState {
        let local = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system());
        let minute = u16::try_from(
            i32::from(local.hour())
                .saturating_mul(60)
                .saturating_add(i32::from(local.minute())),
        )
        .unwrap_or(0);
        let weekday = u8::try_from(local.weekday().to_monday_zero_offset()).unwrap_or(0);
        let policy = self.policy.lock_recover();
        FocusState {
            do_not_disturb: policy.do_not_disturb,
            focus: policy.focus.active_until(minute, weekday).is_some(),
        }
    }

    /// Start every firing as its saver.
    fn start(&self, firings: Vec<Firing>) {
        let Some(broker) = self.broker.clone() else {
            // The broker may still be starting: what fired waits for it, and starts from the pass
            // that finds it.
            self.waiting.borrow_mut().extend(firings);
            return;
        };
        for firing in firings {
            if let Err(error) = broker.start(&firing) {
                // A shortcut deleted or changed elsewhere, or one whose saver may not run it here:
                // nothing started, and the next arming drops it.
                tracing::info!(
                    shortcut = %firing.shortcut,
                    trigger = firing.trigger.kind(),
                    %error,
                    "a shortcut trigger fired but the shortcut did not start"
                );
            }
        }
    }

    /// Keep the runs the broker finished until the history recorder takes them.
    fn keep_finished(&self) {
        let Some(broker) = &self.broker else {
            return;
        };
        let mut finished = self.finished.borrow_mut();
        finished.extend(broker.take_finished_invocations());
        if finished.len() > KEPT_RUNS {
            let excess = finished.len().saturating_sub(KEPT_RUNS);
            finished.drain(..excess);
        }
    }

    /// The shortcut runs that finished and no history recorder took yet, oldest first.
    pub fn take_finished(&self) -> Vec<FinishedInvocation> {
        std::mem::take(&mut *self.finished.borrow_mut())
    }
}

/// Every trigger of `armed` that `matches`, with its payload, firing here.
fn fire_matching<'t>(
    armed: &[Armed],
    mut matches: impl FnMut(&TriggerSpec) -> Option<Vec<(&'t str, Value)>>,
) -> Vec<Firing> {
    let mut firings = Vec::new();
    for armed in armed {
        for trigger in &armed.shortcut.triggers {
            if let Some(payload) = matches(trigger) {
                firings.push(seen_here(armed, trigger, &payload));
            }
        }
    }
    firings
}

/// The shortcuts whose keyboard trigger names `pressed`.
fn chord_pressed(armed: &[Armed], pressed: &KeyChord) -> Vec<Firing> {
    fire_matching(armed, |trigger| {
        let TriggerSpec::Keyboard { chord } = trigger else {
            return None;
        };
        let parsed = tilo_failure::ignored!(
            "a chord that does not parse matches no key",
            KeyChord::parse(chord).ok()
        );
        (parsed.as_ref() == Some(pressed)).then(Vec::new)
    })
}

/// The application-opened triggers of the apps in `running` that were not in `before`. The first
/// look (`before` is `None`) only learns what runs.
fn apps_opened(
    armed: &[Armed],
    before: Option<BTreeSet<String>>,
    running: &BTreeSet<String>,
) -> Vec<Firing> {
    let Some(before) = before else {
        return Vec::new();
    };
    running
        .difference(&before)
        .flat_map(|opened| {
            fire_matching(armed, |trigger| match trigger {
                TriggerSpec::AppOpened { app_id } if app_id == opened => {
                    Some(vec![("app_id", Value::from(opened.as_str()))])
                }
                _ => None,
            })
        })
        .collect()
}

/// The focus-mode triggers of the modes that switched from `before` to `now`. The first look
/// (`before` is `None`) only learns which are on.
fn focus_switched(armed: &[Armed], before: Option<FocusState>, now: FocusState) -> Vec<Firing> {
    let Some(before) = before.filter(|before| *before != now) else {
        return Vec::new();
    };
    fire_matching(armed, |trigger| {
        let TriggerSpec::FocusMode { mode, transition } = trigger else {
            return None;
        };
        let active = now.is_on(mode)?;
        let switched = before.is_on(mode)? != active;
        let wanted = match transition {
            FocusTransition::Enter => active,
            FocusTransition::Leave => !active,
        };
        (switched && wanted).then(|| {
            vec![
                ("mode", Value::from(mode.as_str())),
                ("active", Value::from(active)),
            ]
        })
    })
}

#[cfg(test)]
mod tests;
