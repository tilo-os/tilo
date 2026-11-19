//! The workspace's files in a page: its tree, the Files record every member
//! of a workspace holds ([`fs_protocol::tree`]), listed where a person can
//! open them.
//!
//! A computer's Files browses that resource through the folder it mounts. A
//! page has no disk and mounts nothing, so this surface reads the resource
//! itself, over the page's session: the folder the window shows, the folders
//! and files in it with their size, when they changed and who changed them,
//! and the content of a file by the hash its node names, which the page asks
//! the workspace for. Opening a file hands that content to the app the file's
//! bytes say opens it, as a window on a computer does.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use fs_protocol::FileAction;
use fs_protocol::files::{FileDocumentId, FileNameV1, FileNodeKind, TreeChunk};
use fs_protocol::tree::{FILES_APP, ROOT_ID, Tree, TreeChunks, TreeEntry, chunk_document_id};
use ui::carry_out::{ContentLinkAsk, ContentLinkFile, Offers};
use ui::framework::{
    Actions, ChosenFile, Dispatcher, EffectAnswer, EffectCx, Effected, People, ResourceApp,
    ResourceOpener, ViewCx, WindowBar, WorkspaceResource,
};
use ui::i18n::EmbeddedCatalog;
use ui::icons::Icon;
use ui::widget::controls::ActionControls;
use ui::widget::{
    ActionEmitter, ArrivingView, Banner, Button, Column, Decorator, Element, ListRow, Row,
    SkeletonShape, TextInput, Widget as _,
};
use ui::window_toolbar::{LocationSegment, ToolbarEvent, ToolbarLocation};

use crate::i18n::{arg, format_bytes, format_datetime, t, t_args};

pub use file_explorer_model::window::{FilesWindow, FilesWindowEffect};

/// The version of the view a window saves: the node of the folder it shows, as text.
const WINDOW_VIEW_VERSION: u32 = 1;

/// The most a person uploads at once from the page's chooser: the page holds the files whole
/// while it keeps them.
const MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

/// Files in a page: the workspace's tree, over the page's session.
pub struct FilesTreeShell;

impl ResourceApp for FilesTreeShell {
    type Resource = FilesWindow;
    const ID: &'static str = ui::apps::FILE_EXPLORER;
    const TITLE: &'static str = "Files";
    const TITLE_KEY: &'static str = "files-app-title";
    const ICON: Option<&'static str> = Some("file-explorer");
    /// The workspace's tree, the resource every member holds: the window shows it and its
    /// History reads its changes, so it is opened with the window.
    const PERSON_RESOURCES: &'static [ui::framework::PersonKind] =
        &[ui::framework::PersonKind::shared::<
            fs_protocol::tree::FilesTree,
        >()];
    /// The folder this window shows: every file's changes in it, as one list, read from
    /// the tree record its rows are.
    const FOLDER_HISTORY: Option<ui::framework::FolderHistory> =
        Some(ui::framework::FolderHistory {
            title: "Folder",
            beside_resource: false,
        });

