//! What the device's apps and records hold, found by words.
//!
//! The device's daemon keeps the one index of it ([`AppContentIndex`]): what a running app
//! publishes over its route, and what the records the device holds say when their app is asked
//! in its record mode ([`records`]). Every caller of the workspace's search (`search#find`: the
//! person's palette, text typed on the canvas, an agent's `find`) is answered from it by
//! [`IndexSearch`], and only with what that caller may read.

pub mod index;
pub mod records;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use tilo_actions::remote::search::{Description, IndexKind, IndexPerson, Picture};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, ActionCallError, Caller, Found, Pending, ResourceSearch, Within,
};
use tilo_resource::search_index::{IndexDelta, IndexEntry, IndexWeight};
use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};

pub use index::{AppContentIndex, Contents, IndexKey, IndexedEntry, record_slot};

/// How many hits of the text index one search reads.
const MAX_HITS: usize = 256;

/// How many people, agents and conversations a search asked for them lists: the most recent.
const MAX_LISTED: usize = 500;

/// The argument an `open` is told the item it shows by.
const OPEN_ANCHOR: &str = "anchor";

/// Separates the parts of a result's key: an entry's own key may hold anything, so it comes last.
const KEY_SEPARATOR: char = '\u{1f}';

/// Whether `caller` may read what was published under a key: the record it came from, decided
/// where every read of that record is.
pub type MayRead = Arc<dyn Fn(&Caller, &IndexKey) -> bool + Send + Sync>;

/// The device's index, searched for each caller as that caller may read it.
pub struct IndexSearch {
    index: Arc<AppContentIndex>,
    /// The snapshot built for the generation last seen, shared by every caller.
    snapshot: Mutex<Option<Arc<Snapshot>>>,
    may_read: MayRead,
}

impl IndexSearch {
    /// A search of `index`, each result of it checked against `may_read` for the caller asking.
    #[must_use]
    pub fn new(index: Arc<AppContentIndex>, may_read: MayRead) -> Self {
        Self {
            index,
            snapshot: Mutex::new(None),
            may_read,
        }
    }

    /// The index this searches.
    #[must_use]
    pub fn index(&self) -> &Arc<AppContentIndex> {
        &self.index
    }

    /// The index as it is now, built again only when it moved.
    fn current(&self) -> Arc<Snapshot> {
        let generation = self.index.generation();
        let mut slot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_ref() {
            Some(snapshot) if snapshot.generation == generation => Arc::clone(snapshot),
            _ => {
                // The generation is read before the contents: a change landing between them
                // leaves this snapshot stale, never stamped current.
                let snapshot = Arc::new(Snapshot::new(generation, self.index.contents()));
                *slot = Some(Arc::clone(&snapshot));
                snapshot
            }
        }
    }

    /// What `needle` finds within `within` that `caller` may read, best first: each entry by its
    /// app's own words for it, then one result per app whose locked secrets it matches, which
    /// opens that app to unlock them. Asked with no words and narrowed to what entries are
    /// about, every one of those, the most recent first.
    #[must_use]
    pub fn found(&self, caller: &Caller, needle: &str, within: &Within) -> Vec<Found> {
        let needle = needle.trim();
        let snapshot = self.current();
        let reads = self.reads(caller);
        if needle.is_empty() {
            return if within.about.is_empty() {
                Vec::new()
            } else {
                snapshot.listed(&reads, within)
            };
        }
        let mut out = snapshot.matched(&reads, needle, within);
        if within.place.is_none() && !within.is_timed() && within.about.is_empty() {
            out.extend(snapshot.unlocking(&reads, needle, within.app.as_deref()));
        }
        out
    }

    /// Every picture the index holds that `caller` may read: each entry an app published that
    /// names a picture's bytes, and that is no secret.
    #[must_use]
    pub fn pictures(&self, caller: &Caller) -> Vec<Picture> {
        let reads = self.reads(caller);
        self.current()
            .visible
            .iter()
            .filter(|indexed| !indexed.entry.secret && reads.allows(&indexed.key))
            .filter_map(|indexed| {
                Some(Picture {
                    resource: indexed.entry.resource.clone(),
                    app: indexed.key.app.clone(),
                    workspace: indexed.key.workspace.clone(),
                    record: indexed.key.record.clone(),
                    image: indexed.entry.image.clone()?,
                })
            })
            .collect()
    }

