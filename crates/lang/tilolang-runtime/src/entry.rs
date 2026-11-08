//! The entry at the end of a list: what the text typed in it will make (`entry_parts`), and the
//! action it commits, its parameters filled from that text ([`Core::entry_fill`]).
//!
//! The action is one a view built but for its `Text` parameter: a call of a create action
//! (`new_task(tasks, p: project)`). Its parameters are the action's, and their types say what
//! the words before the text typed take: a `Date` or `Time` from a phrase, a `Principal` from
//! `@name` among the workspace's members, a record from `#title` among the records the subject's
//! collection of its type holds. The reader is [`tilolang::entry`], which knows nothing of
//! storage or people; this module answers it from the workspace and builds the values.
//!
//! What is typed fills the action when the entry commits it, not when the view draws: the text
//! the entry sends with its `submit` is the one the action is made of, so a press right after a
//! keystroke reads what was typed.

use std::rc::Rc;

use tilolang::entry::{Filled, Look, Param, Parsed, Takes};
use tilolang::natural::Language;
use tilolang::types::Ty;

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::{HostFail, Wasm};
use crate::host_ui::THEN_ACTION;
use crate::ids::Principal;
use crate::meta::{ActionMeta, FieldKind, TypeKey, element_record, parse_ty};
use crate::store::{FieldSpec, FieldValue};
use crate::value::{ActionRef, RefValue, ResourceRef, Value};

impl Core {
    /// `entry_parts(action, text)`: what the text typed in the entry will make of `action`, as
    /// `host.EntryPart`s (a day, a time, a person, a record), in the order they stand.
    fn entry_parts(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let aref = match self.val(a.h(0)?)? {
            Value::Action(aref) => aref,
            other => {
                return trap(format!(
                    "an entry runs an action, got {}",
                    other.kind_name()
                ));
            }
        };
        let text = self.text(a.h(1)?)?;
        let given: Vec<Option<Value>> = aref.args.iter().cloned().map(Some).collect();
        let (parsed, _) = self.read_entry(&aref.action, &given, &text)?;
        let parts = parsed
            .parts
            .into_iter()
            .map(|part| {
                Value::Tuple(vec![
                    Value::Text(part.label),
                    Value::Opt(Some(Box::new(Value::Text(part.icon.to_owned())))),
                    Value::Text(part.without),
                ])
            })
            .collect();
        self.ret_h(Value::List(parts))
    }

    pub(crate) fn entry_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::EntryParts => self.entry_parts(a),
            _ => trap("not an entry import"),
        }
    }

    /// Fills the action's one `Text` parameter of the action `aref` runs, and the parameters
    /// the text says, with what `text` says ("Call the landlord fri 10am @bo"): the words
    /// nothing else takes title the item, and the rest are its day, its moment, who it is for
    /// and what it is of. An action with no one `Text` parameter, or whose call gave it, is left
    /// as it is, as is every action `then` joins to one that is filled.
    ///
    /// Whether the entry makes anything: text that is nothing but space and what the other
    /// parameters take ("fri 10am @bo") names no item, so the action does not run.
    pub(crate) fn entry_fill(&self, aref: &mut ActionRef, text: &str) -> HResult<bool> {
        if aref.action == THEN_ACTION {
            let mut makes = true;
            for arg in &mut aref.args {
                if let Value::Action(inner) = arg {
                    makes = self.entry_fill(inner, text)? && makes;
                }
            }
            return Ok(makes);
        }
        let given: Vec<Option<Value>> = aref.args.iter().cloned().map(Some).collect();
        let Some(given) = self.entry_fill_given(&aref.action, given, text)? else {
            return Ok(false);
        };
        aref.args = self
            .complete_args(&aref.action, given)
            .map_err(|e| HostFail::Trap(e.to_string()))?;
        Ok(true)
    }

    /// [`Core::entry_fill`] over the arguments of a call not yet made: the parameters `given`
    /// leaves out are filled from `text`, and the entry parameter with the words the text says
    /// of nothing else. A parameter a caller gave is left as it is, except the entry one, which
    /// is what the person typed and so reads as the words other fields did not take.
    ///
    /// `None` when the text names no item: text that is nothing but space and what the other
    /// parameters take ("fri 10am @bo") makes nothing, as it does from a view's entry.
    pub(crate) fn entry_fill_given(
        &self,
        action: &str,
        mut given: Vec<Option<Value>>,
        text: &str,
    ) -> HResult<Option<Vec<Option<Value>>>> {
        let Some((meta, app)) = self.action_meta(action) else {
            // Not a declaration of a loaded package: the run says so.
            return Ok(Some(given));
        };
        let Some(slot) = meta.params.iter().position(|p| p.entry) else {
            return Ok(Some(given));
        };
        // The call may stop at the entry: what it did not give is the text's to fill.
        given.resize(meta.params.len(), None);
        if given.get(slot).is_some_and(Option::is_some) {
            // The call gave it: the entry fills nothing.
            return Ok(Some(given));
        }
        let (parsed, params) = self.read_entry(action, &given, text)?;
        if parsed.title.trim().is_empty() {
            // Nothing typed but what other parameters take makes no item.
            return Ok(None);
        }
        let mut fills = parsed.fills;
        fills.sort_by_key(|(slot, _)| *slot);
        for (slot, filled) in fills {
            let (Some(param), Some(declared), Some(written)) =
                (params.get(slot), meta.params.get(slot), given.get_mut(slot))
            else {
                return trap(format!(
                    "what the entry says of `{action}` names a parameter it has not"
                ));
            };
            let value = self.filled_value(&filled, param)?;
            // As the parameter's type holds it: an optional takes it `.some`.
            *written = Some(match parse_ty(&declared.ty, &app) {
                Ok(Ty::Option(_)) => Value::some(value),
                _ => value,
            });
        }
        // The words nothing else took are the item's own, in the parameter that takes the entry.
        if let Some(written) = given.get_mut(slot) {
            *written = Some(Value::Text(parsed.title));
        }
        Ok(Some(given))
    }

    /// The declaration of the action `id`, and the app that declares it.
    fn action_meta(&self, id: &str) -> Option<(ActionMeta, String)> {
        let registry = self.registry.borrow();
        let pkg = registry.action_package(id)?;
        Some((pkg.action(id)?.clone(), pkg.meta.app.clone()))
    }

    /// What the text says of the action `action`, and what its parameters are.
    fn read_entry(
        &self,
        action: &str,
        given: &[Option<Value>],
        text: &str,
    ) -> HResult<(Parsed, Vec<Param>)> {
        let (meta, app) = self.action_meta(action).ok_or_else(|| {
            HostFail::Trap(format!(
                "the entry runs `{action}`, which no loaded package declares"
            ))
        })?;
        let params: Vec<Param> = meta
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| Param {
                takes: takes(&p.ty, p.entry, &app),
                // A call that gave an optional `.none` left it empty: what is typed fills it.
                given: given
                    .get(i)
                    .is_some_and(|v| v.as_ref().is_some_and(|v| !matches!(v, Value::Opt(None)))),
            })
            .collect();
        let subject = given.iter().flatten().find_map(|arg| match arg {
            Value::Ref(r) => Some(r.target.as_ref().clone()),
            _ => None,
        });
        let look = Workspace {
            core: self,
            subject: subject.as_ref(),
        };
        let now = tilolang::calendar::instant(&self.clock_now()).unwrap_or(0);
        let offset = self.zone.borrow().as_ref().map_or(0, |zone| zone());
        let lang = Language::of(&self.words.borrow().language());
        let parsed = tilolang::entry::read(text, &params, &look, now, offset, lang);
        Ok((parsed, params))
    }

    /// The value a parameter takes from what the text said.
    fn filled_value(&self, filled: &Filled, param: &Param) -> HResult<Value> {
        Ok(match filled {
            Filled::Day(day) => Value::Text(day.clone()),
            Filled::Time(at) => Value::Text(at.clone()),
            Filled::Who(id) => Value::Principal(Principal::new(id.clone())),
            Filled::Record(id) => {
                let Takes::What { app, ty } = &param.takes else {
                    return trap("a record filled a parameter that takes no record");
                };
                let key = TypeKey {
                    app: app.clone(),
                    name: ty.clone(),
                };
                let Some(ty) = self.registry.borrow().type_for(&key) else {
                    return trap(format!("no type `{}` in `{app}`", key.name));
                };
                let Some(target) = ResourceRef::parse_id(id, ty) else {
                    return trap(format!("malformed record `{id}`"));
                };
                Value::Ref(RefValue {
                    target: Rc::new(target),
                    author: None,
                })
            }
        })
    }
}

