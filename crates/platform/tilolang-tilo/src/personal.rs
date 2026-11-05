//! A person's own state of an app's workspace: the window and lens state they set in views, and
//! what they have seen.
//!
//! The runtime holds those slots while a window is open ([`tilolang_runtime::view_state`]); this
//! resource is the record they are kept in, so what a person picked is still theirs after a
//! restart and reaches their other devices. It is their own record of the workspace,
//! `member:<person>:tilolang.personal` ([`tilolang::PERSONAL_RECORD_KIND`]), which the framework
//! opens as a person record on a route only that person's devices are admitted to. Every change
//! of it is a housekeeping change of theirs ([`Keep`]): never an undo step, and never in the
//! workspace's activity, its search or anyone else's view.
//!
//! What the record holds, under `$personal`:
//!
//! ```text
//! $personal/views/<window>/<window|lens>/<state>  the value, as JSON
//! $personal/seen/<resource>/{change,at}           the newest version the person saw, and when
//! $personal/runs/<run>                            a task run they reviewed
//! ```

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ObjType, ROOT, ReadDoc as _};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilo_actions::{Action, ActionError, ActionKey, ChangeKind, Described, Did};
use tilo_resource::{Doc, ModelError, Resource};
use tilolang_runtime::seen::SeenMark;
use tilolang_runtime::value::Value;
use tilolang_runtime::view_state::{StateKey, StateLayer};

/// The record kind of a person's own state of an app's workspace. The spelling is what records
/// on disk carry, so it stays.
pub const KIND: &str = tilolang::PERSONAL_RECORD_KIND;

/// The verb keeping one slot of window or lens state.
pub const VIEW_STATE: &str = crate::surface::VIEW_STATE;
/// The verb keeping one seen mark.
pub const SEEN: &str = crate::surface::SEEN;
/// The verb keeping the task runs a person reviewed.
pub const RUNS: &str = crate::surface::RUNS;

/// Where the record keeps its own state, apart from anything else it holds.
const PERSONAL: &str = "$personal";
const VIEWS: &str = "views";
const SEEN_MARKS: &str = "seen";
const REVIEWED: &str = "runs";

/// One slot of the person's window or lens state, as the runtime keys it and as their record
/// keeps it. The person is not part of what is kept: the record is theirs, and the runtime is
/// told whose slots these are when it is given them.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// The id of the window that declared the state.
    pub window: String,
    /// Which of the two layers the state is in.
    pub layer: StateLayer,
    /// The declaration's id.
    pub state: String,
    /// What the person set it to.
    pub value: Value,
}

impl Slot {
    /// The runtime's key for the slot, as `person`'s own.
    #[must_use]
    pub fn key(&self, person: &str) -> StateKey {
        match self.layer {
            StateLayer::Window => StateKey::window(self.window.clone(), person, self.state.clone()),
            StateLayer::Lens => StateKey::lens(self.window.clone(), person, self.state.clone()),
        }
    }
}

/// What a person's own record of an app's workspace holds: the window and lens state they set,
/// what they have seen, and the task runs they reviewed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersonalState {
    /// How their windows show their resources, and where they are in them.
    pub slots: Vec<Slot>,
    /// Per resource, the newest version they saw.
    pub seen: Vec<(String, SeenMark)>,
    /// The task runs they reviewed.
    pub runs: Vec<String>,
}

impl PersonalState {
    /// Whether the person set or saw nothing here yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty() && self.seen.is_empty() && self.runs.is_empty()
    }
}

/// A person's own state of an app's workspace, as a record the framework keeps for them.
pub struct Personal;

/// The app whose record kind this is: the tilolang host's own, since the state is of the
/// windows it runs rather than of one package.
const APP: &str = tilolang::HOST_APP;

impl Resource for Personal {
    const KIND: &'static str = KIND;
    const LABEL: &'static str = "Your place in the app";
    const APP_ID: &'static str = APP;
    type State = PersonalState;
    type Action = Keep;

    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        Ok(PersonalState {
            slots: slots(doc)?,
            seen: marks(doc)?,
            runs: reviewed(doc),
        })
    }
}

/// One piece of a person's own state of the workspace, as a change to their record keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "keep", rename_all = "kebab-case")]
pub enum Keep {
    /// `state` of the window `window` (`StateKey::window`'s id), in `layer`, is `value`:
    /// [`Value`]'s JSON form.
    View {
        window: String,
        layer: StateLayer,
        state: String,
        value: Json,
    },
    /// The person has seen `resource` up to `mark`; `None` takes the mark back.
    Seen {
        resource: String,
        mark: Option<SeenMark>,
    },
    /// The task runs the person reviewed.
    Runs { runs: Vec<String> },
}

