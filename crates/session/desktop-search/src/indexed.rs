//! What apps hold, found whether or not they run: a source over the
//! session's persistent index of apps' search entries
//! ([`crate::content_index`]): a note, a message, a login, by its title, its
//! secondary line and its short fields.
//!
//! A hit runs, by address, the entry's own published action, or the app's
//! `open` at the entry's resource ([`OPEN_ACTION`]); the host invokes it on
//! the running app or launches the app first. Secret-bearing entries
//! (Passwords' logins) are found only while their app last said its secrets
//! are unlocked; while they are locked, a query one of them would match gets
//! one row instead, "Unlock Passwords to search for …", which opens the app
//! to unlock it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use tilo_search::{Document, Matching, Page, SearchIndex};
use ui::search::index_publication::{IndexEntry, IndexWeight};
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind, provider_score};

use crate::content_index::{AppContentIndex, Contents, IndexedEntry, SlotKey};
use crate::model::{Hit, InvokeTarget, Source, Target};

/// The action every framework app publishes to show the item at an anchor
/// (`tilo_app::framework`'s `open`), and its parameter.
pub const OPEN_ACTION: &str = "open";
pub const OPEN_ANCHOR: &str = "anchor";

/// Separates the parts of a row id: an entry's key may hold anything, so
/// it comes last.
const ID_SEPARATOR: char = '\u{1f}';
/// The id prefix of an app's unlock row.
const UNLOCK: &str = "unlock";

/// How many index hits one query reads.
const MAX_HITS: usize = 256;

/// The index at one generation, ready to search.
pub struct Snapshot {
    pub generation: u64,
    pub visible: Vec<IndexedEntry>,
    pub index: SearchIndex,
    /// Locked secret-bearing entries, matched only to offer unlocking.
    locked: Vec<IndexedEntry>,
    locked_index: SearchIndex,
    /// `(app, room, resource)` of every visible entry.
    covered: HashSet<(String, String, String)>,
    /// The boost its app gave each visible entry that has one, by row id.
    boosts: HashMap<String, i32>,
}

impl Snapshot {
    fn new(generation: u64, contents: Contents) -> Self {
        let Contents { visible, locked } = contents;
        let covered = visible
            .iter()
            .map(|indexed| {
                (
                    indexed.slot.app.clone(),
                    indexed.slot.room.clone(),
                    indexed.entry.resource.clone(),
                )
            })
            .collect();
        let boosts = visible
            .iter()
            .filter(|indexed| indexed.entry.boost != 0)
            .map(|indexed| {
                (
                    entry_id(&indexed.slot, &indexed.entry.key),
                    indexed.entry.boost,
                )
            })
            .collect();
        Self {
            generation,
            index: text_index(&visible),
            locked_index: text_index(&locked),
            visible,
            locked,
            covered,
            boosts,
        }
    }
}

/// The entries' texts, each found by its position.
fn text_index(entries: &[IndexedEntry]) -> SearchIndex {
    // Names a person half remembers: words across fields and typos.
    let mut index = SearchIndex::new(Matching::LOOSE);
    index.replace_all(entries.iter().enumerate().map(|(at, indexed)| {
        // What the entry refers to counts as it is called now, never as the
        // title written into its text when the reference was made.
        indexed
            .entry
            .searchable()
            .chain(
                indexed
                    .referenced
                    .iter()
                    .map(|title| (title.as_str(), IndexWeight::Body)),
            )
            .fold(Document::new(at as u64), |document, (text, weight)| {
                document.field(text, weight.field_weight())
            })
    }));
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index apps' content");
    }
    index
}

/// A row's id: where its entry was published from, then its key.
#[must_use]
pub fn entry_id(slot: &SlotKey, key: &str) -> String {
    format!(
        "{}{ID_SEPARATOR}{}{ID_SEPARATOR}{}{ID_SEPARATOR}{key}",
        slot.app, slot.room, slot.record
    )
}

/// The app an unlock row's id names.
#[must_use]
pub fn unlock_app(id: &str) -> Option<&str> {
    id.strip_prefix(UNLOCK)?.strip_prefix(ID_SEPARATOR)
}

/// The slot and key a row id names.
#[must_use]
pub fn parse_entry_id(id: &str) -> Option<(SlotKey, &str)> {
    let mut parts = id.splitn(4, ID_SEPARATOR);
    let app = parts.next()?;
    let room = parts.next()?;
    let record = parts.next()?;
    let key = parts.next()?;
    Some((
        SlotKey {
            app: app.to_owned(),
            room: room.to_owned(),
            record: record.to_owned(),
        },
        key,
    ))
}

