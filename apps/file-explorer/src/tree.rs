//! The workspace's files in a page: its tree, the Files record every member
//! of a workspace holds ([`fs_protocol::tree`]), listed where a person can
//! open them.
//!
//! A computer's Files browses that record through the folder it mounts. A
//! page has no disk and mounts nothing, so this surface reads the record
//! itself, over the page's session: the folder the window shows, the folders
//! and files in it with their size, when they changed and who changed them,
//! and the content of a file by the hash its node names, which the page asks
//! the workspace for. Opening a file hands that content to the app the file's
//! bytes say opens it, as a window on a computer does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{FILES_APP, ROOT_ID, TREE_KIND, Tree, TreeEntry};
use ui::framework::{Dispatcher, Live, People, ResourceApp, ViewCx, WorkspaceRecord};
use ui::i18n::EmbeddedCatalog;
use ui::icons::Icon;
use ui::portal::PickRequest;
use ui::tilo_actions::remote::ApplicationLaunch;
use ui::tilo_actions::{ActionBinding, ResourceSpec};
use ui::tilo_resource::{self, Atom, Model, resource};
use ui::widget::controls::ActionControls;
use ui::widget::{
    ActionEmitter, Button, Column, Decorator, Element, EmptyState, ListRow, Row, Text, Widget as _,
};

use crate::i18n::{format_bytes, format_datetime, t};

/// The window's own record: the folder of the workspace's tree it shows, so a
/// window comes back to where the person left it.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct FilesWindow {
    /// The node of the tree the window shows; empty for the workspace's root.
    pub folder: Atom,
}

#[resource(
    kind = "files.workspace",
    label = "files-workspace-window",
    search = FilesWindow::search
)]
impl FilesWindow {}

impl FilesWindow {
    /// Nothing: a workspace's files are found by name and by what their text
    /// holds in the tree's own search source, not as rows of this record.
    fn search(_: &Self) -> Vec<ui::tilo_resource::SearchEntry> {
        Vec::new()
    }
}

/// Files in a page: the workspace's tree, over the page's session.
pub struct FilesTreeShell;

impl ResourceApp for FilesTreeShell {
    type Resource = FilesWindow;
    const ID: &'static str = "so.tilo.file-explorer";
    const TITLE: &'static str = "Files";
    const TITLE_KEY: &'static str = "files-app-title";
    const ICON: Option<&'static str> = Some("file-explorer");

    fn view(cx: &mut ViewCx<FilesWindow>) -> Element {
        Box::new(TreeBrowser::new(cx))
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }
}

/// The bytes of content the window holds, by hash. Reading content the page
/// lacks is what asks the workspace for it.
type Reads = Rc<dyn Fn(&str) -> Option<Vec<u8>>>;

/// A file the person asked to open, waiting for the bytes that say which app
/// opens it.
struct Asked {
    hash: String,
    name: String,
}

/// A file or folder the person asked to download, waiting for the bytes of
/// every file in it.
#[derive(Clone)]
struct Download {
    /// The name it is saved under: the file's own, or the folder's with `.zip`.
    name: String,
    /// Each file, by its path within the download and the hash of its bytes.
    files: Vec<(String, String)>,
    /// A folder downloads as a zip of its files.
    folder: bool,
}

/// What the rows were built from, so they are built again only when it moved.
struct Shown {
    folder: String,
    tree: Option<Rc<Tree>>,
    names: u64,
}

/// The app that opens the file `name`, whose first bytes are `head`: the
/// bytes decide, as they do wherever a workspace lands a file in an app, and
/// the name only says which of them are text.
fn opening_app(head: &[u8], name: &str) -> Option<String> {
    let landing = tilo_content_types::landing(head, name)?;
    Some(format!("so.tilo.{}", landing.app))
}

