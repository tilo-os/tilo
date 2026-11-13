//! What code reads of the platform (`actions(r)`, `lenses(r)`, `meta(r)`, `phrase(...)`) and
//! the value an event carries before it happens.
//!
//! Each answer is a value of the language: a host value type (`host.Action`, `host.Lens`,
//! `host.Meta`, [`tilolang::host::VALUE_TYPES`]) held as a tuple of its fields in their order.
//! What an answer depends on is read through the view's reads, so the view runs again when it
//! moves: an action's label that follows its subject, a title, who is at a resource.

use std::rc::Rc;

use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::handles::{FrameKind, ViewState};
use crate::ids::Principal;
use crate::meta::{ActionMeta, Meta, ParamMeta, TypeMeta};
use crate::registry::Package;
use crate::render::Offer;
use crate::runtime::{Presentation, Reading};
use crate::seen::SeenMark;
use crate::value::{ActionRef, EnumValue, RefValue, ResourceRef, Value};
use crate::view_state::StateKey;
use tilolang::types::Ty;

/// The window the lenses on each resource are kept under, as view state of no window: set by
/// the host as presence moves, read by `lenses(r)`.
pub const LENS_WINDOW: &str = "$lenses";
/// Where the runtime keeps how each resource's outside effects stand (`delivery(r)`).
pub const DELIVERY_WINDOW: &str = "$delivery";

/// One line of what proposing an action with some arguments says: the words a change to the
/// field reads with, and the value they speak of as its type shows it, when they speak of one.
pub(crate) struct Says {
    /// The words, without the value ("set the due date to").
    pub(crate) words: String,
    /// What the words speak of as its type shows it, when they speak of a value.
    pub(crate) value: Option<Value>,
}

