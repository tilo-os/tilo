//! The Store's one catalogue: Tilo apps and bots from the configured
//! registries, Flathub apps from Flathub's catalogue, and what this device
//! has installed, as one list of [`StoreItem`]s that each say their source.
//! An app on both Tilo and Flathub shows the Tilo one first.
//!
//! This layer is what the screens and the platform search read; it owns no
//! widget and does no network work on the UI thread:
//!
//! * [`cache`] keeps the merged catalogue on disk, so a new window paints
//!   real apps in its first frame and the background refresh only updates
//!   them in place;
//! * [`flathub`] keeps Flathub's whole catalogue on disk in the desktop's
//!   language, so search answers from this device per keystroke;
//! * [`details`] is what an app's page adds (localized description,
//!   screenshots, releases, size, licence, permissions), fetched by the
//!   [`fetcher`] thread when the page opens and kept on disk;
//! * blocking is the shared [`app_catalogue::BlockList`] desktop search
//!   hides apps by too: the person's own list and the device's and
//!   workspace's policies. Blocked apps are listed nowhere;
//! * [`Home`] and [`search`] decide what the screens list and in which order,
//!   keeping what is on screen in place when a refresh lands.

pub mod cache;
pub mod details;
pub mod fetcher;
pub mod flathub;

use std::collections::{BTreeMap, HashSet};

use ui::search::{FieldWeight, ListIndex};

pub use app_catalogue::{BlockList, Blocker};
pub use details::{AppDetails, Permission, Release};

use crate::model::{StoreItem, StoreSource};

/// The items of a list that nobody blocked, in their order.
pub trait Unblocked {
    fn unblocked<'a>(
        &'a self,
        items: impl IntoIterator<Item = &'a StoreItem> + 'a,
    ) -> impl Iterator<Item = &'a StoreItem> + 'a;
}

impl Unblocked for BlockList {
    fn unblocked<'a>(
        &'a self,
        items: impl IntoIterator<Item = &'a StoreItem> + 'a,
    ) -> impl Iterator<Item = &'a StoreItem> + 'a {
        items
            .into_iter()
            .filter(move |item| !self.is_blocked(&item.id))
    }
}

/// Whether the person may lift the block on `app`: only their own, and only
/// when nobody else blocked it too (a policy's block stays).
#[must_use]
pub fn person_can_unblock(blocks: &BlockList, app: &str) -> bool {
    let blockers = blocks.blockers(app);
    !blockers.is_empty() && blockers.iter().all(|by| **by == Blocker::Person)
}

/// How many apps a Home shelf shows.
pub const SHELF: usize = 12;

/// Now, in Unix seconds.
#[must_use]
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The desktop's language as the Store follows it (BCP 47): Flathub's
/// localized fields and kept details are in this language.
#[must_use]
pub fn desktop_locale() -> String {
    crate::resource::STORE_CATALOG.current_locale()
}

/// Whether `item` is a bot: bots have their own section and are never in an
/// app grid.
#[must_use]
pub fn is_bot(item: &StoreItem) -> bool {
    item.bot.is_some()
}

/// Whether `item` is something a person opens from the Store's lists: an
/// app, not a bot or a runtime component.
#[must_use]
pub fn is_listed_app(item: &StoreItem) -> bool {
    !is_bot(item) && !ui::registry::is_runtime_package_id(&item.id)
}

/// The source an app states on its row and page, in the person's words.
#[must_use]
pub fn source_label(item: &StoreItem) -> String {
    match &item.source {
        StoreSource::Tilo { .. } => ui::i18n!("store-source-tilo"),
        StoreSource::Flathub => ui::i18n!("store-source-flathub"),
        StoreSource::FlatpakRemote { title, .. } => title.clone(),
    }
}