/// The window: the folder it shows and the entries in it.
pub struct TreeBrowser {
    live: Live<FilesWindow>,
    /// The workspace's tree, as the page holds it.
    tree: Option<WorkspaceRecord<fs_protocol::tree::FilesTree>>,
    /// What the workspace calls the people its nodes name.
    people: People,
    /// The window's own record, which the open folder is set in.
    dispatch: Dispatcher<FilesWindow>,
    read: Option<Reads>,
    body: Column,
    shown: Option<Shown>,
    structure_dirty: bool,
    /// Files asked for whose bytes have not arrived yet, shared with the rows
    /// that asked.
    asked: Rc<RefCell<Vec<Asked>>>,
    /// The handoff that opens a file, by the app that opens its type.
    handoffs: RefCell<HashMap<String, ActionEmitter<String>>>,
    /// Downloads asked for whose bytes have not all arrived yet.
    downloads: Rc<RefCell<Vec<Download>>>,
}

impl TreeBrowser {
    fn new(cx: &ViewCx<FilesWindow>) -> Self {
        // A node names its file by the hash of the file's content manifest: the file is the
        // chunks the manifest lists, each read (and asked for) by its own hash.
        let read: Option<Reads> = cx.content().map(|content| {
            Rc::new(move |hash: &str| {
                content.read(hash).and_then(|bytes| {
                    fs_content::whole(bytes, u64::MAX, |chunk| content.read(&hex::encode(chunk)))
                })
            }) as Rc<dyn Fn(&str) -> Option<Vec<u8>>>
        });
        let mut browser = Self {
            live: cx.live().clone(),
            tree: cx.shared_record::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &collab_types::person_record_id(TREE_KIND),
            ),
            people: cx.people(),
            dispatch: cx.dispatcher(),
            read,
            body: Column::new(),
            shown: None,
            structure_dirty: true,
            asked: Rc::default(),
            handoffs: RefCell::new(HashMap::new()),
            downloads: Rc::default(),
        };
        browser.refresh();
        browser
    }

    /// The folder the window shows: the one its record names, or the root
    /// when it names none the tree holds.
    fn folder(&self, tree: Option<&Tree>) -> String {
        let named = self.live.get().folder.0.clone();
        if named.is_empty() {
            return ROOT_ID.to_owned();
        }
        match tree {
            Some(tree) if tree.node(&named).is_none() || tree.trashed(&named) => ROOT_ID.to_owned(),
            _ => named,
        }
    }

    /// Build the rows again when the tree, the open folder or the people it
    /// names moved; open what the person asked for once its bytes are here.
    fn refresh(&mut self) -> bool {
        self.open_what_arrived();
        self.save_what_arrived();
        let tree = self.tree.as_ref().map(WorkspaceRecord::get);
        let folder = self.folder(tree.as_deref());
        let names = self.people.revision().get();
        let moved = self.shown.as_ref().is_none_or(|shown| {
            shown.folder != folder
                || shown.names != names
                || !match (&shown.tree, &tree) {
                    (Some(before), Some(now)) => Rc::ptr_eq(before, now),
                    (None, None) => true,
                    _ => false,
                }
        });
        if !moved {
            return false;
        }
        self.body = self.rows(tree.as_deref(), &folder);
        self.shown = Some(Shown {
            folder,
            tree,
            names,
        });
        self.structure_dirty = true;
        true
    }

    /// The window's content: where the open folder is, then its entries.
    fn rows(&self, tree: Option<&Tree>, folder: &str) -> Column {
        let mut body = Column::new()
            .with_id("files-tree")
            .push(self.crumbs(tree, folder));
        let entries: Vec<(&str, &TreeEntry)> = tree.map_or_else(Vec::new, |tree| {
            tree.children(folder)
                .into_iter()
                .filter(|(id, _)| !tree.trashed(id))
                .collect()
        });
        if entries.is_empty() {
            return body.push(Box::new(
                EmptyState::new(t("files-tree-empty")).with_icon(Icon::Folder),
            ));
        }
        for (id, node) in entries {
            let download = tree.map(|tree| Self::download_of(tree, id, node));
            body = body.push(self.row(id, node, download));
        }
        body
    }

    /// Where the open folder is, and the way up to the folder holding it.
    fn crumbs(&self, tree: Option<&Tree>, folder: &str) -> Box<dyn ui::widget::Widget> {
        let mut row = Row::new().with_id("files-tree-crumbs").with_gap(8.0);
        if folder != ROOT_ID {
            let parent = tree
                .and_then(|tree| tree.node(folder))
                .and_then(|node| node.parent.clone())
                .unwrap_or_else(|| ROOT_ID.to_owned());
            let dispatch = self.dispatch.clone();
            row = row.push(Box::new(
                Button::new("files-tree-up", &t("files-tree-up"))
                    .with_icon(Icon::ArrowUp)
                    .on_click(move || {
                        dispatch.submit(FilesWindow::paths().folder.set(Atom(parent.clone())));
                    }),
            ));
        }
        let path = tree.map_or_else(String::new, |tree| Self::path_of(tree, folder));
        Box::new(row.push(Text::new(&path).strong().into_widget()))
    }

    /// One entry of the open folder: its name, its size and when and by whom
    /// it last changed; choosing it opens the folder or the file.
    fn row(
        &self,
        id: &str,
        node: &TreeEntry,
        download: Option<Download>,
    ) -> Box<dyn ui::widget::Widget> {
        let name = Self::name_of(node);
        let is_folder = node.kind == FileNodeKind::Directory;
        let mut row = ListRow::new(id.to_owned(), name.clone()).secondary(Self::details(node));
        // Getting it out of the workspace onto this computer: the file, or
        // the folder as one zip of everything in it.
        if let Some(download) = download {
            let label = t(if is_folder {
                "files-tree-download-folder"
            } else {
                "files-tree-download"
            });
            let downloads = Rc::clone(&self.downloads);
            let read = self.read.clone();
            row = row.action(Icon::Download, label, move || {
                if let Some(read) = &read {
                    // Reading content the page lacks asks the workspace for it.
                    for (_, hash) in &download.files {
                        let _ = read(hash);
                    }
                }
                downloads.borrow_mut().push(download.clone());
            });
        }
        if is_folder {
            row = row.mark(Icon::Folder);
        }
        if let Some(author) = node.author.as_deref() {
            row = row.meta(self.people.name(author));
        }
        if is_folder {
            let dispatch = self.dispatch.clone();
            let into = id.to_owned();
            return Box::new(row.on_view(move || {
                dispatch.submit(FilesWindow::paths().folder.set(Atom(into.clone())));
            }));
        }
        let Some(hash) = node.hash.clone() else {
            return Box::new(row);
        };
        let asked = Rc::clone(&self.asked);
        let read = self.read.clone();
        Box::new(row.on_view(move || {
            if let Some(read) = &read {
                // Reading content the page lacks asks the workspace for it;
                // the file opens once its bytes are here.
                let _ = read(&hash);
            }
            asked.borrow_mut().push(Asked {
                hash: hash.clone(),
                name: name.clone(),
            });
        }))
    }

    /// What downloading the entry `id` takes: the file itself, or every file
    /// under the folder, by its path from the folder's own place.
    fn download_of(tree: &Tree, id: &str, node: &TreeEntry) -> Download {
        let name = Self::name_of(node);
        if node.kind != FileNodeKind::Directory {
            return Download {
                files: node
                    .hash
                    .clone()
                    .map(|hash| (name.clone(), hash))
                    .into_iter()
                    .collect(),
                name,
                folder: false,
            };
        }
        let mut files = Vec::new();
        Self::files_under(tree, id, &name, &mut files);
        Download {
            name: format!("{name}.zip"),
            files,
            folder: true,
        }
    }

    /// Every file under the folder `id` (at `path` in the download), by its
    /// path and hash; trashed entries are left out.
    fn files_under(tree: &Tree, id: &str, path: &str, files: &mut Vec<(String, String)>) {
        for (child, node) in tree.children(id) {
            if tree.trashed(child) || child == id {
                continue;
            }
            let at = format!("{path}/{}", Self::name_of(node));
            if node.kind == FileNodeKind::Directory {
                Self::files_under(tree, child, &at, files);
            } else if let Some(hash) = node.hash.clone() {
                files.push((at, hash));
            }
        }
    }

    /// Save every download whose bytes are all here: a file as it is, a
    /// folder as one zip. The browser's own Save dialog asks where, or the
    /// browser downloads it when the bytes arrived after the click.
    fn save_what_arrived(&self) {
        let asked = std::mem::take(&mut *self.downloads.borrow_mut());
        let mut waiting: Vec<Download> = Vec::new();
        for download in asked {
            let contents: Option<Vec<Vec<u8>>> = self
                .read
                .as_ref()
                .and_then(|read| download.files.iter().map(|(_, hash)| read(hash)).collect());
            let Some(contents) = contents else {
                waiting.push(download);
                continue;
            };
            let bytes = if download.folder {
                let members: Vec<crate::store_zip::Member<'_>> = download
                    .files
                    .iter()
                    .zip(&contents)
                    .map(|((path, _), bytes)| crate::store_zip::Member { path, bytes })
                    .collect();
                match crate::store_zip::zip(&members) {
                    Ok(bytes) => bytes,
                    Err(why) => {
                        tracing::warn!("{}: {why}", download.name);
                        continue;
                    }
                }
            } else {
                contents.into_iter().next().unwrap_or_default()
            };
            let request = PickRequest::save(
                &t("files-tree-save-title"),
                &t("files-tree-save"),
                &download.name,
            );
            // The browser takes it from here: its Save dialog, or its downloads.
            drop(request.send_save_contents(bytes));
        }
        *self.downloads.borrow_mut() = waiting;
    }

    /// An entry's name, whose bytes are not necessarily UTF-8.
    fn name_of(node: &TreeEntry) -> String {
        String::from_utf8_lossy(node.name.as_bytes()).into_owned()
    }

    /// What a row says under the entry's name: how large a file is and when
    /// it last changed.
    fn details(node: &TreeEntry) -> String {
        let mut parts: Vec<String> = Vec::new();
        if node.kind == FileNodeKind::File {
            parts.push(format_bytes(node.size));
        }
        if let Some(at) =
            chrono::DateTime::from_timestamp_millis(i64::try_from(node.mtime).unwrap_or(i64::MAX))
        {
            parts.push(format_datetime(at.with_timezone(&chrono::Local)));
        }
        parts.join(" · ")
    }

    /// The path of `folder` from the workspace's root, as `a / b`.
    fn path_of(tree: &Tree, folder: &str) -> String {
        let mut names: Vec<String> = Vec::new();
        let mut at = folder.to_owned();
        // A node names its parent, so the path is read up from the folder and
        // turned round; a tree that points back on itself ends the walk at the
        // repeat rather than looping.
        while at != ROOT_ID {
            let Some(node) = tree.node(&at) else {
                break;
            };
            names.push(Self::name_of(node));
            match node.parent.clone() {
                Some(parent) if parent != at => at = parent,
                _ => break,
            }
        }
        names.reverse();
        names.join(" / ")
    }

    /// Open every file the person asked for whose bytes are here, in the app
    /// its bytes say opens it.
    fn open_what_arrived(&self) {
        let asked = std::mem::take(&mut *self.asked.borrow_mut());
        let mut waiting: Vec<Asked> = Vec::new();
        for ask in asked {
            match self.read.as_ref().and_then(|read| read(&ask.hash)) {
                Some(bytes) => self.hand_off(&ask, &bytes),
                // Asked for again, so content the workspace has yet to answer
                // with is asked once per wake rather than once per frame.
                None => waiting.push(ask),
            }
        }
        *self.asked.borrow_mut() = waiting;
    }

    /// Hand the file `ask` names to the app that opens what its bytes are.
    fn hand_off(&self, ask: &Asked, bytes: &[u8]) {
        let head = bytes
            .get(..bytes.len().min(tilo_content_types::HEAD_BYTES))
            .unwrap_or(bytes);
        let Some(app) = opening_app(head, &ask.name) else {
            tracing::info!("no app in this page opens {}", ask.name);
            return;
        };
        let mut handoffs = self.handoffs.borrow_mut();
        if let std::collections::hash_map::Entry::Vacant(slot) = handoffs.entry(app.clone()) {
            let Some(built) = Self::handoff(&app, &t("files-tree-open")) else {
                tracing::warn!("the handoff of {app} cannot be bound");
                return;
            };
            slot.insert(built);
        }
        if let Some(handoff) = handoffs.get_mut(&app)
            && let Err(error) = handoff.emit(ask.hash.clone())
        {
            tracing::warn!("opening {} in {app}: {error}", ask.name);
        }
    }

    /// The handoff that opens a file by its content in `app`, as Camera hands
    /// a capture to Photos: the app's own `open`, with the hash bound by the
    /// row that chose it.
    fn handoff(app: &str, label: &str) -> Option<ActionEmitter<String>> {
        let spec =
            ApplicationLaunch::content_spec(app, "open", label, &t("files-tree-open-detail"));
        let resource =
            ResourceSpec::new(format!("{app}.app"), "applications.app", label).with_action(spec);
        let binding = resource.actions.first().and_then(|spec| {
            ActionBinding::new(&resource, spec, |hash: String| {
                let mut arguments = serde_json::Map::new();
                arguments.insert("content_hash".into(), serde_json::Value::String(hash));
                arguments
            })
            .ok()
        });
        // The spec is this app's own, built from the resource a line above:
        // one it refuses is a change here, and the file stays unopened.
        binding.map(ActionEmitter::new)
    }
}

