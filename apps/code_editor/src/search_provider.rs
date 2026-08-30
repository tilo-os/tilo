//! Code editor files as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: the editor declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! session, so an **open buffer** (a current tab) and a **project file** (a
//! node the tree has revealed) are both findable from the unified Ctrl+K
//! palette by name or path. Activating a row hands back the file's node id,
//! which the app opens / focuses.
//!
//! The provider is deliberately **synchronous and in-memory**: it walks only
//! the open buffers and the already-loaded project nodes (the tree is lazy, so
//! this is bounded to what has been expanded). The editor's disk-walking
//! project-wide *content* grep stays in its own find surface, which is async by
//! nature — a Ctrl+K query must never block.

use std::collections::HashSet;

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};

use crate::model::{NodeId, Session};

/// The group header for open-tab results.
pub const OPEN_KIND: &str = "Open files";
/// The group header for revealed project-file results.
pub const PROJECT_KIND: &str = "Project files";

const ACTION_PREFIX: &str = "code.node:";

/// One searchable file entry snapshotted from the session.
struct FileEntry {
    node: NodeId,
    name: String,
    path: String,
    open: bool,
}

/// A [`SearchProvider`] over a snapshot of the open buffers + revealed project
/// files.
pub struct FileSearchProvider {
    files: Vec<FileEntry>,
}

impl FileSearchProvider {
    /// Snapshot the current session into a provider.
    pub fn new(session: &Session) -> Self {
        let open_nodes: HashSet<NodeId> = session.open.iter().map(|b| b.node).collect();
        let mut files = Vec::new();

        // Open tabs first, so a currently-edited file leads and carries its own
        // buffer name (a scratch buffer may have no project node).
        for b in &session.open {
            let path = b
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    session
                        .project
                        .get(b.node)
                        .map(|_| session.project.path_of(b.node))
                })
                .unwrap_or_else(|| b.name.clone());
            files.push(FileEntry {
                node: b.node,
                name: b.name.clone(),
                path,
                open: true,
            });
        }

        // Then every revealed file node that is not already an open tab.
        for (id, tn) in &session.project.nodes {
            if tn.is_folder() || open_nodes.contains(id) {
                continue;
            }
            files.push(FileEntry {
                node: *id,
                name: tn.name.clone(),
                path: session.project.path_of(*id),
                open: false,
            });
        }

        Self { files }
    }

    /// The `action_id` for a file node — what a chosen row hands back.
    pub fn action_id(node: NodeId) -> String {
        format!("{ACTION_PREFIX}{node}")
    }

    /// Resolve an `action_id` produced by this provider back into a node id.
    pub fn parse_action_id(action_id: &str) -> Option<NodeId> {
        action_id
            .strip_prefix(ACTION_PREFIX)
            .and_then(|rest| rest.parse::<NodeId>().ok())
    }
}

impl SearchProvider for FileSearchProvider {
    fn name(&self) -> &'static str {
        // Rows carry their own kind ("Open files" / "Project files"); this is
        // the default/diagnostic label.
        "Files"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        self.files
            .iter()
            .filter(|f| {
                q.is_empty()
                    || f.name.to_lowercase().contains(&q)
                    || f.path.to_lowercase().contains(&q)
            })
            .map(|f| {
                let kind = if f.open { OPEN_KIND } else { PROJECT_KIND };
                let mut r = SearchResult::new(kind, f.name.clone(), Self::action_id(f.node))
                    .with_icon(Icon::FileCode);
                // The breadcrumb path disambiguates same-named files; skip it
                // when it is just the bare name.
                if f.path != f.name && !f.path.is_empty() {
                    r = r.with_subtitle(f.path.clone());
                }
                // Float open tabs a notch above equally-matching project files.
                if f.open {
                    r = r.with_score(1);
                }
                r
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::SearchRegistry;

    fn sample_session() -> Session {
        crate::app::CodeApp::with_sample()
            .session_for_test()
            .clone()
    }

    #[test]
    fn provider_finds_a_project_file_by_name() {
        let session = sample_session();
        let provider = FileSearchProvider::new(&session);
        // Every non-folder node should be findable by browsing.
        let browse = provider.search("");
        assert!(!browse.is_empty(), "the sample project has revealed files");
        assert!(
            browse
                .iter()
                .all(|r| r.kind == OPEN_KIND || r.kind == PROJECT_KIND)
        );
    }

    #[test]
    fn action_id_round_trips_to_a_real_node() {
        let session = sample_session();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(FileSearchProvider::new(&session)));
        let hit = registry
            .query("")
            .into_iter()
            .next()
            .expect("something is browsable");
        let node = FileSearchProvider::parse_action_id(&hit.action_id)
            .expect("action_id parses to a node");
        assert!(
            session.project.get(node).is_some(),
            "the id resolves to a real project node"
        );
    }

    #[test]
    fn parse_action_id_rejects_foreign_ids() {
        assert!(FileSearchProvider::parse_action_id("file:/etc/passwd").is_none());
        assert!(FileSearchProvider::parse_action_id("code.node:not-a-number").is_none());
    }

    #[test]
    fn widget_hooks_find_then_open_the_file() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::CodeApp::with_sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Browse the app's OWN provider for a Project-files row (a file not
        // already the active tab), then activate it.
        let (action_id, node) = {
            let providers = app.search_providers();
            assert!(
                !providers.is_empty(),
                "code_editor publishes a search provider"
            );
            let hit = providers[0]
                .search("")
                .into_iter()
                .find(|r| r.kind == PROJECT_KIND)
                .expect("a project file is browsable through the real hook");
            let node = FileSearchProvider::parse_action_id(&hit.action_id).unwrap();
            (hit.action_id, node)
        };

        app.activate_search_result(&action_id, &mut doc, &ROOT);
        assert_eq!(
            app.active_file_node(),
            Some(node),
            "activation opened/focused the chosen file"
        );
    }
}
