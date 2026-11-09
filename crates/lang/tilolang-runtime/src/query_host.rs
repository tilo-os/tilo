//! The imports behind `query(T, ...)`, `items.matching(q)`, the filters a type declares
//! (`filters(T)`, `Task.today`) and the parts of a `std.Identifier` (see [`crate::query`]).

use std::rc::Rc;

use tilolang::messages;
use tilolang::meta::{FilterKind, FilterMeta, TypeMeta};

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::meta::EnumMeta;
use crate::query::{Clock, Query, Term, find, resolve};
use crate::store::{FieldSpec, Lit, lit_of, ordered_cmp};
use crate::value::{RefValue, Value};

impl Core {
    /// The clock queries are read against: the host's, to the minute, so a view that asks
    /// `received: -7d` asks the same query on every frame of that minute, in the person's zone,
    /// so `today` is their day.
    fn query_clock(&self) -> HResult<Clock> {
        let now = self
            .clock
            .borrow()
            .as_ref()
            .map_or_else(|| crate::core::EPOCH.to_owned(), |clock| clock());
        let minute = now.get(..17).map(|m| format!("{m}00Z")).unwrap_or(now);
        let zone = self.zone.borrow().as_ref().map_or(0, |zone| zone());
        Ok(Clock::at(&minute)?.in_zone(zone))
    }

    /// `query(ty, scope, terms, text)`: the resources of the type `ty` that the query of the
    /// filters `terms` (`(key, value)` pairs) and the typed search `text` holds of, inside
    /// `scope`: a collection read in a view (answered by the store, a collection again), a list
    /// of resources, or none (every one of the type, outside a view).
    pub(crate) fn query_import(
        &self,
        ty: i32,
        scope: i32,
        terms: i32,
        text: i32,
    ) -> HResult<Value> {
        let ty = self.text(ty)?;
        let text = self.text(text)?;
        let mut query: Query = find(&text).map_err(|why| format!("query `{text}`: {why}"))?;
        if let Value::List(pairs) = self.val(terms)? {
            for pair in pairs {
                let Value::Tuple(parts) = pair else {
                    return trap("query: a term is a (key, value) pair");
                };
                let [Value::Text(key), Value::Text(value)] = parts.as_slice() else {
                    return trap("query: a term is a (key, value) pair of texts");
                };
                if key == "sort" {
                    query.sort = Some(value.clone());
                } else {
                    query.terms.push(Term::new(key, value));
                }
            }
        }
        let (pkg, info) =
            self.registry.borrow().type_info(&ty).ok_or_else(|| {
                format!("query: type `{ty}` is not declared by any loaded package")
            })?;
        let Some(declared) = pkg.meta.types.iter().find(|t| t.id == ty) else {
            return trap(format!("query: type `{ty}` is not declared"));
        };
        let clock = self.query_clock()?;
        let viewer = self.looking().ok();
        let resolved = resolve(&query, declared, &pkg.meta.enums, &clock, viewer.as_deref())?;
        // A view that asked by the time (`is:today`, `received:-7d`) runs again when what it
        // read of it moves on, and only then: at the value the store keeps that lies closest
        // after now, or at the person's next midnight for the day.
        let next = self
            .store
            .borrow()
            .next_clock_change(&ty, &resolved.clock)
            .map_err(|e| e.to_string())?;
        clock.instants(next);
        self.note_time(clock.changes_at());
        // A query that asks nothing: the items as they are.
        if resolved.pred == tilolang::pred::Pred::Const(true) && resolved.order.is_none() {
            let held = self.val(scope)?;
            if matches!(held, Value::Coll(_) | Value::List(_)) {
                return Ok(held);
            }
        }
        let Some(pred) = crate::colls::field_reads(&resolved.pred, &info) else {
            return trap(format!(
                "query: {} cannot be read as its filters ask",
                declared.name
            ));
        };
        match self.val(scope)? {
            Value::Coll(coll) => Ok(Value::Coll(Rc::new(coll.queried(pred, resolved.order)))),
            Value::List(items) => self.query_list(items, &pred, resolved.order.as_ref()),
            Value::Opt(None) | Value::Unit => self.query_all(&ty, &pred, resolved.order.as_ref()),
            other => trap(format!(
                "query: `in:` is a collection or a list of resources, not {}",
                other.kind_name()
            )),
        }
    }

