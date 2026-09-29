//! File-explorer places as a Tilo global-search source.
//!
//! Mirrors the reference wiring in `apps/contacts`: the file explorer declares
//! its searchable type by implementing `SearchProvider` over the user's
//! curated **Favourites / Places** list, so a pinned location is findable from
//! the unified Ctrl+K palette by its label or path. Activating a row navigates
//! there.
//!
//! Only the stable, in-memory places set is indexed — never the live directory
//! contents (which are not a bounded, stable library) nor the transient
//! back/forward history. The query path touches no filesystem, so it stays
//! synchronous.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use sha2::{Digest as _, Sha256};

use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};
use ui::tilo_actions::{ActionAddress, ActionContract, ActionSpec, OperationKind, Resource};

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

pub fn place_open_address(path: &std::path::Path) -> ActionAddress {
    ActionAddress::new(place_resource_id(path), OPEN_ACTION)
}

pub fn record_resource_id(record_id: uuid::Uuid) -> String {
    format!("files.record:{record_id}")
}

pub fn record_reveal_address(record_id: uuid::Uuid) -> ActionAddress {
    ActionAddress::new(record_resource_id(record_id), REVEAL_ACTION)
}

pub fn remote_account_open_address(account: &RemoteAccount) -> ActionAddress {
    ActionAddress::new(
        crate::remote_accounts::resource_id(&account.id),
        REMOTE_ACCOUNT_OPEN_ACTION,
    )
}

/// Join independently searchable fields without allowing a query to match
/// across a label/path boundary.
fn searchable_fields(fields: impl IntoIterator<Item = String>) -> String {
    fields.into_iter().collect::<Vec<_>>().join("\u{1f}")
}

/// A retained, revision-gated catalogue over the private Places list.  The
/// widget changes the revision only after a favourite mutation, so a palette
/// frame merely shares this Arc-backed index instead of re-walking paths.
#[derive(Clone)]
pub struct PlacesSearchProvider {
    catalog: SearchCatalog,
    snapshot: Arc<RwLock<PlacesSnapshot>>,
    kind: String,
}

#[derive(Clone, Debug)]
struct PlacesSnapshot {
    revision: Option<u64>,
    favorites: BTreeMap<String, Favorite>,
}

impl PlacesSearchProvider {
    /// Create a retained provider with an initial snapshot.
    pub fn new(favorites: &[Favorite]) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(crate::i18n::t("files-a11y-places")),
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
    /// a steady-state `sync` into a full catalogue traversal.
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
        let mut current = BTreeMap::new();
        for favorite in favorites {
            let id = place_resource_id(&favorite.path);
            let result = SearchResult::for_action(
                &self.kind,
                favorite.label.clone(),
                place_open_address(&favorite.path),
            )
            .with_subtitle(favorite.path.display().to_string())
            .with_icon(favorite.icon());
            let _ = self.catalog.upsert(
                id.clone(),
                searchable_fields([
                    favorite.label.clone(),
                    favorite.path.to_string_lossy().into_owned(),
                ]),
                result,
            );
            current.insert(id, favorite.clone());
        }
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for removed in snapshot
            .favorites
            .keys()
            .filter(|id| !current.contains_key(*id))
        {
            self.catalog.remove(removed);
        }
        snapshot.favorites = current;
        snapshot.revision = Some(revision);
        true
    }

    pub(crate) fn resource(&self, id: &str) -> Option<Resource> {
        let favorite = self.snapshot.read().ok()?.favorites.get(id)?.clone();
        Some(
            Resource::new(id, "files.place", favorite.label).with_action(
                ActionSpec::operation(OPEN_ACTION, "Open", OperationKind::View, "files.open", &id)
                    .ok()?
                    .with_contract(ActionContract::view()),
            ),
        )
    }

    #[cfg(test)]
    fn stats(&self) -> ui::search::SearchCatalogStats {
        self.catalog.stats()
    }
}

impl CatalogSearchProvider for PlacesSearchProvider {
    fn provider_name(&self) -> &str {
        &self.kind
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        (address.action == OPEN_ACTION)
            .then(|| self.resource(&address.resource))
            .flatten()
    }
}

/// Complete search over the mount's ACL-filtered room record catalog. Record
/// UUIDs, rather than paths, are action identity: paths are mutable provenance
/// used only for matching and presentation.
#[derive(Clone)]
pub struct CollaborativeRecordSearchProvider {
    catalog: SearchCatalog,
    snapshot: Arc<RwLock<RecordSnapshot>>,
    kind: String,
}

#[derive(Clone, Debug, Default)]
struct RecordSnapshot {
    revision: Option<u64>,
    records: BTreeMap<String, WorkspaceRecord>,
}

