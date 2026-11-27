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

use std::collections::{BTreeMap, BTreeSet};

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
    /// The call it names by digest is not kept by its document, or is not the one it keeps.
    Uncalled(tilo_actions::stamp::CallUnresolved),
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
    ruled: &dyn Fn(&str, &str) -> bool,
) -> Result<(), String> {
    if schema.types.is_empty() {
        return Ok(());
    }
    // A resource is nested only where its parent's type nests its kind, unless its own type says
    // it is read by a document other than its parent's: such a one is placed in a document of
    // those readers, never in its parent's, so what nests it is not its kind's to hold. A rule
    // that stays its parent's readers (an `until` that has come true reads as every member)
    // does not lift this.
    for (id, touch) in &touched.resources {
        if let (Some(kind), Some(parent)) = (&touch.kind, &touch.parent) {
            if ruled(kind, id) {
                continue;
            }
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

/// Whether the change's stamp names an app, and the change writes only kinds that app declares:
/// an app writes its own kinds and never another app's, whoever carries the change, so every
/// holder reaches the same refusal. A change no app made (a person's, an agent's, one the
/// platform wrote) is no app's to own, and a kind the schema knows no owner of is not judged.
pub(crate) fn owned_by_its_app(
    schema: &Schema,
    touched: &Touched,
    stamp: Option<&Stamp>,
) -> Result<(), String> {
    let Some(app) = stamp
        .and_then(Stamp::invoked_by)
        .and_then(|actor| actor.app().cloned())
    else {
        return Ok(());
    };
    for touch in touched.resources.values() {
        let Some(kind) = &touch.kind else {
            continue;
        };
        if let Some(owners) = schema.owners.get(kind)
            && !owners.is_empty()
            && !owners.contains(app.as_str())
        {
            return Err(format!(
                "`{app}` writes `{kind}`, which {} declares",
                owners.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
    }
    Ok(())
}

/// Whether what `touched` writes lies within what the verbs `stamp` names declare they write, the
/// change making `operations` operations. A change names one verb, or the several actions of a
/// group its entry says it was one step of, each of which must be declared and one of which
/// admits the resource's write. A schema that declares no verbs checks none but the verbs of
/// history every schema has; one that does refuses a change that names no verb.
pub(crate) fn within_verb(
    schema: &Schema,
    touched: &Touched,
    writes: &[(String, String, Option<ScalarValue>)],
    stamp: Option<&Stamp>,
    operations: usize,
    queued_by: &BTreeMap<String, String>,
) -> Result<(), String> {
    let names = stamp.map(named_verbs).unwrap_or_default();
    let verbs: Vec<&crate::schema::Verb> = names
        .iter()
        .flat_map(|name| schema.verbs_naming(name))
        .collect();
    // A verb that marks a step writes nothing: a change it alone names is checked for that
    // alone. A group of actions is no mark's, whatever one of its members says.
    if let Some(stamp) = stamp
        && names.len() == 1
        && verbs.iter().any(|verb| verb.marks)
    {
        return marks(stamp, operations);
    }
    if schema.verbs.is_empty() {
        return Ok(());
    }
    let Some(stamp) = stamp else {
        return Err("the change names no verb".to_owned());
    };
    if names.is_empty() {
        return Err(format!("no verb `{}` is declared", stamp.verb));
    }
    // Every action the change names is declared, whichever of them admits the write: a group
    // may not name one the store's schema knows nothing of.
    if let Some(undeclared) = names
        .iter()
        .find(|name| schema.verbs_naming(name).next().is_none())
    {
        return Err(format!("no verb `{undeclared}` is declared"));
    }
    // A branch's founding step is one record's: a change that touches another record is no step
    // of a branch's.
    if let Some(branch) = names
        .iter()
        .find(|name| schema.verbs_naming(name).any(|verb| verb.branches))
        && touched.resources.len() > 1
    {
        return Err(format!(
            "`{branch}` is one record's step, and this change touches {}",
            touched.resources.len()
        ));
    }
    for (id, touch) in &touched.resources {
        let Some(kind) = &touch.kind else {
            continue;
        };
        // One verb of the names that admits the resource's write is enough: a family declared
        // by prefix writes its own kinds beside a verb of the same name another app declares.
        if !verbs.iter().any(|verb| {
            verb_admits(schema, verb, stamp, id, kind, touch, writes, queued_by).is_ok()
        }) {
            return verbs.iter().try_for_each(|verb| {
                verb_admits(schema, verb, stamp, id, kind, touch, writes, queued_by)
            });
        }
    }
    Ok(())
}

/// The verbs `stamp` names: its own, every distinct action of a group its entry says the change
/// was one step of, and, for a step through history, the platform's verb for that step (a store
/// stamp names the same one as `$undo` or `$restore`). Empty names are left out, and a name two
/// of those say is said once.
fn named_verbs(stamp: &Stamp) -> Vec<&str> {
    use tilo_actions::{Did, HistoryOp};
    let mut names = vec![stamp.verb.as_str()];
    if let Some(context) = stamp.context.as_deref() {
        match context.what.as_ref() {
            Some(Did::Group(group)) => {
                let members = if group.distinct.is_empty() {
                    &group.members
                } else {
                    &group.distinct
                };
                for member in members {
                    if let Did::Verb(verb) = member {
                        names.push(verb.action.action.as_str());
                    }
                }
            }
            Some(Did::History(step)) => names.push(match step.op {
                HistoryOp::Undo | HistoryOp::Redo => Stamp::UNDO_VERB,
                HistoryOp::Restore => Stamp::RESTORE_VERB,
                HistoryOp::Checkpoint { .. } => Stamp::CHECKPOINT_VERB,
                HistoryOp::Migrate { .. } => Stamp::MIGRATE_VERB,
            }),
            _ => {}
        }
    }
    names.retain(|name| !name.is_empty());
    names.sort_unstable();
    names.dedup();
    names
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

/// Whether the origin's write, `verb`, admits a write of a resource of `kind`: any field or
/// content of a kind an origin declares, as the move places one of any kind. The platform's own
/// write alone makes it: a change whose entry names what it did is an action's, never the
/// origin's, and a kind no outside service keeps in step is no origin's to write.
fn origin_admits(
    schema: &Schema,
    verb: &crate::schema::Verb,
    stamp: &Stamp,
    kind: &str,
) -> Result<(), String> {
    let ty = schema
        .get(kind)
        .ok_or_else(|| format!("no type `{kind}` is declared"))?;
    if ty.origin.is_none() {
        return Err(format!(
            "`{kind}` is no origin's kind, so `{}` does not write it",
            verb.name
        ));
    }
    if stamp
        .context
        .as_deref()
        .and_then(|context| context.what.as_ref())
        .is_some()
    {
        return Err(format!(
            "`{}` is the platform's write, and no action makes it",
            verb.name
        ));
    }
    Ok(())
}

/// Whether `verb` admits what `touch` does to the resource `id` of `kind`, by the step `stamp`
/// names.
#[expect(
    clippy::too_many_lines,
    reason = "each kind of verb a store declares and what it admits, told together"
)]
fn verb_admits(
    schema: &Schema,
    verb: &crate::schema::Verb,
    stamp: &Stamp,
    id: &str,
    kind: &str,
    touch: &Touch,
    writes: &[(String, String, Option<ScalarValue>)],
    queued_by: &BTreeMap<String, String>,
) -> Result<(), String> {
    // A move founds, fills and removes a resource of whatever kind the store declares, and a verb
    // the platform writes under (an invocation, a settling block, a step taken back or brought
    // back) writes any field and content of a declared kind: whether
    // the caller may is the workspace's to decide, and the kind is the store's.
    if verb.moves || verb.platform {
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
    // A branch carries one record across with its history, and admits only the change its own
    // writer makes: the entry naming the branch verb. A change that merely names the verb is no
    // step of a branch's.
    if verb.branches {
        schema
            .get(kind)
            .ok_or_else(|| format!("no type `{kind}` is declared"))?;
        let what = stamp
            .context
            .as_deref()
            .and_then(|context| context.what.as_ref());
        if !matches!(
            what,
            Some(tilo_actions::Did::Verb(call)) if call.action.action == verb.name
        ) {
            return Err(format!(
                "`{}` only admits the step its writer makes on `{id}`",
                verb.name
            ));
        }
        // The step itself writes the record's content or founds the record it carried across,
        // and nothing the store keeps of it: no kind, place, field or removal. The platform's
        // own fields are no write of an app's.
        let wrote = touch.removed
            || touch.placed
            || touch.retyped_from.is_some()
            || touch.fields.iter().any(|field| !field.starts_with('$'));
        return if !wrote && (touch.content || touch.made) {
            Ok(())
        } else {
            Err(format!("`{}` only carries `{id}` across", verb.name))
        };
    }
    // The origin's write takes what an outside service said into a kind that service keeps: any
    // field or content the kind declares, as the move places one of any kind. It is the
    // platform's own write, and no app action makes it.
    if verb.origins {
        return origin_admits(schema, verb, stamp, kind);
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
        // A notice is how an action's code tells a person something: a verb that writes one
        // declares it (`notify(...)`), as it declares the outside capabilities it queues.
        if kind == HOST_NOTICE_KIND {
            return if verb.notice {
                Ok(())
            } else {
                Err(format!(
                    "`{}` tells a person nothing, so it writes no `{kind}`",
                    verb.name
                ))
            };
        }
        // A queued effect is how an action's code reaches beyond the workspace: it writes one
        // only for a capability its own app's surface declares, and the entry names that app.
        if kind == QUEUED_EFFECT_KIND {
            return queued_effect_admits(verb, id, touch, writes, queued_by);
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

/// Whether a change of `verb` queues the outside effect `id` writes: it makes the entry, its
/// code declares the capability it names, and the app the entry says it is for is the app whose
/// package wrote it. Once an entry waits, the app that queued it may take it back before it
/// goes, settling it as withdrawn; what else becomes of it is the device carrying it out's to
/// say, under the platform's own verbs.
fn queued_effect_admits(
    verb: &crate::schema::Verb,
    id: &str,
    touch: &Touch,
    writes: &[(String, String, Option<ScalarValue>)],
    queued_by: &BTreeMap<String, String>,
) -> Result<(), String> {
    // Taking an effect back is how a person undoes a send before it goes: it queues nothing.
    if touch.removed && !touch.made {
        return Ok(());
    }
    if !touch.made {
        // Taking it back before it goes: the app whose effect it is settles the entry as
        // withdrawn, for the reason it gives, and changes nothing else of it. A queued send is
        // its author's to stop until the device carries it out.
        let mine = queued_by
            .get(id)
            .is_some_and(|app| declaring_apps(verb).contains(app));
        return if mine && takes_back(id, writes) {
            Ok(())
        } else {
            Err(format!(
                "`{}` may not change a queued effect; only the app whose effect it is may take \
                 it back, and the device carrying it out says how it went",
                verb.name
            ))
        };
    }
    let written = |field: &str| -> Option<&str> {
        writes
            .iter()
            .find(|(written, name, _)| written == id && name == field)
            .and_then(|(_, _, value)| match value {
                Some(ScalarValue::Str(text)) => Some(text.as_str()),
                _ => None,
            })
    };
    let Some(app) = written(QUEUED_EFFECT_APP) else {
        return Err(format!("`{}` queues an effect naming no app", verb.name));
    };
    let Some(capability) = written(QUEUED_EFFECT_CAPABILITY) else {
        return Err(format!(
            "`{}` queues an effect naming no capability",
            verb.name
        ));
    };
    // The app an entry is for is the one whose package wrote it: a change claiming another
    // app's entry is refused here, whatever it says of itself. A verb of the package's own
    // surface writes only that surface, so a verb that names no type is the package's own.
    let apps = declaring_apps(verb);
    let ours = if apps.is_empty() {
        verb.outside
            .keys()
            .any(|surface| surface_belongs_to(surface, app))
    } else {
        apps.contains(app)
    };
    if !ours {
        return Err(format!(
            "`{}` queues an effect for `{app}`, whose package wrote it not",
            verb.name
        ));
    }
    if !outside_declared(verb, app, capability) {
        return Err(format!(
            "`{app}` declares no outside capability `{capability}`, so `{}` queues none",
            verb.name
        ));
    }
    Ok(())
}

/// Whether the writes to the queued effect `id` take it back, and nothing else: they settle it
/// as withdrawn, for the reason they give (`tilolang-tilo`'s `withdraw_in`). How it went any
/// other way is the device's to write, never an action's.
fn takes_back(id: &str, writes: &[(String, String, Option<ScalarValue>)]) -> bool {
    let mut settled = false;
    for (_, field, value) in writes.iter().filter(|(written, _, _)| written == id) {
        match (field.as_str(), value) {
            (QUEUED_EFFECT_SETTLED, Some(ScalarValue::Boolean(true))) => settled = true,
            (QUEUED_EFFECT_OUTCOME, Some(ScalarValue::Str(word)))
                if word.as_str() == QUEUED_EFFECT_WITHDRAWN => {}
            (QUEUED_EFFECT_REASON, Some(ScalarValue::Str(_))) => {}
            _ => return false,
        }
    }
    settled
}

/// The apps whose packages the verb's writes are of: each `tl.<app>.<Type>` it writes, by the
/// package the type is declared in.
fn declaring_apps(verb: &crate::schema::Verb) -> BTreeSet<String> {
    verb.writes
        .keys()
        .filter(|kind| kind.as_str() != HOST_NOTICE_KIND)
        .filter_map(|kind| tilolang_package(kind))
        .map(str::to_owned)
        .collect()
}

/// Whether the surface of `app` declares that `verb`'s code can queue `capability`: its
/// `outside(...)` calls name it, and the compilers put them in the surface the app publishes.
fn outside_declared(verb: &crate::schema::Verb, app: &str, capability: &str) -> bool {
    verb.outside.iter().any(|(surface, capabilities)| {
        capabilities.contains(capability) && surface_belongs_to(surface, app)
    })
}

/// The kind of the tilolang host's `Notice`, which a package's action writes to tell a person.
const HOST_NOTICE_KIND: &str = "tl.tilo.host.Notice";

/// The kind an outside effect waits as, which a package's action queues one of
/// (`tilolang.effect`). Its entry names the app that queued it and the capability it carries
/// out.
pub(crate) const QUEUED_EFFECT_KIND: &str = "tilolang.effect";

/// The field of a queued effect naming the app whose package queued it.
pub(crate) const QUEUED_EFFECT_APP: &str = "app";

/// The field of a queued effect naming the outside capability it carries out.
const QUEUED_EFFECT_CAPABILITY: &str = "capability";

/// The field of a queued effect that says it was settled, once it was.
const QUEUED_EFFECT_SETTLED: &str = "settled";

/// The field of a queued effect saying how it went: `withdrawn` for one a person took back
/// before it went.
const QUEUED_EFFECT_OUTCOME: &str = "outcome";

/// The field of a queued effect giving the reason it was settled as it was.
const QUEUED_EFFECT_REASON: &str = "reason";

/// The outcome of an effect a person took back before it went.
const QUEUED_EFFECT_WITHDRAWN: &str = "withdrawn";

/// The tilolang package a kind of one (`tl.<app>.<Type>`) is of: none for another kind.
fn tilolang_package(kind: &str) -> Option<&str> {
    let (package, _) = kind.strip_prefix("tl.")?.rsplit_once('.')?;
    Some(package)
}

/// Whether the surface kind `surface` (`tl.<app>` or `tl.<app>.<Type>`) is declared by `app`.
fn surface_belongs_to(surface: &str, app: &str) -> bool {
    surface
        .strip_prefix("tl.")
        .is_some_and(|rest| rest == app || rest.starts_with(&format!("{app}.")))
}

/// Whether `kind` stands among `nested` or among the kinds the types between them nest.
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