    fn view(cx: &mut ViewCx<FilesWindow>) -> Element {
        // New folder, New file and Upload are the window's own effects, so the bar, the
        // palette and an agent ask for them alike; each answers on the window that shows the
        // folder.
        cx.window_host_effects(|browser: &mut TreeBrowser, effect: FilesWindowEffect, cx| {
            browser.answer(effect, cx)
        });
        // The folder the window shows is its own view, as a scroll position is: each window
        // goes into folders on its own and never moves another person's. An agent's `open`
        // names a node, which the window shows as choosing its row would.
        let asks = Asks::default();
        let opened = asks.clone();
        cx.on_open(move |anchor: &str| opened.open(anchor));
        Box::new(TreeBrowser::new(cx, asks))
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

/// What a press in the window's rows asks of the window, taken on its next refresh.
enum Ask {
    /// Let the new folder or file be.
    StopNaming,
    /// Open the node, as choosing its row does.
    Open(String),
}

/// The asks the rows made since the window last took them.
#[derive(Clone, Default)]
struct Asks(Rc<RefCell<Vec<Ask>>>);

impl Asks {
    fn open(&self, node: &str) {
        self.0.borrow_mut().push(Ask::Open(node.to_owned()));
    }

    fn stop_naming(&self) {
        self.0.borrow_mut().push(Ask::StopNaming);
    }

    fn take(&self) -> Vec<Ask> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

/// What the rows were built from, so they are built again only when it moved.
#[derive(PartialEq)]
struct Shown {
    folder: String,
    /// The tree's revision when the rows were built: it moves when a chunk's nodes do.
    revision: u64,
    names: u64,
    /// Whether the tree had arrived.
    arrived: bool,
    /// What kind of node was being named, while one was.
    naming: Option<FileNodeKind>,
    /// What the window last had to say about something it could not do.
    notice: Option<Notice>,
}

/// Something the window could not do: what failed, and why.
#[derive(Clone, PartialEq)]
struct Notice {
    title: String,
    reason: Option<String>,
}

/// The workspace's tree as the page holds it: the resource that names its root and lists its
/// chunks, and each chunk opened as the tree lists it, so a change of one chunk is the only
/// one read again.
struct Held {
    /// The tree's own document (chunk 0): the root's identity and the chunks it lists.
    index: WorkspaceResource<fs_protocol::tree::FilesTree>,
    /// Opens a chunk's resource as the tree lists it.
    chunks_open: ResourceOpener<fs_protocol::tree::FilesTree>,
    /// Each chunk, by number, with the revision its nodes were last read at.
    chunks: BTreeMap<TreeChunk, HeldChunk>,
    /// The whole tree, kept as each chunk's nodes move.
    tree: Tree,
    /// Moves when the nodes the page shows do.
    revision: u64,
    /// The tree's own document and every chunk it lists have been read: what the window
    /// shows is the tree, not part of it on its way.
    arrived: bool,
    /// Nodes this window made that the workspace did not take, as they were taken
    /// (some only once the store was free): what was asked and why it failed.
    failed: Rc<RefCell<Vec<Notice>>>,
}

/// One chunk of the tree, read as its resource changes.
struct HeldChunk {
    resource: WorkspaceResource<fs_protocol::tree::FilesTree>,
    /// The revision the nodes were last read at; `None` before the first read.
    revision: Option<u64>,
    nodes: BTreeMap<String, TreeEntry>,
    /// Who last changed each of its nodes, as its changes name them.
    writers: HashMap<String, (String, ui::tilo_actions::ActionContext)>,
}

impl Held {
    fn new(
        index: WorkspaceResource<fs_protocol::tree::FilesTree>,
        chunks_open: ResourceOpener<fs_protocol::tree::FilesTree>,
    ) -> Self {
        Self {
            index,
            chunks_open,
            chunks: BTreeMap::new(),
            tree: Tree::default(),
            revision: 0,
            arrived: false,
            failed: Rc::default(),
        }
    }

    /// What this window asked of the tree that failed since it last looked.
    fn take_failed(&self) -> Vec<Notice> {
        std::mem::take(&mut self.failed.borrow_mut())
    }

    /// The whole tree, every chunk read so far.
    fn tree(&self) -> &Tree {
        &self.tree
    }

    /// The chunk `chunk`, when the page holds it.
    fn chunk(&self, chunk: TreeChunk) -> Option<&HeldChunk> {
        self.chunks.get(&chunk)
    }

    /// Open the chunk the next node made here goes to, ahead of it: a chunk nobody has written
    /// yet is founded as it opens, which the window can do only outside its frame (as it
    /// answers an effect), never while it builds its rows.
    fn open_next_chunk(&mut self) {
        let next = self.tree.chunk_for_new();
        if self.chunks.contains_key(&next) {
            return;
        }
        if let Some(resource) = self.chunks_open.open(&chunk_document_id(next)) {
            self.chunks.insert(
                next,
                HeldChunk {
                    resource,
                    revision: None,
                    nodes: BTreeMap::new(),
                    writers: HashMap::new(),
                },
            );
        }
    }

    /// Make a node called `name` in the folder `parent` (the first free name beside it when the
    /// folder has one already), as the person at this window: one change of the chunk it goes
    /// to, through the tree's own verbs, then listed in the tree's own document. A file's
    /// `content` is the hash of its manifest and its length. The node it made.
    fn create(
        &mut self,
        parent: &str,
        name: &str,
        kind: FileNodeKind,
        content: Option<(String, u64)>,
        failed_title: String,
    ) -> Result<String, String> {
        let chunk = self.tree.chunk_for_new();
        let resource = match self.chunks.get(&chunk) {
            Some(held) => held.resource.clone(),
            None => self
                .chunks_open
                .open(&chunk_document_id(chunk))
                .ok_or_else(|| t("files-tree-not-here"))?,
        };
        let name = fs_protocol::tree::free_name(&self.tree, parent, name);
        let document_id = FileDocumentId::in_chunk(chunk);
        let node = fs_protocol::tree::document_node_id(document_id);
        let (hash, size) = content.map_or((None, 0), |(hash, size)| (Some(hash), size));
        let mtime = ui::time::unix_now_ms();
        let action = FileAction::Create {
            node: node.clone(),
            document_id,
            parent: parent.to_owned(),
            name: FileNameV1::from(name.as_str()),
            kind,
            hash: hash.clone(),
            size,
            mtime,
            author: resource.me().id().to_owned(),
        };
        let invoke = ui::tilo_resource::Invoke::<fs_protocol::tree::FilesTree>::apply(&action)
            .map_err(|error| error.to_string())?;
        // The tree's own document lists every chunk a node is in, so every member opens it: a
        // chunk nobody listed yet is listed first, with the tree's own action. Both wait for the
        // store when the window's own effect holds it (a chooser that answers), and
        // what fails says so on the window's next refresh.
        let unlisted = !self
            .index
            .read(fs_protocol::tree::listed_chunks)
            .unwrap_or_default()
            .contains(&chunk);
        if unlisted {
            let list = ui::tilo_resource::Invoke::<fs_protocol::tree::FilesTree>::apply(
                &FileAction::ListChunk { chunk },
            )
            .map_err(|error| error.to_string())?;
            let failed = Rc::clone(&self.failed);
            let failed_title = failed_title.clone();
            self.index.invoke_then(&list, move |listed| {
                if let Err(why) = listed {
                    tracing::warn!("listing part {chunk} of the workspace's tree: {why}");
                    failed.borrow_mut().push(Notice {
                        title: failed_title,
                        reason: Some(why.to_string()),
                    });
                }
            });
        }
        let failed = Rc::clone(&self.failed);
        let made_name = name.clone();
        resource.invoke_then(&invoke, move |made| {
            if let Err(why) = made {
                tracing::warn!("making {made_name} in the workspace's tree: {why}");
                failed.borrow_mut().push(Notice {
                    title: failed_title,
                    reason: Some(why.to_string()),
                });
            }
        });
        self.chunks.entry(chunk).or_insert_with(|| HeldChunk {
            resource,
            revision: None,
            nodes: BTreeMap::new(),
            writers: HashMap::new(),
        });
        // The next node made before the chunk is read again sees this one's name.
        self.tree.nodes.insert(
            node.clone(),
            TreeEntry {
                parent: Some(parent.to_owned()),
                name: FileNameV1::from(name.as_str()),
                kind,
                hash,
                size,
                mtime,
                author: None,
                document_id: Some(document_id),
            },
        );
        Ok(node)
    }

    /// Read the chunks the tree lists that are not read yet, and the nodes of each one that
    /// moved, in place of what it held before. Whether the page's nodes moved.
    fn follow(&mut self) -> bool {
        let mut moved = false;
        // A chunk is opened as the tree's own document lists it; one still on its way here is
        // tried again on the next wake.
        let mut index = TreeChunks::default();
        let index_read = self.index.read(|doc| index.read_index(doc)).is_some();
        if index_read {
            let root = index.tree().root_document_id;
            if self.tree.root_document_id != root {
                self.tree.root_document_id = root;
                moved = true;
            }
            for chunk in index.listed() {
                if self.chunks.contains_key(&chunk) {
                    continue;
                }
                if let Some(resource) = self.chunks_open.open(&chunk_document_id(chunk)) {
                    self.chunks.insert(
                        chunk,
                        HeldChunk {
                            resource,
                            revision: None,
                            nodes: BTreeMap::new(),
                            writers: HashMap::new(),
                        },
                    );
                }
            }
        }
        // Each chunk's nodes are read only when its resource moved: the rest of the tree is
        // left as it was, so a change costs its chunk.
        let mut tree = std::mem::take(&mut self.tree);
        for held in self.chunks.values_mut() {
            let state = held.resource.get();
            let revision = held.resource.revision();
            if held.revision == Some(revision) {
                continue;
            }
            held.revision = Some(revision);
            for id in held.nodes.keys() {
                tree.nodes.remove(id);
            }
            held.nodes = state.nodes.clone();
            held.writers = held
                .resource
                .read(ui::history_source::folder::last_change_by_node)
                .unwrap_or_default();
            tree.nodes.extend(
                held.nodes
                    .iter()
                    .map(|(id, node)| (id.clone(), node.clone())),
            );
            moved = true;
        }
        self.tree = tree;
        let arrived = index_read
            && index.listed().iter().all(|chunk| {
                self.chunks
                    .get(chunk)
                    .is_some_and(|held| held.revision.is_some())
            });
        if arrived != self.arrived {
            self.arrived = arrived;
            moved = true;
        }
        if moved {
            self.revision = self.revision.wrapping_add(1);
        }
        moved
    }
}

/// What the list of a folder shows.
enum Listing<'a> {
    /// The workspace's tree is on its way: the list's place, held for it.
    Arriving,
    /// The folder holds nothing yet.
    Empty,
    /// The folder's entries, out of the trash.
    Entries(&'a Tree, Vec<(&'a str, &'a TreeEntry)>),
}

/// What the list of `folder` shows, `tree` being the workspace's tree once it is here whole.
fn listing<'a>(tree: Option<&'a Tree>, folder: &str) -> Listing<'a> {
    let Some(tree) = tree else {
        return Listing::Arriving;
    };
    let entries: Vec<(&str, &TreeEntry)> = tree
        .children(folder)
        .into_iter()
        .filter(|(id, _)| !tree.trashed(id))
        .collect();
    if entries.is_empty() {
        Listing::Empty
    } else {
        Listing::Entries(tree, entries)
    }
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
    /// The folder this window shows, by its node; empty for the workspace's root. The
    /// window's own view, never written to the record every window opens.
    folder: ui::LocalState<String>,
    /// The workspace's tree, as the page holds it: its own document and the chunks it lists.
    /// `None` while the workspace has not delivered it.
    tree: Option<Held>,
    /// Opens the tree's resources as the workspace delivers them.
    opener: ResourceOpener<fs_protocol::tree::FilesTree>,
    /// When the window started waiting for the tree: its placeholder keeps its timing however
    /// often the rows are built again.
    since: ui::time::Instant,
    /// The kind and the name being typed for a new folder or file, while one is: `None`
    /// otherwise.
    naming: Option<(FileNodeKind, ui::LocalState<String>)>,
    /// What the window could not do, said above its rows until the next thing it does.
    notice: Option<Notice>,
    /// The window's bar: where it is, and New folder, New file and Upload.
    bar: WindowBar<FilesWindow>,
    /// What the rows asked of the window.
    asks: Asks,
    /// What the workspace calls the people its nodes name.
    people: People,
    /// The window's own document, whose effects the bar offers.
    dispatch: Dispatcher<FilesWindow>,
    /// The window's published effects, which the row a new folder or file is named in
    /// invokes as the bar does.
    actions: Actions<FilesWindow>,
    read: Option<Reads>,
    /// Where the window keeps the content of files put into the workspace from here.
    content: Option<ui::content::ContentStore>,
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
    fn new(cx: &ViewCx<FilesWindow>, asks: Asks) -> Self {
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
        let people = cx.people();
        // The workspace's tree is the resource every member holds, under the
        // Files app's own name for it — the one the page's search reads
        // when it finds a file by what it says. Its chunks are opened as
        // the resource lists them: one resource each, read as each one changes.
        let opener = cx.resource_opener(FILES_APP);
        let tree = opener
            .open(&fs_protocol::tree::document_id())
            .map(|index| Held::new(index, opener.clone()));
        let mut browser = Self {
            folder: cx.local(String::new()),
            tree,
            opener,
            since: ui::time::now(),
            naming: None,
            notice: None,
            bar: cx.toolbar(),
            asks,
            people,
            dispatch: cx.dispatcher(),
            actions: cx.actions(),
            read,
            content: cx.content(),
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

    /// The folder the window shows: the one it went into, or the root when that is not in
    /// the tree (any more).
    fn folder(&self, tree: Option<&Tree>) -> String {
        let named = self.folder.get();
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
        for ask in self.asks.take() {
            match ask {
                Ask::StopNaming => self.naming = None,
                Ask::Open(node) => {
                    if node.is_empty() || node == ROOT_ID {
                        self.show_folder(ROOT_ID);
                    } else {
                        self.open_node(&node);
                    }
                }
            }
        }
        // The tree is opened once the workspace delivered it: a window opened before that
        // waits for it rather than showing an empty folder.
        if self.tree.is_none()
            && let Some(index) = self.opener.open(&fs_protocol::tree::document_id())
        {
            self.tree = Some(Held::new(index, self.opener.clone()));
        }
        // A chunk of the tree that moved is the only one read again.
        if let Some(tree) = self.tree.as_mut() {
            tree.follow();
            if let Some(failed) = tree.take_failed().pop() {
                self.notice = Some(failed);
            }
        }
        let folder = self.folder(self.tree.as_ref().map(Held::tree));
        self.declare_bar(&folder);
        let shown = Shown {
            names: self.people.revision().get(),
            revision: self.tree.as_ref().map_or(0, |tree| tree.revision),
            arrived: self.arrived(),
            naming: self.naming.as_ref().map(|(kind, _)| *kind),
            notice: self.notice.clone(),
            folder,
        };
        if self.shown.as_ref() == Some(&shown) {
            return false;
        }
        let tree = self
            .tree
            .as_ref()
            .filter(|held| held.arrived)
            .map(Held::tree);
        self.body = self.rows(tree, &shown.folder);
        self.shown = Some(shown);
        self.structure_dirty = true;
        true
    }

    /// Whether the workspace's tree is here whole: until it is, the window says it is on its
    /// way rather than showing a folder that only looks empty.
    fn arrived(&self) -> bool {
        self.tree.as_ref().is_some_and(|held| held.arrived)
    }

    /// The window's bar: the open folder and the folders above it, each a step back up, and
    /// the ways to put something into it.
    fn declare_bar(&mut self, folder: &str) {
        let tree = self.tree.as_ref().map(Held::tree);
        let root = t("files-tree-root");
        let mut segments = vec![LocationSegment {
            label: root.clone(),
            value: ROOT_ID.to_owned(),
        }];
        if let Some(tree) = tree {
            segments.extend(Self::trail(tree, folder).into_iter().map(|(id, name)| {
                LocationSegment {
                    label: name,
                    value: id,
                }
            }));
        }
        let label = segments
            .last()
            .map_or_else(|| root.clone(), |segment| segment.label.clone());
        let location = ToolbarLocation {
            label,
            name: t("files-location"),
            segments,
            // The workspace's own folder names a part of Files, not something the person
            // opened: the window keeps the app's name.
            section: folder == ROOT_ID,
            ..ToolbarLocation::default()
        };
        let mut bar = WindowBar::new(self.dispatch.clone())
            .place(Some(location))
            .effect(&FilesWindowEffect::NewFolder { name: None })
            .enabled(self.arrived())
            .effect(&FilesWindowEffect::NewFile { name: None })
            .enabled(self.arrived())
            .effect(&FilesWindowEffect::Upload)
            .enabled(self.arrived());
        if folder != ROOT_ID {
            bar = bar.effect(&FilesWindowEffect::Up).in_menu();
        }
        self.bar = bar;
        self.bar.declare();
    }

    /// Take what the person pressed in the window's bar: a level of the location opens that
    /// folder; the rest is the bar's own.
    fn take_bar_events(&self) -> bool {
        let mut asked = false;
        for event in ui::window_toolbar::take_events() {
            asked = true;
            match &event {
                ToolbarEvent::Commit { text } => self.show_folder(text),
                _ => {
                    self.bar.press(&event);
                }
            }
        }
        asked
    }

    /// Show the folder `id` in this window alone: the workspace's own for [`ROOT_ID`].
    fn show_folder(&self, id: &str) {
        let id = if id == ROOT_ID {
            String::new()
        } else {
            id.to_owned()
        };
        self.folder.set_if_changed(id);
    }

    /// The folder that holds the one the window shows: the root holds itself.
    fn enclosing(&self) -> String {
        let tree = self.tree.as_ref().map(Held::tree);
        let folder = self.folder(tree);
        tree.and_then(|tree| tree.node(&folder))
            .and_then(|node| node.parent.clone())
            .unwrap_or_else(|| ROOT_ID.to_owned())
    }

    /// Answer the window's effect: the row a new folder or file is named in, the folder
    /// above, or the chooser whose files go into the open folder.
    fn answer(
        &mut self,
        effect: FilesWindowEffect,
        cx: &mut EffectCx<FilesWindow, Self>,
    ) -> EffectAnswer {
        match effect {
            FilesWindowEffect::NewFolder { name } => self.new_node(FileNodeKind::Directory, name),
            FilesWindowEffect::NewFile { name } => self.new_node(FileNodeKind::File, name),
            FilesWindowEffect::Download { node } => {
                let tree = self
                    .tree
                    .as_ref()
                    .filter(|held| held.arrived)
                    .map(Held::tree)
                    .ok_or_else(|| t("files-tree-not-here"))?;
                let entry = tree
                    .node(&node)
                    .filter(|_| !tree.trashed(&node))
                    .ok_or_else(|| t("files-selection-unavailable"))?;
                if !ui::carry_out::available() {
                    return Err(t("files-tree-download-unavailable"));
                }
                ui::carry_out::save(TreeBrowser::carried(tree, &node, entry));
                Ok(Effected::done())
            }
            FilesWindowEffect::Up => {
                let enclosing = self.enclosing();
                self.show_folder(&enclosing);
                Ok(Effected::done())
            }
            FilesWindowEffect::Upload => {
                let request = ui::portal::PickRequest::open_multiple(
                    &t("files-tree-upload-title"),
                    &t("files-tree-upload-accept"),
                )
                .max_contents_bytes(MAX_UPLOAD_BYTES);
                // The files go in as the chooser answers, outside the window's frame, as a new
                // folder does: the action that adds each needs the tree's store to itself.
                Ok(cx.choose_files(request, |browser: &mut Self, files, _| {
                    if let Some(held) = browser.tree.as_mut() {
                        held.open_next_chunk();
                    }
                    browser.upload(files);
                    browser.refresh();
                    match &browser.notice {
                        Some(notice) => Err(notice.title.clone()),
                        None => Ok(Effected::done()),
                    }
                }))
            }
        }
    }

    /// Make a folder or an empty text file of `kind` called `name` in the open folder; with
    /// no name, show the row it is named in.
    fn new_node(&mut self, kind: FileNodeKind, name: Option<String>) -> EffectAnswer {
        // The chunk the node goes to is founded here, outside the window's frame.
        if let Some(held) = self.tree.as_mut() {
            held.open_next_chunk();
        }
        let is_folder = kind == FileNodeKind::Directory;
        match name {
            Some(name) if name.trim().is_empty() => Err(t(if is_folder {
                "files-folder-name-empty"
            } else {
                "files-name-empty"
            })),
            Some(name) => {
                self.make(kind, &name);
                match &self.notice {
                    Some(notice) => Err(notice
                        .reason
                        .clone()
                        .unwrap_or_else(|| notice.title.clone())),
                    None => Ok(Effected::done()),
                }
            }
            None => {
                let name = t(if is_folder {
                    "files-new-folder-default-name"
                } else {
                    "files-tree-new-file-default-name"
                });
                self.naming = Some((kind, ui::LocalState::new(name)));
                self.notice = None;
                Ok(Effected::done())
            }
        }
    }

    /// Put the files `chosen` into the open folder: each kept in the workspace's content, then
    /// a file of the tree that names it.
    fn upload(&mut self, chosen: Vec<ChosenFile>) {
        let folder = self.folder(self.tree.as_ref().map(Held::tree));
        let Some(content) = self.content.clone() else {
            self.notice = Some(Notice {
                title: t("files-tree-not-here"),
                reason: None,
            });
            return;
        };
        let Some(held) = self.tree.as_mut() else {
            self.notice = Some(Notice {
                title: t("files-tree-not-here"),
                reason: None,
            });
            return;
        };
        for file in chosen {
            let made = ui::content::manifest::keep(&content, &file.bytes).and_then(|hash| {
                let size = u64::try_from(file.bytes.len()).unwrap_or(u64::MAX);
                held.create(
                    &folder,
                    &file.name,
                    FileNodeKind::File,
                    Some((hash, size)),
                    t_args(
                        "files-tree-upload-failed",
                        &[arg("name", file.name.clone())],
                    ),
                )
            });
            if let Err(why) = made {
                tracing::warn!("uploading {}: {why}", file.name);
                self.notice = Some(Notice {
                    title: t_args(
                        "files-tree-upload-failed",
                        &[arg("name", file.name.clone())],
                    ),
                    reason: Some(why),
                });
            }
        }
    }

    /// Make the folder or the empty file of `kind` named `name` in the open folder, and stop
    /// naming one. A file is opened once it is made, in the app that reads text.
    fn make(&mut self, kind: FileNodeKind, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let title = t(if kind == FileNodeKind::Directory {
            "files-new-folder-failed-title"
        } else {
            "files-tree-new-file-failed-title"
        });
        let folder = self.folder(self.tree.as_ref().map(Held::tree));
        // A file's content is kept in the workspace's content like an upload's, empty.
        let content = match kind {
            FileNodeKind::Directory => Ok(None),
            FileNodeKind::File => match self.content.as_ref() {
                Some(content) => {
                    ui::content::manifest::keep(content, &[]).map(|hash| Some((hash, 0)))
                }
                None => Err(t("files-tree-not-here")),
            },
        };
        let Some(held) = self.tree.as_mut() else {
            return;
        };
        match content.and_then(|content| held.create(&folder, name, kind, content, title.clone())) {
            Ok(node) => {
                self.naming = None;
                self.notice = None;
                if kind != FileNodeKind::Directory {
                    self.open_node(&node);
                }
            }
            Err(why) => {
                self.notice = Some(Notice {
                    title,
                    reason: Some(why),
                });
            }
        }
    }

    /// The window's content: the open folder's entries, under the bar that says where it is.
    /// `tree` is `None` while the workspace's tree is on its way: the list's place is held
    /// for it then, never shown as an empty folder.
    fn rows(&self, tree: Option<&Tree>, folder: &str) -> Column {
        let mut body = Column::new().with_id("files-tree");
        self.offers.clear();
        if let Some(notice) = &self.notice {
            let mut banner = Banner::warning(notice.title.clone());
            if let Some(reason) = &notice.reason {
                banner = banner.with_message(reason.clone());
            }
            body = body.push(Box::new(banner));
        }
        if let Some((kind, name)) = self.naming.as_ref().filter(|_| tree.is_some()) {
            body = body.push(self.naming_row(*kind, name));
        }
        match listing(tree, folder) {
            Listing::Arriving => body.push(ArrivingView::placeholder_since(
                ui::arriving::Source::Local,
                self.since,
                SkeletonShape::Page,
            )),
            // An empty folder is a list with nothing in it yet: the bar above still offers
            // New folder, New file and Upload, and files dropped on the window land here.
            Listing::Empty if self.naming.is_none() => body.push(Box::new(
                ListRow::new("files-tree-empty".to_owned(), t("files-empty-folder")).quiet(),
            )),
            Listing::Empty => body,
            Listing::Entries(tree, entries) => {
                for (id, node) in entries {
                    body =
                        body.push(self.row(id, node, Some(TreeBrowser::carried(tree, id, node))));
                }
                body
            }
        }
    }

    /// The row a new folder or file is named in: its name, chosen and ready to type over (a
    /// file's name up to its extension), and the buttons that make it or let it be.
    fn naming_row(
        &self,
        kind: FileNodeKind,
        name: &ui::LocalState<String>,
    ) -> Box<dyn ui::widget::Widget> {
        let (part, icon) = if kind == FileNodeKind::Directory {
            ("folder", Icon::Folder)
        } else {
            ("file", Icon::FileText)
        };
        // Enter in the name and Create invoke the window's New folder or New file with the
        // typed name, as the bar, the palette and an agent do.
        let effect = if kind == FileNodeKind::Directory {
            FilesWindowEffect::NEW_FOLDER
        } else {
            FilesWindowEffect::NEW_FILE
        };
        let mut field = TextInput::from_state(name).with_id(&format!("files-tree-new-{part}-name"));
        if let Some(binding) = self
            .actions
            .effect_binding(effect.map(|typed: String| Some(typed)))
        {
            field = field.on_submit_action(binding);
        }
        field.request_focus();
        let typed = name.get();
        match std::path::Path::new(&typed)
            .file_stem()
            .filter(|_| kind != FileNodeKind::Directory)
        {
            Some(stem) => field.set_selection(0, stem.len()),
            None => field.select_all(),
        }
        let typed = name.clone();
        let create = self.actions.bind_effect(
            Button::new(
                &format!("files-tree-new-{part}-create"),
                &t("files-tree-create"),
            ),
            effect.map(move |()| Some(typed.get())),
        );
        let window = self.asks.clone();
        let cancel = Button::new(
            &format!("files-tree-new-{part}-cancel"),
            &t("files-tree-cancel"),
        )
        .on_view(move || window.stop_naming());
        Box::new(
            Row::new()
                .with_id(format!("files-tree-new-{part}"))
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(Box::new(ui::widget::IconWidget::new(icon)))
                .push(Box::new(field))
                .push(Box::new(create))
                .push(Box::new(cancel)),
        )
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
        let mut row = ListRow::new(id.to_owned(), name.clone())
            // What it is leads the row: a folder, or the kind of file its name
            // says, so a list reads at a glance without opening anything.
            .icon(ui::icons::icon_for_path(
                std::path::Path::new(&name),
                is_folder,
            ));
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
            row = row.carries_out(self.offers.offer(carried));
            // The window's Download, as an agent asks for it too.
            if let Some(binding) = self.actions.effect(&FilesWindowEffect::Download {
                node: id.to_owned(),
            }) {
                row = row.action_binding(Icon::Download, label, binding);
            }
        }
        if let Some(author) = self.author_of(id, node) {
            // Who made it as its own cell: the person's face and name, and what wrote
            // for them as a chip wearing the agent mark and its own colour. The hover
            // names the agent, its session, the computer and the folder. Never one line
            // joined from names.
            let parts = ui::author_parts(&author);
            let identity = parts
                .person_identity
                .clone()
                .unwrap_or_else(|| parts.person.clone());
            row = row.author_cell(parts.person.clone(), identity);
            if let Some(writer) = &parts.writer {
                row = row.author_writer(writer.clone(), ui::tokens::PresenceHue::of_agent(writer));
            }
            // For a reader who cannot see the mark: the person, the writer and the
            // computer, as one description built from the fields.
            row = row.description(ui::author_spoken(&parts));
            if let Some(hover) = ui::attribution_hover(&author) {
                row = row.tooltip(hover);
            }
        } else if let Some(author) = node.author.as_deref() {
            // A tree that resources no version here yet: the person or device
            // the workspace names, as a name alone. One it does not name (the
            // workspace's host, which writes the files of notes) is left
            // unnamed rather than shown as its key.
            let name = ui::tilo_resource::MemberId::parsed_person(author)
                .map_or_else(|| author.to_owned(), |member| self.people.name(&member));
            if name != author {
                row = row.meta(name);
            }
        }
        if is_folder {
            let folder = self.folder.clone();
            let into = id.to_owned();
            return Box::new(row.on_view(move || {
                folder.set_if_changed(into.clone());
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

    /// Who last changed the node `id`, as the change names them: the person, or the agent
    /// that wrote for them. `None` for a node no named change of its chunk acted on.
    fn author_of(&self, id: &str, node: &TreeEntry) -> Option<ui::HistoryAuthor> {
        let document_id = node.document_id?;
        // A node's changes are in the document of the chunk its document id names.
        let chunk = self.tree.as_ref()?.chunk(document_id.chunk())?;
        let (actor, context) = chunk.writers.get(id)?;
        Some(self.people.registry().resolve_entry(actor, Some(context)))
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
        jiff::Timestamp::from_millisecond(i64::try_from(node.mtime).unwrap_or(i64::MAX))
            .ok()
            .map(format_datetime)
    }

    /// The folders from the workspace's root down to `folder`, each by its node and name; the
    /// root itself is not one of them.
    fn trail(tree: &Tree, folder: &str) -> Vec<(String, String)> {
        let mut trail: Vec<(String, String)> = Vec::new();
        let mut at = folder.to_owned();
        // A node names its parent, so the trail is read up from the folder and turned round;
        // a tree that points back on itself ends the walk at the repeat rather than looping.
        while at != ROOT_ID && !trail.iter().any(|(id, _)| *id == at) {
            let Some(node) = tree.node(&at) else {
                break;
            };
            trail.push((at.clone(), Self::name_of(node)));
            match node.parent.clone() {
                Some(parent) => at = parent,
                None => break,
            }
        }
        trail.reverse();
        trail
    }

    /// The path of `folder` from the workspace's root, as `a / b`.
    #[cfg(test)]
    fn path_of(tree: &Tree, folder: &str) -> String {
        Self::trail(tree, folder)
            .into_iter()
            .map(|(_, name)| name)
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// Open the file at node `id`, as choosing its row does: a folder is shown, a file is
    /// opened in the app its bytes say opens it once they are here. Whether there is
    /// such a node to open.
    fn open_node(&self, id: &str) -> bool {
        let Some(held) = self.tree.as_ref() else {
            return false;
        };
        let tree = held.tree();
        let Some(node) = tree.node(id).filter(|_| !tree.trashed(id)).cloned() else {
            return false;
        };
        let name = Self::name_of(&node);
        if node.kind == FileNodeKind::Directory {
            self.show_folder(id);
            return true;
        }
        let Some(hash) = node.hash else {
            return false;
        };
        if let Some(read) = &self.read {
            // Reading content the page lacks asks the workspace for it.
            let _ = read(&hash);
        }
        self.asked.borrow_mut().push(Asked { hash, name });
        self.open_what_arrived();
        true
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
        ui::framework::ContentHandoff::new(app, label, t("files-tree-open-detail")).emitter("open")
    }
}

/// A folder's History names each change's file as a link: choosing it opens the file here,
/// as choosing its row does.
impl ui::history_mode::capability::PresentsHistoryEntries for TreeBrowser {
    fn history_entry_presenter(&self) -> Option<Rc<dyn ui::widget::HistoryEntryPresenter>> {
        None
    }

    fn open_history_entry(&mut self, entry: &ui::widget::history::HistoryEntry) -> bool {
        let Some(target) = entry.target.as_ref() else {
            return false;
        };
        self.open_node(&target.id)
    }
}

/// The folder the window shows is kept with the window, as its scroll position is: a
/// window brought back opens on the folder it was left in, and no other window moves.
impl ui::application_view::ApplicationViewState for TreeBrowser {
    fn capture_application_view(
        &self,
    ) -> Result<Option<ui::application_view::ApplicationLocalView>, ui::view::ThawError> {
        Ok(Some(ui::application_view::ApplicationLocalView {
            app_id: <FilesTreeShell as ResourceApp>::ID.to_owned(),
            version: WINDOW_VIEW_VERSION,
            payload: self.folder.get().into_bytes(),
        }))
    }

    fn restore_application_view(
        &mut self,
        local: &ui::application_view::ApplicationLocalView,
        _: &ui::automerge::AutoCommit,
    ) -> Result<ui::application_view::ApplicationFocusRestore, ui::view::ThawError> {
        if local.version != WINDOW_VIEW_VERSION {
            return Err(ui::view::ThawError::UnsupportedVersion {
                found: local.version,
                supported: WINDOW_VIEW_VERSION,
            });
        }
        let folder = String::from_utf8(local.payload.clone())
            .map_err(|_| ui::view::ThawError::InvalidView("a folder that is not text".into()))?;
        self.folder.set_if_changed(folder);
        Ok(ui::application_view::ApplicationFocusRestore::Generic)
    }
}

impl Decorator for TreeBrowser {
    ui::provides!(
        dyn ui::history_mode::capability::PresentsHistoryEntries,
        dyn ui::application_view::ApplicationViewState
    );

    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.body
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.body
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.take_bar_events();
        self.refresh();
        self.body.sync(state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let asked = self.take_bar_events();
        let rebuilt = self.refresh();
        let updated = self.body.update(state, path);
        asked || rebuilt || updated || std::mem::take(&mut self.structure_dirty)
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
            document_id: None,
        };
        (id.to_owned(), entry)
    }

    /// A workspace's tree: `docs/` holding `plan.md` and `scan.pdf`, and
    /// `notes.txt` beside it, written by Ada.
    fn workspace() -> Tree {
        Tree {
            root_document_id: None,
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
    fn a_tree_on_its_way_is_not_an_empty_folder() {
        assert!(
            matches!(listing(None, ROOT_ID), Listing::Arriving),
            "before the tree is here the list waits for it"
        );
        let empty = Tree::default();
        assert!(
            matches!(listing(Some(&empty), ROOT_ID), Listing::Empty),
            "a tree with nothing in it is an empty folder"
        );
        let tree = workspace();
        assert!(
            matches!(listing(Some(&tree), ROOT_ID), Listing::Entries(_, entries) if entries.len() == 2)
        );
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

#[cfg(test)]
mod window_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

    use super::*;
    use fs_protocol::FileAction;
    use fs_protocol::tree::Placing;
    use ui::automerge::AutoCommit;

    const WORKSPACE: &str = "00000000-0000-4000-8000-0000000000f1";

    /// An agent session working for the person at `[7; 32]`.
    fn agent() -> ui::tilo_actions::ActionInvoker {
        ui::tilo_actions::ActionInvoker::admitted(
            &ui::tilo_actions::Acting::agent("claude").in_session("abc"),
        )
        .named(Some("Claude Code".into()))
        .acting_for(hex::encode([7_u8; 32]))
    }

    /// Apply `action` to `doc` as the executor does: one change, naming `invoker`, else the
    /// person at the device.
    fn apply(
        doc: &mut AutoCommit,
        action: &FileAction,
        invoker: Option<ui::tilo_actions::ActionInvoker>,
    ) {
        let context = fs_protocol::file_action::change_context("tree", action.described(), invoker);
        let encoded = ui::tilo_actions::EncodedAction {
            type_id: fs_protocol::file_action::FILE_ACTION_TYPE.to_owned(),
            payload: serde_json::to_vec(action).unwrap(),
        };
        ui::tilo_actions::apply_local_with_context(doc, action, encoded, None, context).unwrap();
    }

    /// A device's writes into chunk 1's document `doc`: `terms.txt` at the root, written by an
    /// agent, and `legal/nda.txt`, whose folder the person renamed.
    fn write_terms_and_legal(doc: &mut AutoCommit) {
        let mut held = Tree::default();
        let placed = fs_protocol::tree::place(
            (&mut *doc, 1),
            &mut held,
            ROOT_ID,
            "terms.txt",
            Placing::File {
                hash: &"a".repeat(64),
                size: 12,
            },
            (1_000, &hex::encode([9_u8; 32])),
        )
        .unwrap();
        doc.commit();
        apply(
            doc,
            &FileAction::Write {
                node: placed.id,
                hash: Some("c".repeat(64)),
                size: 14,
                mtime: 1_500,
                author: hex::encode([9_u8; 32]),
            },
            Some(agent()),
        );
        let folder = fs_protocol::tree::place(
            (&mut *doc, 1),
            &mut held,
            ROOT_ID,
            "drafts",
            Placing::Folder,
            (2_000, &hex::encode([9_u8; 32])),
        )
        .unwrap();
        fs_protocol::tree::place(
            (&mut *doc, 1),
            &mut held,
            &folder.id,
            "nda.txt",
            Placing::File {
                hash: &"b".repeat(64),
                size: 3,
            },
            (2_500, &hex::encode([9_u8; 32])),
        )
        .unwrap();
        doc.commit();
        apply(
            doc,
            &FileAction::Move {
                node: folder.id.clone(),
                parent: ROOT_ID.to_owned(),
                name: "legal".into(),
                mtime: 3_000,
            },
            None,
        );
    }

    #[test]
    fn the_page_window_lists_the_trees_files_and_history_their_changes() {
        let mut app = ui::prelude::TestApp::<FilesTreeShell>::in_workspace(WORKSPACE)
            .expect("the window opens");
        let index = app
            .shared_resource::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &fs_protocol::tree::document_id(),
            )
            .expect("the window holds the workspace's tree");
        let chunk = app
            .shared_resource::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(1),
            )
            .expect("the window opens the tree's chunk");
        // The tree as a device that mounts the folder writes it: a node in the chunk that
        // holds it, and its versions in that chunk's document, changed elsewhere and arriving
        // here.
        let mut doc: AutoCommit = chunk.read(Clone::clone).expect("the chunk reads");
        write_terms_and_legal(&mut doc);
        chunk
            .record()
            .merge_peer_document(doc.save())
            .expect("the chunk arrives");
        // The tree's own document lists the chunk it wrote the file into.
        let mut index_doc: AutoCommit = index.read(Clone::clone).expect("the resource reads");
        fs_protocol::tree::list_chunk(&mut index_doc, 1).unwrap();
        index
            .record()
            .merge_peer_document(index_doc.save())
            .expect("the tree arrives");
        app.flush().unwrap();
        let texts = app.texts();
        assert!(
            texts.iter().any(|text| text == "terms.txt"),
            "the window draws the tree's file: {texts:?}"
        );
        // History, opened from the window, lists the folder's change: the file, by its
        // path, written by the agent.
        app.harness()
            .open_history()
            .expect("the window offers History");
        app.flush().unwrap();
        app.harness().relayout();
        let labels: Vec<String> = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect();
        assert!(
            labels
                .iter()
                .any(|label| label.ends_with("Edited terms.txt")),
            "History lists the file's change by its path: {labels:?}"
        );
        // Choosing the folder's change opens the folder in the window.
        let row = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.label()
                    .is_some_and(|label| label.contains("legal") && !label.contains("nda"))
            })
            .map_or_else(
                || panic!("History lists the folder's change: {labels:?}"),
                |(id, _)| id,
            );
        app.harness()
            .perform_a11y_action(row, &ui::a11y::Action::Activate)
            .expect("the row opens");
        app.flush().unwrap();
        app.harness().relayout();
        let texts = app.texts();
        assert!(
            texts.iter().any(|text| text == "nda.txt"),
            "the window shows the folder's files: {texts:?}"
        );
    }

    /// The workspace's tree as `write_terms_and_legal` writes it, arrived at `app`'s window.
    fn deliver_terms_and_legal(app: &mut ui::prelude::TestApp<FilesTreeShell>) {
        let index = app
            .shared_resource::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &fs_protocol::tree::document_id(),
            )
            .expect("the window holds the workspace's tree");
        let chunk = app
            .shared_resource::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(1),
            )
            .expect("the window opens the tree's chunk");
        let mut doc: AutoCommit = chunk.read(Clone::clone).expect("the chunk reads");
        write_terms_and_legal(&mut doc);
        chunk
            .record()
            .merge_peer_document(doc.save())
            .expect("the chunk arrives");
        let mut index_doc: AutoCommit = index.read(Clone::clone).expect("the resource reads");
        fs_protocol::tree::list_chunk(&mut index_doc, 1).unwrap();
        index
            .record()
            .merge_peer_document(index_doc.save())
            .expect("the tree arrives");
        app.flush().unwrap();
    }

    /// Choose the row of the open folder whose label starts with `name`, as a click does.
    fn choose_row(app: &mut ui::prelude::TestApp<FilesTreeShell>, name: &str) {
        app.harness().relayout();
        let row = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label().is_some_and(|label| label.starts_with(name)))
            .map_or_else(|| panic!("the folder lists {name}"), |(id, _)| id);
        app.harness()
            .perform_a11y_action(row, &ui::a11y::Action::Activate)
            .expect("the row opens");
        app.flush().unwrap();
        app.harness().relayout();
    }

    #[test]
    fn going_into_a_folder_moves_only_the_window_that_went() {
        let mut first = ui::prelude::TestApp::<FilesTreeShell>::in_workspace(WORKSPACE)
            .expect("the window opens");
        let mut second = first.open_window().expect("a second window opens");
        deliver_terms_and_legal(&mut first);
        deliver_terms_and_legal(&mut second);
        assert!(
            second.texts().iter().any(|text| text == "terms.txt"),
            "the second window shows the workspace's top: {:?}",
            second.texts()
        );
        // The first window goes into `legal`.
        choose_row(&mut first, "legal");
        assert!(
            first.texts().iter().any(|text| text == "nda.txt"),
            "the window that went shows the folder: {:?}",
            first.texts()
        );
        // Going into a folder writes nothing the workspace hands the other windows of its
        // record, and the second window stays where it was.
        let took_in = first
            .deliver_changes_to(&mut second)
            .expect("both windows show the workspace's record");
        assert!(!took_in, "going into a folder changed the shared record");
        second.flush().unwrap();
        second.harness().relayout();
        let texts = second.texts();
        assert!(
            texts.iter().any(|text| text == "terms.txt")
                && !texts.iter().any(|text| text == "nda.txt"),
            "the other window stays at the top: {texts:?}"
        );
        // Enclosing folder brings the first window back up, again on its own.
        first.effect(&FilesWindowEffect::Up).expect("Up answers");
        first.flush().unwrap();
        first.harness().relayout();
        assert!(
            first.texts().iter().any(|text| text == "terms.txt"),
            "the window is back at the top: {:?}",
            first.texts()
        );
    }

    #[test]
    fn a_window_on_a_tree_in_several_documents_keeps_its_actions() {
        let mut app = ui::prelude::TestApp::<FilesTreeShell>::in_workspace(WORKSPACE)
            .expect("the window opens");
        deliver_terms_and_legal(&mut app);
        let violations: Vec<String> = app
            .harness()
            .component_contract_violations()
            .iter()
            .map(|violation| format!("{}: {}", violation.address, violation.reason))
            .collect();
        assert!(
            app.effect(&FilesWindowEffect::Download {
                node: "gone".to_owned()
            })
            .is_err(),
            "Download refuses a node the tree does not hold"
        );
        assert!(
            violations.is_empty(),
            "the window's actions would be withdrawn: {violations:?}"
        );
    }

    #[test]
    fn a_window_reopens_on_the_folder_it_was_left_in() {
        let mut app = ui::prelude::TestApp::<FilesTreeShell>::in_workspace(WORKSPACE)
            .expect("the window opens");
        deliver_terms_and_legal(&mut app);
        choose_row(&mut app, "legal");
        let mut app = app.reopen().expect("the window opens again");
        app.flush().unwrap();
        app.harness().relayout();
        assert!(
            app.texts().iter().any(|text| text == "nda.txt"),
            "the window opens on the folder it showed: {:?}",
            app.texts()
        );
    }

    #[test]
    fn new_file_is_named_in_place_and_made_empty() {
        let mut app = ui::prelude::TestApp::<FilesTreeShell>::in_workspace(WORKSPACE)
            .expect("the window opens");
        app.flush().unwrap();
        let bar = ui::window_toolbar::declared().expect("the window declares its bar");
        assert!(
            bar.actions
                .iter()
                .any(|action| action.name == "New file" && action.enabled),
            "the bar offers New file"
        );
        app.effect(&FilesWindowEffect::NewFile { name: None })
            .expect("New file answers");
        app.flush().unwrap();
        app.harness()
            .click_named("files-tree-new-file-create")
            .expect("the naming row offers Create");
        app.flush().unwrap();
        let texts = app.texts();
        assert!(
            !texts.iter().any(|text| text.starts_with("Couldn’t")),
            "the file was made: {texts:?}"
        );
        assert!(
            texts.iter().any(|text| text == "New file.txt"),
            "the new file is listed: {texts:?}"
        );
        assert!(
            app.harness()
                .click_named("files-tree-new-file-create")
                .is_err(),
            "the naming row is gone once the file is made"
        );
        // Made, it opens: its empty bytes and its name hand it to Notes to be written.
        assert_eq!(
            app.with_widget::<TreeBrowser, _>(|browser| browser
                .handoffs
                .borrow()
                .contains_key("so.tilo.notes")),
            Some(true),
            "the new file is handed to Notes"
        );
    }

    #[test]
    fn an_empty_workspace_is_a_file_manager_with_nothing_in_it_yet() {
        let mut app = ui::prelude::TestApp::<FilesTreeShell>::in_workspace(WORKSPACE)
            .expect("the window opens");
        app.flush().unwrap();
        let texts = app.texts();
        assert!(
            texts.iter().any(|text| text == "This folder is empty"),
            "the empty folder says so in its list: {texts:?}"
        );
        // The bar says where the window is and offers what puts something into it.
        let bar = ui::window_toolbar::declared().expect("the window declares its bar");
        let location = bar.location.expect("the bar shows the folder");
        assert_eq!(location.label, "Workspace");
        let offered: Vec<(&str, bool)> = bar
            .actions
            .iter()
            .map(|action| (action.name.as_str(), action.enabled))
            .collect();
        assert!(
            offered.contains(&("New folder", true)) && offered.contains(&("Upload files", true)),
            "the bar offers New folder and Upload: {offered:?}"
        );
        // New folder asks for its name in the list, and Create makes it there.
        app.effect(&FilesWindowEffect::NewFolder { name: None })
            .expect("New folder answers");
        app.flush().unwrap();
        app.harness()
            .click_named("files-tree-new-folder-create")
            .expect("the naming row offers Create");
        app.flush().unwrap();
        let texts = app.texts();
        assert!(
            !texts.iter().any(|text| text.starts_with("Couldn’t")),
            "the folder was made: {texts:?}"
        );
        assert!(
            app.harness()
                .click_named("files-tree-new-folder-create")
                .is_err(),
            "the naming row is gone once the folder is made"
        );
        assert!(
            texts.iter().any(|text| text == "New folder"),
            "the new folder is listed: {texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text == "This folder is empty"),
            "the folder is no longer empty: {texts:?}"
        );
        let index = app
            .shared_resource::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &fs_protocol::tree::document_id(),
            )
            .expect("the window holds the workspace's tree");
        let listed = index
            .read(fs_protocol::tree::listed_chunks)
            .expect("the tree reads");
        assert_eq!(listed, [1], "the tree lists the chunk the folder went into");
    }

    #[test]
    fn a_tree_in_two_chunks_lists_and_a_change_to_one_chunk_shows() {
        let mut app = ui::prelude::TestApp::<FilesTreeShell>::in_workspace(WORKSPACE)
            .expect("the window opens");
        let index = app
            .shared_resource::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &fs_protocol::tree::document_id(),
            )
            .expect("the window holds the workspace's tree");
        let first = app
            .shared_resource::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(1),
            )
            .expect("the window opens the first chunk");
        let second = app
            .shared_resource::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(2),
            )
            .expect("the window opens the second chunk");
        let mut held = Tree::default();
        let author = hex::encode([9_u8; 32]);
        // A folder in the first chunk, and a file in it in the second: the window's rows
        // walk one tree whatever chunk a node is in.
        let mut first_doc: AutoCommit = first.read(Clone::clone).expect("the first chunk reads");
        let docs = fs_protocol::tree::place(
            (&mut first_doc, 1),
            &mut held,
            ROOT_ID,
            "docs",
            Placing::Folder,
            (1_000, &author),
        )
        .unwrap();
        first
            .record()
            .merge_peer_document(first_doc.save())
            .expect("the first chunk arrives");
        let mut second_doc: AutoCommit = second.read(Clone::clone).expect("the second chunk reads");
        fs_protocol::tree::place(
            (&mut second_doc, 2),
            &mut held,
            &docs.id,
            "plan.md",
            Placing::File {
                hash: &"a".repeat(64),
                size: 12,
            },
            (2_000, &author),
        )
        .unwrap();
        second
            .record()
            .merge_peer_document(second_doc.save())
            .expect("the second chunk arrives");
        let mut index_doc: AutoCommit = index.read(Clone::clone).expect("the resource reads");
        fs_protocol::tree::list_chunk(&mut index_doc, 1).unwrap();
        fs_protocol::tree::list_chunk(&mut index_doc, 2).unwrap();
        index
            .record()
            .merge_peer_document(index_doc.save())
            .expect("the tree arrives");
        app.flush().unwrap();
        let texts = app.texts();
        assert!(
            texts.iter().any(|text| text == "docs"),
            "the window lists the folder of the first chunk: {texts:?}"
        );
        // A change of the second chunk alone shows, and the first chunk's folder stays.
        let mut again: AutoCommit = second
            .read(Clone::clone)
            .expect("the second chunk reads again");
        fs_protocol::tree::place(
            (&mut again, 2),
            &mut held,
            ROOT_ID,
            "notes.txt",
            Placing::File {
                hash: &"b".repeat(64),
                size: 3,
            },
            (3_000, &author),
        )
        .unwrap();
        second
            .record()
            .merge_peer_document(again.save())
            .expect("the change arrives");
        app.flush().unwrap();
        let texts = app.texts();
        assert!(
            texts.iter().any(|text| text == "notes.txt"),
            "a change of the second chunk shows: {texts:?}"
        );
        assert!(
            texts.iter().any(|text| text == "docs"),
            "the folder in the untouched chunk is still listed: {texts:?}"
        );
    }
}
