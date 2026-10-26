//! Reactions: people answering something with an emoji.
//!
//! Any item of any record, or a record itself, takes reactions by holding a
//! [`Reactions`] field:
//!
//! ```
//! use tilo_resource::{Collection, Model, Reactions};
//!
//! #[derive(Model, Default)]
//! pub struct Page {
//!     pub title: String,
//!     pub reactions: Reactions,
//! }
//! ```
//!
//! Its edits are the framework's two field edits, [`EditOp::React`] and
//! [`EditOp::Unreact`] (`Page::paths().reactions.react("👍")`). They store who
//! invoked them ([`ActionCx::author`]), never an argument, so a reaction is
//! always its author's own: a read-only item (a chat message) still takes
//! them, and every door publishes, permissions, records in history, syncs
//! and undoes them like any other field edit. A bot reacting for someone
//! elsewhere (a Discord member, through the bridge) records the account it
//! acts for and the name it shows ([`ActionCx::appearance`]), so each of
//! them is a reaction of their own.
//!
//! An emoji is either a Unicode emoji (`👍`) or a workspace's own emoji
//! written `:name:` ([`custom_emoji_name`]), which every view draws from the
//! workspace's emoji and shows as its `:name:` text where the workspace has
//! none of that name.

use std::collections::BTreeSet;

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ObjType, ReadDoc, Value};

use crate::action::{ActionCx, Refusal};
use crate::delta::Delta;
use crate::map::Map;
use crate::model::{Atom, Doc, Model, ModelError, Slot};
use crate::path::{EditOp, Segment};

/// The most different emoji one thing holds reactions with.
pub const MAX_EMOJI: usize = 20;
/// The most reactions one thing holds, everyone's together.
pub const MAX_REACTIONS: usize = 2000;
/// The longest emoji, in bytes: a Unicode sequence (a family with skin
/// tones) or a workspace emoji's `:name:`.
pub const MAX_EMOJI_BYTES: usize = 64;
/// The longest workspace emoji name, in characters.
pub const MAX_CUSTOM_NAME: usize = 32;

/// Separates the parts of a reaction's key: never part of an emoji or of a
/// participant id.
const KEY_SEPARATOR: char = '\u{1f}';

/// One reaction: who reacted with which emoji, and when.
#[derive(crate::Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Reaction {
    /// The emoji: Unicode, or a workspace emoji as `:name:`.
    pub emoji: Atom,
    /// The participant who invoked it.
    pub by: Atom,
    /// The outside account a bot reacted for (`discord/80351110224678912`).
    pub persona: Option<Atom>,
    /// The name the account shows, when a bot reacted for one.
    pub name: Option<Atom>,
    /// When, in milliseconds since the epoch.
    pub at: i64,
}

impl Reaction {
    /// Whether `participant` made this reaction themselves: not a bot
    /// reacting for someone elsewhere.
    #[must_use]
    pub fn is_own(&self, participant: &str) -> bool {
        self.persona.is_none() && self.by.0 == participant
    }

    fn key(&self) -> String {
        key_of(
            &self.emoji.0,
            &self.by.0,
            self.persona.as_ref().map(|p| p.0.as_str()),
        )
    }
}

fn key_of(emoji: &str, by: &str, persona: Option<&str>) -> String {
    let mut key = String::new();
    key.push_str(emoji);
    key.push(KEY_SEPARATOR);
    key.push_str(persona.unwrap_or_default());
    key.push(KEY_SEPARATOR);
    key.push_str(by);
    key
}

/// The emoji part of a stored key.
fn emoji_of_key(key: &str) -> &str {
    key.split(KEY_SEPARATOR).next().unwrap_or(key)
}

/// One person (or outside account) among those who reacted with an emoji.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reactor {
    /// The participant who invoked the reaction.
    pub by: String,
    /// The outside account a bot reacted for.
    pub persona: Option<String>,
    /// The name that account shows.
    pub name: Option<String>,
}

/// Everyone who reacted with one emoji, first reaction first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionGroup {
    pub emoji: String,
    pub reactors: Vec<Reactor>,
    /// When the first of them reacted.
    pub first_at: i64,
}

impl ReactionGroup {
    #[must_use]
    pub fn count(&self) -> usize {
        self.reactors.len()
    }

    /// Whether `participant` is among them themselves.
    #[must_use]
    pub fn includes(&self, participant: &str) -> bool {
        self.reactors
            .iter()
            .any(|reactor| reactor.persona.is_none() && reactor.by == participant)
    }
}

