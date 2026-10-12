//! What the file explorer finds in the unified Ctrl+K palette: the user's
//! curated **Favourites / Places**, the collaborative records of the mount
//! and the remote file accounts, each a search entry the explorer surface
//! contributes to the app's one search source. A pinned location is
//! findable by its label or path; activating a row navigates there.
//!
//! Only stable, in-memory sets are searchable — never the live directory
//! contents (which are not a bounded, stable library) nor the transient
//! back/forward history. Reading the entries touches no filesystem, so it
//! stays synchronous.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use sha2::{Digest as _, Sha256};

use ui::tilo_actions::Resource;
use ui::tilo_resource::SearchEntry;

use crate::favorites::Favorite;
use crate::model::WorkspaceRecord;
use crate::remote_accounts::RemoteAccount;

pub const OPEN_ACTION: &str = "open";
pub const REVEAL_ACTION: &str = "reveal";
pub const REMOTE_ACCOUNT_OPEN_ACTION: &str = "open-remote-account";

/// Stable resource identity for a local place. The digest keeps both wire
/// separators *and the local path itself* out of resource ids: action ids can
/// be retained by search, history and agents, while a host path is only valid
/// in this process's live presentation snapshot.
pub fn place_resource_id(path: &std::path::Path) -> String {
    format!("files.place:{}", path_identity_suffix(path))
}

/// A path is an opaque filesystem name, not display text. Resource ids cross
/// the action door, so use a domain-separated digest rather than a reversible
/// escaping scheme. Keep this shared by folders and entries so every local
/// Files resource has the same privacy boundary.
fn path_identity_suffix(path: &std::path::Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut digest = Sha256::new();
    digest.update(b"tilo.files.local-resource.v1\\0");
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    // 128 bits makes collisions infeasible for a local, bounded directory
    // while retaining a compact action address.
    let digest = digest.finalize();
    hex::encode(digest.get(..16).unwrap_or(&digest))
}

/// Stable, wire-safe identity for a currently presented directory.  The path
/// is deliberately the identity here: ordinary host folders do not have a
/// Tilo record UUID, while collaborative records use [`record_resource_id`].
pub fn folder_resource_id(path: &std::path::Path) -> String {
    format!("files.folder:{}", path_identity_suffix(path))
}

/// Stable, wire-safe identity for an entry in the currently presented folder.
/// Collaborative resources additionally publish their record UUID identity,
/// which survives rename and move; this address is the local filesystem view.
pub fn entry_resource_id(path: &std::path::Path) -> String {
    format!("files.entry:{}", path_identity_suffix(path))
}

pub fn record_resource_id(record_id: uuid::Uuid) -> String {
    format!("files.record:{record_id}")
}

/// A revision-gated snapshot of the private Places list, the search entries
/// and rows it offers. The widget changes the revision only after a
/// favourite mutation, so a search merely reads the snapshot instead of
/// re-walking paths.
#[derive(Clone)]
pub struct PlacesSearch {
    snapshot: Arc<RwLock<PlacesSnapshot>>,
    kind: String,
}

#[derive(Clone, Debug)]
struct PlacesSnapshot {
    revision: Option<u64>,
    favorites: BTreeMap<String, Favorite>,
}

impl PlacesSearch {
    /// Create a snapshot of `favorites`.
    pub fn new(favorites: &[Favorite]) -> Self {
        let provider = Self {
            snapshot: Arc::new(RwLock::new(PlacesSnapshot {
                revision: None,
                favorites: BTreeMap::new(),
            })),
            kind: crate::i18n::t("files-a11y-places"),
        };
        provider.refresh(0, favorites);
        provider
    }

