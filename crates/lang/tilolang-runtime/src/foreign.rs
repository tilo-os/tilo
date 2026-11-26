//! Resources of apps this runtime does not run: what they print, and their readings.
//!
//! An app the platform publishes through the session (the sound, the displays, the desktop) is
//! not loaded here as code: the host registers its description ([`crate::Runtime::load_foreign`])
//! and hands the runtime the newest reading the session's watch saw
//! ([`crate::Runtime::set_reading`]). A view that takes such a resource as a parameter reads its
//! fields from that reading, typed by the description, and runs again when a new reading comes
//! in: the reading is one slot of view state per resource, so the boundaries that read it are
//! exactly the ones marked ([`crate::view_state`]).
//!
//! A resource of another app is written `<app>#<Type>` as its type ([`is_foreign`]), and its id
//! is the session's resource it stands for (`sound`, `desktop:overlays`).

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value as Json;

use crate::error::ViewFailure;
use crate::ids::ModuleHash;
use crate::meta::{Ty, TypeKey};
use crate::registry::Registry;
use crate::store::Lit;
use crate::value::{RefValue, ResourceRef, Value};
use crate::view_state::StateKey;

/// How a resource of an app this runtime does not run printed when a program met it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Shown {
    /// How a program writes it: `@notes.Note:root-1/notes:n2`.
    pub literal: String,
    pub app: String,
    /// Its type's name.
    pub ty: String,
    /// Its id ([`ResourceRef::id_string`]).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// What a foreign resource reads its fields from.
#[derive(Debug)]
enum Reading {
    /// The newest reading of it, and the generation it was set with.
    Set { json: Json, generation: u64 },
    /// Where a record inside another app's reading stands (a menu's row): the reading it stands
    /// in, and the fields and items down to it. It reads as that reading says now.
    Inside(Inside),
    /// The host publishes nothing of it, and none is coming: a resource this device has none of
    /// ([`crate::Runtime::set_absent`]). Its fields read as its type starts, so a view draws the
    /// empty state it draws for a device without the resource, never a skeleton no reading fills.
    Absent,
}

/// The way down to a record inside a reading: a field's name, or an item's index.
#[derive(Debug)]
struct Inside {
    parent: String,
    steps: Vec<String>,
}

/// Where a host answers the reading of a resource its code meets, while the code runs
/// ([`crate::Runtime::set_readings_asked_for`]).
type AskForReading = Box<dyn Fn(&ResourceRef) -> Result<Option<Json>, String>>;

/// Results a value carries that `show` prints for the resources of apps the runtime does not
/// hold: how each printed, and its reading.
#[derive(Default)]
pub struct Foreign {
    by_key: BTreeMap<String, Shown>,
    readings: BTreeMap<String, Reading>,
    /// Moves with every reading that changed.
    clock: u64,
    /// The resources of apps whose code runs here that a view read before their reading came
    /// in, by key: each its id and type, asked of the host once ([`Foreign::take_wanted`]).
    wanted: BTreeMap<String, (String, String)>,
    /// Every resource asked for, by key: asked again only once forgotten.
    asked: std::collections::BTreeSet<String>,
    /// Whether a field was read from a reading that has not arrived yet (or a record of one):
    /// the errors those reads gave. Kept per message, not as one flag, so several places each
    /// waiting on their own reading are all read as waiting, and a fault beside them is still
    /// a fault.
    waiting: std::cell::RefCell<std::collections::BTreeSet<String>>,
    /// Where a reading this runtime lacks is asked for while the code runs, instead of waiting
    /// for the host to follow it ([`crate::Runtime::set_readings_asked_for`]): the runtime of a
    /// task's host, whose programs read the session's own resources (the device's, the
    /// session's, Settings') within the program they run, not on a next pass. `None` where the
    /// host hands readings in as the session publishes them.
    ask: Option<AskForReading>,
    /// The apps whose resources no host here publishes at all (the device's, the session's, on a
    /// host that has neither): each resource of one reads as its type starts
    /// ([`crate::Runtime::set_app_absent`]).
    absent_apps: std::collections::BTreeSet<String>,
}

impl Foreign {
    /// The reading of `target` held now, asking the host for nothing.
    pub(crate) fn held(&self, target: &ResourceRef) -> Option<&Json> {
        self.source(&Self::key(target)).map(|(json, _)| json)
    }

    /// Say the host publishes nothing of `target`, and none is coming: reading a field of it
    /// reads the field as its type starts, and the view that read it is drawn, never left
    /// waiting. A reading the host hands in later stands in its place.
    pub(crate) fn absent(&mut self, target: &ResourceRef) {
        let key = Self::key(target);
        self.clock = self.clock.saturating_add(1);
        self.readings.insert(key, Reading::Absent);
        self.asked.remove(&Self::key(target));
        self.wanted.remove(&Self::key(target));
    }

    /// Say the host publishes nothing of any resource of the app `app`, and none is coming: what
    /// a host whose device has no such app at all says ([`crate::Runtime::set_app_absent`]).
    pub(crate) fn absent_app(&mut self, app: &str) {
        self.absent_apps.insert(app.to_owned());
    }

    /// Whether the host publishes nothing of `target`: said of the resource itself, or of its app.
    fn publishes_nothing(&self, target: &ResourceRef) -> bool {
        if matches!(self.readings.get(&Self::key(target)), Some(Reading::Absent)) {
            return true;
        }
        crate::meta::foreign_type(&target.ty).is_some_and(|key| self.absent_apps.contains(&key.app))
    }

    /// The host asks for the reading of a resource the code meets and this holds none of, while
    /// the code runs, and hands the reading in at once.
    pub(crate) fn set_ask(
        &mut self,
        ask: impl Fn(&ResourceRef) -> Result<Option<Json>, String> + 'static,
    ) {
        self.ask = Some(Box::new(ask));
    }

    /// Whether the reading of `target` is held, asking the host for one now when it is not: a
    /// program reading a resource the session publishes gets what the session holds within the
    /// program it runs, not on the host's next pass. A resource is asked for once (until
    /// [`Foreign::forget_asked`], at the next program), so a loop over one that has no reading
    /// asks once.
    ///
    /// # Errors
    ///
    /// Why the host could not read it.
    pub(crate) fn arrive(&mut self, target: &ResourceRef) -> Result<bool, String> {
        let at = Self::key(target);
        if self.readings.contains_key(&at) && !self.asked.contains(&at) {
            return Ok(true);
        }
        // A host that takes questions answers them; one that publishes readings on its own pass
        // is left to it (what a read leaves in [`Foreign::wanted`]).
        if self.ask.is_none() || !self.asked.insert(at.clone()) {
            return Ok(self.readings.contains_key(&at));
        }
        match self.ask.as_ref().map(|ask| ask(target)) {
            // A reading the session holds nothing of now leaves what came before: a resource
            // whose service answered earlier still reads as it did.
            Some(Ok(None)) => Ok(self.readings.contains_key(&at)),
            Some(Ok(Some(json))) => {
                self.set_reading(target, json);
                Ok(true)
            }
            Some(Err(why)) => Err(why),
            None => Ok(false),
        }
    }

    /// Ask the host for the readings it was asked for again: a program starts, and what a
    /// session publishes moved since the last one, so nothing it read before is read as it was
    /// then. A host that publishes readings on its own pass keeps what it was asked for until it
    /// hands one in ([`Foreign::take_wanted`]).
    pub(crate) fn forget_asked(&mut self) {
        if self.ask.is_none() {
            return;
        }
        for at in std::mem::take(&mut self.asked) {
            self.readings.remove(&at);
        }
        self.wanted.clear();
    }

