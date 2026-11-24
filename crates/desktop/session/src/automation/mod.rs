//! The shortcuts a person saved that a trigger of this device starts: a chord pressed at its
//! keyboard, an app it started, a focus mode switching on it. These happen on this device only,
//! so it starts them itself. Each runs its shortcut as the person runs it from anywhere: through
//! the device's one dispatch, as them, which checks again that the shortcut still exists and that
//! its saver may still run it, and where a step that needs their yes asks them as the shortcut's
//! asked record and the run starts once they approve. A trigger every device of the workspace can
//! see (a
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
use tilo_actions::InvocationId;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{ActionProvider, InvokeRequest, RemoteActionTarget};
use tilo_automation::{FOCUS_DO_NOT_DISTURB, FOCUS_SCHEDULE, FocusTransition, TriggerSpec};
use tilo_lock::MutexRecover as _;
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_workspace_service::apps::automation::{chord_target, fired_context};
use tilo_workspace_service::apps::triggers::{Armed, Firing, armed, seen_here};

/// How often the apps running and the focus modes are looked at.
const TICK_INTERVAL: Duration = Duration::from_millis(500);

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
    /// The device's one dispatch, on the session's shell link: where every firing's shortcut runs,
    /// as the person.
    dispatch: Option<Arc<dyn ActionProvider + Send + Sync>>,
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
}

/// What the session shows its triggers besides its shell: the apps it starts, the person's
/// notification policy, and what each of its workspaces revokes. The workspaces' own triggers
/// fire in the device's daemon, which keeps the workspaces.
pub struct Feeds {
    pub launches: Arc<crate::launches::Launches>,
    pub policy: Arc<std::sync::Mutex<NotificationPolicy>>,
    pub revocations: crate::engine::Revocations,
    /// The device's one dispatch, on the session's link to its daemon, when it has one.
    pub dispatch: Option<Arc<dyn ActionProvider + Send + Sync>>,
}

impl Automation {
    /// The triggers of a session whose shell is `handle`.
    #[must_use]
    pub fn new(handle: DesktopHandle, person: String, feeds: Feeds) -> Self {
        let Feeds {
            launches,
            policy,
            revocations,
            dispatch,
        } = feeds;
        Self {
            dispatch,
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
        }
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

    /// Look at the apps and focus modes when it is time, and start whatever fired. Once per pass.
    pub fn settle(&mut self, shell: Option<&crate::app_wire::LayoutAuthority>) {
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
                // A desktop session runs its own person's shortcuts only.
                false,
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
        for firing in &firings {
            match &self.dispatch {
                Some(dispatch) => run_fired(dispatch.as_ref(), firing),
                None => {
                    tracing::warn!(shortcut = %firing.shortcut, trigger = firing.trigger.kind(), "a shortcut's trigger fired, but this session reaches no dispatch to run it");
                }
            }
        }
    }
}

/// Run the shortcut `firing` names through the device's one dispatch, `dispatch`, as the person
/// who saved it: the run they would start from anywhere, saying what started it. One that needs
/// their yes waits for it as its asked record, which they answer from any device. A shortcut
/// deleted or changed elsewhere, or one whose saver may not run it here, starts nothing, and the
/// next arming drops it.
fn run_fired(dispatch: &dyn ActionProvider, firing: &Firing) {
    let shortcut = firing.shortcut.clone();
    let trigger = firing.trigger.kind();
    dispatch
        .invoke(&firing.caller(), fired_run(firing))
        .then(move |answered| match answered {
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                tracing::info!(%shortcut, trigger, %error, "a shortcut's trigger fired but the shortcut did not start");
            }
            None => {
                tracing::info!(%shortcut, trigger, "a shortcut's trigger fired but the dispatch stopped before it started");
            }
        });
}

/// The call a firing makes: its shortcut's `run`, under the firing's id, saying what started it.
fn fired_run(firing: &Firing) -> InvokeRequest {
    let target = chord_target(&firing.workspace, &firing.shortcut);
    let mut request = InvokeRequest::plain(
        RemoteActionTarget {
            provider: target.provider,
            address: ActionAddress::new(target.resource, target.action),
        },
        firing.inputs.clone(),
    );
    request.invocation = InvocationId(firing.invocation.0);
    request.context = Some(fired_context(&firing.trigger));
    request
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
