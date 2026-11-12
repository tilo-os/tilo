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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};
use ui::search::index_publication::{IndexEntry, IndexKind, IndexPerson, IndexWeight};
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind, provider_score};

use crate::content_index::{AppContentIndex, Contents, IndexedEntry, SlotKey};
use crate::model::{Hit, InvokeTarget, LaunchPlace, Source, Target};

pub use crate::model::{OPEN_ACTION, OPEN_ANCHOR};

/// Separates the parts of a row id: an entry's key may hold anything, so
/// it comes last.
const ID_SEPARATOR: char = '\u{1f}';
/// The id prefix of an app's unlock row.
const UNLOCK: &str = "unlock";

/// How many index hits one query reads.
const MAX_HITS: usize = 256;

/// How many recipients one search answers a box with: what a person reads
/// without scrolling.
const MAX_RECIPIENTS: usize = 12;

/// How well a recipient answers what was typed into a box, best last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Standing {
    /// The text matched something else it says.
    Text,
    /// A conversation the text matched.
    Conversation,
    /// A word of its name starts with what was typed.
    WordPrefix,
    /// Its name starts with what was typed.
    Prefix,
    /// It is called what was typed.
    Exact,
}

impl Standing {
    fn of(needle: &str, recipient: &ui::floating_surface::Recipient) -> Self {
        let needle = needle.to_lowercase();
        let name = recipient.name.to_lowercase();
        if needle.is_empty() {
            return Self::Text;
        }
        if name == needle {
            Self::Exact
        } else if name.starts_with(&needle) {
            Self::Prefix
        } else if name
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| word.starts_with(&needle))
        {
            Self::WordPrefix
        } else if recipient.kind == ui::floating_surface::RecipientKind::Conversation {
            Self::Conversation
        } else {
            Self::Text
        }
    }
}

/// The conversation a `chat.workspace/channels/<id>` row keeps.
fn conversation_id(resource: &str) -> Option<&str> {
    let id = resource.rsplit_once('/')?.1;
    (!id.is_empty()).then_some(id)
}

/// The index at one generation, ready to search.
pub struct Snapshot {
    pub generation: u64,
    pub visible: Vec<IndexedEntry>,
    pub index: SearchIndex,
    /// Locked secret-bearing entries, matched only to offer unlocking.
    locked: Vec<IndexedEntry>,
    locked_index: SearchIndex,
    /// `(app, workspace, resource)` of every visible entry.
    covered: HashSet<(String, String, String)>,
    /// The boost its app gave each visible entry that has one, by row id.
    boosts: HashMap<String, i32>,
    /// What each resource is called now, by `(workspace, resource)` and the
    /// app that named it: a row says who a message is from, and in which
    /// conversation, without the record that holds the message knowing
    /// either.
    titles: HashMap<(String, String), Vec<(String, String)>>,
    /// What each person is called, by `(workspace, identity)`: what the
    /// entry about them says, for a record that holds only their id.
    names: HashMap<(String, String), String>,
}

