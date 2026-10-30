//! `org.freedesktop.impl.portal.GlobalShortcuts`: an app's shortcuts that
//! work wherever the keyboard is.
//!
//! `BindShortcuts` asks the person which of the app's shortcuts to bind
//! and to which chords, a question of kind [`SHORTCUTS_QUESTION`]. Each
//! bound shortcut is published with its [`Trigger`] and the actions
//! `activate` and `deactivate`: the session's key handling claims a press
//! of the trigger and invokes them (as a script or an agent may), and
//! only then does the app hear `Activated` and `Deactivated`. The portal
//! never sees a key itself. The desktop's own chords are never granted
//! ([`crate::trigger::reserved`]), and no chord is bound to two apps.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use settings_store::hardware::ShortcutConfig;
use tilo_actions::{ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect;
use tilo_service::{Answer, Question, Started, decode, offer};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Str, Value as Variant};

use crate::reply::{Ended, OrFail, Reply, Results, answer, ensure};
use crate::session::{SessionKind, Sessions};
use crate::trigger::{Modifiers, Trigger, reserved};
use crate::{OBJECT_PATH, Portal, PortalCaller, option, question_handle};

/// The kind of the question that binds an app's shortcuts.
pub const SHORTCUTS_QUESTION: &str = "portal.global-shortcuts";
/// An answer binding every free chord the app proposed.
pub const BIND_AS_PROPOSED: usize = 0;
const SHORTCUT_PREFIX: &str = "portal:shortcut:";
const MAX_SHORTCUTS: usize = 64;
const MAX_ID_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 1024;

type ShortcutWire = (String, HashMap<String, OwnedValue>);

/// What a bound shortcut offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.portal.shortcut")]
pub enum ShortcutEffect {
    /// Press the app's shortcut
    #[effect(own_device)]
    Activate,
    /// Release the app's shortcut
    #[effect(own_device, idempotent)]
    Deactivate,
    /// Unbind the app's shortcut
    #[effect(own_device, idempotent)]
    Unbind,
}

/// A shortcut an app asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Requested {
    id: String,
    description: Option<String>,
    preferred: Option<String>,
}

/// A shortcut bound to a chord.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bound {
    id: String,
    description: Option<String>,
    trigger: Trigger,
}

impl Bound {
    fn wire(&self) -> ShortcutWire {
        let mut options = HashMap::new();
        if let Some(description) = &self.description {
            options.insert(
                "description".into(),
                OwnedValue::from(Str::from(description.as_str())),
            );
        }
        options.insert(
            "trigger_description".into(),
            OwnedValue::from(Str::from(self.trigger.display())),
        );
        (self.id.clone(), options)
    }
}

/// One row of the binding sheet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutRow {
    /// What the answer names it by in `uris`: `<id>` binds it to
    /// `trigger`, `<id>=<chord>` to the chord the person chose.
    pub id: String,
    /// The app's words for it.
    pub description: Option<String>,
    /// The chord proposed, as `Ctrl + Shift + K`.
    pub trigger: Option<String>,
    /// Why the proposed chord cannot be had: `reserved` (the desktop's),
    /// `taken` (another app's, named in `taken_by`).
    pub status: Option<String>,
    pub taken_by: Option<String>,
}

/// What the sheet shows for binding shortcuts. Answering with choice
/// [`BIND_AS_PROPOSED`] binds every row whose chord is free.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutsQuestion {
    pub shortcuts: Vec<ShortcutRow>,
}

#[derive(Clone, Default)]
struct Session {
    bound: Vec<Bound>,
    binding: bool,
}

#[derive(Default)]
struct State {
    sessions: BTreeMap<String, Session>,
    /// The shortcuts pressed and not yet released, by resource.
    held: HashSet<String>,
    table: ShortcutConfig,
}

/// The bound shortcuts, by session path.
#[derive(Clone, Default)]
pub struct Shortcuts {
    state: Arc<Mutex<State>>,
    sessions: Sessions,
}