    /// Reconcile only when the owner has observed a new source revision.
    /// Supplying the same revision is intentionally a no-op: it must not turn
    /// a steady-state `sync` into a full traversal.
    pub fn refresh(&self, revision: u64, favorites: &[Favorite]) -> bool {
        {
            let snapshot = self
                .snapshot
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if snapshot.revision == Some(revision) {
                return false;
            }
        }
        let current = favorites
            .iter()
            .map(|favorite| (place_resource_id(&favorite.path), favorite.clone()))
            .collect();
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.favorites = current;
        snapshot.revision = Some(revision);
        true
    }

    pub(crate) fn resource(&self, id: &str) -> Option<Resource> {
        let favorite = self.snapshot.read().ok()?.favorites.get(id)?.clone();
        Some(
            Resource::new(id, "files.place", favorite.label).with_action(
                crate::effects::row_action(
                    &crate::effects::FilesEffect::OpenItem {
                        item: id.to_owned(),
                    },
                    OPEN_ACTION,
                    "Open",
                ),
            ),
        )
    }

    /// Every place as a search entry: found by its label or path, opening it.
    pub(crate) fn entries(&self) -> Vec<SearchEntry> {
        let Ok(snapshot) = self.snapshot.read() else {
            return Vec::new();
        };
        snapshot
            .favorites
            .values()
            .map(|favorite| {
                SearchEntry::new(favorite.label.clone(), String::new())
                    .anchored(place_resource_id(&favorite.path))
                    .grouped(self.kind.clone())
                    .subtitled(favorite.path.display().to_string())
                    .with_icon(favorite.icon().name())
            })
            .collect()
    }
}

/// Complete search over the mount's ACL-filtered room record catalog. Record
/// UUIDs, rather than paths, are action identity: paths are mutable provenance
/// used only for matching and presentation.
#[derive(Clone)]
pub struct CollaborativeRecordSearch {
    snapshot: Arc<RwLock<RecordSnapshot>>,
    kind: String,
}

#[derive(Clone, Debug, Default)]
struct RecordSnapshot {
    revision: Option<u64>,
    records: BTreeMap<String, WorkspaceRecord>,
}

impl CollaborativeRecordSearch {
    pub(crate) fn new(entries: &[WorkspaceRecord]) -> Self {
        let kind = crate::i18n::t("files-search-collaborative-records");
        let provider = Self {
            snapshot: Arc::new(RwLock::new(RecordSnapshot::default())),
            kind,
        };
        provider.refresh(0, entries);
        provider
    }

    pub(crate) fn refresh(&self, revision: u64, entries: &[WorkspaceRecord]) -> bool {
        {
            let snapshot = self
                .snapshot
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if snapshot.revision == Some(revision) {
                return false;
            }
        }
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.records.clear();
        snapshot.revision = Some(revision);
        drop(snapshot);
        self.append_page(revision, entries);
        true
    }

    /// Add one already-authorised mount page without revisiting earlier pages.
    /// A source reset uses [`Self::refresh`] first; a mismatched revision is
    /// refused rather than merging two authority generations.
    pub(crate) fn append_page(&self, revision: u64, entries: &[WorkspaceRecord]) -> bool {
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if snapshot.revision != Some(revision) {
            return false;
        }
        for entry in entries {
            snapshot
                .records
                .insert(record_resource_id(entry.record_id), entry.clone());
        }
        true
    }

    /// Every collaborative record as a search entry: found by its name or
    /// path, revealing it by its stable record id.
    pub(crate) fn entries(&self) -> Vec<SearchEntry> {
        let Ok(snapshot) = self.snapshot.read() else {
            return Vec::new();
        };
        snapshot
            .records
            .values()
            .map(|entry| {
                SearchEntry::new(entry.name.clone(), String::new())
                    .anchored(record_resource_id(entry.record_id))
                    .grouped(self.kind.clone())
                    .subtitled(crate::i18n::t_args(
                        "files-search-record-provenance",
                        &[
                            crate::i18n::arg("path", entry.path.display().to_string()),
                            crate::i18n::arg("record", entry.record_id.to_string()),
                        ],
                    ))
                    .with_icon(ui::icons::icon_for_path(&entry.path, entry.is_dir).name())
            })
            .collect()
    }

