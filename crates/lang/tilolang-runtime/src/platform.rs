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
use crate::handles::FrameKind;
use crate::ids::Principal;
use crate::meta::{ActionMeta, Meta, ParamMeta, TypeMeta};
use crate::render::Offer;
use crate::seen::SeenMark;
use crate::value::{ActionRef, RefValue, ResourceRef, Value};
use crate::view_state::StateKey;

/// The window the lenses on each resource are kept under, as view state of no window: set by
/// the host as presence moves, read by `lenses(r)`.
pub const LENS_WINDOW: &str = "$lenses";
/// Where the runtime keeps how each resource's outside effects stand (`delivery(r)`).
pub const DELIVERY_WINDOW: &str = "$delivery";


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
fn names_type(meta: &Meta, name: &str, id: &str) -> bool {
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
fn all_asked(meta: &Meta, params: &[ParamMeta]) -> bool {
    params
        .iter()
        .all(|param| param.default.is_some() || form_asks(meta, &param.ty))
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
/// type's control (forms-and-diagrams.md §1).
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

/// The state key `delivery(r)` reads for the resource `id`.
#[must_use]
pub fn delivery_key(id: &str) -> StateKey {
    StateKey {
        window: DELIVERY_WINDOW.to_owned(),
        principal: None,
        state: id.to_owned(),
    }
}

/// The key of the lenses on the resource `id`.
#[must_use]
pub fn lens_key(id: &str) -> StateKey {
    StateKey {
        window: LENS_WINDOW.to_owned(),
        principal: None,
        state: id.to_owned(),
    }
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

/// One action offered on a subject, before it is presented.
struct Offered<'m> {
    action: &'m ActionMeta,
    /// What it is applied to: the subject, or the resource holding the subject and the subject.
    args: Vec<Value>,
    /// Its parameters after those.
    rest: &'m [ParamMeta],
    /// Whether it is presented for the subject: its own actions are, its holder's not.
    present: bool,
}

/// The longest run of a conversation's unread messages a mention count reads: one page of
/// them, so a workspace nobody has read is not read message by message for a badge.
const MENTION_SCAN: usize = 64;

impl Core {
    /// `delivery(r)` of the document `doc`: how its last outside effect stands, as the host
    /// last said; a view that read it runs again when that moves.
    fn delivery_of(&self, doc: &str) -> HResult<Value> {
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
        self.note_state_read(key, generation, &value)?;
        Ok(value)
    }

    /// `dated(text, at)`: when `text` says something is due, read at the time `at` in the
    /// person's zone, as a `host.Dated` tuple of the rest of the text, the day and the moment.
    fn dated(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let text = self.text(a.h(0)?)?;
        let at = self.text(a.h(1)?)?;
        let Some(now) = tilolang::calendar::instant(&at) else {
            return trap(format!("dated: `{at}` is not a time"));
        };
        let offset = self.zone.borrow().as_ref().map_or(0, |zone| zone());
        let read = tilolang::natural::dated(&text, now, offset);
        let opt = |v: Option<String>| Value::Opt(v.map(|t| Box::new(Value::Text(t))));
        self.ret_h(Value::Tuple(vec![
            Value::Text(read.text),
            opt(read.day),
            opt(read.at),
        ]))
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
                let offered = self.actions_on(&target)?;
                self.ret_h(Value::List(offered))
            }
            Imp::Delivery => {
                let target = self.ref_arg(a.h(0)?)?;
                let value = self.delivery_of(&target.target.doc)?;
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
                self.note_state_read(key, generation, &value)?;
                self.ret_h(value)
            }
            Imp::Versions | Imp::VersionEnd | Imp::RunChanges | Imp::Unseen | Imp::Steps => {
                let value = self.versions_import(imp, a)?;
                self.ret_h(value)
            }
            Imp::Mentions => {
                // `mentions(c, field, Message)`: how many of `c`'s items name the person looking,
                // that they have not seen: a channel's mention count, next to its unread count.
                let coll = self.ref_arg(a.h(0)?)?;
                let field = self.text(a.h(1)?)?;
                let item = self.text(a.h(2)?)?;
                let person = self.looking()?;
                let count = self
                    .mentions_of(&person, &coll.target, &field, &item)
                    .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
                self.read_versions_of(&coll.target)?;
                self.note_seen_read(&person, &coll.target.id_string())?;
                crate::core::ret_i64(i64::try_from(count).unwrap_or(i64::MAX))
            }
            Imp::FirstUnseen => {
                // `first_unseen(c, field, Message)`: where the first unread item of `c`'s
                // collection begins, read by the frame running, so the view runs again when it
                // moves.
                let coll = self.ref_arg(a.h(0)?)?;
                let field = self.text(a.h(1)?)?;
                let item = self.text(a.h(2)?)?;
                let person = self.looking()?;
                let found = self
                    .first_unseen_of(&person, &coll.target, &field, &item)
                    .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
                // The view read what the answer is made of: `c`'s own versions (which move when
                // an item is posted) and the person's seen mark and reviewed runs (which move
                // when they read on).
                self.read_versions_of(&coll.target)?;
                self.note_seen_read(&person, &coll.target.id_string())?;
                let value = found.map_or_else(Value::none, |id| {
                    let at = coll.target.item(&id, item);
                    Value::some(Value::resource(&at))
                });
                self.ret_h(value)
            }
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
            Imp::Dated => self.dated(a),
            Imp::Phrase => {
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
                let text = self.words.borrow().word(&id, &args, &id);
                self.ret_h(Value::Text(text))
            }
            _ => trap("not a platform import"),
        }
    }

    /// `workspace.me`: the person the view is shown to, which the frame running carries as the
    /// runtime was opened for them. An action reads `invoker` instead: it acts for someone,
    /// where the viewer only looks.
    fn viewer_import(&self) -> HResult<Option<Wasm>> {
        let who = self.looking()?;
        self.ret_h(Value::Principal(crate::ids::Principal(who)))
    }

    /// A version of `of` as a `host.Version`: who, when, what, the task run, its kind, the
    /// fields it changed by name, the resource, what its action says it does and its icon, and
    /// each field it changed with what it held before and after; then the id of the action that
    /// made it, which `v.is(edit)` compares (`tilolang::host::action_id_at`).
    fn version_value(&self, v: crate::store::Version, of: &ResourceRef) -> HResult<Value> {
        let task = v.task.as_ref().map(|run| {
            Box::new(Value::Tuple(vec![
                Value::Text(run.id.clone()),
                Value::Text(run.task().to_owned()),
                Value::Text(run.title.clone()),
            ]))
        });
        let info = self.type_of_ref(&of.ty).ok().map(|(_, info)| info);
        let fields = v
            .fields
            .iter()
            .map(|id| {
                let name = info
                    .as_ref()
                    .and_then(|info| info.field(id))
                    .map_or_else(|| id.clone(), |f| f.name.clone());
                Value::Text(name)
            })
            .collect();
        let declared = self
            .registry
            .borrow()
            .action_package(&v.action)
            .and_then(|pkg| pkg.action(&v.action).cloned());
        let description = declared
            .as_ref()
            .map_or_else(String::new, |a| a.doc.clone());
        let icon = declared
            .as_ref()
            .and_then(|a| a.icon.clone())
            .map(|icon| Box::new(Value::Text(icon)));
        let mine = v.task.is_none() && self.looking_now().as_deref() == Some(v.who.as_str());
        let changes = match &info {
            Some(info) => self.changes_value(&v, of, info)?,
            None => Vec::new(),
        };
        Ok(Value::Tuple(vec![
            Value::Principal(Principal(v.who)),
            Value::Text(v.at),
            Value::Text(self.action_label(&v.action)),
            Value::Opt(task),
            Value::Text(v.kind.word().to_owned()),
            Value::List(fields),
            Value::resource(of),
            Value::Bool(mine),
            Value::Text(description),
            Value::Opt(icon),
            Value::List(changes),
            Value::Text(v.action),
            Value::Text(v.change),
        ]))
    }

    /// Each field the version `v` of `of` changed, as a `host.Change`: its name, its name in
    /// words, and what it held before and after as its type shows it, read from the change
    /// itself. None for a version that made or removed the resource, or one not committed yet.
    fn changes_value(
        &self,
        v: &crate::store::Version,
        of: &ResourceRef,
        info: &crate::registry::TypeInfo,
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
        let mut out = Vec::with_capacity(written.len());
        for (id, before, after) in written {
            let Some(field) = info.field(&id) else {
                continue;
            };
            let side = |held| -> HResult<Value> {
                let value = self
                    .field_value(of, field, held, None)
                    .map_err(crate::engine::HostFail::Trap)?;
                self.change_shown(&field.ty, &value)
            };
            out.push(Value::Tuple(vec![
                Value::Text(field.name.clone()),
                Value::Text(sentence(&field.name)),
                side(before)?,
                side(after)?,
            ]));
        }
        Ok(out)
    }

    /// `value`, of the type `ty`, as `c.before` and `c.after` show it ([`Self::change_view`]):
    /// the kind of thing it is, what stands for it, and what goes with it (a case's icon, a
    /// person's name). A list of these for a set or a list.
    fn change_shown(&self, ty: &tilolang::types::Ty, value: &Value) -> HResult<Value> {
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
            (Ty::Option(inner), Value::Opt(Some(held))) => return self.change_shown(inner, held),
            (Ty::Date, Value::Text(day)) => one("date", Value::Text(day.clone()), None),
            (Ty::Time, Value::Text(at)) => one("time", Value::Text(at.clone()), None),
            (Ty::Principal, Value::Principal(who)) => {
                let name = self
                    .names
                    .borrow()
                    .as_ref()
                    .map_or_else(|| who.0.clone(), |host| host.name(who));
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
                    .map(|item| self.change_shown(inner, item))
                    .collect::<HResult<_>>()?,
            ),
            // A map: each entry as its key and what it holds, as people read it ("👍 Ada, Bo").
            (Ty::Map(inner), Value::List(entries)) => Value::List(
                entries
                    .iter()
                    .map(|entry| match entry {
                        Value::Tuple(pair) => match pair.as_slice() {
                            [Value::Text(key), held] => {
                                let shown = said(&self.change_shown(inner, held)?);
                                Ok(one("text", Value::Text(format!("{key} {shown}")), None))
                            }
                            _ => trap("a map entry is a key and its value"),
                        },
                        other => trap(format!("a map entry holds {}", other.kind_name())),
                    })
                    .collect::<HResult<_>>()?,
            ),
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
                Imp::VersionEnd => Value::none(),
                _ => Value::List(Vec::new()),
            });
        }
        Ok(match imp {
            Imp::VersionEnd => {
                let last = a.h(1)? != 0;
                let end = self.store.borrow().version_end(at, last).map_err(trapped)?;
                self.read_versions_of(at)?;
                match end {
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
            Imp::Steps => {
                let versions = self.store.borrow().versions(at).map_err(trapped)?;
                self.read_versions_of(at)?;
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
                for step in steps.into_iter().rev() {
                    let Some(last) = step.last() else {
                        continue;
                    };
                    let who = Value::Principal(Principal(last.who.clone()));
                    let at_time = Value::Text(last.at.clone());
                    let task = Value::Opt(last.task.as_ref().map(|run| {
                        Box::new(Value::Tuple(vec![
                            Value::Text(run.id.clone()),
                            Value::Text(run.task().to_owned()),
                            Value::Text(run.title.clone()),
                        ]))
                    }));
                    let mine = last.task.is_none()
                        && self.looking_now().as_deref() == Some(last.who.as_str());
                    let versions = step
                        .into_iter()
                        .map(|v| self.version_value(v, at))
                        .collect::<HResult<_>>()?;
                    out.push(Value::Tuple(vec![
                        who,
                        at_time,
                        task,
                        Value::List(versions),
                        Value::Bool(mine),
                    ]));
                }
                Value::List(out)
            }
            _ => {
                let versions = self.store.borrow().versions(at).map_err(trapped)?;
                self.read_versions_of(at)?;
                Value::List(
                    versions
                        .into_iter()
                        .map(|v| self.version_value(v, at))
                        .collect::<HResult<_>>()?,
                )
            }
        })
    }

    /// `changes(of: run)` of the `host.TaskRun` held at `h`: every version it made, oldest
    /// first, each of the resource it is a version of.
    fn run_changes_value(&self, h: i32) -> HResult<Value> {
        let run = match self.val(h)? {
            Value::Tuple(fields) => match fields.into_iter().next() {
                Some(Value::Text(id)) => id,
                _ => return trap("changes: a task run names its group first"),
            },
            other => {
                return trap(format!(
                    "changes: expected a task run, got {}",
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
            self.read_versions_of(&at)?;
            list.push(self.version_value(v, &at)?);
        }
        Ok(Value::List(list))
    }

    /// Who is looking, when someone is: [`Self::looking`] in a view or an action, `None` in
    /// code no one is looking through (a function an agent's query runs).
    pub(crate) fn looking_now(&self) -> Option<String> {
        let frames = self.frames.borrow();
        let top = frames.top().ok()?;
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
        Ok(match &frames.top()?.kind {
            FrameKind::View(view) => view.viewer.0.clone(),
            FrameKind::Action(state) => state
                .acting_as
                .last()
                .map_or(&state.invoker, |(who, _)| who)
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

    /// `person`'s seen mark that covers `resource`: its own, else the mark of whatever holds
    /// it, then of whatever holds that. Opening a conversation marks seen what its window
    /// showed (task-legibility.md §4), and what it showed is the messages in it: items are
    /// stored flat by id (convergence.md, "Items"), so the collection holding one names its
    /// holder, never the path down to it.
    pub(crate) fn seen_mark_of_tree(&self, person: &str, at: &ResourceRef) -> Option<SeenMark> {
        if let Some(mark) = self.seen_mark(person, &at.id_string()) {
            return Some(mark);
        }
        let mut holding = at.clone();
        // A collection of items nests at most a few deep (a board's card, a card's comment).
        for _ in 0..8 {
            let Ok(Some((holder, _))) = self.store.borrow().container(&holding) else {
                return None;
            };
            if let Some(mark) = self.seen_mark(person, &holder.id_string()) {
                return Some(mark);
            }
            holding = holder;
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
        let mark_others = self.marks_others(&at.ty);
        Ok(crate::seen::unseen(
            &versions,
            person,
            mark.as_ref(),
            &runs,
            mark_others,
        ))
    }

    /// The first item of `at`'s collection `field` that `person` has not seen, oldest first (a
    /// conversation's first unread message), as a transcript shows it.
    ///
    /// Read item by item from the collection's end, a page at most: an item is unseen when a
    /// version of it is newer than what the person has seen of `at` (its mark, or the newest
    /// item they made, which they wrote and so read). The first one found, oldest first, is the
    /// answer, and a channel the person is caught up with costs the page it looks through.
    pub(crate) fn first_unseen_of(
        &self,
        person: &str,
        at: &ResourceRef,
        field: &str,
        item: &str,
    ) -> Result<Option<String>, crate::store::StoreError> {
        // How many items from the end are looked through for the first unread one: one index
        // page, which is what a listing reads to place an item.
        const TAIL: usize = 64;
        let store = self.store.borrow();
        let storage = self
            .type_of_ref(&at.ty)
            .ok()
            .and_then(|(_, info)| info.field_by_id_or_name(field).map(|f| f.id.clone()));
        let Some(storage) = storage else {
            return Ok(None);
        };
        // A record the store does not hold yet (one just made, or empty) has nothing unseen.
        let Ok(items) = store.listing(
            at,
            &crate::store::FieldSpec {
                id: storage,
                kind: crate::meta::FieldKind::Collection,
            },
        ) else {
            return Ok(None);
        };
        let len = items.len();
        if len == 0 {
            return Ok(None);
        }
        let runs = self.seen_runs(person);
        let mark_others = self.marks_others(item);
        let mark = self.seen_mark(person, &at.id_string());
        // Where the person read to, by the log rather than by a clock: the newest item whose
        // change their mark of the channel names (a store with no channel versions names the
        // newest item it holds), and the newest item they made, since writing one means reading
        // up to it. Both are looked for in the tail, a page deep.
        let mut read_to: Option<usize> = None;
        for index in (len.saturating_sub(TAIL)..len).rev() {
            let Some(id) = items.get(index) else {
                continue;
            };
            let versions = store.versions(&at.item(&id, item.to_owned()))?;
            let named = mark
                .as_ref()
                .is_some_and(|mark| versions.iter().any(|v| v.change == mark.change));
            if named || versions.iter().any(|v| v.who == person) {
                read_to = Some(read_to.map_or(index, |at| at.max(index)));
            }
        }
        let from = read_to.map_or(0, |index| index.saturating_add(1));
        // The first item after where they read that someone changed since, oldest first: a
        // version of theirs is one they saw, so it never makes an item unread of its own. With
        // nothing read (no mark, nothing of theirs), the first item is where they stopped: it
        // alone is asked, so an unread conversation costs one message rather than all of them.
        let upto = if read_to.is_none() {
            from.saturating_add(1)
        } else {
            len
        };
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
            if !crate::seen::unseen(&versions, person, seen.as_ref(), &runs, mark_others).is_empty()
            {
                return Ok(Some(id.to_string()));
            }
        }
        Ok(None)
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
        let upto = from.saturating_add(MENTION_SCAN).min(len);
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

    /// Whether the type `ty` declares `mark others`: another person's change to it is marked
    /// unseen, as an agent's is.
    pub(crate) fn marks_others(&self, ty: &str) -> bool {
        self.registry
            .borrow()
            .type_info(ty)
            .is_some_and(|(_, info)| info.mark_others)
    }

    /// The task runs `person` reviewed, as the runtime holds them.
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
            self.note_state_read(key, generation, &value)?;
        }
        self.read_versions_of(at)?;
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
                mark,
            });
        Ok(true)
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

    /// The task runs `person` reviewed are `runs` now.
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

    /// Record, in a view's frame, that it read the versions of `target`: it runs again when any
    /// field of the resource changes, which is a new version.
    pub(crate) fn read_versions_of(&self, target: &crate::value::ResourceRef) -> HResult<()> {
        let id = target.id_string();
        let fields: Vec<String> = self
            .type_of_ref(&target.ty)
            .map(|(_, info)| info.fields.iter().map(|f| f.id.clone()).collect())
            .unwrap_or_default();
        if let FrameKind::View(vs) = &mut self.frames.borrow_mut().top_mut()?.kind {
            for field in fields {
                vs.reads.push((id.clone(), field));
            }
        }
        Ok(())
    }

    /// What an action is called where history names it: its label, or what the host did.
    fn action_label(&self, id: &str) -> String {
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
    pub(crate) fn note_state_read(
        &self,
        key: StateKey,
        generation: u64,
        seen: &Value,
    ) -> HResult<()> {
        let mut frames = self.frames.borrow_mut();
        if let FrameKind::View(vs) = &mut frames.top_mut()?.kind {
            vs.state_reads.push((key.clone(), generation));
            vs.state_seen.push((key, seen.clone()));
        }
        Ok(())
    }

    /// Record, in a view's frame, that it read `person`'s seen mark of `resource`: it runs
    /// again when the mark moves.
    pub(crate) fn note_seen_read(&self, person: &str, resource: &str) -> HResult<()> {
        let key = crate::seen::seen_key(person, resource);
        let (generation, value) = {
            let states = self.view_states.borrow();
            (
                states.generation(&key),
                states.get(&key).cloned().unwrap_or_else(Value::none),
            )
        };
        self.note_state_read(key, generation, &value)
    }

    /// Take `(resource, field)` reads into the running view's reads, when a view runs.
    pub(crate) fn note_reads(
        &self,
        reads: impl IntoIterator<Item = (String, String)>,
    ) -> HResult<()> {
        let mut frames = self.frames.borrow_mut();
        if let FrameKind::View(vs) = &mut frames.top_mut()?.kind {
            vs.reads.extend(reads);
        }
        Ok(())
    }

    /// Record, in a view's frame, that what it read of the time reads differently at
    /// `changes_at` (seconds since 1970, UTC): it runs again then. Outside a view nothing
    /// follows the time: an action reads one time throughout.
    pub(crate) fn note_time(&self, changes_at: Option<i64>) -> HResult<()> {
        if changes_at.is_none() {
            return Ok(());
        }
        let mut frames = self.frames.borrow_mut();
        if let FrameKind::View(vs) = &mut frames.top_mut()?.kind {
            vs.changes_at = crate::render::earliest(vs.changes_at, changes_at);
        }
        Ok(())
    }

    /// What the platform shows of the type `ty`, and of the resource `r` of it when there is
    /// one: a `host.Meta`.
    fn type_meta(&self, ty: &str, r: Option<&RefValue>) -> HResult<Value> {
        // Another app's resource names its type `app#Type`, as its readings come in.
        let resolved = self.registry.borrow().type_id(ty);
        let ty = resolved.as_deref().unwrap_or(ty);
        let (pkg, _) =
            self.registry.borrow().type_info(ty).ok_or_else(|| {
                format!("meta: type `{ty}` is not declared by any loaded package")
            })?;
        let meta = &pkg.meta;
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
        ]))
    }

    /// What one (`count` 1) or several of the type `id` are called, in the reader's language:
    /// its catalogue's noun, else `source`.
    fn noun(&self, id: &str, count: i64, source: &str) -> String {
        let key = format!("{}.noun", tilolang::messages::key_of(id));
        self.words
            .borrow()
            .word(&key, &[("count".to_owned(), count.to_string())], source)
    }

    /// The title of `target`: its field marked `title`, when it holds more than spaces; else,
    /// for a type without one, the first line of its field marked `body` (a message is called
    /// by what it says, rich text as its reader reads it); else, for a root of its package, the
    /// package's title; else its type's name.
    pub(crate) fn title_of(
        &self,
        target: &ResourceRef,
        meta: &Meta,
        ty: &TypeMeta,
    ) -> HResult<String> {
        let role = |name: &str| {
            ty.fields
                .iter()
                .find(|field| field.role.as_deref() == Some(name))
        };
        let titled = role("title").map(|field| (field, false));
        if let Some((field, body)) = titled.or_else(|| role("body").map(|field| (field, true))) {
            let (_, info) = self.lookup_field(target, &field.id)?;
            self.note_reads([(target.id_string(), field.id.clone())])?;
            let value = self.read_value(target, &info, None)?;
            let text = match value {
                Value::Text(text) if body => text
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or_default()
                    .to_owned(),
                Value::Text(text) => text,
                Value::Rich(rich) => self.read_rich(&rich, true)?,
                _ => String::new(),
            };
            if !text.trim().is_empty() {
                return Ok(text);
            }
        }
        if target.item.is_none() && meta.roots.contains(&ty.id) {
            return Ok(meta.title.clone());
        }
        Ok(sentence(&ty.name))
    }

    /// The actions offered on `subject`: its type's own that need nothing else (what else they
    /// take has a default, which the invocation gets), the primary one first, and then the
    /// actions of the resource holding it that take it as their second argument, each a
    /// `host.Action`.
    fn actions_on(&self, subject: &RefValue) -> HResult<Vec<Value>> {
        let target = &subject.target;
        let packages = self.registry.borrow().packages();
        let mut out = Vec::new();
        // The subject's own.
        let found = self.registry.borrow().type_info(&target.ty);
        if let Some((pkg, info)) = found {
            let mut own: Vec<&ActionMeta> = pkg
                .meta
                .offered_actions()
                .filter(|action| {
                    action.params.split_first().is_some_and(|(first, rest)| {
                        all_asked(&pkg.meta, rest)
                            && !asks_for_own_item(&pkg.meta, info.id.as_str(), rest)
                            && names_type(&pkg.meta, &first.ty, info.id.as_str())
                    })
                })
                .collect();
            own.sort_by_key(|action| action.role.as_deref() != Some("primary"));
            for action in own {
                let shown = Offered {
                    action,
                    args: vec![Value::Ref(subject.clone())],
                    rest: action.params.get(1..).unwrap_or_default(),
                    present: true,
                };
                out.extend(self.action_value(&shown, subject)?);
            }
        }
        // The holder's, on it.
        if let Some(holder) = self.holder_of(target) {
            let holder_ty = holder.ty.clone();
            for pkg in &packages {
                let Some(holder_info) = pkg.type_by_id(&holder_ty) else {
                    continue;
                };
                for action in pkg.meta.offered_actions() {
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
                    };
                    out.extend(self.action_value(&shown, subject)?);
                }
            }
        }
        Ok(out)
    }

    /// The resource holding the item `target`, with its type: where the store has it now.
    fn holder_of(&self, target: &ResourceRef) -> Option<ResourceRef> {
        let (holder, _) = self.store.borrow().container(target).ok()??;
        Some(holder)
    }

    /// An offered action as a `host.Action`: presented for the subject (its label, icon and
    /// toggled state as its presentation view gives them now), the view taking what that read.
    /// `None` while a requirement of it on the subject does not hold: it is not offered.
    fn action_value(&self, shown: &Offered<'_>, subject: &RefValue) -> HResult<Option<Value>> {
        let action = shown.action;
        let mut label =
            self.words
                .borrow()
                .word(&tilolang::messages::key_of(&action.id), &[], &label_of(action));
        let mut icon = action.icon.clone();
        let mut on: Option<bool> = None;
        if shown.present
            && let Some(view) = &action.present
            && let Some(node) = self.present(view, subject)?
        {
            if let Some(Value::Text(text)) = node.props.get("label") {
                label.clone_from(text);
            }
            if let Some(Value::Text(name)) = node.props.get("icon") {
                icon = Some(name.clone());
            }
            on = match node.props.get("on") {
                Some(Value::Bool(flag)) => Some(*flag),
                Some(Value::Int(flag)) => Some(*flag != 0),
                _ => None,
            };
            if matches!(node.props.get("unavailable"), Some(Value::Text(why)) if !why.is_empty()) {
                return Ok(None);
            }
        }
        // An action that still asks for something opens its form: its label says so, as a
        // menu item that opens a dialog does.
        if shown.rest.iter().any(|param| param.default.is_none()) && !label.ends_with('…') {
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
            }),
            action
                .group
                .clone()
                .map_or_else(Value::none, |group| Value::some(Value::Text(group))),
            Value::Bool(action.frequent),
            // After its fields, the action's id, which `a.is(edit)` compares
            // (`tilolang::host::action_id_at`).
            Value::Text(action.id.clone()),
        ])))
    }

    /// The `present` node the presentation view `view` builds for `subject`, rendered as part
    /// of the running view: what it read is the running view's too.
    fn present(&self, view: &str, subject: &RefValue) -> HResult<Option<crate::view::UiNode>> {
        let (space, window, viewer, depth) = {
            let frames = self.frames.borrow();
            match &frames.top()?.kind {
                FrameKind::View(vs) => (vs.space, vs.window.clone(), vs.viewer.clone(), 1),
                _ => (
                    crate::space::Space::default(),
                    String::new(),
                    Principal(String::new()),
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
        self.note_reads(rendered.reads.iter().cloned())?;
        self.note_time(rendered.changes_at)?;
        {
            let mut frames = self.frames.borrow_mut();
            if let FrameKind::View(vs) = &mut frames.top_mut()?.kind {
                vs.state_reads.extend(rendered.state_reads.iter().cloned());
                vs.state_seen.extend(rendered.state_seen.iter().cloned());
                vs.space_reads.extend(rendered.space_reads.iter().cloned());
            }
        }
        Ok(Some(rendered.root))
    }
}

/// Whether `next`, the version after `last`, folds into the same step of history: both made in
/// the same task run, or both by the same person outside any run.
fn same_step(last: &crate::store::Version, next: &crate::store::Version) -> bool {
    match (&last.task, &next.task) {
        (Some(a), Some(b)) => a.id == b.id,
        (None, None) => last.who == next.who,
        _ => false,
    }
}

/// What a value as `c.before` and `c.after` show it says as text, where it is part of a phrase
/// (an entry of a map): a person by name, a case by its label, a list one item after another.
fn said(shown: &Value) -> String {
    match shown {
        Value::List(items) => items.iter().map(said).collect::<Vec<_>>().join(", "),
        Value::Tuple(parts) => match parts.as_slice() {
            [Value::Text(kind), _, Value::Opt(Some(name))] if kind == "principal" => {
                said_value(name)
            }
            [_, held, _] => said_value(held),
            _ => String::new(),
        },
        other => said_value(other),
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
