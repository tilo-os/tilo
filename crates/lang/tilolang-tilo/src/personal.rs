//! A person's own state of an app's workspace: the view state they set in views, and what they
//! have seen.
//!
//! The runtime holds those slots while a window is open ([`tilolang_runtime::view_state`]); the
//! store keeps them, so what a person picked is still theirs after a restart and reaches their
//! other devices. Each person's state of one app is one resource of the kind [`KIND`] in the
//! device's resource store ([`id_of`]), which only that person reads: its kind's readers are its
//! author ([`crate::surface::personal_surface`]), so the store places it in a document of theirs
//! alone and it reaches only their devices.
//!
//! The resource indexes nothing; its content holds the state:
//!
//! ```text
//! views  { "[\"<window>\",\"<state>\"]": <value as JSON> }   the view state, per window and slot
//! seen   { "<resource>": { change, at } }                    the newest version they saw
//! runs   { "<run>": true }                                    the runs they reviewed
//! ```
//!
//! Every write keeps one slot or one mark, as an edit of that one entry, so two devices of the
//! person that keep different entries at once both keep theirs.

use collab_types::store::Resource;
use collab_types::store::wire::{Command, Edit, Reply};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use tilo_resource_store::Stamp;
use tilolang_runtime::ids::{Actor, MemberId};
use tilolang_runtime::seen::SeenMark;
use tilolang_runtime::value::Value;
use tilolang_runtime::view_state::StateKey;

use tilo_resource_store::Port;

/// The kind each person's own state of an app is kept as.
pub const KIND: &str = tilolang::PERSONAL_RESOURCE_TYPE;

/// The app that keeps the state: the tilolang host's own, since the state is of the windows it
/// runs rather than of one package's types.
pub const OWNER: &str = tilolang::HOST_APP;

/// The verb keeping one slot of view state.
pub const VIEW_STATE: &str = "view-state";
/// The verb keeping one seen mark.
pub const SEEN: &str = "seen";
/// The verb keeping the runs a person reviewed.
pub const RUNS: &str = "seen-runs";

/// The content map of the view state, by window and slot.
const VIEWS: &str = "views";
/// The content map of the seen marks, by resource.
const SEEN_MARKS: &str = "seen";
/// The content map of the reviewed runs.
const REVIEWED: &str = "runs";

/// The id `view`'s state is kept under for the window that takes the shell's tab `tab`: each
/// window of an app is at its own place, so two windows of Tasks stand on two places, and a window
/// keeps its place on the person's other devices and after a restart, where the shell keeps the
/// same tab.
#[must_use]
pub fn in_window(tab: u64, view: &str) -> String {
    format!("{tab}/{view}")
}

/// The view whose state is kept under `kept` for the window of the tab `tab`, as [`in_window`]
/// writes it; `None` for another window's.
#[must_use]
pub fn of_window(tab: u64, kept: &str) -> Option<&str> {
    kept.strip_prefix(&format!("{tab}/"))
}

/// The person `id` as the runtime names them.
#[must_use]
pub fn actor(id: &str) -> Actor {
    Actor::Member(MemberId::person(id))
}

/// The person `who` is, when they are one: whose own state is kept.
#[must_use]
pub fn person_of(who: &Actor) -> Option<&str> {
    who.member_id()?.person_id()
}

/// What a store is told of the kind: its author alone reads it. Only that, so the store checks
/// no change's verb against this one surface.
#[must_use]
pub fn readers() -> tilo_resource_store::Schema {
    tilo_resource_store::Schema {
        readers: std::iter::once((
            KIND.to_owned(),
            tilo_actions::surface::ReaderRule::Author { until: None },
        ))
        .collect(),
        ..tilo_resource_store::Schema::default()
    }
}

/// The id of `person`'s own state of the app `app` in the store.
#[must_use]
pub fn id_of(app: &str, person: &str) -> String {
    format!("{app}@{}", person.to_ascii_lowercase())
}

/// One slot of the person's view state, as the runtime keys it and as the store keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// Whose state it is.
    pub person: String,
    /// The id of the view that declared the state, in the window it is kept for ([`in_window`]).
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
        StateKey::view(self.window.clone(), actor(&self.person), self.state.clone())
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

/// A person's own state of an app: the window state they set, what they have seen, and the runs
/// they reviewed.
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

    /// Whether nothing is set or seen here yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty() && self.seen.is_empty() && self.runs.is_empty()
    }
}

/// One piece of a person's own state, as a write keeps it.
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
    /// The runs `person` reviewed, all of them: the ones kept before and not named are dropped.
    Runs { person: String, runs: Vec<String> },
}

