//! A person's own state of an app's workspace: the view state they set in views, and what they
//! have seen.
//!
//! The runtime holds those slots while a window is open ([`tilolang_runtime::view_state`]); this
//! resource is the record they are kept in, so what a person picked is still theirs after a
//! restart and reaches their other devices. It is the workspace's record of the kind
//! `tilolang.personal` ([`tilolang::PERSONAL_RECORD_KIND`]), kept by the language's host app
//! ([`OWNER`]) and opened by every tilolang app's window: one record per workspace, synced to
//! every member like any record and decided by the workspace's grants like any record's changes.
//!
//! Each person's entries are theirs, under `$personal/<person>`:
//!
//! ```text
//! $personal/<person>/views/<window>/<state>                the value, as JSON
//! $personal/<person>/seen/<resource>/{change,at}           the newest version they saw, and when
//! $personal/<person>/runs/<run>                            a run they reviewed
//! ```
//!
//! A record kept while a view's state was still split into two kinds holds each value one map
//! deeper, under `<window>/<window|lens>/<state>`; it reads as the same state, and the place
//! this build keeps wins where it holds one ([`Keep::View`]).
//!
//! Only the person an entry is about writes it: [`Keep::apply`] refuses a change whose invoking
//! actor is not the entry's person, so a window keeps its own person's state and nobody
//! else's. Others read the entries — a window draws another person's place as presence — and
//! their changes arrive the way any change of theirs does. Every change is a housekeeping
//! change ([`Keep::change_kind`]): never an undo step, and never in the workspace's activity or
//! its search.

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ObjType, ROOT, ReadDoc as _};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilo_actions::{Action, ActionError, ActionKey, ChangeKind, Described, Did};
use tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use tilolang_runtime::seen::SeenMark;
use tilolang_runtime::value::Value;
use tilolang_runtime::view_state::StateKey;

/// The record kind of the workspace's record of each person's own state. The spelling is what
/// records on disk carry, so it stays.
pub const KIND: &str = tilolang::PERSONAL_RECORD_KIND;

/// The app that keeps the record: the tilolang host's own, since the state is of the windows it
/// runs rather than of one package, and every package of a workspace shares the one record.
pub const OWNER: &str = tilolang::HOST_APP;

/// The verb keeping one slot of view state.
pub const VIEW_STATE: &str = "view-state";
/// The verb keeping one seen mark.
pub const SEEN: &str = "seen";
/// The verb keeping the runs a person reviewed.
pub const RUNS: &str = "seen-runs";
/// The verb a delivered reminder was kept by. Retired with the reminder scheduler: a reminder is
/// now an effect an app schedules through `uses`, so the record no longer keeps deliveries.
pub const REMINDED: &str = "reminded";
/// Where the record keeps its own state, apart from anything else it holds.
const PERSONAL: &str = "$personal";
const VIEWS: &str = "views";
const SEEN_MARKS: &str = "seen";
const REVIEWED: &str = "runs";

/// One slot of the person's view state, as the runtime keys it and as their record keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// Whose state it is.
    pub person: String,
    /// The id of the window that declared the state.
    pub window: String,
    /// The declaration's id.
    pub state: String,
    /// What the person set it to.
    pub value: Value,
}

impl Slot {
    /// The runtime's key for the slot.
    #[must_use]
    pub fn key(&self) -> StateKey {
        StateKey::view(self.window.clone(), self.person.clone(), self.state.clone())
    }
}

/// The newest version of a resource one person has seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// Whose mark it is.
    pub person: String,
    /// The resource, as [`tilolang_runtime::value::ResourceRef::id_string`] writes it.
    pub resource: String,
    /// The version they saw, and when.
    pub mark: SeenMark,
}

/// One run a person reviewed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reviewed {
    /// Whose review it is.
    pub person: String,
    /// The run's id.
    pub run: String,
}

