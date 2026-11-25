//! Who reads a resource of a kind whose type names its readers ([`ReaderRule`]): the store works
//! them out, from the rule, the resource's author and its fields, for every write it makes and
//! every change it takes from a peer, so a record is only ever placed in a document of exactly
//! its readers, sealed to their key group. No writer names those readers itself.

use std::collections::{BTreeMap, HashMap};

use tilo_actions::surface::ReaderRule;
use tilo_actions::{Actor, MemberId, PersonId};

use super::{Store, StoreError};
use crate::admit::Refusal;
use collab_types::store::wire::Command;
use collab_types::store::{Cell, Value};
use collab_types::touch::Touched;

/// The workspace's one read decision, as the store asks it when it places a record whose type
/// names its readers by a field: who of the actors that field names reads the record, and the
/// person its keys seal to.
///
/// A host makes one per workspace store, so the decision is that workspace's own: a person reads
/// as themselves, an agent as the person it acts for, and an agent whose grant does not reach the
/// record reads none of it. The store decides no read itself.
pub trait RecordReader: Send + Sync {
    /// The person `actor` reads as for the record of `kind` and `id`: itself for a person, the
    /// person an agent acts for (an agent is the pair of its id and that person, as a field names
    /// it), and `None` where it reads none of the record (no member, or a grant that does not
    /// reach it).
    fn reader_of(&self, actor: &Actor, kind: &str, id: &str) -> Option<PersonId>;
}

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
                        Author {
                            actor: stamp.invoked_by(),
                            acted_for: stamp.acted_for.clone(),
                        }
                    };
                    resource.readers = readers_by_rule(
                        rule,
                        &author,
                        &resource.fields,
                        kind,
                        id,
                        self.reader.as_deref(),
                    )
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

    /// Files the resource `id` of `kind` under the readers its type's rule gives it where it is
    /// held in a document every member reads: a record written there before its type named its
    /// readers, or by a writer that did not know the rule, is read as the rule says, never by
    /// the document's `*`. One whose rule names nobody it can tell is read by nobody.
    pub(super) fn file_by_rule(&mut self, kind: &str, id: &str) {
        let Some(rule) = self.rules.get(kind) else {
            return;
        };
        let Some(held) = self.index.get(kind, id) else {
            return;
        };
        if self.index.readers(&held.doc) != Some(collab_types::EVERY_MEMBER) {
            return;
        }
        let readers = readers_by_rule(
            rule,
            &self.author(kind, id),
            &held.fields,
            kind,
            id,
            self.reader.as_deref(),
        )
        .unwrap_or_default();
        self.index.refile(kind, id, &readers);
    }

    /// Whether the resource `id` of `kind` is held where its type's rule does not place it: in a
    /// document every member reads while its rule names fewer.
    #[must_use]
    pub fn misplaced(&self, kind: &str, id: &str) -> bool {
        self.rules.contains_key(kind)
            && self.index.get(kind, id).is_some_and(|held| {
                self.index.readers(&held.doc) == Some(collab_types::EVERY_MEMBER)
                    && self.index.filed(kind, id) != Some(collab_types::EVERY_MEMBER)
            })
    }

    /// Every resource held where its type's rule does not place it ([`Self::misplaced`]), by
    /// kind and id, in kind then id order: what a workspace moves into its rule's document when
    /// it opens, and what an operator counts.
    #[must_use]
    pub fn misplaced_records(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for kind in self.rules.keys() {
            let mut ids: Vec<&str> = self.index.ids_of(kind).collect();
            ids.sort_unstable();
            for id in ids {
                if self.misplaced(kind, id) {
                    out.push((kind.clone(), id.to_owned()));
                }
            }
        }
        out
    }

    /// Who reads the history of the removed resource `id` of `kind`, whose type names its
    /// readers, when it was held where every member read it: those its rule gives a record of
    /// no fields, its author for `readers: author`, and nobody where the rule names no one.
    pub(super) fn removed_readers(&self, kind: &str, id: &str) -> String {
        let Some(rule) = self.rules.get(kind) else {
            return collab_types::EVERY_MEMBER.to_owned();
        };
        readers_by_rule(
            rule,
            &self.author(kind, id),
            &BTreeMap::new(),
            kind,
            id,
            self.reader.as_deref(),
        )
        .unwrap_or_default()
    }

    /// The readers of the resource `id`, of whatever kind it is.
    fn readers_of_id(&self, id: &str) -> Option<String> {
        let kind = self.index.kind_anywhere(id)?;
        let held = self.index.get(&kind, id)?;
        self.index.readers(&held.doc).map(str::to_owned)
    }

    /// Who made the resource `id` of `kind`, as its reader set names them: the person whose
    /// record it is, when a person made it. What a repair of a record whose type names its
    /// readers acts for, so the moved record reads as its rule says.
    #[must_use]
    pub fn author_person(&self, kind: &str, id: &str) -> Option<String> {
        self.author(kind, id).person()
    }

    /// Who made the resource `id` of `kind`: the invoker of its first version, and the person it
    /// acted for.
    fn author(&self, kind: &str, id: &str) -> Author {
        self.version_end(kind, id, false)
            .and_then(|version| version.stamp)
            .map_or_else(Author::default, |stamp| Author {
                actor: stamp.invoked_by(),
                acted_for: stamp.acted_for,
            })
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
                Author {
                    actor: stamp
                        .as_ref()
                        .and_then(collab_types::stamp::Stamp::invoked_by),
                    acted_for: stamp.as_ref().and_then(|stamp| stamp.acted_for.clone()),
                }
            } else {
                self.author(&kind, id)
            };
            let expected = readers_by_rule(
                rule,
                &author,
                &resource.fields,
                &kind,
                id,
                self.reader.as_deref(),
            )
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