/// What a parameter of type `ty` takes out of the text typed in an entry.
fn takes(ty: &str, entry: bool, app: &str) -> Takes {
    if entry {
        return Takes::Entry;
    }
    let Ok(ty) = parse_ty(ty, app) else {
        return Takes::Other;
    };
    let ty = match ty {
        Ty::Option(inner) => *inner,
        other => other,
    };
    match ty {
        Ty::Date => Takes::Day,
        Ty::Time => Takes::Time,
        Ty::Principal => Takes::Who,
        Ty::Record(key) => Takes::What {
            app: key.app,
            ty: key.name,
        },
        _ => Takes::Other,
    }
}

/// What the text of an entry can name, answered from the workspace the action runs in: the
/// members, and the records the subject's collections hold.
struct Workspace<'a> {
    core: &'a Core,
    subject: Option<&'a ResourceRef>,
}

impl Look for Workspace<'_> {
    fn people(&self) -> Vec<(String, String)> {
        let names = self.core.names.borrow();
        self.core
            .read_members()
            .into_iter()
            .map(|member| {
                let id = member.who.to_string();
                let name = names
                    .as_ref()
                    .map_or_else(|| id.clone(), |names| names.name(&member.who));
                (name, id)
            })
            .collect()
    }

    fn records(&self, app: &str, ty: &str) -> Vec<(String, String)> {
        let Some(subject) = self.subject else {
            return Vec::new();
        };
        let Ok((_, subject_ty)) = self.core.type_of_ref(&subject.ty) else {
            return Vec::new();
        };
        // The subject's collections of the type, as the type declares them
        // (`projects: Collection<Project>`).
        let key = TypeKey {
            app: app.to_owned(),
            name: ty.to_owned(),
        };
        let Some(element) = self.core.registry.borrow().type_for(&key) else {
            return Vec::new();
        };
        let Ok((_, element_ty)) = self.core.type_of_ref(&element) else {
            return Vec::new();
        };
        let Some(title) = element_ty.name_field() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for field in &subject_ty.fields {
            if field.kind != FieldKind::Collection || element_record(&field.ty) != Some(&key) {
                continue;
            }
            let spec = FieldSpec {
                id: field.id.clone(),
                kind: field.kind,
            };
            let Ok(FieldValue::Items(items)) = self.core.store.borrow().read(subject, &spec) else {
                continue;
            };
            for item in items {
                let item_ref = ResourceRef {
                    doc: subject.doc.clone(),
                    item: Some(item),
                    ty: element.clone(),
                };
                let spec = FieldSpec {
                    id: title.id.clone(),
                    kind: title.kind,
                };
                if let Ok(FieldValue::Text(title)) = self.core.store.borrow().read(&item_ref, &spec)
                {
                    out.push((title, item_ref.id_string()));
                }
            }
        }
        out
    }
}