/// What the workspace's record of each person's own state holds: the window and lens state they
/// set, what they have seen, and the runs they reviewed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersonalState {
    /// How their windows show their resources, and where they are in them.
    pub slots: Vec<Slot>,
    /// Per resource, the newest version they saw.
    pub seen: Vec<Seen>,
    /// The runs they reviewed.
    pub runs: Vec<Reviewed>,
}

impl PersonalState {
    /// Everything `person` holds here, and nothing of anyone else's.
    #[must_use]
    pub fn of(&self, person: &str) -> Self {
        let theirs = |who: &str| who.eq_ignore_ascii_case(person);
        Self {
            slots: self
                .slots
                .iter()
                .filter(|slot| theirs(&slot.person))
                .cloned()
                .collect(),
            seen: self
                .seen
                .iter()
                .filter(|seen| theirs(&seen.person))
                .cloned()
                .collect(),
            runs: self
                .runs
                .iter()
                .filter(|reviewed| theirs(&reviewed.person))
                .cloned()
                .collect(),
        }
    }

    /// Whether nobody set or saw anything here yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty() && self.seen.is_empty() && self.runs.is_empty()
    }
}

/// The workspace's record of each person's own state of an app's windows, as the framework
/// keeps it.
pub struct Personal;

impl Resource for Personal {
    const KIND: &'static str = KIND;
    const LABEL: &'static str = "Your place in the app";
    const APP_ID: &'static str = OWNER;
    type State = PersonalState;
    type Action = Keep;

    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        Ok(PersonalState {
            slots: slots(doc),
            seen: marks(doc)?,
            runs: reviewed(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        [
            (VIEW_STATE, "Keep a view's state"),
            (SEEN, "Keep what is seen"),
            (RUNS, "Keep reviewed runs"),
            (REMINDED, "Keep a reminder delivered"),
        ]
        .into_iter()
        .map(|(verb, label)| declaration(verb, label))
        .collect()
    }
}

/// The declaration of the verb `verb`: the one change a window of the person's keeps as `verb`,
/// by name, for the workspaces whose grants decide each change against what the app declares.
fn declaration(verb: &'static str, label: &'static str) -> ActionDecl<Personal> {
    let keep = tilo_actions::ActionParam::required_owned(
        KEEP,
        tilo_actions::ParamType::Object,
        "The entry the person keeps",
    );
    ActionDecl::new(verb, label, vec![keep], move |args, _now| {
        let Some(value) = args.to_json().remove(KEEP) else {
            return Err(format!("`{KEEP}` is missing"));
        };
        let keep: Keep =
            serde_json::from_value(value).map_err(|error| format!("`{KEEP}`: {error}"))?;
        if keep.verb() != verb {
            return Err(format!("`{KEEP}`: not a {verb}"));
        }
        Ok(keep)
    })
}

/// The parameter the one change of a verb is bound to.
const KEEP: &str = "keep";

/// One piece of a person's own state of the workspace, as a change to the record keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "keep", rename_all = "kebab-case")]
pub enum Keep {
    /// `state` of the window `window` (`StateKey::view`'s id) is `value`: [`Value`]'s JSON
    /// form, kept as `person`'s.
    View {
        person: String,
        window: String,
        state: String,
        value: Json,
    },
    /// `person` has seen `resource` up to `mark`; `None` takes the mark back.
    Seen {
        person: String,
        resource: String,
        mark: Option<SeenMark>,
    },
    /// The runs `person` reviewed.
    Runs { person: String, runs: Vec<String> },
    /// `person` was told the reminder `reminder`: no device of theirs tells it again. Retired
    /// with the platform's reminder scheduler; the variant stays so entries written by it still
    /// read, and keeping one now keeps nothing.
    Reminded { person: String, reminder: String },
}

impl Keep {
    /// The verb the change names on [`KIND`].
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::View { .. } => VIEW_STATE,
            Self::Seen { .. } => SEEN,
            Self::Runs { .. } => RUNS,
            Self::Reminded { .. } => REMINDED,
        }
    }

    /// Whose state the change keeps.
    #[must_use]
    pub fn person(&self) -> &str {
        match self {
            Self::View { person, .. }
            | Self::Seen { person, .. }
            | Self::Runs { person, .. }
            | Self::Reminded { person, .. } => person,
        }
    }

