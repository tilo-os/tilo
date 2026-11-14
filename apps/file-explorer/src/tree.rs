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
use ui::tilo_actions::remote::ApplicationLaunch;
use ui::tilo_actions::{ActionBinding, ResourceSpec};
use ui::tilo_resource::{self, Atom, Model, resource};
use ui::widget::controls::ActionControls;
use ui::widget::{
    ActionEmitter, Button, Column, Decorator, Element, EmptyState, ListRow, Row, Text, Widget as _,
};

ui::embedded_catalog!(static CATALOG, ["en-US", "de-DE"]);

/// The catalogue's text for `key`.
fn t(key: &str) -> String {
    CATALOG.t(key)
}

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
        Some(&CATALOG)
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

/// What the rows were built from, so they are built again only when it moved.
struct Shown {
    folder: String,
    tree: Option<Rc<Tree>>,
    names: u64,
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
}

impl TreeBrowser {
    fn new(cx: &ViewCx<FilesWindow>) -> Self {
        let read: Option<Reads> = cx.content().map(|content| {
            Rc::new(move |hash: &str| content.read(hash)) as Rc<dyn Fn(&str) -> Option<Vec<u8>>>
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
            body = body.push(self.row(id, node));
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
    fn row(&self, id: &str, node: &TreeEntry) -> Box<dyn ui::widget::Widget> {
        let name = Self::name_of(node);
        let is_folder = node.kind == FileNodeKind::Directory;
        let mut row = ListRow::new(id.to_owned(), name.clone()).secondary(Self::details(node));
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

    /// An entry's name, whose bytes are not necessarily UTF-8.
    fn name_of(node: &TreeEntry) -> String {
        String::from_utf8_lossy(node.name.as_bytes()).into_owned()
    }

    /// What a row says under the entry's name: how large a file is and when
    /// it last changed.
    fn details(node: &TreeEntry) -> String {
        let mut parts: Vec<String> = Vec::new();
        if node.kind == FileNodeKind::File {
            parts.push(ui::format::memory(node.size));
        }
        if let Some(at) =
            chrono::DateTime::from_timestamp_millis(i64::try_from(node.mtime).unwrap_or(i64::MAX))
        {
            parts.push(ui::format::timestamp(&at.with_timezone(&chrono::Local)));
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
        let Some(landing) = tilo_content_types::landing(head, &ask.name) else {
            tracing::info!("no app in this page opens {}", ask.name);
            return;
        };
        let app = format!("so.tilo.{}", landing.app);
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
