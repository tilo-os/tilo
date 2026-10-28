//! The Flathub catalogue kept on this device.
//!
//! The Store answers search and browsing from this copy, never from a
//! network request per keystroke. It holds every app Flathub lists, each
//! with the processors it is built for, read page by page from Flathub's
//! catalogue in the desktop's language, with Flathub's popular collection as
//! the featured order and each app's last update for "recently updated". It
//! is fetched in the background, kept on disk, and refetched once it is
//! older than [`FRESH_FOR_SECONDS`] or when the person asks for a refresh.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::flathub_client::{
    AppStreamRecord, fetch_catalogue_page, fetch_collection, locale_parameter,
};
use crate::model::StoreItem;

/// The file the catalogue is kept in under the Store's cache.
const FILE: &str = "flathub-v2.json";
/// The file earlier Stores kept it in: without the apps that have no build
/// for this device's processor, so it is never read, only removed.
const OLD_FILE: &str = "flathub-v1.json";
/// A kept catalogue larger than this is not read.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// How long a kept catalogue stands before it is fetched again.
pub const FRESH_FOR_SECONDS: u64 = 6 * 60 * 60;
/// Apps per catalogue page.
const PER_PAGE: usize = 250;
/// The most catalogue pages read: Flathub lists a few thousand apps, so this
/// bounds a server that answers an endless page count, not the catalogue.
const MAX_PAGES: usize = 64;
/// How many popular apps make up the featured order.
const FEATURED: usize = 100;

/// Flathub's catalogue as the Store keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlathubFeed {
    /// The language the localized fields are in, as Flathub names it.
    pub locale: String,
    /// When it was fetched, Unix seconds.
    pub fetched_unix: u64,
    /// Every app, featured ones first in Flathub's popular order, each with
    /// its featured rank and last update. Install state is not part of it:
    /// the refresh applies this device's.
    pub items: Vec<StoreItem>,
    /// Whether every catalogue page was read. A partial feed still lists
    /// what it has and is fetched again at the next refresh.
    pub complete: bool,
}

impl FlathubFeed {
    /// Whether the feed is recent enough, in `locale`, to stand without a
    /// fetch.
    #[must_use]
    pub fn is_fresh(&self, locale: &str, now_unix: u64) -> bool {
        self.complete
            && self.locale == locale_parameter(locale)
            && now_unix.saturating_sub(self.fetched_unix) < FRESH_FOR_SECONDS
    }

    /// Build the feed from Flathub's popular collection and its catalogue
    /// pages (either may be partial).
    #[must_use]
    pub fn from_records(
        locale: &str,
        popular: &[AppStreamRecord],
        catalogue: &[AppStreamRecord],
        complete: bool,
        now_unix: u64,
    ) -> Self {
        let mut seen = HashSet::new();
        let mut items = Vec::with_capacity(catalogue.len().max(popular.len()));
        for (rank, record) in popular.iter().enumerate() {
            if seen.insert(record.app_id.clone()) {
                let mut item = crate::sources::flathub_record_to_item(record, &[]);
                item.listing.featured_rank = u32::try_from(rank).ok();
                items.push(item);
            }
        }
        for record in catalogue {
            if seen.insert(record.app_id.clone()) {
                items.push(crate::sources::flathub_record_to_item(record, &[]));
            } else if let Some(item) = items.iter_mut().find(|item| item.id == record.app_id) {
                // The catalogue page carries fields a collection row may
                // not; keep the featured rank and fill what is missing.
                let fresh = crate::sources::flathub_record_to_item(record, &[]);
                if item.listing.updated_at_unix == 0 {
                    item.listing.updated_at_unix = fresh.listing.updated_at_unix;
                }
                if item.listing.developer.is_empty() {
                    item.listing.developer = fresh.listing.developer;
                }
                if item.listing.license.is_empty() {
                    item.listing.license = fresh.listing.license;
                }
            }
        }
        Self {
            locale: locale_parameter(locale),
            fetched_unix: now_unix,
            items,
            complete,
        }
    }
}

/// Fetch the whole catalogue from Flathub in `locale`. A failed catalogue
/// page after the first ends the read with what it has.
pub fn fetch(locale: &str, now_unix: u64) -> Result<FlathubFeed, String> {
    let popular = fetch_collection("popular", FEATURED, locale);
    let mut catalogue = Vec::new();
    let mut complete = true;
    let mut pages = 1;
    let mut page = 1;
    while page <= pages && page <= MAX_PAGES {
        match fetch_catalogue_page(page, PER_PAGE, locale) {
            Ok((records, total)) => {
                if records.is_empty() {
                    // The catalogue ended before the page count it claimed.
                    pages = page.saturating_sub(1);
                    break;
                }
                pages = total.max(1);
                catalogue.extend(records);
            }
            Err(error) => {
                tracing::warn!(%error, page, "could not read a Flathub catalogue page");
                complete = false;
                break;
            }
        }
        page += 1;
    }
    if page <= pages {
        complete = false;
    }
    let popular = match popular {
        Ok(popular) => popular,
        Err(error) if !catalogue.is_empty() => {
            tracing::warn!(%error, "could not read Flathub's popular apps");
            Vec::new()
        }
        Err(error) => return Err(error.to_string()),
    };
    Ok(FlathubFeed::from_records(
        locale, &popular, &catalogue, complete, now_unix,
    ))
}