impl CollaborativeRecordSearchProvider {
    #[allow(
        clippy::expect_used,
        reason = "the action address and catalogue key are derived from the same immutable record UUID"
    )]
    pub(crate) fn new(entries: &[WorkspaceRecord]) -> Self {
        let kind = crate::i18n::t("files-search-collaborative-records");
        let provider = Self {
            catalog: SearchCatalog::new(kind.clone()),
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
        for removed in snapshot.records.keys() {
            self.catalog.remove(removed);
        }
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
            let address = record_reveal_address(entry.record_id);
            let result = SearchResult::for_action(&self.kind, entry.name.clone(), &address)
                .with_subtitle(crate::i18n::t_args(
                    "files-search-record-provenance",
                    &[
                        crate::i18n::arg("path", entry.path.display().to_string()),
                        crate::i18n::arg("record", entry.record_id.to_string()),
                    ],
                ))
                .with_icon(ui::icons::icon_for_path(&entry.path, entry.is_dir));
            let _ = self.catalog.upsert(
                address.resource.clone(),
                searchable_fields([
                    entry.name.clone(),
                    entry.path.to_string_lossy().into_owned(),
                ]),
                result,
            );
            snapshot.records.insert(address.resource, entry.clone());
        }
        true
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
        Some(
            Resource::new(id, "files.record", record.name).with_action(
                ActionSpec::operation(
                    REVEAL_ACTION,
                    "Reveal in Files",
                    OperationKind::View,
                    "files.reveal-record",
                    &record.record_id.to_string(),
                )
                .ok()?
                .with_contract(ActionContract::view()),
            ),
        )
    }
}

impl CatalogSearchProvider for CollaborativeRecordSearchProvider {
    fn provider_name(&self) -> &str {
        &self.kind
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        (address.action == REVEAL_ACTION)
            .then(|| self.resource(&address.resource))
            .flatten()
    }
}

/// Searchable snapshot of the canonical, non-secret file-provider accounts.
/// This is deliberately a small metadata projection, not a remote directory
/// crawler: the mount daemon owns remote I/O and Files must remain responsive
/// and offline-safe while the palette is open.
#[derive(Clone)]
pub struct RemoteAccountSearchProvider {
    catalog: SearchCatalog,
    snapshot: Arc<RwLock<RemoteAccountsSnapshot>>,
    kind: String,
}

#[derive(Clone, Debug)]
struct RemoteAccountsSnapshot {
    revision: Option<u64>,
    accounts: BTreeMap<String, RemoteAccount>,
}

impl RemoteAccountSearchProvider {
    pub fn new(accounts: &[RemoteAccount]) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new("Remote file accounts"),
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
        let mut current = BTreeMap::new();
        for account in accounts {
            let id = crate::remote_accounts::resource_id(&account.id);
            let result = SearchResult::for_action(
                &self.kind,
                account.label.clone(),
                remote_account_open_address(account),
            )
            .with_subtitle(format!(
                "{} · {} · {}",
                account.kind.label(),
                account.host,
                account.health.label()
            ))
            .with_icon(ui::icons::icon_for_path(
                std::path::Path::new("remote-folder"),
                true,
            ));
            let _ = self.catalog.upsert(
                id.clone(),
                searchable_fields([
                    account.label.clone(),
                    account.host.clone(),
                    account.kind.label().to_owned(),
                ]),
                result,
            );
            current.insert(id, account.clone());
        }
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for removed in snapshot
            .accounts
            .keys()
            .filter(|id| !current.contains_key(*id))
        {
            self.catalog.remove(removed);
        }
        snapshot.accounts = current;
        snapshot.revision = Some(revision);
        true
    }

    pub(crate) fn resource(&self, id: &str) -> Option<Resource> {
        let account = self.snapshot.read().ok()?.accounts.get(id)?.clone();
        let payload = vec!["details".to_string(), account.id.to_string()];
        Some(
            Resource::new(id, "files.remote-account", account.label).with_action(
                ActionSpec::operation(
                    REMOTE_ACCOUNT_OPEN_ACTION,
                    "Show connection details",
                    OperationKind::View,
                    "files.remote-account",
                    &payload,
                )
                .ok()?
                .with_contract(ActionContract::view()),
            ),
        )
    }
}