/// The reactions to one thing. See the [module docs](self).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reactions {
    entries: Map<Reaction>,
}

impl Reactions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many reactions, everyone's together.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Every reaction, by emoji.
    pub fn iter(&self) -> impl Iterator<Item = &Reaction> {
        self.entries.values()
    }

    /// The reactions grouped by emoji, the emoji reacted with first first:
    /// what a reaction strip shows.
    #[must_use]
    pub fn groups(&self) -> Vec<ReactionGroup> {
        let mut groups: Vec<ReactionGroup> = Vec::new();
        let mut sorted: Vec<&Reaction> = self.entries.values().collect();
        sorted.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.key().cmp(&b.key())));
        for reaction in sorted {
            let reactor = Reactor {
                by: reaction.by.0.clone(),
                persona: reaction.persona.as_ref().map(|p| p.0.clone()),
                name: reaction.name.as_ref().map(|n| n.0.clone()),
            };
            match groups.iter_mut().find(|g| g.emoji == reaction.emoji.0) {
                Some(group) => group.reactors.push(reactor),
                None => groups.push(ReactionGroup {
                    emoji: reaction.emoji.0.clone(),
                    reactors: vec![reactor],
                    first_at: reaction.at,
                }),
            }
        }
        groups
    }

    /// Whether `participant` reacted with `emoji` themselves.
    #[must_use]
    pub fn reacted(&self, emoji: &str, participant: &str) -> bool {
        self.entries.contains_key(&key_of(emoji, participant, None))
    }

    /// The edit that adds `participant`'s reaction with `emoji`, or takes it
    /// back when they already reacted with it: what a click on a pill or a
    /// pick in the picker does.
    #[must_use]
    pub fn toggle(&self, emoji: &str, participant: &str) -> EditOp {
        if self.reacted(emoji, participant) {
            EditOp::Unreact {
                emoji: emoji.to_owned(),
            }
        } else {
            EditOp::React {
                emoji: emoji.to_owned(),
            }
        }
    }

    /// Add `reaction` (for fixtures and imports that hold a value, never a
    /// verb: those react through [`EditOp::React`]).
    pub fn insert(&mut self, reaction: Reaction) {
        self.entries.insert(reaction.key(), reaction);
    }
}

/// The [`Reactions`] field of `record`, a record of `table`'s type, and the
/// key it is stored under, when the record holds one: the reactions to the
/// record itself, which a window showing it offers in its bar. A record
/// opts in by holding the field, nothing else; the first such field
/// counts.
///
/// `table` is the record's own ([`Resource::RECORD_TABLE`](crate::Resource::RECORD_TABLE)),
/// so code that does not know the record's type reads it.
#[must_use]
pub fn record_reactions<'a>(
    table: &'static crate::table::ModelTable,
    record: &'a dyn std::any::Any,
) -> Option<(&'static str, &'a Reactions)> {
    let desc = table.record()?;
    let own = <Reactions as Model>::TABLE;
    desc.fields
        .iter()
        .enumerate()
        .filter(|(_, field)| std::ptr::eq(field.table, own))
        .find_map(|(index, field)| {
            (desc.field)(record, index)?
                .downcast_ref::<Reactions>()
                .map(|reactions| (field.key, reactions))
        })
}

/// The name of the workspace emoji `emoji` writes as `:name:`, when it is
/// one.
#[must_use]
pub fn custom_emoji_name(emoji: &str) -> Option<&str> {
    emoji
        .strip_prefix(':')
        .and_then(|rest| rest.strip_suffix(':'))
        .filter(|name| valid_custom_emoji_name(name))
}

