//! The Store catalogue as search entries.
//!
//! The palette never receives an install operation as its activation: picking
//! an app opens its details, where provenance and the explicit primary action
//! remain visible. The worker builds the entries off the UI thread from the
//! complete authoritative catalogue, not the filtered rows a window shows;
//! the directory keeps the apps by id, so a hit's address resolves to the
//! app as it is now and an app that disappeared is refused rather than
//! navigating to a stale package identity.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};

use ui::tilo_resource::SearchEntry;

use crate::StoreItem;
use ui::lock::{MutexRecover as _, RwLockRecover as _};

#[derive(Default)]
struct DirectoryState {
    revision: Option<[u8; 32]>,
    /// The current catalogue as search entries. A replacement is completely
    /// built before this lock is held, then swapped with its apps in one
    /// short critical section.
    entries: Arc<Vec<SearchEntry>>,
    apps: BTreeMap<String, StoreItem>,
    registries: Vec<crate::config::RegistryConfig>,
}

/// The Store's complete catalogue directory: every app by id, and its search
/// entries. Replacing an unchanged worker snapshot is O(1).
#[derive(Clone, Default)]
pub struct StoreDirectory {
    state: Arc<RwLock<DirectoryState>>,
    /// Rebuilds may take linear time, and their publication order must match
    /// authoritative worker order. This intentionally does not guard reads.
    writer: Arc<Mutex<()>>,
}

impl StoreDirectory {
    /// Replace the directory after a worker snapshot.
    pub fn sync(&self, items: &[StoreItem]) {
        self.rebuild_serialized(items, || {});
    }

    /// Build a complete replacement without the read lock, then publish it,
    /// all under the writer lock so publications follow worker order.
    fn rebuild_serialized(&self, items: &[StoreItem], before_publish: impl FnOnce()) {
        let writer = self.writer.lock_recover();
        self.rebuild_with_writer(items, before_publish);
        drop(writer);
    }

    fn rebuild_with_writer(&self, items: &[StoreItem], before_publish: impl FnOnce()) {
        let revision = catalogue_revision(items);
        let state = self.state.read_recover();
        if state.revision == Some(revision) {
            return;
        }
        drop(state);

        let mut apps = BTreeMap::new();
        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            if apps.insert(item.id.clone(), item.clone()).is_some() {
                // A catalogue naming one app twice is malformed: withdraw the
                // directory rather than let agents act on either row.
                tracing::error!(app = %item.id, "withdrawing Store directory: an app is listed twice");
                *self.state.write_recover() = DirectoryState::default();
                return;
            }
            entries.push(app_entry(item));
        }

        before_publish();

        let mut state = self.state.write_recover();
        state.revision = Some(revision);
        state.entries = Arc::new(entries);
        state.apps = apps;
    }

    /// Keep host-resolved destinations alongside the catalogue.
    pub(crate) fn set_registries(&self, registries: Vec<crate::config::RegistryConfig>) {
        self.state.write_recover().registries = registries;
    }

    pub(crate) fn registry(&self, name: &str) -> Option<crate::config::RegistryConfig> {
        self.state
            .read_recover()
            .registries
            .iter()
            .find(|registry| registry.enabled && registry.name == name)
            .cloned()
    }

    /// Every app of the catalogue as a search entry.
    pub fn entries(&self) -> Vec<SearchEntry> {
        let entries = Arc::clone(&self.state.read_recover().entries);
        entries.as_ref().clone()
    }

    /// The catalogue app `id`, as the directory holds it now.
    pub fn app(&self, id: &str) -> Option<StoreItem> {
        self.state.read_recover().apps.get(id).cloned()
    }
}

/// `item` as a search hit opening its details.
fn app_entry(item: &StoreItem) -> SearchEntry {
    SearchEntry::new(item.name.clone(), searchable_text(item))
        .anchored(resource_id(item))
        .grouped("Apps")
        .subtitled(format!(
            "Version {2} from {0}. Trust: {1}.",
            item.source.short_label(),
            item.trust.label(),
            item.latest_version()
        ))
        .with_icon("layout-grid")
}

fn resource_id(item: &StoreItem) -> String {
    format!("app:{}", item.id)
}