/// The category's name in the person's words; a category no message names
/// keeps the name its source gives it.
#[must_use]
pub fn category_label(category: &str) -> String {
    let key = format!("store-category-{}", category.to_ascii_lowercase());
    if ui::i18n::I18nManager::has_message(&key) {
        ui::i18n::I18nManager::lookup_plain(&key)
    } else {
        category.to_owned()
    }
}

/// The order Tilo apps come before Flathub ones in: Tilo first, then
/// Flathub, then other repositories.
fn source_order(item: &StoreItem) -> u8 {
    match item.source {
        StoreSource::Tilo { .. } => 0,
        StoreSource::Flathub => 1,
        StoreSource::FlatpakRemote { .. } => 2,
    }
}

/// `ids` updated to `fresh` without moving what is on screen: ids still in
/// `fresh` keep their place, ids gone from it leave, and new ones join at
/// the end in `fresh`'s order.
#[must_use]
pub fn keep_order(ids: &[String], fresh: &[String]) -> Vec<String> {
    let wanted: HashSet<&str> = fresh.iter().map(String::as_str).collect();
    let mut kept: Vec<String> = ids
        .iter()
        .filter(|id| wanted.contains(id.as_str()))
        .cloned()
        .collect();
    let present: HashSet<String> = kept.iter().cloned().collect();
    kept.extend(fresh.iter().filter(|id| !present.contains(*id)).cloned());
    kept
}

/// `fresh` items in the order `previous` showed them: a refresh replaces
/// each item's content where it stands and adds new ones at the end, so
/// nothing on screen jumps.
#[must_use]
pub fn in_previous_order(previous: &[StoreItem], fresh: Vec<StoreItem>) -> Vec<StoreItem> {
    if previous.is_empty() {
        return fresh;
    }
    let order: Vec<String> = previous.iter().map(|item| item.id.clone()).collect();
    let fresh_ids: Vec<String> = fresh.iter().map(|item| item.id.clone()).collect();
    let mut by_id: BTreeMap<String, StoreItem> = fresh
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect();
    keep_order(&order, &fresh_ids)
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// What Home lists, by app id, in the order it first showed them. A refresh
/// [reconciles](Self::reconcile) it so nothing on screen moves; going Home
/// again computes it afresh.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Home {
    /// Installed apps a newer version is offered for.
    pub updates: Vec<String>,
    /// Tilo's own apps first, then Flathub's most popular.
    pub featured: Vec<String>,
    /// The apps that changed most recently in their source.
    pub recent: Vec<String>,
    /// Every category with apps in it, by name, with how many.
    pub categories: Vec<(String, usize)>,
}

impl Home {
    /// Home for `items`, without what `blocks` blocks.
    #[must_use]
    pub fn compute(items: &[StoreItem], blocks: &BlockList) -> Self {
        Self {
            updates: updates(items, blocks),
            featured: featured(items, blocks),
            recent: recent(items, blocks),
            categories: categories(items, blocks),
        }
    }

    /// Bring Home up to date with `items` without moving what it shows:
    /// apps that no longer qualify leave, new ones join at the end of their
    /// shelf.
    pub fn reconcile(&mut self, items: &[StoreItem], blocks: &BlockList) {
        let fresh = Self::compute(items, blocks);
        self.updates = keep_order(&self.updates, &fresh.updates);
        self.featured = keep_order(&self.featured, &fresh.featured);
        self.featured.truncate(SHELF);
        self.recent = keep_order(&self.recent, &fresh.recent);
        self.recent.truncate(SHELF);
        self.categories = fresh.categories;
    }

    /// Whether Home has anything to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.featured.is_empty() && self.recent.is_empty()
    }
}

/// Installed apps a newer version is offered for, Tilo first, by name.
#[must_use]
pub fn updates(items: &[StoreItem], blocks: &BlockList) -> Vec<String> {
    let mut found: Vec<&StoreItem> = blocks
        .unblocked(items)
        .filter(|item| is_listed_app(item) && item.installed.update_available())
        .collect();
    found.sort_by_key(|item| (source_order(item), item.name.to_lowercase()));
    found.into_iter().map(|item| item.id.clone()).collect()
}

