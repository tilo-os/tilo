//! What a reader outside tilolang reads of a resource beyond its fields: who made it, when, and
//! what from (`created(r)?.who`, `.at`, `.from`), and a page of a collection's items that match
//! fields of theirs, or that were made from a resource.
//!
//! Both are answered from the store's index: a resource's first version is kept per resource, a
//! match on a collection's items from the columns of their type, and what was made from a
//! resource from the reverse list of what links to it. A page of a collection of ten thousand
//! costs the page, not the collection.

use std::rc::Rc;

use serde_json::Value as Json;
use tilolang::pred::{CmpOp, Lit, Operand};

use crate::error::RuntimeError;
use crate::ids::Actor;
use crate::meta::{FieldKind, Ty};
use crate::registry::{FieldInfo, TypeInfo};
use crate::runtime::Runtime;
use crate::store::{FieldRead, FieldSpec, LinkHow, Listing, Pred, Store, StoreError};
use crate::value::ResourceRef;

/// Who made a resource, when, and what it was made from: its first version.
#[derive(Clone, Debug, PartialEq)]
pub struct Created {
    /// Who made it: the automation, for a Shortcut's run.
    pub who: Actor,
    /// Whose it is to a check of who may change it: the person a Shortcut's run made it for,
    /// else whoever made it.
    pub owner: Actor,
    /// ISO-8601 UTC.
    pub at: String,
    pub from: Option<ResourceRef>,
}

/// A page of a collection to read ([`Runtime::page`]).
#[derive(Clone, Debug)]
pub struct PageAsk<'a> {
    /// The resource the collection is a field of.
    pub at: &'a ResourceRef,
    /// The collection field, by name or id.
    pub field: &'a str,
    /// Fields of the items, by name or id, each with the value it must equal, as JSON writes it:
    /// a reference as the id of the resource it names.
    pub matching: &'a [(String, Json)],
    /// Only the items made from this resource.
    pub from: Option<&'a ResourceRef>,
    /// The page starts after this item (its id in the collection).
    pub after: Option<&'a str>,
    pub limit: usize,
}

/// One page of a collection, in its order.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub items: Vec<ResourceRef>,
    /// The id of the last item on the page, when more items follow it: where the next page
    /// starts.
    pub next: Option<String>,
}

impl Runtime {
    /// Who made `at`, when, and what from: its first version in the log, `None` while it has none
    /// (an unmade root).
    ///
    /// # Errors
    ///
    /// A store that cannot read the resource's log.
    pub fn created(&self, at: &ResourceRef) -> Result<Option<Created>, RuntimeError> {
        let first = self.core.store.borrow().version_end(at, false)?;
        Ok(first.map(|version| Created {
            who: self.core.made_by(&version, at),
            owner: self.core.owner(&version, at),
            from: version.from.clone(),
            at: version.at,
        }))
    }

    /// The items of the collection `ask.field` of `ask.at` that match `ask.matching` and were made
    /// from `ask.from`, of those `keep` keeps (the ones the reader may read), from after
    /// `ask.after`, at most `ask.limit`. An item `keep` drops is not on the page and does not
    /// count against its size.
    ///
    /// # Errors
    ///
    /// `NotFound` for a field the type lacks or an `after` the matching items do not hold;
    /// `BadArgs` for a field that is no collection, a value that does not fit the field it is
    /// matched against, or a match on the fields of a collection of several types.
    pub fn page(
        &self,
        ask: &PageAsk<'_>,
        keep: &dyn Fn(&ResourceRef) -> bool,
    ) -> Result<Page, RuntimeError> {
        let (_, info) = self
            .core
            .type_of_ref(&ask.at.ty)
            .map_err(RuntimeError::NotFound)?;
        let field = named_field(&info, ask.field)?;
        if field.kind != FieldKind::Collection {
            return Err(RuntimeError::BadArgs(format!(
                "`{}` of `{}` is not a collection",
                field.name, info.name
            )));
        }
        // A collection of a union's types names no one item type: each item's is its own.
        let item = match &field.ty {
            Ty::Collection(inner) | Ty::List(inner) if matches!(**inner, Ty::Union(_)) => None,
            ty => Some(self.core.item_type(ty).map_err(RuntimeError::NotFound)?),
        };
        let pred = matching(item.as_deref(), &field.name, ask.matching)?;
        let store = self.core.store.borrow();
        let collection = Collection {
            store: &**store,
            at: ask.at,
            spec: FieldSpec {
                id: field.id.clone(),
                kind: FieldKind::Collection,
            },
            item: item.as_deref(),
        };
        let listed = match ask.from {
            // What was made from `from` is read from what links to it, which holds as many as
            // were made from it, placed in the collection's order.
            Some(from) => collection.made_from(from, pred.as_ref())?,
            None => collection.listing(pred.as_ref())?,
        };
        let start = match ask.after {
            None => 0,
            Some(after) => match listed.position(after) {
                Some(Some(index)) => index.saturating_add(1),
                _ => {
                    return Err(RuntimeError::NotFound(format!(
                        "`{after}` is not an item of `{}` that matches",
                        field.name
                    )));
                }
            },
        };
        let mut items = Vec::new();
        let mut next = None;
        for id in (start..listed.len()).map_while(|index| listed.get(index)) {
            let target = collection.item_ref(&id)?;
            if !keep(&target) {
                continue;
            }
            if items.len() == ask.limit {
                next = items
                    .last()
                    .and_then(|last: &ResourceRef| last.item.clone());
                break;
            }
            items.push(target);
        }
        Ok(Page { items, next })
    }
}