impl Keep {
    /// The verb the write is stamped with.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::View { .. } => VIEW_STATE,
            Self::Seen { .. } => SEEN,
            Self::Runs { .. } => RUNS,
        }
    }

    /// Whose state the write keeps.
    #[must_use]
    pub fn person(&self) -> &str {
        match self {
            Self::View { person, .. } | Self::Seen { person, .. } | Self::Runs { person, .. } => {
                person
            }
        }
    }

    /// The write keeping the slot `key` set on this device.
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
            .actor
            .as_ref()
            .and_then(person_of)
            .ok_or_else(|| "a reading no person set is not a person's state".to_owned())?
            .to_owned();
        let value = tilolang_runtime::value::to_json(value)?;
        Ok(Self::View {
            person,
            window: key.window.clone(),
            state: key.state.clone(),
            value,
        })
    }

    /// The edits of the content that keep this, given what the content holds now.
    fn edits(&self, content: &Map<String, Json>) -> Vec<Edit> {
        match self {
            Self::View {
                window,
                state,
                value,
                ..
            } => vec![Edit::MapPut {
                field: VIEWS.to_owned(),
                key: view_key(window, state),
                value: value.clone(),
            }],
            Self::Seen {
                resource,
                mark: Some(mark),
                ..
            } => vec![Edit::MapPut {
                field: SEEN_MARKS.to_owned(),
                key: resource.clone(),
                value: serde_json::json!({ "change": mark.change, "at": mark.at }),
            }],
            Self::Seen {
                resource,
                mark: None,
                ..
            } => vec![Edit::MapDelete {
                field: SEEN_MARKS.to_owned(),
                key: resource.clone(),
            }],
            Self::Runs { runs, .. } => {
                let held = content.get(REVIEWED).and_then(Json::as_object);
                let dropped = held
                    .into_iter()
                    .flat_map(Map::keys)
                    .filter(|run| !runs.contains(run))
                    .map(|run| Edit::MapDelete {
                        field: REVIEWED.to_owned(),
                        key: run.clone(),
                    });
                let kept = runs
                    .iter()
                    .filter(|run| !held.is_some_and(|held| held.contains_key(*run)))
                    .map(|run| Edit::MapPut {
                        field: REVIEWED.to_owned(),
                        key: run.clone(),
                        value: Json::Bool(true),
                    });
                dropped.chain(kept).collect()
            }
        }
    }
}

/// The key a slot is kept under in the view map: the window and the slot, as a JSON pair, so
/// neither needs a separator it could itself hold.
fn view_key(window: &str, state: &str) -> String {
    serde_json::to_string(&(window, state)).unwrap_or_default()
}

/// What `person` keeps of the app `app`, as the store holds it now: nothing when they kept
/// nothing yet.
///
/// # Errors
/// When the store cannot be read.
pub fn read(port: &dyn Port, app: &str, person: &str) -> Result<PersonalState, String> {
    Ok(state_of(
        person,
        &content(port, app, person)?.unwrap_or_default(),
    ))
}

/// Keep `keeps`, the changes `person` made of their own state of the app `app`, in the store as
/// theirs: their resource is made the first time, readable by them alone, and each change is its
/// own write. A change of anyone else's state is refused, and keeps nothing.
///
/// # Errors
/// A change is someone else's, or the store refused it; what came before it is kept.
pub fn keep(port: &dyn Port, app: &str, person: &str, keeps: &[Keep]) -> Result<(), String> {
    if let Some(other) = keeps
        .iter()
        .find(|keep| !keep.person().eq_ignore_ascii_case(person))
    {
        return Err(format!(
            "only a person keeps their own state, not {}'s",
            other.person()
        ));
    }
    let id = id_of(app, person);
    let mut held = content(port, app, person)?;
    for keep in keeps {
        let stamp = stamp(keep.verb(), person);
        let none = Map::new();
        let edits = keep.edits(held.as_ref().unwrap_or(&none));
        if edits.is_empty() {
            continue;
        }
        let edit = Command::Edit {
            kind: KIND.to_owned(),
            id: id.clone(),
            edits: edits.clone(),
            stamp: stamp.clone(),
        };
        let command = if held.is_some() {
            edit
        } else {
            let made = Command::Put {
                kind: KIND.to_owned(),
                id: id.clone(),
                // Its readers are its author's alone, as its kind says.
                resource: Resource::default(),
                stamp,
            };
            Command::Batch(vec![made, edit])
        };
        port.command(command)?;
        let content = held.get_or_insert_with(Map::new);
        for edit in &edits {
            apply(content, edit);
        }
    }
    Ok(())
}