    pub(crate) fn record(&self, record_id: uuid::Uuid) -> Option<WorkspaceRecord> {
        self.snapshot
            .read()
            .ok()?
            .records
            .get(&record_resource_id(record_id))
            .cloned()
    }

    pub(crate) fn resource(&self, id: &str) -> Option<Resource> {
        let record = self.snapshot.read().ok()?.records.get(id)?.clone();
        Some(Resource::new(id, "files.record", record.name).with_action(
            crate::effects::row_action(
                &crate::effects::FilesEffect::Reveal {
                    record: record.record_id.to_string(),
                },
                REVEAL_ACTION,
                "Reveal in Files",
            ),
        ))
    }
}

/// Searchable snapshot of the canonical, non-secret file-provider accounts.
/// This is deliberately a small metadata projection, not a remote directory
/// crawler: the mount daemon owns remote I/O and Files must remain responsive
/// and offline-safe while the palette is open.
#[derive(Clone)]
pub struct RemoteAccountSearch {
    snapshot: Arc<RwLock<RemoteAccountsSnapshot>>,
    kind: String,
}

#[derive(Clone, Debug)]
struct RemoteAccountsSnapshot {
    revision: Option<u64>,
    accounts: BTreeMap<String, RemoteAccount>,
}

impl RemoteAccountSearch {
    pub fn new(accounts: &[RemoteAccount]) -> Self {
        let provider = Self {
            snapshot: Arc::new(RwLock::new(RemoteAccountsSnapshot {
                revision: None,
                accounts: BTreeMap::new(),
            })),
            kind: "Remote file accounts".into(),
        };
        provider.refresh(0, accounts);
        provider
    }

    pub fn refresh(&self, revision: u64, accounts: &[RemoteAccount]) -> bool {
        {
            let snapshot = self
                .snapshot
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if snapshot.revision == Some(revision) {
                return false;
            }
        }
        let current = accounts
            .iter()
            .map(|account| {
                (
                    crate::remote_accounts::resource_id(&account.id),
                    account.clone(),
                )
            })
            .collect();
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.accounts = current;
        snapshot.revision = Some(revision);
        true
    }

    pub(crate) fn resource(&self, id: &str) -> Option<Resource> {
        let account = self.snapshot.read().ok()?.accounts.get(id)?.clone();
        Some(
            Resource::new(id, "files.remote-account", account.label).with_action(
                crate::effects::row_action(
                    &crate::effects::FilesEffect::AccountDetails {
                        account: account.id.to_string(),
                    },
                    REMOTE_ACCOUNT_OPEN_ACTION,
                    "Show connection details",
                ),
            ),
        )
    }
}

