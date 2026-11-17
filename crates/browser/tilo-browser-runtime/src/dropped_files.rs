//! Where files dropped on the page go: into the workspace's file tree, the one place a workspace's
//! files are kept, which a computer that logged in mounts as a folder and an agent lists and reads.
//!
//! A drop is copied in as it was: every file at its path in what was dropped (`docs/2026/plan.md`
//! for a folder `docs`), each folder made once, into the folder the drop fell on (the tree's root
//! for a drop on the shell). A name the folder already has is kept beside it, `plan (2).md`, as a
//! file manager keeps both. The bytes are the file's content manifest and its chunks, kept where
//! the page keeps the workspace's content, so the workspace's host fetches them from the page by
//! their hashes as it fetches a mounted computer's ([`fs_content`]).
//!
//! The tree is kept in chunks, each a record of its own ([`fs_protocol::tree`]): a node goes to
//! the newest chunk while it has room and the next one after, and the tree's own record lists
//! every chunk a node went to ([`TreeWrite`]).
//!
//! Then each file opens where it fell: in the app that opens what it is ([`opens_in`]), or Files,
//! showing it, when no app does.

use std::collections::{BTreeMap, BTreeSet};

use fs_protocol::files::{FIRST_TREE_CHUNK, TreeChunk};
use fs_protocol::tree::{self, Placing, ROOT_ID, Tree};

/// One file of a drop, its content kept: its path in what was dropped, and its manifest's hash
/// (hex) and length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Kept {
    pub path: String,
    pub hash: String,
    pub size: u64,
}

/// One file the drop put into the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Landed {
    /// Its path in what was dropped.
    pub path: String,
    /// Its node in the tree.
    pub node: String,
    /// The name it took in its folder: the dropped one, or one beside it.
    pub name: String,
}

/// What a drop put into the tree: each file, and each thing dropped at its top (a file or a
/// folder) under the name it took in the folder the drop fell on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Placed {
    pub files: Vec<Landed>,
    pub top: Vec<String>,
}

/// Where a drop writes the tree: each chunk's document, and the tree's own record that lists the
/// chunks it wrote into. The page's records are one ([`crate::browser::drops`]); a test's own
/// documents are another.
pub(crate) trait TreeWrite {
    /// Put a node into chunk `chunk` of `tree`, which takes it too.
    fn put(
        &mut self,
        chunk: TreeChunk,
        tree: &mut Tree,
        parent: &str,
        name: &str,
        placing: Placing<'_>,
        when: (u64, &str),
    ) -> Result<tree::Placed, String>;

    /// List chunk `chunk` in the tree's own record; listing one already listed changes nothing.
    fn list(&mut self, chunk: TreeChunk) -> Result<(), String>;
}

/// Put the dropped `files` into the folder `into` of `tree`, as `author` at `mtime`
/// (milliseconds since the epoch), each node into the chunk [`Tree::chunk_for_new`] names and
/// the chunks written into listed in the tree's own record.
///
/// # Errors
///
/// A path with a part no file can be named (an empty part, `.`, `..`), a folder that is gone, or
/// a document that refused the write; what was placed before it stays.
pub(crate) fn place(
    write: &mut impl TreeWrite,
    tree: &mut Tree,
    into: &str,
    files: &[Kept],
    (mtime, author): (u64, &str),
) -> Result<Placed, String> {
    let mut placed = Placed::default();
    let mut written = BTreeSet::new();
    // Each folder of the drop, by its path in it, made once.
    let mut folders: BTreeMap<String, String> = BTreeMap::new();
    for file in files {
        let parts: Vec<&str> = file.path.split('/').collect();
        let Some((name, folders_of)) = parts.split_last() else {
            continue;
        };
        let mut parent = into.to_owned();
        let mut at = String::new();
        for (depth, folder) in folders_of.iter().enumerate() {
            if !at.is_empty() {
                at.push('/');
            }
            at.push_str(folder);
            parent = if let Some(id) = folders.get(&at) {
                id.clone()
            } else {
                let chunk = tree.chunk_for_new();
                let made = write.put(
                    chunk,
                    tree,
                    &parent,
                    folder,
                    Placing::Folder,
                    (mtime, author),
                )?;
                written.insert(chunk);
                if depth == 0 {
                    placed.top.push(made.name.clone());
                }
                folders.insert(at.clone(), made.id.clone());
                made.id
            };
        }
        let content = Placing::File {
            hash: &file.hash,
            size: file.size,
        };
        let chunk = tree.chunk_for_new();
        let made = write.put(chunk, tree, &parent, name, content, (mtime, author))?;
        written.insert(chunk);
        if folders_of.is_empty() {
            placed.top.push(made.name.clone());
        }
        placed.files.push(Landed {
            path: file.path.clone(),
            node: made.id,
            name: made.name,
        });
    }
    for chunk in written {
        write.list(chunk)?;
    }
    Ok(placed)
}