    /// Add what `description` says its picture shows to the entry its app published for it, so
    /// one picture is one result: its own name and glyph stay, and it is found by those words as
    /// well. A picture whose app published no entry for it gets one of its own.
    ///
    /// # Errors
    /// `caller` may not read the record the picture is in.
    pub fn describe(&self, caller: &Caller, description: Description) -> Result<(), ActionCallError> {
        let Description {
            picture,
            title,
            words,
        } = description;
        let key = IndexKey::new(&picture.app, &picture.workspace, &picture.record);
        if !self.reads(caller).allows(&key) {
            return Err(ActionCallError::Refused {
                reason: "the picture is in a record this caller may not read".into(),
            });
        }
        let published = self.index.contents().visible.into_iter().find(|indexed| {
            indexed.key == key && indexed.entry.resource == picture.resource && !indexed.entry.secret
        });
        let entry = match published {
            Some(indexed) => {
                let mut entry = indexed.entry;
                for (text, weight) in words {
                    if !entry.fields.iter().any(|(held, _)| held == &text) {
                        entry.fields.push((text, weight));
                    }
                }
                entry.image = Some(picture.image);
                entry
            }
            None => IndexEntry {
                key: picture.resource.clone(),
                resource: picture.resource,
                title,
                fields: words,
                image: Some(picture.image),
                icon: Some("image".to_owned()),
                ..IndexEntry::default()
            },
        };
        self.index.apply(
            &key,
            IndexDelta {
                upsert: vec![entry],
                ..IndexDelta::default()
            },
        );
        Ok(())
    }

    /// What `caller` may read of the index.
    fn reads<'a>(&'a self, caller: &'a Caller) -> Reads<'a> {
        Reads {
            caller,
            may_read: &self.may_read,
            workspace: caller.workspace().filter(|_| !caller.acting().is_person()),
        }
    }
}

/// The device's index answers the workspace's search for every caller, as that caller may read
/// it: narrowed to a place, a stretch of time, an app or what entries are about, from what the
/// entries say, never by reading anything through.
impl ResourceSearch for IndexSearch {
    fn find(&self, caller: &Caller, needle: &str) -> Pending<Result<Vec<Found>, ActionCallError>> {
        Pending::ready(Ok(self.found(caller, needle, &Within::default())))
    }

    fn find_within(
        &self,
        caller: &Caller,
        needle: &str,
        within: &Within,
    ) -> Pending<Result<Vec<Found>, ActionCallError>> {
        Pending::ready(Ok(self.found(caller, needle, within)))
    }

    fn pictures(&self, caller: &Caller) -> Pending<Result<Vec<Picture>, ActionCallError>> {
        Pending::ready(Ok(Self::pictures(self, caller)))
    }

    fn describe(
        &self,
        caller: &Caller,
        description: Description,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Self::describe(self, caller, description))
    }
}

/// What one caller may read of the index.
struct Reads<'a> {
    caller: &'a Caller,
    may_read: &'a MayRead,
    /// The one workspace an agent searches: the person's search spans every workspace they see.
    workspace: Option<&'a str>,
}

impl Reads<'_> {
    fn allows(&self, key: &IndexKey) -> bool {
        self.workspace.is_none_or(|workspace| key.workspace == workspace)
            && (self.may_read)(self.caller, key)
    }
}

/// The index at one generation, ready to search.
struct Snapshot {
    generation: u64,
    visible: Vec<IndexedEntry>,
    index: SearchIndex,
    /// Locked secret-bearing entries, matched only to offer unlocking.
    locked: Vec<IndexedEntry>,
    locked_index: SearchIndex,
    /// What each resource is called now, by `(workspace, resource)`, with the key of the entry
    /// that named it: a result says who a message is from and in which conversation, without the
    /// record that holds the message knowing either.
    titles: HashMap<(String, String), Vec<(IndexKey, String)>>,
    /// What each person is called, by `(workspace, identity)`, with the key of the entry about
    /// them.
    names: HashMap<(String, String), (IndexKey, String)>,
}

impl Snapshot {
    fn new(generation: u64, contents: Contents) -> Self {
        let Contents { visible, locked } = contents;
        Self {
            generation,
            index: text_index(&visible),
            locked_index: text_index(&locked),
            titles: titles_of(&visible),
            names: names_of(&visible),
            visible,
            locked,
        }
    }

    /// The visible entries `needle` matches that `reads` allows, within `within`, best first: two
    /// workspaces' copies of one app's entry (a vault opened in each) are one result.
    fn matched(&self, reads: &Reads<'_>, needle: &str, within: &Within) -> Vec<Found> {
        let mut shown: HashSet<(&str, &str)> = HashSet::new();
        self.index
            .search(needle, Page::first(MAX_HITS))
            .hits
            .iter()
            .filter_map(|hit| {
                let indexed = usize::try_from(hit.key)
                    .ok()
                    .and_then(|at| self.visible.get(at))?;
                (holds(within, indexed)
                    && reads.allows(&indexed.key)
                    && shown.insert((indexed.key.app.as_str(), indexed.entry.key.as_str())))
                .then(|| self.found(reads, indexed, Some(needle), hit.rank.unwrap_or(0)))
            })
            .collect()
    }

