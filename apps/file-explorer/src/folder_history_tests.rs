//! What the mount writes is the tree's own History: a file's changes are listed under who
//! made them, and History's undo and restore act on them as changes of their own.

#![allow(clippy::unwrap_used, reason = "test assertions")]

use std::collections::BTreeMap;

use ::tilo_fs::namespace::{Namespace, ROOT_INO, acting_as};
use fs_protocol::files::TreeChunk;
use fs_protocol::tree::{FilesTree, Tree, TreeChunks};
use ui::automerge::AutoCommit;
use ui::framework::history::{EDIT_TYPE, HistoryEdit, ResourceHistoryEdit};
use ui::history_source::AuthorDirectory;
use ui::history_source::folder::{FolderChange, FolderChunks, changes};

/// The documents of a namespace's tree, as a window holds them.
struct Folder {
    chunks: BTreeMap<TreeChunk, AutoCommit>,
}

impl Folder {
    fn of(namespace: &mut Namespace) -> Self {
        let chunks = namespace
            .records()
            .into_iter()
            .map(|chunk| {
                let bytes = namespace.save_document(chunk).unwrap();
                (chunk, AutoCommit::load(&bytes).unwrap())
            })
            .collect();
        Self { chunks }
    }

    /// Apply History's `edit` to chunk `chunk`, naming `node`, as the person at this window.
    fn apply(&mut self, chunk: TreeChunk, edit: HistoryEdit, node: &str) {
        let encoded = ui::tilo_actions::EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload: serde_json::to_vec(&edit).unwrap(),
        };
        let action = ResourceHistoryEdit::<FilesTree>::new(edit);
        let mut context = ui::tilo_actions::ActionContext::for_resource("tree", "tree", "Files")
            .with_invoker(ui::tilo_actions::ActionInvoker::person("ada"));
        context.item = Some(ui::tilo_actions::ActionItem {
            id: node.to_owned(),
            label: node.to_owned(),
        });
        let doc = self.chunks.get_mut(&chunk).unwrap();
        ui::tilo_actions::apply_local_with_context(doc, &action, encoded, None, context).unwrap();
    }

    fn hash_of(&self, node: &str) -> Option<String> {
        self.tree().unwrap().node(node).unwrap().hash.clone()
    }
}

impl FolderChunks for Folder {
    fn tree(&self) -> Option<Tree> {
        let mut chunks = TreeChunks::default();
        for (chunk, doc) in &self.chunks {
            chunks
                .read_resource(
                    &fs_protocol::tree::chunk_document_id(*chunk),
                    &doc.clone().save(),
                )
                .unwrap();
        }
        Some(chunks.tree())
    }

    fn read_chunk<T>(&self, chunk: TreeChunk, read: impl FnOnce(&AutoCommit) -> T) -> Option<T> {
        self.chunks.get(&chunk).map(read)
    }
}

fn hash(byte: u8) -> iroh_blobs::Hash {
    iroh_blobs::Hash::from_bytes([byte; 32])
}

fn by_agent(change: &FolderChange) -> bool {
    change
        .entry
        .context
        .as_ref()
        .and_then(|context| context.invoker.as_ref())
        .and_then(|invoker| invoker.acting.agent_id())
        == Some("claude")
}

/// A file the person made, which an agent session then wrote through the mount.
fn written_by_an_agent() -> (Folder, String, fs_protocol::FileDocumentId) {
    let mut namespace = Namespace::new("ada").unwrap();
    let ino = namespace
        .create_file(ROOT_INO, "plan.md", Some(&hash(1)), 4, 1)
        .unwrap();
    {
        let agent = ui::tilo_actions::ActionInvoker::admitted(
            &ui::tilo_actions::Acting::agent("claude").in_session("s1"),
        )
        .named(Some("Tidy the notes".into()));
        let _acting = acting_as(Some(
            ui::tilo_actions::ActionContext::default().with_invoker(agent),
        ));
        assert!(namespace.set_content(ino, Some(&hash(2)), 5, 2));
    }
    let node = namespace.id_for_ino(ino).unwrap().to_owned();
    let document = namespace.document_id_for(ino).unwrap();
    (Folder::of(&mut namespace), node, document)
}