/// The chunks placing `files` into `tree` will write into: each node made goes to the newest
/// chunk while it has room, and to the ones after it once it is full, so a caller opens every
/// record a drop touches before it writes.
pub(crate) fn chunks_for(tree: &Tree, files: &[Kept]) -> BTreeSet<TreeChunk> {
    // Every file is a node, and every folder of its path — each made once — another.
    let mut folders: BTreeSet<String> = BTreeSet::new();
    for file in files {
        let parts: Vec<&str> = file.path.split('/').collect();
        if let Some((_, of)) = parts.split_last() {
            let mut at = String::new();
            for folder in of {
                if !at.is_empty() {
                    at.push('/');
                }
                at.push_str(folder);
                folders.insert(at.clone());
            }
        }
    }
    let nodes = files.len().saturating_add(folders.len());
    // The counts are of the tree's nodes, as `place` finds them.
    let mut counts: BTreeMap<TreeChunk, usize> = BTreeMap::new();
    for node in tree.nodes.values() {
        let chunk = node.document_id.map_or(FIRST_TREE_CHUNK, |id| id.chunk());
        let count = counts.entry(chunk).or_default();
        *count = count.saturating_add(1);
    }
    let mut newest = counts.last_key_value().map(|(chunk, _)| *chunk);
    let mut held = newest.map_or(0, |chunk| counts.get(&chunk).copied().unwrap_or(0));
    let mut chunks = BTreeSet::new();
    for _ in 0..nodes {
        let chunk = tree::chunk_for_new(newest, held);
        chunks.insert(chunk);
        if newest != Some(chunk) {
            newest = Some(chunk);
            held = 0;
        }
        held = held.saturating_add(1);
    }
    chunks
}

/// The tree's root: where a drop on the shell goes. The workspace's folder as a computer that
/// logged in mounts it, so `ls` there shows what was just dropped, beside what the person keeps.
pub(crate) const SHELL_FOLDER: &str = ROOT_ID;

/// The app (its package) that opens a file named `name` whose first bytes are `head`: the app
/// its kind lands in ([`tilo_content_types::landing`]). `None` when no app opens it.
pub(crate) fn opens_in(head: &[u8], name: &str) -> Option<String> {
    let file = name.rsplit('/').next().unwrap_or(name);
    tilo_content_types::landing(head, file).map(|landing| format!("so.tilo.{}", landing.app))
}

/// How one line names `count` files.
pub(crate) fn files(count: usize) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

