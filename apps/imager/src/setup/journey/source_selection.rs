//! The source inventory behind Your data: what was discovered, and what the person
//! chose to bring over.

use super::belongings::Belongings;
use imager_backend::setup::source_tree::{
    Counts, Decision, Exclusions, Selection, SelectionState, SteamCatalog, Tree,
};
use std::path::PathBuf;

pub trait Sources {
    fn windows_tree(&self, catalog: SteamCatalog) -> Option<Tree>;
    fn home_tree(&self, home: &std::path::Path, catalog: SteamCatalog) -> Result<Tree, String>;
}

impl Sources for Belongings {
    fn windows_tree(&self, catalog: SteamCatalog) -> Option<Tree> {
        self.windows_source_tree(catalog)
    }

    fn home_tree(&self, home: &std::path::Path, catalog: SteamCatalog) -> Result<Tree, String> {
        self.home_source_tree(home, catalog)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSet {
    Windows,
    Home(PathBuf),
}

#[derive(Clone, Default)]
pub struct SourceSelection {
    selection: Option<Selection>,
    state: &'static str,
}

impl SourceSelection {
    pub fn discover<S: Sources + ?Sized>(sources: &S, set: &SourceSet) -> Self {
        let catalog = SteamCatalog::builtin();
        let tree = match set {
            SourceSet::Windows => sources
                .windows_tree(catalog)
                .ok_or_else(|| "Windows sources unavailable".to_string()),
            SourceSet::Home(home) => sources.home_tree(home, catalog),
        };
        Self::from_tree(tree)
    }

    pub fn from_tree(tree: Result<Tree, String>) -> Self {
        match tree {
            Ok(tree) => Self {
                selection: Some(tree.selection()),
                state: "ready",
            },
            Err(_) => Self {
                selection: None,
                state: "failed",
            },
        }
    }

    pub fn ready(&self) -> bool {
        self.selection.is_some()
    }

    pub fn state(&self) -> &'static str {
        if self.state.is_empty() {
            "idle"
        } else {
            self.state
        }
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.selection.as_ref().map(Selection::tree)
    }

    pub fn set(&mut self, id: &str, decision: Decision) -> Result<(), &'static str> {
        self.selection
            .as_mut()
            .ok_or("installer-your-data-changed")?
            .set(id, decision)
            .map_err(|_| "installer-your-data-changed")
    }

    pub fn select_all(&mut self, selected: bool) {
        if let Some(selection) = &mut self.selection {
            selection.select_all(selected);
        }
    }

    pub fn state_of(&self, id: &str) -> Option<SelectionState> {
        self.selection.as_ref()?.state(id)
    }

    pub fn node_state(&self, id: &str) -> Option<SelectionState> {
        self.state_of(id)
    }

    pub fn any_selected(&self) -> bool {
        let Some(selection) = &self.selection else {
            return false;
        };
        selection.tree().sources.iter().any(|node| {
            matches!(
                selection.state(&node.id),
                Some(SelectionState::Selected | SelectionState::Mixed)
            )
        })
    }

    pub fn counts(&self) -> Option<Counts> {
        self.selection.as_ref().map(Selection::counts)
    }

    pub fn exclusions(&self) -> Option<Exclusions> {
        self.selection.as_ref().map(Selection::exclusions)
    }

    pub fn selected_files(&self) -> Option<bool> {
        self.selection
            .as_ref()
            .map(|selection| !selection.selected_is_known_empty())
    }

    pub fn report(&self) -> serde_json::Value {
        match &self.selection {
            Some(selection) => selection.report(),
            None => serde_json::json!({ "state": self.state() }),
        }
    }