impl Decorator for TreeBrowser {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.body
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.body
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.refresh();
        self.body.sync(state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let rebuilt = self.refresh();
        let updated = self.body.update(state, path);
        rebuilt || updated || std::mem::take(&mut self.structure_dirty)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        own || self.body.take_structure_dirty()
    }

    fn name(&self) -> &'static str {
        "FilesTree"
    }
}

impl ActionControls for TreeBrowser {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.handoffs
            .borrow_mut()
            .values_mut()
            .find_map(ActionEmitter::take)
    }
}

#[cfg(test)]
mod tests {
    use ui::automerge::transaction::Transactable as _;
    use ui::automerge::{AutoCommit, ObjId, ObjType, ROOT};

    use super::*;
    use fs_protocol::files::FileNameV1;
    use fs_protocol::tree::{
        AUTHOR_KEY, HASH_KEY, KIND_KEY, MTIME_KEY, NAME_KEY, NODES_KEY, PARENT_KEY, RECORD_ID_KEY,
        SIZE_KEY,
    };

    /// A node of the tree `doc` holds: `id` under `parent`, called `name`.
    fn node(
        doc: &mut AutoCommit,
        nodes: &ObjId,
        id: &str,
        parent: &str,
        name: &str,
        file: Option<(&str, u64)>,
    ) {
        let entry = doc.put_object(nodes, id, ObjType::Map).unwrap();
        doc.put(&entry, PARENT_KEY, parent).unwrap();
        doc.put(&entry, NAME_KEY, name.as_bytes().to_vec()).unwrap();
        doc.put(&entry, RECORD_ID_KEY, uuid::Uuid::new_v4().to_string())
            .unwrap();
        doc.put(&entry, AUTHOR_KEY, "ada".to_owned()).unwrap();
        doc.put(&entry, MTIME_KEY, 1_767_225_600_000_u64).unwrap();
        if let Some((hash, size)) = file {
            doc.put(&entry, KIND_KEY, "file").unwrap();
            doc.put(&entry, HASH_KEY, hash).unwrap();
            doc.put(&entry, SIZE_KEY, size).unwrap();
        } else {
            doc.put(&entry, KIND_KEY, "dir").unwrap();
            doc.put(&entry, SIZE_KEY, 0_u64).unwrap();
        }
    }