impl Snapshot {
    fn new(generation: u64, contents: Contents) -> Self {
        let Contents { visible, locked } = contents;
        let covered = visible
            .iter()
            .map(|indexed| {
                (
                    indexed.slot.app.clone(),
                    indexed.slot.workspace.clone(),
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
            titles: titles_of(&visible),
            names: names_of(&visible),
            visible,
            locked,
            covered,
            boosts,
        }
    }

    /// What the resource `resource` of `app` in `workspace` is called now,
    /// as the entry that opens it names it.
    fn title(&self, workspace: &str, app: &str, resource: &str) -> Option<&str> {
        let held = self
            .titles
            .get(&(workspace.to_owned(), resource.to_owned()))?;
        held.iter()
            .find(|(named_by, _)| named_by == app)
            .or_else(|| held.first())
            .map(|(_, title)| title.as_str())
    }
}

/// What each resource the entries name is called now, by `(workspace,
/// resource)`: an entry's own name of itself, for the rows that show the
/// people and places another entry holds.
fn titles_of(entries: &[IndexedEntry]) -> HashMap<(String, String), Vec<(String, String)>> {
    let mut titles: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
    for indexed in entries {
        titles
            .entry((
                indexed.slot.workspace.clone(),
                indexed.entry.resource.clone(),
            ))
            .or_default()
            .push((indexed.slot.app.clone(), indexed.entry.title.clone()));
    }
    titles
}

/// A person as a row's face stack draws them.
fn face(name: &str, person: &IndexPerson) -> ui::search::RowFace {
    ui::search::RowFace {
        name: name.to_owned(),
        identity: person.identity.clone(),
        colour: ui::presence::identity_colour(&person.identity),
        is_agent: person.agent,
        dimmed: false,
    }
}

/// What each person the entries name is called, by `(workspace, identity)`:
/// the title of the entry about them.
fn names_of(entries: &[IndexedEntry]) -> HashMap<(String, String), String> {
    entries
        .iter()
        .filter(|indexed| indexed.entry.kind == Some(IndexKind::Person))
        .filter_map(|indexed| {
            let person = indexed.entry.people.first()?;
            Some((
                (indexed.slot.workspace.clone(), person.identity.clone()),
                indexed.entry.title.clone(),
            ))
        })
        .collect()
}

/// The one line of `text` a row shows for `needle`: all of it when it is
/// short, else a window around what the person typed, so the match is in
/// the row rather than the start it happens to have.
fn excerpt(text: &str, needle: &str) -> String {
    /// How much of a long line a row shows.
    const SHOWN: usize = 120;
    /// How much of the window leads the match.
    const LEAD: usize = 40;
    let word: String = ui::search::fold_search_text(needle.trim())
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let chars: Vec<char> = text.chars().collect();
    if word.is_empty() || chars.len() <= SHOWN {
        return text.to_owned();
    }
    let folded: Vec<char> = ui::search::fold_search_text(text).chars().collect();
    // Folding keeps one char per char for the scripts a message is written
    // in; where it does not, the beginning is the line.
    let found = (folded.len() == chars.len())
        .then(|| {
            let word: Vec<char> = word.chars().collect();
            folded
                .windows(word.len())
                .position(|window| window == word.as_slice())
        })
        .flatten();
    let Some(found) = found else {
        return chars.iter().take(SHOWN).collect();
    };
    let start = found.saturating_sub(LEAD);
    let end = start.saturating_add(SHOWN).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push_str("\u{2026} ");
    }
    out.extend(chars.get(start..end).unwrap_or_default());
    if end < chars.len() {
        out.push_str(" \u{2026}");
    }
    out
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
                document.field(text, field_weight(weight))
            })
    }));
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index apps' content");
    }
    index
}

/// The index's weight for a field an entry ranks as `weight`.
fn field_weight(weight: IndexWeight) -> FieldWeight {
    match weight {
        IndexWeight::Title => FieldWeight::TITLE,
        IndexWeight::Identity => FieldWeight::IDENTITY,
        IndexWeight::Subtitle => FieldWeight::SUBTITLE,
        IndexWeight::Metadata => FieldWeight::METADATA,
        IndexWeight::Body => FieldWeight::BODY,
    }
}