/// The content of `person`'s state of `app`: `None` while the store holds no resource of it.
fn content(port: &dyn Port, app: &str, person: &str) -> Result<Option<Map<String, Json>>, String> {
    let id = id_of(app, person);
    let found = port.command(Command::Get {
        kind: KIND.to_owned(),
        id: id.clone(),
    })?;
    match found {
        Reply::Resource(None) => return Ok(None),
        Reply::Resource(Some(_)) => {}
        other => return Err(format!("a read answered {other:?}")),
    }
    match port.command(Command::Content {
        kind: KIND.to_owned(),
        id,
    })? {
        Reply::Content(Some(Json::Object(content))) => Ok(Some(content)),
        Reply::Content(_) => Ok(Some(Map::new())),
        other => Err(format!("a read answered {other:?}")),
    }
}

/// `content` as `edit` leaves it: what the next change of the same keep is decided against.
fn apply(content: &mut Map<String, Json>, edit: &Edit) {
    match edit {
        Edit::MapPut { field, key, value } => {
            let map = content
                .entry(field.clone())
                .or_insert_with(|| Json::Object(Map::new()));
            if !map.is_object() {
                *map = Json::Object(Map::new());
            }
            if let Some(map) = map.as_object_mut() {
                map.insert(key.clone(), value.clone());
            }
        }
        Edit::MapDelete { field, key } => {
            if let Some(map) = content.get_mut(field).and_then(Json::as_object_mut) {
                map.remove(key);
            }
        }
        Edit::Set { .. } | Edit::Splice { .. } | Edit::Add { .. } | Edit::Remove { .. } => {}
    }
}

/// The stamp of one write of `person`'s own state, of the verb `verb`.
fn stamp(verb: &str, person: &str) -> Stamp {
    Stamp {
        verb: verb.to_owned(),
        invoker: Stamp::by(&actor(person)),
        invocation: uuid::Uuid::new_v4().to_string(),
        at: crate::host::system_time(),
        ..Stamp::default()
    }
}