    /// A workspace's tree: `docs/` holding `plan.md` and `scan.pdf`, and
    /// `notes.txt` beside it, written by Ada.
    fn workspace() -> Tree {
        let mut doc = AutoCommit::new();
        let map = doc.put_object(&ROOT, NODES_KEY, ObjType::Map).unwrap();
        node(&mut doc, &map, "docs", ROOT_ID, "docs", None);
        let (plan, scan, notes) = ("ab".repeat(32), "cd".repeat(32), "ef".repeat(32));
        node(
            &mut doc,
            &map,
            "plan",
            "docs",
            "plan.md",
            Some((plan.as_str(), 2048)),
        );
        node(
            &mut doc,
            &map,
            "scan",
            "docs",
            "scan.pdf",
            Some((scan.as_str(), 4096)),
        );
        node(
            &mut doc,
            &map,
            "notes",
            ROOT_ID,
            "notes.txt",
            Some((notes.as_str(), 12)),
        );
        <fs_protocol::tree::FilesTree as ui::tilo_resource::Resource>::read(&doc)
            .expect("the tree reads")
    }

    #[test]
    fn a_folder_lists_its_entries_with_their_size_and_who_wrote_them() {
        let tree = workspace();
        let names: Vec<String> = tree
            .children("docs")
            .into_iter()
            .map(|(_, node)| String::from_utf8_lossy(node.name.as_bytes()).into_owned())
            .collect();
        assert_eq!(names, ["plan.md", "scan.pdf"]);
        let (_, plan) = tree.children("docs")[0];
        assert_eq!(plan.kind, FileNodeKind::File);
        assert_eq!(plan.size, 2048);
        assert_eq!(plan.author.as_deref(), Some("ada"));
        let details = TreeBrowser::details(plan);
        assert!(details.starts_with("2 kB"), "{details}");
        assert!(
            details.contains(" · "),
            "the size, then when it changed: {details}"
        );
        assert_eq!(
            TreeBrowser::name_of(&TreeEntry {
                name: FileNameV1(b"stra\xc3\x9fe.txt".to_vec()),
                ..plan.clone()
            }),
            "straße.txt"
        );
    }

