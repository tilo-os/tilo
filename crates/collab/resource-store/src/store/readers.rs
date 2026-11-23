//! Who reads a resource of a kind whose type names its readers ([`ReaderRule`]): the store works
//! them out, from the rule, the resource's author and its fields, for every write it makes and
//! every change it takes from a peer, so a record is only ever placed in a document of exactly
//! its readers, sealed to their key group. No writer names those readers itself.

use std::collections::{BTreeMap, HashMap};

use tilo_actions::surface::ReaderRule;
use tilo_actions::{Actor, MemberId};

use super::{Store, StoreError};
use crate::admit::{Refusal, Touched};
use crate::value::{Cell, Value};
use crate::wire::Command;

impl Store {
    /// `command`, each resource it puts of a kind with a reader rule placed with the readers the
    /// rule gives, and each one nested in another without readers of its own placed with its
    /// parent's.
    ///
    /// # Errors
    /// A put names readers for a kind whose rule names them, takes back what an `until` field
    /// held, or its rule names no person.
    pub(crate) fn placed_by_rules(&self, mut command: Command) -> Result<Command, StoreError> {
        self.place_by_rules(&mut command, &mut HashMap::new())?;
        Ok(command)
    }

    fn place_by_rules(
        &self,
        command: &mut Command,
        placed: &mut HashMap<String, String>,
    ) -> Result<(), StoreError> {
        match command {
            Command::Batch(commands) => {
                for command in commands {
                    self.place_by_rules(command, placed)?;
                }
            }
            Command::Put {
                kind,
                id,
                resource,
                stamp,
            } => {
                if let Some(rule) = self.rules.get(kind.as_str()) {
                    if !resource.readers.is_empty() {
                        return Err(StoreError::Readers(format!(
                            "a {kind} is read by whom its type names ({rule}), so a write names \
                             no readers for it"
                        )));
                    }
                    let held = self.index.get(kind, id);
                    if let (ReaderRule::Author { until: Some(until) }, Some(held)) = (rule, held)
                        && holds(&held.fields, until)
                        && !holds(&resource.fields, until)
                    {
                        return Err(StoreError::Readers(format!(
                            "every member reads a {kind} once it is {until}, and it stays so"
                        )));
                    }
                    let author = if held.is_some() {
                        self.author(kind, id)
                    } else {
                        stamp.invoked_by()
                    };
                    resource.readers = readers_by_rule(rule, author.as_ref(), &resource.fields)
                        .map_err(|reason| StoreError::Readers(format!("a {kind}: {reason}")))?;
                } else if resource.readers.is_empty()
                    && let Some(parent) = &resource.parent
                {
                    resource.readers = placed
                        .get(parent)
                        .cloned()
                        .or_else(|| self.readers_of_id(parent))
                        .ok_or_else(|| {
                            StoreError::Readers(format!(
                                "a {kind} is read by whoever reads what it is in, and there is \
                                 no `{parent}`"
                            ))
                        })?;
                }
                placed.insert(id.clone(), resource.readers.clone());
            }
            _ => {}
        }
        Ok(())
    }

    /// The readers of the resource `id`, of whatever kind it is.
    fn readers_of_id(&self, id: &str) -> Option<String> {
        let kind = self.index.kind_anywhere(id)?;
        let held = self.index.get(&kind, id)?;
        self.index.readers(&held.doc).map(str::to_owned)
    }

    /// Who made the resource `id` of `kind`: the invoker of its first version.
    fn author(&self, kind: &str, id: &str) -> Option<Actor> {
        self.version_end(kind, id, false)?.stamp?.invoked_by()
    }

    /// Whether the change `received` of the document `doc_id`, of readers `readers`, leaves each
    /// resource it touches of a kind with a reader rule in a document of the readers the rule
    /// gives it.
    pub(super) fn judge_readers(
        &self,
        doc_id: &str,
        readers: &str,
        touched: &Touched,
        change: &automerge::Change,
    ) -> Result<(), Refusal> {
        let ruled: Vec<(&String, String, &ReaderRule, bool)> = touched
            .resources
            .iter()
            .filter(|(_, touch)| !touch.removed)
            .filter_map(|(id, touch)| {
                let kind = touch
                    .kind
                    .clone()
                    .or_else(|| self.index.kind_anywhere(id))?;
                let rule = self.rules.get(&kind)?;
                Some((id, kind, rule, touch.made))
            })
            .collect();
        if ruled.is_empty() {
            return Ok(());
        }
        let Some(doc) = self.docs.get(doc_id) else {
            return Err(Refusal::Unplaced);
        };
        let mut after = doc.fork();
        after
            .apply_changes([change.clone()])
            .map_err(|_| Refusal::Unplaced)?;
        let stamp = crate::admit::Stamp::of(change);
        for (id, kind, rule, made) in ruled {
            let Some((_, resource)) = super::read(&after, id) else {
                continue;
            };
            let author = if made {
                stamp
                    .as_ref()
                    .and_then(collab_types::stamp::Stamp::invoked_by)
            } else {
                self.author(&kind, id)
            };
            let expected = readers_by_rule(rule, author.as_ref(), &resource.fields)
                .map_err(|reason| Refusal::Undeclared(format!("a {kind}: {reason}")))?;
            if expected != readers {
                return Err(Refusal::Undeclared(format!(
                    "a {kind} is read by whom its type names ({rule}), not by `{readers}`"
                )));
            }
        }
        Ok(())
    }
}

/// The reader set of a resource whose kind's rule is `rule`, made by `author`, with `fields`:
/// [`collab_types::EVERY_MEMBER`], or each person it names, sorted, each once. A person reads on
/// each of their devices; an agent's record is the person's it acts for.
///
fn readers_by_rule(
    rule: &ReaderRule,
    author: Option<&Actor>,
    fields: &BTreeMap<String, Cell>,
) -> Result<String, String> {
    let mut people: Vec<String> = match rule {
        ReaderRule::Author { until } => {
            if until.as_deref().is_some_and(|until| holds(fields, until)) {
                return Ok(collab_types::EVERY_MEMBER.to_owned());
            }
            vec![
                author
                    .and_then(person_of)
                    .ok_or("only its author reads it, and no person made it")?,
            ]
        }
        ReaderRule::Field { field } => match fields.get(field) {
            Some(Cell::Set(members)) => members
                .iter()
                .map(|member| match member {
                    Value::Text(text) => serde_json::from_str::<Actor>(text)
                        .ok()
                        .as_ref()
                        .and_then(person_of)
                        .ok_or_else(|| format!("its {field} names `{text}`, who is no person")),
                    other => Err(format!("its {field} names {other:?}, who is no person")),
                })
                .collect::<Result<_, _>>()?,
            _ => Vec::new(),
        },
    };
    people.sort();
    people.dedup();
    if people.is_empty() {
        return Err("the people who read it are named by a field that names nobody".to_owned());
    }
    Ok(people.join(","))
}

/// The person `actor` is, as a reader set names them.
fn person_of(actor: &Actor) -> Option<String> {
    actor
        .member_id()
        .and_then(MemberId::person_id)
        .map(str::to_ascii_lowercase)
}

/// Whether the `Bool` field `field` holds.
fn holds(fields: &BTreeMap<String, Cell>, field: &str) -> bool {
    matches!(fields.get(field), Some(Cell::One(Value::Bool(true))))
}