/// The field `name` (or id) of `info`.
fn named_field<'a>(info: &'a TypeInfo, name: &str) -> Result<&'a FieldInfo, RuntimeError> {
    info.field_by_id_or_name(name).ok_or_else(|| {
        RuntimeError::NotFound(format!(
            "field `{name}` of `{}`; fields are: {}",
            info.name,
            info.field_names()
        ))
    })
}

/// What the items of a collection `of`, of the type `item`, must hold to match `matching`.
fn matching(
    item: Option<&TypeInfo>,
    of: &str,
    matching: &[(String, Json)],
) -> Result<Option<Pred>, RuntimeError> {
    let mut reads = Vec::new();
    for (name, value) in matching {
        let item = item.ok_or_else(|| {
            RuntimeError::BadArgs(format!(
                "the items of `{of}` are of several types: match them by `from` alone"
            ))
        })?;
        let field = named_field(item, name)?;
        let read = FieldRead::of(&field.id, field.kind, &field.ty, field.default.as_ref())
            .ok_or_else(|| RuntimeError::BadArgs(format!("`{name}` is not matched by value")))?;
        let lit = lit_of(&field.ty, value).ok_or_else(|| {
            RuntimeError::BadArgs(format!("`{value}` is not a value of `{name}`"))
        })?;
        reads.push(Pred::Cmp(
            CmpOp::Eq,
            Operand::Field(read),
            Operand::Lit(lit),
        ));
    }
    Ok(match reads.len() {
        0 => None,
        1 => reads.pop(),
        _ => Some(Pred::And(reads)),
    })
}

/// One collection of a store, as a page reads it.
struct Collection<'a> {
    store: &'a dyn Store,
    at: &'a ResourceRef,
    spec: FieldSpec,
    item: Option<&'a TypeInfo>,
}

impl Collection<'_> {
    /// Its items `pred` holds of, from the store's index: all of them for none.
    fn listing(&self, pred: Option<&Pred>) -> Result<Rc<Listing>, StoreError> {
        let listed = match (pred, self.item) {
            (Some(pred), Some(item)) => self
                .store
                .listing_where(self.at, &self.spec, &item.id, pred),
            _ => self.store.listing(self.at, &self.spec),
        };
        match listed {
            Err(StoreError::NotFound(_)) if self.at.is_unmade_root() => {
                Ok(Rc::new(Listing::from_ids(Vec::new())))
            }
            other => other,
        }
    }

    /// Its items made from `from` that `pred` holds of, in its order.
    fn made_from(
        &self,
        from: &ResourceRef,
        pred: Option<&Pred>,
    ) -> Result<Rc<Listing>, StoreError> {
        let all = self.listing(None)?;
        let at = self.at.id_string();
        let mut placed = Vec::new();
        for link in self.store.linked(from)? {
            let Some(id) = link
                .of
                .item
                .as_deref()
                .filter(|_| link.how == LinkHow::CreatedFrom)
            else {
                continue;
            };
            let held = self.store.container(&link.of)?;
            if !held.is_some_and(|(holder, by)| by == self.spec.id && holder.id_string() == at) {
                continue;
            }
            if let Some(pred) = pred {
                let target = self.item_ref(id)?;
                if !pred.eval(&mut |read: &FieldRead| self.store.field_lit(&target, read))? {
                    continue;
                }
            }
            if let Some(Some(index)) = all.position(id) {
                placed.push((index, Rc::<str>::from(id)));
            }
        }
        placed.sort_unstable_by_key(|(index, _)| *index);
        Ok(Rc::new(Listing::from_items(
            placed.into_iter().map(|(_, id)| id).collect(),
        )))
    }

    /// The item `id` of it, with its type.
    fn item_ref(&self, id: &str) -> Result<ResourceRef, StoreError> {
        if let Some(item) = self.item {
            return Ok(self.at.item(id, item.id.clone()));
        }
        let probe = self.at.item(id, String::new());
        let ty = self.store.type_of(&probe)?;
        Ok(ResourceRef { ty, ..probe })
    }
}

/// `value` as a filter compares it with a field of the type `ty`: a reference by the id of the
/// resource it names, a case of an enum by its name, an option's none as `null`.
fn lit_of(ty: &Ty, value: &Json) -> Option<Lit> {
    match (ty, value) {
        (Ty::Option(_), Json::Null) => Some(Lit::Absent),
        (Ty::Option(inner), _) => lit_of(inner, value),
        (Ty::Record(_) | Ty::Union(_), Json::String(id)) => Some(Lit::Ref(id.clone())),
        (Ty::Enum(_), Json::String(case)) => Some(Lit::Variant(case.clone())),
        _ => Lit::of_json(value),
    }
}