fn path() -> Option<PathBuf> {
    crate::config::cache_dir().map(|dir| dir.join(FILE))
}

/// The kept catalogue, however old; `None` when there is none.
#[must_use]
pub fn load() -> Option<FlathubFeed> {
    let path = path()?;
    if std::fs::metadata(&path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    serde_json::from_slice(&std::fs::read(&path).ok()?).ok()
}

/// Keep `feed` for the next refresh and the next window.
pub fn save(feed: &FlathubFeed) -> Result<(), String> {
    let path = path().ok_or_else(|| "no cache directory".to_owned())?;
    let bytes = serde_json::to_vec(feed).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent(&path, &bytes)
        .map_err(|error| error.to_string())?;
    if let Some(old) = path.parent().map(|dir| dir.join(OLD_FILE)) {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

/// Flathub's apps from the `AppStream` metadata Flatpak keeps on this device,
/// read through the catalogue desktop search shares
/// ([`app_catalogue::appstream`]): no network, no popularity or dates, but
/// every app the configured Flathub remotes list. `None` when Flatpak keeps
/// none this process can read.
fn from_local_appstream(locale: &str, now_unix: u64) -> Option<FlathubFeed> {
    let items: Vec<StoreItem> = app_catalogue::appstream::load_cached(locale)
        .into_iter()
        .map(|entry| {
            let record = AppStreamRecord {
                app_id: entry.id,
                name: entry.name,
                summary: entry.tagline,
                description: entry.description,
                categories: entry
                    .category
                    .into_iter()
                    .map(|name| crate::flathub_client::CategoryRef { name })
                    .collect(),
                ..AppStreamRecord::default()
            };
            crate::sources::flathub_record_to_item(&record, &[])
        })
        .collect();
    (!items.is_empty()).then(|| FlathubFeed {
        locale: locale_parameter(locale),
        fetched_unix: now_unix,
        items,
        // Not Flathub's own answer: read Flathub again at the next refresh.
        complete: false,
    })
}

/// The catalogue a refresh uses: the kept one while it is fresh (and no
/// refresh was asked for), else a new fetch, kept for next time. A failed
/// fetch answers the kept one when there is one, else what Flatpak keeps on
/// this device.
pub fn load_or_fetch(locale: &str, force: bool) -> Result<FlathubFeed, String> {
    let now = crate::catalogue::now_unix();
    let kept = load();
    if !force && let Some(kept) = kept.as_ref().filter(|kept| kept.is_fresh(locale, now)) {
        return Ok(kept.clone());
    }
    match fetch(locale, now) {
        Ok(feed) => {
            if let Err(error) = save(&feed) {
                tracing::warn!(%error, "could not keep the Flathub catalogue");
            }
            Ok(feed)
        }
        Err(error) => kept
            .or_else(|| from_local_appstream(locale, now))
            .ok_or(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, updated: u64) -> AppStreamRecord {
        AppStreamRecord {
            app_id: id.into(),
            name: id.rsplit('.').next().unwrap_or(id).into(),
            summary: format!("{id} summary"),
            updated_at_unix: updated,
            ..AppStreamRecord::default()
        }
    }

    #[test]
    fn popular_apps_come_first_ranked_and_the_catalogue_fills_the_rest() {
        let feed = FlathubFeed::from_records(
            "de-DE",
            &[record("org.b.B", 0), record("org.a.A", 0)],
            &[record("org.a.A", 50), record("org.c.C", 70)],
            true,
            100,
        );
        let ids: Vec<&str> = feed.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["org.b.B", "org.a.A", "org.c.C"]);
        assert_eq!(feed.items[0].listing.featured_rank, Some(0));
        assert_eq!(feed.items[1].listing.featured_rank, Some(1));
        assert_eq!(
            feed.items[1].listing.updated_at_unix, 50,
            "a catalogue page fills what the collection row lacked"
        );
        assert_eq!(feed.items[2].listing.featured_rank, None);
        assert_eq!(feed.locale, "de_DE");
        assert!(feed.is_fresh("de-DE", 100 + FRESH_FOR_SECONDS - 1));
        assert!(!feed.is_fresh("de-DE", 100 + FRESH_FOR_SECONDS));
        assert!(
            !feed.is_fresh("en-US", 101),
            "another language is fetched again"
        );
    }
}