    /// The change keeping the slot `key` set on this device.
    ///
    /// # Errors
    /// The slot is nobody's, or its value is not one the runtime keeps as JSON.
    pub fn view(key: &StateKey, value: &Value) -> Result<Self, String> {
        if !key.declared {
            return Err(
                "a reading the runtime keeps for the platform is nobody's state".to_owned(),
            );
        }
        let person = key
            .principal
            .clone()
            .ok_or_else(|| "a reading no person set is not a person's state".to_owned())?;
        let value = tilolang_runtime::value::to_json(value)?;
        Ok(Self::View {
            person,
            window: key.window.clone(),
            state: key.state.clone(),
            value,
        })
    }
}

/// The person whose action applies now: the actor the host admitted with it. `None` outside an
/// action's apply, where no person is acting.
fn acting() -> Option<String> {
    let invoker = tilo_actions::invoking_context()?.invoker?;
    Some(invoker.on_behalf_of.unwrap_or(invoker.actor))
}

impl Action for Keep {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // A person's own entries are only ever theirs to write: the actor the host admitted
        // with the change, pinned in its envelope, must be the person it is about.
        if acting().as_deref() != Some(self.person()) {
            return Err(ActionError::Refused(
                "only a person keeps their own state".to_owned(),
            ));
        }
        let person = ensure_map(doc, &ROOT, PERSONAL)?;
        let person = ensure_map(doc, &person, self.person())?;
        match self {
            Self::View {
                window,
                state,
                value,
                ..
            } => {
                let views = ensure_map(doc, &person, VIEWS)?;
                let place = ensure_map(doc, &views, window)?;
                doc.put(&place, state.as_str(), value.to_string())?;
            }
            Self::Seen { resource, mark, .. } => {
                let seen = ensure_map(doc, &person, SEEN_MARKS)?;
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
            Self::Runs { runs, .. } => {
                let reviewed = ensure_map(doc, &person, REVIEWED)?;
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
            // Nothing tells a reminder any more, so there is nothing left to keep: an entry
            // written when one was told reads, and applies nothing.
            Self::Reminded { .. } => {}
        }
        Ok(())
    }

    fn describe(&self, doc: &Doc) -> Option<Described> {
        let _ = doc;
        let key = ActionKey::new(OWNER, KIND, self.verb());
        Some(Described::new(Did::verb(key, Vec::new())))
    }

    fn change_kind(&self) -> ChangeKind {
        ChangeKind::Housekeeping
    }
}

/// The maps a record kept a window's state in while a view's state was still split into two
/// kinds: how its window showed the resource, and where its person was in it. The names are
/// what those records carry, so they stay.
const SPLIT: [&str; 2] = ["window", "lens"];

/// The map under `key` of `parent`, made when absent; the copies of it a replica made before it
/// met the others are folded into the one the record keeps ([`crate::containers`]).
fn ensure_map(doc: &mut Doc, parent: &ObjId, key: &str) -> Result<ObjId, ActionError> {
    crate::containers::single(doc, parent, key)
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

/// The maps `parent` holds at `key`, the one the record keeps first: a replica that kept a map
/// of its own before it met the others wrote into a copy the record does not keep, and what it
/// wrote is the person's all the same ([`crate::containers`]).
fn kept_first(doc: &Doc, parent: &ObjId, key: &str) -> Vec<ObjId> {
    let mut held = crate::containers::maps(doc, parent, key);
    held.reverse();
    held
}

/// The people the record holds any state of, each with a copy of their map, the one the record
/// keeps first: a person's map is made twice when two devices of theirs wrote it at once.
fn people(doc: &Doc) -> Vec<(String, ObjId)> {
    let mut people = Vec::new();
    for personal in kept_first(doc, &ROOT, PERSONAL) {
        for person in doc.keys(&personal) {
            people.extend(
                kept_first(doc, &personal, &person)
                    .into_iter()
                    .map(|held| (person.clone(), held)),
            );
        }
    }
    people
}

/// The states a window keeps in `place`: the place this build keeps them at, then the two maps
/// a record kept before it did, each state once and the newer place first.
fn place_states(doc: &Doc, place: &ObjId) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    let collect = |slots_of: &ObjId, found: &mut Vec<(String, Value)>| {
        for state in doc.keys(slots_of) {
            if found.iter().any(|(known, _)| *known == state) {
                continue;
            }
            let text = text_at(doc, slots_of, &state).ok().flatten();
            if let Some(value) = text.as_deref().and_then(view_slot) {
                found.push((state, value));
            }
        }
    };
    collect(place, &mut found);
    for split in SPLIT {
        if let Some(slots_of) = map_at(doc, place, split) {
            collect(&slots_of, &mut found);
        }
    }
    found
}

/// Every slot of view state the record keeps, of everyone, each state once. Every copy of a
/// person's map is read, so a state a replica set before it met the others is theirs, until the
/// next write keeps it.
fn slots(doc: &Doc) -> Vec<Slot> {
    let mut found: Vec<Slot> = Vec::new();
    for (person, held) in people(doc) {
        for views in kept_first(doc, &held, VIEWS) {
            for window in doc.keys(&views) {
                for place in kept_first(doc, &views, &window) {
                    for (state, value) in place_states(doc, &place) {
                        let known = |slot: &Slot| {
                            slot.person == person && slot.window == window && slot.state == state
                        };
                        if found.iter().any(known) {
                            continue;
                        }
                        found.push(Slot {
                            person: person.clone(),
                            window: window.clone(),
                            state,
                            value,
                        });
                    }
                }
            }
        }
    }
    found
}

/// The seen marks the record keeps, of everyone, per resource.
fn marks(doc: &Doc) -> Result<Vec<Seen>, ModelError> {
    let mut marks: Vec<Seen> = Vec::new();
    for (person, held) in people(doc) {
        for seen in kept_first(doc, &held, SEEN_MARKS) {
            for resource in doc.keys(&seen) {
                let known = |seen: &Seen| seen.person == person && seen.resource == resource;
                if marks.iter().any(known) {
                    continue;
                }
                let Some(at) = map_at(doc, &seen, &resource) else {
                    continue;
                };
                let mark = SeenMark {
                    change: text_at(doc, &at, "change")?.unwrap_or_default(),
                    at: text_at(doc, &at, "at")?.unwrap_or_default(),
                };
                marks.push(Seen {
                    person: person.clone(),
                    resource,
                    mark,
                });
            }
        }
    }
    Ok(marks)
}

/// The runs people reviewed.
fn reviewed(doc: &Doc) -> Vec<Reviewed> {
    let mut reviewed: Vec<Reviewed> = Vec::new();
    for (person, held) in people(doc) {
        for runs in kept_first(doc, &held, REVIEWED) {
            for run in doc.keys(&runs) {
                if reviewed
                    .iter()
                    .any(|known| known.person == person && known.run == run)
                {
                    continue;
                }
                reviewed.push(Reviewed {
                    person: person.clone(),
                    run,
                });
            }
        }
    }
    reviewed
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionContext;

    /// `keep` applied as `actor`'s change: as the record's own window applies it.
    fn apply_as(doc: &mut Doc, actor: &str, keep: &Keep) -> Result<(), String> {
        let encoded = tilo_resource::encode::<Personal>(keep).map_err(|e| e.to_string())?;
        let context = ActionContext::for_resource(KIND, KIND, Personal::LABEL)
            .with_invoker(tilo_actions::ActionInvoker::actor(actor));
        tilo_actions::apply_local_with_context(doc, keep, encoded, None, context)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn kept(keeps: &[(&str, Keep)]) -> Doc {
        let mut doc = Doc::new();
        for (actor, keep) in keeps {
            apply_as(&mut doc, actor, keep).expect("the change applies");
        }
        doc
    }

    fn view(person: &str, window: &str, state: &str, value: Json) -> Keep {
        Keep::View {
            person: person.to_owned(),
            window: window.to_owned(),
            state: state.to_owned(),
            value,
        }
    }

    fn seen(person: &str, resource: &str, mark: Option<SeenMark>) -> Keep {
        Keep::Seen {
            person: person.to_owned(),
            resource: resource.to_owned(),
            mark,
        }
    }

    #[test]
    fn a_persons_view_state_and_seen_marks_read_back_as_they_were_kept() {
        let doc = kept(&[
            (
                "ada",
                view("ada", "tasks", "place", Json::String("today".to_owned())),
            ),
            ("ada", view("ada", "tasks", "scroll", Json::from(120))),
            (
                "ada",
                seen(
                    "ada",
                    "tl:so.tilo.lang.tasks:inbox",
                    Some(SeenMark {
                        change: "ab".repeat(32),
                        at: "2026-11-04T12:00:00Z".to_owned(),
                    }),
                ),
            ),
            (
                "ada",
                Keep::Runs {
                    person: "ada".to_owned(),
                    runs: vec!["run-7".to_owned()],
                },
            ),
        ]);
        let state = Personal::read(&doc).expect("the record reads");
        let theirs = state.of("ada");
        assert_eq!(theirs.slots.len(), 2);
        assert_eq!(
            theirs.slots[0].key(),
            StateKey::view("tasks", "ada", "place")
        );
        assert_eq!(theirs.slots[0].value, Value::text("today"));
        assert_eq!(
            theirs.slots[1].key(),
            StateKey::view("tasks", "ada", "scroll")
        );
        assert_eq!(theirs.seen.len(), 1);
        assert_eq!(theirs.seen[0].resource, "tl:so.tilo.lang.tasks:inbox");
        assert_eq!(theirs.seen[0].mark.change, "ab".repeat(32));
        assert_eq!(theirs.runs[0].run, "run-7");
    }

    /// The maps a record kept a window's state in before a view's state was one kind.
    fn kept_under_the_split(
        doc: &mut Doc,
        person: &str,
        window: &str,
        which: &str,
        state: &str,
        value: &str,
    ) {
        let personal = ensure_map(doc, &ROOT, PERSONAL).expect("the map opens");
        let theirs = ensure_map(doc, &personal, person).expect("the map opens");
        let views = ensure_map(doc, &theirs, VIEWS).expect("the map opens");
        let place = ensure_map(doc, &views, window).expect("the map opens");
        let slots = ensure_map(doc, &place, which).expect("the map opens");
        doc.put(&slots, state, value).expect("the value keeps");
    }

    #[test]
    fn state_a_record_kept_under_the_two_old_maps_still_reads() {
        let mut doc = Doc::new();
        kept_under_the_split(&mut doc, "ada", "tasks", "window", "order", "\"due\"");
        kept_under_the_split(&mut doc, "ada", "tasks", "lens", "draft", "\"hi\"");
        let state = Personal::read(&doc).expect("the record reads");
        let theirs = state.of("ada");
        assert_eq!(theirs.slots.len(), 2);
        let value = |name: &str| {
            theirs
                .slots
                .iter()
                .find(|slot| slot.state == name)
                .map(|slot| slot.value.clone())
        };
        assert_eq!(value("order"), Some(Value::text("due")));
        assert_eq!(value("draft"), Some(Value::text("hi")));
        assert_eq!(
            theirs.slots[0].key(),
            StateKey::view("tasks", "ada", theirs.slots[0].state.clone())
        );
    }

    #[test]
    fn the_place_this_build_keeps_a_state_at_wins_over_the_old_maps() {
        let mut doc = Doc::new();
        kept_under_the_split(&mut doc, "ada", "tasks", "window", "order", "\"due\"");
        apply_as(
            &mut doc,
            "ada",
            &view("ada", "tasks", "order", Json::from("created")),
        )
        .expect("the change applies");
        let state = Personal::read(&doc).expect("the record reads");
        assert_eq!(state.slots.len(), 1, "one state, read once");
        assert_eq!(state.slots[0].value, Value::text("created"));
    }

    #[test]
    fn one_persons_state_is_nobodys_else_and_two_people_keep_theirs_side_by_side() {
        let doc = kept(&[
            ("ada", view("ada", "tasks", "place", Json::from("today"))),
            ("bo", view("bo", "tasks", "place", Json::from("inbox"))),
        ]);
        let state = Personal::read(&doc).expect("the record reads");
        assert_eq!(state.slots.len(), 2, "one record holds both");
        assert_eq!(
            state.of("ada").slots[0].value,
            Value::text("today"),
            "ada's is hers"
        );
        assert_eq!(state.of("bo").slots[0].value, Value::text("inbox"));
        assert!(state.of("cy").is_empty());
    }

    #[test]
    fn a_change_for_another_person_is_refused_and_keeps_nothing() {
        let mut doc = Doc::new();
        let error = apply_as(
            &mut doc,
            "bo",
            &view("ada", "tasks", "place", Json::from("today")),
        )
        .expect_err("bo keeps no state of ada's");
        assert!(error.contains("their own state"), "{error}");
        assert!(
            Personal::read(&doc).expect("the record reads").is_empty(),
            "nothing was kept"
        );
    }

    #[test]
    fn a_change_no_person_invoked_is_refused() {
        let mut doc = Doc::new();
        let keep = view("ada", "tasks", "place", Json::from("today"));
        let mut plain = Doc::new();
        assert!(keep.apply(&mut plain).is_err(), "nobody's action");
        let encoded = tilo_resource::encode::<Personal>(&keep).expect("the change encodes");
        let error = tilo_actions::apply_local_with_context(
            &mut doc,
            &keep,
            encoded,
            None,
            ActionContext::for_resource(KIND, KIND, Personal::LABEL),
        )
        .expect_err("no invoker");
        assert!(error.to_string().contains("their own state"), "{error}");
    }

    #[test]
    fn a_verb_builds_the_change_of_its_own_kind_and_no_other() {
        let decl = declaration(SEEN, "Keep what is seen");
        let keep = serde_json::to_value(seen("ada", "tl:x", None)).expect("the change serializes");
        let args = tilo_actions::ActionArgs::bind(
            &decl.params,
            [(
                KEEP.to_owned(),
                tilo_actions::ArgValue::Object(keep.as_object().unwrap().clone()),
            )],
        )
        .expect("the argument binds");
        assert_eq!(decl.build_action(&args, 0), Ok(seen("ada", "tl:x", None)));
        let other = serde_json::to_value(view("ada", "tasks", "place", Json::from("today")))
            .expect("the change serializes");
        let args = tilo_actions::ActionArgs::bind(
            &decl.params,
            [(
                KEEP.to_owned(),
                tilo_actions::ArgValue::Object(other.as_object().unwrap().clone()),
            )],
        )
        .expect("the argument binds");
        assert!(
            decl.build_action(&args, 0).is_err(),
            "a seen verb builds no view"
        );
    }

    #[test]
    fn a_slot_kept_twice_reads_back_once_as_it_is_now() {
        let doc = kept(&[
            ("ada", view("ada", "tasks", "place", Json::from("inbox"))),
            ("ada", view("ada", "tasks", "place", Json::from("today"))),
        ]);
        let state = Personal::read(&doc).expect("the record reads");
        assert_eq!(state.of("ada").slots.len(), 1);
        assert_eq!(state.of("ada").slots[0].value, Value::text("today"));
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
            (
                "ada",
                seen(
                    "ada",
                    "tl:so.tilo.lang.tasks:inbox",
                    Some(SeenMark {
                        change: "cd".repeat(32),
                        at: "2026-11-04T12:00:00Z".to_owned(),
                    }),
                ),
            ),
            ("ada", seen("ada", "tl:so.tilo.lang.tasks:inbox", None)),
            (
                "ada",
                Keep::Runs {
                    person: "ada".to_owned(),
                    runs: vec!["run-1".to_owned(), "run-2".to_owned()],
                },
            ),
            (
                "ada",
                Keep::Runs {
                    person: "ada".to_owned(),
                    runs: vec!["run-2".to_owned()],
                },
            ),
        ]);
        let state = Personal::read(&doc).expect("the record reads");
        let theirs = state.of("ada");
        assert!(theirs.seen.is_empty(), "{theirs:?}");
        assert_eq!(
            theirs.runs,
            vec![Reviewed {
                person: "ada".to_owned(),
                run: "run-2".to_owned()
            }]
        );
    }

