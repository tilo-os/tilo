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
use fs_protocol::tree::{FILES_APP, ROOT_ID, Tree, TreeEntry};
use ui::carry_out::{ContentLinkAsk, ContentLinkFile, Offers};
use ui::framework::{Dispatcher, Live, People, ResourceApp, ViewCx, WorkspaceResource};
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
    const ID: &'static str = ui::apps::FILE_EXPLORER;
    const TITLE: &'static str = "Files";
    const TITLE_KEY: &'static str = "files-app-title";
    const ICON: Option<&'static str> = Some("file-explorer");
    /// The workspace's tree, the record every member holds: the window shows it and its
    /// History reads its changes, so it is opened with the window.
    const PERSON_RESOURCES: &'static [ui::framework::PersonKind] =
        &[ui::framework::PersonKind::shared::<
            fs_protocol::tree::FilesTree,
        >()];
    /// The folder this window shows: every file's changes in it, as one list, read from
    /// the tree record its rows are.
    const FOLDER_HISTORY: Option<ui::framework::FolderHistory> =
        Some(ui::framework::FolderHistory { title: "Folder" });

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
    tree: Option<WorkspaceResource<fs_protocol::tree::FilesTree>>,
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
        let people = cx.people();
        // The workspace's tree is the record every member holds, under the
        // Files app's own name for it — the one the page's search reads
        // when it finds a file by what it says.
        let tree = cx.shared_resource::<fs_protocol::tree::FilesTree>(
            FILES_APP,
            &fs_protocol::tree::document_id(),
        );
        let mut browser = Self {
            live: cx.live().clone(),
            tree,
            people,
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
        let tree = self.tree.as_ref().map(WorkspaceResource::get);
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
            row = row.carries_out(self.offers.offer(carried.clone()));
            row = row.action(Icon::Download, label, move || {
                ui::carry_out::save(carried.clone());
            });
        }
        if let Some(author) = self.author_of(node) {
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
            // A tree that records no version here yet: the person or device
            // the workspace names, as a name alone. One it does not name (the
            // workspace's host, which writes the files of notes) is left
            // unnamed rather than shown as its key.
            let name = self
                .people
                .name(&ui::tilo_resource::MemberId::person(author));
            if name != author {
                row = row.meta(name);
            }
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
        let document_id = node.document_id?;
        let tree = self.tree.as_ref()?;
        let (version, attribution) =
            tree.read(|doc| fs_protocol::history::latest(doc, document_id))??;
        let registry = self.people.registry();
        let folder = node
            .parent
            .as_deref()
            .filter(|parent| *parent != ROOT_ID)
            .map(|parent| {
                let held = self.tree.as_ref().map(WorkspaceResource::get);
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

    /// Open the file at node `id`, as choosing its row does: a folder is shown, a file is
    /// opened in the app its bytes say opens it once they are here. Whether there is
    /// such a node to open.
    fn open_node(&self, id: &str) -> bool {
        let Some(tree) = self.tree.clone() else {
            return false;
        };
        let held = tree.get();
        let Some(node) = held.node(id).filter(|_| !held.trashed(id)).cloned() else {
            return false;
        };
        drop(held);
        let name = Self::name_of(&node);
        if node.kind == FileNodeKind::Directory {
            self.dispatch
                .submit(FilesWindow::paths().folder.set(Atom(id.to_owned())));
            return true;
        }
        let hash = if let Some(hash) = node.hash {
            if let Some(read) = &self.read {
                // Reading content the page lacks asks the workspace for it.
                let _ = read(&hash);
            }
            hash
        } else {
            let file = tree
                .read(|doc| fs_protocol::tree::document_file(doc, id, &ui::content::NoContent))
                .flatten();
            let Some(hash) = file
                .as_deref()
                .and_then(|file| self.keep.as_ref().and_then(|keep| keep(file)))
            else {
                return false;
            };
            hash
        };
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

impl Decorator for TreeBrowser {
    ui::provides!(dyn ui::history_mode::capability::PresentsHistoryEntries);

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
            document_id: None,
            codec: None,
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
    use fs_protocol::tree::Placing;
    use fs_protocol::{
        ContentHash, Ed25519PublicKey, FileActor, FileAttributionV1, FileProvenance,
        FileVersionAttributionV1, FileVersionId, FileVersionV1,
    };
    use ui::automerge::{AutoCommit, ObjType};

    const WORKSPACE: &str = "00000000-0000-4000-8000-0000000000f1";

    /// A version of `document_id` written by an agent session for the person at `[7; 32]`.
    fn write_version(doc: &mut AutoCommit, document_id: fs_protocol::FileDocumentId, id: u128) {
        use ui::automerge::ScalarValue;
        use ui::tilo_resource::Slot;

        let version = FileVersionV1 {
            id: FileVersionId(uuid::Uuid::from_u128(id)),
            document_id,
            parents: Vec::new(),
            actor: FileActor {
                person: Ed25519PublicKey([7; 32]),
                device: Ed25519PublicKey([9; 32]),
            },
            timestamp_ms: 1_767_225_600_000 + u64::try_from(id).unwrap() * 1_000,
            content_hash: Some(ContentHash([1; 32])),
            metadata_hash: ContentHash([2; 32]),
            provenance: FileProvenance::Edit,
        };
        let root = Slot::Key(&ui::automerge::ROOT, "history")
            .object(doc, ObjType::Map)
            .unwrap();
        let record = Slot::Key(&root, &document_id.0.to_string())
            .object(doc, ObjType::Map)
            .unwrap();
        let versions = Slot::Key(&record, "versions")
            .object(doc, ObjType::Map)
            .unwrap();
        Slot::Key(&versions, &version.id.0.to_string())
            .put_scalar(
                doc,
                ScalarValue::Bytes(fs_protocol::canonical::canonical_bytes(&version).unwrap()),
            )
            .unwrap();
        let attributed = FileVersionAttributionV1 {
            version_id: version.id,
            attribution: FileAttributionV1 {
                writer: fs_protocol::AttributedWriter::agent_session("claude", "abc"),
                label: "Claude Code".into(),
                color: [10, 20, 30],
            },
        };
        let map = Slot::Key(&record, "attributions")
            .object(doc, ObjType::Map)
            .unwrap();
        Slot::Key(&map, &version.id.0.to_string())
            .put_scalar(
                doc,
                ScalarValue::Bytes(fs_protocol::canonical::canonical_bytes(&attributed).unwrap()),
            )
            .unwrap();
        doc.commit();
    }

    #[test]
    fn the_page_window_lists_the_trees_files_and_history_their_changes() {
        let mut app = ui::prelude::TestApp::<FilesTreeShell>::in_workspace(WORKSPACE)
            .expect("the window opens");
        let tree = app
            .shared_resource::<fs_protocol::tree::FilesTree>(
                FILES_APP,
                &fs_protocol::tree::document_id(),
            )
            .expect("the window holds the workspace's tree");
        // The tree as a device that mounts the folder writes it: the record's own document,
        // changed elsewhere and arriving here.
        let mut doc: AutoCommit = tree.read(Clone::clone).expect("the record reads");
        let mut held = Tree::default();
        let placed = fs_protocol::tree::place(
            &mut doc,
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
        let document_id = held.node(&placed.id).unwrap().document_id.unwrap();
        write_version(&mut doc, document_id, 1);
        let folder = fs_protocol::tree::place(
            &mut doc,
            &mut held,
            ROOT_ID,
            "legal",
            Placing::Folder,
            (2_000, &hex::encode([9_u8; 32])),
        )
        .unwrap();
        fs_protocol::tree::place(
            &mut doc,
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
        let folder_record = held.node(&folder.id).unwrap().document_id.unwrap();
        write_version(&mut doc, folder_record, 2);
        tree.record()
            .merge_peer_document(doc.save())
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
            .click_named("history-open")
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
                    .is_some_and(|label| label.ends_with("Edited legal"))
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
}