    /// The reading of `target`, asking the host for one where this holds none (a program reading a
    /// resource the session publishes): the JSON as the session holds it, `None` for a resource
    /// the host has no reading of either.
    ///
    /// # Errors
    ///
    /// Why the host could not read it.
    pub(crate) fn reading(&mut self, target: &ResourceRef) -> Result<Option<Json>, String> {
        if !self.arrive(target)? {
            return Ok(None);
        }
        Ok(self
            .source(&Self::key(target))
            .map(|(json, _)| json.clone()))
    }

    /// Every field of a resource of an app this runtime does not run, as a view of it reads them:
    /// each field's value from `reading`, typed by its declaration. A field the reading leaves out,
    /// or that does not read as its type says, is left out here, for the caller to read as the
    /// type starts.
    pub(crate) fn values(
        target: &ResourceRef,
        info: &crate::registry::TypeInfo,
        reading: &Json,
        registry: &Registry,
        blobs: Option<&dyn crate::blob::BlobHost>,
    ) -> BTreeMap<String, Result<Value, String>> {
        let mut values = BTreeMap::new();
        for f in &info.fields {
            let Some((step, value)) = held(reading, f) else {
                continue;
            };
            let mut decoder = Decoder {
                author: None,
                registry,
                blobs,
                base: target.id_string(),
                steps: Vec::new(),
                records: Vec::new(),
            };
            let field_value = decoder
                .down(step, value, &f.ty)
                .map(|read| as_a_field(&f.ty, read));
            values.insert(f.id.clone(), field_value);
        }
        values
    }

    /// The key of a foreign resource: its type (`app#Type`) and id.
    pub(crate) fn key(target: &ResourceRef) -> String {
        format!("{}@{}", target.ty, target.id_string())
    }

    pub(crate) fn get(&self, target: &ResourceRef) -> Option<&Shown> {
        self.by_key.get(&Self::key(target))
    }

    /// Remember how `target` printed.
    pub(crate) fn show(&mut self, target: &ResourceRef, shown: Shown) {
        self.by_key.insert(Self::key(target), shown);
    }

    /// The generation of `target`'s reading: 0 until one was set, larger after each change.
    pub fn generation(&self, target: &ResourceRef) -> u64 {
        self.source(&Self::key(target))
            .map_or(0, |(_, moved)| moved)
    }

    /// The resource an edit of `target` changes, and the fields from it down to `target`: a
    /// record that stands inside another app's reading (a record held in a field) is changed on
    /// the resource the reading is of, at the path the reading holds it ([`Reading::Inside`]).
    /// A resource read from its root is its own, with no path.
    pub(crate) fn root_of(&self, target: &ResourceRef) -> (ResourceRef, Vec<String>) {
        let mut steps = Vec::new();
        let mut at = Self::key(target);
        // Each record names the reading it stands in and its path from that reading's root, so
        // where it stands is read from where its parent stands.
        while let Some(Reading::Inside(inside)) = self.readings.get(&at) {
            if steps.is_empty() {
                steps.clone_from(&inside.steps);
            } else {
                steps.splice(0..0, inside.steps.iter().cloned());
            }
            at = inside.parent.clone();
        }
        at.split_once('@')
            .and_then(|(ty, id)| ResourceRef::parse_id(id, ty))
            .map_or_else(|| (target.clone(), Vec::new()), |root| (root, steps))
    }

    /// The type of the resource of another app whose id is `id`, as its reading came in:
    /// `None` until one did.
    ///
    /// The id is read as the reference it names ([`ResourceRef::parse_id`]) and compared in the
    /// one canonical form a reading's key holds ([`ResourceRef::id_string`], which
    /// [`Foreign::key`] writes): a place names its resource by the path the session publishes it
    /// at (`chat.workspace/channels/<id>`), whose canonical id is that path's document and item
    /// (`chat.workspace/<id>`), so matching the raw text would find no reading and a place would
    /// draw nothing.
    pub(crate) fn type_of(&self, id: &str) -> Option<String> {
        let at = ResourceRef::parse_id(id, String::new())
            .map_or_else(|| id.to_owned(), |at| at.id_string());
        let suffix = format!("@{at}");
        self.readings
            .keys()
            .find_map(|key| key.strip_suffix(&suffix).map(str::to_owned))
    }

    /// How many resources of other apps have a reading here.
    pub(crate) fn known(&self) -> usize {
        self.readings.len()
    }

    /// Whether `error` — a field read's failure — was waiting on a reading that has not
    /// arrived yet (a place whose app publishes nothing yet, or whose record the session has
    /// not published): the read failed for want of data, not for a fault. A host shows the
    /// call that read it its waiting state and draws the rest of the screen, and the call runs
    /// again when the reading comes ([`Runtime::set_reading`]). Every such error since the last
    /// [`Foreign::take_waiting`] is kept, so each of several places waiting on their own
    /// reading is read as waiting, not only the first.
    pub(crate) fn waiting(&self, error: &str) -> bool {
        // The host wraps what a read said in the import's name (`res_get_h: …`), and the view
        // call may wrap it further: the remembered message is the read's own, so it is enough
        // that the error holds it.
        self.waiting
            .borrow()
            .iter()
            .any(|waited| error.contains(waited.as_str()))
    }

    /// Forget the waiting errors gathered so far: a turn's tree is built afresh, and what it
    /// gathers is what it failed for.
    pub(crate) fn take_waiting(&self) {
        self.waiting.borrow_mut().clear();
    }

    /// Keep `error` as one a read gave for want of a reading that has not arrived yet.
    pub(crate) fn remember_waiting(&self, error: &str) {
        self.waiting.borrow_mut().insert(error.to_owned());
    }

    /// Why a read of `target` found nothing: no reading of it arrived yet, kept as a read
    /// waiting on one.
    fn not_arrived(&self, target: &ResourceRef) -> String {
        let why = format!(
            "no reading of `{}` arrived yet: its app publishes it through the session",
            target.id_string()
        );
        self.remember_waiting(&why);
        why
    }

    /// Ask the host for `target`'s reading, kept under `at`, once until it is forgotten.
    fn want(&mut self, at: String, target: &ResourceRef) {
        if self.asked.insert(at.clone()) {
            self.wanted
                .insert(at, (target.id_string(), target.ty.clone()));
        }
    }

    /// What links to `target`, each with how, as the reading of it says (`$links`): a resource
    /// of another app carries its links in its reading, as it carries its fields, because the
    /// store that keeps them runs where the app's code runs, not here.
    ///
    /// # Errors
    ///
    /// A message when no reading of it arrived yet, kept as a read waiting on one.
    pub(crate) fn links(
        &mut self,
        target: &ResourceRef,
    ) -> Result<Vec<(ResourceRef, crate::store::LinkHow)>, String> {
        let at = Self::key(target);
        if !self.arrive(target)? {
            // A resource no host here publishes has no links, as it holds no fields.
            if self.publishes_nothing(target) {
                return Ok(Vec::new());
            }
            return Err(self.not_arrived(target));
        }
        let Some((json, _)) = self.source(&at) else {
            return Ok(Vec::new());
        };
        let Some(items) = json.get(crate::store::LINKS).and_then(Json::as_array) else {
            return Ok(Vec::new());
        };
        let mut links = Vec::new();
        for item in items {
            let named = ["ref", "ty", "how"].map(|field| item.get(field).and_then(Json::as_str));
            let [Some(id), Some(ty), Some(how)] = named else {
                continue;
            };
            let (Some(link), Some(how)) = (
                ResourceRef::parse_id(id, ty.to_owned()),
                crate::store::LinkHow::of_word(how),
            ) else {
                continue;
            };
            links.push((link, how));
        }
        Ok(links)
    }