impl CatalogSearchProvider for RemoteAccountSearchProvider {
    fn provider_name(&self) -> &str {
        &self.kind
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        (address.action == REMOTE_ACCOUNT_OPEN_ACTION)
            .then(|| self.resource(&address.resource))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::favorites::{FavKind, Favorite};
    use std::path::PathBuf;
    use ui::search::SearchProvider;
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
            let provider = PlacesSearchProvider::new(&values);
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
            registry.register(Box::new(PlacesSearchProvider::new(&values)));
            let hit = registry
                .query("documents")
                .into_iter()
                .find(|r| r.title == "Documents")
                .expect("Documents is findable");
            assert_eq!(
                hit.action_address(),
                Some(place_open_address(
                    PathBuf::from("/home/tester/Documents").as_path()
                ))
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
            let provider = RemoteAccountSearchProvider::new(&accounts);
            for query in ["personal", "example", "nextcloud"] {
                assert_eq!(provider.search(query).len(), 1, "{query}");
            }
            assert_eq!(
                provider.search("personal")[0].action_address(),
                Some(remote_account_open_address(&account))
            );
        }

        // --- phase: place_resource_ids_are_wire_safe ---
        {
            let address = place_open_address(PathBuf::from("/tmp/a\u{1f}b").as_path());
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
            use automerge::{AutoCommit, ROOT};
            use ui::widget::Widget;

            // Build the app against real temp dirs so navigation actually loads.
            let base = std::env::temp_dir().join(format!("tilo-fe-search-{}", std::process::id()));
            let home = base.join("home");
            let sub = home.join("sub");
            let cfg = base.join("cfg");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::create_dir_all(&cfg).unwrap();

            let mut app = crate::files_app::FileExplorerWidget::with_persistence(
                Some(cfg),
                Some(home.clone()),
            );
            // Move away from Home so activation demonstrably navigates back.
            app.navigate(sub.clone());
            assert_eq!(app.current_path(), sub);

            let providers = ui::search::capability::search_providers(&app);
            assert!(
                !providers.is_empty(),
                "the file explorer publishes a search provider"
            );
            let hit = providers[0]
                .search("")
                .into_iter()
                .find(|r| r.action_address() == Some(place_open_address(&home)))
                .expect("the Home favourite is findable through the real hook");

            let mut doc = AutoCommit::new();
            let address = hit.action_address().expect("canonical action address");
            let resources = ui::resources::resources(&app, &doc);
            let spec = ui::tilo_actions::door::resolve(&resources, &address)
                .expect("search address is exported");
            assert_eq!(spec.kind, ui::tilo_actions::OperationKind::View);
            assert_eq!(spec.contract, ui::tilo_actions::ActionContract::view());
            let kind = spec.kind;
            let encoded =
                ui::tilo_actions::door::encode_json(&resources, &address, &serde_json::Map::new())
                    .expect("search address is exported");
            assert!(matches!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        kind,
                        &encoded.type_id,
                        &encoded.payload
                    ),
                    &mut doc,
                    &ROOT
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
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
        // --- phase: retained_places_catalogue_gates_unchanged_revisions_and_pages_with_hard_budgets ---
        {
            let values = places();
            let provider = PlacesSearchProvider::new(&values);
            let before = provider.stats();
            let changed = vec![Favorite {
                label: "Late place".into(),
                path: "/home/tester/late-needle".into(),
                is_home: false,
                kind: FavKind::Folder,
            }];
            assert!(
                !provider.refresh(0, &changed),
                "the owner must advance the source revision before any traversal/reindex"
            );
            assert_eq!(provider.stats(), before);
            assert!(provider.search("late-needle").is_empty());
            assert!(provider.refresh(1, &changed));
            assert_eq!(provider.search("late-needle").len(), 1);

            let page = provider.search_page("late", None, 1, 1);
            assert_eq!(page.inspected, 1);
            assert_eq!(page.results.len(), 1);
            assert!(page.next.is_none());
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
            let provider = RemoteAccountSearchProvider::new(&accounts);
            let first = provider.search_page("record", None, 3, 3);
            assert_eq!(first.inspected, 3);
            assert_eq!(first.results.len(), 3);
            let second = provider.search_page("record", first.next.as_ref(), 3, 3);
            assert_eq!(second.inspected, 3);
            assert!(
                second
                    .results
                    .iter()
                    .all(|hit| !first.results.contains(hit))
            );

            let hit = first.results.first().expect("a page has a result");
            let address = hit.action_address().expect("canonical account address");
            let resource = provider
                .resolve_action_resource(&address)
                .expect("the omitted account is materialized exactly");
            let resources = [resource];
            let spec = ui::tilo_actions::door::resolve(&resources, &address)
                .expect("the lazy action matches the search address");
            assert_eq!(spec.id, REMOTE_ACCOUNT_OPEN_ACTION);
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
            let provider = CollaborativeRecordSearchProvider::new(&[entry]);
            let hit = provider.search("workspace").pop().expect("path matches");
            assert_eq!(hit.action_address(), Some(record_reveal_address(record_id)));
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
            let provider = CollaborativeRecordSearchProvider::new(&entries);
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
                    CollaborativeRecordSearchProvider::new(&entries)
                },
                3_000,
            );
            assert_eq!(
                hit.title,
                format!("ordinary record 2999 {LATE_HIT_NEEDLE}-文書")
            );

            let page = provider.search_page("needle-文書", None, 1, 1);
            assert_eq!(page.inspected, 1);
            assert_eq!(page.results, vec![hit]);
            assert!(
                page.next.is_none(),
                "the exact late hit exhausted its posting"
            );

            // Broad searches remain bounded and carry an explicit continuation.
            let first = provider.search_page("record", None, 7, 7);
            assert_eq!(first.inspected, 7);
            assert_eq!(first.results.len(), 7);
            let second = provider.search_page("record", first.next.as_ref(), 7, 7);
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