impl Keep {
    /// The verb the change names on [`KIND`].
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::View { .. } => VIEW_STATE,
            Self::Seen { .. } => SEEN,
            Self::Runs { .. } => RUNS,
        }
    }

    /// The change keeping the slot `key` set by the person on this device.
    ///
    /// # Errors
    /// The value is not one the runtime keeps as JSON.
    pub fn view(key: &StateKey, value: &Value) -> Result<Self, String> {
        let layer = key.layer.ok_or_else(|| {
            "a reading the runtime keeps for the platform is nobody's state".to_owned()
        })?;
        let value = tilolang_runtime::value::to_json(value)?;
        Ok(Self::View {
            window: key.window.clone(),
            layer,
            state: key.state.clone(),
            value,
        })
    }
}

impl Action for Keep {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let personal = ensure_map(doc, &ROOT, PERSONAL)?;
        match self {
            Self::View {
                window,
                layer,
                state,
                value,
            } => {
                let views = ensure_map(doc, &personal, VIEWS)?;
                let place = ensure_map(doc, &views, window)?;
                let slots = ensure_map(doc, &place, layer_key(*layer))?;
                doc.put(&slots, state.as_str(), value.to_string())?;
            }
            Self::Seen { resource, mark } => {
                let seen = ensure_map(doc, &personal, SEEN_MARKS)?;
                match mark {
                    Some(mark) => {
                        let held = ensure_map(doc, &seen, resource)?;
                        doc.put(&held, "change", mark.change.as_str())?;
                        doc.put(&held, "at", mark.at.as_str())?;
                    }
                    None => {
                        doc.delete(&seen, resource.as_str())?;
                    }
                }
            }
            Self::Runs { runs } => {
                let reviewed = ensure_map(doc, &personal, REVIEWED)?;
                let held: Vec<String> = doc.keys(&reviewed).collect();
                for run in held {
                    if !runs.contains(&run) {
                        doc.delete(&reviewed, run.as_str())?;
                    }
                }
                for run in runs {
                    doc.put(&reviewed, run.as_str(), true)?;
                }
            }
        }
        Ok(())
    }

    fn describe(&self, doc: &Doc) -> Option<Described> {
        let _ = doc;
        let key = ActionKey::new(APP, KIND, self.verb());
        Some(Described::new(Did::verb(key, Vec::new())))
    }

    fn change_kind(&self) -> ChangeKind {
        ChangeKind::Housekeeping
    }
}

/// The layer a slot is kept under.
const fn layer_key(layer: StateLayer) -> &'static str {
    match layer {
        StateLayer::Window => "window",
        StateLayer::Lens => "lens",
    }
}

/// The map under `key` of `parent`, made when absent.
fn ensure_map(doc: &mut Doc, parent: &ObjId, key: &str) -> Result<ObjId, ActionError> {
    if let Some((automerge::Value::Object(ObjType::Map), id)) = doc.get(parent, key)? {
        return Ok(id);
    }
    Ok(doc.put_object(parent, key, ObjType::Map)?)
}

/// The map at `path` below the record's own state, when it is there.
fn held(doc: &Doc, path: &[&str]) -> Option<ObjId> {
    let mut at = map_at(doc, &ROOT, PERSONAL)?;
    for key in path {
        at = map_at(doc, &at, key)?;
    }
    Some(at)
}

/// The map under `key` of `parent`, when it is there.
fn map_at(doc: &Doc, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((automerge::Value::Object(ObjType::Map), id))) => Some(id),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, "personal: a map the record keeps does not read");
            None
        }
    }
}