    /// What the reading of `target` says the person it was read for has not seen of its items
    /// ([`crate::store::UNSEEN`]); `None` for a reading that says nothing of it.
    ///
    /// # Errors
    ///
    /// A message when no reading of it arrived yet, kept as a read waiting on one.
    pub(crate) fn unseen(&mut self, target: &ResourceRef) -> Result<Option<Json>, String> {
        let at = Self::key(target);
        if !self.arrive(target)? {
            // The host is asked for its reading, as a field read of one yet to arrive asks.
            self.want(at, target);
            return Err(self.not_arrived(target));
        }
        Ok(self
            .source(&at)
            .and_then(|(json, _)| json.get(crate::store::UNSEEN).cloned()))
    }

    /// The words [`Self::unreadable`] answers for a root nothing published: what a view's row
    /// says under the pane's own words for what could not be read.
    pub const NOTHING_PUBLISHED: &'static str = "Nothing publishes it here";

    /// Why the reading of `target` says it could not be read, in words
    /// ([`crate::store::UNREADABLE`]); `None` for a reading that answers, and for one that
    /// arrived as the type starts before its app published anything. A resource nothing of
    /// which ever arrived is unknown too: its app is not on this device, or the workspace's
    /// service is not connected, and a view says that rather than reading every field as the
    /// empty value its type starts with.
    ///
    /// # Errors
    ///
    /// A message when the host was asked and could not answer.
    pub(crate) fn unreadable(&mut self, target: &ResourceRef) -> Result<Option<String>, String> {
        let at = Self::key(target);
        if !self.arrive(target)? {
            // A resource no host here publishes has no value to be unknown: a view over it
            // reads as its type starts, as its fields do, and draws its empty state.
            if self.publishes_nothing(target) {
                return Ok(None);
            }
            // Nothing of it came: the host is asked for it once, and the view told it is
            // unknown until a reading answers.
            self.want(at, target);
            return Ok(Some(Self::NOTHING_PUBLISHED.to_owned()));
        }
        Ok(self
            .source(&at)
            .and_then(|(json, _)| json.get(crate::store::UNREADABLE))
            .and_then(Json::as_str)
            .map(str::to_owned))
    }