/// Who made a resource: the actor, and the person it acted for when that is not itself.
#[derive(Clone, Debug, Default)]
struct Author {
    actor: Option<Actor>,
    acted_for: Option<String>,
}

/// The reader set of a resource whose kind's rule is `rule`, made by `author`, with `fields`:
/// [`collab_types::EVERY_MEMBER`], or each person it names, sorted, each once, after the role
/// whose holders read it beside them when the rule names one (`@role:moderator,<author>`). Who
/// holds that role is never written here: the set's key group follows the workspace's governance.
/// A person reads on each of their devices; an agent's record is the person's it acts for.
fn readers_by_rule(
    rule: &ReaderRule,
    author: &Author,
    fields: &BTreeMap<String, Cell>,
    kind: &str,
    id: &str,
    reader: Option<&dyn RecordReader>,
) -> Result<String, String> {
    let mut people: Vec<String> = match rule {
        ReaderRule::AuthorAnd { role } => {
            let author = author
                .person()
                .ok_or("its author reads it, and no person made it")?;
            return Ok(format!(
                "{}{},{author}",
                collab_types::ROLE_READERS,
                role.key()
            ));
        }
        ReaderRule::Author { until } => {
            if until.as_deref().is_some_and(|until| holds(fields, until)) {
                return Ok(collab_types::EVERY_MEMBER.to_owned());
            }
            vec![
                author
                    .person()
                    .ok_or("only its author reads it, and no person made it")?,
            ]
        }
        ReaderRule::Field { field } => match fields.get(field) {
            Some(Cell::Set(members)) => members
                .iter()
                .map(|member| match member {
                    Value::Text(text) => {
                        let actor = serde_json::from_str::<Actor>(text)
                            .map_err(|_| format!("its {field} names `{text}`, who is no person"))?;
                        // A person reads as themselves; software reads as the person the
                        // workspace's one read decision gives it, and none where it reads none.
                        let person = match actor.member_id() {
                            Some(MemberId::Person(person)) => {
                                Some(person.as_str().to_ascii_lowercase())
                            }
                            _ => reader
                                .and_then(|reader| reader.reader_of(&actor, kind, id))
                                .map(|person| person.as_str().to_ascii_lowercase()),
                        };
                        person.ok_or_else(|| {
                            format!("its {field} names `{text}`, who reads none of it")
                        })
                    }
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

impl Author {
    /// The person whose record this is, as a reader set names them: the actor when it is a
    /// person, else the person it acted for.
    fn person(&self) -> Option<String> {
        self.actor
            .as_ref()
            .and_then(|actor| match actor.member_id() {
                Some(MemberId::Person(person)) => Some(person.as_str().to_ascii_lowercase()),
                _ => None,
            })
            .or_else(|| self.acted_for.as_deref().map(str::to_ascii_lowercase))
    }
}

/// Whether the `Bool` field `field` holds.
fn holds(fields: &BTreeMap<String, Cell>, field: &str) -> bool {
    matches!(fields.get(field), Some(Cell::One(Value::Bool(true))))
}