/// The text held under `key` of `map`, when it is a text.
fn text_at(doc: &Doc, map: &ObjId, key: &str) -> Result<Option<String>, ModelError> {
    Ok(match doc.get(map, key)? {
        Some((automerge::Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
        _ => None,
    })
}

/// What one slot of view state reads back as: `None` when its value is not one the runtime
/// holds.
fn view_slot(text: &str) -> Option<Value> {
    let json: Json = serde_json::from_str(text)
        .inspect_err(|error| tracing::warn!(%error, "personal: a kept value does not parse"))
        .ok()?;
    tilolang_runtime::value::from_json(&json)
        .inspect_err(|error| {
            tracing::warn!(
                ?error,
                "personal: a kept value is not one the runtime holds"
            );
        })
        .ok()
}

/// Every slot of window and lens state the record keeps.
fn slots(doc: &Doc) -> Result<Vec<Slot>, ModelError> {
    let mut found = Vec::new();
    let Some(views) = held(doc, &[VIEWS]) else {
        return Ok(found);
    };
    for window in doc.keys(&views) {
        let Some(place) = map_at(doc, &views, &window) else {
            continue;
        };
        for layer in [StateLayer::Window, StateLayer::Lens] {
            let Some(slots_of) = map_at(doc, &place, layer_key(layer)) else {
                continue;
            };
            for state in doc.keys(&slots_of) {
                let Some(value) = text_at(doc, &slots_of, &state)?
                    .as_deref()
                    .and_then(view_slot)
                else {
                    continue;
                };
                found.push(Slot {
                    window: window.clone(),
                    layer,
                    state,
                    value,
                });
            }
        }
    }
    Ok(found)
}

/// The seen marks the record keeps, per resource.
fn marks(doc: &Doc) -> Result<Vec<(String, SeenMark)>, ModelError> {
    let mut marks = Vec::new();
    let Some(seen) = held(doc, &[SEEN_MARKS]) else {
        return Ok(marks);
    };
    for resource in doc.keys(&seen) {
        let Some(at) = map_at(doc, &seen, &resource) else {
            continue;
        };
        let mark = SeenMark {
            change: text_at(doc, &at, "change")?.unwrap_or_default(),
            at: text_at(doc, &at, "at")?.unwrap_or_default(),
        };
        marks.push((resource, mark));
    }
    Ok(marks)
}

/// The task runs the person reviewed.
fn reviewed(doc: &Doc) -> Vec<String> {
    held(doc, &[REVIEWED])
        .map(|runs| doc.keys(&runs).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(keeps: &[Keep]) -> Doc {
        let mut doc = Doc::new();
        for keep in keeps {
            keep.apply(&mut doc).expect("the change applies");
        }
        doc
    }

    #[test]
    fn a_persons_window_and_lens_state_and_seen_marks_read_back_as_they_were_kept() {
        let doc = kept(&[
            Keep::View {
                window: "tasks".to_owned(),
                layer: StateLayer::Window,
                state: "place".to_owned(),
                value: Json::String("today".to_owned()),
            },
            Keep::View {
                window: "tasks".to_owned(),
                layer: StateLayer::Lens,
                state: "scroll".to_owned(),
                value: Json::from(120),
            },
            Keep::Seen {
                resource: "tl:so.tilo.lang.tasks:inbox".to_owned(),
                mark: Some(SeenMark {
                    change: "ab".repeat(32),
                    at: "2026-11-04T12:00:00Z".to_owned(),
                }),
            },
            Keep::Runs {
                runs: vec!["run-7".to_owned()],
            },
        ]);
        let state = Personal::read(&doc).expect("the record reads");
        assert_eq!(state.slots.len(), 2);
        assert_eq!(
            state.slots[0].key("ada"),
            StateKey::window("tasks", "ada", "place")
        );
        assert_eq!(state.slots[0].value, Value::text("today"));
        assert_eq!(
            state.slots[1].key("ada"),
            StateKey::lens("tasks", "ada", "scroll")
        );
        assert_eq!(state.seen.len(), 1);
        assert_eq!(state.seen[0].0, "tl:so.tilo.lang.tasks:inbox");
        assert_eq!(state.seen[0].1.change, "ab".repeat(32));
        assert_eq!(state.runs, vec!["run-7".to_owned()]);
    }

    #[test]
    fn a_slot_kept_twice_reads_back_once_as_it_is_now() {
        let doc = kept(&[
            Keep::View {
                window: "tasks".to_owned(),
                layer: StateLayer::Window,
                state: "place".to_owned(),
                value: Json::String("inbox".to_owned()),
            },
            Keep::View {
                window: "tasks".to_owned(),
                layer: StateLayer::Window,
                state: "place".to_owned(),
                value: Json::String("today".to_owned()),
            },
        ]);
        let state = Personal::read(&doc).expect("the record reads");
        assert_eq!(state.slots.len(), 1);
        assert_eq!(state.slots[0].value, Value::text("today"));
    }

    #[test]
    fn a_reading_the_platform_keeps_is_not_the_persons_state_and_is_not_kept() {
        let key = StateKey::runtime("tasks", None, "present");
        assert!(Keep::view(&key, &Value::text("ada")).is_err());
    }

    #[test]
    fn a_record_that_kept_no_state_reads_as_none() {
        let state = Personal::read(&Doc::new()).expect("an empty record reads");
        assert!(state.is_empty(), "{state:?}");
    }

    #[test]
    fn a_taken_back_seen_mark_is_no_mark_and_reviewed_runs_replace_the_ones_before() {
        let doc = kept(&[
            Keep::Seen {
                resource: "tl:so.tilo.lang.tasks:inbox".to_owned(),
                mark: Some(SeenMark {
                    change: "cd".repeat(32),
                    at: "2026-11-04T12:00:00Z".to_owned(),
                }),
            },
            Keep::Seen {
                resource: "tl:so.tilo.lang.tasks:inbox".to_owned(),
                mark: None,
            },
            Keep::Runs {
                runs: vec!["run-1".to_owned(), "run-2".to_owned()],
            },
            Keep::Runs {
                runs: vec!["run-2".to_owned()],
            },
        ]);
        let state = Personal::read(&doc).expect("the record reads");
        assert!(state.seen.is_empty(), "{state:?}");
        assert_eq!(state.runs, vec!["run-2".to_owned()]);
    }
}