/// A name in sentence case: `new_note` and `NewNote` read "New note".
#[must_use]
pub fn sentence(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for ch in name.chars() {
        if ch == '_' || ch == ' ' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if ch.is_uppercase() && !word.is_empty() {
            words.push(std::mem::take(&mut word));
            word.extend(ch.to_lowercase());
        } else {
            word.extend(ch.to_lowercase());
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let joined = words.join(" ");
    let mut chars = joined.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Whether `ty` names a resource (a reference), rather than a value of one.
fn holds_resource(ty: &Ty) -> bool {
    match ty {
        Ty::Union(_) => true,
        Ty::Record(key) => !tilolang::host::is_value(key),
        Ty::Option(inner) => holds_resource(inner),
        _ => false,
    }
}

/// A field's name as a sentence goes on with it: `Notes` reads "notes".
fn field_of_sentence(name: &str) -> String {
    let name = sentence(name);
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

/// The label an action is shown with: as the compiler derived it, else its name in sentence
/// case.
#[must_use]
pub fn label_of(action: &ActionMeta) -> String {
    if action.label.is_empty() {
        sentence(&action.name)
    } else {
        action.label.clone()
    }
}

/// Whether the parameter type `name` (as metadata writes it) names the record type `id`:
/// either that type itself, or one of the types of a union it is written as
/// (`TextChannel | Thread`).
pub(crate) fn names_type(meta: &Meta, name: &str, id: &str) -> bool {
    let Some(ty) = meta.types.iter().find(|ty| ty.id == id) else {
        return false;
    };
    match crate::meta::parse_ty(name, &meta.app) {
        Ok(crate::meta::Ty::Record(key)) => key.name == ty.name,
        Ok(crate::meta::Ty::Union(keys)) => keys.iter().any(|key| key.name == ty.name),
        _ => false,
    }
}

/// Whether an invocation can be offered with only its first parameters given: each of the rest
/// has a default, or is a value its form asks the person for.
pub(crate) fn all_asked(meta: &Meta, params: &[ParamMeta]) -> bool {
    params
        .iter()
        .filter(|param| !param.place)
        .all(|param| param.default.is_some() || form_asks(meta, &param.ty))
}

/// How many parameters of `action` the view offering it supplies: the places that follow its
/// subject (`above: Option<Task> (above)`).
fn places_of(action: &ActionMeta) -> usize {
    action
        .params
        .iter()
        .skip(1)
        .take_while(|param| param.place)
        .count()
}

/// What an offer of `action` on `subject` gives before the parameters it asks for: the subject,
/// then each place the view supplies, from `above` or as nothing when the offer stands in no
/// view of the item.
fn offered_args(action: &ActionMeta, subject: &RefValue, above: &Value) -> Vec<Value> {
    let places = places_of(action);
    let mut args = Vec::with_capacity(places.saturating_add(1));
    args.push(Value::Ref(subject.clone()));
    for _ in 0..places {
        args.push(above.clone());
    }
    args
}

/// Whether an action on a holder asks for one of the items it holds (`remove(tasks, t: Task)`):
/// such an action is offered on each item, which gives it, rather than on the holder.
fn asks_for_own_item(meta: &Meta, holder: &str, rest: &[ParamMeta]) -> bool {
    let Some(holder) = meta.types.iter().find(|ty| ty.id == holder) else {
        return false;
    };
    rest.iter()
        .filter(|param| param.default.is_none())
        .any(|param| {
            let item = param.ty.trim();
            holder.fields.iter().any(|field| {
                field
                    .ty
                    .strip_prefix("Collection<")
                    .and_then(|held| held.strip_suffix('>'))
                    .is_some_and(|held| held.trim() == item)
            })
        })
}

/// Whether an action's form can ask the person for a value of the type `ty` (as metadata writes
/// it): text, a number, a flag, a day or a moment, a person, a resource, or a case of the
/// package's own plain enum, or none of one of those. What a form shows for each is the
/// type's control.
#[must_use]
pub fn form_asks(meta: &Meta, ty: &str) -> bool {
    use crate::meta::Ty;
    fn plain(meta: &Meta, ty: &Ty) -> bool {
        match ty {
            Ty::Enum(key) => {
                key.app == meta.app
                    && meta.enums.iter().any(|declared| {
                        declared.name == key.name
                            && declared.variants.iter().all(|case| case.fields.is_empty())
                    })
            }
            Ty::Option(inner) => plain(meta, inner),
            _ => true,
        }
    }
    crate::meta::parse_ty(ty.trim(), &meta.app)
        .is_ok_and(|parsed| parsed.asked_by_form() && plain(meta, &parsed))
}

/// Whether `ty`, as metadata writes it, is a `File`: the parameter a person's own chooser fills
/// (`file.choose(a)`), which no form asks for and no call gets from anywhere else.
#[must_use]
pub fn chooser_fills(meta: &Meta, ty: &str) -> bool {
    crate::meta::parse_ty(ty.trim(), &meta.app).is_ok_and(|ty| ty == crate::meta::Ty::File)
}

/// The state key `delivery(r)` reads for the resource `id`.
#[must_use]
pub fn delivery_key(id: &str) -> StateKey {
    StateKey::runtime(DELIVERY_WINDOW, None, id)
}

/// The key of the lenses on the resource `id`.
#[must_use]
pub fn lens_key(id: &str) -> StateKey {
    StateKey::runtime(LENS_WINDOW, None, id)
}

/// The seen mark a slot holds ([`crate::Runtime::set_seen`]): `.some((change, at))`.
fn mark_of(held: Option<&Value>) -> Option<SeenMark> {
    let Some(Value::Opt(Some(mark))) = held else {
        return None;
    };
    match &**mark {
        Value::Tuple(fields) => match fields.as_slice() {
            [Value::Text(change), Value::Text(at)] => Some(SeenMark {
                change: change.clone(),
                at: at.clone(),
            }),
            _ => None,
        },
        _ => None,
    }
}

/// What decides whether an action can take an item now, or apply to it where a view offers it.
#[derive(Clone, Copy)]
enum Decides {
    /// Its `require` conditions: a menu shows it disabled, with the reason.
    Requirement,
    /// Its `applies` conditions: a menu leaves it out.
    Applies,
}

/// One action offered on a subject, before it is presented.
pub(crate) struct Offered<'m> {
    pub(crate) action: &'m ActionMeta,
    /// What it is applied to: the subject, or the resource holding the subject and the subject.
    pub(crate) args: Vec<Value>,
    /// Its parameters after those.
    pub(crate) rest: &'m [ParamMeta],
    /// Whether it is presented for the subject: its own actions are, its holder's not.
    pub(crate) present: bool,
    /// The resource the invocation is made from, when it is made from one.
    pub(crate) from: Option<ResourceRef>,
    /// The group the menu folds it under, when it is not the action's own.
    pub(crate) group: Option<String>,
    /// The label the menu gives it, when it is not the action's own.
    pub(crate) label: Option<String>,
}

/// What a make offer is made from: the title the made resource starts with, and the type the
/// subject is, which a parameter of the action may take.
struct Source {
    title: String,
    app: String,
    name: String,
}

impl Source {
    /// Whether the type `ty`, written in the package `app`, is the type of this source.
    fn names_param(&self, ty: &str, app: &str) -> bool {
        let named = |key: &crate::meta::TypeKey| key.app == self.app && key.name == self.name;
        match crate::meta::parse_ty(ty, app) {
            Ok(crate::meta::Ty::Record(key)) => named(&key),
            Ok(crate::meta::Ty::Union(keys)) => keys.iter().any(named),
            _ => false,
        }
    }
}

/// The longest run of a conversation's unread messages a count reads: one page of them, so a
/// workspace nobody has read is not read message by message for a badge. A conversation holding
/// more unread than this says the page it read, its mentions and its unread count alike.
const UNREAD_SCAN: usize = 64;

/// A suggestion's opening words when the host has no locale string for them.
const SAYS_FALLBACK: &str = "{name} suggests:";

impl Core {
    /// `delivery(r)` of the document `doc`: how its last outside effect stands, as the host
    /// last said; a view that read it runs again when that moves.
    fn delivery_of(&self, doc: &str) -> Value {
        let key = delivery_key(doc);
        let (value, generation) = {
            let states = self.view_states.borrow();
            (
                states
                    .get(&key)
                    .cloned()
                    .map_or_else(Value::none, Value::some),
                states.generation(&key),
            )
        };
        self.note_state_read(key, generation, &value);
        value
    }

    pub(crate) fn platform_import(
        &self,
        imp: crate::abi::Imp,
        a: &Args<'_>,
    ) -> HResult<Option<Wasm>> {
        use crate::abi::Imp;
        match imp {
            Imp::EventValue => self.ret_h(Value::Event),
            Imp::Viewer => self.viewer_import(),
            Imp::EventPart => self.ret_h(Value::EventPart(usize::try_from(a.h(0)?).unwrap_or(0))),
            Imp::Actions => {
                let target = self.ref_arg(a.h(0)?)?;
                // Where the view shows the item: the item just above it in its group, which
                // the actions offered get as their place, never one read in storage order.
                let above = self.val(a.h(1)?)?;
                let offered = self.actions_on(&target, &above)?;
                self.ret_h(Value::List(offered))
            }
            Imp::Delivery => {
                let target = self.ref_arg(a.h(0)?)?;
                let value = self.delivery_of(&target.target.doc);
                self.ret_h(value)
            }
            Imp::Lenses => {
                let target = self.ref_arg(a.h(0)?)?;
                let key = lens_key(&target.target.id_string());
                let (value, generation) = {
                    let states = self.view_states.borrow();
                    (
                        states.get(&key).cloned().unwrap_or(Value::List(Vec::new())),
                        states.generation(&key),
                    )
                };
                self.note_state_read(key, generation, &value);
                self.ret_h(value)
            }
            Imp::Versions
            | Imp::VersionEnd
            | Imp::FieldChanged
            | Imp::RunChanges
            | Imp::Unseen
            | Imp::Steps => {
                let value = self.versions_import(imp, a)?;
                self.ret_h(value)
            }
            Imp::Links => {
                let value = self.links_import(a)?;
                self.ret_h(value)
            }
            Imp::Mentions => self.mentions_import(a),
            Imp::FirstUnseen => self.first_unseen_import(a),
            Imp::SeenBy => self.seen_by_import(a),
            Imp::MetaOf => {
                let subject = self.val(a.h(0)?)?;
                let meta = match subject {
                    Value::Text(ty) => self.type_meta(&ty, None)?,
                    Value::Ref(r) => self.type_meta(&r.target.ty, Some(&r))?,
                    other => {
                        return trap(format!(
                            "meta: expected a resource or a type, got {}",
                            other.kind_name()
                        ));
                    }
                };
                self.ret_h(meta)
            }
            Imp::EntryParts => self.entry_import(imp, a),
            Imp::Phrase | Imp::Word => {
                let id = self.text(a.h(0)?)?;
                let args = match self.val(a.h(1)?)? {
                    Value::List(items) => items
                        .into_iter()
                        .filter_map(|item| match item {
                            Value::Tuple(pair) => match pair.as_slice() {
                                [Value::Text(name), Value::Text(value)] => {
                                    Some((name.clone(), value.clone()))
                                }
                                _ => None,
                            },
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                // A phrase is the platform's, known by its id; a view's words fall back to the
                // text as the view wrote it.
                let source = if imp == Imp::Word {
                    self.text(a.h(2)?)?
                } else {
                    id.clone()
                };
                let text = self.word_args(&id, &args, &source);
                self.ret_h(Value::Text(text))
            }
            _ => trap("not a platform import"),
        }
    }

    /// `mentions(c, field, Message)`: how many of `c`'s items name the person looking,
    /// that they have not seen: a channel's mention count, next to its unread count.
    fn mentions_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let coll = self.ref_arg(a.h(0)?)?;
        let field = self.text(a.h(1)?)?;
        let item = self.text(a.h(2)?)?;
        let person = self.looking()?;
        let count = self
            .mentions_of(&person, &coll.target, &field, &item)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        self.read_versions_of(&coll.target);
        self.note_seen_read(&person, &coll.target.id_string());
        crate::core::ret_i64(i64::try_from(count).unwrap_or(i64::MAX))
    }

    /// `first_unseen(c, field, Message)`: where the first unread item of `c`'s
    /// collection begins, read by the frame running, so the view runs again when it
    /// moves.
    fn first_unseen_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let coll = self.ref_arg(a.h(0)?)?;
        let field = self.text(a.h(1)?)?;
        let item = self.text(a.h(2)?)?;
        // What another app's person has not seen is read where that app runs, and comes here in
        // its reading, which the view runs again on when it moves.
        if crate::foreign::is_foreign(&coll.target) {
            self.note_reading(crate::foreign::reading_key(&coll.target));
            let unseen = self.foreign.borrow_mut().unseen(&coll.target)?;
            let first = unseen
                .as_ref()
                .and_then(|unseen| unseen.get("first"))
                .and_then(serde_json::Value::as_str)
                .map(|id| Value::some(Value::resource(&coll.target.item(id, item.clone()))));
            return self.ret_h(first.unwrap_or_else(Value::none));
        }
        let person = self.looking()?;
        let found = self
            .first_unseen_of(&person, &coll.target, &field, &item)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        // The view read what the answer is made of: `c`'s own versions (which move
        // when an item is posted) and the person's seen mark and reviewed runs (which
        // move when they read on).
        self.read_versions_of(&coll.target);
        self.note_seen_read(&person, &coll.target.id_string());
        let value = found.map_or_else(Value::none, |id| {
            let at = coll.target.item(&id, item);
            Value::some(Value::resource(&at))
        });
        self.ret_h(value)
    }

    /// `workspace.me`: the person the view is shown to, which the frame running carries as the
    /// runtime was opened for them. An action reads `invoker` instead: it acts for someone,
    /// where the viewer only looks.
    fn viewer_import(&self) -> HResult<Option<Wasm>> {
        let who = self.looking()?;
        self.ret_h(Value::Principal(crate::ids::Principal::new(who)))
    }

    /// A version of `of` as a `host.Version`: who, when, what, the run, its kind, the
    /// fields it changed by name, the resource, what its action says it does, what it did, its
    /// icon, each field it changed with what it held before and after, and how many items the
    /// change was made over; then the id of the action that made it and of its change, which
    /// code compares rather than reads (`tilolang::host::action_id_at`).
    fn version_value(&self, v: crate::store::Version, of: &ResourceRef) -> HResult<Value> {
        let run = v.run.as_ref().map(|run| Box::new(run_value(run)));
        let info = self.type_of_ref(&of.ty).ok().map(|(_, info)| info);
        let named: Vec<String> = v
            .fields
            .iter()
            .map(|id| {
                info.as_ref()
                    .and_then(|info| info.field(id))
                    .map_or_else(|| id.clone(), |f| f.name.clone())
            })
            .collect();
        let fields = named.iter().cloned().map(Value::Text).collect();
        let declared = self
            .registry
            .borrow()
            .action_package(&v.action)
            .and_then(|pkg| pkg.action(&v.action).cloned());
        // What it did, in the words a sentence goes on with after who did it: the app's own
        // words for the action (`did:`) where it declares them, else what the change itself
        // says (`self.changes_value`), and for one that made or removed the resource, that.
        // Read through the catalogue, so a translation says it in the reader's language.
        let declared_did = self.declared_did(&v);
        let did = match v.kind {
            crate::store::VersionKind::Created if declared_did.is_empty() => {
                self.word("tilolang-did-created", "created it")
            }
            crate::store::VersionKind::Removed if declared_did.is_empty() => {
                self.word("tilolang-did-removed", "removed it")
            }
            _ => declared_did,
        };
        let icon = declared
            .as_ref()
            .and_then(|a| a.icon.clone())
            .map(|icon| Box::new(Value::Text(icon)));
        let mine = v.run.is_none() && self.looking_now().as_deref() == Some(v.who.as_str());
        let actor = self.actor(&v.who, mine);
        // Who proposed the action the person accepted, by the name the workspace calls them.
        let suggested = v.suggested_by.as_ref().map_or_else(String::new, |who| {
            let who = Principal::new(who.clone());
            self.names
                .borrow()
                .as_ref()
                .map_or_else(|| who.0.clone(), |host| host.name(&who))
        });
        let changes = match &info {
            Some(info) => self.changes_value(&v, of, info, self.looking_now().as_deref())?,
            None => Vec::new(),
        };
        Ok(Value::Tuple(vec![
            Value::Principal(Principal::new(v.who)),
            Value::Text(v.at),
            Value::Text(self.action_label(&v.action)),
            Value::Opt(run),
            Value::Text(v.kind.word().to_owned()),
            Value::List(fields),
            Value::resource(of),
            Value::Opt(v.from.as_ref().map(|from| Box::new(Value::resource(from)))),
            Value::Bool(mine),
            Value::Text(actor),
            Value::Text(did),
            Value::Opt(icon),
            Value::List(changes),
            Value::Text(suggested),
            // How many items it changed, read by name; the action and the change it names
            // stand after the fields code reads by name (`host::action_id_at`).
            Value::Int(i64::from(v.count)),
            Value::Text(v.action),
            Value::Text(v.change),
        ]))
    }

    /// The words the action that made `v` declares it does (`did:`), read through the
    /// catalogue, or empty where it declares none.
    fn declared_did(&self, v: &crate::store::Version) -> String {
        self.registry
            .borrow()
            .action_package(&v.action)
            .and_then(|pkg| pkg.action(&v.action).cloned())
            .map_or_else(String::new, |a| match (&a.did, &a.id) {
                (Some(source), id) => {
                    let key = format!("{}.did", tilolang::messages::key_of(id));
                    self.word_args(&key, &[], source)
                }
                (None, _) => String::new(),
            })
    }

    /// What a version of `of` did, as one line of text: the sentence history says of it
    /// (`std.version_said`), its values as their types show them in a phrase. A row says it
    /// under its title, and a step of a catch-up popover reads it, so what a person reads is
    /// worded once wherever it stands.
    pub(crate) fn version_line(
        &self,
        v: &crate::store::Version,
        of: &ResourceRef,
        viewer: &str,
    ) -> String {
        let info = self.type_of_ref(&of.ty).ok().map(|(_, info)| info);
        if v.count > 1 {
            // The items' name in the reader's language, from the type's own catalogue: never an
            // English plural inside a sentence of another language.
            let kind = self
                .item_nouns(&of.ty)
                .map_or_else(String::new, |(_, many)| many);
            return self.word_args(
                "tilolang-did-many",
                &[
                    ("count".to_owned(), v.count.to_string()),
                    ("kind".to_owned(), kind),
                ],
                "changed { $count }",
            );
        }
        let declared = self.declared_did(v);
        if !declared.is_empty() {
            return declared;
        }
        match v.kind {
            crate::store::VersionKind::Created => {
                return self.word("tilolang-did-created", "created it");
            }
            crate::store::VersionKind::Removed => {
                return self.word("tilolang-did-removed", "removed it");
            }
            crate::store::VersionKind::Changed => {}
        }
        let Some(info) = info else {
            return self.action_label(&v.action);
        };
        let said: Vec<String> = self
            .changes_value(v, of, &info, Some(viewer))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|change| {
                let Value::Tuple(parts) = change else {
                    return None;
                };
                let words = parts.get(4).and_then(Value::as_text)?.to_owned();
                let held = match parts.get(5).and_then(Value::as_text) {
                    Some("after") => parts.get(3),
                    Some("before") => parts.get(2),
                    _ => None,
                };
                Some(held.map_or_else(
                    || words.clone(),
                    |held| format!("{words} {}", self.said(held)),
                ))
            })
            .collect();
        if said.is_empty() {
            self.action_label(&v.action)
        } else {
            said.join(" ")
        }
    }

    /// What proposing `action` with `args` would change, in the words history says a change
    /// with ([`crate::did`]): one line per argument it names, over the field of its type
    /// ("set the due date to Saturday", "assigned it to Bo"), holding the value the type
    /// shows it as when the words speak of one. A parameter that names no value says nothing.
    pub(crate) fn proposal_lines(&self, action: &str, args: &[Value]) -> Vec<Says> {
        let params = {
            let reg = self.registry.borrow();
            let pkg = reg.action_package(action);
            match pkg.and_then(|pkg| {
                pkg.action(action)
                    .map(|a| (pkg.meta.app.clone(), a.params.clone()))
            }) {
                Some(found) => found,
                None => return Vec::new(),
            }
        };
        let (app, params) = params;
        let looking = self.looking_now();
        let mut out = Vec::new();
        for (param, arg) in params.iter().zip(args) {
            if matches!(arg, Value::Unit | Value::Opt(None)) {
                continue;
            }
            let Ok(ty) = crate::meta::parse_ty(&param.ty, &app) else {
                continue;
            };
            // The resource an action is about is where the change goes, not what it changes:
            // the sentences say the values proposed for it.
            if holds_resource(&ty) {
                continue;
            }
            let field = crate::registry::FieldInfo {
                id: param.name.clone(),
                name: param.name.clone(),
                ty: ty.clone(),
                kind: crate::meta::FieldKind::Register,
                default: None,
            };
            let named = field_of_sentence(&param.name);
            // A proposal is read against nothing: as a change that sets the value.
            let nothing = self
                .change_shown(&ty, &Value::none(), looking.as_deref())
                .unwrap_or_else(|_| Value::none());
            let verbs = crate::did::said(&field, &named, &nothing, arg);
            let fills: Vec<(String, String)> = verbs
                .field
                .iter()
                .map(|name| ("field".to_owned(), name.clone()))
                .collect();
            let source = match &verbs.field {
                Some(name) => verbs.source.replace("{ $field }", name),
                None => verbs.source.to_owned(),
            };
            let voices = self.word_args(verbs.key, &fills, &source);
            let value = match verbs.shows {
                crate::did::Shows::After => self.change_shown(&ty, arg, looking.as_deref()).ok(),
                _ => None,
            };
            out.push(Says {
                words: voices,
                value,
            });
        }
        out
    }

    /// `proposal_note(r)`: what waits to be proposed of `r`, as one line of text — who
    /// proposed it and each proposal's arguments in the words a change to them reads with,
    /// their values as the person reads them, or the action's name when they say nothing; empty
    /// when nothing waits.
    pub(crate) fn proposal_note(&self, r: i32) -> HResult<Value> {
        let target = self.ref_arg(r)?;
        let mut says: Vec<String> = Vec::new();
        for inv in self.pending_proposals(&target.target) {
            let Ok(rec) = self.read_invocation(&inv) else {
                continue;
            };
            let args = serde_json::from_str::<serde_json::Value>(&rec.args)
                .ok()
                .and_then(|json| crate::value::from_json(&json).ok())
                .and_then(|value| match value {
                    Value::List(args) => Some(args),
                    _ => None,
                })
                .unwrap_or_default();
            let mut lines: Vec<String> = self
                .proposal_lines(&rec.action, &args)
                .into_iter()
                .map(|line| match line.value {
                    Some(value) => format!("{} {}", line.words, self.said(&value)),
                    None => line.words,
                })
                .collect();
            if lines.is_empty() {
                lines.push(self.action_label(&rec.action));
            }
            let name = self.name_of(&rec.author);
            let said = self.word_args(
                "tilolang-suggestion-says",
                &[("name".to_owned(), name)],
                SAYS_FALLBACK,
            );
            says.push(format!("{said} {}", lines.join(", ")));
        }
        if says.is_empty() {
            return Ok(Value::text(""));
        }
        Ok(Value::text(says.join("; ")))
    }

    /// What the workspace calls `who`, as a sentence names them: the person looking as the
    /// platform's word for them, anyone else by their name.
    pub(crate) fn name_of(&self, who: &Principal) -> String {
        let mine = self.looking_now().as_deref() == Some(who.0.as_str());
        self.actor(&who.0, mine)
    }

    /// Each field the version `v` of `of` changed, as a `host.Change`: its name, its name in
    /// words, what it held before and after as its type shows it, and the words history says it
    /// with ([`crate::did`]), read from the change itself. None for a version that made or
    /// removed the resource, or one not committed yet. A time of day that moved with its day
    /// reads with it ("Tomorrow 9:00 AM"), not as a change of its own.
    fn changes_value(
        &self,
        v: &crate::store::Version,
        of: &ResourceRef,
        info: &crate::registry::TypeInfo,
        viewer: Option<&str>,
    ) -> HResult<Vec<Value>> {
        if v.fields.is_empty() || v.change.is_empty() {
            return Ok(Vec::new());
        }
        let specs: Vec<crate::store::FieldSpec> = v
            .fields
            .iter()
            .filter_map(|id| info.field(id))
            .filter(|f| f.kind != crate::meta::FieldKind::Collection)
            .map(|f| crate::store::FieldSpec {
                id: f.id.clone(),
                kind: f.kind,
            })
            .collect();
        if specs.is_empty() {
            return Ok(Vec::new());
        }
        let written = self
            .store
            .borrow()
            .written(of, &specs, &v.change)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        // Each field it wrote, with what it held before and after as its type shows it.
        let mut held: Vec<(&crate::registry::FieldInfo, Value, Value)> =
            Vec::with_capacity(written.len());
        for (id, before, after) in written {
            let Some(field) = info.field(&id) else {
                continue;
            };
            let side = |held: crate::store::FieldValue| -> HResult<Value> {
                let value = self
                    .field_value(of, field, held, None)
                    .map_err(crate::engine::HostFail::Trap)?;
                self.change_shown(&field.ty, &value, viewer)
            };
            held.push((field, side(before)?, side(after)?));
        }
        let mut out = Vec::with_capacity(held.len());
        let mut first = true;
        for (field, before, after) in &held {
            let said = crate::did::said(field, &field_of_sentence(&field.name), before, after);
            let args: Vec<(String, String)> = said
                .field
                .iter()
                .map(|name| ("field".to_owned(), name.clone()))
                .collect();
            let source = match &said.field {
                Some(name) => said.source.replace("{ $field }", name),
                None => said.source.to_owned(),
            };
            let voices = self.word_args(said.key, &args, &source);
            // One sentence for the whole version: who made it stands once, in the version's own
            // row, and "and" goes on with what a later change of the same version did.
            let words = if first {
                voices
            } else {
                format!("{} {voices}", self.word("tilolang-did-and", "and"))
            };
            first = false;
            out.push(Value::Tuple(vec![
                Value::Text(field.name.clone()),
                Value::Text(sentence(&field.name)),
                before.clone(),
                after.clone(),
                Value::Text(words),
                Value::Text(said.shows.word().to_owned()),
            ]));
        }
        Ok(out)
    }

    /// The platform's words for `key`, in the reader's language; `source` where no catalogue
    /// holds it.
    pub(crate) fn word(&self, key: &str, source: &str) -> String {
        self.word_args(key, &[], source)
    }

    /// [`Self::word`] with the `args` a count or a name goes in. A view that shows a word shows
    /// something only the window knows ([`Self::read_person`]): a window that speaks another
    /// language runs again the boundaries that show one, and no others.
    pub(crate) fn word_args(&self, key: &str, args: &[(String, String)], source: &str) -> String {
        self.read_person();
        self.words.borrow().word(key, args, source)
    }

    /// Who made a version, as a sentence names them: the person looking as the platform's word
    /// for them, anyone else by the name the avatar beside it shows.
    pub(crate) fn actor(&self, who: &str, mine: bool) -> String {
        if mine {
            return self.word("tilolang-you", "You");
        }
        self.named(&Principal::new(who.to_owned()))
    }

    /// What the host calls `who`, and that a view showing it read it: the window calling someone
    /// something else runs again the boundaries that show a name, and no others.
    pub(crate) fn named(&self, who: &Principal) -> String {
        self.read_person();
        self.names
            .borrow()
            .as_ref()
            .map_or_else(|| who.0.clone(), |host| host.name(who))
    }

    /// Note that the code running shows something only the window knows, when it draws a view: a
    /// participant's name, a day or a time of day in the person's formats, a word in the language
    /// their window speaks. The boundary runs again when any of them changes, and no other does.
    pub(crate) fn read_person(&self) {
        self.in_view(crate::handles::ViewState::read_person);
    }

    /// `value`, of the type `ty`, as `c.before` and `c.after` show it ([`Self::change_view`]):
    /// the kind of thing it is, what stands for it, and what goes with it (a case's icon, a
    /// person's name). A list of these for a set or a list.
    fn change_shown(
        &self,
        ty: &tilolang::types::Ty,
        value: &Value,
        viewer: Option<&str>,
    ) -> HResult<Value> {
        use tilolang::types::Ty;
        let one = |kind: &str, value: Value, icon: Option<String>| {
            Value::Tuple(vec![
                Value::Text(kind.to_owned()),
                value,
                Value::Opt(icon.map(|icon| Box::new(Value::Text(icon)))),
            ])
        };
        Ok(match (ty, value) {
            (Ty::Option(_), Value::Opt(None)) => one("none", Value::Unit, None),
            (Ty::Option(inner), Value::Opt(Some(held))) => {
                return self.change_shown(inner, held, viewer);
            }
            (Ty::Date, Value::Text(day)) => one("date", Value::Text(day.clone()), None),
            (Ty::Time, Value::Text(at)) => one("time", Value::Text(at.clone()), None),
            (Ty::Principal, Value::Principal(who)) => {
                // A person is named as a sentence names them: the person looking as the word
                // for them ("assigned it to you"), anyone else by their name.
                let name = if viewer == Some(who.0.as_str()) {
                    self.word("tilolang-you-lower", "you")
                } else {
                    self.names
                        .borrow()
                        .as_ref()
                        .map_or_else(|| who.0.clone(), |host| host.name(who))
                };
                one("principal", Value::Principal(who.clone()), Some(name))
            }
            (_, Value::Bool(flag)) => one("bool", Value::Bool(*flag), None),
            (_, Value::Int(_) | Value::Float(_)) => one("number", value.clone(), None),
            (_, Value::Enum(e)) => {
                let case = self.registry.borrow().enum_meta(&e.ty).and_then(|meta| {
                    meta.variants
                        .into_iter()
                        .find(|c| c.id == e.variant || c.name == e.variant)
                });
                match case {
                    Some(case) => one("case", Value::Text(case.label), case.icon),
                    None => one("case", Value::Text(sentence(&e.variant)), None),
                }
            }
            (_, Value::Ref(r)) => {
                let title = match self.type_meta(&r.target.ty, Some(r))? {
                    Value::Tuple(fields) => fields.into_iter().next(),
                    _ => None,
                };
                one(
                    "resource",
                    title.unwrap_or_else(|| Value::Text(String::new())),
                    None,
                )
            }
            (Ty::Set(inner) | Ty::List(inner), Value::List(items)) => Value::List(
                items
                    .iter()
                    .map(|item| self.change_shown(inner, item, viewer))
                    .collect::<HResult<_>>()?,
            ),
            // A map: each entry as its key and what it holds, as people read it ("👍 Ada, Bo").
            (Ty::Map(inner), Value::List(entries)) => Value::List(
                entries
                    .iter()
                    .map(|entry| match entry {
                        Value::Tuple(pair) => match pair.as_slice() {
                            [Value::Text(key), held] => {
                                let shown = self.said(&self.change_shown(inner, held, viewer)?);
                                Ok(one("text", Value::Text(format!("{key} {shown}")), None))
                            }
                            _ => trap("a map entry is a key and its value"),
                        },
                        other => trap(format!("a map entry holds {}", other.kind_name())),
                    })
                    .collect::<HResult<_>>()?,
            ),
            // A file a change put in a field or took out: what it is called, as a person reads
            // it; where its content is, a picture beside the row tells.
            (_, Value::File(file)) => one("text", Value::Text(file.name.clone()), None),
            // A rich text a change made: its paragraphs, the runs it inserted marked and the
            // runs it deleted struck, shown by the shared rich text component.
            (_, Value::Rich(rich)) => one("rich", Value::Rich(rich.clone()), None),
            (_, Value::Text(text)) => one("text", Value::Text(text.clone()), None),
            (_, other) => {
                return trap(format!(
                    "a change of a field of type {ty:?} holds {}",
                    other.kind_name()
                ));
            }
        })
    }

    /// Reflection on versions: `history(r)`, `created(r)` and `updated(r)`, `changes(of:)` and
    /// `unseen(r)`.
    fn versions_import(&self, imp: crate::abi::Imp, a: &Args<'_>) -> HResult<Value> {
        use crate::abi::Imp;
        let trapped = |e: crate::store::StoreError| crate::engine::HostFail::Trap(e.to_string());
        if imp == Imp::RunChanges {
            return self.run_changes_value(a.h(0)?);
        }
        let target = self.ref_arg(a.h(0)?)?;
        let at = &target.target;
        // The history of a resource of an app this runtime does not run is kept where that app
        // runs, and its reading carries none of it: nothing of it is held here.
        if crate::foreign::is_foreign(at) {
            return Ok(match imp {
                Imp::VersionEnd | Imp::FieldChanged => Value::none(),
                _ => Value::List(Vec::new()),
            });
        }
        Ok(match imp {
            Imp::VersionEnd => {
                let last = a.h(1)? != 0;
                let end = self.store.borrow().version_end(at, last).map_err(trapped)?;
                self.read_versions_of(at);
                match end {
                    Some(v) => Value::some(self.version_value(v, at)?),
                    None => Value::none(),
                }
            }
            Imp::FieldChanged => {
                let field = self.text(a.h(1)?)?;
                let changed = self
                    .store
                    .borrow()
                    .field_changed(at, &field)
                    .map_err(trapped)?;
                self.read_field_of(at, &field);
                match changed {
                    Some(v) => Value::some(self.version_value(v, at)?),
                    None => Value::none(),
                }
            }
            Imp::Unseen => {
                let person = self.looking()?;
                let unseen = self.unseen_read(&person, at)?;
                Value::List(
                    unseen
                        .into_iter()
                        .map(|v| self.version_value(v, at))
                        .collect::<HResult<_>>()?,
                )
            }
            Imp::Steps => self.steps_value(at)?,
            _ => {
                let versions = self.store.borrow().versions(at).map_err(trapped)?;
                self.read_versions_of(at);
                Value::List(
                    versions
                        .into_iter()
                        .map(|v| self.version_value(v, at))
                        .collect::<HResult<_>>()?,
                )
            }
        })
    }

    /// `steps(at)`: the changes made to a resource, newest first, each with the versions one
    /// change made together, the count of changes it folds, and where it stands among the visits
    /// history reads: whether it opens one, and whether the page ends with it.
    fn steps_value(&self, at: &crate::value::ResourceRef) -> HResult<Value> {
        let trapped = |e: crate::store::StoreError| crate::engine::HostFail::Trap(e.to_string());
        let versions = self.store.borrow().versions(at).map_err(trapped)?;
        self.read_versions_of(at);
        let mut steps: Vec<Vec<crate::store::Version>> = Vec::new();
        for v in versions {
            match steps.last_mut() {
                Some(step) if step.last().is_some_and(|last| same_step(last, &v)) => {
                    step.push(v);
                }
                _ => steps.push(vec![v]),
            }
        }
        let mut out = Vec::with_capacity(steps.len());
        // The step above the one being built: the newest one opens a visit, and each below it
        // joins that visit when it is the same hand's a moment later. Steps say this of
        // themselves, so one entry of a history reads what it shows and nothing of the step
        // above it.
        let mut above: Option<crate::store::Version> = None;
        for step in steps.into_iter().rev() {
            let Some(last) = step.last() else {
                continue;
            };
            let who = Value::Principal(Principal::new(last.who.clone()));
            let at_time = Value::Text(last.at.clone());
            let run = Value::Opt(last.run.as_ref().map(|run| Box::new(run_value(run))));
            let mine =
                last.run.is_none() && self.looking_now().as_deref() == Some(last.who.as_str());
            let opens = above.as_ref().is_none_or(|above| opens_visit(above, last));
            let latest = above.is_none();
            above = Some(last.clone());
            // How many acts the step folds: versions of one change are one command over many
            // items, and the changes of one gesture are the keystrokes of one burst of typing —
            // history says the count of each, and Undo takes them all back together.
            let changes = {
                let mut count = 0usize;
                let mut previous: Option<(&str, Option<&str>)> = None;
                for v in &step {
                    let holds = (v.change.as_str(), v.gesture.as_deref());
                    let continues = previous.is_some_and(|(change, gesture)| {
                        change == holds.0 || (gesture.is_some() && gesture == holds.1)
                    });
                    if !continues {
                        count = count.saturating_add(1);
                    }
                    previous = Some(holds);
                }
                count
            };
            let versions = step
                .into_iter()
                .map(|v| self.version_value(v, at))
                .collect::<HResult<_>>()?;
            out.push(Value::Tuple(vec![
                who,
                at_time,
                run,
                Value::List(versions),
                Value::Bool(mine),
                Value::Int(i64::try_from(changes).unwrap_or(i64::MAX)),
                Value::Bool(opens),
                Value::Bool(latest),
            ]));
        }
        Ok(Value::List(out))
    }

    /// `changes(of: run)` of the `host.Run` held at `h`: every version it made, oldest
    /// first, each of the resource it is a version of.
    fn run_changes_value(&self, h: i32) -> HResult<Value> {
        let run = match self.val(h)? {
            Value::Tuple(fields) => match fields.into_iter().next() {
                Some(Value::Text(id)) => id,
                _ => return trap("changes: a run names its group first"),
            },
            other => {
                return trap(format!(
                    "changes: expected a run, got {}",
                    other.kind_name()
                ));
            }
        };
        let versions = self
            .store
            .borrow()
            .run_changes(&run)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        let mut list = Vec::with_capacity(versions.len());
        for v in versions {
            let Some(at) = ResourceRef::parse_id(&v.resource, String::new()) else {
                return trap(format!("changes: `{}` names no resource", v.resource));
            };
            // A resource the run removed has no type any more; its version is still the run's.
            let ty = match self.store.borrow().type_of(&at) {
                Ok(ty) => ty,
                Err(crate::store::StoreError::NotFound(_)) => String::new(),
                Err(error) => return trap(error.to_string()),
            };
            let at = ResourceRef { ty, ..at };
            self.read_versions_of(&at);
            list.push(self.version_value(v, &at)?);
        }
        Ok(Value::List(list))
    }

    /// Who is looking, when someone is: [`Self::looking`] in a view or an action, `None` in
    /// code no one is looking through (a function an agent's query runs).
    pub(crate) fn looking_now(&self) -> Option<String> {
        let frames = self.frames.borrow();
        let top = frames.deciding()?;
        match &top.kind {
            FrameKind::View(_) | FrameKind::Action(_) => {
                drop(frames);
                self.looking().ok()
            }
            _ => None,
        }
    }

    /// Who is looking: the person a view shows to, or the invoker of the action running.
    pub(crate) fn looking(&self) -> HResult<String> {
        let frames = self.frames.borrow();
        let Some(frame) = frames.deciding() else {
            return trap("unseen: only a view or an action has someone looking");
        };
        Ok(match &frame.kind {
            FrameKind::View(view) => view.viewer.0.clone(),
            FrameKind::Action(state) => state
                .invocations
                .last()
                .map_or(&state.invoker, |inv| &inv.who)
                .0
                .clone(),
            _ => return trap("unseen: only a view or an action has someone looking"),
        })
    }

    /// `person`'s seen mark of `resource`, as the runtime holds it.
    pub(crate) fn seen_mark(&self, person: &str, resource: &str) -> Option<SeenMark> {
        let key = crate::seen::seen_key(person, resource);
        mark_of(self.view_states.borrow().get(&key))
    }

    /// `person`'s seen mark that covers `resource`: its own, else the mark of what it was made
    /// from and of whatever holds it, then of what those were made from or holds. Opening a
    /// conversation marks seen what its window showed (task-legibility.md §4), and what it
    /// showed is the messages in it: items are stored flat by id (convergence.md, "Items"), so
    /// the collection holding one names its holder, never the path down to it. Reading what a
    /// record was made from reads the record, as a task's page shows the comments under it.
    pub(crate) fn seen_mark_of_tree(&self, person: &str, at: &ResourceRef) -> Option<SeenMark> {
        let mut at = at.clone();
        // A record is made from one record and held by a few containers (a board's card, a
        // card's comment): the walk up ends either way in a few steps.
        for _ in 0..8 {
            if let Some(mark) = self.seen_mark(person, &at.id_string()) {
                return Some(mark);
            }
            let from = self
                .store
                .borrow()
                .version_end(&at, false)
                .ok()
                .flatten()
                .and_then(|made| made.from);
            at = match from {
                Some(from) => from,
                None => match self.store.borrow().container(&at) {
                    Ok(Some((holder, _))) => holder,
                    _ => return None,
                },
            };
        }
        None
    }

    /// The versions of `at` `person` has not seen.
    pub(crate) fn unseen_of(
        &self,
        person: &str,
        at: &ResourceRef,
    ) -> Result<Vec<crate::store::Version>, crate::store::StoreError> {
        let versions = self.store.borrow().versions(at)?;
        let mark = self.seen_mark_of_tree(person, at);
        let runs = self.seen_runs(person);
        Ok(crate::seen::unseen(&versions, person, mark.as_ref(), &runs))
    }

    /// The newest version of `at` `person` has not seen, if any: what a row's mark says,
    /// answered by the index's newest version of the resource without its log.
    pub(crate) fn newest_unseen_of(
        &self,
        person: &str,
        at: &ResourceRef,
    ) -> Result<Option<crate::store::Version>, crate::store::StoreError> {
        let Some(newest) = self.store.borrow().newest(at)? else {
            return Ok(None);
        };
        let mark = self.seen_mark_of_tree(person, at);
        let runs = self.seen_runs(person);
        Ok(crate::seen::unseen(std::slice::from_ref(&newest), person, mark.as_ref(), &runs).pop())
    }

    /// The first item of `at`'s collection `field` that `person` has not seen, oldest first (a
    /// conversation's first unread message), as a transcript shows it: the first of
    /// [`Self::unread_items`], which is where its "New messages" divider stands.
    pub(crate) fn first_unseen_of(
        &self,
        person: &str,
        at: &ResourceRef,
        field: &str,
        item: &str,
    ) -> Result<Option<String>, crate::store::StoreError> {
        Ok(self
            .unread_items(person, at, field, item, 1)?
            .into_iter()
            .next())
    }

    /// The items of `at`'s collection `field` (of `item`) `person` has not seen, oldest first,
    /// `most` of them at most: what the divider of a transcript stands before and so what the
    /// place that opens it and the window of it count. One count, read here, however many
    /// numbers a person is shown.
    ///
    /// Read item by item from where the person read to, a page at most: an item is unseen when
    /// a version of it is newer than what the person has seen of `at` (its mark), and an item
    /// they wrote is one they saw. A channel the person is caught up with costs the page it
    /// looks through, and one they never opened costs its first page, never its whole
    /// conversation: what a person is shown is a page, so counting it reads a page. Writing a
    /// message reads nothing above it: a reply counts as neither a mark nor a reading of what
    /// came before it.
    pub(crate) fn unread_items(
        &self,
        person: &str,
        at: &ResourceRef,
        field: &str,
        item: &str,
        most: usize,
    ) -> Result<Vec<String>, crate::store::StoreError> {
        // How many items are looked through: one index page, which is what a listing reads to
        // place an item, and what a transcript shows in one frame.
        const PAGE: usize = 64;
        let mut unread = Vec::new();
        let store = self.store.borrow();
        let storage = self
            .record_type_of(at)
            .and_then(|info| info.field_by_id_or_name(field).map(|f| f.id.clone()));
        let Some(storage) = storage else {
            return Ok(unread);
        };
        // A record the store does not hold yet (one just made, or empty) has nothing unseen.
        let Ok(items) = store.listing(
            at,
            &crate::store::FieldSpec {
                id: storage,
                kind: crate::meta::FieldKind::Collection,
            },
        ) else {
            return Ok(unread);
        };
        let len = items.len();
        if len == 0 {
            return Ok(unread);
        }
        let runs = self.seen_runs(person);
        let mark = self.seen_mark(person, &at.id_string());
        // Where the person read to, by the log rather than by a clock: the newest item whose
        // change their mark of the channel names (a store with no channel versions names the
        // newest item it holds). It is looked for in the tail, a page deep.
        let mut read_to: Option<usize> = None;
        for index in (len.saturating_sub(PAGE)..len).rev() {
            let Some(id) = items.get(index) else {
                continue;
            };
            let versions = store.versions(&at.item(&id, item.to_owned()))?;
            if mark
                .as_ref()
                .is_some_and(|mark| versions.iter().any(|v| v.change == mark.change))
            {
                read_to = Some(index);
                break;
            }
        }
        // The first item after where they read that someone changed since, oldest first: a
        // version of theirs is one they saw, so it never makes an item unread of its own. A mark
        // that named nothing in the tail is older than the page shown from it, so the page is
        // all after it, and one who never read anything is read from the conversation's first
        // page.
        let from = match read_to {
            Some(index) => index.saturating_add(1),
            None if mark.is_some() => len.saturating_sub(PAGE),
            None => 0,
        };
        // The items looked through are the page from there, never the conversation: an unread
        // item beyond it is one the count says nothing of, as a transcript shows a page.
        let upto = len.min(from.saturating_add(PAGE));
        for index in from.min(len)..upto {
            let Some(id) = items.get(index) else {
                break;
            };
            let candidate = at.item(&id, item.to_owned());
            if store.type_of(&candidate).is_err() {
                continue;
            }
            let versions = store.versions(&candidate)?;
            // An item nothing changed is not one anyone said: nothing unseen in it.
            if versions.is_empty() {
                continue;
            }
            let seen = self.seen_mark(person, &candidate.id_string());
            let un = crate::seen::unseen(&versions, person, seen.as_ref(), &runs);
            if !un.is_empty() {
                unread.push(id.to_string());
                if unread.len() >= most {
                    break;
                }
            }
        }
        Ok(unread)
    }

    /// The type `at` is of: the one its ref names, or — for a ref that names none, as a window's
    /// does — the one the store holds the record under.
    fn record_type_of(&self, at: &ResourceRef) -> Option<Rc<crate::registry::TypeInfo>> {
        self.type_of_ref(&at.ty)
            .ok()
            .or_else(|| {
                let ty = self.store.borrow().type_of(at).ok()?;
                self.type_of_ref(&ty).ok()
            })
            .map(|(_, info)| info)
    }

    /// The collection of records `at` holds, and the type of its items: what a fact about the
    /// items of a resource reads (`first_unseen`, `mentions`), which is the list a window of it
    /// shows and a place that opens it counts. A resource holding none has no items of its own.
    pub(crate) fn item_collection_of(&self, at: &ResourceRef) -> Option<(String, String)> {
        let info = self.record_type_of(at)?;
        let field = info.fields.iter().find(|field| {
            field.kind == crate::meta::FieldKind::Collection && self.item_type(&field.ty).is_ok()
        })?;
        let item = self.item_type(&field.ty).ok()?;
        Some((field.id.clone(), item.id.clone()))
    }

    /// How many of the items `at` holds `person` has not seen, oldest first: the number a place
    /// that opens it shows on its badge and a window of it in its header, both read here. `None`
    /// for a resource that holds no collection of records: nothing of it is an item.
    pub(crate) fn unread_count_of(
        &self,
        person: &str,
        at: &ResourceRef,
    ) -> Result<Option<i64>, crate::store::StoreError> {
        let Some((field, item)) = self.item_collection_of(at) else {
            return Ok(None);
        };
        let unread = self.unread_items(person, at, &field, &item, UNREAD_SCAN)?;
        Ok(Some(i64::try_from(unread.len()).unwrap_or(i64::MAX)))
    }

    /// The members, the person looking aside, whose reading of `coll` ends at `at`: their seen
    /// mark of `coll` names the change that made `at`, so the last line they read is that one.
    ///
    /// Read from the marks the host gives of everyone's reading (`crate::Runtime::set_seen`),
    /// so a conversation shows who read to a line without counting anything: `at`'s own versions
    /// say which change it is, and each member is one slot read. A member without a mark of
    /// `coll` reads nothing of it; a mark that names another line read further.
    pub(crate) fn seen_by_of(
        &self,
        looking: &str,
        coll: &ResourceRef,
        at: &ResourceRef,
    ) -> Result<Vec<crate::members::Member>, crate::store::StoreError> {
        let versions = self.store.borrow().versions(at)?;
        if versions.is_empty() {
            return Ok(Vec::new());
        }
        let resource = coll.id_string();
        let mut read = Vec::new();
        for member in self.read_members() {
            if member.who.0.eq_ignore_ascii_case(looking) {
                continue;
            }
            // Every member's mark is read, so the view runs again when any of them reads on.
            self.note_seen_read(&member.who.0, &resource);
            let Some(mark) = self.seen_mark(&member.who.0, &resource) else {
                continue;
            };
            if versions.iter().any(|version| version.change == mark.change) {
                read.push(member);
            }
        }
        Ok(read)
    }

    /// `seen_by(c, m)`: the members whose reading of `c` ends at `m`, the person looking aside:
    /// who has read to that line. A view that reads it runs again when `m` moves or when one of
    /// them reads on.
    fn seen_by_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let coll = self.ref_arg(a.h(0)?)?;
        let at = self.ref_arg(a.h(1)?)?;
        let person = self.looking()?;
        let read = self
            .seen_by_of(&person, &coll.target, &at.target)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        // What the answer is made of: `m`'s own versions, noted as read by the answer itself.
        self.read_versions_of(&at.target);
        self.ret_h(crate::members::principals(read.iter()))
    }

    /// How many of the items of `at`'s collection `field` (of `item`) name `person` in their
    /// rich text and are not seen by them: a channel's mention count, beside its unread count.
    ///
    /// A mention is a `mention` mark whose value is the person's id
    /// ([`crate::rich::MENTION`]), as the system's mention notices read it. Only the messages
    /// the person has not seen are read — from `first_unseen` on — so a channel the person is
    /// caught up with costs one message, never its whole history. (A reference column in the
    /// index would answer it in one read; none is kept yet.)
    pub(crate) fn mentions_of(
        &self,
        person: &str,
        at: &ResourceRef,
        field: &str,
        item: &str,
    ) -> Result<usize, crate::store::StoreError> {
        let store = self.store.borrow();
        let storage = self
            .type_of_ref(&at.ty)
            .ok()
            .and_then(|(_, info)| info.field_by_id_or_name(field).map(|f| f.id.clone()));
        let Some(storage) = storage else {
            return Ok(0);
        };
        let Ok(items) = store.listing(
            at,
            &crate::store::FieldSpec {
                id: storage,
                kind: crate::meta::FieldKind::Collection,
            },
        ) else {
            return Ok(0);
        };
        let len = items.len();
        // From the first unread message on: the only ones that can name the person unseen.
        let from = match self.first_unseen_of(person, at, field, item)? {
            Some(id) => items.position(&id).flatten().unwrap_or(0),
            None => return Ok(0),
        };
        // The item's type, which the caller named: its rich text fields are what a mention is
        // written in.
        let Ok((_, info)) = self.type_of_ref(item) else {
            return Ok(0);
        };
        let texts: Vec<String> = info
            .fields
            .iter()
            .filter(|f| f.kind == crate::meta::FieldKind::RichText)
            .map(|f| f.id.clone())
            .collect();
        // The longest run of unread messages read for a count: a workspace nobody has read counts
        // mentions in its first page of new messages, not in every one.
        let upto = from.saturating_add(UNREAD_SCAN).min(len);
        let mut found: usize = 0;
        for index in from..upto {
            let Some(id) = items.get(index) else {
                break;
            };
            let candidate = at.item(&id, item.to_owned());
            // The item may have been removed since: it names nobody.
            if store.type_of(&candidate).is_err() {
                continue;
            }
            for field in &texts {
                let spec = crate::store::FieldSpec {
                    id: field.clone(),
                    kind: crate::meta::FieldKind::RichText,
                };
                let Ok(crate::store::FieldValue::Rich(rich)) = store.read(&candidate, &spec) else {
                    continue;
                };
                if rich.mentions(person) {
                    found = found.saturating_add(1);
                    break;
                }
            }
        }
        Ok(found)
    }

    /// The runs `person` reviewed, as the runtime holds them.
    pub(crate) fn seen_runs(&self, person: &str) -> Vec<String> {
        match self
            .view_states
            .borrow()
            .get(&crate::seen::runs_key(person))
        {
            Some(Value::List(runs)) => runs
                .iter()
                .filter_map(|run| run.as_text().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// [`Self::unseen_of`], read by the frame running: it runs again when the versions of
    /// `at`, the person's mark of it or the runs they reviewed move.
    pub(crate) fn unseen_read(
        &self,
        person: &str,
        at: &ResourceRef,
    ) -> HResult<Vec<crate::store::Version>> {
        for key in [
            crate::seen::seen_key(person, &at.id_string()),
            crate::seen::runs_key(person),
        ] {
            let (value, generation) = {
                let states = self.view_states.borrow();
                (
                    states.get(&key).cloned().unwrap_or_else(Value::none),
                    states.generation(&key),
                )
            };
            self.note_state_read(key, generation, &value);
        }
        self.read_versions_of(at);
        self.unseen_of(person, at)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))
    }

    /// `person` has seen every version of `at` there is now, and of what it holds when it is a
    /// container (a conversation's messages): the time they read to, which each item's version
    /// is later or earlier than. Whether the mark moved.
    pub(crate) fn mark_seen(
        &self,
        person: &str,
        at: &ResourceRef,
    ) -> Result<bool, crate::store::StoreError> {
        let newest_item = self.newest_item_version(at);
        let resource = at.id_string();
        let versions = self.store.borrow().versions(at)?;
        // A store that keeps a version per item names the mark by the newest of what it holds;
        // one that keeps the container's own versions names it by theirs. The later of the two
        // stands: a person who read a channel has read everything in it.
        let by_container = versions.last().cloned();
        let mark = match (by_container, newest_item) {
            (Some(container), Some(item)) if item.at > container.at => SeenMark::of(&item),
            (Some(container), _) => SeenMark::of(&container),
            (None, Some(item)) => SeenMark::of(&item),
            (None, None) => return Ok(false),
        };
        if self.seen_mark(person, &resource).as_ref() == Some(&mark) {
            return Ok(false);
        }
        self.view_states.borrow_mut().set(
            crate::seen::seen_key(person, &resource),
            Value::some(Value::Tuple(vec![
                Value::Text(mark.change.clone()),
                Value::Text(mark.at.clone()),
            ])),
        );
        self.seen_writes
            .borrow_mut()
            .push(crate::seen::SeenWrite::Resource {
                person: person.to_owned(),
                resource,
                mark: Some(mark),
            });
        Ok(true)
    }

    /// `person` marked everything they had not seen of `resources` seen, in one move: the
    /// *Mark all seen* of a catch-up. What each mark was before is kept, so that the move can
    /// be taken back ([`Self::undo_seen`]); hands the host one write for each mark it moved.
    ///
    /// # Errors
    ///
    /// The store could not read the versions of one of them.
    pub(crate) fn mark_all_seen(
        &self,
        person: &str,
        resources: &[String],
    ) -> Result<usize, crate::store::StoreError> {
        let mut before = Vec::with_capacity(resources.len());
        let mut marked: usize = 0;
        for resource in resources {
            let Some(at) = ResourceRef::parse_id(resource, String::new()) else {
                continue;
            };
            before.push((resource.clone(), self.seen_mark(person, resource)));
            if self.mark_seen(person, &at)? {
                marked = marked.saturating_add(1);
            }
        }
        *self.seen_undo.borrow_mut() = Some(crate::seen::SeenUndo {
            person: person.to_owned(),
            before,
        });
        Ok(marked)
    }

    /// Give back what the last catch-up took away: every mark it moved, as it was. Whether
    /// anything was given back; what the host is handed to keep says so too.
    pub(crate) fn undo_seen(&self, person: &str) -> bool {
        let Some(undo) = self.seen_undo.borrow_mut().take() else {
            return false;
        };
        if undo.person != person {
            return false;
        }
        for (resource, mark) in undo.before {
            let key = crate::seen::seen_key(person, &resource);
            let value = match &mark {
                Some(mark) => Value::some(Value::Tuple(vec![
                    Value::Text(mark.change.clone()),
                    Value::Text(mark.at.clone()),
                ])),
                None => Value::none(),
            };
            self.view_states.borrow_mut().set(key, value);
            self.seen_writes
                .borrow_mut()
                .push(crate::seen::SeenWrite::Resource {
                    person: person.to_owned(),
                    resource,
                    mark,
                });
        }
        true
    }

    /// The title `at` is called by, as a catch-up names the group it changes: its `title`,
    /// `name` or `label` field, else its first text field, else what its newest change's
    /// action is called.
    pub(crate) fn item_title(
        &self,
        at: &ResourceRef,
        version: Option<&crate::store::Version>,
    ) -> String {
        if let Some(title) = self.program_title(at) {
            return title;
        }
        version.map_or_else(String::new, |v| self.action_label(&v.action))
    }

    /// The newest item's version among `at`'s collections, when it holds any: the time a read
    /// of `at` reaches, found from the end a page at most rather than by walking the items.
    fn newest_item_version(&self, at: &ResourceRef) -> Option<crate::store::Version> {
        // How many items from the end are looked through for one that says something.
        const TAIL: usize = 8;
        let Ok((_, info)) = self.type_of_ref(&at.ty) else {
            return None;
        };
        let mut newest: Option<crate::store::Version> = None;
        for spec in info.specs() {
            if spec.kind != crate::meta::FieldKind::Collection {
                continue;
            }
            let Some(crate::meta::Ty::Record(item_ty)) = info.field(&spec.id).map(|f| f.ty.clone())
            else {
                continue;
            };
            let Ok(items) = self.store.borrow().listing(at, &spec) else {
                continue;
            };
            let len = items.len();
            for index in (len.saturating_sub(TAIL)..len).rev() {
                let Some(id) = items.get(index) else {
                    continue;
                };
                let Ok(versions) = self
                    .store
                    .borrow()
                    .versions(&at.item(&id, item_ty.name.clone()))
                else {
                    continue;
                };
                let Some(last) = versions.last() else {
                    continue;
                };
                if newest.as_ref().is_none_or(|held| held.at < last.at) {
                    newest = Some(last.clone());
                }
                break;
            }
        }
        newest
    }

    /// The runs `person` reviewed are `runs` now.
    pub(crate) fn set_seen_runs(&self, person: &str, runs: &[String]) {
        let key = crate::seen::runs_key(person);
        let mut sorted = runs.to_vec();
        sorted.sort();
        sorted.dedup();
        let value = Value::List(sorted.into_iter().map(Value::Text).collect());
        let mut states = self.view_states.borrow_mut();
        if states.get(&key) != Some(&value) {
            states.set(key, value);
        }
    }

    /// `linked(r)`: the resources made from `r`, mentioning it in their rich text or naming it
    /// in a reference field, as a `host.Link` each: the resource and how it is linked.
    fn links_import(&self, a: &Args<'_>) -> HResult<Value> {
        let target = self.ref_arg(a.h(0)?)?;
        let at = &target.target;
        // What links to a resource of another app is kept where that app runs, and comes here
        // in its reading (`$links`), which the view runs again on when it moves.
        if crate::foreign::is_foreign(at) {
            let key = crate::foreign::reading_key(at);
            self.note_reading(key);
            let links = self.foreign.borrow().links(at)?;
            return Ok(Value::List(
                links
                    .into_iter()
                    .map(|(of, how)| {
                        Value::Tuple(vec![
                            Value::resource(&of),
                            Value::Text(how.word().to_owned()),
                        ])
                    })
                    .collect(),
            ));
        }
        self.read_links_of(at);
        let links = self
            .store
            .borrow()
            .linked(at)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        Ok(Value::List(
            links
                .into_iter()
                .map(|link| {
                    Value::Tuple(vec![
                        Value::resource(&link.of),
                        // How it links, as the `host.LinkHow` case its word names.
                        Value::Enum(EnumValue {
                            ty: tilolang::host::LINK_HOW_TYPE.to_owned(),
                            variant: link.how.word().to_owned(),
                            fields: std::collections::BTreeMap::new(),
                        }),
                    ])
                })
                .collect(),
        ))
    }

    /// Record, in a view's frame, that it read what links to `target`: it runs again when
    /// something is made from it, mentions it or names it in a field.
    pub(crate) fn read_links_of(&self, target: &ResourceRef) {
        if let Some(vs) = self.frames.borrow_mut().view_mut() {
            vs.reads
                .push((target.id_string(), crate::store::LINKS.to_owned()));
        }
    }

    /// Record, in a view's frame, that it read the versions of `target`: it runs again when a
    /// version of the resource comes, which every change to it makes one of — the version, not
    /// each field, so a view that reads a resource's history reads one thing of it and not its
    /// every field.
    pub(crate) fn read_versions_of(&self, target: &crate::value::ResourceRef) {
        if let Some(vs) = self.frames.borrow_mut().view_mut() {
            vs.reads
                .push((target.id_string(), crate::marks::DOC_VERSIONS.to_owned()));
        }
    }

    /// Record in a view's frame that it read when the field `field` of `target` last changed: it
    /// runs again when that field changes, and when a change that set it at creation is taken
    /// back.
    pub(crate) fn read_field_of(&self, target: &crate::value::ResourceRef, field: &str) {
        if let Some(vs) = self.frames.borrow_mut().view_mut() {
            vs.reads
                .push((target.id_string(), crate::store::changed_field(field)));
            vs.reads.push((
                target.id_string(),
                crate::store::Reflected::CREATED.to_owned(),
            ));
        }
    }

    /// What an action is called where history names it: its label, or what the host did.
    pub(crate) fn action_label(&self, id: &str) -> String {
        if let Some(found) = self
            .registry
            .borrow()
            .action_package(id)
            .and_then(|pkg| pkg.action(id).map(label_of))
        {
            return found;
        }
        sentence(id.trim_start_matches('$'))
    }

    /// Take a view state slot read into the running view's reads, when a view runs.
    pub(crate) fn note_state_read(&self, key: StateKey, generation: u64, seen: &Value) {
        let mut frames = self.frames.borrow_mut();
        if let Some(vs) = frames.view_mut() {
            vs.state_reads.push((key.clone(), generation));
            vs.state_seen.push((key, seen.clone()));
        }
    }

    /// Record, in a view's frame, that it read `person`'s seen mark of `resource`: it runs
    /// again when the mark moves.
    pub(crate) fn note_seen_read(&self, person: &str, resource: &str) {
        let key = crate::seen::seen_key(person, resource);
        let (generation, value) = {
            let states = self.view_states.borrow();
            (
                states.generation(&key),
                states.get(&key).cloned().unwrap_or_else(Value::none),
            )
        };
        self.note_state_read(key, generation, &value);
    }

    /// Take `(resource, field)` reads into the running view's reads, when a view runs.
    pub(crate) fn note_reads(&self, reads: impl IntoIterator<Item = (String, String)>) {
        let mut frames = self.frames.borrow_mut();
        if let Some(vs) = frames.view_mut() {
            vs.reads.extend(reads);
        }
    }

    /// Record, in a view's frame, that what it read of the time reads differently at
    /// `changes_at` (seconds since 1970, UTC): it runs again then. Outside a view nothing
    /// follows the time: an action reads one time throughout.
    pub(crate) fn note_time(&self, changes_at: Option<i64>) {
        if changes_at.is_none() {
            return;
        }
        let mut frames = self.frames.borrow_mut();
        if let Some(vs) = frames.view_mut() {
            vs.changes_at = crate::render::earliest(vs.changes_at, changes_at);
        }
    }

    /// What the platform shows of the type `ty`, and of the resource `r` of it when there is
    /// one: a `host.Meta`.
    fn type_meta(&self, ty: &str, r: Option<&RefValue>) -> HResult<Value> {
        // A type a Rust app's views draw has no package declaring it: its metadata comes from the
        // views the app declared (a place shows a resource of it when its type has a default
        // view), and the runtime draws it through that view (one-app-model.md §2).
        if let Some(views) = crate::rust_views::app_of(&self.rust_views, ty) {
            let kind = crate::rust_views::kind_of(&views, ty);
            let name = kind.rsplit(['#', '.']).next().unwrap_or(&kind).to_owned();
            let type_name = sentence(&name);
            let opens = views.views_of(&kind).iter().any(|view| view.is_default);
            return Ok(Value::Tuple(vec![
                Value::Text(type_name.clone()),
                Value::Text(type_name.clone()),
                Value::Text(type_name.clone()),
                Value::Text(type_name),
                Value::none(),
                Value::Bool(opens),
            ]));
        }
        // Another app's resource names its type `app#Type`, as its readings come in: a type
        // whose package is not loaded here asks for it, and the call runs again when it comes.
        let (pkg, info) = self.type_of_ref(ty)?;
        let meta = &pkg.meta;
        let ty = info.id.as_str();
        let Some(declared) = meta.types.iter().find(|t| t.id == ty) else {
            return trap(format!("meta: type `{ty}` is not declared"));
        };
        let type_name = sentence(&declared.name);
        let title = match r {
            Some(r) => self.title_of(&r.target, meta, declared)?,
            None => type_name.clone(),
        };
        Ok(Value::Tuple(vec![
            Value::Text(title),
            Value::Text(type_name),
            Value::Text(self.noun(&declared.id, 1, &declared.noun())),
            Value::Text(self.noun(&declared.id, 2, &declared.plural())),
            meta.icon
                .clone()
                .map_or_else(Value::none, |icon| Value::some(Value::Text(icon))),
            Value::Bool(meta.default_view(ty).is_some()),
            Value::Bool(meta.row_view(ty).is_some()),
        ]))
    }

    /// What one (`count` 1) or several of the type `id` are called, in the reader's language:
    /// its catalogue's noun, else `source`.
    fn noun(&self, id: &str, count: i64, source: &str) -> String {
        let key = format!("{}.noun", tilolang::messages::key_of(id));
        self.word_args(&key, &[("count".to_owned(), count.to_string())], source)
    }

    /// What one item the window of `ty` shows is called, singular and plural, in the reader's
    /// language: the type's own noun, or of what it holds when it is a document (a window of
    /// tasks says "tasks"). A window's mark stands for the document's own resource, whose type
    /// only the store names, so `ty` may be either. `None` when neither type is known.
    ///
    /// The declared English words are only the source a catalogue without the language reads:
    /// both numbers come from the type's one `<id>.noun` message, so a window never says an
    /// English plural in a sentence of another language.
    pub(crate) fn item_nouns(&self, ty: &str) -> Option<(String, String)> {
        let (_, info) = self.type_of_ref(ty).ok()?;
        let item = info
            .fields
            .iter()
            .find(|field| field.kind == crate::meta::FieldKind::Collection)
            .and_then(|field| crate::meta::element_record(&field.ty))
            .and_then(|key| self.type_of_ref(&format!("{}#{}", key.app, key.name)).ok())
            .map_or_else(
                || (info.id.clone(), info.name.clone()),
                |(_, item)| (item.id.clone(), item.name.clone()),
            );
        let (id, name) = item;
        let words = tilolang::meta::in_words(&name);
        Some((
            self.noun(&id, 1, &words),
            self.noun(&id, 2, &tilolang::meta::plural(&words)),
        ))
    }

    /// The title of `target`: the first line its first text field holds, when it holds more
    /// than spaces (a record is named by what it says, rich text as its reader reads it); else,
    /// for a root of its package, the package's title; else what the type says it is called
    /// while its title is empty (`(untitled: "Untitled")`); else its type's name.
    pub(crate) fn title_of(
        &self,
        target: &ResourceRef,
        meta: &Meta,
        ty: &TypeMeta,
    ) -> HResult<String> {
        let (_, info) = self.type_of_ref(&target.ty)?;
        if let Some(name) = info.name_field() {
            let field = name.clone();
            self.note_reads([(target.id_string(), field.id.clone())]);
            let value = self.read_value(target, &field, None)?;
            let text = match value {
                Value::Text(text) => text
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or_default()
                    .to_owned(),
                Value::Rich(rich) => self.read_rich(&rich, true),
                _ => String::new(),
            };
            if !text.trim().is_empty() {
                return Ok(text);
            }
        }
        if target.item.is_none() && meta.roots.contains(&ty.id) {
            return Ok(self.word_args(&tilolang::messages::key_of(&meta.app), &[], &meta.title));
        }
        if let Some(untitled) = &ty.untitled {
            let key = format!("{}.untitled", tilolang::messages::key_of(&ty.id));
            return Ok(self.word_args(&key, &[], untitled));
        }
        Ok(sentence(&ty.name))
    }

    /// The actions offered on `subject`: its type's own that need nothing else (what else it
    /// takes has a default, which the invocation gets, or a form asks), and the actions of the
    /// resource holding it that take it as their second argument — in the order their packages
    /// declare them, the primary one first, each a `host.Action`. An app declares its actions in
    /// the order a person reads them, so a menu of one item lists them as it wrote them.
    pub(crate) fn actions_on(&self, subject: &RefValue, above: &Value) -> HResult<Vec<Value>> {
        let target = &subject.target;
        let packages = self.registry.borrow().packages();
        let rank = |pkg: &Rc<Package>| {
            packages
                .iter()
                .position(|p| Rc::ptr_eq(p, pkg))
                .unwrap_or(usize::MAX)
        };
        let mut found: Vec<(bool, usize, usize, Offered)> = Vec::new();
        // The subject's own. Another app's resource names its type as `app#Type`, which
        // resolves to the id its package declares before the package is looked up.
        let own = self.registry.borrow().type_id(&target.ty).and_then(|id| {
            let registry = self.registry.borrow();
            registry.type_info(&id)
        });
        if let Some((pkg, info)) = &own {
            let at = rank(pkg);
            for (index, action) in pkg.meta.offered_actions().enumerate() {
                let takes_it = action.params.split_first().is_some_and(|(first, rest)| {
                    all_asked(&pkg.meta, rest)
                        && !asks_for_own_item(&pkg.meta, info.id.as_str(), rest)
                        && names_type(&pkg.meta, &first.ty, info.id.as_str())
                });
                if !takes_it {
                    continue;
                }
                let shown = Offered {
                    action,
                    args: offered_args(action, subject, above),
                    rest: action
                        .params
                        .get(places_of(action).saturating_add(1)..)
                        .unwrap_or_default(),
                    present: true,
                    from: None,
                    group: None,
                    label: None,
                };
                found.push((action.role.as_deref() == Some("primary"), at, index, shown));
            }
        }
        // The holder's, on it.
        if let Some(holder) = self.holder_of(target) {
            let holder_ty = holder.ty.clone();
            for pkg in &packages {
                let Some(holder_info) = pkg.type_by_id(&holder_ty) else {
                    continue;
                };
                let at = rank(pkg);
                for (index, action) in pkg.meta.offered_actions().enumerate() {
                    let [first, second, rest @ ..] = action.params.as_slice() else {
                        continue;
                    };
                    if !all_asked(&pkg.meta, rest) {
                        continue;
                    }
                    let on_holder = names_type(&pkg.meta, &first.ty, holder_info.id.as_str());
                    let takes_subject = names_type(&pkg.meta, &second.ty, &target.ty);
                    if !(on_holder && takes_subject) {
                        continue;
                    }
                    let shown = Offered {
                        action,
                        args: vec![
                            Value::Ref(RefValue {
                                target: Rc::new(holder.clone()),
                                author: subject.author.clone(),
                            }),
                            Value::Ref(subject.clone()),
                        ],
                        rest,
                        present: false,
                        from: None,
                        group: None,
                        label: None,
                    };
                    found.push((action.role.as_deref() == Some("primary"), at, index, shown));
                }
            }
        }
        found.sort_by_key(|(primary, pkg, index, _)| (!primary, *pkg, *index));
        // What it already offers as itself is not offered again as something to make: an
        // action written for the subject is where a person finds it.
        let direct: Vec<&str> = found
            .iter()
            .map(|(_, _, _, shown)| shown.action.id.as_str())
            .collect();
        let mut out = Vec::new();
        for (_, _, _, shown) in found {
            out.extend(self.action_value(&shown, Some(subject))?);
        }
        // Last, under the menu's one group, what can be made from it: a submenu of its own at
        // the end, so what the app offers the item stays what a person reads first.
        for shown in self.make_offers(subject, own.as_ref(), &packages)? {
            if direct.contains(&shown.action.id.as_str()) {
                continue;
            }
            out.extend(self.action_value(&shown, None)?);
        }
        Ok(out)
    }

    /// The actions that can make something of `subject`: for every type of the packages this
    /// runtime runs, the create action that makes it — one whose result is that type, whose
    /// first parameter is the package's root it puts it in, and whose other parameters are a
    /// title (the title its type is called by) and what it may default. The made resource is
    /// made from `subject` and starts with its title. One per type: the action that takes a
    /// source of `subject`'s type when the package declares one, since that action was written
    /// for what the subject is.
    fn make_offers<'m>(
        &self,
        subject: &RefValue,
        own: Option<&(Rc<Package>, Rc<crate::registry::TypeInfo>)>,
        packages: &'m [Rc<Package>],
    ) -> HResult<Vec<Offered<'m>>> {
        let source = match own {
            Some((pkg, info)) => {
                let Some(ty) = pkg.meta.types.iter().find(|ty| ty.id == info.id) else {
                    return Ok(Vec::new());
                };
                Some(Source {
                    title: self.title_of(&subject.target, &pkg.meta, ty)?,
                    app: pkg.meta.app.clone(),
                    name: info.name.clone(),
                })
            }
            None => None,
        };
        let group = self.word_args("tilolang-make", &[], "Make");
        let mut found: Vec<(usize, usize, bool, Offered<'m>)> = Vec::new();
        for (rank, pkg) in packages.iter().enumerate() {
            for action in pkg.meta.offered_actions() {
                let Some((at, source_arg, shown)) =
                    self.make_offer(pkg, action, source.as_ref(), subject, &group)
                else {
                    continue;
                };
                match found
                    .iter_mut()
                    .find(|(pkg, ty, _, _)| *pkg == rank && *ty == at)
                {
                    // A source of its type beats the plain create action of the same type.
                    Some(held) if source_arg && !held.2 => {
                        *held = (rank, at, source_arg, shown);
                    }
                    Some(_) => {}
                    None => found.push((rank, at, source_arg, shown)),
                }
            }
        }
        found.sort_by_key(|(pkg, ty, _, _)| (*pkg, *ty));
        Ok(found.into_iter().map(|(_, _, _, shown)| shown).collect())
    }

    /// The offer of making what `action` makes from a subject of the `source`'s type: `action`
    /// makes a type its package declares, puts it in the package's root its first parameter
    /// names, and takes the source, when a parameter's type is the source's type, and the title
    /// its type is called by; what it leaves to default means a menu runs it as it is, and an
    /// optional one before the title is passed `.none`, which is what its default is. Answers
    /// the type it makes, whether the source is a parameter of it, and the offer.
    fn make_offer<'m>(
        &self,
        pkg: &'m Rc<Package>,
        action: &'m crate::meta::ActionMeta,
        source: Option<&Source>,
        subject: &RefValue,
        group: &str,
    ) -> Option<(usize, bool, Offered<'m>)> {
        let meta = &pkg.meta;
        // What it makes: the type as code writes it (`Task`), one the package declares.
        let result = action.result.as_deref()?;
        let Ok(crate::meta::Ty::Record(made)) = crate::meta::parse_ty(result, &meta.app) else {
            return None;
        };
        let (at, ty) = meta
            .types
            .iter()
            .enumerate()
            .find(|(_, ty)| ty.name == made.name)?;
        let title = ty.fields.iter().find(|field| field.holds_text())?;
        let root = action.params.first().and_then(|param| {
            meta.roots
                .iter()
                .find(|root| names_type(meta, &param.ty, root))
        })?;
        // The package's root the made resource stands in: one this runtime holds, or, for an
        // app it sends to, one that app holds.
        let root = ResourceRef::root(format!("root-{root}"), root.clone());
        if !pkg.foreign && self.store.borrow().type_of(&root).is_err() {
            return None;
        }
        let source = source?;
        let mut args = vec![Value::Ref(RefValue {
            target: Rc::new(root),
            author: None,
        })];
        let mut source_arg = false;
        let mut title_arg = false;
        let mut fits = true;
        for param in action.params.iter().skip(1) {
            if !source_arg && source.names_param(&param.ty, &meta.app) {
                args.push(Value::Ref(subject.clone()));
                source_arg = true;
            } else if !title_arg && param.name == title.name && param.ty == "Text" {
                args.push(Value::Text(source.title.clone()));
                title_arg = true;
            } else if param
                .default
                .as_ref()
                .is_some_and(crate::meta::DefaultMeta::is_none)
            {
                // A parameter whose default is `.none` needs nothing before it: the offer passes
                // it there, so the title the action takes after it still stands.
                args.push(Value::none());
            } else if param.default.is_some() {
                break;
            } else {
                fits = false;
                break;
            }
        }
        // An action left asking for something after what this offer gives is one a menu cannot
        // run.
        let done = action
            .params
            .iter()
            .skip(args.len())
            .all(|param| param.default.is_some());
        if !(fits && title_arg && done) {
            return None;
        }
        Some((
            at,
            source_arg,
            Offered {
                action,
                args,
                rest: &[],
                present: false,
                from: Some(ResourceRef::clone(&subject.target)),
                group: Some(group.to_owned()),
                label: Some(sentence(&ty.name)),
            },
        ))
    }

    /// The action `reference` names (`new_project(t)`, as a view hands it to a prop that shows
    /// one: `ui.NavSection(add: new_project(t))`) as the `host.Action` a control shows it by.
    pub(crate) fn action_ref_value(&self, reference: &ActionRef) -> HResult<Value> {
        let pkg = self
            .registry
            .borrow()
            .action_package(&reference.action)
            .ok_or_else(|| format!("`{}` is no action of a loaded package", reference.action))?;
        let action = pkg
            .action(&reference.action)
            .ok_or_else(|| format!("`{}` is no action", reference.action))?;
        let shown = Offered {
            action,
            args: reference.args.clone(),
            rest: &[],
            present: false,
            from: reference.from.clone(),
            group: None,
            label: None,
        };
        let value = self
            .action_value(&shown, None)?
            .ok_or_else(|| format!("`{}` is offered as nothing", reference.action))?;
        Ok(value)
    }

    /// The resource holding the item `target`, with its type: where the store has it now.
    fn holder_of(&self, target: &ResourceRef) -> Option<ResourceRef> {
        let (holder, _) = self.store.borrow().container(target).ok()??;
        Some(holder)
    }

    /// An offered action as a `host.Action`: presented for the subject (its label, icon and
    /// toggled state as its presentation view gives them now), the view taking what that read,
    /// and whether it can run on the subject now (`available`, with the `reason` it cannot).
    /// Without a subject it is offered as it stands, its presentation unread.
    pub(crate) fn action_value(
        &self,
        shown: &Offered<'_>,
        subject: Option<&RefValue>,
    ) -> HResult<Option<Value>> {
        let action = shown.action;
        let mut label = match &shown.label {
            Some(label) => label.clone(),
            None => self.word_args(
                &tilolang::messages::key_of(&action.id),
                &[],
                &label_of(action),
            ),
        };
        let mut icon = action.icon.clone();
        let mut on: Option<bool> = None;
        if let Some(subject) = subject
            && shown.present
            && let Some(view) = &action.present
            && let Some(node) = self.present(view, subject)?
        {
            let presented = presentation_of(&node);
            if let Some(text) = presented.label {
                label = text;
            }
            if let Some(name) = presented.icon {
                icon = Some(name);
            }
            on = presented.on;
        }
        // Whether it applies to the item where the view offers it at all, and whether it can
        // run on it now, as the conditions its subject and place decide say: a menu leaves out
        // what cannot apply, and a control for what cannot run stands disabled with the reason.
        let viewer = self.viewer_here();
        let mut applies = true;
        let mut available = true;
        let mut why = String::new();
        if let (Some(refused), _) =
            self.decided(&action.id, &shown.args, &viewer, Decides::Applies)?
        {
            applies = false;
            available = false;
            why = refused;
        } else if let (Some(refused), _) =
            self.decided(&action.id, &shown.args, &viewer, Decides::Requirement)?
        {
            available = false;
            why = refused;
        }
        // An action that still asks for something opens its form: its label says so, as a menu
        // item that opens a dialog does. What it asks is what this offer left unbound — the
        // parameters after the ones it gave, the subject and its places among them — so a
        // command the view or the host answers every question of runs, and reads, as it is.
        let bound = shown
            .args
            .iter()
            .position(|arg| *arg == Value::Missing)
            .unwrap_or(shown.args.len());
        if action.unbound(bound).next().is_some() && !label.ends_with('…') {
            label.push('…');
        }
        let role = action.role.clone().unwrap_or_default();
        let params = shown
            .rest
            .iter()
            .map(|param| {
                Value::Tuple(vec![
                    Value::Text(param.name.clone()),
                    Value::Text(param.ty.clone()),
                    param.default.as_ref().map_or_else(Value::none, |default| {
                        Value::some(Value::Text(default.text.clone()))
                    }),
                ])
            })
            .collect();
        let author = Some(self.module_of_frame()?);
        Ok(Some(Value::Tuple(vec![
            Value::Text(label),
            icon.map_or_else(Value::none, |icon| Value::some(Value::Text(icon))),
            Value::Text(role.clone()),
            Value::Bool(role == "primary" && shown.present),
            Value::Bool(role == "destructive"),
            on.map_or_else(Value::none, |on| Value::some(Value::Bool(on))),
            action
                .key
                .clone()
                .map_or_else(Value::none, |key| Value::some(Value::Text(key))),
            Value::List(params),
            Value::Action(ActionRef {
                action: action.id.clone(),
                args: shown.args.clone(),
                author,
                from: shown.from.clone(),
            }),
            shown
                .group
                .clone()
                .or_else(|| action.group.clone())
                .map_or_else(Value::none, |group| Value::some(Value::Text(group))),
            Value::Bool(available),
            Value::Text(why),
            Value::Bool(applies),
            // After its fields, the action's id, which `a.is(edit)` compares
            // (`tilolang::host::action_id_at`).
            Value::Text(action.id.clone()),
        ])))
    }

    /// Why the action `action` cannot run on `subject` now, as the requirements its subject
    /// alone decides say: the first that does not hold, in `viewer`'s words. `None` when they
    /// all hold, or when the app keeps none the subject could decide. A place the view would
    /// supply is given as nothing: the item stands in no view here.
    pub(crate) fn refusal(
        &self,
        action: &str,
        subject: &RefValue,
        viewer: &Principal,
    ) -> HResult<Option<String>> {
        self.refusal_reading(action, subject, viewer)
            .map(|(reason, _)| reason)
    }

    /// As [`Self::refusal`], with what reading the requirements read: the resources, and whether
    /// they read more than the workspace holds (the clock, the window), which no change in the
    /// workspace moves.
    pub(crate) fn refusal_reading(
        &self,
        action: &str,
        subject: &RefValue,
        viewer: &Principal,
    ) -> HResult<(Option<String>, Reading)> {
        self.decided(
            action,
            &self.subject_args(action, subject),
            viewer,
            Decides::Requirement,
        )
    }

    /// Why the action `action` cannot apply to `subject` where a view offers it, as its
    /// `applies` conditions say: the first that does not hold, in `viewer`'s words, or `None`
    /// when it can.
    pub(crate) fn applies_reading(
        &self,
        action: &str,
        subject: &RefValue,
        viewer: &Principal,
    ) -> HResult<(Option<String>, Reading)> {
        self.decided(
            action,
            &self.subject_args(action, subject),
            viewer,
            Decides::Applies,
        )
    }

    /// What the action `action` shows offered on `subject` now: the label, icon and toggled
    /// state its presentation gives it, with what reading them read. `None` for an action whose
    /// app presents nothing of it there.
    pub(crate) fn presentation_reading(
        &self,
        action: &str,
        subject: &RefValue,
        viewer: &Principal,
    ) -> HResult<(Option<Presentation>, Reading)> {
        let (pkg, presents) = {
            let registry = self.registry.borrow();
            let Some(pkg) = registry.action_package(action) else {
                return Ok((None, Reading::default()));
            };
            let presents = pkg.action(action).and_then(|meta| meta.present.clone());
            (pkg, presents)
        };
        let Some(view) = presents else {
            return Ok((None, Reading::default()));
        };
        self.frames.borrow_mut().push(
            pkg.hash.clone(),
            FrameKind::View(ViewState {
                viewer: viewer.clone(),
                decides: true,
                ..ViewState::default()
            }),
        );
        // The presentation runs in a frame of its own, as a requirement does, so a failure
        // leaves nothing behind and what it read is taken with the rest.
        let node = self.present(&view, subject);
        let frame = self.frames.borrow_mut().pop();
        let Some(FrameKind::View(reads)) = frame.map(|f| f.kind) else {
            return trap("the presentation frame vanished");
        };
        let mut at = reads.reads.clone();
        at.sort_unstable();
        at.dedup();
        let beyond = reads.changes_at.is_some()
            || !reads.state_reads.is_empty()
            || !reads.space_reads.is_empty();
        let shown = node?.as_ref().map(presentation_of);
        Ok((shown, Reading { reads: at, beyond }))
    }

    /// What a condition of `action` is decided on: its subject, and the places after it left to
    /// their own.
    fn subject_args(&self, action: &str, subject: &RefValue) -> Vec<Value> {
        let places = {
            let registry = self.registry.borrow();
            registry
                .action_package(action)
                .and_then(|pkg| pkg.action(action).map(places_of))
                .unwrap_or(0)
        };
        let mut args = vec![Value::Ref(subject.clone())];
        args.extend((0..places).map(|_| Value::none()));
        args
    }

    /// Why the action `action` cannot do what `which` asks — run on the item now, or apply to
    /// the item where the view offers it — as the conditions its subject and the place it is
    /// offered in decide say: the first that does not hold, in `viewer`'s words, or `None` when
    /// they all hold. `args` is what the offer gives before the parameters it asks for.
    fn decided(
        &self,
        action: &str,
        args: &[Value],
        viewer: &Principal,
        which: Decides,
    ) -> HResult<(Option<String>, Reading)> {
        let (pkg, params, export) = {
            let registry = self.registry.borrow();
            let Some(pkg) = registry.action_package(action) else {
                return Ok((None, Reading::default()));
            };
            let Some(meta) = pkg.action(action) else {
                return Ok((None, Reading::default()));
            };
            let exported = match which {
                Decides::Requirement => meta.guard,
                Decides::Applies => meta.applies,
            };
            if !exported {
                return Ok((None, Reading::default()));
            }
            let decided = places_of(meta).saturating_add(1);
            let params = meta.params.get(..decided).unwrap_or_default().to_vec();
            let export = match which {
                Decides::Requirement => tilolang::ir::guard_export(action),
                Decides::Applies => tilolang::ir::applies_export(action),
            };
            (pkg.clone(), params, export)
        };
        self.frames.borrow_mut().push(
            pkg.hash.clone(),
            FrameKind::View(ViewState {
                viewer: viewer.clone(),
                decides: true,
                ..ViewState::default()
            }),
        );
        let given = args.get(..params.len()).unwrap_or(args);
        let answered = self
            .call_in_frame(&pkg, &export, &params, given, Some("Text"))
            .map_err(|e| e.to_string());
        let frame = self.frames.borrow_mut().pop();
        let Some(FrameKind::View(reads)) = frame.map(|f| f.kind) else {
            return trap("the guard frame vanished");
        };
        let reason = match answered? {
            Value::Text(text) => text,
            other => {
                return trap(format!(
                    "the guard of `{action}` answered {} where a reason is",
                    other.kind_name()
                ));
            }
        };
        self.note_time(reads.changes_at);
        self.note_reads(reads.reads.clone());
        {
            let mut frames = self.frames.borrow_mut();
            if let Some(vs) = frames.view_mut() {
                vs.state_reads.extend(reads.state_reads.clone());
                vs.state_seen.extend(reads.state_seen);
                vs.space_reads.extend(reads.space_reads.clone());
            }
        }
        // What the requirement read of the workspace, each field once: what reading it again
        // waits on. A requirement that read the clock, the window's space or the person's view
        // state reads beyond the workspace, and is read again whatever changes.
        let mut at = reads.reads.clone();
        at.sort_unstable();
        at.dedup();
        let beyond = reads.changes_at.is_some()
            || !reads.state_reads.is_empty()
            || !reads.space_reads.is_empty();
        Ok((
            (!reason.is_empty()).then_some(reason),
            Reading { reads: at, beyond },
        ))
    }

    /// The person the code running is for: whom an action offered now would be offered to.
    fn viewer_here(&self) -> Principal {
        let frames = self.frames.borrow();
        match frames.deciding().map(|f| &f.kind) {
            Some(FrameKind::View(vs)) => vs.viewer.clone(),
            Some(FrameKind::Action(state)) => state.invoker.clone(),
            _ => Principal::new(String::new()),
        }
    }

    /// The `present` node the presentation view `view` builds for `subject`, rendered as part
    /// of the running view: what it read is the running view's too.
    fn present(&self, view: &str, subject: &RefValue) -> HResult<Option<crate::view::UiNode>> {
        let (space, window, viewer, depth) = {
            let frames = self.frames.borrow();
            match frames.deciding().map(|f| &f.kind) {
                Some(FrameKind::View(vs)) => (vs.space, vs.window.clone(), vs.viewer.clone(), 1),
                _ => (
                    crate::space::Space::default(),
                    String::new(),
                    Principal::new(String::new()),
                    1,
                ),
            }
        };
        let target = self.view_target(view).map_err(|e| e.to_string())?;
        let offer = Offer {
            space,
            viewer: &viewer,
            window: &window,
            depth,
        };
        let rendered = self
            .render_at(&target, &[Value::Ref(subject.clone())], offer, None)
            .map_err(|e| e.to_string())?;
        self.note_reads(rendered.reads.iter().cloned());
        self.note_time(rendered.changes_at);
        {
            let mut frames = self.frames.borrow_mut();
            if let Some(vs) = frames.view_mut() {
                vs.state_reads.extend(rendered.state_reads.iter().cloned());
                vs.state_seen.extend(rendered.state_seen.iter().cloned());
                vs.space_reads.extend(rendered.space_reads.iter().cloned());
                if rendered.person_reads {
                    vs.read_person();
                }
            }
        }
        Ok(Some(rendered.root))
    }
}

/// What the presentation `node` says of the action it presents: the label it is called there,
/// the icon beside it, and whether the state it toggles is on.
fn presentation_of(node: &crate::view::UiNode) -> Presentation {
    let text = |prop: &str| match node.props.get(prop) {
        Some(Value::Text(text)) => Some(text.clone()),
        _ => None,
    };
    Presentation {
        label: text("label"),
        icon: text("icon"),
        on: match node.props.get("on") {
            Some(Value::Bool(flag)) => Some(*flag),
            Some(Value::Int(flag)) => Some(*flag != 0),
            _ => None,
        },
    }
}

/// The run `run` as a `host.Run`: its group, the work it is a run of, and what it was asked.
fn run_value(run: &crate::store::Run) -> Value {
    Value::Tuple(vec![
        Value::Text(run.id.clone()),
        Value::Text(run.of().to_owned()),
        Value::Text(run.title.clone()),
    ])
}

/// Whether `next`, the version after `last`, folds into the same step of history: both made in
/// the same run, or both by the same person outside any run.
fn same_step(last: &crate::store::Version, next: &crate::store::Version) -> bool {
    match (&last.run, &next.run) {
        (Some(a), Some(b)) => a.id == b.id,
        (None, None) => last.who == next.who,
        _ => false,
    }
}

/// How long after one step the next one by the same hand still joins its visit: a person's
/// steps moments apart read as one visit, under the face of the step that opened it.
const VISIT_SECONDS: i64 = 60 * 60;

/// Whether a step whose last version is `next` opens a visit of its own under `above`, the step
/// history lists just before it (newer): another hand's, or made [`VISIT_SECONDS`] or more after
/// it. The newest step opens the visit history reads from it.
fn opens_visit(above: &crate::store::Version, next: &crate::store::Version) -> bool {
    if above.who != next.who {
        return true;
    }
    match (
        tilolang::calendar::instant(&above.at),
        tilolang::calendar::instant(&next.at),
    ) {
        (Some(above), Some(next)) => above.saturating_sub(next) >= VISIT_SECONDS,
        _ => true,
    }
}

impl Core {
    /// What a value as `c.before` and `c.after` show it says as text, where it is part of a
    /// phrase (an entry of a map, a row's line): a person by name, a case by its label, a day
    /// or a time of day as the host writes it, a list one item after another.
    pub(crate) fn said(&self, shown: &Value) -> String {
        match shown {
            Value::List(items) => items
                .iter()
                .map(|item| self.said(item))
                .collect::<Vec<_>>()
                .join(", "),
            Value::Tuple(parts) => match parts.as_slice() {
                [Value::Text(kind), _, Value::Opt(Some(name))] if kind == "principal" => {
                    said_value(name)
                }
                [Value::Text(kind), held, _] if kind == "date" || kind == "time" => {
                    let text = said_value(held);
                    self.read_person();
                    self.shown
                        .borrow()
                        .as_ref()
                        .map_or_else(|| text.clone(), |host| host.shown(kind, &text))
                }
                [_, held, _] => said_value(held),
                _ => String::new(),
            },
            Value::Principal(who) => self.name_of(who),
            other => said_value(other),
        }
    }
}

fn said_value(value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => x.to_string(),
        Value::Principal(who) => who.0.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::form_asks;

    fn meta() -> crate::meta::Meta {
        tilolang::compile_package(
            "app t.forms \"Forms\"\n\
             enum Colour {\n    red,\n    green\n}\n\
             type Thing {\n    name: Text\n}\n\
             action touch(t: Thing) {\n}\n",
            &tilolang::IdsLock::new(),
            &[],
        )
        .expect("the package compiles")
        .meta
    }

    /// A form asks for each type that has a control, and for none of the types only an
    /// argument or a default reaches: an enum of this package's plain cases, or none of what
    /// a form asks for.
    #[test]
    fn a_form_asks_for_every_type_it_has_a_control_for() {
        let meta = meta();
        for ty in [
            "Int",
            "Float",
            "Bool",
            "Text",
            "RichText",
            "Date",
            "Time",
            "Duration",
            "std.Identifier",
            "Principal",
            "Thing",
            "Option<Thing>",
            "Option<Duration>",
            "enum Colour",
            "Option<enum Colour>",
        ] {
            assert!(form_asks(&meta, ty), "a form asks for {ty}");
        }
        for ty in [
            "Bytes",
            "Response",
            "json.Value",
            "()",
            "Action",
            "View",
            "List<Text>",
            "Set<Text>",
            "Map<Text>",
            "Collection<Thing>",
            "Filter<Thing>",
            "Option<List<Text>>",
            "Option<Option<Text>>",
        ] {
            assert!(!form_asks(&meta, ty), "no form asks for {ty}");
        }
    }
}