    /// The resources a view read before their readings came in, each its id and type, since
    /// the last call: what the host asks the session to publish. Each is asked for once.
    pub(crate) fn take_wanted(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.wanted).into_values().collect()
    }

    /// What [`Self::take_wanted`] takes, of the apps `keep` names: the rest stay asked for, so
    /// the host that follows one kind of app's readings leaves another's to the host that
    /// answers them.
    pub(crate) fn take_wanted_of(
        &mut self,
        mut keep: impl FnMut(&str) -> bool,
    ) -> Vec<(String, String)> {
        let mut taken = Vec::new();
        let mut kept = BTreeMap::new();
        for (at, (id, ty)) in std::mem::take(&mut self.wanted) {
            if ty.split_once('#').is_some_and(|(app, _)| keep(app)) {
                taken.push((id, ty));
            } else {
                kept.insert(at, (id, ty));
            }
        }
        self.wanted = kept;
        taken
    }

    /// [`Self::take_wanted`], of the resources `take` names by their id and type: the rest stay
    /// asked for. A host that answers one family of addresses (an app's preferences) refuses
    /// what it may not give without holding another host's readings back.
    pub(crate) fn take_wanted_if(
        &mut self,
        mut take: impl FnMut(&str, &str) -> bool,
    ) -> Vec<(String, String)> {
        let mut taken = Vec::new();
        let mut kept = BTreeMap::new();
        for (at, (id, ty)) in std::mem::take(&mut self.wanted) {
            if take(&id, &ty) {
                taken.push((id, ty));
            } else {
                kept.insert(at, (id, ty));
            }
        }
        self.wanted = kept;
        taken
    }

    /// The reading of `target` whole, as JSON, with the generation it was read at, when one
    /// arrived: what a Rust app's view is handed of the resource it shows (its own model type
    /// reads its fields from it).
    ///
    /// The caller records the generation against [`reading_key`] as what the boundary read, so a
    /// boundary that built a Rust view runs again when a newer reading comes in, as one that
    /// read a field of it does.
    pub(crate) fn whole(&mut self, target: &ResourceRef) -> Option<(Json, u64)> {
        let key = Self::key(target);
        if self.publishes_nothing(target) && self.source(&key).is_none() {
            // Nothing of it is published: a Rust view over it is handed no fields, as one over a
            // device without the resource reads, and no reading is asked for.
            return Some((
                Json::Object(serde_json::Map::new()),
                self.generation(target),
            ));
        }
        if !self.arrive(target).unwrap_or(false) {
            // The host is asked for the resource's reading, as a field read of one yet to arrive
            // asks for it; the boundary runs again when it comes.
            self.want(key, target);
            return None;
        }
        let generation = self.generation(target);
        self.source(&key)
            .map(|(json, _)| (json.clone(), generation))
    }

    /// Forget every reading of the app `app`'s resources, and what was asked of them: nothing
    /// here shows that app any more.
    pub(crate) fn forget_app(&mut self, app: &str) {
        let prefix = format!("{app}#");
        self.readings.retain(|key, _| !key.starts_with(&prefix));
        self.by_key.retain(|key, _| !key.starts_with(&prefix));
        self.wanted.retain(|key, _| !key.starts_with(&prefix));
        self.asked.retain(|key| !key.starts_with(&prefix));
    }

    /// Take `json` as `target`'s reading. Whether it is not the one held.
    pub(crate) fn set_reading(&mut self, target: &ResourceRef, json: Json) -> bool {
        let key = Self::key(target);
        if matches!(
            self.readings.get(&key),
            Some(Reading::Set { json: held, .. }) if *held == json
        ) {
            return false;
        }
        self.clock = self.clock.saturating_add(1);
        let generation = self.clock;
        self.readings.insert(key, Reading::Set { json, generation });
        // The records that stood in the reading it replaces and stand in it no longer are of
        // the old one.
        let gone: Vec<String> = self
            .readings
            .iter()
            .filter(|(at, held)| matches!(held, Reading::Inside(_)) && self.source(at).is_none())
            .map(|(at, _)| at.clone())
            .collect();
        for at in gone {
            self.readings.remove(&at);
        }
        true
    }

    /// The records standing inside `target`'s reading, however deep, each by its key: what a
    /// view that read one of their fields read from that reading, so a new one moves them too.
    pub(crate) fn standing_in(&self, target: &ResourceRef) -> Vec<String> {
        let root = Self::key(target);
        self.readings
            .iter()
            .filter(|(at, held)| {
                matches!(held, Reading::Inside(_))
                    && self.set_reading_of(at).as_deref() == Some(root.as_str())
            })
            .map(|(at, _)| at.clone())
            .collect()
    }

    /// The key of the reading the host set that the record `at` stands in, through every record
    /// between them; `at` itself when the host set its reading.
    fn set_reading_of(&self, at: &str) -> Option<String> {
        let mut at = at.to_owned();
        // A record stands inside another at most as deep as there are readings.
        for _ in 0..=self.readings.len() {
            match self.readings.get(&at)? {
                Reading::Set { .. } | Reading::Absent => return Some(at),
                Reading::Inside(inside) => at.clone_from(&inside.parent),
            }
        }
        None
    }

    /// Say where a record inside `parent`'s reading stands, `target` naming it: what a view
    /// reads its fields from.
    fn place(&mut self, target: &ResourceRef, steps: Vec<String>, parent: String) {
        let at = Self::key(target);
        let stood = match self.readings.get(&at) {
            Some(Reading::Inside(held)) => held.parent == parent && held.steps == steps,
            _ => false,
        };
        if !stood {
            self.readings
                .insert(at, Reading::Inside(Inside { parent, steps }));
        }
    }

    /// The reading the fields of `at` are read from, and the generation it moved with: the one
    /// the host set, or the record where it stands in that one.
    fn source(&self, at: &str) -> Option<(&Json, u64)> {
        let held = self.readings.get(at)?;
        let inside = match held {
            Reading::Set { json, generation } => return Some((json, *generation)),
            // Nothing of it is published, so the reading has no JSON for any field to come from.
            Reading::Absent => return None,
            Reading::Inside(inside) => inside,
        };
        let (mut json, generation) = self.source(&inside.parent)?;
        for step in &inside.steps {
            json = match json {
                Json::Object(fields) => fields.get(step)?,
                Json::Array(items) => items.get(step.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some((json, generation))
    }

    /// The field `field` of `target` (its name, then its id) as its declared type, from the
    /// reading now. A record inside the reading (a row of a menu) is a resource of the app too:
    /// its own fields read the same way, from where the reading holds it.
    pub(crate) fn field(
        &mut self,
        target: &ResourceRef,
        field: &crate::registry::FieldInfo,
        author: Option<&ModuleHash>,
        registry: &Registry,
        blobs: Option<&dyn crate::blob::BlobHost>,
    ) -> Result<Value, String> {
        let at = Self::key(target);
        // What the host holds of it, now: a program reading a resource its session publishes
        // asks for its reading where it stands, and reads it within the same program.
        self.arrive(target)?;
        // The reading a record stands in, and where it stands: a field of it stands one step
        // further down.
        let (parent, base, left_out) = match self.readings.get(&at) {
            Some(Reading::Inside(inside)) => (inside.parent.clone(), inside.steps.clone(), true),
            _ => (at.clone(), Vec::new(), false),
        };
        let found = match self.source(&at) {
            Some((json, _)) => held(json, field),
            // A resource no host here publishes, and none is coming ([`crate::Runtime::set_absent`]),
            // or a record the reading leaves out (a device without Wi-Fi publishes its network
            // with none): its fields read as their types start, and the view goes on to the
            // empty state it draws for a device without the resource.
            None if self.publishes_nothing(target) || left_out => None,
            // Nothing of it arrived: its app is not here, this device has no such reading, or it
            // has yet to come. It reads as it would start, and the view that read it shows it is
            // unknown where it asks [`Self::unreadable`]; the reading that answers replaces it.
            // A record of an app whose code runs here reads the same way: it is published by its
            // own id (an item of a collection the reading lists), and the host is asked for it.
            None => {
                self.want(at, target);
                // The read is one waiting on its reading where that has no start to give
                // (`Step`'s `task`, a required record): the turn's caller shows the place its
                // waiting state, not a fault.
                let value = pending(&field.ty, field.default.as_ref()).map_err(|why| {
                    format!(
                        "`{}` of `{}` before its reading came in: {why}",
                        field.name,
                        target.id_string()
                    )
                });
                match value {
                    Ok(value) => return Ok(value),
                    Err(why) => {
                        self.remember_waiting(&why);
                        return Err(why);
                    }
                }
            }
        };
        let Some((step, json)) = found else {
            // A record the reading leaves out (a device without Wi-Fi publishes its network
            // with none) stands where it would be, and its own fields read as absent too: off,
            // empty, none.
            if let Ty::Record(key) = &field.ty {
                let mut steps = base;
                steps.push(held_key(field).to_owned());
                let value = referred(
                    &format!("{}.{}", target.id_string(), field.name),
                    key,
                    author,
                )?;
                if let Value::Ref(record) = &value {
                    self.place(&record.target, steps, parent);
                }
                return Ok(value);
            }
            if self.publishes_nothing(target) || left_out {
                return start_value(&field.ty, field.default.as_ref(), registry);
            }
            return crate::value::declared_default(&field.ty, field.default.as_ref())
                .map_err(|why| format!("`{}` has no `{}`: {why}", target.id_string(), field.name));
        };
        let (value, records) = {
            let mut decoder = Decoder {
                author,
                registry,
                blobs,
                base: target.id_string(),
                steps: base,
                records: Vec::new(),
            };
            let value = decoder.down(step, json, &field.ty);
            (value, decoder.records)
        };
        let value =
            value.map_err(|why| format!("`{}` of `{}`: {why}", field.name, target.id_string()))?;
        for (record, steps) in records {
            self.place(&record, steps, parent.clone());
        }
        Ok(as_a_field(&field.ty, value))
    }
}

impl Foreign {
    /// The records of another app's type `key` as that app lists them: the reading of its listing
    /// ([`listing_of`]), each item a record of the type, named by its own `id`. A listing that has
    /// not come in yet is asked of the host, and lists nothing until it does; the view that asked
    /// runs again then, as one reading any field of it does.
    ///
    /// # Errors
    ///
    /// Why the host could not read it, or an item that is not a record of the type.
    pub(crate) fn listed(
        &mut self,
        key: &TypeKey,
        author: Option<&ModuleHash>,
        registry: &Registry,
        blobs: Option<&dyn crate::blob::BlobHost>,
    ) -> Result<Vec<Value>, String> {
        let target = listing_of(&format!("{}#{}", key.app, key.name));
        let at = Self::key(&target);
        if !self.arrive(&target)? {
            self.want(at, &target);
            return Ok(Vec::new());
        }
        let Some((json, _)) = self.source(&at) else {
            return Ok(Vec::new());
        };
        let Some(items) = json.get(LISTED) else {
            return Err(format!(
                "the listing of `{}#{}` holds no `{LISTED}`",
                key.app, key.name
            ));
        };
        let items = items.clone();
        let (value, records) = {
            let mut decoder = Decoder {
                author,
                registry,
                blobs,
                base: target.id_string(),
                steps: Vec::new(),
                records: Vec::new(),
            };
            let value = decoder.down(LISTED, &items, &Ty::List(Box::new(Ty::Record(key.clone()))));
            (value, decoder.records)
        };
        let value = value.map_err(|why| format!("the listing of `{}`: {why}", key.name))?;
        for (record, steps) in records {
            self.place(&record, steps, at.clone());
        }
        match value {
            Value::List(items) => Ok(items),
            other => Err(format!(
                "the listing of `{}` read as {}",
                key.name,
                other.kind_name()
            )),
        }
    }
}

/// What the listing of another app's records is named by in place of a record's id: a query
/// over all of that app's records of a type (`query(invocations.Invocation, status: .waiting)`)
/// reads the reading of `<app>#<Type>@*`, which the host follows from the app.
pub const LISTING: &str = tilolang::host::LISTING;

/// Where a listing's reading holds its records: `{"items": [...]}`.
pub const LISTED: &str = "items";

/// The resource the listing of the type `ty` (`<app>#<Type>`) is read as ([`LISTING`]).
#[must_use]
pub fn listing_of(ty: &str) -> ResourceRef {
    ResourceRef::root(LISTING, ty)
}

/// Whether `target` names the listing of its type rather than a record of it.
#[must_use]
pub fn is_listing(target: &ResourceRef) -> bool {
    target.item.is_none() && target.doc == LISTING
}

/// `value`, read as `ty`, as a field of that type reads here. A field of a map reads as the
/// list of its entries, each a key and its value: what the store holds of one, and what
/// `keys()` and `get(key)` of a field are asked of. A map anywhere else in a reading (what an
/// option, a list, another map or a case holds) stays the map it is, which the same two are
/// asked of as of any map code holds.
fn as_a_field(ty: &Ty, value: Value) -> Value {
    match (ty, value) {
        (Ty::Map(_), Value::Map(entries)) => Value::List(
            entries
                .into_iter()
                .map(|(key, value)| Value::Tuple(vec![key, value]))
                .collect(),
        ),
        (_, value) => value,
    }
}

/// Whether `target` is a resource of an app this runtime does not run.
#[must_use]
pub fn is_foreign(target: &ResourceRef) -> bool {
    target.ty.contains('#')
}

/// What a field of a record whose reading has not come in reads as: its declared value, or
/// what its type starts as, a time at the epoch.
fn pending(ty: &Ty, default: Option<&Json>) -> Result<Value, String> {
    match (ty, default) {
        (Ty::Time, None) => Ok(Value::text("1970-01-01T00:00:00Z")),
        (Ty::Date, None) => Ok(Value::text("1970-01-01")),
        (Ty::Identifier, None) => Ok(Value::text("")),
        _ => crate::value::declared_default(ty, default),
    }
}

/// What a field of a resource no host publishes reads as: what its type starts as, as a root
/// nobody opened yet reads. An enum with no declared default starts as its first case, which the
/// language says a field of it starts as.
fn start_value(ty: &Ty, default: Option<&Json>, registry: &Registry) -> Result<Value, String> {
    if let (Ty::Enum(key), None) = (ty, default) {
        let declared = registry
            .enum_of(key)
            .ok_or_else(|| format!("`{}` is not declared by any loaded package", key.name))?;
        let first = declared
            .variants
            .first()
            .filter(|case| case.fields.is_empty())
            .ok_or_else(|| {
                format!(
                    "`{}` starts as no case: its first case carries values",
                    key.name
                )
            })?;
        return Ok(Value::Enum(crate::value::EnumValue {
            ty: declared.id.clone(),
            variant: first.id.clone(),
            fields: BTreeMap::new(),
        }));
    }
    crate::value::declared_default(ty, default)
}

/// The value a field of a record of another app holds, as a filter compares it: what
/// `items.matching(q)` over a list of that app's resources reads, as the store's filters read
/// a field the store holds. A set is its members, a list how many it holds.
pub(crate) fn lit_of_value(kind: crate::meta::FieldKind, value: &Value) -> Option<Lit> {
    Some(match value {
        Value::Bool(b) => Lit::Bool(*b),
        Value::Int(i) => Lit::Int(*i),
        Value::Float(f) => Lit::Float(tilolang::pred::Float(*f)),
        Value::Text(t) => Lit::Text(t.clone()),
        Value::Opt(None) | Value::Unit => Lit::Absent,
        Value::Opt(Some(inner)) => return lit_of_value(kind, inner),
        Value::Enum(e) => Lit::Variant(e.variant.clone()),
        Value::Ref(r) => Lit::Ref(r.target.id_string()),
        Value::Actor(who) => Lit::Actor(who.clone()),
        Value::List(items) if kind == crate::meta::FieldKind::Set => Lit::Set(
            items
                .iter()
                .map(|item| lit_of_value(kind, item))
                .collect::<Option<Vec<_>>>()?,
        ),
        Value::List(items) => Lit::Int(i64::try_from(items.len()).unwrap_or(i64::MAX)),
        _ => return None,
    })
}

/// The view state slot a foreign resource's reading moves, so that a view which read it runs
/// again at the next poll. Its type and id name it, as [`Foreign::key`] does.
#[must_use]
pub fn reading_key(target: &ResourceRef) -> StateKey {
    reading_key_at(&Foreign::key(target))
}

/// [`reading_key`] of the resource whose key ([`Foreign::key`]) is `key`.
pub(crate) fn reading_key_at(key: &str) -> StateKey {
    StateKey::runtime(
        crate::view_state::LENS_WINDOW,
        None,
        format!("$reading:{key}"),
    )
}

/// The resource a [`reading_key`] names, when `state` is one: its type and id.
#[must_use]
pub fn reading_target(state: &str) -> Option<ResourceRef> {
    let (ty, id) = state.strip_prefix("$reading:")?.split_once('@')?;
    ResourceRef::parse_id(id, ty.to_owned())
}

/// The view state slot that moves every time any reading of another app is set here: what a
/// view call waiting on a reading it does not yet know runs again on. A placeholder reads it,
/// so it re-runs and draws its record the moment that reading arrives, whatever resource it is
/// (a call that failed does not know which of the resources it read was missing).
#[must_use]
pub fn arrived_key() -> StateKey {
    StateKey::runtime(crate::view_state::LENS_WINDOW, None, "$readings-arrived")
}

/// The view state slot that moves when a resource of another app is first known here: a view
/// that looked one up by id ([`Foreign::type_of`]) and found nothing runs again.
#[must_use]
pub fn known_key() -> StateKey {
    StateKey::runtime(crate::view_state::LENS_WINDOW, None, "$readings")
}

/// The view state slot that moves when another app's package arrives here: a view call that
/// read a resource of a type no loaded package declared ([`WantedPackages`]) runs again, and
/// draws it through its app's own views.
#[must_use]
pub fn packages_key() -> StateKey {
    StateKey::runtime(crate::view_state::LENS_WINDOW, None, "$packages-arrived")
}

/// The apps whose package a view read a resource of, or called a view of, before the package
/// was loaded: what it named, by app. The host loads each app's package
/// ([`crate::Runtime::load_foreign_package`]), and the view that read it runs again then
/// ([`packages_key`]).
///
/// Each app is asked for once: a package that never declares the type or the view it was asked
/// for, or one no host can serve, leaves the view call its waiting state instead of asking
/// again every frame. Unloading the app forgets the ask, so a package that comes again is
/// loaded again.
#[derive(Debug, Default)]
pub struct WantedPackages {
    /// What a view read of each app, by app, since the host last took them: the type it read a
    /// resource of (`app#Type`), or the app itself for a view call (`view:<app>:<view id>`).
    wanted: BTreeMap<String, String>,
    /// Every app asked for, by app: an app is asked for once until it is unloaded.
    asked: std::collections::BTreeSet<String>,
    /// How many packages arrived here: the value the slot that moves on each stands at.
    arrivals: u64,
}

impl WantedPackages {
    /// Ask for the package that declares `ty`, a type a view read a resource of (`app#Type`).
    /// Whether such a package can arrive at all: `false` for a type that is no app's (a bare
    /// type id), whose read is a fault, not one waiting.
    pub(crate) fn want(&mut self, ty: &str) -> bool {
        let Some(key) = crate::meta::foreign_type(ty) else {
            return false;
        };
        // The standard library is loaded beside every package and is no app a session serves:
        // a view that reads of it waits on nothing ([`stdlib`](tilolang::stdlib)).
        if key.app == tilolang::stdlib::APP {
            return false;
        }
        if self.asked.insert(key.app.clone()) {
            self.wanted.insert(key.app, ty.to_owned());
        }
        true
    }

    /// Ask for the package of `app`, whose view a view call composes (`view:<app>:<view id>`):
    /// a view of another app is that app's code, as a resource of its type is its app's record.
    pub(crate) fn want_app(&mut self, app: &str) {
        if app == tilolang::stdlib::APP {
            return;
        }
        if self.asked.insert(app.to_owned()) {
            self.wanted.insert(app.to_owned(), app.to_owned());
        }
    }

    /// The apps asked for since the last call, each with the type as the view named it: what
    /// the host serves.
    pub(crate) fn take(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.wanted).into_iter().collect()
    }

    /// Whether an app a view asked for has not been loaded yet.
    pub(crate) fn wants(&self) -> bool {
        !self.wanted.is_empty()
    }

    /// The app `app`'s package arrived: nothing more is asked of it. How many packages arrived
    /// here so far ([`packages_key`] moves with it).
    pub(crate) fn arrived(&mut self, app: &str) -> u64 {
        self.wanted.remove(app);
        self.arrivals = self.arrivals.saturating_add(1);
        self.arrivals
    }

    /// The app `app`'s package was unloaded: a view that reads a resource of it asks again.
    pub(crate) fn forget(&mut self, app: &str) {
        self.wanted.remove(app);
        self.asked.remove(app);
    }
}

/// The view calls that failed, by the fragment's key in the tree: each its app, view and reason.
///
/// A view call fails in one of two ways, and both end in a failed state rather than a fault of
/// the whole screen: its package could not be loaded here (the host refused it: it does not
/// compile against this runtime's packages, or it is not one), or its view failed when it ran
/// (a trap). What the failure is stands until the call runs again and succeeds, so a screen
/// that shows it reports it once however often it draws.
#[derive(Debug, Default)]
pub struct FailedViews {
    /// What failed, by the call's key in the tree.
    failed: BTreeMap<String, ViewFailure>,
    /// The apps whose package the host could not load here, each with why: a view of theirs is
    /// a failed state rather than a wait, since nothing more is coming.
    apps: BTreeMap<String, String>,
    /// The apps whose package failed to load and then loaded: what the host says the person's
    /// choice is back.
    recovered: Vec<String>,
    /// The view calls whose failure ended — the call ran again and succeeded: what the host
    /// says the person's view is back.
    recovered_views: Vec<ViewFailure>,
    /// Answers the host has not taken yet: what it says the person should hear about.
    said: Vec<ViewFailure>,
}

impl FailedViews {
    /// The package of `app` did not load, for `reason`: every call of one of its views draws a
    /// failed state naming it, now and until the app's package arrives.
    pub(crate) fn app_failed(&mut self, app: &str, reason: String) {
        self.apps.insert(app.to_owned(), reason);
    }

    /// The app's package arrived after all: it is no longer failed to load, and the host is
    /// told so ([`FailedViews::take_recovered`]) — a style that failed comes back.
    pub(crate) fn app_arrived(&mut self, app: &str) {
        if self.apps.remove(app).is_some() {
            self.recovered.push(app.to_owned());
        }
    }

    /// The apps whose package failed to load and now loaded: what the host says the person's
    /// choice is back (`shell.md` §6, "Nothing can lock a person out"). Each once.
    pub(crate) fn take_recovered(&mut self) -> Vec<String> {
        std::mem::take(&mut self.recovered)
    }

    /// Why the package of `app` failed to load, when it did.
    #[must_use]
    pub fn failed_app(&self, app: &str) -> Option<&str> {
        self.apps.get(app).map(String::as_str)
    }

    /// `failure` is what the call at `key` draws now. Whether it is news: the first time that
    /// call fails, or the first time it fails for a different reason.
    pub(crate) fn note(&mut self, key: &str, failure: ViewFailure) -> bool {
        match self.failed.get(key) {
            Some(was) if *was == failure => false,
            _ => {
                self.said.push(failure.clone());
                self.failed.insert(key.to_owned(), failure);
                true
            }
        }
    }

    /// The call at `key` runs again and succeeded: it is no longer failed, and if it had
    /// failed the host is told the view came back (`take_recovered`).
    pub(crate) fn cleared(&mut self, key: &str) {
        if let Some(was) = self.failed.remove(key) {
            self.recovered_views.push(was);
        }
    }

    /// The views whose failure ended since the last call — the call ran again and succeeded:
    /// what the host says the person's choice is back (`shell.md` §6). Each once.
    pub(crate) fn take_recovered_views(&mut self) -> Vec<ViewFailure> {
        std::mem::take(&mut self.recovered_views)
    }

    /// What the host has not been told about yet, each once.
    pub(crate) fn take(&mut self) -> Vec<ViewFailure> {
        std::mem::take(&mut self.said)
    }

    /// Whether any call draws a failed state now.
    #[must_use]
    pub fn any(&self) -> bool {
        !self.failed.is_empty()
    }
}

/// `json` as the value `ty` declares, as a reading of another app publishes it.
///
/// A reading is plain JSON — what its service serialised (a number for an `Int`, a string for
/// a `Text`, an object for a record's fields) — and a value the runtime itself wrote is the
/// [`crate::value::to_json`] encoding of it, which is read too.
///
/// A record is a resource of the app that published the reading, as every record is in
/// tilolang: it reads as a reference, and its fields are the record's own, checked against the
/// types the app declares. A record the reading spells out as an object names itself by its
/// `id` when it has one (a menu's rows), and by where it stands in the reading when it does not
/// (a menu's corner, or a row that chooses nothing and gives no id); a reading that gives it as
/// an id string or as that encoding is the same reference.
struct Decoder<'a> {
    author: Option<&'a ModuleHash>,
    registry: &'a Registry,
    /// The device's content store, for bytes a reading names by address: none where the reader
    /// keeps no content.
    blobs: Option<&'a dyn crate::blob::BlobHost>,
    /// The id of the record the field read belongs to: what a record without an id of its own
    /// is named after.
    base: String,
    /// Where the value being read stands in the reading: the fields and items down to it.
    steps: Vec<String>,
    /// The records the reading spells out, and where each stands: what a view reads the fields
    /// of later, as the reading says then.
    records: Vec<(Rc<ResourceRef>, Vec<String>)>,
}