    pub fn adopt(&mut self, previous: &Self) {
        let (Some(previous), Some(selection)) = (&previous.selection, &mut self.selection) else {
            return;
        };
        selection.adopt(previous);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use imager_backend::setup::source_tree::Label;
    use std::fs;

    fn home_fixture(root: &std::path::Path) -> std::path::PathBuf {
        let home = root.join("home");
        fs::create_dir_all(home.join("Documents/Work")).unwrap();
        fs::create_dir_all(home.join("Documents/Private")).unwrap();
        fs::write(home.join("Documents/Work/plan"), b"work").unwrap();
        fs::write(home.join("Documents/Private/note"), b"private").unwrap();
        fs::write(home.join("Documents/loose"), b"loose").unwrap();
        fs::create_dir_all(home.join(".config")).unwrap();
        home
    }

    fn belongings(home: &std::path::Path) -> Belongings {
        Belongings::from_roots(imager_backend::account_transfer::SourceRoots {
            home: home.to_path_buf(),
            config: imager_backend::home_transfer::granted_root(&home.join(".config")).unwrap(),
        })
    }

    fn top(selection: &SourceSelection, label: Label) -> &imager_backend::setup::source_tree::Node {
        selection
            .tree()
            .unwrap()
            .sources
            .iter()
            .find(|node| node.label == label)
            .expect("recognized source")
    }

    #[test]
    fn home_sources_are_discovered_and_selection_never_broadens() {
        let fixture = tempfile::tempdir().unwrap();
        let home = home_fixture(fixture.path());
        let mut selection =
            SourceSelection::discover(&belongings(&home), &SourceSet::Home(home.clone()));
        assert!(selection.ready());
        assert_eq!(selection.state(), "ready");
        let documents = top(
            &selection,
            Label::KnownFolder(imager_backend::setup::source_tree::KnownFolder::Documents),
        )
        .id
        .clone();
        let folders = selection
            .tree()
            .unwrap()
            .node(&documents)
            .unwrap()
            .children
            .iter()
            .filter(|child| {
                child.label == Label::Folder
                    && child.kind == imager_backend::setup::source_tree::NodeKind::Folder
            })
            .map(|child| child.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(folders.len(), 2, "both folders are selectable children");
        selection.set(&documents, Decision::Exclude).unwrap();
        assert_eq!(
            selection.node_state(&documents),
            Some(SelectionState::Excluded)
        );
        selection.set(&folders[1], Decision::Select).unwrap();
        assert_eq!(
            selection.node_state(&documents),
            Some(SelectionState::Mixed)
        );
        let exclusions = selection.exclusions().unwrap();
        assert!(
            exclusions
                .home()
                .iter()
                .any(|path| path.ends_with("Private"))
        );
        assert!(
            exclusions.home().iter().any(|path| path.ends_with("loose")),
            "the folder's loose files stay excluded: {exclusions:?}"
        );
        assert!(
            !exclusions
                .home()
                .iter()
                .any(|path| path == std::path::Path::new("Documents"))
        );
        let text = selection.report().to_string();
        assert!(!text.contains(home.to_string_lossy().as_ref()));
        assert!(!text.contains("Private"));
    }

    #[test]
    fn toggling_all_is_reversible_and_discovery_is_not_repeated() {
        let fixture = tempfile::tempdir().unwrap();
        let home = home_fixture(fixture.path());
        let mut selection = SourceSelection::discover(&belongings(&home), &SourceSet::Home(home));
        let sources = selection.tree().unwrap().sources.len();
        assert_ne!(sources, 0);
        selection.select_all(false);
        assert!(!selection.exclusions().unwrap().is_empty());
        assert_eq!(selection.counts().unwrap().local_bytes, 0);
        selection.select_all(true);
        assert!(selection.exclusions().unwrap().is_empty());
        assert!(selection.counts().unwrap().local_bytes > 0);
    }

    #[test]
    fn an_excluded_folder_survives_rediscovery_but_an_unknown_node_is_refused() {
        let fixture = tempfile::tempdir().unwrap();
        let home = home_fixture(fixture.path());
        let mut previous =
            SourceSelection::discover(&belongings(&home), &SourceSet::Home(home.clone()));
        let documents = top(
            &previous,
            Label::KnownFolder(imager_backend::setup::source_tree::KnownFolder::Documents),
        )
        .id
        .clone();
        previous.set(&documents, Decision::Exclude).unwrap();
        let mut refreshed = SourceSelection::discover(&belongings(&home), &SourceSet::Home(home));
        refreshed.adopt(&previous);
        assert_eq!(
            refreshed.node_state(&documents),
            Some(SelectionState::Excluded)
        );
        assert!(
            !refreshed.exclusions().unwrap().is_empty(),
            "the person's exclusion reaches the copy producer after a refresh"
        );
        assert_eq!(
            refreshed.set("invented", Decision::Exclude),
            Err("installer-your-data-changed")
        );
    }

    #[test]
    fn a_refused_discovery_is_a_named_state_not_an_empty_success() {
        let selection = SourceSelection::from_tree(Err("private detail".into()));
        assert!(!selection.ready());
        assert_eq!(selection.state(), "failed");
        assert!(!selection.report().to_string().contains("private detail"));
        assert!(selection.exclusions().is_none());
        assert_eq!(
            selection.selected_files(),
            None,
            "a source that could not be read is unknown, not empty"
        );
    }

    #[test]
    fn a_complete_empty_home_is_known_empty_but_a_nonempty_one_is_not() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("empty-home");
        fs::create_dir_all(home.join(".config")).unwrap();
        let empty = SourceSelection::discover(&belongings(&home), &SourceSet::Home(home.clone()));
        assert!(empty.ready());
        assert_eq!(
            empty.selected_files(),
            Some(false),
            "a complete tree with nothing selected has nothing to capture"
        );

        let recognized = home_fixture(fixture.path());
        let populated =
            SourceSelection::discover(&belongings(&recognized), &SourceSet::Home(recognized));
        assert_eq!(populated.selected_files(), Some(true));
    }
}