fn resource_of(session: &str, id: &str) -> String {
    format!("{SHORTCUT_PREFIX}{session}\u{1f}{id}")
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

impl Shortcuts {
    /// Shortcuts of the sessions in `sessions`, gone when theirs end.
    #[must_use]
    pub fn new(sessions: &Sessions) -> Self {
        let shortcuts = Self {
            state: Arc::default(),
            sessions: sessions.clone(),
        };
        let ended = Arc::clone(&shortcuts.state);
        sessions.on_end(move |path| {
            let mut state = ended.lock_recover();
            state.sessions.remove(path);
            let prefix = resource_of(path, "");
            state.held.retain(|held| !held.starts_with(&prefix));
        });
        shortcuts
    }

    /// The desktop's shortcut table, whose chords no app is granted. A
    /// binding that now collides with it is let go.
    pub fn set_shortcut_table(&self, table: ShortcutConfig) {
        let mut state = self.state.lock_recover();
        for session in state.sessions.values_mut() {
            session
                .bound
                .retain(|bound| !reserved(&bound.trigger, &table));
        }
        state.table = table;
        drop(state);
        self.sessions.changed();
    }

    /// The resource of the shortcut bound to a press of `keycode` with
    /// `modifiers`, if an app has one: the session's key handling invokes
    /// its `activate` on the press and `deactivate` on the release.
    #[must_use]
    pub fn triggered(&self, keycode: u32, modifiers: Modifiers) -> Option<String> {
        let state = self.state.lock_recover();
        state.sessions.iter().find_map(|(path, session)| {
            session
                .bound
                .iter()
                .find(|bound| bound.trigger.matches(keycode, modifiers))
                .map(|bound| resource_of(path, &bound.id))
        })
    }

    /// The rows of a binding sheet for `requested` of session `path`.
    fn rows(&self, path: &str, requested: &[Requested]) -> Vec<ShortcutRow> {
        let state = self.state.lock_recover();
        requested
            .iter()
            .map(|shortcut| {
                let trigger = shortcut.preferred.as_deref().and_then(Trigger::parse);
                let taken_by = trigger
                    .as_ref()
                    .and_then(|trigger| self.taken_by(&state, path, trigger));
                let status = trigger.as_ref().and_then(|trigger| {
                    if reserved(trigger, &state.table) {
                        Some("reserved".to_owned())
                    } else {
                        taken_by.as_ref().map(|_| "taken".to_owned())
                    }
                });
                ShortcutRow {
                    id: shortcut.id.clone(),
                    description: shortcut.description.clone(),
                    trigger: trigger.map(|trigger| trigger.display()),
                    status,
                    taken_by,
                }
            })
            .collect()
    }

    /// The app another session bound `trigger` for.
    fn taken_by(&self, state: &State, path: &str, trigger: &Trigger) -> Option<String> {
        state
            .sessions
            .iter()
            .filter(|(other, _)| other.as_str() != path)
            .find(|(_, session)| {
                session
                    .bound
                    .iter()
                    .any(|bound| bound.trigger.canonical == trigger.canonical)
            })
            .map(|(other, _)| {
                self.sessions
                    .get(other)
                    .map_or_else(String::new, |live| live.caller.app_id)
            })
    }

    /// The bindings `answer` makes of `requested` for session `path`: only
    /// requested ids, chords that parse, are free and are not given twice.
    fn bindings(&self, path: &str, requested: &[Requested], answer: &Answer) -> Vec<Bound> {
        let chosen: Vec<(String, Option<String>)> = if answer.choice == Some(BIND_AS_PROPOSED) {
            requested
                .iter()
                .map(|shortcut| (shortcut.id.clone(), None))
                .collect()
        } else {
            answer
                .uris
                .iter()
                .map(|entry| match entry.split_once('=') {
                    Some((id, chord)) => (id.to_owned(), Some(chord.to_owned())),
                    None => (entry.clone(), None),
                })
                .collect()
        };
        let state = self.state.lock_recover();
        let mut bound: Vec<Bound> = Vec::new();
        for (id, chord) in chosen {
            let Some(shortcut) = requested.iter().find(|shortcut| shortcut.id == id) else {
                continue;
            };
            let Some(trigger) = chord
                .as_deref()
                .or(shortcut.preferred.as_deref())
                .and_then(Trigger::parse)
            else {
                continue;
            };
            let free = !reserved(&trigger, &state.table)
                && self.taken_by(&state, path, &trigger).is_none()
                && !bound
                    .iter()
                    .any(|other| other.id == id || other.trigger.canonical == trigger.canonical);
            if free {
                bound.push(Bound {
                    id,
                    description: shortcut.description.clone(),
                    trigger,
                });
            }
        }
        drop(state);
        bound
    }

    /// Start binding session `path`; `false` while it already is.
    fn begin(&self, path: &str) -> bool {
        !std::mem::replace(
            &mut self
                .state
                .lock_recover()
                .sessions
                .entry(path.to_owned())
                .or_default()
                .binding,
            true,
        )
    }

    /// Session `path` bound `bound` (or, `None`, its binding ended without).
    fn finish(&self, path: &str, bound: Option<Vec<Bound>>) -> bool {
        let mut state = self.state.lock_recover();
        let Some(session) = state.sessions.get_mut(path) else {
            return false;
        };
        session.binding = false;
        if let Some(bound) = bound {
            session.bound = bound;
        }
        drop(state);
        self.sessions.changed();
        true
    }

    fn bound(&self, path: &str) -> Vec<Bound> {
        self.state
            .lock_recover()
            .sessions
            .get(path)
            .map(|session| session.bound.clone())
            .unwrap_or_default()
    }

    fn all(&self) -> Vec<(String, Bound)> {
        self.state
            .lock_recover()
            .sessions
            .iter()
            .flat_map(|(path, session)| {
                session
                    .bound
                    .iter()
                    .map(move |bound| (path.clone(), bound.clone()))
            })
            .collect()
    }

    fn reading_of(&self, path: &str, bound: &Bound) -> Value {
        let app_id = self.sessions.get(path).map(|live| live.caller.app_id);
        serde_json::json!({
            "app_id": app_id,
            "id": bound.id,
            "description": bound.description,
            "trigger": bound.trigger,
            "held": self.state.lock_recover().held.contains(&resource_of(path, &bound.id)),
        })
    }

    /// Emit `edge` of shortcut `id` of session `path` to its app.
    fn emit(&self, path: &str, id: &str, activated: bool) {
        let Some(connection) = self.sessions.connection() else {
            return;
        };
        let (path, id) = (path.to_owned(), id.to_owned());
        self.sessions.spawn(async move {
            let (Ok(emitter), Ok(session)) = (
                SignalEmitter::new(&connection, OBJECT_PATH),
                ObjectPath::try_from(path.as_str()),
            ) else {
                return;
            };
            let timestamp = unix_millis();
            let _ = if activated {
                GlobalShortcuts::activated(&emitter, session, id, timestamp, HashMap::new()).await
            } else {
                GlobalShortcuts::deactivated(&emitter, session, id, timestamp, HashMap::new()).await
            };
        });
    }

    fn emit_changed(&self, path: &str) {
        let Some(connection) = self.sessions.connection() else {
            return;
        };
        let wire: Vec<ShortcutWire> = self.bound(path).iter().map(Bound::wire).collect();
        let path = path.to_owned();
        self.sessions.spawn(async move {
            let (Ok(emitter), Ok(session)) = (
                SignalEmitter::new(&connection, OBJECT_PATH),
                ObjectPath::try_from(path.as_str()),
            ) else {
                return;
            };
            let _ = GlobalShortcuts::shortcuts_changed(&emitter, session, wire).await;
        });
    }
}

impl crate::Published for Shortcuts {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.all()
            .into_iter()
            .map(|(path, bound)| {
                let app_id = self
                    .sessions
                    .get(&path)
                    .map_or_else(String::new, |live| live.caller.app_id);
                let label = bound
                    .description
                    .clone()
                    .unwrap_or_else(|| bound.id.clone());
                offer::<ShortcutEffect>(ResourceSpec::new(
                    resource_of(&path, &bound.id),
                    "portal.shortcut",
                    label,
                ))
                .with_presentation(ResourcePresentation {
                    detail: Some(bound.trigger.display()),
                    badge: Some(app_id),
                    preview: None,
                })
            })
            .collect()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let (path, id) = resource
            .strip_prefix(SHORTCUT_PREFIX)?
            .split_once('\u{1f}')?;
        let bound = self.bound(path).into_iter().find(|bound| bound.id == id)?;
        Some(self.reading_of(path, &bound))
    }

    fn invoke(
        &self,
        published: &[ResourceSpec],
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Option<Result<Started, DoorError>> {
        let (path, id) = resource
            .strip_prefix(SHORTCUT_PREFIX)?
            .split_once('\u{1f}')?;
        let unknown = || DoorError::Unknown(tilo_actions::ActionAddress::new(resource, action));
        Some(
            decode::<ShortcutEffect>(published, resource, action, arguments).and_then(|effect| {
                let mut state = self.state.lock_recover();
                let bound = state
                    .sessions
                    .get(path)
                    .is_some_and(|session| session.bound.iter().any(|bound| bound.id == id));
                if !bound {
                    return Err(unknown());
                }
                match effect {
                    ShortcutEffect::Activate => {
                        // A key held down is one press, however often it
                        // is offered.
                        if state.held.insert(resource.to_owned()) {
                            drop(state);
                            self.emit(path, id, true);
                        }
                    }
                    ShortcutEffect::Deactivate => {
                        if state.held.remove(resource) {
                            drop(state);
                            self.emit(path, id, false);
                        }
                    }
                    ShortcutEffect::Unbind => {
                        if let Some(session) = state.sessions.get_mut(path) {
                            session.bound.retain(|bound| bound.id != id);
                        }
                        let was_held = state.held.remove(resource);
                        drop(state);
                        if was_held {
                            self.emit(path, id, false);
                        }
                        self.emit_changed(path);
                        self.sessions.changed();
                    }
                }
                Ok(Started::Done(ActionOutput::default()))
            }),
        )
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_text(value: &str) -> bool {
    value.len() <= MAX_TEXT_BYTES && !value.chars().any(char::is_control)
}

/// The shortcuts of a `BindShortcuts` call: bounded, unique, plain ids and
/// text.
fn requested(wire: Vec<ShortcutWire>) -> Option<Vec<Requested>> {
    if wire.len() > MAX_SHORTCUTS {
        return None;
    }
    let mut ids = HashSet::new();
    wire.into_iter()
        .map(|(id, options)| {
            if !valid_id(&id) || !ids.insert(id.clone()) {
                return None;
            }
            let description = option::<String>(&options, "description");
            let preferred = option::<String>(&options, "preferred_trigger");
            if description
                .iter()
                .chain(preferred.iter())
                .any(|text| !valid_text(text))
            {
                return None;
            }
            Some(Requested {
                id,
                description,
                preferred,
            })
        })
        .collect()
}

fn shortcuts_value(bound: &[Bound]) -> Option<OwnedValue> {
    OwnedValue::try_from(Variant::new(
        bound.iter().map(Bound::wire).collect::<Vec<_>>(),
    ))
    .ok()
}

/// The `org.freedesktop.impl.portal.GlobalShortcuts` interface.
pub struct GlobalShortcuts {
    portal: Portal,
}

impl GlobalShortcuts {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }

    fn shortcuts(&self) -> &Shortcuts {
        &self.portal.shortcuts
    }

    /// Ask the person to bind `requested` for session `path` under
    /// `handle`; the bindings made.
    async fn ask(
        &self,
        handle: &str,
        path: &str,
        caller: &PortalCaller,
        requested: &[Requested],
    ) -> Result<Vec<Bound>, Ended> {
        let question = ShortcutsQuestion {
            shortcuts: self.shortcuts().rows(path, requested),
        };
        let answer = self
            .portal
            .questions
            .ask(Question {
                handle: handle.to_owned(),
                kind: SHORTCUTS_QUESTION.into(),
                app_id: Some(caller.app_id.clone()),
                label: caller.app_id.clone(),
                details: serde_json::to_value(question).unwrap_or_default(),
            })
            .await
            .ok_or(Ended::Cancelled)?;
        Ok(self.shortcuts().bindings(path, requested, &answer))
    }
}

#[interface(name = "org.freedesktop.impl.portal.GlobalShortcuts")]
impl GlobalShortcuts {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    async fn create_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let auth = self
                .portal
                .authenticate(&header, handle.as_str(), &app_id, None)
                .await?;
            let path = session_handle.as_str();
            self.portal
                .sessions
                .create(
                    connection,
                    path,
                    auth.owner.as_str(),
                    auth.caller,
                    SessionKind::GlobalShortcuts,
                )
                .await?;
            self.shortcuts()
                .state
                .lock_recover()
                .sessions
                .insert(path.to_owned(), Session::default());
            Ok(Results::new())
        })
        .await
    }

    async fn bind_shortcuts(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        shortcuts: Vec<ShortcutWire>,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let path = session_handle.as_str();
            let (owner, session) = self.portal.authenticate_session(&header, path, "").await?;
            ensure(session.kind == SessionKind::GlobalShortcuts && valid_text(&parent_window))?;
            let requested = requested(shortcuts).or_fail()?;
            ensure(self.shortcuts().begin(path))?;
            let request = handle.as_str();
            let bound = self
                .portal
                .requests
                .run(
                    connection,
                    request,
                    owner.as_str(),
                    self.ask(&question_handle(request), path, &session.caller, &requested),
                )
                .await;
            let bound = match bound {
                Ok(bound) => bound,
                Err(ended) => {
                    self.shortcuts().finish(path, None);
                    return Err(ended);
                }
            };
            ensure(self.shortcuts().finish(path, Some(bound.clone())))?;
            self.shortcuts().emit_changed(path);
            let mut results = Results::new();
            results.insert("shortcuts".into(), shortcuts_value(&bound).or_fail()?);
            Ok(results)
        })
        .await
    }

    async fn list_shortcuts(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
    ) -> Reply {
        answer(async {
            let path = session_handle.as_str();
            let (_, session) = self.portal.authenticate_session(&header, path, "").await?;
            ensure(session.kind == SessionKind::GlobalShortcuts)?;
            let mut results = Results::new();
            results.insert(
                "shortcuts".into(),
                shortcuts_value(&self.shortcuts().bound(path)).or_fail()?,
            );
            Ok(results)
        })
        .await
    }

    /// Let the person change the chords of the session's shortcuts.
    async fn configure_shortcuts(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_handle: ObjectPath<'_>,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let path = session_handle.as_str();
        let (_, session) = self
            .portal
            .authenticate_session(&header, path, "")
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted portal caller".into()))?;
        if session.kind != SessionKind::GlobalShortcuts || !valid_text(&parent_window) {
            return Err(zbus::fdo::Error::InvalidArgs("no shortcuts session".into()));
        }
        if !self.shortcuts().begin(path) {
            return Err(zbus::fdo::Error::Failed(
                "the shortcuts are being bound".into(),
            ));
        }
        let requested: Vec<Requested> = self
            .shortcuts()
            .bound(path)
            .into_iter()
            .map(|bound| Requested {
                id: bound.id,
                description: bound.description,
                preferred: Some(bound.trigger.display()),
            })
            .collect();
        let handle = format!("portal:{path}:configure");
        // A session closed meanwhile withdraws the question: its answer
        // then binds nothing.
        let bound = self.ask(&handle, path, &session.caller, &requested).await;
        let Ok(bound) = bound else {
            self.shortcuts().finish(path, None);
            return Err(zbus::fdo::Error::Failed("shortcut setup cancelled".into()));
        };
        if self.shortcuts().finish(path, Some(bound)) {
            self.shortcuts().emit_changed(path);
        }
        Ok(())
    }

    #[zbus(signal)]
    async fn activated(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        shortcut_id: String,
        timestamp: u64,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        shortcut_id: String,
        timestamp: u64,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn shortcuts_changed(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        shortcuts: Vec<ShortcutWire>,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use settings_store::hardware::{KeyChord, ShortcutAction};

    use crate::Published as _;

    use super::*;

    fn requests() -> Vec<Requested> {
        vec![
            Requested {
                id: "talk".into(),
                description: Some("Push to talk".into()),
                preferred: Some("Ctrl+Shift+P".into()),
            },
            Requested {
                id: "search".into(),
                description: None,
                preferred: Some("Ctrl+K".into()),
            },
            Requested {
                id: "mute".into(),
                description: None,
                preferred: None,
            },
        ]
    }

    #[test]
    fn requested_shortcuts_are_bounded_and_unique() {
        let good = vec![("toggle".into(), HashMap::new())];
        assert_eq!(requested(good).expect("valid").len(), 1);
        let duplicate = vec![
            ("toggle".into(), HashMap::new()),
            ("toggle".into(), HashMap::new()),
        ];
        assert!(requested(duplicate).is_none());
        assert!(requested(vec![("bad space".into(), HashMap::new())]).is_none());
    }

    #[test]
    fn an_answer_binds_free_chords_once_and_never_the_desktops() {
        let shortcuts = Shortcuts::default();
        let rows = shortcuts.rows("/s/a", &requests());
        assert_eq!(rows[1].status.as_deref(), Some("reserved"));
        assert_eq!(rows[2].trigger, None);
        let as_proposed = Answer {
            choice: Some(BIND_AS_PROPOSED),
            ..Answer::default()
        };
        let bound = shortcuts.bindings("/s/a", &requests(), &as_proposed);
        assert_eq!(
            bound
                .iter()
                .map(|bound| bound.id.as_str())
                .collect::<Vec<_>>(),
            ["talk"],
            "the reserved and the chordless are left out"
        );
        let chosen = Answer {
            uris: vec![
                "mute=Ctrl+Alt+M".into(),
                "search=Ctrl+Alt+M".into(),
                "unknown=Ctrl+Alt+U".into(),
            ],
            ..Answer::default()
        };
        let bound = shortcuts.bindings("/s/a", &requests(), &chosen);
        assert_eq!(bound.len(), 1, "a chord is given once");
        assert_eq!(
            (bound[0].id.as_str(), bound[0].trigger.canonical.as_str()),
            ("mute", "CTRL+ALT+KEYM")
        );
    }

    #[test]
    fn a_chord_bound_by_one_session_is_taken_for_another() {
        let shortcuts = Shortcuts::default();
        assert!(shortcuts.begin("/s/a"));
        let as_proposed = Answer {
            choice: Some(BIND_AS_PROPOSED),
            ..Answer::default()
        };
        let bound = shortcuts.bindings("/s/a", &requests(), &as_proposed);
        assert!(shortcuts.finish("/s/a", Some(bound)));
        let rows = shortcuts.rows("/s/b", &requests());
        assert_eq!(rows[0].status.as_deref(), Some("taken"));
        assert!(
            shortcuts
                .bindings("/s/b", &requests(), &as_proposed)
                .is_empty()
        );
    }

    #[test]
    fn a_bound_shortcut_is_published_with_its_trigger_and_presses_once() {
        let shortcuts = Shortcuts::default();
        assert!(shortcuts.begin("/s/a"));
        let as_proposed = Answer {
            choice: Some(BIND_AS_PROPOSED),
            ..Answer::default()
        };
        let bound = shortcuts.bindings("/s/a", &requests(), &as_proposed);
        assert!(shortcuts.finish("/s/a", Some(bound)));
        let published = shortcuts.resources();
        assert_eq!(published.len(), 1);
        let resource = published[0].id.as_str().to_owned();
        let reading = shortcuts.read(&resource).expect("read");
        assert_eq!(reading["trigger"]["keycode"], 25);
        assert_eq!(reading["trigger"]["ctrl"], true);
        for _ in 0..2 {
            let pressed = shortcuts.invoke(&published, &resource, "activate", &Map::new());
            assert!(matches!(pressed, Some(Ok(_))), "{pressed:?}");
        }
        assert_eq!(shortcuts.state.lock_recover().held.len(), 1);
        let released = shortcuts.invoke(&published, &resource, "deactivate", &Map::new());
        assert!(matches!(released, Some(Ok(_))));
        assert!(shortcuts.state.lock_recover().held.is_empty());
        let unbound = shortcuts.invoke(&published, &resource, "unbind", &Map::new());
        assert!(matches!(unbound, Some(Ok(_))));
        assert!(shortcuts.resources().is_empty());
    }

    #[test]
    fn a_press_goes_to_the_shortcut_bound_to_its_chord() {
        let shortcuts = Shortcuts::default();
        assert!(shortcuts.begin("/s/a"));
        let as_proposed = Answer {
            choice: Some(BIND_AS_PROPOSED),
            ..Answer::default()
        };
        let bound = shortcuts.bindings("/s/a", &requests(), &as_proposed);
        assert!(shortcuts.finish("/s/a", Some(bound)));
        // `talk` was bound to Ctrl+Shift+P, which is keycode 25.
        let chord = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(
            shortcuts.triggered(25, chord),
            Some(resource_of("/s/a", "talk"))
        );
        // The same key with other modifiers, or another key, is the app's
        // own to type.
        assert_eq!(
            shortcuts.triggered(
                25,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                }
            ),
            None
        );
        assert_eq!(shortcuts.triggered(24, chord), None);
        // The desktop's own chords win: a binding that collides is let go.
        shortcuts.set_shortcut_table(ShortcutConfig {
            bindings: BTreeMap::from([(
                ShortcutAction::WorkspaceOverview,
                KeyChord::parse("Ctrl+Shift+P").expect("a chord"),
            )]),
            ..ShortcutConfig::default()
        });
        assert_eq!(shortcuts.triggered(25, chord), None);
    }
}
