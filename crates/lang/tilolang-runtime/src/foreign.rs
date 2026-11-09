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
}

/// The way down to a record inside a reading: a field's name, or an item's index.
#[derive(Debug)]
struct Inside {
    parent: String,
    steps: Vec<String>,
}

/// Results a value carries that `show` prints for the resources of apps the runtime does not
/// hold: how each printed, and its reading.
#[derive(Debug, Default)]
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
}

impl Foreign {
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

    /// What links to `target`, each with how, as the reading of it says (`$links`): a resource
    /// of another app carries its links in its reading, as it carries its fields, because the
    /// store that keeps them runs where the app's code runs, not here.
    ///
    /// # Errors
    ///
    /// A message when no reading of it arrived yet, kept as a read waiting on one.
    pub(crate) fn links(
        &self,
        target: &ResourceRef,
    ) -> Result<Vec<(ResourceRef, crate::store::LinkHow)>, String> {
        let at = Self::key(target);
        if !self.readings.contains_key(&at) {
            let why = format!(
                "no reading of `{}` arrived yet: its app publishes it through the session",
                target.id_string()
            );
            self.remember_waiting(&why);
            return Err(why);
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

    /// The resources a view read before their readings came in, each its id and type, since
    /// the last call: what the host asks the session to publish. Each is asked for once.
    pub(crate) fn take_wanted(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.wanted).into_values().collect()
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
        if !self.readings.contains_key(&key) {
            // The host is asked for the resource's reading, as a field read of one yet to arrive
            // asks for it; the boundary runs again when it comes.
            if self.asked.insert(key.clone()) {
                self.wanted
                    .insert(key, (target.id_string(), target.ty.clone()));
            }
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
    ) -> Result<Value, String> {
        let at = Self::key(target);
        // The reading a record stands in, and where it stands: a field of it stands one step
        // further down.
        let (parent, base) = match self.readings.get(&at) {
            Some(Reading::Inside(inside)) => (inside.parent.clone(), inside.steps.clone()),
            _ => (at.clone(), Vec::new()),
        };
        let found = match self.source(&at) {
            Some((json, _)) => held(json, field),
            // A record the reading leaves out stands where the reading would hold it: it has
            // no fields of its own there.
            None if matches!(self.readings.get(&at), Some(Reading::Inside(_))) => None,
            // A record of an app whose code runs here is a resource of its own, published by
            // its id (an item of a collection the reading lists): the host is asked for its
            // reading, and until it comes the record reads as it would start. The view that
            // read it runs again when it does.
            None if runs_here(target, registry) => {
                if self.asked.insert(at.clone()) {
                    self.wanted
                        .insert(at, (target.id_string(), target.ty.clone()));
                }
                // The record reads as it would start, and the read is one waiting on its
                // reading where that has no start to give (`Step`'s `task`, a required
                // record): the turn's caller shows the place its waiting state, not a fault.
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
            None => {
                let why = format!(
                    "no reading of `{}` arrived yet: its app publishes it through the session",
                    target.id_string()
                );
                self.remember_waiting(&why);
                return Err(why);
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
            return crate::host_res::declared_default(&field.ty, field.default.as_ref())
                .map_err(|why| format!("`{}` has no `{}`: {why}", target.id_string(), field.name));
        };
        let (value, records) = {
            let mut decoder = Decoder {
                author,
                registry,
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
        Ok(value)
    }
}

/// Whether `target` is a resource of an app this runtime does not run.
#[must_use]
pub fn is_foreign(target: &ResourceRef) -> bool {
    target.ty.contains('#')
}

/// Whether `target`'s app is one whose code runs here (`Runtime::load_foreign_package`): a
/// tilolang app, each of whose records the session publishes as a resource of its own. A
/// description's records (a menu's rows) stand only inside the readings that hold them.
fn runs_here(target: &ResourceRef, registry: &Registry) -> bool {
    foreign_type(&target.ty)
        .and_then(|key| registry.type_for(&key))
        .and_then(|ty| registry.type_info(&ty))
        .is_some_and(|(package, _)| package.code)
}

/// What a field of a record whose reading has not come in reads as: its declared value, or
/// what its type starts as, a time at the epoch.
fn pending(ty: &Ty, default: Option<&Json>) -> Result<Value, String> {
    match (ty, default) {
        (Ty::Time, None) => Ok(Value::text("1970-01-01T00:00:00Z")),
        (Ty::Date, None) => Ok(Value::text("1970-01-01")),
        (Ty::Identifier, None) => Ok(Value::text("")),
        _ => crate::host_res::declared_default(ty, default),
    }
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

/// The type of a foreign resource as its description names it: the app and the type's name.
#[must_use]
pub fn foreign_type(ty: &str) -> Option<TypeKey> {
    let (app, name) = ty.split_once('#')?;
    Some(TypeKey {
        app: app.to_owned(),
        name: name.to_owned(),
    })
}

/// The view state slot a foreign resource's reading moves, so that a view which read it runs
/// again at the next poll. Its type and id name it, as [`Foreign::key`] does.
#[must_use]
pub fn reading_key(target: &ResourceRef) -> StateKey {
    StateKey::runtime(
        crate::platform::LENS_WINDOW,
        None,
        format!("$reading:{}", Foreign::key(target)),
    )
}

/// The view state slot that moves every time any reading of another app is set here: what a
/// view call waiting on a reading it does not yet know runs again on. A placeholder reads it,
/// so it re-runs and draws its record the moment that reading arrives, whatever resource it is
/// (a call that failed does not know which of the resources it read was missing).
#[must_use]
pub fn arrived_key() -> StateKey {
    StateKey::runtime(crate::platform::LENS_WINDOW, None, "$readings-arrived")
}

/// The view state slot that moves when a resource of another app is first known here: a view
/// that looked one up by id ([`Foreign::type_of`]) and found nothing runs again.
#[must_use]
pub fn known_key() -> StateKey {
    StateKey::runtime(crate::platform::LENS_WINDOW, None, "$readings")
}

/// The view state slot that moves when another app's package arrives here: a view call that
/// read a resource of a type no loaded package declared ([`WantedPackages`]) runs again, and
/// draws it through its app's own views.
#[must_use]
pub fn packages_key() -> StateKey {
    StateKey::runtime(crate::platform::LENS_WINDOW, None, "$packages-arrived")
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
        let Some(key) = foreign_type(ty) else {
            return false;
        };
        if self.asked.insert(key.app.clone()) {
            self.wanted.insert(key.app, ty.to_owned());
        }
        true
    }

    /// Ask for the package of `app`, whose view a view call composes (`view:<app>:<view id>`):
    /// a view of another app is that app's code, as a resource of its type is its app's record.
    pub(crate) fn want_app(&mut self, app: &str) {
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

/// A view call that could not be shown: its package did not load, or its view failed when it
/// ran. What a `show` boundary draws in its failed state instead of a blank frame or an endless
/// waiting state, and what it reports to its host ([`FailedViews`]) so the session can say so
/// (shell.md §6, "Nothing can lock a person out").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewFailure {
    /// The app whose view could not be shown.
    pub app: String,
    /// The view's name, as the caller named it: its id or name, without its package.
    pub view: String,
    /// Why, in the runtime's own words: the load's refusal, or what the view failed with.
    pub reason: String,
}

impl std::fmt::Display for ViewFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.view.is_empty() {
            write!(f, "`{}` could not be shown: {}", self.app, self.reason)
        } else {
            write!(
                f,
                "`{}`'s view `{}` could not be shown: {}",
                self.app, self.view, self.reason
            )
        }
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
    /// told so ([`FailedViews::take_recovered`]) — a desk style that failed comes back.
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

    /// `json` as `ty`, standing where [`Decoder::steps`] says.
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
            | Ty::Principal
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
                encoded => crate::value::from_json(encoded),
            },
            Ty::Result(..) | Ty::RichText | Ty::Map(_) => crate::value::from_json(json),
            Ty::Action
            | Ty::View
            | Ty::Choice(_)
            | Ty::Bytes
            | Ty::Response
            | Ty::Json
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
                // A field the reading leaves out is none, or the value the type declares for
                // it; one that has neither is missing, not empty.
                if matches!(field.ty, Ty::Option(_)) || field.default.is_some() {
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
        // A record names itself with its `id` when the reading gives one (a menu's row), and
        // by where it stands in the reading when it does not (a menu's corner). A row that
        // chooses nothing gives its id as empty, which names nothing either.
        let id = json
            .get("id")
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
        _ => tagged(json, "principal")
            .unwrap_or(json)
            .as_str()
            .map(|who| Value::Principal(crate::ids::Principal::new(who))),
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
}