/// Tilo's own apps (by name), then the apps Flathub features (by rank).
fn featured(items: &[StoreItem], blocks: &BlockList) -> Vec<String> {
    let mut tilo: Vec<&StoreItem> = blocks
        .unblocked(items)
        .filter(|item| is_listed_app(item) && matches!(item.source, StoreSource::Tilo { .. }))
        .filter(|item| !item.installed.is_installed())
        .collect();
    tilo.sort_by_key(|item| item.name.to_lowercase());
    let mut flathub: Vec<(&StoreItem, u32)> = blocks
        .unblocked(items)
        .filter(|item| is_listed_app(item) && !matches!(item.source, StoreSource::Tilo { .. }))
        .filter_map(|item| item.listing.featured_rank.map(|rank| (item, rank)))
        .collect();
    flathub.sort_by_key(|(_, rank)| *rank);
    tilo.into_iter()
        .chain(flathub.into_iter().map(|(item, _)| item))
        .take(SHELF)
        .map(|item| item.id.clone())
        .collect()
}

/// The apps that changed most recently in their source.
fn recent(items: &[StoreItem], blocks: &BlockList) -> Vec<String> {
    let mut found: Vec<&StoreItem> = blocks
        .unblocked(items)
        .filter(|item| is_listed_app(item) && item.listing.updated_at_unix > 0)
        .collect();
    found.sort_by_key(|item| {
        (
            std::cmp::Reverse(item.listing.updated_at_unix),
            source_order(item),
        )
    });
    found
        .into_iter()
        .take(SHELF)
        .map(|item| item.id.clone())
        .collect()
}

/// Every category with apps in it, by its label, with how many.
fn categories(items: &[StoreItem], blocks: &BlockList) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in blocks.unblocked(items).filter(|item| is_listed_app(item)) {
        for category in &item.categories {
            let category = category.trim();
            if !category.is_empty() {
                *counts.entry(category.to_owned()).or_default() += 1;
            }
        }
    }
    let mut categories: Vec<(String, usize)> = counts.into_iter().collect();
    categories.sort_by_key(|(category, _)| category_label(category).to_lowercase());
    categories
}

/// The apps in `category`: Tilo first, then by featured rank, then by name.
#[must_use]
pub fn in_category(items: &[StoreItem], blocks: &BlockList, category: &str) -> Vec<String> {
    let mut found: Vec<&StoreItem> = blocks
        .unblocked(items)
        .filter(|item| is_listed_app(item) && item.categories.iter().any(|c| c == category))
        .collect();
    found.sort_by_key(|item| {
        (
            source_order(item),
            item.listing.featured_rank.unwrap_or(u32::MAX),
            item.name.to_lowercase(),
        )
    });
    found.into_iter().map(|item| item.id.clone()).collect()
}

/// The bots of the catalogue, by name.
#[must_use]
pub fn bots(items: &[StoreItem], blocks: &BlockList) -> Vec<String> {
    let mut found: Vec<&StoreItem> = blocks
        .unblocked(items)
        .filter(|item| is_bot(item))
        .collect();
    found.sort_by_key(|item| item.name.to_lowercase());
    found.into_iter().map(|item| item.id.clone()).collect()
}

/// What a search inside the Store found, apps and bots apart, best first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub apps: Vec<String>,
    pub bots: Vec<String>,
}

impl SearchResults {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty() && self.bots.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.apps.len() + self.bots.len()
    }
}

/// The text a search matches an item by, with how much each field weighs:
/// its names, tag line, developer and categories outrank its description.
fn search_fields(item: &StoreItem) -> Vec<(&str, FieldWeight)> {
    crate::model::searchable_fields(item)
        .chain(std::iter::once((
            item.listing.developer.as_str(),
            FieldWeight::METADATA,
        )))
        .collect()
}

