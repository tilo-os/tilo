//! The imports behind `query(T, ...)`, `items.matching(q)`, `filters(T)` and the parts of a
//! `std.Identifier` (see [`crate::query`]).

use std::rc::Rc;

use tilolang::meta::{FilterKind, TypeMeta};
use tilolang::pred::{CmpOp, Lit as LangLit, Operand, Pred as LangPred};

use crate::abi::Imp;
use crate::colls::field_reads;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::ids::ModuleHash;
use crate::meta::{EnumMeta, FieldMeta, FieldRole};
use crate::query::{Clock, Query, Term, find, resolve};
use crate::registry::TypeInfo;
use crate::store::{FieldSpec, Lit, lit_of, ordered_cmp};
use crate::value::{CollRef, RefValue, Value};

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
        // read of it moves on, and only then.
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

    /// The schema's catalogue of a type, as a view asks for it: `filters(T)`, or the
    /// `places(T, in: root)` a window's sidebar offers.
    pub(crate) fn catalogue_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let found = match imp {
            Imp::Filters => self.filters_import(a.h(0)?)?,
            _ => self.places_import(a.h(0)?, a.h(1)?)?,
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

/// The filters of `ty` as `host.Filter` values: a toggle each for a Bool or a named filter,
/// one per case for an enum, one with no query for a filter whose value is picked.
#[must_use]
pub fn filters_of(ty: &TypeMeta, enums: &[tilolang::meta::EnumMeta]) -> Vec<Value> {
    let filter =
        |key: &str, label: &str, icon: Option<&str>, query: String, toggle: bool, counts: bool| {
            Value::Tuple(vec![
                Value::text(key),
                Value::text(label),
                icon.map_or_else(Value::none, |icon| Value::some(Value::text(icon))),
                Value::Text(query),
                Value::Bool(toggle),
                Value::Bool(counts),
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
                    f.count,
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
                                false,
                            ));
                        }
                    }
                    None => out.push(filter(&f.key, &f.label, None, String::new(), false, false)),
                }
            }
            _ => out.push(filter(&f.key, &f.label, None, String::new(), false, false)),
        }
    }
    out
}

/// How a place is written for the app drawing it: the filters a sidebar shows a place for.
fn place_value(key: &str, label: &str, icon: Option<&str>, count: usize, counts: bool) -> Value {
    let some = |text: Option<&str>| {
        text.map_or_else(Value::none, |t| Value::some(Value::Text(t.to_owned())))
    };
    Value::Tuple(vec![
        Value::Text(key.to_owned()),
        Value::Text(label.to_owned()),
        some(icon),
        Value::Int(i64::try_from(count).unwrap_or(i64::MAX)),
        Value::Bool(counts),
    ])
}

impl Core {
    /// `places(T, in: root)`: the places a window of the root's items offers in its sidebar, as
    /// a `List<host.Place>`: the item type's filters that are on or off, in the order the type
    /// declares them, each the query `is:<key>` and how many of the items it holds. Nothing else
    /// is a place: what a window shows beside them is the app's own.
    ///
    /// A place counts the items it holds that are open, unless its filter says what is done
    /// with — the index's answer, never a scan.
    pub(crate) fn places_import(&self, ty: i32, root: i32) -> HResult<Value> {
        let ty = self.text(ty)?;
        let root = self.ref_arg(root)?;
        let (pkg, info) = self
            .registry
            .borrow()
            .type_info(&ty)
            .ok_or_else(|| format!("places: `{ty}` is no type a loaded package declares"))?;
        let Some(declared) = pkg.meta.types.iter().find(|t| t.id == ty) else {
            return trap(format!("places: `{ty}` is not declared"));
        };
        let (root_pkg, _) = self
            .registry
            .borrow()
            .type_info(&root.target.ty)
            .ok_or_else(|| {
                format!(
                    "places: the root `{}` is of `{}`, no type a loaded package declares",
                    root.target.id_string(),
                    root.target.ty
                )
            })?;
        let Some(root_declared) = root_pkg.meta.types.iter().find(|t| t.id == root.target.ty)
        else {
            return trap(format!("places: `{}` is not declared", root.target.ty));
        };
        let author = Some(self.module_of_frame()?);
        let Some((items, _)) = self.holding(&root, root_declared, &ty, author) else {
            return trap(format!(
                "places: the root `{}` holds no collection of `{}`",
                root.target.id_string(),
                declared.name
            ));
        };
        // A place counts what it leaves open: the flag saying an item is finished is what the
        // count leaves out, unless the place's own filter says what is done with, and then it
        // counts what it holds.
        let done = declared
            .fields
            .iter()
            .find(|field| field.role.as_ref().is_some_and(FieldRole::is_done));
        let places = self.filter_places(declared, &items, &info, &pkg.meta.enums, done)?;
        Ok(Value::List(places))
    }