    /// The resources of `items` that `pred` holds of, in the order `order`, each read from
    /// the store.
    pub(crate) fn query_list(
        &self,
        items: Vec<Value>,
        pred: &crate::store::Pred,
        order: Option<&crate::query::Order>,
    ) -> HResult<Value> {
        // Another app's resources are read from their readings, each the field the filter
        // asks of it: one whose reading has not come in reads as it would start until it does,
        // and the view that asked runs again then.
        if items
            .iter()
            .any(|item| matches!(item, Value::Ref(r) if crate::foreign::is_foreign(&r.target)))
        {
            return self.query_foreign(items, pred, order);
        }
        let store = self.store.borrow();
        let mut kept = Vec::new();
        for item in items {
            let Value::Ref(r) = &item else {
                return trap("query: the list holds something other than resources");
            };
            let holds = pred.eval(&mut |read: &crate::store::FieldRead| {
                read.lit(store.read(&r.target, &read.spec)?)
            });
            if holds.map_err(|e| e.to_string())? {
                kept.push(item);
            }
        }
        let Some(order) = order else {
            return Ok(Value::List(kept));
        };
        let levels = order.levels();
        let mut keyed = Vec::with_capacity(kept.len());
        for item in kept {
            let mut keys = Vec::with_capacity(levels.len());
            for level in &levels {
                let key = match &item {
                    Value::Ref(r) => lit_of(
                        &level.field,
                        store
                            .read(
                                &r.target,
                                &FieldSpec {
                                    id: level.field.clone(),
                                    kind: level.kind,
                                },
                            )
                            .map_err(|e| e.to_string())?,
                        &Lit::Absent,
                    )
                    .map_err(|e| e.to_string())?,
                    _ => Lit::Absent,
                };
                keys.push(key);
            }
            keyed.push((keys, item));
        }
        keyed.sort_by(|(a, _), (b, _)| ordered_cmp(order, a, b));
        Ok(Value::List(
            keyed.into_iter().map(|(_, item)| item).collect(),
        ))
    }

    /// [`Self::query_list`] over resources of an app this runtime does not run: each field the
    /// filter or the order asks is read from the resource's reading.
    fn query_foreign(
        &self,
        items: Vec<Value>,
        pred: &crate::store::Pred,
        order: Option<&crate::query::Order>,
    ) -> HResult<Value> {
        let lit = |r: &RefValue, field: &str, absent: &Lit| -> Result<Lit, String> {
            let (_, info) = self.type_of_ref(&r.target.ty)?;
            let Some(f) = info.field(field) else {
                return Err(format!("`{}` has no field `{field}`", info.name));
            };
            self.note_reading(crate::foreign::reading_key(&r.target));
            let value = self.read_value(&r.target, f, None)?;
            Ok(match crate::foreign::lit_of_value(f.kind, &value) {
                Some(Lit::Absent) | None => absent.clone(),
                Some(held) => held,
            })
        };
        let mut kept = Vec::new();
        for item in items {
            let Value::Ref(r) = &item else {
                return trap("query: the list holds something other than resources");
            };
            let holds = pred.eval(&mut |read: &crate::store::FieldRead| {
                lit(r, &read.spec.id, &read.absent).map_err(crate::store::StoreError::Invalid)
            });
            if holds.map_err(|e| e.to_string())? {
                kept.push(item);
            }
        }
        let Some(order) = order else {
            return Ok(Value::List(kept));
        };
        let levels = order.levels();
        let mut keyed = Vec::with_capacity(kept.len());
        for item in kept {
            let mut keys = Vec::with_capacity(levels.len());
            for level in &levels {
                let key = match &item {
                    Value::Ref(r) => lit(r, &level.field, &Lit::Absent)?,
                    _ => Lit::Absent,
                };
                keys.push(key);
            }
            keyed.push((keys, item));
        }
        keyed.sort_by(|(a, _), (b, _)| ordered_cmp(order, a, b));
        Ok(Value::List(
            keyed.into_iter().map(|(_, item)| item).collect(),
        ))
    }

    /// Every resource of the type `ty` the store holds that `pred` holds of, in the order
    /// `order`: outside a view, which could not follow every resource of a type.
    pub(crate) fn query_all(
        &self,
        ty: &str,
        pred: &crate::store::Pred,
        order: Option<&crate::query::Order>,
    ) -> HResult<Value> {
        let (author, in_view) = {
            let frames = self.frames.borrow();
            let top = frames.top()?;
            (top.module.clone(), frames.view().is_some())
        };
        if in_view {
            return trap("a query in a view looks inside a resource: `query(T, in: x, ...)`");
        }
        let found = self
            .store
            .borrow()
            .select(&ty.to_owned(), pred, order)
            .map_err(|e| e.to_string())?;
        Ok(Value::List(
            found
                .into_iter()
                .map(|target| {
                    Value::Ref(RefValue {
                        target: Rc::new(target),
                        author: Some(author.clone()),
                    })
                })
                .collect(),
        ))
    }