impl Decoder<'_> {
    /// The name of a record that gives no id of its own: the id of the record whose field it
    /// is, and where it stands in the reading.
    fn at(&self) -> String {
        let mut at = self.base.clone();
        for step in &self.steps {
            at.push('.');
            at.push_str(step);
        }
        at
    }

    /// `json` as `ty`, where [`Decoder::steps`] says.
    fn read(&mut self, json: &Json, ty: &Ty) -> Result<Value, String> {
        match ty {
            Ty::Unit
            | Ty::Int
            | Ty::Float
            | Ty::Bool
            | Ty::Text
            | Ty::Date
            | Ty::Time
            | Ty::Identifier
            | Ty::Actor
            | Ty::Filter(_)
            | Ty::Length(_) => plain(json, ty),
            Ty::Option(inner) => match json {
                Json::Null => Ok(Value::none()),
                value if tagged(value, "none").is_some() => Ok(Value::none()),
                value => {
                    let held = tagged(value, "some").unwrap_or(value);
                    Ok(Value::some(self.down("", held, inner)?))
                }
            },
            // No reading gives one: a length of time is never a field's type.
            Ty::Duration => Err("a Duration is not a value a record holds".to_owned()),
            Ty::List(inner) | Ty::Collection(inner) | Ty::Set(inner) => {
                let Some(items) = tagged(json, "list").unwrap_or(json).as_array() else {
                    return Err(format!("{json} is not a list"));
                };
                let mut values = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    values.push(self.down(&index.to_string(), item, inner)?);
                }
                Ok(Value::List(values))
            }
            Ty::Tuple(items) => {
                let Some(given) = tagged(json, "tuple").unwrap_or(json).as_array() else {
                    return Err(format!("{json} is not a tuple"));
                };
                if given.len() != items.len() {
                    return Err(format!("{json} is not a tuple of {} values", items.len()));
                }
                let mut values = Vec::with_capacity(items.len());
                for (index, (ty, item)) in items.iter().zip(given).enumerate() {
                    values.push(self.down(&index.to_string(), item, ty)?);
                }
                Ok(Value::Tuple(values))
            }
            Ty::Record(key) => self.record(json, key),
            Ty::Union(keys) => one_of(json, keys, self.author),
            Ty::Enum(key) => match json {
                // A reading names the case as its service writes it (the enum's kebab-case
                // name); the value holds the declared case's id, as every enum value here
                // does, so a view's `== .wifi` asks the same thing of both.
                Json::String(case) => Ok(Value::Enum(self.registry.enum_value(key, case)?)),
                // A case that carries values, as a data enum is written in JSON.
                Json::Object(spelled) if spelled.contains_key(tilo_resource::VARIANT_KEY) => {
                    self.case(json, key)
                }
                encoded => crate::value::from_json(encoded),
            },
            Ty::Result(..) | Ty::RichText => crate::value::from_json(json),
            // Bytes as the stored form a reading carries: the data itself when small, else the
            // content store's address, read here when this device holds it.
            Ty::Bytes => crate::store::BytesData::from_json(json).and_then(|data| match data {
                crate::store::BytesData::Inline(bytes) => Ok(Value::Bytes(bytes)),
                crate::store::BytesData::Kept { hash, .. } => self
                    .blobs
                    .and_then(|blobs| blobs.read(&hash))
                    .map(|bytes| Value::Bytes(Rc::new(bytes)))
                    .ok_or_else(|| format!("the bytes (`{hash}`) are not on this device")),
            }),
            Ty::Map(inner) => self.map(json, inner),
            Ty::Action
            | Ty::View
            | Ty::Choice(_)
            | Ty::Sort(_)
            | Ty::Response
            | Ty::Json
            | Ty::File
            | Ty::Error => Err(format!(
                "a value of type `{}` is not read from another app's reading",
                ty.meta_text("")
            )),
        }
    }

    /// `json` as `ty`, one step further down the reading with `step` (none for a value that
    /// stands where its container does, as an option's).
    fn down(&mut self, step: &str, json: &Json, ty: &Ty) -> Result<Value, String> {
        if step.is_empty() {
            return self.read(json, ty);
        }
        self.steps.push(step.to_owned());
        let value = self.read(json, ty);
        self.steps.pop();
        value
    }

    /// A record of `key`'s type: the reference to it, once every field it declares is a value
    /// of its declared type. The fields are read again from where the reading holds it, one at
    /// a time, as a view asks for them.
    fn record(&mut self, json: &Json, key: &TypeKey) -> Result<Value, String> {
        if !json.is_object() || json.get("ref").is_some() {
            return reference(json, key, self.author);
        }
        let (_, info) = self
            .registry
            .type_for(key)
            .and_then(|id| self.registry.type_info(&id))
            .ok_or_else(|| {
                format!(
                    "`{}#{}` is not declared by any loaded package",
                    key.app, key.name
                )
            })?;
        for field in &info.fields {
            let Some((step, value)) = held(json, field) else {
                // A field the reading leaves out is none, empty (a service leaves out a list
                // it holds nothing in), or the value the type declares for it; one that has
                // none of those is missing.
                if matches!(
                    field.ty,
                    Ty::Option(_) | Ty::List(_) | Ty::Set(_) | Ty::Map(_)
                ) || field.default.is_some()
                {
                    continue;
                }
                return Err(format!(
                    "`{}` of `{}` is missing from the reading",
                    field.name, info.name
                ));
            };
            self.down(step, value, &field.ty)
                .map_err(|why| format!("`{}` of `{}`: {why}", field.name, info.name))?;
        }
        // A record names itself with its resource's address when it is a resource of its own
        // (a Wi-Fi network in the network reading), with its `id` when the reading gives one (a
        // menu's row), and by where it stands in the reading when it gives neither (a menu's
        // corner). A row that chooses nothing gives its id as empty, which names nothing either.
        let id = json
            .get(tilo_actions::surface::ADDRESS_KEY)
            .or_else(|| json.get("id"))
            .and_then(Json::as_str)
            .filter(|id| !id.is_empty())
            .map_or_else(|| self.at(), ToOwned::to_owned);
        let value = referred(&id, key, self.author)?;
        if let Value::Ref(target) = &value {
            let stands = (Rc::clone(&target.target), self.steps.clone());
            self.records.push(stands);
        }
        Ok(value)
    }

    /// A case of `key`'s enum that carries values, spelled the way a data enum is written in
    /// JSON ([`tilo_resource::VARIANT_KEY`]): the case's name beside its values by name. Each
    /// value is read as the type the case declares for it, the way a declared type's field
    /// is, so a resource a case carries is one a view reads the fields of. A plain case
    /// spelled that way is the case alone.
    fn case(&mut self, json: &Json, key: &TypeKey) -> Result<Value, String> {
        let name = json
            .get(tilo_resource::VARIANT_KEY)
            .and_then(Json::as_str)
            .ok_or_else(|| format!("{json} does not name its case"))?;
        let declared = self.registry.enum_of(key).ok_or_else(|| {
            format!(
                "`{}.{}` is not declared by any loaded package",
                key.app, key.name
            )
        })?;
        let variant = declared
            .variants
            .iter()
            .find(|variant| variant.name == name || variant.id == name)
            .ok_or_else(|| format!("`{name}` is not a case of `{}`", key.name))?;
        let mut fields = BTreeMap::new();
        for field in &variant.fields {
            let ty = crate::meta::parse_ty(&field.ty, &key.app)?;
            // A value stands under its name, or under the key its id keeps.
            let kept = field
                .id
                .rsplit_once('.')
                .map_or(field.name.as_str(), |(_, kept)| kept);
            let found = [field.name.as_str(), kept]
                .into_iter()
                .find_map(|at| json.get(at).map(|value| (at, value)));
            let value = match found {
                Some((step, value)) => self
                    .down(step, value, &ty)
                    .map_err(|why| format!("`{}` of `.{}`: {why}", field.name, variant.name))?,
                // A value the reading leaves out is none where the case declares an option.
                None if matches!(ty, Ty::Option(_)) => Value::none(),
                None => {
                    return Err(format!(
                        "`{}` of `.{}` is missing from the reading",
                        field.name, variant.name
                    ));
                }
            };
            fields.insert(field.id.clone(), value);
        }
        Ok(Value::Enum(crate::value::EnumValue {
            ty: declared.id.clone(),
            variant: variant.id.clone(),
            fields,
        }))
    }

    /// A map, spelled as the object of its entries: each key once, in the order of the keys,
    /// with its value read as the type the map declares, the way a declared type's field is,
    /// so a resource a map holds is one a view reads the fields of. Entries the runtime
    /// itself encoded ([`encoded_entries`]) read as they did.
    fn map(&mut self, json: &Json, inner: &Ty) -> Result<Value, String> {
        if encoded_entries(json) {
            return crate::value::from_json(json);
        }
        let Json::Object(entries) = json else {
            return Err(format!("{json} is not a map"));
        };
        let held: BTreeMap<&String, &Json> = entries.iter().collect();
        let mut read = Vec::with_capacity(held.len());
        for (key, value) in held {
            let value = self
                .down(key, value, inner)
                .map_err(|why| format!("the entry `{key}`: {why}"))?;
            read.push((Value::Text(key.clone()), value));
        }
        Ok(Value::Map(read))
    }
}

