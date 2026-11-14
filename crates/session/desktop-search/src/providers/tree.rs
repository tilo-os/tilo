//! A workspace's files, found by their names and by what their text holds.
//!
//! A computer's Files are found by a walk of its disk ([`super::files`]). A
//! page has no disk to walk: a workspace's files are its Files record, the
//! tree every member holds, and their content is what the page asked the
//! workspace for. The host hands this source the tree as it last read it, and
//! the words of each file whose content it holds; a file whose content has not
//! arrived is still found by its name.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use ui::search::FieldWeight;
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind, rank_fields};

use crate::model::{Hit, Source, Target};

/// One file of a workspace's tree, as a row names and opens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeFile {
    /// The node of the tree it is, so a row stands for the file it names.
    pub id: String,
    /// What it is called.
    pub name: String,
    /// Where it is in the workspace, as the window lists it (`docs/plan.md`).
    pub path: String,
    /// The content its bytes are, by hash; none for a folder.
    pub hash: Option<String>,
    pub folder: bool,
    /// How large it is, in bytes.
    pub size: u64,
}

/// The app that keeps a workspace's tree: the folder a row opens it at.
const FILES_APP: &str = "so.tilo.file-explorer";

/// The most rows one query offers.
const MAX_ROWS: usize = 64;
/// A file found by what its text holds, not by its name: still a match, below
/// any named one.
const TEXT_SCORE: i32 = 12;

/// The files of one workspace's tree, and the words of those whose content
/// the host holds.
pub struct TreeFiles {
    workspace: String,
    files: RwLock<Arc<Vec<TreeFile>>>,
    /// The words of each file, by content hash.
    texts: RwLock<Arc<HashMap<String, String>>>,
}

impl TreeFiles {
    /// The files of `workspace`'s tree, as the host last read them.
    #[must_use]
    pub fn new(workspace: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            files: RwLock::default(),
            texts: RwLock::default(),
        }
    }

    /// The tree as the host reads it now; whether it moved.
    pub fn set_files(&self, files: Vec<TreeFile>) -> bool {
        let mut held = self.files.write().unwrap_or_else(PoisonError::into_inner);
        if **held == files {
            return false;
        }
        *held = Arc::new(files);
        true
    }

    /// What the file whose content is `hash` says, as the host read it.
    pub fn note_text(&self, hash: &str, text: String) {
        let mut texts = self.texts.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = (**texts).clone();
        next.insert(hash.to_owned(), text);
        *texts = Arc::new(next);
    }

    /// Whether the words of the file whose content is `hash` are here.
    #[must_use]
    pub fn has_text(&self, hash: &str) -> bool {
        self.texts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(hash)
    }

    /// The row for `file`, found by `score`.
    fn hit(&self, file: &TreeFile, score: i32) -> Hit {
        let (kind, target) = match &file.hash {
            None => (
                ResourceKind::Folder,
                Target::Launch {
                    app: FILES_APP.to_owned(),
                    place: crate::LaunchPlace::Focus,
                },
            ),
            Some(hash) => (
                ResourceKind::File,
                Target::Content {
                    hash: hash.clone(),
                    name: file.name.clone(),
                },
            ),
        };
        Hit {
            kind,
            provider: self.id().to_owned(),
            id: file.id.clone(),
            title: file.name.clone(),
            subtitle: Some(format!("{} · {}", self.workspace_named(), file.path)),
            facts: Vec::new(),
            score,
            icon: None,
            faces: Vec::new(),
            outcome: None,
            row: None,
            preview: None,
            target,
        }
    }

    /// What the workspace's own rows say the files are of: its name when the
    /// host gave one, else nothing.
    fn workspace_named(&self) -> &str {
        &self.workspace
    }
}

impl Source for TreeFiles {
    fn id(&self) -> &'static str {
        "workspace-files"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-workspace-files")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        let files = Arc::clone(&self.files.read().unwrap_or_else(PoisonError::into_inner));
        let texts = Arc::clone(&self.texts.read().unwrap_or_else(PoisonError::into_inner));
        let mut hits: Vec<Hit> = files
            .iter()
            .filter_map(|file| {
                rank_fields(needle, &[(&file.name, FieldWeight::TITLE)])
                    .map(|score| self.hit(file, score))
            })
            .collect();
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
        hits.truncate(MAX_ROWS);
        // What the words of a file say, for the files not already named: a
        // note's body, a report's page, found as its app finds it.
        let folded = ui::search::fold_search_text(needle);
        let needle = folded.trim();
        if needle.is_empty() {
            return hits;
        }
        for file in files.iter() {
            if hits.len() >= MAX_ROWS {
                break;
            }
            let Some(hash) = &file.hash else {
                continue;
            };
            let Some(text) = texts.get(hash) else {
                continue;
            };
            if !ui::search::fold_search_text(text).contains(needle) {
                continue;
            }
            if hits.iter().any(|hit| hit.id == file.id) {
                continue;
            }
            hits.push(self.hit(file, TEXT_SCORE.min(MAX_PROVIDER_SCORE)));
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: &str, name: &str, hash: Option<&str>) -> TreeFile {
        TreeFile {
            id: id.to_owned(),
            name: name.to_owned(),
            path: format!("docs/{name}"),
            hash: hash.map(str::to_owned),
            folder: hash.is_none(),
            size: 12,
        }
    }

    #[test]
    fn a_file_is_found_by_its_name_and_by_the_words_its_text_holds() {
        let files = TreeFiles::new("Lounge");
        let tree = || {
            vec![
                file("plan", "plan.md", Some(&"ab".repeat(32))),
                file("scan", "scan.pdf", Some(&"cd".repeat(32))),
                file("notes", "notes.txt", Some(&"ef".repeat(32))),
            ]
        };
        assert!(files.set_files(tree()));
        assert!(!files.set_files(tree()), "the same tree is not a new one");
        let (plan, scan) = ("ab".repeat(32), "cd".repeat(32));
        files.note_text(&plan, "# Plan\n\nThe boat leaves on Friday.\n".to_owned());
        files.note_text(&scan, "Quarterly report: revenue rose.".to_owned());

        let by_name = files.query("scan");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].title, "scan.pdf");
        assert!(matches!(by_name[0].target, Target::Content { .. }));

        let by_words = files.query("boat");
        assert_eq!(by_words.len(), 1, "the words inside a markdown file");
        assert_eq!(by_words[0].title, "plan.md");

        let in_pdf = files.query("revenue");
        assert_eq!(in_pdf.len(), 1, "the words inside a PDF");
        assert_eq!(in_pdf[0].title, "scan.pdf");

        assert!(files.query("harbour").is_empty());
    }

    #[test]
    fn a_folder_is_found_by_its_name_and_opens_files() {
        let files = TreeFiles::new("Lounge");
        files.set_files(vec![file("docs", "docs", None)]);
        let found = files.query("docs");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ResourceKind::Folder);
        assert!(matches!(found[0].target, Target::Launch { .. }));
    }
}