    /// `filters(ty)`: the filters of the type `ty`, each a `Filter<ty>` (key, label, icon,
    /// query, toggle); an enum's is one per case.
    pub(crate) fn filters_import(&self, ty: i32) -> HResult<Value> {
        let ty = self.text(ty)?;
        let (pkg, _) =
            self.registry.borrow().type_info(&ty).ok_or_else(|| {
                format!("filters: type `{ty}` is not declared by any loaded package")
            })?;
        let Some(declared) = pkg.meta.types.iter().find(|t| t.id == ty) else {
            return trap(format!("filters: type `{ty}` is not declared"));
        };
        let words = &self.words.borrow();
        Ok(Value::List(filters_of(
            declared,
            &pkg.meta.enums,
            &|key, source| words.word(key, &[], source),
        )))
    }

    /// `filter_lit(ty, key, value)`: one filter of a type as code names it (`Task.today`,
    /// `Task.project(p)`), the value `filters(ty)` lists it as and a query takes it: the
    /// query it asks (`is:today`, `project:<id>`), what a control shows it as, whether it is a
    /// toggle, and whether its count is one to show.
    pub(crate) fn filter_lit_import(&self, ty: i32, key: i32, value: i32) -> HResult<Value> {
        let ty = self.text(ty)?;
        let key = self.text(key)?;
        let value = self.val(value)?;
        let filter = {
            let registry = self.registry.borrow();
            let (pkg, _) = registry.type_info(&ty).ok_or_else(|| {
                format!("filter: type `{ty}` is not declared by any loaded package")
            })?;
            let Some(declared) = pkg.meta.types.iter().find(|t| t.id == ty) else {
                return trap(format!("filter: type `{ty}` is not declared"));
            };
            let Some(f) = declared.search.filters.iter().find(|f| f.key == key) else {
                let keys: Vec<&str> = declared
                    .search
                    .filters
                    .iter()
                    .map(|f| f.key.as_str())
                    .collect();
                return trap(format!(
                    "filter: `{}` declares no filter `{key}`; its filters: {}",
                    declared.name,
                    keys.join(", ")
                ));
            };
            valued_filter(&declared.id, f, &pkg.meta.enums, &value, &|key, source| {
                self.words.borrow().word(key, &[], source)
            })
            .map_err(|why| format!("filter: `{}.{key}`: {why}", declared.name))?
        };
        Ok(filter)
    }

    /// `sort_lit(ty, field)`: the order by a field of a type as code names it (`Note.title`),
    /// the value `Sort<T>` holds: what its menu entry says, and the order it asks as a query
    /// writes it (`sort:title`, `sort:-received`).
    pub(crate) fn sort_lit_import(&self, ty: i32, field: i32) -> HResult<Value> {
        let ty = self.text(ty)?;
        let field = self.text(field)?;
        let found = {
            let registry = self.registry.borrow();
            let (pkg, _) = registry.type_info(&ty).ok_or_else(|| {
                format!("sort: type `{ty}` is not declared by any loaded package")
            })?;
            let Some(declared) = pkg.meta.types.iter().find(|t| t.id == ty) else {
                return trap(format!("sort: type `{ty}` is not declared"));
            };
            let Some(f) = declared.fields.iter().find(|f| f.name == field) else {
                return trap(format!("sort: `{}` has no field `{field}`", declared.name));
            };
            let Some(query) = crate::query::sort_of(&f.name, &f.ty) else {
                return trap(format!(
                    "sort: `{}.{field}` is a `{}`, which has no order",
                    declared.name, f.ty
                ));
            };
            let label =
                self.words
                    .borrow()
                    .word(&messages::key_of(&f.id), &[], &messages::label(&f.name));
            Value::Tuple(vec![Value::Text(label), Value::Text(query)])
        };
        Ok(found)
    }

    /// The type's catalogue, as a view asks for it: `filters(T)` lists it, and `filter_lit`
    /// is one of its entries; `sort_lit` is one of the orders it offers.
    pub(crate) fn catalogue_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let found = match imp {
            Imp::Filters => self.filters_import(a.h(0)?)?,
            Imp::SortLit => self.sort_lit_import(a.h(0)?, a.h(1)?)?,
            _ => self.filter_lit_import(a.h(0)?, a.h(1)?, a.h(2)?)?,
        };
        self.ret_h(found)
    }

    /// `ident_part(id, 0)` is an identifier's scheme, `ident_part(id, 1)` its value.
    pub(crate) fn ident_part(&self, id: i32, part: i32) -> HResult<Value> {
        let held = self.text(id)?;
        let (scheme, value) = tilolang::identifier::parts(&held);
        Ok(Value::text(if part == 0 { scheme } else { value }))
    }
}

