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
use fs_protocol::tree::{ROOT_ID, Tree, TreeEntry};
use ui::carry_out::{ContentLinkAsk, ContentLinkFile, Offers};
use ui::framework::{Dispatcher, Live, People, PersonRecord, ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::icons::Icon;
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
    search = FilesWindow::search,
    read_content = FilesWindow::read_document
)]
impl FilesWindow {}

impl FilesWindow {
    /// What the record holds: the folder the window shows.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }

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
/// Keeps bytes in the content the window reads, answering their hash: a text
/// document's file, which the tree holds as a document rather than by hash.
type Keeps = Rc<dyn Fn(&[u8]) -> Option<String>>;

/// A file the person asked to open, waiting for the bytes that say which app
/// opens it.
struct Asked {
    hash: String,
    name: String,
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
    let landing = ui::content::landing(head, name)?;
    Some(format!("so.tilo.{}", landing.app))
}

/// The window: the folder it shows and the entries in it.
pub struct TreeBrowser {
    live: Live<FilesWindow>,
    /// The workspace's tree, as the page holds it.
    tree: Option<PersonRecord<fs_protocol::tree::FilesTree>>,
    /// What the workspace calls the people its nodes name.
    people: People,
    /// The window's own record, which the open folder is set in.
    dispatch: Dispatcher<FilesWindow>,
    read: Option<Reads>,
    keep: Option<Keeps>,
    body: Column,
    shown: Option<Shown>,
    structure_dirty: bool,
    /// Files asked for whose bytes have not arrived yet, shared with the rows
    /// that asked.
    asked: Rc<RefCell<Vec<Asked>>>,
    /// The handoff that opens a file, by the app that opens its type.
    handoffs: RefCell<HashMap<String, ActionEmitter<String>>>,
    /// What the rows shown now can be dragged out of the window with.
    offers: Offers,
}

impl TreeBrowser {
    fn new(cx: &ViewCx<FilesWindow>) -> Self {
        // A node names its file by the hash of the file's content manifest: the file is the
        // chunks the manifest lists, each read (and asked for) by its own hash.
        let read: Option<Reads> = cx.content().map(|content| {
            Rc::new(move |hash: &str| {
                content.read(hash).and_then(|bytes| {
                    ui::content::manifest::whole(bytes, u64::MAX, |chunk| {
                        content.read(&hex::encode(chunk))
                    })
                })
            }) as Rc<dyn Fn(&str) -> Option<Vec<u8>>>
        });
        let keep: Option<Keeps> = cx
            .content()
            .map(|content| Rc::new(move |bytes: &[u8]| content.put(bytes).ok()) as Keeps);
        let mut browser = Self {
            live: cx.live().clone(),
            tree: cx.shared_person::<fs_protocol::tree::FilesTree>(),
            people: cx.people(),
            dispatch: cx.dispatcher(),
            read,
            keep,
            body: Column::new(),
            shown: None,
            structure_dirty: true,
            asked: Rc::default(),
            handoffs: RefCell::new(HashMap::new()),
            offers: Offers::default(),
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
        let tree = self.tree.as_ref().map(PersonRecord::get);
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
        self.offers.clear();
        if entries.is_empty() {
            return body.push(Box::new(
                EmptyState::new(t("files-tree-empty")).with_icon(Icon::Folder),
            ));
        }
        for (id, node) in entries {
            let carried = tree.map(|tree| Self::carried(tree, id, node));
            body = body.push(self.row(id, node, carried));
        }
        body
    }

    /// Where the open folder is, and the way up to the folder holding it.
    fn crumbs(&self, tree: Option<&Tree>, folder: &str) -> Box<dyn ui::widget::Widget> {
        let mut row = Row::new()
            .with_id("files-tree-crumbs")
            .with_rhythm(ui::tokens::Rhythm::Related);
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
        carried: Option<ContentLinkAsk>,
    ) -> Box<dyn ui::widget::Widget> {
        let name = Self::name_of(node);
        let is_folder = node.kind == FileNodeKind::Directory;
        // How large it is, when it changed and who changed it are each their
        // own part of the row, not one line joined by a separator.
        let mut row = ListRow::new(id.to_owned(), name.clone());
        if let Some(changed) = Self::changed_of(node) {
            row = row.secondary(changed);
        }
        if let Some(size) = Self::size_of(node) {
            row = row.value(size);
        }
        // Getting it out of the workspace onto this computer, the file or
        // the folder as one zip of everything in it: saved where the person
        // chooses, or dragged onto the computer's desktop or file manager.
        // The page's host serves the bytes straight from the workspace, so
        // a file of any size goes without passing through the page.
        if let Some(carried) = carried.filter(|_| ui::carry_out::available()) {
            let label = t(if is_folder {
                "files-tree-download-folder"
            } else {
                "files-tree-download"
            });
            row = row.carries_out(self.offers.offer(carried.clone()));
            row = row.action(Icon::Download, label, move || {
                ui::carry_out::save(carried.clone());
            });
        }
        if is_folder {
            row = row.mark(Icon::Folder);
        }
        if let Some(author) = self.author_of(node) {
            // The person, and the agent when one wrote for them: what a
            // computer's Files says of the same change.
            let (person, agent) = ui::row_people(&author);
            let meta = agent.map_or(person.clone(), |agent| format!("{person} · {agent}"));
            row = row.meta(meta);
            if let Some(hover) = ui::attribution_hover(&author) {
                row = row.tooltip(hover);
            }
        } else if let Some(author) = node.author.as_deref() {
            // A tree that records no version here yet: the device that wrote
            // it, when the workspace names that device.
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
            return Box::new(self.document_row(row, id, node, name));
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

    /// The author of `node`'s newest version, as the folder's versions record
    /// it: the person whose device signed it, and the agent that wrote for them
    /// when one did. `None` for a node the tree holds no version of.
    fn author_of(&self, node: &TreeEntry) -> Option<ui::HistoryAuthor> {
        let record_id = node.record_id?;
        let tree = self.tree.as_ref()?;
        let (version, attribution) =
            tree.read(|doc| fs_protocol::history::latest(doc, record_id))??;
        let registry = self.people.registry();
        let folder = node
            .parent
            .as_deref()
            .filter(|parent| *parent != ROOT_ID)
            .map(|parent| {
                let held = self.tree.as_ref().map(WorkspaceRecord::get);
                let path = match held.as_deref() {
                    Some(tree) => Self::path_of(tree, parent),
                    None => String::new(),
                };
                format!("/{path}")
            });
        registry.resolve_attributed_file_actor_in(
            version.actor.person.as_bytes(),
            version.actor.device.as_bytes(),
            attribution
                .as_ref()
                .map(|attributed| &attributed.attribution),
            ui::history_source::WriterPlace {
                folder: folder.as_deref(),
            },
        )
    }

    /// A text document's row (a `.md`, an `.odt`): its content is the tree's
    /// own, so choosing it writes the file from the tree and opens that, as a
    /// computer's folder reads it.
    fn document_row(&self, row: ListRow, id: &str, node: &TreeEntry, name: String) -> ListRow {
        let (Some(tree), Some(keep)) = (self.tree.clone(), self.keep.clone()) else {
            return row;
        };
        if node.codec.is_none() {
            return row;
        }
        let asked = Rc::clone(&self.asked);
        let id = id.to_owned();
        row.on_view(move || {
            let file = tree
                .read(|doc| fs_protocol::tree::document_file(doc, &id, &ui::content::NoContent))
                .flatten();
            if let Some(hash) = file.as_deref().and_then(|file| keep(file)) {
                asked.borrow_mut().push(Asked {
                    hash,
                    name: name.clone(),
                });
            }
        })
    }

    /// What carrying the entry `id` out of the workspace takes: the file
    /// itself, or every file under the folder, by its path from the folder's
    /// own place, as one zip.
    fn carried(tree: &Tree, id: &str, node: &TreeEntry) -> ContentLinkAsk {
        let name = Self::name_of(node);
        if node.kind != FileNodeKind::Directory {
            return ContentLinkAsk {
                files: node
                    .hash
                    .clone()
                    .map(|hash| ContentLinkFile {
                        path: name.clone(),
                        hash,
                    })
                    .into_iter()
                    .collect(),
                name,
                zip: false,
            };
        }
        let mut files = Vec::new();
        Self::files_under(tree, id, &name, &mut files);
        ContentLinkAsk {
            name: format!("{name}.zip"),
            files,
            zip: true,
        }
    }

    /// Every file under the folder `id` (at `path` in the zip), by its path
    /// and hash; trashed entries are left out.
    fn files_under(tree: &Tree, id: &str, path: &str, files: &mut Vec<ContentLinkFile>) {
        for (child, node) in tree.children(id) {
            if tree.trashed(child) || child == id {
                continue;
            }
            let at = format!("{path}/{}", Self::name_of(node));
            if node.kind == FileNodeKind::Directory {
                Self::files_under(tree, child, &at, files);
            } else if let Some(hash) = node.hash.clone() {
                files.push(ContentLinkFile { path: at, hash });
            }
        }
    }

    /// An entry's name, whose bytes are not necessarily UTF-8.
    fn name_of(node: &TreeEntry) -> String {
        String::from_utf8_lossy(node.name.as_bytes()).into_owned()
    }

    /// How large a file is; a folder has no size of its own.
    fn size_of(node: &TreeEntry) -> Option<String> {
        (node.kind == FileNodeKind::File).then(|| format_bytes(node.size))
    }

    /// When the entry last changed.
    fn changed_of(node: &TreeEntry) -> Option<String> {
        chrono::DateTime::from_timestamp_millis(i64::try_from(node.mtime).unwrap_or(i64::MAX))
            .map(|at| format_datetime(at.with_timezone(&chrono::Local)))
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
            .get(..bytes.len().min(ui::content::HEAD_BYTES))
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
    use super::*;
    use fs_protocol::files::FileNameV1;

    /// A node `id` under `parent`, called `name`, last written by Ada: a file
    /// when it has content, else a folder.
    fn node(
        id: &str,
        parent: &str,
        name: &str,
        file: Option<(String, u64)>,
    ) -> (String, TreeEntry) {
        let (kind, hash, size) = match file {
            Some((hash, size)) => (FileNodeKind::File, Some(hash), size),
            None => (FileNodeKind::Directory, None, 0),
        };
        let entry = TreeEntry {
            parent: Some(parent.to_owned()),
            name: FileNameV1(name.as_bytes().to_vec()),
            kind,
            hash,
            size,
            mtime: 1_767_225_600_000,
            author: Some("ada".to_owned()),
            record_id: None,
            codec: None,
        };
        (id.to_owned(), entry)
    }

    /// A workspace's tree: `docs/` holding `plan.md` and `scan.pdf`, and
    /// `notes.txt` beside it, written by Ada.
    fn workspace() -> Tree {
        Tree {
            root_record_id: None,
            nodes: [
                node("docs", ROOT_ID, "docs", None),
                node("plan", "docs", "plan.md", Some(("ab".repeat(32), 2048))),
                node("scan", "docs", "scan.pdf", Some(("cd".repeat(32), 4096))),
                node("notes", ROOT_ID, "notes.txt", Some(("ef".repeat(32), 12))),
            ]
            .into_iter()
            .collect(),
        }
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
        assert_eq!(TreeBrowser::size_of(plan).as_deref(), Some("2 kB"));
        assert!(TreeBrowser::changed_of(plan).is_some(), "when it changed");
        let docs = tree.node("docs").expect("the docs folder");
        assert_eq!(TreeBrowser::size_of(docs), None, "a folder has no size");
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