/// What a finished drop says where it fell: what it added, and where. `top` is what was dropped
/// as the folder it fell on names it, `folder` that folder's name (`None` for the workspace's own).
pub(crate) fn added_line(top: &[String], count: usize, folder: Option<&str>) -> String {
    let what = match top {
        [one] => one.clone(),
        _ => files(count),
    };
    match folder {
        Some(folder) => format!("Added {what} to {folder}"),
        None => format!("Added {what} to the workspace"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_protocol::files::FileNodeKind;
    use ui::automerge::AutoCommit as Doc;

    /// The page's documents, in the test's own memory: the tree's own record and each chunk's.
    struct Docs {
        index: Doc,
        chunks: BTreeMap<TreeChunk, Doc>,
    }

    impl Default for Docs {
        fn default() -> Self {
            Self {
                index: Doc::new(),
                chunks: BTreeMap::new(),
            }
        }
    }

    impl TreeWrite for Docs {
        fn put(
            &mut self,
            chunk: TreeChunk,
            tree: &mut Tree,
            parent: &str,
            name: &str,
            placing: Placing<'_>,
            when: (u64, &str),
        ) -> Result<tree::Placed, String> {
            let doc = self.chunks.entry(chunk).or_default();
            tree::place((doc, chunk), tree, parent, name, placing, when)
                .map_err(|error| error.to_string())
        }

        fn list(&mut self, chunk: TreeChunk) -> Result<(), String> {
            tree::list_chunk(&mut self.index, chunk)
                .map(drop)
                .map_err(|error| error.to_string())
        }
    }

    impl Docs {
        /// The tree a device reads from the records: the tree's own and each chunk's.
        fn tree(&self) -> Tree {
            let index = self.index.clone().save();
            let chunks: Vec<Vec<u8>> = self.chunks.values().map(|doc| doc.clone().save()).collect();
            Tree::of_records(Some(&index), chunks.iter().map(Vec::as_slice)).unwrap()
        }
    }

    fn kept(path: &str) -> Kept {
        Kept {
            path: path.into(),
            hash: blake(path),
            size: path.len() as u64,
        }
    }

    fn blake(text: &str) -> String {
        hex::encode(text.as_bytes())
            .chars()
            .cycle()
            .take(64)
            .collect()
    }

    fn path_of(tree: &Tree, id: &str) -> String {
        let mut parts = Vec::new();
        let mut at = Some(id.to_owned());
        while let Some(id) = at.filter(|id| id != ROOT_ID) {
            let node = tree.node(&id).unwrap();
            parts.push(String::from_utf8_lossy(node.name.as_bytes()).into_owned());
            at = node.parent.clone();
        }
        parts.reverse();
        parts.join("/")
    }

    #[test]
    fn a_dropped_folder_keeps_its_names_and_subfolders_and_a_taken_name_is_kept_beside() {
        let mut docs = Docs::default();
        let mut tree = Tree::default();
        let first = [kept("plan.pdf")];
        place(&mut docs, &mut tree, SHELL_FOLDER, &first, (1, "page")).unwrap();
        let drop = [
            kept("docs/2026/plan.md"),
            kept("docs/2026/budget.xlsx"),
            kept("docs/readme.txt"),
            kept("plan.pdf"),
        ];
        let placed = place(&mut docs, &mut tree, SHELL_FOLDER, &drop, (2, "page")).unwrap();
        assert_eq!(placed.top, ["docs", "plan (2).pdf"]);
        let paths: Vec<String> = placed
            .files
            .iter()
            .map(|file| path_of(&tree, &file.node))
            .collect();
        assert_eq!(
            paths,
            [
                "docs/2026/plan.md",
                "docs/2026/budget.xlsx",
                "docs/readme.txt",
                "plan (2).pdf"
            ]
        );
        // One folder each, however many files are in it.
        let folders = tree
            .nodes
            .values()
            .filter(|node| node.kind == FileNodeKind::Directory)
            .count();
        assert_eq!(folders, 2);
        // What the page wrote is what any device reads from the records.
        assert_eq!(docs.tree(), tree);
        // Every node went into a chunk the tree's own record lists.
        assert_eq!(tree::listed_chunks(&docs.index), [1]);
        let plan = tree.node(&placed.files[0].node).unwrap();
        assert_eq!(
            plan.hash.as_deref(),
            Some(blake("docs/2026/plan.md").as_str())
        );
        // The same folder dropped again sits beside the first.
        let again = place(&mut docs, &mut tree, SHELL_FOLDER, &drop[..1], (3, "page")).unwrap();
        assert_eq!(again.top, ["docs (2)"]);
    }

    /// A drop that fills the newest chunk starts the next one, and the tree's own record lists
    /// both: a tree the page writes is one a device reads whole.
    #[test]
    fn a_drop_that_fills_a_chunk_starts_the_next_and_lists_both() {
        let mut docs = Docs::default();
        let mut tree = Tree::default();
        let nodes = tree::CHUNK_NODES;
        let drop: Vec<Kept> = (0..nodes).map(|n| kept(&format!("f{n}.txt"))).collect();
        assert_eq!(chunks_for(&tree, &drop), BTreeSet::from([1]));
        place(&mut docs, &mut tree, SHELL_FOLDER, &drop, (1, "page")).unwrap();
        assert_eq!(tree.nodes.len(), nodes);
        // The next file goes to the chunk after it.
        let more = [kept("last.txt")];
        assert_eq!(chunks_for(&tree, &more), BTreeSet::from([2]));
        place(&mut docs, &mut tree, SHELL_FOLDER, &more, (2, "page")).unwrap();
        assert_eq!(tree::listed_chunks(&docs.index), [1, 2]);
        assert_eq!(docs.tree(), tree);
    }

    #[test]
    fn a_drop_on_a_folder_goes_into_it_and_a_name_no_file_has_is_refused() {
        let mut docs = Docs::default();
        let mut tree = Tree::default();
        let made = place(
            &mut docs,
            &mut tree,
            SHELL_FOLDER,
            &[kept("inbox/a.txt")],
            (1, "p"),
        )
        .unwrap();
        let inbox = fs_protocol::tree::folder_named(&tree, ROOT_ID, "inbox").unwrap();
        let into = place(&mut docs, &mut tree, &inbox, &[kept("b.txt")], (2, "p")).unwrap();
        assert_eq!(path_of(&tree, &into.files[0].node), "inbox/b.txt");
        assert_eq!(made.top, ["inbox"]);
        assert!(
            place(
                &mut docs,
                &mut tree,
                SHELL_FOLDER,
                &[kept("a/../b")],
                (3, "p")
            )
            .is_err()
        );
    }

    #[test]
    fn each_file_opens_in_the_app_its_kind_lands_in() {
        assert_eq!(
            opens_in(b"%PDF-1.7\n", "scan.pdf").as_deref(),
            Some("so.tilo.pdf-viewer")
        );
        assert_eq!(
            opens_in(b"\x89PNG\r\n\x1a\n....", "x.png").as_deref(),
            Some("so.tilo.photos")
        );
        assert_eq!(
            opens_in(b"# Plan\n", "docs/plan.md").as_deref(),
            Some("so.tilo.notes")
        );
        assert_eq!(opens_in(b"{\"a\":1}", "data.json"), None);
        assert_eq!(
            added_line(&["plan.pdf".into()], 1, None),
            "Added plan.pdf to the workspace"
        );
        assert_eq!(
            added_line(&["a".into(), "b".into()], 2, Some("docs")),
            "Added 2 files to docs"
        );
    }
}