    /// The places the type's own filters make: one for each filter that is on or off, holding
    /// the items it says, counted as what they leave open.
    fn filter_places(
        &self,
        declared: &TypeMeta,
        items: &Rc<CollRef>,
        info: &TypeInfo,
        enums: &[EnumMeta],
        done: Option<&FieldMeta>,
    ) -> HResult<Vec<Value>> {
        let mut places = Vec::new();
        for filter in &declared.search.filters {
            if !matches!(filter.kind, FilterKind::Bool | FilterKind::Predicate) {
                continue;
            }
            let query = format!("is:{}", filter.key);
            let count = self.place_count(items, declared, info, enums, &query, done)?;
            places.push(place_value(
                &query,
                &filter.label,
                filter.icon.as_deref(),
                count,
                filter.count,
            ));
        }
        Ok(places)
    }

    /// The root's collection of the type `ty`, with the root's field holding it, when the root
    /// declares one.
    fn holding(
        &self,
        at: &RefValue,
        holder: &TypeMeta,
        ty: &str,
        author: Option<ModuleHash>,
    ) -> Option<(Rc<CollRef>, FieldMeta)> {
        for field in &holder.fields {
            let Ok((_, f)) = self.lookup_field(&at.target, &field.id) else {
                continue;
            };
            let Ok(item) = self.item_type(&f.ty) else {
                continue;
            };
            if item.id != ty {
                continue;
            }
            let held = Rc::new(CollRef {
                target: Rc::clone(&at.target),
                field: f.id,
                item: item.id.clone(),
                author,
                filter: None,
                order: None,
            });
            return Some((held, field.clone()));
        }
        None
    }

    /// How many items a place holds: the items of the collection `items` whose query `query`
    /// holds of, with the items the type's `done` flag marks finished left out unless the query
    /// says what is done with, counted from the index's columns.
    ///
    /// # Errors
    ///
    /// A message when the query is not one the type's filters read.
    fn place_count(
        &self,
        items: &Rc<CollRef>,
        declared: &TypeMeta,
        info: &TypeInfo,
        enums: &[EnumMeta],
        query: &str,
        done: Option<&FieldMeta>,
    ) -> HResult<usize> {
        let clock = self.query_clock()?;
        let viewer = self.looking().ok();
        let parsed = find(query).map_err(|why| format!("places: `{query}`: {why}"))?;
        let resolved = resolve(&parsed, declared, enums, &clock, viewer.as_deref())?;
        self.note_time(clock.changes_at());
        let mut pred = resolved.pred;
        // A place counts what it leaves open, never what is done with; a filter that names the
        // flag itself already says what it leaves open.
        if let Some(done) = done
            && !pred.fields().iter().any(|held| **held == done.id)
        {
            pred = LangPred::And(vec![
                pred,
                LangPred::Cmp(
                    CmpOp::Eq,
                    Operand::Field(done.id.clone()),
                    Operand::Lit(LangLit::Bool(false)),
                ),
            ]);
        }
        let Some(read) = field_reads(&pred, info) else {
            return trap(format!(
                "places: `{query}` cannot be read as a filter of `{}`",
                declared.name
            ));
        };
        self.coll_count(&Rc::new(items.queried(read, None)))
    }
}