/// A row's id: where its entry was published from, then its key.
#[must_use]
pub fn entry_id(slot: &SlotKey, key: &str) -> String {
    format!(
        "{}{ID_SEPARATOR}{}{ID_SEPARATOR}{}{ID_SEPARATOR}{key}",
        slot.app, slot.workspace, slot.record
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
    let workspace = parts.next()?;
    let record = parts.next()?;
    let key = parts.next()?;
    Some((
        SlotKey {
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            record: record.to_owned(),
        },
        key,
    ))
}

/// What running `entry` of `slot` invokes: the entry's own action, else
/// the app's `open` at the entry's resource.
#[must_use]
pub fn opening(slot: &SlotKey, entry: &IndexEntry) -> InvokeTarget {
    match &entry.action {
        Some(action) => {
            InvokeTarget::action(&slot.app, &slot.workspace, &action.resource, &action.action)
        }
        None => InvokeTarget::open(&slot.app, &slot.workspace, &entry.resource),
    }
}

/// What apps hold, from the session's index ([`AppContentIndex`]).
pub struct IndexedContent {
    index: Arc<AppContentIndex>,
    /// The snapshot built for the generation last seen: built on the frame
    /// or on a worker, whichever asked first, so it is shared rather than
    /// held by one thread.
    snapshot: Mutex<Option<Arc<Snapshot>>>,
    /// An app's name as the person knows it.
    app_name: Arc<dyn Fn(&str) -> String + Send + Sync>,
    /// The person searching, by their id: a row of what they keep says
    /// "you" rather than their name.
    person: String,
    /// The one app whose entries this source is over, when it is a window's
    /// search: what that app holds, and nothing of another's.
    app: Option<String>,
}

impl IndexedContent {
    /// A source over `index`, naming apps by `app_name` and the person
    /// searching by `person`.
    pub fn new(
        index: Arc<AppContentIndex>,
        app_name: impl Fn(&str) -> String + Send + Sync + 'static,
        person: impl Into<String>,
    ) -> Self {
        Self {
            index,
            snapshot: Mutex::new(None),
            app_name: Arc::new(app_name),
            person: person.into(),
            app: None,
        }
    }

    /// This source over what `app` alone published: what a window's search
    /// finds in the app it shows, whether or not the app is running.
    #[must_use]
    pub fn for_app(&self, app: &str) -> Self {
        Self {
            index: Arc::clone(&self.index),
            snapshot: Mutex::new(None),
            app_name: Arc::clone(&self.app_name),
            person: self.person.clone(),
            app: Some(app.to_owned()),
        }
    }

    /// What this source searches: every app's entries, or the one app's.
    fn contents(&self) -> Contents {
        let contents = self.index.contents();
        let Some(app) = self.app.as_deref() else {
            return contents;
        };
        let mine = |indexed: &&IndexedEntry| indexed.slot.app == app;
        Contents {
            visible: contents.visible.iter().filter(mine).cloned().collect(),
            locked: contents.locked.iter().filter(mine).cloned().collect(),
        }
    }

    /// The index this source searches.
    #[must_use]
    pub fn index(&self) -> &Arc<AppContentIndex> {
        &self.index
    }

    /// The snapshot of the index as it is now, built if it moved.
    pub fn current(&self) -> Arc<Snapshot> {
        let generation = self.index.generation();
        let mut slot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_ref() {
            Some(snapshot) if snapshot.generation == generation => Arc::clone(snapshot),
            _ => {
                // Read the generation before the contents: a change landing
                // between them leaves this snapshot stale, never stamped
                // current.
                let snapshot = Arc::new(Snapshot::new(generation, self.contents()));
                *slot = Some(Arc::clone(&snapshot));
                snapshot
            }
        }
    }

    /// Whether this source finds `resource` of `app` in `workspace`, whether or
    /// not the app runs: one resource is one row, so a window's own row for
    /// it gives way.
    #[must_use]
    pub fn covers(&self, app: &str, workspace: &str, resource: &str) -> bool {
        self.current().covered.contains(&(
            app.to_owned(),
            workspace.to_owned(),
            resource.to_owned(),
        ))
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

    /// The row for one visible entry, scored `score`, found by `needle`: a
    /// message shows the line around what the person typed rather than how
    /// it happens to begin.
    #[must_use]
    pub fn row(&self, indexed: &IndexedEntry, score: i32, needle: &str) -> Hit {
        let IndexedEntry { slot, entry, .. } = indexed;
        let subtitle = entry.subtitle.clone().or_else(|| match entry.kind {
            Some(_) => self.under_name(slot, entry),
            None => entry
                .group
                .clone()
                .or_else(|| Some((self.app_name)(&slot.app))),
        });
        let (avatar, faces) = self.shown(slot, entry);
        let title = match entry.kind {
            Some(IndexKind::Message) => excerpt(&entry.title, needle),
            _ => entry.title.clone(),
        };
        // What the entry is, when its app says something the generic record
        // row would read poorly: its own section, its own glyph, its people
        // at the front and beside it.
        let drawn = (!faces.is_empty() || avatar.is_some()).then(|| {
            ui::search::SearchRow::Command(Box::new(ui::search::CommandRow {
                avatar,
                faces,
                ..ui::search::CommandRow::default()
            }))
        });
        Hit {
            row: drawn.or_else(|| {
                entry
                    .action
                    .is_none()
                    .then(|| ui::search::SearchRow::item_of(slot.app.clone(), &entry.resource))
                    .flatten()
            }),
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: Self::kind(entry),
            provider: self.id().into(),
            id: entry_id(slot, &entry.key),
            title,
            subtitle,
            facts: Vec::new(),
            score,
            icon: entry.icon.as_deref().and_then(ui::icons::Icon::from_name),
            target: Target::Invoke(Box::new(opening(slot, entry))),
        }
    }

    /// The section an entry belongs to: what it is, when its app says, else
    /// the records an app keeps.
    fn kind(entry: &IndexEntry) -> ResourceKind {
        match entry.kind {
            Some(IndexKind::Agent) => ResourceKind::Custom(crate::i18n::t("search-kind-agents")),
            Some(IndexKind::Conversation) => {
                ResourceKind::Custom(crate::i18n::t("search-kind-conversations"))
            }
            Some(IndexKind::Message) => {
                ResourceKind::Custom(crate::i18n::t("search-kind-messages"))
            }
            Some(IndexKind::Person) => ResourceKind::Custom(crate::i18n::t("search-kind-people")),
            None => ResourceKind::Record,
        }
    }

    /// The avatar and the faces of the row an entry draws: an agent or a
    /// message shows its person in front, a conversation shows the people in
    /// it beside its name.
    fn shown(
        &self,
        slot: &SlotKey,
        entry: &IndexEntry,
    ) -> (Option<(String, String)>, Vec<ui::search::RowFace>) {
        let people: Vec<(String, &IndexPerson)> = entry
            .people
            .iter()
            .filter_map(|person| self.named(slot, person).map(|name| (name, person)))
            .collect();
        match entry.kind {
            Some(IndexKind::Agent | IndexKind::Message | IndexKind::Person) => (
                people
                    .first()
                    .map(|(name, person)| (name.clone(), person.identity.clone())),
                Vec::new(),
            ),
            Some(IndexKind::Conversation) => (
                None,
                people
                    .iter()
                    .map(|(name, person)| face(name, person))
                    .collect(),
            ),
            None => (None, Vec::new()),
        }
    }

    /// What `person` is called: the name the record gave, else what the
    /// resource that holds them is called now.
    fn named(&self, slot: &SlotKey, person: &IndexPerson) -> Option<String> {
        let name = person.name.trim();
        if !name.is_empty() {
            return Some(name.to_owned());
        }
        let snapshot = self.current();
        if !person.anchor.is_empty()
            && let Some(title) = snapshot.title(&slot.workspace, &slot.app, &person.anchor)
        {
            return Some(title.to_owned());
        }
        snapshot
            .names
            .get(&(slot.workspace.clone(), person.identity.clone()))
            .cloned()
    }

    /// What the entry says under its name when its app left it unsaid: who
    /// keeps an agent, and who a message is from, where, and how long ago.
    fn under_name(&self, slot: &SlotKey, entry: &IndexEntry) -> Option<String> {
        match entry.kind {
            Some(IndexKind::Agent) => {
                let keeper = entry.keeper.as_ref()?;
                if !self.person.is_empty() && keeper.identity == self.person {
                    return Some(crate::i18n::t("search-row-agent-yours"));
                }
                self.named(slot, keeper).map(|name| {
                    crate::i18n::t_args("search-row-agent-kept-by", &[("keeper", name.into())])
                })
            }
            Some(IndexKind::Conversation) => {
                let names: Vec<String> = entry
                    .people
                    .iter()
                    .filter_map(|person| self.named(slot, person))
                    .collect();
                (!names.is_empty()).then(|| names.join(", "))
            }
            Some(IndexKind::Message) => {
                let sender = entry
                    .people
                    .first()
                    .and_then(|person| self.named(slot, person));
                let place = entry
                    .place
                    .as_ref()
                    .and_then(|person| self.named(slot, person));
                let age = entry.when.map(|when| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |elapsed| {
                            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
                        });
                    ui::format::relative(now.saturating_sub(when))
                });
                match (sender, place, age) {
                    (Some(sender), Some(place), Some(age)) => Some(crate::i18n::t_args(
                        "search-row-message",
                        &[
                            ("sender", sender.into()),
                            ("conversation", place.into()),
                            ("age", age.into()),
                        ],
                    )),
                    (None, Some(place), Some(age)) => Some(crate::i18n::t_args(
                        "search-row-message-unnamed",
                        &[("conversation", place.into()), ("age", age.into())],
                    )),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// One row offering to unlock `app`, for the `needle` its locked
    /// entries match.
    fn unlock_row(&self, app: &str, needle: &str, score: i32) -> Hit {
        let name = (self.app_name)(app);
        Hit {
            // The row says what was typed back, in quotes: nothing in it is a match.
            row: Some(ui::search::SearchRow::echoing_query()),
            preview: None,
            faces: Vec::new(),
            outcome: None,
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
                place: LaunchPlace::Focus,
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

    /// The agents, people and conversations `needle` names, as a box that
    /// sends a message lists whom it could go to: found by the same index the
    /// palette reads. What is called exactly what was typed comes first, then
    /// what a name starts with it, then the conversations it matches, the
    /// most recent first, and then the rest by the index's own ranking. An
    /// empty `needle` lists what the workspace holds, the most recent first,
    /// which is what a box opens on before anything is typed.
    #[must_use]
    pub fn recipients(&self, needle: &str) -> Vec<ui::floating_surface::Recipient> {
        let snapshot = self.current();
        let needle = needle.trim();
        let matches: Vec<(i32, &IndexedEntry)> = if needle.is_empty() {
            snapshot
                .visible
                .iter()
                .map(|indexed| (0, indexed))
                .collect()
        } else {
            snapshot
                .index
                .search(needle, Page::first(MAX_HITS))
                .hits
                .iter()
                .filter_map(|hit| {
                    let indexed = usize::try_from(hit.key)
                        .ok()
                        .and_then(|at| snapshot.visible.get(at))?;
                    Some((hit.score, indexed))
                })
                .collect()
        };
        let mut found: Vec<(Standing, i32, i64, ui::floating_surface::Recipient)> = matches
            .into_iter()
            .filter_map(|(score, indexed)| {
                let recipient = self.recipient(indexed)?;
                let when = indexed.entry.when.unwrap_or(i64::MIN);
                let standing = Standing::of(needle, &recipient);
                // Among conversations the most recent leads, whatever the text scored.
                let score = if standing == Standing::Conversation {
                    0
                } else {
                    score
                };
                Some((standing, score, when, recipient))
            })
            .collect();
        // The sort is stable, so the app's own order settles what is equal.
        found.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then(right.1.cmp(&left.1))
                .then(right.2.cmp(&left.2))
        });
        let mut seen: HashSet<(ui::floating_surface::RecipientKind, String)> = HashSet::new();
        let mut out: Vec<ui::floating_surface::Recipient> = Vec::new();
        for (_, _, _, recipient) in found {
            if seen.insert((recipient.kind, recipient.id.clone())) {
                out.push(recipient);
            }
        }
        out.truncate(MAX_RECIPIENTS);
        out
    }

    /// The recipient `indexed` names, when it names one a message can go to: an
    /// agent the workspace keeps, a person, or a conversation. A person
    /// sitting at this desk is no recipient of their own message.
    fn recipient(&self, indexed: &IndexedEntry) -> Option<ui::floating_surface::Recipient> {
        use ui::floating_surface::{Recipient, RecipientKind};
        let entry = &indexed.entry;
        let who = entry.people.first();
        let recipient = match entry.kind? {
            IndexKind::Agent => {
                let agent = who?.identity.strip_prefix("agent:")?;
                let kept = entry.keeper.as_ref().map_or_else(String::new, |keeper| {
                    if keeper.identity == self.person {
                        crate::i18n::t("search-kept-by-you")
                    } else {
                        keeper.name.clone()
                    }
                });
                Recipient::found(RecipientKind::Agent, agent, &entry.title).with_detail(kept)
            }
            IndexKind::Person => {
                let person = who?;
                // The identity as the app that keeps the person spells it,
                // without the prefix the index adds to say what it is.
                let id = person
                    .identity
                    .strip_prefix("agent:")
                    .or_else(|| person.identity.strip_prefix("person:"))
                    .unwrap_or(&person.identity);
                if id == self.person {
                    return None;
                }
                let kind = if person.agent {
                    RecipientKind::Agent
                } else {
                    RecipientKind::Person
                };
                Recipient::found(kind, id, &entry.title)
                    .with_detail(entry.subtitle.clone().unwrap_or_default())
            }
            IndexKind::Conversation => {
                let id = conversation_id(&entry.resource)?;
                let people: Vec<&str> = entry
                    .people
                    .iter()
                    .map(|person| person.name.as_str())
                    .filter(|name| !name.is_empty())
                    .collect();
                Recipient::found(RecipientKind::Conversation, id, &entry.title).with_detail(
                    if people.is_empty() {
                        entry.subtitle.clone().unwrap_or_default()
                    } else {
                        people.join(", ")
                    },
                )
            }
            IndexKind::Message => return None,
        };
        Some(recipient)
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
            .map(|indexed| self.row(&indexed, MAX_PROVIDER_SCORE, needle))
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
        // Two workspaces' copies of one app's entry (a vault opened in each) are
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
                        self.row(indexed, score, needle)
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
            ..IndexEntry::default()
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
                    "workspace",
                    app,
                    "tab:0001",
                    &delta.encode().expect("a delta encodes"),
                )
                .expect("the index takes the delta");
        }
    }

    /// A window's search is over the one app it shows: what that app published, never another's.
    #[test]
    fn a_window_reads_only_its_own_apps_entries() {
        let index = Arc::new(AppContentIndex::in_memory());
        publish(
            &index,
            "so.tilo.chat",
            None,
            vec![note("c_voice", "voice", false)],
        );
        publish(
            &index,
            "so.tilo.mail",
            None,
            vec![note("inbox", "Inbox", false)],
        );
        let content = IndexedContent::new(Arc::clone(&index), str::to_owned, "");
        let window = content.for_app("so.tilo.chat");
        assert_eq!(window.query("voice").len(), 1);
        assert!(
            window.query("inbox").is_empty(),
            "another app's entry is not the window's"
        );
        assert_eq!(
            content.query("inbox").len(),
            1,
            "the whole index still finds it"
        );
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
        let content = IndexedContent::new(Arc::clone(&index), |app| format!("{app}!"), "");
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
        assert!(content.covers("so.tilo.notes", "workspace", "n1"));
        assert_eq!(
            content
                .resolve(&hits[0].id, "")
                .map(|hit| hit.title)
                .as_deref(),
            Some("Groceries")
        );
    }

    /// An entry an app publishes as an item of a resource (a note in a
    /// notebook) is a reference: the row says which item it is, so the
    /// palette can draw it as the app lists it, and it opens there. An entry
    /// that names no item is drawn as the generic row.
    #[test]
    fn an_entry_that_names_an_item_carries_its_reference() {
        let index = Arc::new(AppContentIndex::in_memory());
        let mut page = note("page-7", "Trip plan", false);
        page.resource = "notes.notebook/pages/page-7".into();
        publish(&index, "so.tilo.notes", None, vec![page.clone()]);
        let content = IndexedContent::new(index, |_| String::new(), "");
        let hits = content.query("trip");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].row,
            Some(ui::search::SearchRow::item(
                "so.tilo.notes",
                "notes.notebook.pages",
                "page-7"
            ))
        );
        // The row addresses the resource the hit opens.
        assert_eq!(
            hits[0]
                .row
                .as_ref()
                .and_then(ui::search::SearchRow::item_resource)
                .as_deref(),
            Some(page.resource.as_str())
        );
    }

    /// A person as an entry names them.
    fn person(name: &str, identity: &str, anchor: &str, agent: bool) -> IndexPerson {
        IndexPerson {
            name: name.into(),
            anchor: anchor.into(),
            identity: identity.into(),
            agent,
        }
    }

    /// `entry` as what an app says it is: its kind, the people it is about,
    /// who keeps it, where it is, and when.
    fn shaped(entry: IndexEntry, kind: IndexKind, people: Vec<IndexPerson>) -> IndexEntry {
        IndexEntry {
            kind: Some(kind),
            people,
            ..entry
        }
    }

    #[test]
    fn what_an_entry_is_names_the_section_it_sits_in() {
        let index = Arc::new(AppContentIndex::in_memory());
        publish(
            &index,
            "so.tilo.chat",
            None,
            vec![
                shaped(
                    note("chat.agent/agents/a_ada", "Ada", false),
                    IndexKind::Agent,
                    vec![person(
                        "Ada",
                        "agent:a_ada",
                        "chat.agent/agents/a_ada",
                        true,
                    )],
                ),
                shaped(
                    note("chat.workspace/channels/c_crit", "Ada's design crit", false),
                    IndexKind::Conversation,
                    vec![person("Ada", "agent:a_ada", "", true)],
                ),
                shaped(
                    note("chat.workspace/messages/m_1", "Ada, can you look?", false),
                    IndexKind::Message,
                    vec![person("Ada", "agent:a_ada", "", true)],
                ),
            ],
        );
        let content = IndexedContent::new(index, str::to_owned, "");
        let kinds: Vec<ResourceKind> = content
            .query("ada")
            .into_iter()
            .map(|hit| hit.kind)
            .collect();
        let labels: Vec<String> = kinds
            .iter()
            .map(|kind| match kind {
                ResourceKind::Custom(label) => label.clone(),
                other => other.label().to_owned(),
            })
            .collect();
        assert_eq!(labels, ["Agents", "Conversations", "Messages"]);
        // The groups keep that order whatever their strongest hit.
        let mut sorted = kinds.clone();
        sorted.sort_by_key(ResourceKind::sort_key);
        assert_eq!(
            sorted.first().map(ResourceKind::sort_key),
            kinds.first().map(ResourceKind::sort_key)
        );
    }

    #[test]
    fn an_agent_reads_as_the_person_it_is_kept_by() {
        let index = Arc::new(AppContentIndex::in_memory());
        let mut agent = shaped(
            note("chat.agent/agents/a_ada", "Ada", false),
            IndexKind::Agent,
            vec![person(
                "Ada",
                "agent:a_ada",
                "chat.agent/agents/a_ada",
                true,
            )],
        );
        agent.icon = Some("sparkles".into());
        agent.keeper = Some(person("", "u_me", "", false));
        publish(&index, "so.tilo.chat", None, vec![agent]);
        let content = IndexedContent::new(index, str::to_owned, "u_me");
        let hits = content.query("ada");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subtitle.as_deref(), Some("Kept by you"));
        let Some(ui::search::SearchRow::Command(row)) = &hits[0].row else {
            panic!("an agent is drawn with its own face: {:?}", hits[0].row);
        };
        assert_eq!(
            row.avatar,
            Some(("Ada".to_owned(), "agent:a_ada".to_owned()))
        );

        let other =
            IndexedContent::new(Arc::clone(content.index()), str::to_owned, "u_someone_else");
        // The keeper `u_me` is not this person: the row says who keeps it.
        let mut kept = shaped(
            note("chat.agent/agents/a_ada", "Ada", false),
            IndexKind::Agent,
            vec![person("Ada", "agent:a_ada", "", true)],
        );
        kept.keeper = Some(person("Sam", "u_me", "", false));
        publish(content.index(), "so.tilo.chat", None, vec![kept]);
        let hits = other.query("ada");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subtitle.as_deref(), Some("Kept by Sam"));
    }

    #[test]
    fn a_conversation_lists_the_people_in_it() {
        let index = Arc::new(AppContentIndex::in_memory());
        publish(
            &index,
            "so.tilo.chat",
            None,
            vec![shaped(
                note("chat.workspace/channels/c_crit", "Ada's design crit", false),
                IndexKind::Conversation,
                vec![
                    person("Aria", "person:u_aria", "", false),
                    person("Ada", "agent:a_ada", "", true),
                ],
            )],
        );
        let content = IndexedContent::new(index, str::to_owned, "u_me");
        let hits = content.query("crit");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subtitle.as_deref(), Some("Aria, Ada"));
        let Some(ui::search::SearchRow::Command(row)) = &hits[0].row else {
            panic!("a conversation shows its people: {:?}", hits[0].row);
        };
        let names: Vec<&str> = row.faces.iter().map(|face| face.name.as_str()).collect();
        assert_eq!(names, ["Aria", "Ada"]);
        assert!(row.faces[1].is_agent);
        assert_eq!(row.avatar, None);
    }

    /// A box's recipients are what the workspace's apps publish of their
    /// agents, people and conversations: never a message, never the person
    /// asking, each once — and an empty query opens on what is there, the
    /// most recent first.
    #[test]
    fn a_box_finds_agents_people_and_conversations_never_messages_or_itself() {
        use ui::floating_surface::RecipientKind;

        let index = Arc::new(AppContentIndex::in_memory());
        let mut agent = shaped(
            note("chat.agent/agents/a_ada", "Ada", false),
            IndexKind::Agent,
            vec![person(
                "Ada",
                "agent:a_ada",
                "chat.agent/agents/a_ada",
                true,
            )],
        );
        agent.keeper = Some(person("Me", "u_me", "", false));
        agent.when = Some(20);
        let mut conversation = shaped(
            note("chat.workspace/channels/c_ada", "Ada", false),
            IndexKind::Conversation,
            vec![
                person("Me", "person:u_me", "", false),
                person("Ada", "agent:a_ada", "", true),
            ],
        );
        conversation.when = Some(30);
        publish(
            &index,
            "so.tilo.chat",
            None,
            vec![
                agent,
                conversation,
                shaped(
                    note("chat.workspace/people/u_me", "Me", false),
                    IndexKind::Person,
                    vec![person(
                        "Me",
                        "person:u_me",
                        "chat.workspace/people/u_me",
                        false,
                    )],
                ),
                shaped(
                    note("chat.workspace/people/u_ada", "Ada", false),
                    IndexKind::Person,
                    vec![person(
                        "Ada",
                        "person:u_ada",
                        "chat.workspace/people/u_ada",
                        false,
                    )],
                ),
                shaped(
                    note("chat.workspace/messages/m_ada", "Ada, can you look?", false),
                    IndexKind::Message,
                    vec![person("Ada", "person:u_ada", "", false)],
                ),
            ],
        );
        let content = IndexedContent::new(index, str::to_owned, "u_me");

        // Typing a name: what it names, never the message that mentions it
        // and never the person typing.
        let found = content.recipients("ada");
        let listed: Vec<(RecipientKind, &str)> = found
            .iter()
            .map(|recipient| (recipient.kind, recipient.id.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                (RecipientKind::Conversation, "c_ada"),
                (RecipientKind::Agent, "a_ada"),
                (RecipientKind::Person, "u_ada"),
            ],
            "the conversation is the most recent, and nobody is listed twice"
        );
        assert_eq!(
            found[1].detail.as_deref(),
            Some("you"),
            "the agent says this person keeps it"
        );

        // Before anything is typed, what the workspace holds, most recent
        // first.
        let held = content.recipients("  ");
        let all: Vec<(RecipientKind, &str)> = held
            .iter()
            .map(|recipient| (recipient.kind, recipient.id.as_str()))
            .collect();
        assert_eq!(
            all,
            [
                (RecipientKind::Conversation, "c_ada"),
                (RecipientKind::Agent, "a_ada"),
                (RecipientKind::Person, "u_ada"),
            ]
        );
    }

    /// Of a workspace's 55 agents, the one called what was typed leads, then
    /// those whose name starts with it, then those with a word that does; a
    /// conversation that matches some other way comes before an agent that
    /// does, the most recent first.
    #[test]
    fn a_box_finds_an_agent_by_name_among_many() {
        use ui::floating_surface::{Recipient, RecipientKind};

        let index = Arc::new(AppContentIndex::in_memory());
        let mut entries = Vec::new();
        let mut agent = |id: &str, name: &str, when: i64| {
            let mut entry = shaped(
                note(&format!("chat.agent/agents/{id}"), name, false),
                IndexKind::Agent,
                vec![person(name, &format!("agent:{id}"), "", true)],
            );
            entry.when = Some(when);
            entries.push(entry);
        };
        for number in 0..52 {
            agent(
                &format!("a_{number}"),
                &format!("Helper {number}"),
                100 + i64::from(number),
            );
        }
        agent("a_weekly", "Weekly digest", 1);
        agent("a_digester", "Digester", 2);
        agent("a_digest", "Digest", 3);
        publish(&index, "so.tilo.chat", None, entries);
        let content = IndexedContent::new(index, str::to_owned, "u_me");

        let ids = |found: Vec<Recipient>| -> Vec<String> {
            found.into_iter().map(|recipient| recipient.id).collect()
        };
        assert_eq!(
            ids(content.recipients("Digest"))[..3],
            ["a_digest", "a_digester", "a_weekly"]
        );
        assert_eq!(
            ids(content.recipients("helper 7"))
                .first()
                .map(String::as_str),
            Some("a_7")
        );
        assert_eq!(ids(content.recipients("helper 4"))[0], "a_4");

        let conversation = |name: &str| Recipient::found(RecipientKind::Conversation, "c", name);
        let named = |name: &str| Recipient::found(RecipientKind::Agent, "a", name);
        assert!(
            Standing::of("ada", &conversation("Design crit"))
                > Standing::of("ada", &named("Reviewer"))
        );
        assert!(
            Standing::of("ada", &named("Ada's helper"))
                > Standing::of("ada", &conversation("Crit"))
        );
        assert!(Standing::of("ada", &named("Ada")) > Standing::of("ada", &named("Adam")));
    }

    #[test]
    fn a_message_says_who_said_it_where_and_when() {
        let index = Arc::new(AppContentIndex::in_memory());
        let text = "We shipped the new palette yesterday and the animations still stutter \
                    when the window resizes; Ada, can you look at the wheel again before \
                    the demo tomorrow morning?";
        let mut message = shaped(
            note("chat.workspace/messages/m_wheel", text, false),
            IndexKind::Message,
            vec![person(
                "",
                "person:u_aria",
                "chat.workspace/people/u_aria",
                false,
            )],
        );
        message.place = Some(person(
            "",
            "c_crit",
            "chat.workspace/channels/c_crit",
            false,
        ));
        message.when = Some(
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("the clock is after the epoch")
                    .as_secs(),
            )
            .expect("seconds fit")
                - 3600,
        );
        publish(
            &index,
            "so.tilo.chat",
            None,
            vec![
                shaped(
                    note("chat.workspace/people/u_aria", "Aria", false),
                    IndexKind::Person,
                    vec![person("Aria", "person:u_aria", "", false)],
                ),
                shaped(
                    note("chat.workspace/channels/c_crit", "Ada's design crit", false),
                    IndexKind::Conversation,
                    vec![person("Aria", "person:u_aria", "", false)],
                ),
                message,
            ],
        );
        let content = IndexedContent::new(index, str::to_owned, "u_me");
        let hits = content.query("ada");
        let hit = hits
            .iter()
            .find(|hit| hit.title.contains("can you look"))
            .expect("the message is found by what it says");
        assert_eq!(
            hit.subtitle.as_deref(),
            Some(format!("Aria in Ada's design crit, {}", ui::format::relative(3600)).as_str())
        );
        assert!(
            hit.title.starts_with('\u{2026}'),
            "a long message shows the line around the match: {:?}",
            hit.title
        );
        let Some(ui::search::SearchRow::Command(row)) = &hit.row else {
            panic!("a message is drawn with its sender's face: {:?}", hit.row);
        };
        assert_eq!(
            row.avatar,
            Some(("Aria".to_owned(), "person:u_aria".to_owned()))
        );
        let Target::Invoke(target) = &hit.target else {
            panic!("a message opens where it lives");
        };
        assert_eq!(target.address.resource, "chat.workspace/messages/m_wheel");
        assert_eq!(
            target.arguments[OPEN_ANCHOR],
            "chat.workspace/messages/m_wheel"
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
        let content = IndexedContent::new(index, str::to_owned, "");
        let hits = content.query("github");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].title,
            "Unlock so.tilo.passwords to search for “github”"
        );
        assert_eq!(
            hits[0].target,
            Target::Launch {
                app: "so.tilo.passwords".into(),
                place: LaunchPlace::Focus,
            }
        );
    }
}