/// Whether `name` can name a workspace emoji: 2 to 32 ASCII letters, digits
/// and underscores, as Discord names its custom emoji, so the same name
/// works on both sides of the bridge.
#[must_use]
pub fn valid_custom_emoji_name(name: &str) -> bool {
    (2..=MAX_CUSTOM_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// `text` as a reaction's emoji, trimmed: a workspace emoji's `:name:`, or
/// one Unicode emoji (no letters, digits alone, spaces or control
/// characters).
///
/// # Errors
/// Why `text` is no emoji.
pub fn reaction_emoji(text: &str) -> Result<&str, Refusal> {
    let emoji = text.trim();
    let unicode = || {
        emoji.len() <= MAX_EMOJI_BYTES
            && !emoji.is_ascii()
            && !emoji
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c.is_alphabetic() || c == ':')
    };
    if custom_emoji_name(emoji).is_some() || (!emoji.is_empty() && unicode()) {
        Ok(emoji)
    } else {
        Err(Refusal::new("a reaction is one emoji"))
    }
}

impl Model for Reactions {
    crate::__own_table!(Reactions);
    crate::model::leaf_paths!();

    fn value_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "array",
            "description": "Reactions: changed only by the react and unreact field edits",
            "items": {"type": "object"},
        })
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.entries.values().map(Model::to_json).collect())
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        let mut reactions = Self::new();
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    reactions.insert(Reaction::from_json(value)?);
                }
            }
            serde_json::Value::Null => {}
            _ => return Err(ModelError::shape("a list of reactions")),
        }
        Ok(reactions)
    }

    /// An entry that reads as no reaction (written by something else under
    /// this field's name) is passed over: the thing holding it still reads.
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::Map)) {
            return Ok(Self::new());
        }
        let entries = doc
            .keys(id)
            .filter_map(|key| {
                let reaction = crate::model::read_key::<Reaction>(doc, id, &key).ok()?;
                (reaction.key() == key).then_some((key, reaction))
            })
            .collect();
        Ok(Self { entries })
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::new())
    }

    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        self.entries.store(doc, slot)
    }

    fn stores_as(&self, before: &Self) -> bool {
        self.entries.stores_as(&before.entries)
    }

    fn store_changed(
        &self,
        before: &Self,
        doc: &mut Doc,
        slot: Slot<'_>,
        dirty: &Delta,
    ) -> Result<(), ModelError> {
        self.entries
            .store_changed(&before.entries, doc, slot, dirty)
    }

    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        // Read again whole, as `load` reads it: a thing's reactions are few.
        let _ = delta;
        *self = Self::load(doc, value, id)?;
        Ok(())
    }

    /// Reactions change only by [`EditOp::React`] and [`EditOp::Unreact`],
    /// which name their author themselves: a copy edited elsewhere says
    /// nothing about who reacted, so it gives no edits.
    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<crate::Edit<R>>) {
        let _ = (after, path, out);
    }

    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        if let Some(segment) = path.first() {
            return Err(ModelError::invalid(format!(
                "nothing inside reactions is called `{segment}`"
            )));
        }
        let (emoji, adding) = match op {
            EditOp::React { emoji } => (emoji, true),
            EditOp::Unreact { emoji } => (emoji, false),
            other => return Err(ModelError::unsupported(other)),
        };
        let emoji = reaction_emoji(emoji).map_err(ModelError::refused)?;
        let by = cx
            .author()
            .filter(|author| !author.is_empty() && author.len() <= 4096)
            .ok_or_else(|| ModelError::Refused("a reaction is always someone's".into()))?
            .to_owned();
        let appearance = cx.appearance();
        let persona = appearance
            .as_ref()
            .and_then(|appearance| appearance.persona.as_ref())
            .map(|persona| format!("{}/{}", persona.service, persona.account));
        let key = key_of(emoji, &by, persona.as_deref());
        let map = slot.object(doc, ObjType::Map)?;
        let present = doc.get(&map, key.as_str())?.is_some();
        if !adding {
            if present {
                doc.delete(&map, key.as_str())?;
            }
            return Ok(());
        }
        if present {
            return Ok(());
        }
        let keys: Vec<String> = doc.keys(&map).collect();
        if keys.len() >= MAX_REACTIONS {
            return Err(ModelError::Refused(
                "this has as many reactions as it can hold".into(),
            ));
        }
        let emoji_held: BTreeSet<&str> = keys.iter().map(|key| emoji_of_key(key)).collect();
        if !emoji_held.contains(emoji) && emoji_held.len() >= MAX_EMOJI {
            return Err(ModelError::Refused(format!(
                "this already has reactions with {MAX_EMOJI} different emoji"
            )));
        }
        let reaction = Reaction {
            emoji: Atom(emoji.to_owned()),
            by: Atom(by),
            name: persona
                .is_some()
                .then(|| appearance.map(|appearance| Atom(appearance.name)))
                .flatten(),
            persona: persona.map(Atom),
            at: cx.now_ms(),
        };
        crate::model::write_key(doc, &map, &key, &reaction)
    }
}

#[cfg(test)]
#[path = "reactions_tests.rs"]
mod tests;
