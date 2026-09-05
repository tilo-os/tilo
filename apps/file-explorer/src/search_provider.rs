//! File-explorer places as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the file explorer declares
//! its searchable type by implementing [`SearchProvider`] over the user's
//! curated **Favourites / Places** list, so a pinned location is findable from
//! the unified Ctrl+K palette by its label or path. Activating a row navigates
//! there.
//!
//! Only the stable, in-memory places set is indexed — never the live directory
//! contents (which are not a bounded, stable library) nor the transient
//! back/forward history. The query path touches no filesystem, so it stays
//! synchronous.

use std::collections::BTreeSet;
use std::path::PathBuf;

use ui::qos_actions::ActionAddress;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::favorites::Favorite;
use crate::model::WorkspaceRecord;
use crate::remote_accounts::RemoteAccount;

pub const OPEN_ACTION: &str = "open";
pub const REVEAL_ACTION: &str = "reveal";
pub const REMOTE_ACCOUNT_OPEN_ACTION: &str = "open-remote-account";

/// Stable resource identity for a local place. Hex keeps the resource/action
/// wire separator out of ids even when a Unix path contains control bytes.
pub fn place_resource_id(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let encoded = path
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(String::new(), |mut output, byte| {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
            output
        });
    format!("files.place:{encoded}")
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

/// A directory projection is untrusted input. Keep the palette snapshot small
/// even when a mount exposes an unusually large directory.
pub const MAX_COLLABORATIVE_SEARCH_RECORDS: usize = 256;

/// A [`SearchProvider`] over a snapshot of the Favourites / Places list.
pub struct PlacesSearchProvider {
    favorites: Vec<Favorite>,
    kind: String,
}

impl PlacesSearchProvider {
    /// Snapshot the current favourites into a provider.
    pub fn new(favorites: Vec<Favorite>) -> Self {
        Self {
            favorites: favorites
                .into_iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .collect(),
            kind: crate::i18n::t("files-a11y-places"),
        }
    }
}

impl SearchProvider for PlacesSearchProvider {
    fn name(&self) -> &str {
        &self.kind
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        self.favorites
            .iter()
            .filter(|f| {
                f.label.to_lowercase().contains(&q)
                    || f.path.to_string_lossy().to_lowercase().contains(&q)
            })
            .map(|f| {
                SearchResult::for_action(&self.kind, f.label.clone(), place_open_address(&f.path))
                    .with_subtitle(f.path.display().to_string())
                    .with_icon(f.icon())
            })
            .take(limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
            .collect()
    }
}

/// Bounded search over the mount's ACL-filtered room record catalog. Record
/// UUIDs, rather than paths, are action identity: paths are mutable provenance
/// used only for matching and presentation.
pub struct CollaborativeRecordSearchProvider {
    records: Vec<CollaborativeSearchRecord>,
    kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CollaborativeSearchRecord {
    record_id: uuid::Uuid,
    name: String,
    provenance: PathBuf,
    is_dir: bool,
}

impl CollaborativeRecordSearchProvider {
    pub fn new(entries: &[WorkspaceRecord]) -> Self {
        let mut seen = BTreeSet::new();
        let records = entries
            .iter()
            .filter_map(|entry| {
                let record_id = entry.record_id;
                seen.insert(record_id).then(|| CollaborativeSearchRecord {
                    record_id,
                    name: entry.name.clone(),
                    provenance: entry.path.clone(),
                    is_dir: entry.is_dir,
                })
            })
            .take(MAX_COLLABORATIVE_SEARCH_RECORDS)
            .collect();
        Self {
            records,
            kind: crate::i18n::t("files-search-collaborative-records"),
        }
    }
}

impl SearchProvider for CollaborativeRecordSearchProvider {
    fn name(&self) -> &str {
        &self.kind
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.to_lowercase();
        self.records
            .iter()
            .filter(|record| {
                record.name.to_lowercase().contains(&query)
                    || record
                        .provenance
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&query)
            })
            .map(|record| {
                SearchResult::for_action(
                    &self.kind,
                    record.name.clone(),
                    record_reveal_address(record.record_id),
                )
                .with_subtitle(crate::i18n::t_args(
                    "files-search-record-provenance",
                    &[
                        crate::i18n::arg("path", record.provenance.display().to_string()),
                        crate::i18n::arg("record", record.record_id.to_string()),
                    ],
                ))
                .with_icon(ui::icons::icon_for_path(&record.provenance, record.is_dir))
            })
            .take(limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
            .collect()
    }
}

/// Searchable snapshot of the canonical, non-secret file-provider accounts.
/// This is deliberately a small metadata projection, not a remote directory
/// crawler: the mount daemon owns remote I/O and Files must remain responsive
/// and offline-safe while the palette is open.
pub struct RemoteAccountSearchProvider {
    accounts: Vec<RemoteAccount>,
    kind: String,
}

impl RemoteAccountSearchProvider {
    pub fn new(accounts: Vec<RemoteAccount>) -> Self {
        Self {
            accounts: accounts
                .into_iter()
                .take(MAX_COLLABORATIVE_SEARCH_RECORDS)
                .collect(),
            kind: "Remote file accounts".into(),
        }
    }
}

impl SearchProvider for RemoteAccountSearchProvider {
    fn name(&self) -> &str {
        &self.kind
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.to_lowercase();
        self.accounts
            .iter()
            .filter(|account| {
                account.label.to_lowercase().contains(&query)
                    || account.host.contains(&query)
                    || account.kind.label().to_lowercase().contains(&query)
            })
            .map(|account| {
                SearchResult::for_action(
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
                ))
            })
            .take(limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::favorites::{FavKind, Favorite};
    use ui::search::SearchRegistry;

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

    #[test]
    fn provider_finds_a_place_by_label_or_path() {
        let provider = PlacesSearchProvider::new(places());
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

    #[test]
    fn action_id_round_trips_to_the_destination_path() {
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(PlacesSearchProvider::new(places())));
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

    #[test]
    fn remote_accounts_are_searchable_by_label_host_and_kind() {
        let account = RemoteAccount {
            id: accounts::AccountId::from("cloud"),
            kind: crate::remote_accounts::RemoteAccountKind::Nextcloud,
            label: "Personal cloud".into(),
            host: "cloud.example".into(),
            health: crate::remote_accounts::RemoteAccountHealth::NeedsAuthentication,
        };
        let provider = RemoteAccountSearchProvider::new(vec![account.clone()]);
        for query in ["personal", "example", "nextcloud"] {
            assert_eq!(provider.search(query).len(), 1, "{query}");
        }
        assert_eq!(
            provider.search("personal")[0].action_address(),
            Some(remote_account_open_address(&account))
        );
    }

    #[test]
    fn place_resource_ids_are_wire_safe() {
        let address = place_open_address(PathBuf::from("/tmp/a\u{1f}b").as_path());
        assert_eq!(ActionAddress::parse(&address.encode()), Some(address));
    }

    #[test]
    fn collaborative_results_use_stable_record_identity_and_path_as_provenance() {
        let record_id = uuid::Uuid::from_u128(0x51);
        let mut entry = collaborative_entry("old-name.txt");
        entry.path = PathBuf::from("/Workspace/old-name.txt");
        entry.record_id = record_id;
        let provider = CollaborativeRecordSearchProvider::new(&[entry]);
        let hit = provider.search("workspace").pop().expect("path matches");
        assert_eq!(hit.action_address(), Some(record_reveal_address(record_id)));
        assert!(hit.subtitle.unwrap().contains("/Workspace/old-name.txt"));
    }

    #[test]
    fn collaborative_projection_is_deduplicated_and_bounded() {
        let entries: Vec<_> = (0..MAX_COLLABORATIVE_SEARCH_RECORDS + 20)
            .flat_map(|index| {
                let id = uuid::Uuid::from_u128(index as u128 + 1);
                let mut first = collaborative_entry(&format!("file-{index}"));
                first.record_id = id;
                let duplicate = first.clone();
                [first, duplicate]
            })
            .collect();
        let provider = CollaborativeRecordSearchProvider::new(&entries);
        assert_eq!(provider.search("").len(), MAX_COLLABORATIVE_SEARCH_RECORDS);
    }

    #[test]
    fn widget_hooks_find_then_navigate_to_the_place() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        // Build the app against real temp dirs so navigation actually loads.
        let base = std::env::temp_dir().join(format!("qos-fe-search-{}", std::process::id()));
        let home = base.join("home");
        let sub = home.join("sub");
        let cfg = base.join("cfg");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&cfg).unwrap();

        let mut app =
            crate::files_app::FileExplorerWidget::with_persistence(Some(cfg), Some(home.clone()));
        // Move away from Home so activation demonstrably navigates back.
        app.navigate(sub.clone());
        assert_eq!(app.current_path(), sub);

        let providers = app.search_providers();
        assert!(
            !providers.is_empty(),
            "the file explorer publishes a search provider"
        );
        let hit = providers[0]
            .search("")
            .into_iter()
            .find(|r| r.action_address() == Some(place_open_address(&home)))
            .expect("the Home favourite is findable through the real hook");

        let doc = AutoCommit::new();
        let address = hit.action_address().expect("canonical action address");
        let resources = app.resources(&doc);
        let spec = ui::qos_actions::door::resolve(&resources, &address)
            .expect("search address is exported");
        assert_eq!(spec.kind, ui::qos_actions::OperationKind::View);
        assert_eq!(spec.contract, ui::qos_actions::ActionContract::view());
        let kind = spec.kind;
        let encoded =
            ui::qos_actions::door::encode_json(&resources, &address, &serde_json::Map::new())
                .expect("search address is exported");
        assert!(
            app.invoke_resource_operation(kind, &encoded.type_id, &encoded.payload, &doc, &ROOT)
                .unwrap()
        );
        assert_eq!(
            app.current_path(),
            home,
            "activation navigated to the chosen place"
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