#[test]
fn an_agents_write_is_in_the_files_history_and_undone_from_it() {
    let (mut folder, node, document) = written_by_an_agent();
    let listed = changes(&folder, &AuthorDirectory::new(), Some(document));
    let write = listed
        .iter()
        .find(|change| by_agent(change))
        .unwrap_or_else(|| panic!("the agent's write is the file's: {listed:#?}"));
    assert_eq!(write.node.as_deref(), Some(node.as_str()));
    assert_eq!(folder.hash_of(&node), Some(hash(2).to_hex()));

    let by = write
        .entry
        .context
        .as_ref()
        .and_then(|context| context.invoker.as_ref())
        .map(|invoker| invoker.acting.by.clone());
    let edit = HistoryEdit::Undo {
        steps: None,
        invocation: None,
        by,
        to: None,
        step: Some(write.hash.to_string()),
    };
    folder.apply(write.chunk, edit, &node);
    assert_eq!(
        folder.hash_of(&node),
        Some(hash(1).to_hex()),
        "the file is back as the person made it"
    );
    let after = changes(&folder, &AuthorDirectory::new(), Some(document));
    assert_eq!(
        after.len(),
        listed.len() + 1,
        "the undo is a change of its own: {after:#?}"
    );
}

#[test]
fn a_restore_to_an_older_version_is_one_history_step() {
    let (mut folder, node, document) = written_by_an_agent();
    let listed = changes(&folder, &AuthorDirectory::new(), Some(document));
    let made = listed
        .iter()
        .find(|change| !by_agent(change))
        .unwrap_or_else(|| panic!("the person made the file: {listed:#?}"));
    let edit = HistoryEdit::Restore {
        version: made.hash.to_string(),
        ids: Some(vec![node.clone()]),
    };
    folder.apply(made.chunk, edit, &node);
    assert_eq!(folder.hash_of(&node), Some(hash(1).to_hex()));
    let after = changes(&folder, &AuthorDirectory::new(), Some(document));
    assert_eq!(after.len(), listed.len() + 1, "one step: {after:#?}");
}

/// The person at `[3; 32]`, as the workspace names them.
fn ada() -> ui::tilo_actions::MemberId {
    ui::tilo_actions::MemberId::person("ada")
}

/// A document the person copied into the workspace's folder on their computer: `letter.txt`,
/// which the mount writes as their certified device.
fn copied_into_the_mount() -> (Folder, String) {
    let mut namespace = ::tilo_fs::namespace::Namespace::new(
        ::tilo_fs::namespace::MountAuthor::person("laptop", ada()),
    )
    .unwrap();
    let ino = namespace
        .create_file(ROOT_INO, "letter.txt", Some(&hash(3)), 3, 1)
        .unwrap();
    let node = namespace.id_for_ino(ino).unwrap().to_owned();
    (Folder::of(&mut namespace), node)
}

#[test]
fn a_file_copied_into_the_mount_reads_as_the_person_whose_device_it_is() {
    let (folder, node) = copied_into_the_mount();
    let mut authors = AuthorDirectory::new();
    authors.register_member(
        &ada(),
        ui::history_source::author_for_identity("Ada", "ada"),
    );
    let listed = changes(&folder, &authors, None);
    let copied = listed
        .iter()
        .find(|change| change.node.as_deref() == Some(node.as_str()))
        .unwrap_or_else(|| panic!("the copy is a change of the file: {listed:#?}"));
    assert_eq!(
        copied.entry.author.name, "Ada",
        "the person who copied it in, never a member nobody can name"
    );
}
