//! Admitting a peer's change: what it touches, whether the declared types allow it, and whether
//! its author may make it.
//!
//! Every change names the action that made it ([`Stamp`]): the verb, who invoked it, the
//! invocation, and the membership its author saw. A holder reads, before applying a change,
//! which resources, fields and placements it touches ([`collab_types::touch::Touched`]), from its operations against
//! the document as it stands, without applying it. The store checks what the declared types can
//! say (a field the type declares, a value of its type, a case of its enum, a kind nested where
//! its parent's type nests it, and what the verb declares it writes); the caller's [`Admit`]
//! decides whether the author may invoke the verb on those resources in that reader set,
//! against the membership the stamp names. Every holder reaches the same
//! answer from the same change, so a change that is refused is kept beside the document but has
//! no effect on it, on any holder; a change built on a refused one is refused too.

use std::collections::BTreeSet;

use automerge::{Change as AmChange, ScalarValue};
use collab_types::touch::{Touch, Touched};

use crate::schema::{Expected, Schema};

pub use collab_types::stamp::{Group, Origin, Stamp};

/// A peer's change as the store asks about it.
#[derive(Debug)]
pub struct Proposal<'a> {
    pub doc: &'a str,
    pub readers: &'a str,
    /// The device that signed the change.
    pub author: &'a [u8; 32],
    /// The action it names; `None` for a change that names none.
    pub stamp: Option<&'a Stamp>,
    pub touched: &'a Touched,
}

/// The one permission check, as a holder asks it of each change it receives: whether the
/// change's author may invoke its verb on the resources it touches in the document's reader set,
/// as of the membership its stamp names. It must answer alike on every holder. That what the
/// change touches lies within what its verb declares it writes the store checks itself, from
/// the declared schema ([`crate::Schema::verbs`]).
pub trait Admit {
    fn admit(&self, proposal: &Proposal<'_>) -> bool;
}

impl<F: Fn(&Proposal<'_>) -> bool> Admit for F {
    fn admit(&self, proposal: &Proposal<'_>) -> bool {
        self(proposal)
    }
}

/// A change received from a peer, with the device that signed it.
#[derive(Clone, Debug)]
pub struct Received {
    pub change: AmChange,
    pub author: [u8; 32],
    /// The kinds of the records the change writes, as the sealed op that carried it claims: what
    /// a member without the document's key decided it by. The store refuses the change unless
    /// they are the kinds it reads ([`collab_types::claim::store_record_kinds`]). `None` for a
    /// change that came in the clear, which its receiver read for itself.
    pub claimed: Option<BTreeSet<String>>,
}

/// Why a change was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It is built on a change that was refused.
    OnRefused,
    /// It writes where the store writes nothing, or into an object it cannot place.
    Unplaced,
    /// The declared types do not allow it.
    Undeclared(String),
    /// Its author may not make it.
    NotPermitted,
    /// It writes records of other kinds than the op that carried it claims.
    Misclaimed(String),
}

/// Whether the kinds a sealed op claims its change writes, `claimed`, are those its holder reads,
/// `read`: the same on every holder, which reads them at the change's earlier changes.
pub(crate) fn as_claimed(
    read: &BTreeSet<String>,
    claimed: Option<&BTreeSet<String>>,
) -> Result<(), String> {
    match claimed {
        Some(claimed) if claimed != read => Err(format!(
            "it writes {} but claims {}",
            listed(read),
            listed(claimed)
        )),
        _ => Ok(()),
    }
}