    /// Every visible entry about what `within` names that `reads` allows, the most recent first.
    fn listed(&self, reads: &Reads<'_>, within: &Within) -> Vec<Found> {
        let mut listed: Vec<&IndexedEntry> = self
            .visible
            .iter()
            .filter(|indexed| holds(within, indexed) && reads.allows(&indexed.key))
            .collect();
        // The sort is stable, so the app's own order settles what is equal.
        listed.sort_by_key(|indexed| std::cmp::Reverse(indexed.entry.when.unwrap_or(i64::MIN)));
        listed
            .into_iter()
            .take(MAX_LISTED)
            .map(|indexed| self.found(reads, indexed, None, 0))
            .collect()
    }

    /// One result per app whose locked entries `needle` matches and `reads` allows, ranked by its
    /// best match: it names nothing of them, and opens the app to unlock them.
    fn unlocking(&self, reads: &Reads<'_>, needle: &str, app: Option<&str>) -> Vec<Found> {
        let mut best: Vec<(String, i32)> = Vec::new();
        for hit in self
            .locked_index
            .search(needle, Page::first(MAX_HITS))
            .hits
        {
            let Some(indexed) = usize::try_from(hit.key)
                .ok()
                .and_then(|at| self.locked.get(at))
            else {
                continue;
            };
            if app.is_some_and(|app| app != indexed.key.app) || !reads.allows(&indexed.key) {
                continue;
            }
            let rank = hit.rank.unwrap_or(0);
            match best.iter_mut().find(|(app, _)| *app == indexed.key.app) {
                Some((_, held)) => *held = (*held).max(rank),
                None => best.push((indexed.key.app.clone(), rank)),
            }
        }
        best.into_iter()
            .map(|(app, rank)| Found {
                resource: app.clone(),
                kind: "app".to_owned(),
                app: Some(app),
                rank: Some(rank),
                locked: true,
                ..Found::default()
            })
            .collect()
    }

    /// `indexed` as a caller reads the result: the action that opens it, and its people and place
    /// by what `reads` may read they are called now. A message found by `needle` reads as the line
    /// around what was typed rather than how it happens to begin.
    fn found(
        &self,
        reads: &Reads<'_>,
        indexed: &IndexedEntry,
        needle: Option<&str>,
        rank: i32,
    ) -> Found {
        let IndexedEntry { key, entry, .. } = indexed;
        let (resource, open, arguments) = opening(entry);
        let label = match (entry.kind, needle) {
            (Some(IndexKind::Message), Some(needle)) => tilo_search::excerpt(&entry.title, needle),
            _ => entry.title.clone(),
        };
        // Everyone the entry names, each by what they are called now where the caller may read
        // it, else with no name: who is in a conversation counts even unnamed.
        let people = entry
            .people
            .iter()
            .map(|person| IndexPerson {
                name: self.named(reads, key, person).unwrap_or_default(),
                ..person.clone()
            })
            .collect();
        Found {
            resource,
            kind: entry
                .kind
                .map_or("record", |kind| match kind {
                    IndexKind::Agent => "agent",
                    IndexKind::Conversation => "conversation",
                    IndexKind::Message => "message",
                    IndexKind::Person => "person",
                })
                .to_owned(),
            label,
            open: Some(open),
            arguments,
            app: Some(key.app.clone()),
            provider: Some(format!("record:{}:{}", key.app, key.record)),
            when: entry.when,
            workspace: Some(key.workspace.clone()),
            key: Some(format!(
                "{}{KEY_SEPARATOR}{}{KEY_SEPARATOR}{}{KEY_SEPARATOR}{}",
                key.app, key.workspace, key.record, entry.key
            )),
            subtitle: entry.subtitle.clone().or_else(|| entry.group.clone()),
            about: entry.kind,
            people,
            place: entry
                .place
                .as_ref()
                .and_then(|place| self.named(reads, key, place)),
            icon: entry.icon.clone(),
            rank: Some(rank),
            item: entry.opens_its_resource(),
            locked: false,
        }
    }