/// What running `entry` of `slot` invokes: the entry's own action, else
/// the app's `open` at the entry's resource.
#[must_use]
pub fn opening(slot: &SlotKey, entry: &IndexEntry) -> InvokeTarget {
    let (address, arguments) = match &entry.action {
        Some(action) => (
            tilo_actions::ActionAddress::new(&action.resource, &action.action),
            serde_json::Map::new(),
        ),
        None => {
            let mut arguments = serde_json::Map::new();
            arguments.insert(
                OPEN_ANCHOR.into(),
                serde_json::Value::String(entry.resource.clone()),
            );
            (
                tilo_actions::ActionAddress::new(&entry.resource, OPEN_ACTION),
                arguments,
            )
        }
    };
    InvokeTarget {
        app: slot.app.clone(),
        room: slot.room.clone(),
        address,
        arguments,
        descriptor: None,
    }
}

/// What apps hold, from the session's index ([`AppContentIndex`]).
pub struct IndexedContent {
    index: Arc<AppContentIndex>,
    snapshot: RefCell<Option<Rc<Snapshot>>>,
    /// An app's name as the person knows it.
    app_name: Box<dyn Fn(&str) -> String>,
}

impl IndexedContent {
    /// A source over `index`, naming apps by `app_name`.
    pub fn new(index: Arc<AppContentIndex>, app_name: impl Fn(&str) -> String + 'static) -> Self {
        Self {
            index,
            snapshot: RefCell::new(None),
            app_name: Box::new(app_name),
        }
    }

    /// The index this source searches.
    #[must_use]
    pub fn index(&self) -> &Arc<AppContentIndex> {
        &self.index
    }

    /// The snapshot of the index as it is now, built if it moved.
    pub fn current(&self) -> Rc<Snapshot> {
        let generation = self.index.generation();
        let mut slot = self.snapshot.borrow_mut();
        match slot.as_ref() {
            Some(snapshot) if snapshot.generation == generation => Rc::clone(snapshot),
            _ => {
                // Read the generation before the contents: a change landing
                // between them leaves this snapshot stale, never stamped
                // current.
                let snapshot = Rc::new(Snapshot::new(generation, self.index.contents()));
                *slot = Some(Rc::clone(&snapshot));
                snapshot
            }
        }
    }

    /// Whether this source finds `resource` of `app` in `room`, whether or
    /// not the app runs: one resource is one row, so a window's own row for
    /// it gives way.
    #[must_use]
    pub fn covers(&self, app: &str, room: &str, resource: &str) -> bool {
        self.current()
            .covered
            .contains(&(app.to_owned(), room.to_owned(), resource.to_owned()))
    }

    /// The boost its app gave row `id`: it orders the row among the app's
    /// own, never against other sources'.
    #[must_use]
    pub fn boost(&self, id: &str) -> i32 {
        self.current().boosts.get(id).copied().unwrap_or(0)
    }

    /// The visible entry row `id` names.
    #[must_use]
    pub fn entry(&self, id: &str) -> Option<IndexedEntry> {
        let (slot, key) = parse_entry_id(id)?;
        self.current()
            .visible
            .iter()
            .find(|indexed| indexed.slot == slot && indexed.entry.key == key)
            .cloned()
    }

    /// The row for one visible entry, scored `score`.
    #[must_use]
    pub fn row(&self, indexed: &IndexedEntry, score: i32) -> Hit {
        let IndexedEntry { slot, entry, .. } = indexed;
        let subtitle = entry
            .subtitle
            .clone()
            .or_else(|| entry.group.clone())
            .unwrap_or_else(|| (self.app_name)(&slot.app));
        Hit {
            kind: ResourceKind::Record,
            provider: self.id().into(),
            id: entry_id(slot, &entry.key),
            title: entry.title.clone(),
            subtitle: Some(subtitle),
            facts: Vec::new(),
            score,
            icon: entry.icon.as_deref().and_then(ui::icons::Icon::from_name),
            target: Target::Invoke(Box::new(opening(slot, entry))),
        }
    }

    /// One row offering to unlock `app`, for the `needle` its locked
    /// entries match.
    fn unlock_row(&self, app: &str, needle: &str, score: i32) -> Hit {
        let name = (self.app_name)(app);
        Hit {
            kind: ResourceKind::Record,
            provider: self.id().into(),
            id: format!("{UNLOCK}{ID_SEPARATOR}{app}"),
            title: crate::i18n::t_args(
                "search-unlock-app",
                &[
                    ("app", name.as_str().into()),
                    ("query", needle.trim().into()),
                ],
            ),
            subtitle: Some(crate::i18n::t_args(
                "search-unlock-app-detail",
                &[("app", name.as_str().into())],
            )),
            facts: Vec::new(),
            score,
            icon: Some(ui::icons::Icon::Lock),
            target: Target::Launch {
                app: app.to_owned(),
            },
        }
    }