    #[test]
    fn the_path_of_a_folder_reads_from_the_root() {
        let tree = workspace();
        assert_eq!(TreeBrowser::path_of(&tree, "docs"), "docs");
        assert_eq!(TreeBrowser::path_of(&tree, ROOT_ID), "");
    }

    #[test]
    fn a_files_bytes_say_which_app_opens_it() {
        assert_eq!(
            opening_app(b"%PDF-1.7\n", "scan.pdf").as_deref(),
            Some("so.tilo.pdf-viewer")
        );
        assert_eq!(
            opening_app(b"# Plan\n\n- one\n", "plan.md").as_deref(),
            Some("so.tilo.notes")
        );
        assert_eq!(
            opening_app(b"plain words\n", "notes.txt").as_deref(),
            Some("so.tilo.notes")
        );
        assert_eq!(
            opening_app(b"\x89PNG\r\n\x1a\n....", "shot.txt").as_deref(),
            Some("so.tilo.photos")
        );
        assert_eq!(opening_app(&[0_u8, 1, 2, 3], "blob.bin"), None);
    }

    #[test]
    fn the_handoff_opens_the_app_the_bytes_name_with_the_files_content() {
        let handoff = TreeBrowser::handoff("so.tilo.notes", "Open").expect("the handoff binds");
        let hash = "ab".repeat(32);
        let invocation = handoff
            .binding()
            .invoke(hash.clone())
            .expect("the row's click is one invocation");
        assert_eq!(invocation.address.resource.as_str(), "so.tilo.notes.app");
        assert_eq!(invocation.address.action.as_str(), "open");
        let arguments = serde_json::Value::Object(invocation.arguments);
        assert_eq!(arguments["content_hash"].as_str(), Some(hash.as_str()));
    }
}