/// A value of the scalar type `ty` as a reading writes it: a number, a flag, or the text it
/// is held as.
fn plain(json: &Json, ty: &Ty) -> Result<Value, String> {
    let value = match ty {
        Ty::Unit => {
            return match json {
                Json::Null => Ok(Value::Unit),
                other => Err(format!("{other} is not none")),
            };
        }
        Ty::Int => json.as_i64().map(Value::Int),
        Ty::Float | Ty::Length(_) => json.as_f64().map(Value::Float),
        Ty::Bool => json.as_bool().map(Value::Bool),
        Ty::Text | Ty::Date | Ty::Time | Ty::Identifier => json.as_str().map(Value::text),
        // A person a service names by their root key (an invocation's `person`), as the
        // workspace names its members everywhere outside a record.
        Ty::Actor if json.is_string() => json
            .as_str()
            .filter(|key| !key.is_empty())
            .and_then(|key| tilo_actions::policy::PersonId::parse(key).ok())
            .map(|person| {
                Value::Actor(crate::ids::Actor::Member(crate::ids::MemberId::Person(
                    person,
                )))
            }),
        _ => tagged(json, "actor")
            .and_then(|who| serde_json::from_value(who.clone()).ok())
            .map(Value::Actor),
    };
    value.ok_or_else(|| format!("{json} is not a {}", ty.meta_text("")))
}