/// What showing the search anchor `anchor` is: the details of the app it
/// names.
///
/// # Errors
/// Refuses an anchor that names no app.
pub(crate) fn opened(
    anchor: &str,
) -> Result<crate::resource::StoreEffect, ui::tilo_resource::Refusal> {
    anchor
        .strip_prefix("app:")
        .filter(|app| !app.is_empty())
        .map(|app| crate::resource::StoreEffect::ShowDetails {
            app: app.to_owned(),
        })
        .ok_or_else(|| ui::tilo_resource::Refusal::new(format!("`{anchor}` is no app")))
}

fn searchable_text(item: &StoreItem) -> String {
    let trust = item.trust.label();
    crate::model::searchable_fields(item)
        .map(|(text, _)| text)
        .chain([trust.as_str(), item.latest_version()])
        .collect::<Vec<_>>()
        .join(" ")
}

fn catalogue_revision(items: &[StoreItem]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for item in items {
        for (text, _) in crate::model::searchable_fields(item) {
            hasher.update(text.as_bytes());
            hasher.update(&[0]);
        }
        hasher.update(item.latest_version().as_bytes());
        hasher.update(&[0]);
        hasher.update(format!("{:?}{:?}{:?}", item.source, item.trust, item.installed).as_bytes());
        hasher.update(&[0xff]);
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstalledState, StoreItemVersion, StoreSource, StoreTrust};
    use std::sync::mpsc;
    use ui::search::SearchProvider;

    /// The app's search source over the directory, as the framework builds it.
    fn source(directory: &StoreDirectory) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<crate::reports::Reports>(directory.entries())
    }

    fn item(index: usize) -> StoreItem {
        StoreItem {
            bot: None,
            id: format!("dev.example.app{index}"),
            name: format!("Example App {index}"),
            description: "A durable Store test row".into(),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec!["Utilities".into()],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Tilo {
                registry_name: "main".into(),
            },
            trust: StoreTrust::PublisherSigned,
            versions: vec![StoreItemVersion {
                version: "1.0.0".into(),
                released_at_unix: 0,
                download_url: None,
                root_hash_hex: None,
                size_bytes: 0,
                runtime: None,
            }],
            publisher_pubkey_hex: None,
            installed: InstalledState::Not,
        }
    }

    /// The full catalogue pages, hits open details, and stale addresses are refused.
    #[test]
    fn catalogue_paging_and_stale_addresses_are_refused() {
        // Case `full_catalogue_is_cursor_paged_and_hits_open_details`.
        {
            let items = (0..2_048).map(item).collect::<Vec<_>>();
            let directory = StoreDirectory::default();
            directory.sync(&items);
            let provider = source(&directory);
            let first = provider.search_page(&ui::search::SearchQuery::new("example"), None, 9, 4);
            assert!(first.inspected <= 9);
            assert_eq!(first.results.len(), 4);
            assert!(first.next.is_some());
            let hit = &first.results[0];
            assert_eq!(hit.kind, "Apps");
            let anchor = hit.anchor();
            let crate::resource::StoreEffect::ShowDetails { app } = opened(&anchor).unwrap() else {
                panic!("a hit opens details");
            };
            let app = app.as_str();
            assert!(
                directory.app(app).is_some(),
                "the hit resolves in the directory"
            );
        }

        // Case `removed_catalogue_entry_refuses_stale_palette_address`.
        {
            let directory = StoreDirectory::default();
            directory.sync(&[item(7)]);
            let hit = source(&directory).search_limited("app 7", 1).pop().unwrap();
            let anchor = hit.anchor();
            directory.sync(&[]);
            let app = anchor.strip_prefix("app:").unwrap();
            assert!(directory.app(app).is_none());
            assert!(source(&directory).search_limited("app 7", 1).is_empty());
        }
    }

    #[test]
    fn configured_repository_is_searchable_through_the_resource_directory() {
        let mut remote_app = item(7);
        remote_app.source = StoreSource::FlatpakRemote {
            name: "tools".into(),
            title: "Example tools repository".into(),
            branch: "stable".into(),
            user_managed: true,
        };
        let directory = StoreDirectory::default();
        directory.sync(&[remote_app]);
        let hit = source(&directory)
            .search_limited("tools repository", 1)
            .pop()
            .expect("the configured repository's application is discoverable");
        assert_eq!(hit.anchor(), "app:dev.example.app7");
        assert_eq!(
            opened(&hit.anchor()).unwrap(),
            crate::resource::StoreEffect::ShowDetails {
                app: "dev.example.app7".into()
            }
        );
        assert!(directory.app("dev.example.app7").is_some());
    }

    #[test]
    fn publisher_metadata_refreshes_the_search_directory() {
        let mut app = item(7);
        let directory = StoreDirectory::default();
        directory.sync(&[app.clone()]);
        assert!(
            source(&directory)
                .search_limited("a1b2c3d4e5f6", 1)
                .is_empty()
        );
        app.publisher_pubkey_hex = Some(format!("{}a1b2", "a1b2c3d4e5f6".repeat(5)));
        directory.sync(&[app]);
        let hit = source(&directory)
            .search_limited("a1b2c3d4e5f6", 1)
            .pop()
            .unwrap();
        assert_eq!(hit.anchor(), "app:dev.example.app7");
    }

    /// Malformed rebuilds withdraw and older stages never publish after newer ones.
    #[test]
    fn bulk_rebuilds_withdraw_malformed_input_and_publish_in_order() {
        // Case `malformed_bulk_rebuild_withdraws_instead_of_partially_publishing`.
        {
            let directory = StoreDirectory::default();
            directory.sync(&[item(9)]);
            directory.sync(&[item(10), item(10)]);
            assert!(directory.entries().is_empty());
            assert!(directory.app("dev.example.app9").is_none());
        }

        // Case `serialized_writers_cannot_publish_an_older_stage_after_a_newer_one`.
        {
            let directory = StoreDirectory::default();
            directory.sync(&[item(1)]);
            let older_directory = directory.clone();
            let (older_staged, older_staged_receiver) = mpsc::channel();
            let (release_older, release_older_receiver) = mpsc::channel();
            let older = std::thread::spawn(move || {
                older_directory.rebuild_serialized(&[item(2)], || {
                    older_staged.send(()).expect("older stage signal");
                    release_older_receiver.recv().expect("older release");
                });
            });
            older_staged_receiver
                .recv()
                .expect("older stage pauses before publication");
            assert!(
                directory.writer.try_lock().is_err(),
                "the serial writer lock covers the complete rebuild lifecycle"
            );

            let newer_directory = directory.clone();
            let (newer_started, newer_started_receiver) = mpsc::channel();
            let newer = std::thread::spawn(move || {
                newer_started.send(()).expect("newer writer started");
                newer_directory.sync(&[item(3)]);
            });
            newer_started_receiver
                .recv()
                .expect("newer writer attempted publication");

            release_older.send(()).expect("release older publication");
            older.join().expect("older writer");
            newer.join().expect("newer writer");

            let provider = source(&directory);
            assert!(provider.search_limited("app 2", 10).is_empty());
            assert_eq!(provider.search_limited("app 3", 10).len(), 1);
        }
    }

    #[test]
    fn large_replacement_keeps_current_entries_and_resolution_before_publish() {
        let directory = StoreDirectory::default();
        directory.sync(&[item(1)]);
        let items = std::iter::once(item(1))
            .chain((2..20_000).map(item))
            .collect::<Vec<_>>();
        let worker_directory = directory.clone();
        let (staged, staged_receiver) = mpsc::channel();
        let (continue_publish, continue_receiver) = mpsc::channel();
        let replacement = std::thread::spawn(move || {
            worker_directory.rebuild_serialized(&items, || {
                staged.send(()).expect("stage signal");
                continue_receiver.recv().expect("publish release");
            });
        });

        staged_receiver
            .recv()
            .expect("large replacement is staged before publication");
        assert_eq!(source(&directory).search_limited("app 1", 1).len(), 1);
        assert!(directory.app("dev.example.app1").is_some());
        continue_publish.send(()).expect("release publication");
        replacement.join().expect("replacement worker");
        assert_eq!(directory.entries().len(), 19_999);
    }
}