    /// How many of the maps `doc` holds at `key` of `parent` hold anything: a fold empties the
    /// copies it folded and leaves them in place, so the record holds one map with entries in
    /// it from that write on.
    fn holding(doc: &Doc, parent: &ObjId, key: &str) -> usize {
        crate::containers::maps(doc, parent, key)
            .iter()
            .filter(|held| doc.keys(held).next().is_some())
            .count()
    }

    /// Two devices of ada's, each keeping her state before they met: each made the `$personal`
    /// map, her map and the maps below of its own, and Automerge keeps one of each.
    fn kept_apart(keeps: &[Keep], other: &[Keep]) -> Doc {
        use tilo_automerge::ApplyIncoming as _;
        let mut mine = Doc::new();
        let mut theirs = mine.fork();
        for keep in keeps {
            apply_as(&mut mine, "ada", keep).expect("her device keeps it");
        }
        for keep in other {
            apply_as(&mut theirs, "ada", keep).expect("her other device keeps it");
        }
        assert_eq!(
            crate::containers::maps(&mine, &ROOT, PERSONAL).len(),
            1,
            "one device made the map"
        );
        mine.merge_incoming(&mut theirs).expect("the devices meet");
        theirs.merge_incoming(&mut mine).expect("and meet again");
        mine
    }