/// The key a reading holds `field` under: the end of its id, which keeps the key a kebab-cased
/// reading spells (`user-switching`) where the name is an identifier (`user_switching`).
fn held_key(field: &crate::registry::FieldInfo) -> &str {
    field
        .id
        .rsplit_once('.')
        .map_or(field.name.as_str(), |(_, key)| key)
}

/// `field` of the record `json` spells out, and the key it stands under: by its name, the key
/// its id keeps, or its id.
fn held<'a>(json: &'a Json, field: &'a crate::registry::FieldInfo) -> Option<(&'a str, &'a Json)> {
    [field.name.as_str(), held_key(field), field.id.as_str()]
        .into_iter()
        .find_map(|key| json.get(key).map(|value| (key, value)))
}

/// What `json` holds when it is the runtime's own encoding of a value tagged `tag`
/// ([`crate::value::to_json`]: `{"list": [...]}`, `{"some": ...}`): an object of that one key.
fn tagged<'a>(json: &'a Json, tag: &str) -> Option<&'a Json> {
    match json {
        Json::Object(fields) if fields.len() == 1 => fields.get(tag),
        _ => None,
    }
}

/// Whether `json` is a map's entries in the runtime's own encoding (`{"list": [{"tuple": [key,
/// value]}, ..]}`): what [`crate::value::to_json`] writes of a field of a map, and the one form
/// a reading could give a map in before it could spell one as an object.
fn encoded_entries(json: &Json) -> bool {
    tagged(json, "list")
        .and_then(Json::as_array)
        .is_some_and(|items| items.iter().all(|item| tagged(item, "tuple").is_some()))
}