    /// What `person` is called: the name the record gave, else what the resource that holds them
    /// is called now, else the title of the entry about them, each only where `reads` may read
    /// the record that says it.
    fn named(&self, reads: &Reads<'_>, slot: &IndexKey, person: &IndexPerson) -> Option<String> {
        let name = person.name.trim();
        if !name.is_empty() {
            return Some(name.to_owned());
        }
        if !person.anchor.is_empty()
            && let Some(held) = self
                .titles
                .get(&(slot.workspace.clone(), person.anchor.clone()))
        {
            let readable = |(key, _): &&(IndexKey, String)| reads.allows(key);
            let named = held
                .iter()
                .filter(readable)
                .find(|(key, _)| key.app == slot.app)
                .or_else(|| held.iter().find(readable));
            if let Some((_, title)) = named {
                return Some(title.clone());
            }
        }
        self.names
            .get(&(slot.workspace.clone(), person.identity.clone()))
            .filter(|(key, _)| reads.allows(key))
            .map(|(_, name)| name.clone())
    }
}

/// Whether `indexed` is inside `within`: where it is placed, when it happened, the app that
/// published it and what it is about.
fn holds(within: &Within, indexed: &IndexedEntry) -> bool {
    let entry = &indexed.entry;
    let place = entry
        .place
        .as_ref()
        .map(|place| (place.anchor.as_str(), place.identity.as_str()));
    within.app.as_deref().is_none_or(|app| app == indexed.key.app)
        && (within.about.is_empty() || entry.kind.is_some_and(|kind| within.about.contains(&kind)))
        && within.holds(place, entry.when)
}

/// The action that opens `entry`, on which resource, told what: its own published action, or the
/// `open` of the resource holding the item it is, told the item as its anchor.
fn opening(entry: &IndexEntry) -> (String, String, serde_json::Map<String, serde_json::Value>) {
    let anchored = |resource: &str| {
        let mut arguments = serde_json::Map::new();
        arguments.insert(OPEN_ANCHOR.to_owned(), entry.resource.clone().into());
        (
            resource.to_owned(),
            APPLICATION_OPEN_ACTION.to_owned(),
            arguments,
        )
    };
    match &entry.action {
        Some(action)
            if action.action == APPLICATION_OPEN_ACTION && action.resource != entry.resource =>
        {
            anchored(&action.resource)
        }
        Some(action) => (
            action.resource.clone(),
            action.action.clone(),
            serde_json::Map::new(),
        ),
        None => anchored(&entry.resource),
    }
}

/// What each resource the entries name is called now, by `(workspace, resource)`: an entry's own
/// name of itself, for the results that show the people and places another entry holds.
fn titles_of(entries: &[IndexedEntry]) -> HashMap<(String, String), Vec<(IndexKey, String)>> {
    let mut titles: HashMap<(String, String), Vec<(IndexKey, String)>> = HashMap::new();
    for indexed in entries {
        titles
            .entry((
                indexed.key.workspace.clone(),
                indexed.entry.resource.clone(),
            ))
            .or_default()
            .push((indexed.key.clone(), indexed.entry.title.clone()));
    }
    titles
}

/// What each person the entries name is called, by `(workspace, identity)`: the title of the entry
/// about them.
fn names_of(entries: &[IndexedEntry]) -> HashMap<(String, String), (IndexKey, String)> {
    entries
        .iter()
        .filter(|indexed| indexed.entry.kind == Some(IndexKind::Person))
        .filter_map(|indexed| {
            let person = indexed.entry.people.first()?;
            Some((
                (indexed.key.workspace.clone(), person.identity.clone()),
                (indexed.key.clone(), indexed.entry.title.clone()),
            ))
        })
        .collect()
}

/// The entries' texts, each found by its position.
fn text_index(entries: &[IndexedEntry]) -> SearchIndex {
    // Names a person half remembers: words across fields and typos.
    let mut index = SearchIndex::new(Matching::LOOSE);
    index.replace_all(entries.iter().enumerate().map(|(at, indexed)| {
        // What the entry refers to counts as it is called now, never as the title written into
        // its text when the reference was made.
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
                document.field(text, field_weight(weight))
            })
    }));
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index apps' content");
    }
    index
}

/// The index's weight for a field an entry ranks as `weight`.
const fn field_weight(weight: IndexWeight) -> FieldWeight {
    match weight {
        IndexWeight::Title => FieldWeight::TITLE,
        IndexWeight::Identity => FieldWeight::IDENTITY,
        IndexWeight::Subtitle => FieldWeight::SUBTITLE,
        IndexWeight::Metadata => FieldWeight::METADATA,
        IndexWeight::Body => FieldWeight::BODY,
    }
}

#[cfg(test)]
mod tests;
