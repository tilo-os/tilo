//! The imports behind `query(T, ...)`, `items.matching(q)`, `filters(T)` and the parts of a
//! `std.Identifier` (see [`crate::query`]).

use std::rc::Rc;

use tilolang::meta::{FilterKind, TypeMeta};

use crate::core::{Core, HResult, trap};
use crate::handles::FrameKind;
use crate::query::{Clock, Query, Term, find, resolve};
use crate::store::{FieldSpec, Lit, lit_of, order_cmp};
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
        let resolved = resolve(&query, declared, &pkg.meta.enums, &clock)?;
        // A view that asked by the time (`is:today`, `received:-7d`) runs again when what it
        // read of it moves on, and only then.
        self.note_time(clock.changes_at())?;
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
        let spec = FieldSpec {
            id: order.field.clone(),
            kind: order.kind,
        };
        let mut keyed = Vec::with_capacity(kept.len());
        for item in kept {
            let key = match &item {
                Value::Ref(r) => lit_of(
                    &spec.id,
                    store.read(&r.target, &spec).map_err(|e| e.to_string())?,
                    &Lit::Absent,
                )
                .map_err(|e| e.to_string())?,
                _ => Lit::Absent,
            };
            keyed.push((key, item));
        }
        keyed.sort_by(|(a, _), (b, _)| order_cmp(a, b));
        if order.descending {
            keyed.reverse();
        }
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
        let author = {
            let frames = self.frames.borrow();
            let top = frames.top()?;
            if matches!(top.kind, FrameKind::View(_)) {
                return trap("a query in a view looks inside a resource: `query(T, in: x, ...)`");
            }
            top.module.clone()
        };
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

    /// `filters(ty)`: the filters of the type `ty`, each a `host.Filter` (key, label, icon,
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
        Ok(Value::List(filters_of(declared, &pkg.meta.enums)))
    }

    /// `ident_part(id, 0)` is an identifier's scheme, `ident_part(id, 1)` its value.
    pub(crate) fn ident_part(&self, id: i32, part: i32) -> HResult<Value> {
        let held = self.text(id)?;
        let (scheme, value) = tilolang::identifier::parts(&held);
        Ok(Value::text(if part == 0 { scheme } else { value }))
    }
}

/// The filters of `ty` as `host.Filter` values: a toggle each for a Bool or a named filter,
/// one per case for an enum, one with no query for a filter whose value is picked.
#[must_use]
pub fn filters_of(ty: &TypeMeta, enums: &[tilolang::meta::EnumMeta]) -> Vec<Value> {
    let filter = |key: &str, label: &str, icon: Option<&str>, query: String, toggle: bool| {
        Value::Tuple(vec![
            Value::text(key),
            Value::text(label),
            icon.map_or_else(Value::none, |icon| Value::some(Value::text(icon))),
            Value::Text(query),
            Value::Bool(toggle),
        ])
    };
    let mut out = Vec::new();
    for f in &ty.search.filters {
        match f.kind {
            FilterKind::Bool | FilterKind::Predicate => {
                out.push(filter(
                    &f.key,
                    &f.label,
                    f.icon.as_deref(),
                    format!("is:{}", f.key),
                    true,
                ));
            }
            FilterKind::Enum => {
                let name =
                    f.ty.trim_start_matches("Set<")
                        .trim_start_matches("Option<")
                        .trim_end_matches('>')
                        .trim_start_matches("enum ");
                let last = name.rsplit('.').next().unwrap_or(name);
                match enums.iter().find(|e| e.name == last || e.id == name) {
                    Some(e) => {
                        for v in &e.variants {
                            let label = if v.label.is_empty() {
                                &v.name
                            } else {
                                &v.label
                            };
                            out.push(filter(
                                &f.key,
                                label,
                                v.icon.as_deref(),
                                format!("{}:{}", f.key, v.name),
                                true,
                            ));
                        }
                    }
                    None => out.push(filter(&f.key, &f.label, None, String::new(), false)),
                }
            }
            _ => out.push(filter(&f.key, &f.label, None, String::new(), false)),
        }
    }
    out
}