impl RemoteAccountSearch {
    /// Every remote account as a search entry: found by its label, host or
    /// kind, opening its connection details.
    pub(crate) fn entries(&self) -> Vec<SearchEntry> {
        let Ok(snapshot) = self.snapshot.read() else {
            return Vec::new();
        };
        snapshot
            .accounts
            .values()
            .map(|account| {
                SearchEntry::new(account.label.clone(), account.kind.label())
                    .anchored(crate::remote_accounts::resource_id(&account.id))
                    .grouped(self.kind.clone())
                    .subtitled(format!(
                        "{} on {} ({})",
                        account.kind.label(),
                        account.host,
                        account.health.label()
                    ))
                    .with_icon(
                        ui::icons::icon_for_path(std::path::Path::new("remote-folder"), true)
                            .name(),
                    )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::favorites::{FavKind, Favorite};
    use std::path::PathBuf;
    use ui::search::SearchProvider;
    use ui::tilo_actions::ActionAddress;

    /// The app's search source over `entries`, as the framework builds it.
    fn source(entries: Vec<SearchEntry>) -> impl SearchProvider {
        ui::framework::testing::search_source::<crate::resource::FileExplorer>(entries)
    }
    use ui::search::SearchRegistry;
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};

    fn places() -> Vec<Favorite> {
        vec![
            Favorite {
                label: "Home".into(),
                path: "/home/tester".into(),
                is_home: true,
                kind: FavKind::Home,
            },
            Favorite {
                label: "Documents".into(),
                path: "/home/tester/Documents".into(),
                is_home: false,
                kind: FavKind::Documents,
            },
            Favorite {
                label: "Projects".into(),
                path: "/home/tester/code/projects".into(),
                is_home: false,
                kind: FavKind::Folder,
            },
        ]
    }

    fn collaborative_entry(name: &str) -> WorkspaceRecord {
        WorkspaceRecord {
            record_id: uuid::Uuid::nil(),
            name: name.into(),
            path: PathBuf::from(name),
            is_dir: false,
        }
    }

    /// Places and remote accounts are found by label, path, host and kind with wire-safe, round-tripping action ids that navigate.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn places_search_finds_and_addresses_destinations() {
        // --- phase: provider_finds_a_place_by_label_or_path ---
        {
            let values = places();
            let provider = source(PlacesSearch::new(&values).entries());
            assert!(
                provider
                    .search("projects")
                    .iter()
                    .any(|r| r.title == "Projects")
            );
            // A path-only match (the "code" segment isn't in any label) still hits.
            assert!(
                provider
                    .search("code")
                    .iter()
                    .any(|r| r.title == "Projects")
            );
            assert_eq!(provider.search("").len(), 3);
        }

        // --- phase: action_id_round_trips_to_the_destination_path ---
        {
            let mut registry = SearchRegistry::new();
            let values = places();
            registry.register(Box::new(source(PlacesSearch::new(&values).entries())));
            let hit = registry
                .query("documents")
                .into_iter()
                .find(|r| r.title == "Documents")
                .expect("Documents is findable");
            assert_eq!(
                hit.action_id,
                place_resource_id(PathBuf::from("/home/tester/Documents").as_path()),
                "a hit is anchored at its place's row"
            );
        }

        // --- phase: remote_accounts_are_searchable_by_label_host_and_kind ---
        {
            let account = RemoteAccount {
                id: accounts::AccountId::from("cloud"),
                kind: crate::remote_accounts::RemoteAccountKind::Nextcloud,
                label: "Personal cloud".into(),
                host: "cloud.example".into(),
                health: crate::remote_accounts::RemoteAccountHealth::NeedsAuthentication,
                mount: None,
            };
            let accounts = vec![account.clone()];
            let provider = source(RemoteAccountSearch::new(&accounts).entries());
            for query in ["personal", "example", "nextcloud"] {
                assert_eq!(provider.search(query).len(), 1, "{query}");
            }
            let anchor = provider.search("personal")[0].action_id.clone();
            assert_eq!(anchor, crate::remote_accounts::resource_id(&account.id));
            assert_eq!(
                crate::remote_accounts::account_of_resource(&anchor).as_deref(),
                Some("cloud")
            );
        }

        // --- phase: place_resource_ids_are_wire_safe ---
        {
            let address = ActionAddress::new(
                place_resource_id(PathBuf::from("/tmp/a\u{1f}b").as_path()),
                OPEN_ACTION,
            );
            assert_eq!(ActionAddress::parse(&address.encode()), Some(address));
        }

        // --- phase: folder_and_entry_resource_ids_are_wire_safe_and_kind_scoped ---
        {
            let path = PathBuf::from("/tmp/a\u{1f}b");
            let folder = ActionAddress::new(folder_resource_id(&path), "refresh");
            let entry = ActionAddress::new(entry_resource_id(&path), "open");
            assert_eq!(ActionAddress::parse(&folder.encode()), Some(folder));
            assert_eq!(ActionAddress::parse(&entry.encode()), Some(entry));
            assert_ne!(folder_resource_id(&path), entry_resource_id(&path));
            for id in [
                place_resource_id(&path),
                folder_resource_id(&path),
                entry_resource_id(&path),
            ] {
                assert!(
                    !id.contains("/tmp") && !id.contains("2f746d70"),
                    "resource ids must not retain either the path or its old reversible hex: {id}"
                );
                assert_eq!(
                    id.rsplit(':').next().map(str::len),
                    Some(32),
                    "resource ids use a compact 128-bit opaque digest"
                );
            }
        }

        // --- phase: widget_hooks_find_then_navigate_to_the_place ---
        {
            use automerge::AutoCommit;

            // Build the app against real temp dirs so navigation actually loads.
            let base = std::env::temp_dir().join(format!("tilo-fe-search-{}", std::process::id()));
            let home = base.join("home");
            let sub = home.join("sub");
            let cfg = base.join("cfg");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::create_dir_all(&cfg).unwrap();

            let mut app = crate::files_app::FileExplorerWidget::with_persistence(
                Some(&cfg),
                Some(home.clone()),
            );
            // Move away from Home so activation demonstrably navigates back.
            app.navigate(sub.clone());
            assert_eq!(app.current_path(), sub);

            let hit = source(app.search_entries_shown())
                .search("")
                .into_iter()
                .find(|r| r.action_id == place_resource_id(&home))
                .expect("the Home favourite is findable through the real hook");

            let doc = AutoCommit::new();
            let resources = crate::files_app::published::tree(&app, &doc);
            let row = resources
                .iter()
                .flat_map(|resource| std::iter::once(resource).chain(&resource.children))
                .find(|resource| resource.id.as_str() == hit.action_id)
                .expect("the hit's anchor is a published row");
            let spec = row
                .actions
                .iter()
                .find(|spec| spec.id == OPEN_ACTION)
                .expect("the place's row opens it");
            assert_eq!(spec.kind, ui::tilo_actions::OperationKind::View);
            assert_eq!(spec.contract, ui::tilo_actions::ActionContract::view());
            app.open_anchor(&hit.anchor())
                .expect("the window opens the hit's anchor");
            assert_eq!(
                app.current_path(),
                home,
                "activation navigated to the chosen place"
            );

            std::fs::remove_dir_all(&base).ok();
        }
    }

    /// Retained catalogues gate unchanged revisions, page with true cursors and hard budgets, and find suffix hits with a fixed budget.
    #[test]
    fn retained_catalogues_page_within_budgets() {
        // --- phase: places_snapshot_gates_unchanged_revisions ---
        {
            let values = places();
            let places = PlacesSearch::new(&values);
            let before = places.entries();
            let changed = vec![Favorite {
                label: "Late place".into(),
                path: "/home/tester/late-needle".into(),
                is_home: false,
                kind: FavKind::Folder,
            }];
            assert!(
                !places.refresh(0, &changed),
                "the owner must advance the source revision before any traversal"
            );
            assert_eq!(places.entries(), before);
            assert!(source(places.entries()).search("late-needle").is_empty());
            assert!(places.refresh(1, &changed));
            assert_eq!(source(places.entries()).search("late-needle").len(), 1);
        }

        // --- phase: every_retained_provider_supports_true_cursor_pages_and_lazy_action_equivalence ---
        {
            let accounts: Vec<_> = (0..20)
                .map(|index| RemoteAccount {
                    id: accounts::AccountId::from(format!("account-{index}")),
                    kind: crate::remote_accounts::RemoteAccountKind::WebDav,
                    label: format!("Shared record {index}"),
                    host: format!("host-{index}.example"),
                    health: crate::remote_accounts::RemoteAccountHealth::Healthy,
                    mount: None,
                })
                .collect();
            let remote = RemoteAccountSearch::new(&accounts);
            let provider = source(remote.entries());
            let first = provider.search_page(&ui::search::SearchQuery::new("record"), None, 3, 3);
            assert_eq!(first.inspected, 3);
            assert_eq!(first.results.len(), 3);
            let second = provider.search_page(
                &ui::search::SearchQuery::new("record"),
                first.next.as_ref(),
                3,
                3,
            );
            assert_eq!(second.inspected, 3);
            assert!(
                second
                    .results
                    .iter()
                    .all(|hit| !first.results.contains(hit))
            );

            let hit = first.results.first().expect("a page has a result");
            let resource = remote
                .resource(&hit.anchor())
                .expect("the omitted account is materialized exactly");
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|spec| spec.id == REMOTE_ACCOUNT_OPEN_ACTION),
                "the anchored row opens the account"
            );
        }
    }

    /// Collaborative results use stable record identity, are deduplicated and complete, and page to late hits.
    #[test]
    fn collaborative_search_uses_stable_record_identity() {
        // --- phase: collaborative_results_use_stable_record_identity_and_path_as_provenance ---
        {
            let record_id = uuid::Uuid::from_u128(0x51);
            let mut entry = collaborative_entry("old-name.txt");
            entry.path = PathBuf::from("/Workspace/old-name.txt");
            entry.record_id = record_id;
            let provider = source(CollaborativeRecordSearch::new(&[entry]).entries());
            let hit = provider.search("workspace").pop().expect("path matches");
            assert_eq!(hit.action_id, record_resource_id(record_id));
            assert!(hit.subtitle.unwrap().contains("/Workspace/old-name.txt"));
        }

        // --- phase: collaborative_projection_is_deduplicated_and_complete_beyond_the_old_cap ---
        {
            const RECORD_COUNT: usize = 276;
            let entries: Vec<_> = (0..RECORD_COUNT)
                .flat_map(|index| {
                    let id = uuid::Uuid::from_u128(index as u128 + 1);
                    let mut first = collaborative_entry(&format!("file-{index}"));
                    first.record_id = id;
                    let duplicate = first.clone();
                    [first, duplicate]
                })
                .collect();
            let provider = source(CollaborativeRecordSearch::new(&entries).entries());
            // Result presentation is intentionally capped, but the index itself
            // is complete: a match beyond the former 256-record snapshot limit is
            // still discoverable.
            assert_eq!(provider.search("file-275").len(), 1);
        }

        // --- phase: collaborative_search_pages_a_late_hit_in_a_large_record_catalog ---
        {
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let entries: Vec<_> = corpus
                        .map(|row| {
                            let name = if row.is_target {
                                format!("{}-文書", row.text)
                            } else {
                                row.text
                            };
                            let mut entry = collaborative_entry(&name);
                            entry.record_id = uuid::Uuid::from_u128(row.index as u128 + 1);
                            entry.path = PathBuf::from(format!("/Workspace/{name}"));
                            entry
                        })
                        .collect();
                    source(CollaborativeRecordSearch::new(&entries).entries())
                },
                3_000,
            );
            assert_eq!(
                hit.title,
                format!("ordinary record 2999 {LATE_HIT_NEEDLE}-文書")
            );

            let page =
                provider.search_page(&ui::search::SearchQuery::new("needle-文書"), None, 1, 1);
            assert_eq!(page.inspected, 1);
            assert_eq!(page.results, vec![hit]);
            assert!(
                page.next.is_none(),
                "the exact late hit exhausted its posting"
            );

            // Broad searches remain bounded and carry an explicit continuation.
            let first = provider.search_page(&ui::search::SearchQuery::new("record"), None, 7, 7);
            assert_eq!(first.inspected, 7);
            assert_eq!(first.results.len(), 7);
            let second = provider.search_page(
                &ui::search::SearchQuery::new("record"),
                first.next.as_ref(),
                7,
                7,
            );
            assert_eq!(second.inspected, 7);
            assert!(
                second
                    .results
                    .iter()
                    .all(|hit| !first.results.contains(hit))
            );
        }
    }
}