/// What `content`, `person`'s state, holds.
fn state_of(person: &str, content: &Map<String, Json>) -> PersonalState {
    let map = |field: &str| content.get(field).and_then(Json::as_object);
    let slots = map(VIEWS)
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let (window, state) = serde_json::from_str::<(String, String)>(key)
                .inspect_err(|error| tracing::warn!(%error, "personal: a slot's key does not read"))
                .ok()?;
            let value = tilolang_runtime::value::from_json(value)
                .inspect_err(|error| {
                    tracing::warn!(
                        ?error,
                        "personal: a kept value is not one the runtime holds"
                    );
                })
                .ok()?;
            Some(Slot {
                person: person.to_owned(),
                window,
                state,
                value,
            })
        })
        .collect();
    let seen = map(SEEN_MARKS)
        .into_iter()
        .flatten()
        .filter_map(|(resource, mark)| {
            let text = |name: &str| mark.get(name).and_then(Json::as_str).map(str::to_owned);
            Some(Seen {
                person: person.to_owned(),
                resource: resource.clone(),
                mark: SeenMark {
                    change: text("change")?,
                    at: text("at").unwrap_or_default(),
                },
            })
        })
        .collect();
    let runs = map(REVIEWED)
        .into_iter()
        .flatten()
        .map(|(run, _)| Reviewed {
            person: person.to_owned(),
            run: run.clone(),
        })
        .collect();
    PersonalState { slots, seen, runs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource_store::LocalPort;

    const APP: &str = "so.tilo.lang.tasks";

    /// One store, as the person `reader` reads it: only resources whose readers are them.
    struct As {
        store: std::rc::Rc<std::cell::RefCell<tilo_resource_store::Store>>,
        reader: &'static str,
    }

    impl Port for As {
        fn command(&self, command: Command) -> Result<Reply, String> {
            if command.writes() {
                return tilo_resource_store::wire::write(&mut self.store.borrow_mut(), command)
                    .map(|_| Reply::Written)
                    .map_err(|refused| refused.to_string());
            }
            tilo_resource_store::wire::read(&self.store.borrow(), &command, &|readers| {
                readers == "*" || readers == self.reader
            })
            .ok_or_else(|| "no read".to_owned())
        }
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

    fn mark(change: &str) -> SeenMark {
        SeenMark {
            change: change.repeat(32),
            at: "2026-11-04T12:00:00Z".to_owned(),
        }
    }

    tilo_test_env::scenario! {
        fn kept_state_reads_back_as_it_was_kept;

        phase a_persons_view_state_and_seen_marks_read_back_as_they_were_kept;
        {
            let port = LocalPort::in_memory();
            keep(
                &port,
                APP,
                "ada",
                &[
                    view("ada", "1/tasks", "place", Json::from("today")),
                    view("ada", "1/tasks", "scroll", Json::from(120)),
                    seen("ada", "inbox", Some(mark("ab"))),
                    Keep::Runs {
                        person: "ada".to_owned(),
                        runs: vec!["run-7".to_owned()],
                    },
                ],
            )
            .expect("her state keeps");
            let theirs = read(&port, APP, "ada").expect("her state reads");
            let mut slots: Vec<String> = theirs
                .slots
                .iter()
                .map(|slot| format!("{}={:?}", slot.state, slot.value))
                .collect();
            slots.sort();
            assert_eq!(slots, ["place=Text(\"today\")", "scroll=Int(120)"]);
            assert_eq!(
                theirs.slots[0].key().window,
                "1/tasks",
                "a slot is kept for its window"
            );
            assert_eq!(theirs.seen.len(), 1);
            assert_eq!(theirs.seen[0].resource, "inbox");
            assert_eq!(theirs.seen[0].mark, mark("ab"));
            assert_eq!(theirs.runs[0].run, "run-7");
        }

        phase a_slot_kept_twice_reads_back_once_as_it_is_now;
        {
            let port = LocalPort::in_memory();
            keep(&port, APP, "ada", &[view("ada", "1/tasks", "place", Json::from("inbox"))])
                .expect("kept");
            keep(&port, APP, "ada", &[view("ada", "1/tasks", "place", Json::from("today"))])
                .expect("kept again");
            let theirs = read(&port, APP, "ada").expect("her state reads");
            assert_eq!(theirs.slots.len(), 1);
            assert_eq!(theirs.slots[0].value, Value::text("today"));
        }

        phase nothing_kept_reads_as_nothing;
        {
            let port = LocalPort::in_memory();
            assert!(read(&port, APP, "ada").expect("reads").is_empty());
        }

        phase a_taken_back_mark_is_no_mark_and_reviewed_runs_replace_the_ones_before;
        {
            let port = LocalPort::in_memory();
            keep(
                &port,
                APP,
                "ada",
                &[
                    seen("ada", "inbox", Some(mark("cd"))),
                    seen("ada", "inbox", None),
                    Keep::Runs {
                        person: "ada".to_owned(),
                        runs: vec!["run-1".to_owned(), "run-2".to_owned()],
                    },
                    Keep::Runs {
                        person: "ada".to_owned(),
                        runs: vec!["run-2".to_owned()],
                    },
                ],
            )
            .expect("kept");
            let theirs = read(&port, APP, "ada").expect("reads");
            assert!(theirs.seen.is_empty(), "{theirs:?}");
            assert_eq!(
                theirs.runs,
                vec![Reviewed {
                    person: "ada".to_owned(),
                    run: "run-2".to_owned(),
                }]
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_persons_state_is_theirs_alone;

        phase two_people_and_two_apps_keep_theirs_apart;
        {
            let port = LocalPort::in_memory();
            keep(&port, APP, "ada", &[view("ada", "1/tasks", "place", Json::from("today"))])
                .expect("ada keeps hers");
            keep(&port, APP, "bo", &[view("bo", "1/tasks", "place", Json::from("inbox"))])
                .expect("bo keeps his");
            keep(
                &port,
                "so.tilo.lang.notes",
                "ada",
                &[view("ada", "2/notes", "place", Json::from("all"))],
            )
            .expect("ada keeps her notes' place");
            let hers = read(&port, APP, "ada").expect("reads");
            assert_eq!(hers.slots.len(), 1);
            assert_eq!(hers.slots[0].value, Value::text("today"));
            assert_eq!(read(&port, APP, "bo").expect("reads").slots[0].value, Value::text("inbox"));
            assert!(read(&port, APP, "cy").expect("reads").is_empty());
        }

        phase her_state_is_read_by_her_alone;
        {
            let store = tilo_resource_store::Store::new("workspace");
            let store = std::rc::Rc::new(std::cell::RefCell::new(store));
            store.borrow_mut().declare(readers()).expect("the personal kind's reader rule is declared");
            let hers = As { store: std::rc::Rc::clone(&store), reader: "ada" };
            let his = As { store, reader: "bo" };
            keep(&hers, APP, "AdA", &[view("AdA", "1/tasks", "place", Json::from("today"))])
                .expect("kept");
            assert_eq!(read(&hers, APP, "ada").expect("reads").slots.len(), 1);
            assert!(
                read(&his, APP, "ada").expect("reads").is_empty(),
                "bo reads nothing of hers"
            );
        }

        phase a_change_for_another_person_is_refused_and_keeps_nothing;
        {
            let port = LocalPort::in_memory();
            let error = keep(&port, APP, "bo", &[view("ada", "1/tasks", "place", Json::from("x"))])
                .expect_err("bo keeps no state of ada's");
            assert!(error.contains("their own state"), "{error}");
            assert!(read(&port, APP, "ada").expect("reads").is_empty());
            assert!(read(&port, APP, "bo").expect("reads").is_empty());
        }

        phase a_reading_the_platform_keeps_is_not_the_persons_state;
        {
            let key = StateKey::runtime("tasks", None, "present");
            assert!(Keep::view(&key, &Value::text("ada")).is_err());
        }
    }
}