fn listed(kinds: &BTreeSet<String>) -> String {
    if kinds.is_empty() {
        return "no kind".to_owned();
    }
    kinds
        .iter()
        .map(|kind| format!("`{kind}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether the declared types allow what a change touches and writes: every resource of a
/// declared kind, every field written one its kind declares, holding a value of its type.
pub(crate) fn declared(
    schema: &Schema,
    touched: &Touched,
    writes: &[(String, String, Option<ScalarValue>)],
    kind_of: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    if schema.types.is_empty() {
        return Ok(());
    }
    // A resource is nested only where its parent's type nests its kind.
    for touch in touched.resources.values() {
        if let (Some(kind), Some(parent)) = (&touch.kind, &touch.parent) {
            let parent_kind = touched
                .resources
                .get(parent)
                .and_then(|touch| touch.kind.clone())
                .or_else(|| kind_of(parent));
            if let Some(parent_kind) = parent_kind
                && schema
                    .get(&parent_kind)
                    .is_some_and(|ty| !ty.nested.iter().any(|(_, nested)| nested == kind))
            {
                return Err(format!("`{parent_kind}` nests no `{kind}`"));
            }
        }
    }
    for (id, touch) in &touched.resources {
        if touch.removed {
            continue;
        }
        let Some(kind) = &touch.kind else {
            return Err(format!("`{id}` is of no kind"));
        };
        if let Some(missing) = std::iter::once(kind)
            .chain(&touch.retyped_from)
            .find(|kind| schema.get(kind).is_none())
        {
            return Err(format!("no type `{missing}` is declared"));
        }
    }
    for (id, field, value) in writes {
        // The platform's own fields, which no declared type has: what a resource stands in, and
        // the times and fields a version reflects (`$created`, `$updated`, `$changed.<field>`).
        if field.starts_with('$') {
            continue;
        }
        let Some(kind) = touched
            .resources
            .get(id)
            .and_then(|touch| touch.kind.as_ref())
        else {
            continue;
        };
        let Some(declared) = schema.get(kind).and_then(|ty| ty.field(field)) else {
            return Err(format!("`{kind}` declares no field `{field}`"));
        };
        if let Some(value) = value
            && !declared.value.holds(value, declared.optional)
        {
            return Err(format!("`{kind}.{field}` holds no {value:?}"));
        }
    }
    Ok(())
}

/// Whether what `touched` writes lies within what the verb `stamp` names declares it writes, the
/// change making `operations` operations. A schema that declares no verbs checks none but the
/// verbs of history every schema has; one that does refuses a change that names no verb.
pub(crate) fn within_verb(
    schema: &Schema,
    touched: &Touched,
    stamp: Option<&Stamp>,
    operations: usize,
) -> Result<(), String> {
    let verbs: Vec<&crate::schema::Verb> = stamp
        .map(|stamp| schema.verbs_naming(&stamp.verb).collect())
        .unwrap_or_default();
    if let (Some(stamp), true) = (stamp, verbs.iter().any(|verb| verb.marks)) {
        return marks(stamp, operations);
    }
    if schema.verbs.is_empty() {
        return Ok(());
    }
    let Some(stamp) = stamp else {
        return Err("the change names no verb".to_owned());
    };
    if verbs.is_empty() {
        return Err(format!("no verb `{}` is declared", stamp.verb));
    }
    for (id, touch) in &touched.resources {
        let Some(kind) = &touch.kind else {
            continue;
        };
        // One verb of the name that admits the resource's write is enough: a family declared
        // by prefix writes its own kinds beside a verb of the same name another app declares.
        if !verbs
            .iter()
            .any(|verb| verb_admits(schema, verb, id, kind, touch).is_ok())
        {
            return verbs
                .iter()
                .try_for_each(|verb| verb_admits(schema, verb, id, kind, touch));
        }
    }
    Ok(())
}

/// Whether a change of a verb of history, `stamp`, making `operations` operations, writes
/// nothing and says it in bounded arguments.
fn marks(stamp: &Stamp, operations: usize) -> Result<(), String> {
    if operations > 0 {
        return Err(format!(
            "`{}` writes nothing, and this change does",
            stamp.verb
        ));
    }
    let args = serde_json::to_vec(&stamp.args).map_or(usize::MAX, |args| args.len());
    if args > crate::schema::MAX_MARK_ARGS {
        return Err(format!(
            "`{}` says at most {} bytes, and this change says {args}",
            stamp.verb,
            crate::schema::MAX_MARK_ARGS
        ));
    }
    Ok(())
}

/// Whether `verb` admits what `touch` does to the resource `id` of `kind`.
fn verb_admits(
    schema: &Schema,
    verb: &crate::schema::Verb,
    id: &str,
    kind: &str,
    touch: &Touch,
) -> Result<(), String> {
    // A move founds, fills and removes a resource of whatever kind the store declares: whether its
    // caller may is the workspace's to decide, on each side of it.
    if verb.moves {
        return schema
            .get(kind)
            .map(drop)
            .ok_or_else(|| format!("no type `{kind}` is declared"));
    }
    // A creation founds a resource of a declared kind and writes nothing in it.
    if verb.creates {
        schema
            .get(kind)
            .ok_or_else(|| format!("no type `{kind}` is declared"))?;
        let writes = touch.removed
            || touch.content
            || touch.retyped_from.is_some()
            || touch.fields.iter().any(|field| !field.starts_with('$'));
        return if touch.made && !writes {
            Ok(())
        } else {
            Err(format!("`{}` only founds an empty `{id}`", verb.name))
        };
    }
    let Some(writes) = verb.writes.get(kind) else {
        // A record's verb writes the kinds its type nests that another surface declares, and
        // their fields are the ones that surface declares: a tilolang package publishes a
        // surface per type, so a list's items are another surface's kind.
        if nested_kind(schema, &verb.nested, kind) {
            return Ok(());
        }
        // A tilolang action's code writes the records of its own package it reaches, as an
        // answer settles the ask it answers: the package's code bounds what its verb writes
        // there, and a surface lists no write a reference leads to. Another package's
        // records stay out of its reach.
        if let Some(package) = tilolang_package(kind)
            && verb.writes.keys().any(|own| {
                tilolang_package(own) == Some(package) || own.strip_prefix("tl.") == Some(package)
            })
        {
            return Ok(());
        }
        // A notice is how any action's code tells a person something: a tilolang verb writes
        // one wherever it writes its own package's records.
        if kind == HOST_NOTICE_KIND && verb.writes.keys().any(|own| own.starts_with("tl.")) {
            return Ok(());
        }
        return Err(format!("`{}` writes no `{kind}`", verb.name));
    };
    if writes.sets_only
        && (touch.made
            || touch.removed
            || touch.placed
            || touch.content
            || touch.retyped_from.is_some())
    {
        return Err(format!("`{}` only sets fields of `{id}`", verb.name));
    }
    // A record given another kind leaves its old one: the verb must write that kind too.
    if let Some(was) = &touch.retyped_from {
        match verb.writes.get(was) {
            Some(old) if !old.sets_only => {}
            Some(_) => return Err(format!("`{}` only sets fields of `{was}`", verb.name)),
            None if nested_kind(schema, &verb.nested, was) => {}
            None => return Err(format!("`{}` writes no `{was}`", verb.name)),
        }
    }
    if let Some(field) = touch
        .fields
        .iter()
        .filter(|field| !field.starts_with('$'))
        .find(|field| !writes.fields.contains(*field))
    {
        return Err(format!("`{}` writes no `{kind}.{field}`", verb.name));
    }
    Ok(())
}

/// Whether `kind` stands among `nested` or among the kinds the types between them nest.
/// The tilolang package a kind of one (`tl.<app>.<Type>`) is of: none for another kind.
/// The kind of the tilolang host's `Notice`, which any package's action writes to tell a person.
const HOST_NOTICE_KIND: &str = "tl.tilo.host.Notice";

fn tilolang_package(kind: &str) -> Option<&str> {
    let (package, _) = kind.strip_prefix("tl.")?.rsplit_once('.')?;
    Some(package)
}

fn nested_kind(schema: &Schema, nested: &BTreeSet<String>, kind: &str) -> bool {
    let mut reached: Vec<&str> = nested.iter().map(String::as_str).collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    while let Some(at) = reached.pop() {
        if at == kind {
            return true;
        }
        if !seen.insert(at) {
            continue;
        }
        if let Some(ty) = schema.get(at) {
            reached.extend(ty.nested.iter().map(|(_, nested)| nested.as_str()));
        }
    }
    false
}

impl Expected {
    /// Whether `value` is one this field may hold.
    fn holds(&self, value: &ScalarValue, optional: bool) -> bool {
        match (self, value) {
            (_, ScalarValue::Null) => optional,
            (Self::Any, _)
            | (Self::Text, ScalarValue::Str(_))
            | (Self::Int, ScalarValue::Int(_) | ScalarValue::Uint(_))
            | (Self::Float, ScalarValue::F64(_) | ScalarValue::Int(_))
            | (Self::Bool, ScalarValue::Boolean(_)) => true,
            (Self::OneOf(cases), ScalarValue::Str(case)) => {
                cases.iter().any(|held| held.as_str() == case.as_str())
            }
            _ => false,
        }
    }
}