/// One entry of a type's catalogue: a filter as the value `Filter<T>` holds it — what a query
/// names it by, what a control shows it as, its icon, the query choosing it asks, and whether
/// it is a toggle.
fn filter_value(key: &str, label: &str, icon: Option<&str>, query: String, toggle: bool) -> Value {
    Value::Tuple(vec![
        Value::text(key),
        Value::text(label),
        icon.map_or_else(Value::none, |icon| Value::some(Value::text(icon))),
        Value::Text(query),
        Value::Bool(toggle),
    ])
}

/// The filters of `ty` as `Filter<T>` values: a toggle each for a Bool or a named filter, one
/// per case for an enum, one with no query for a filter whose value is picked.
///
/// A label is words the window shows, so `say` reads the message
/// [`tilolang::messages::filter_key`] names in the reader's language, the filter's own words
/// standing in when no catalogue of theirs holds it.
#[must_use]
pub fn filters_of(
    ty: &TypeMeta,
    enums: &[tilolang::meta::EnumMeta],
    say: &dyn Fn(&str, &str) -> String,
) -> Vec<Value> {
    let mut out = Vec::new();
    for f in &ty.search.filters {
        let label = say(&tilolang::messages::filter_key(&ty.id, &f.key), &f.label);
        match f.kind {
            FilterKind::Bool | FilterKind::Predicate => {
                out.push(filter_value(
                    &f.key,
                    &label,
                    f.icon.as_deref(),
                    format!("is:{}", f.key),
                    true,
                ));
            }
            FilterKind::Enum => match enum_of(f, enums) {
                Some(e) => {
                    for v in &e.variants {
                        let source = if v.label.is_empty() {
                            &v.name
                        } else {
                            &v.label
                        };
                        let label = say(&tilolang::messages::key_of(&v.id), source);
                        out.push(filter_value(
                            &f.key,
                            &label,
                            v.icon.as_deref(),
                            format!("{}:{}", f.key, v.name),
                            true,
                        ));
                    }
                }
                None => out.push(filter_value(&f.key, &label, None, String::new(), false)),
            },
            _ => out.push(filter_value(&f.key, &label, None, String::new(), false)),
        }
    }
    out
}

/// The enum a filter's field declares, when it is one of cases: `status` of `enum Status`.
fn enum_of<'a>(f: &FilterMeta, enums: &'a [EnumMeta]) -> Option<&'a EnumMeta> {
    let name =
        f.ty.trim_start_matches("Set<")
            .trim_start_matches("Option<")
            .trim_end_matches('>')
            .trim_start_matches("enum ");
    let last = name.rsplit('.').next().unwrap_or(name);
    enums.iter().find(|e| e.name == last || e.id == name)
}

/// The filter `f` with the value `v` a program names it with (`Task.project(p)`,
/// `Task.priority(.high)`): the query it asks (`project:<id>`, `priority:high`), the label a
/// control shows for it, and whether it is a toggle. A filter given a value is none: its
/// value is what it asks, so no single place is on or off.
fn valued_filter(
    ty: &str,
    f: &FilterMeta,
    enums: &[EnumMeta],
    v: &Value,
    say: &dyn Fn(&str, &str) -> String,
) -> Result<Value, String> {
    let plain = || say(&tilolang::messages::filter_key(ty, &f.key), &f.label);
    let (query, label) = match v {
        Value::Opt(None) => (format!("is:{}", f.key), plain()),
        Value::Opt(Some(v)) => {
            return valued_filter(ty, f, enums, v, say);
        }
        Value::Bool(b) => (format!("{}:{b}", f.key), plain()),
        Value::Int(n) => (format!("{}:{n}", f.key), plain()),
        Value::Float(n) => (format!("{}:{n}", f.key), plain()),
        Value::Text(t) => (format!("{}:{t}", f.key), plain()),
        Value::Enum(e) => {
            let case = enum_of(f, enums)
                .and_then(|meta| meta.variants.iter().find(|x| x.name == e.variant));
            let label = match case {
                Some(x) => {
                    let source = if x.label.is_empty() {
                        &x.name
                    } else {
                        &x.label
                    };
                    say(&tilolang::messages::key_of(&x.id), source)
                }
                None => e.variant.clone(),
            };
            (format!("{}:{}", f.key, e.variant), label)
        }
        Value::Ref(r) => (format!("{}:{}", f.key, r.target.id_string()), plain()),
        other => {
            return Err(format!(
                "it takes a value of its field's type, not {}",
                other.kind_name()
            ));
        }
    };
    let toggle = matches!(v, Value::Opt(None)) && f.kind.is_toggle();
    Ok(filter_value(
        &f.key,
        &label,
        f.icon.as_deref(),
        query,
        toggle,
    ))
}