    /// Every entry ada's devices kept before they met reads, and her next change keeps the one
    /// map there is.
    #[test]
    fn the_state_each_device_kept_before_they_met_reads_on_both() {
        let mut doc = kept_apart(
            &[view("ada", "tasks", "place", Json::from("today"))],
            &[
                view("ada", "tasks", "scroll", Json::from(120)),
                seen(
                    "ada",
                    "tl:so.tilo.lang.tasks:inbox",
                    Some(SeenMark {
                        change: "ab".repeat(32),
                        at: "2026-11-04T12:00:00Z".to_owned(),
                    }),
                ),
                Keep::Runs {
                    person: "ada".to_owned(),
                    runs: vec!["run-7".to_owned()],
                },
            ],
        );
        assert_eq!(
            crate::containers::maps(&doc, &ROOT, PERSONAL).len(),
            2,
            "the devices made a map each"
        );
        let state = Personal::read(&doc).expect("the record reads");
        let theirs = state.of("ada");
        let mut places: Vec<String> = theirs
            .slots
            .iter()
            .map(|slot| format!("{}={:?}", slot.state, slot.value))
            .collect();
        places.sort();
        assert_eq!(places, ["place=Text(\"today\")", "scroll=Int(120)"]);
        assert_eq!(theirs.seen.len(), 1, "the seen mark her other device kept");
        assert_eq!(theirs.runs[0].run, "run-7");

        // Her next change keeps the state of both in the one map, and nothing is lost.
        apply_as(
            &mut doc,
            "ada",
            &view("ada", "tasks", "lens", Json::from(2)),
        )
        .expect("the change applies");
        assert_eq!(
            holding(&doc, &ROOT, PERSONAL),
            1,
            "one map of her state from that change on"
        );
        assert_eq!(
            crate::containers::reach(&doc, &ROOT, &[PERSONAL, "ada", VIEWS, "tasks"])
                .iter()
                .filter(|place| doc.keys(place).next().is_some())
                .count(),
            1,
            "her window's state is in one map from that change on"
        );
        let state = Personal::read(&doc).expect("the record reads");
        assert_eq!(state.of("ada").slots.len(), 3);
        assert_eq!(state.of("ada").seen.len(), 1);
        assert_eq!(state.of("ada").runs.len(), 1);
    }
}