/// The rows `index` ranks for `items`: the same rows in the same order every
/// time, so the index built for them (ahead of the first keystroke, by
/// [`warm`]) answers each keystroke without indexing again.
fn search_rows(items: &[StoreItem]) -> impl Iterator<Item = Vec<(&str, FieldWeight)>> {
    items.iter().map(search_fields)
}

/// Build `index` for `items` now, so the first keystroke does not wait for
/// it. Run off the UI thread; the index serializes with a search that
/// arrives meanwhile.
pub fn warm(index: &ListIndex, items: &[StoreItem]) {
    let _ = index.ranks("tilo", search_rows(items));
}

/// Search `items` for `query` from `index`, on this device: apps and bots
/// apart, without what `blocks` blocks, best match first and a Tilo app
/// before an equally good Flathub one.
#[must_use]
pub fn search(
    index: &ListIndex,
    query: &str,
    items: &[StoreItem],
    blocks: &BlockList,
) -> SearchResults {
    let query = query.trim();
    if query.is_empty() {
        return SearchResults::default();
    }
    let ranks = index.ranks(query, search_rows(items));
    let mut hits: Vec<(&StoreItem, i32)> = items
        .iter()
        .zip(ranks)
        .filter_map(|(item, rank)| Some((item, rank?)))
        .filter(|(item, _)| !blocks.is_blocked(&item.id))
        .filter(|(item, _)| !ui::registry::is_runtime_package_id(&item.id))
        .collect();
    hits.sort_by_key(|(item, rank)| {
        (
            std::cmp::Reverse(*rank),
            source_order(item),
            std::cmp::Reverse(item.installed.is_installed()),
            item.name.to_lowercase(),
        )
    });
    let (bots, apps): (Vec<_>, Vec<_>) = hits.into_iter().partition(|(item, _)| is_bot(item));
    SearchResults {
        apps: apps.into_iter().map(|(item, _)| item.id.clone()).collect(),
        bots: bots.into_iter().map(|(item, _)| item.id.clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstalledState, Listing};

    fn app(id: &str, name: &str, source: StoreSource) -> StoreItem {
        let mut item = crate::fixture::fixture_catalog().remove(0);
        item.id = id.into();
        item.name = name.into();
        item.tagline = format!("{name} does one thing");
        item.description = String::new();
        item.categories = vec!["Office".into()];
        item.source = source;
        item.installed = InstalledState::Not;
        item.bot = None;
        item.listing = Listing::default();
        item
    }

    fn tilo(id: &str, name: &str) -> StoreItem {
        app(
            id,
            name,
            StoreSource::Tilo {
                registry_name: "Tilo main".into(),
            },
        )
    }

    #[test]
    fn a_refresh_keeps_what_is_on_screen_in_place() {
        let shown = vec!["b".to_owned(), "a".to_owned(), "c".to_owned()];
        let fresh = vec!["a".to_owned(), "d".to_owned(), "b".to_owned()];
        assert_eq!(keep_order(&shown, &fresh), ["b", "a", "d"]);
        let previous = vec![tilo("x.b", "B"), tilo("x.a", "A")];
        let mut updated = tilo("x.a", "A");
        updated.tagline = "changed".into();
        let merged =
            in_previous_order(&previous, vec![updated, tilo("x.c", "C"), tilo("x.b", "B")]);
        let ids: Vec<&str> = merged.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["x.b", "x.a", "x.c"]);
        assert_eq!(
            merged[1].tagline, "changed",
            "content updates where it stands"
        );
    }

    #[test]
    fn home_features_tilo_first_then_flathub_by_rank_and_hides_blocked_apps() {
        let mut popular = app("org.example.Popular", "Popular", StoreSource::Flathub);
        popular.listing.featured_rank = Some(0);
        popular.listing.updated_at_unix = 20;
        let mut second = app("org.example.Second", "Second", StoreSource::Flathub);
        second.listing.featured_rank = Some(1);
        second.listing.updated_at_unix = 30;
        let notes = tilo("so.tilo.notes", "Notes");
        let blocked = tilo("so.tilo.tube", "Tube");
        let items = vec![second, popular, notes, blocked];
        let blocks = BlockList::empty().with_workspace_policy("School", ["so.tilo.tube"]);
        let mut home = Home::compute(&items, &blocks);
        assert_eq!(
            home.featured,
            ["so.tilo.notes", "org.example.Popular", "org.example.Second"]
        );
        assert_eq!(home.recent, ["org.example.Second", "org.example.Popular"]);
        assert!(home.updates.is_empty(), "no shelf without real updates");
        // A refresh that changes the ranking keeps the shelf where it was.
        let mut items = items;
        items[0].listing.featured_rank = Some(0);
        items[1].listing.featured_rank = Some(5);
        home.reconcile(&items, &blocks);
        assert_eq!(
            home.featured,
            ["so.tilo.notes", "org.example.Popular", "org.example.Second"]
        );
    }

    // USER REQUIREMENT (2026-10-19): "apps can of course be blocked in the app store"
    /// Only the person's own block is theirs to lift; a policy's stays.
    #[test]
    fn the_person_unblocks_only_what_they_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let person = dir.path().join("blocked.json");
        let device = dir.path().join("store-policy.json");
        std::fs::write(
            &person,
            r#"{"blocked": ["com.example.Game", "com.example.Both"]}"#,
        )
        .unwrap();
        std::fs::write(
            &device,
            r#"{"admin": "IT at Example", "blocked": ["com.spotify.Client", "com.example.Both"]}"#,
        )
        .unwrap();
        let blocks = BlockList::load_from(Some(&person), Some(&device));
        assert!(person_can_unblock(&blocks, "com.example.Game"));
        assert!(!person_can_unblock(&blocks, "com.spotify.Client"));
        assert!(
            !person_can_unblock(&blocks, "com.example.Both"),
            "lifting the person's block would leave the administrator's"
        );
        assert!(
            !person_can_unblock(&blocks, "so.tilo.notes"),
            "nothing to lift"
        );
    }

    // USER REQUIREMENT (2026-10-19): "apps can of course be blocked in the app store"
    #[test]
    fn search_answers_locally_with_apps_and_bots_apart_and_tilo_first() {
        let mut bot = tilo("so.tilo.bot.notes-digest", "Notes digest");
        bot.bot = Some(crate::model::BotListing {
            version: "1.0.0".into(),
            triggers: vec!["schedule".into()],
            apps: vec!["so.tilo.notes".into()],
            connectors: Vec::new(),
            appearance: false,
            artifact: ui::registry::BotArtifact {
                program_url: "https://apps.tilo.so/blobs/00".into(),
                program_blake3_hex: "00".repeat(32),
                program_size_bytes: 10,
                requires: ui::registry::BotRequirements::default(),
            },
        });
        let items = vec![
            app("org.example.Notes", "Notes", StoreSource::Flathub),
            tilo("so.tilo.notes", "Notes"),
            bot,
            tilo("so.tilo.blocked-notes", "Notes blocked"),
        ];
        let blocks = BlockList::empty().with_workspace_policy("School", ["so.tilo.blocked-notes"]);
        let index = ListIndex::default();
        warm(&index, &items);
        let found = search(&index, "notes", &items, &blocks);
        assert_eq!(
            found.apps.first().map(String::as_str),
            Some("so.tilo.notes")
        );
        assert!(found.apps.iter().any(|id| id == "org.example.Notes"));
        assert!(!found.apps.iter().any(|id| id == "so.tilo.blocked-notes"));
        assert_eq!(found.bots, ["so.tilo.bot.notes-digest"]);
        assert!(search(&index, "  ", &items, &blocks).is_empty());
    }
}