/// A reference to a resource of one of the union's types: the type the reading names, since an
/// id on its own does not say which of them it is.
fn one_of(json: &Json, keys: &[TypeKey], author: Option<&ModuleHash>) -> Result<Value, String> {
    let Some((app, name)) = encoded_ref_ty(json) else {
        return Err(format!(
            "{json} does not say which of a union's types it is"
        ));
    };
    let key = keys
        .iter()
        .find(|key| key.app == app && key.name == name)
        .ok_or_else(|| format!("`{app}#{name}` is not one of this field's types"))?;
    reference(json, key, author)
}

/// The app and type name an encoded reference names.
fn encoded_ref_ty(json: &Json) -> Option<(String, String)> {
    let ty = json.get("ty")?.as_str()?;
    let (app, name) = ty.split_once('#')?;
    Some((app.to_owned(), name.to_owned()))
}

/// `json` as a reference to a resource of `key`'s type: an id string, an encoded reference, or
/// a record spelled out with the `id` it names.
fn reference(json: &Json, key: &TypeKey, author: Option<&ModuleHash>) -> Result<Value, String> {
    let id = match json {
        Json::String(id) => id.clone(),
        Json::Object(fields) => match ["ref", "id"]
            .iter()
            .find_map(|name| fields.get(*name).and_then(Json::as_str))
        {
            Some(id) => id.to_owned(),
            None => return Err(format!("{json} is not a reference")),
        },
        other => return Err(format!("{other} is not a reference")),
    };
    referred(&id, key, author)
}

/// A reference to the resource `id` names, of `key`'s type.
fn referred(id: &str, key: &TypeKey, author: Option<&ModuleHash>) -> Result<Value, String> {
    let ty = format!("{}#{}", key.app, key.name);
    let target =
        ResourceRef::parse_id(id, ty).ok_or_else(|| format!("`{id}` is not a resource id"))?;
    Ok(Value::Ref(RefValue {
        target: Rc::new(target),
        author: author.cloned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource a place names by the path the session publishes it at
    /// (`chat.workspace/channels/<id>`) is the same resource its reading's key holds in the one
    /// canonical form (`chat.workspace/<id>`): `type_of` finds it, so `resource(id)` names the
    /// reading and a place draws it rather than showing nothing.
    #[test]
    fn a_resource_named_by_its_published_path_is_found_by_its_canonical_id() {
        let mut foreign = Foreign::default();
        let published = ResourceRef::parse_id("chat.workspace/channels/c_1", "chat#Channel")
            .expect("the published path is an id");
        let reading = serde_json::json!({"name": "general"});
        assert!(foreign.set_reading(&published, reading));
        // The key holds the canonical id, the path's document and its last item.
        assert_eq!(published.id_string(), "chat.workspace/c_1");
        // A view asks for the type by the path it was handed (`resource(p.record)`), not the
        // canonical form: both find the one reading.
        assert_eq!(
            foreign.type_of("chat.workspace/channels/c_1"),
            Some("chat#Channel".to_owned()),
            "the path a place names finds the reading its canonical id holds"
        );
        assert_eq!(
            foreign.type_of("chat.workspace/c_1"),
            Some("chat#Channel".to_owned()),
            "the canonical id finds it too"
        );
        assert_eq!(
            foreign.type_of("chat.workspace/channels/none"),
            None,
            "a resource with no reading finds none"
        );
    }

    /// A root nothing published is unknown, in words, rather than a read that fails: a device
    /// without the service, or a platform that has no such reading, draws the row that says so.
    /// A resource of an app no host here publishes at all reads as its type starts and is not
    /// unknown — there is nothing there to be unknown about.
    #[test]
    fn a_root_nothing_published_is_unknown_rather_than_absent() {
        let mut foreign = Foreign::default();
        let privacy = ResourceRef::parse_id("session:privacy", "so.tilo.device#Privacy".to_owned())
            .expect("a resource id");
        assert_eq!(
            foreign
                .unreadable(&privacy)
                .expect("nothing published is answered"),
            Some("Nothing publishes it here".to_owned()),
            "a root nothing published says so"
        );

        let printers = ResourceRef::parse_id("printers", "so.tilo.device#Printers".to_owned())
            .expect("a resource id");
        foreign.absent(&printers);
        assert_eq!(
            foreign.unreadable(&printers).expect("answered"),
            None,
            "a host that publishes nothing of it reads as its type starts"
        );
    }
}