    /// One unlock row per app whose locked entries match `needle`, scored
    /// by its best match.
    #[must_use]
    pub fn unlock_rows(&self, needle: &str) -> Vec<Hit> {
        let snapshot = self.current();
        let mut best: Vec<(String, i32)> = Vec::new();
        for hit in snapshot
            .locked_index
            .search(needle, Page::first(MAX_HITS))
            .hits
        {
            let Some(indexed) = usize::try_from(hit.key)
                .ok()
                .and_then(|at| snapshot.locked.get(at))
            else {
                continue;
            };
            let score = provider_score(hit.rank.unwrap_or(0));
            match best.iter_mut().find(|(app, _)| *app == indexed.slot.app) {
                Some((_, held)) => *held = (*held).max(score),
                None => best.push((indexed.slot.app.clone(), score)),
            }
        }
        best.into_iter()
            .map(|(app, score)| self.unlock_row(&app, needle, score.max(1)))
            .collect()
    }

    /// The row `id` names, as an empty query scores it.
    #[must_use]
    pub fn resolve(&self, id: &str, needle: &str) -> Option<Hit> {
        if let Some(app) = unlock_app(id) {
            let snapshot = self.current();
            return snapshot
                .locked
                .iter()
                .any(|indexed| indexed.slot.app == app)
                .then(|| self.unlock_row(app, needle, MAX_PROVIDER_SCORE));
        }
        self.entry(id)
            .map(|indexed| self.row(&indexed, MAX_PROVIDER_SCORE))
    }
}

impl Source for IndexedContent {
    fn id(&self) -> &'static str {
        "app-content"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        let snapshot = self.current();
        // Two rooms' copies of one app's entry (a vault opened in each) are
        // one row.
        let mut shown: HashSet<(&str, &str)> = HashSet::new();
        let mut out: Vec<Hit> = snapshot
            .index
            .search(needle, Page::first(MAX_HITS))
            .hits
            .iter()
            .filter_map(|hit| {
                let indexed = usize::try_from(hit.key)
                    .ok()
                    .and_then(|at| snapshot.visible.get(at))?;
                shown
                    .insert((indexed.slot.app.as_str(), indexed.entry.key.as_str()))
                    .then(|| {
                        let score =
                            provider_score(hit.rank.unwrap_or(0)).clamp(1, MAX_PROVIDER_SCORE);
                        self.row(indexed, score)
                    })
            })
            .collect();
        out.extend(self.unlock_rows(needle));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::index_publication::{IndexState, Publisher};

    fn note(key: &str, title: &str, secret: bool) -> IndexEntry {
        IndexEntry {
            key: key.into(),
            resource: key.into(),
            action: None,
            title: title.into(),
            subtitle: None,
            group: None,
            icon: None,
            boost: 0,
            fields: Vec::new(),
            secret,
            references: Vec::new(),
        }
    }

    fn publish(
        index: &AppContentIndex,
        app: &str,
        unlocked: Option<bool>,
        entries: Vec<IndexEntry>,
    ) {
        let mut publisher = Publisher::default();
        let state = IndexState {
            generation: 1,
            secrets_unlocked: unlocked,
        };
        for delta in publisher.publish(state, entries) {
            index
                .publish(
                    "room",
                    app,
                    "tab:0001",
                    &delta.encode().expect("a delta encodes"),
                )
                .expect("the index takes the delta");
        }
    }

    #[test]
    fn a_hit_opens_its_app_at_the_entry_by_address() {
        let index = Arc::new(AppContentIndex::in_memory());
        publish(
            &index,
            "so.tilo.notes",
            None,
            vec![note("n1", "Groceries", false)],
        );
        let content = IndexedContent::new(Arc::clone(&index), |app| format!("{app}!"));
        let hits = content.query("groceries");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subtitle.as_deref(), Some("so.tilo.notes!"));
        let Target::Invoke(target) = &hits[0].target else {
            panic!("an entry runs its app's open");
        };
        assert_eq!(
            target.address,
            tilo_actions::ActionAddress::new("n1", OPEN_ACTION)
        );
        assert_eq!(target.arguments[OPEN_ANCHOR], "n1");
        assert!(content.covers("so.tilo.notes", "room", "n1"));
        assert_eq!(
            content
                .resolve(&hits[0].id, "")
                .map(|hit| hit.title)
                .as_deref(),
            Some("Groceries")
        );
    }

    #[test]
    fn a_locked_vault_offers_only_to_unlock_it() {
        let index = Arc::new(AppContentIndex::in_memory());
        publish(
            &index,
            "so.tilo.passwords",
            Some(false),
            vec![note("l1", "GitHub", true)],
        );
        let content = IndexedContent::new(index, str::to_owned);
        let hits = content.query("github");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].title,
            "Unlock so.tilo.passwords to search for “github”"
        );
        assert_eq!(
            hits[0].target,
            Target::Launch {
                app: "so.tilo.passwords".into()
            }
        );
    }
}
