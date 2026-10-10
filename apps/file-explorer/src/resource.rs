//! Files as a resource: the person's bookmarked places and the filesystem
//! changes made from Files.
//!
//! The document holds the places (pinned in the sidebar), in order, and the
//! queue of filesystem changes ([`FileExplorer::ops`]): each is a verb that
//! queues a [`FileOp`] for the device whose disk it changes, which carries it
//! out and settles it with what it changed. Undoing one queues its revert
//! the same way. How a window shows folders (list or grid, sort, item size,
//! hidden files) is the person's [`FilesPreferences`], which follow them to
//! their other devices and are never seen by collaborators. The folder a
//! window shows, its listing and selection are the window's own; its other
//! work is effects on the live rows the surface publishes.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, FileHistory, Fixture, LiveRow, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::storage::{Scope, SettingsSchema};
use ui::tilo_resource::{
    self, ActionCx, ActionDecl, Atom, Collection, Id, Invoke, Model, Outbox, Refusal, Settled,
    Undoing, resource,
};
use ui::widget::Element;

use crate::effects::FilesEffect;
use crate::file_ops::{Change, FileOp};
use crate::files_app::{FileExplorerWidget, Ran};
use crate::model::{SortBy, SortOrder, ViewMode};

/// How many carried-out filesystem changes the document keeps, so recent
/// ones can still be undone.
const KEPT_OPS: usize = 64;

/// The most entries one move or copy takes.
pub(crate) const MAX_MOVE_ENTRIES: usize = 256;

/// One bookmarked place, stored as [`crate::places`] names it so it means
/// the same folder on every device.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Bookmark {
    /// Place
    #[model(label)]
    #[model(readonly)]
    pub place: Atom,
}

tilo_resource::json_model!(FileOp);

/// A filesystem change queued for the device whose disk it changes.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct FileTask {
    /// The device that alone carries it out.
    pub device: String,
    pub op: FileOp,
}

/// The Files document: the bookmarked places behind every Files window and
/// the filesystem changes queued from Files.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct FileExplorer {
    /// The bookmarked places, in order.
    ///
    /// Each is stored under an id derived from the place, so two people
    /// pinning the same folder pin it once.
    pub places: Collection<Bookmark>,
    /// Filesystem changes, each settled by its device with what it changed.
    pub ops: Outbox<FileTask, Vec<Change>>,
}

/// The id a bookmark of `place` is stored under.
#[must_use]
pub fn bookmark_id(place: &str) -> Id {
    let digest = Sha256::digest(place.as_bytes());
    Id::from_raw(format!(
        "place-{}",
        hex::encode(digest.get(..12).unwrap_or(&digest))
    ))
}

/// `entry` of the folder `folder`: a name in it, or an absolute path.
fn entry_path(folder: &str, entry: &str) -> Result<String, Refusal> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err(Refusal::new("an entry is named"));
    }
    if std::path::Path::new(entry).is_absolute() {
        return Ok(entry.to_owned());
    }
    if !std::path::Path::new(folder).is_absolute() || entry.contains('/') {
        return Err(Refusal::new(format!(
            "`{entry}` is neither a name in the folder nor an absolute path"
        )));
    }
    Ok(std::path::Path::new(folder)
        .join(entry)
        .display()
        .to_string())
}

/// The paths of `entries` of `folder`: at least one, at most
/// [`MAX_MOVE_ENTRIES`].
fn entry_paths(folder: &str, entries: &[String]) -> Result<Vec<String>, Refusal> {
    if entries.is_empty() || entries.len() > MAX_MOVE_ENTRIES {
        return Err(Refusal::new(format!(
            "name 1 to {MAX_MOVE_ENTRIES} entries"
        )));
    }
    entries
        .iter()
        .map(|entry| entry_path(folder, entry))
        .collect()
}

fn absolute_folder(folder: &str) -> Result<String, Refusal> {
    if std::path::Path::new(folder).is_absolute() {
        Ok(folder.to_owned())
    } else {
        Err(Refusal::new(format!("{folder} is not an absolute path")))
    }
}

fn file_name(name: &str) -> Result<String, Refusal> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(Refusal::new("a name, without a slash"));
    }
    Ok(name.to_owned())
}

/// Queue `op` for `device` to carry out, forgetting the oldest settled
/// changes beyond [`KEPT_OPS`].
fn queue(
    cx: &mut ActionCx,
    ops: &mut Outbox<FileTask, Vec<Change>>,
    device: String,
    op: FileOp,
) -> Result<(), Refusal> {
    if device.trim().is_empty() {
        return Err(Refusal::new("the device whose files change is named"));
    }
    ops.push(cx.id(), FileTask { device, op });
    ops.prune(KEPT_OPS);
    Ok(())
}

/// Record the native transfer `receipt` names, settled at once. Recorded
/// again while it is taken back (a redo), it asks the host to make it again.
fn record_native(
    cx: &mut ActionCx,
    ops: &mut Outbox<FileTask, Vec<Change>>,
    device: String,
    receipt: &crate::files_app::ClipboardReceipt,
) -> Result<(), Refusal> {
    if uuid::Uuid::parse_str(&receipt.operation).is_err()
        || receipt.completed == 0
        || receipt.completed > 256
    {
        return Err(Refusal::new("not a clipboard receipt"));
    }
    let operation = receipt.operation.clone();
    let recorded = ops.entries().any(|(_, entry)| {
        matches!(&entry.value.op, FileOp::ClipboardReceipt { operation: recorded } if *recorded == operation)
    });
    if !recorded {
        let id = cx.id();
        ops.push(
            id.clone(),
            FileTask {
                device,
                op: FileOp::ClipboardReceipt {
                    operation: operation.clone(),
                },
            },
        );
        ops.settle(
            &id,
            Settled::Done(vec![Change::ClipboardTransfer { operation }]),
        );
        ops.prune(KEPT_OPS);
        return Ok(());
    }
    // The latest settled step of this transfer says whether it is in effect.
    let latest = ops
        .entries()
        .filter_map(|(id, entry)| Some((id, entry.settled.as_ref()?)))
        .filter(|(_, settled)| {
            settled.result().iter().any(|change| {
                matches!(change, Change::ClipboardTransfer { operation: o } | Change::ClipboardUndo { operation: o } if *o == operation)
            })
        })
        .last();
    match latest {
        Some((undone, settled))
            if settled
                .result()
                .iter()
                .any(|change| matches!(change, Change::ClipboardUndo { .. })) =>
        {
            let undone = undone.clone();
            queue(
                cx,
                ops,
                device,
                FileOp::Revert {
                    op: undone.as_str().to_owned(),
                },
            )
        }
        _ => Err(Refusal::new("the transfer is already recorded")),
    }
}

#[resource(kind = "files.folder", label = "Folder", initialize = FileExplorer::with_default_places)]
impl FileExplorer {
    /// Pin a folder to the Favourites in the sidebar.
    #[action(label = "Add to Favourites")]
    pub fn add_bookmark(
        &mut self,
        #[param("Folder path on this device, or a stored place such as xdg:documents")]
        place: String,
        #[param("Position among the Favourites, from 0; the end when none")] at: Option<usize>,
    ) -> Result<(), Refusal> {
        let place = place.trim();
        if place.is_empty() {
            return Err(Refusal::new("a folder is named"));
        }
        let id = bookmark_id(place);
        if self.places.get(&id).is_none() {
            let len = self.places.len();
            self.places.insert(
                at.unwrap_or(len).min(len),
                id,
                Bookmark {
                    place: Atom(place.to_owned()),
                },
            );
        }
        Ok(())
    }

    /// Unpin a folder from the Favourites.
    #[action(label = "Remove from Favourites")]
    pub fn remove_bookmark(
        &mut self,
        #[param("Folder path on this device, or a stored place such as xdg:documents")]
        place: String,
    ) -> Result<(), Refusal> {
        self.places
            .remove(&bookmark_id(place.trim()))
            .map(drop)
            .ok_or_else(|| Refusal::new("the folder is not a favourite"))
    }

    /// Create a folder.
    #[action(undo = Self::revert_queued)]
    pub fn new_folder(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device whose files change")] device: String,
        #[param("Absolute path of the folder to create it in")] parent: String,
        #[param("Name of the new folder")] name: String,
    ) -> Result<(), Refusal> {
        let op = FileOp::NewFolder {
            parent: absolute_folder(&parent)?,
            name: file_name(&name)?,
        };
        queue(cx, ops, device, op)
    }

    /// Rename an entry.
    #[action(undo = Self::revert_queued)]
    pub fn rename(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device whose files change")] device: String,
        #[param("Absolute path of the folder holding the entry")] folder: String,
        #[param("The entry: its name in the folder, or its absolute path")] entry: String,
        #[param("The new name")] name: String,
    ) -> Result<(), Refusal> {
        let op = FileOp::Rename {
            path: entry_path(&folder, &entry)?,
            name: file_name(&name)?,
        };
        queue(cx, ops, device, op)
    }

    /// Move entries to Trash; undo puts them back.
    #[action(label = "Move to Trash", undo = Self::revert_queued)]
    pub fn trash(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device whose files change")] device: String,
        #[param("Absolute path of the folder holding the entries")] folder: String,
        #[param("The entries: names in the folder, or absolute paths")] entries: Vec<String>,
    ) -> Result<(), Refusal> {
        let op = FileOp::Trash {
            paths: entry_paths(&folder, &entries)?,
        };
        queue(cx, ops, device, op)
    }

    /// Move entries into another folder in one step, keeping their names.
    #[action(id = "move", label = "Move entries to folder", undo = Self::revert_queued)]
    pub fn move_entries(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device whose files change")] device: String,
        #[param("Absolute path of the folder holding the entries")] from: String,
        #[param("The entries: names in that folder, or absolute paths")] entries: Vec<String>,
        #[param("Absolute path of the destination folder")] to: String,
        #[param("Create the destination folder when it does not exist (the default)")]
        create: Option<bool>,
    ) -> Result<(), Refusal> {
        let op = FileOp::Move {
            paths: entry_paths(&from, &entries)?,
            folder: absolute_folder(&to)?,
            create: create.unwrap_or(true),
        };
        queue(cx, ops, device, op)
    }

    /// Copy entries into a folder; a copy whose name is taken gets a free
    /// one.
    #[action(undo = Self::revert_queued)]
    pub fn copy_into(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device whose files change")] device: String,
        #[param("Absolute path of the folder holding the entries")] from: String,
        #[param("The entries: names in that folder, or absolute paths")] entries: Vec<String>,
        #[param("Absolute path of the destination folder")] to: String,
    ) -> Result<(), Refusal> {
        let op = FileOp::Copy {
            paths: entry_paths(&from, &entries)?,
            folder: absolute_folder(&to)?,
        };
        queue(cx, ops, device, op)
    }

    /// Record a paste the native clipboard host already carried out, so it
    /// is in history and undoable like any change: undo asks the host to
    /// take back exactly what it transferred.
    #[action(
        id = "record-clipboard-transfer",
        label = "Paste files",
        undo = Self::revert_queued
    )]
    pub fn record_transfer(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The receiving device")] device: String,
        #[param("The host's receipt of the transfer; grants no authority")]
        receipt: crate::files_app::ClipboardReceipt,
    ) -> Result<(), Refusal> {
        record_native(cx, ops, device, &receipt)
    }

    /// Record a restore from Trash the native host already carried out, so
    /// it is in history and undoable.
    #[action(id = "record-trash-restore", label = "Restore file", undo = Self::revert_queued)]
    pub fn record_restore(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The receiving device")] device: String,
        #[param("The host's receipt of the restore; grants no authority")]
        receipt: crate::files_app::ClipboardReceipt,
    ) -> Result<(), Refusal> {
        record_native(cx, ops, device, &receipt)
    }

    /// Take back a filesystem change: the device that made it compensates
    /// what it recorded, in reverse order.
    #[action(item = ops, undo = Self::revert_queued)]
    pub fn revert(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        op: Id,
    ) -> Result<(), Refusal> {
        let device = ops.get(&op).ok_or("no such change")?.value.device.clone();
        queue(
            cx,
            ops,
            device,
            FileOp::Revert {
                op: op.as_str().to_owned(),
            },
        )
    }
}

impl FileExplorer {
    /// A new document's places: [`crate::places::DEFAULT_PLACES`].
    fn with_default_places() -> Self {
        let mut files = Self::default();
        for place in crate::places::DEFAULT_PLACES {
            files.places.push(
                bookmark_id(place),
                Bookmark {
                    place: Atom((*place).to_owned()),
                },
            );
        }
        files
    }

    /// Undoing a queued change queues its revert for the same device.
    fn revert_queued<P>(undoing: Undoing<'_, Self, P>) -> Option<Invoke<Self>> {
        Some(Self::REVERT.with(undoing.minted.first()?.clone()))
    }

    /// The bookmarked places, in order.
    #[must_use]
    pub fn bookmarks(&self) -> Vec<String> {
        self.places
            .values()
            .map(|bookmark| bookmark.place.0.clone())
            .collect()
    }

    /// The verb that queues `op` for `device`; none for a native transfer,
    /// which only its receipt records.
    pub(crate) fn queueing(device: &str, op: FileOp) -> Option<Invoke<Self>> {
        let device = device.to_owned();
        Some(match op {
            FileOp::NewFolder { parent, name } => Self::NEW_FOLDER.with(file_explorer::NewFolder {
                device,
                parent,
                name,
            }),
            FileOp::Rename { path, name } => Self::RENAME.with(file_explorer::Rename {
                device,
                folder: String::new(),
                entry: path,
                name,
            }),
            FileOp::Trash { paths } => Self::TRASH.with(file_explorer::Trash {
                device,
                folder: String::new(),
                entries: paths,
            }),
            FileOp::Move {
                paths,
                folder,
                create,
            } => Self::MOVE_ENTRIES.with(file_explorer::MoveEntries {
                device,
                from: String::new(),
                entries: paths,
                to: folder,
                create: Some(create),
            }),
            FileOp::Copy { paths, folder } => Self::COPY_INTO.with(file_explorer::CopyInto {
                device,
                from: String::new(),
                entries: paths,
                to: folder,
            }),
            FileOp::Revert { op } => Self::REVERT.with(Id::from_raw(op)),
            FileOp::ClipboardReceipt { .. } => return None,
        })
    }
}

/// How the person likes folders shown, on every device they use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesPreferences {
    /// `list` or `grid`.
    pub view: String,
    /// `name`, `size`, `type` or `modified`.
    pub sort: String,
    /// `ascending` or `descending`.
    pub order: String,
    /// The item size step ([`crate::config::zoom_scale`]).
    pub zoom: i32,
    /// Whether dotfiles are listed.
    pub hidden: bool,
}

impl Default for FilesPreferences {
    fn default() -> Self {
        Self {
            view: "list".into(),
            sort: "name".into(),
            order: "ascending".into(),
            zoom: 0,
            hidden: false,
        }
    }
}

impl SettingsSchema for FilesPreferences {
    const SCOPE: Scope = Scope::Personal;
}

pub(crate) fn view_mode_token(mode: ViewMode) -> &'static str {
    match mode {
        ViewMode::List => "list",
        ViewMode::Grid => "grid",
    }
}

pub(crate) fn parse_view_mode(token: &str) -> ViewMode {
    match token {
        "grid" => ViewMode::Grid,
        _ => ViewMode::List,
    }
}

pub(crate) fn sort_key_token(sort: SortBy) -> &'static str {
    match sort {
        SortBy::Name => "name",
        SortBy::Size => "size",
        SortBy::Modified => "modified",
        SortBy::Type => "type",
    }
}

pub(crate) fn parse_sort_key(token: &str) -> SortBy {
    match token {
        "size" => SortBy::Size,
        "modified" => SortBy::Modified,
        "type" => SortBy::Type,
        _ => SortBy::Name,
    }
}

pub(crate) fn sort_order_token(order: SortOrder) -> &'static str {
    match order {
        SortOrder::Ascending => "ascending",
        SortOrder::Descending => "descending",
    }
}

pub(crate) fn parse_sort_order(token: &str) -> SortOrder {
    match token {
        "descending" => SortOrder::Descending,
        _ => SortOrder::Ascending,
    }
}

/// A row the window publishes, as the framework carries it.
fn live_row(resource: tilo_actions::Resource) -> LiveRow {
    let mut row = LiveRow::new(resource.id.as_str(), resource.kind.as_str(), resource.label);
    if let Some(presentation) = resource.presentation {
        row = row.presentation(presentation);
    }
    for action in resource.actions {
        row = row.operation(action);
    }
    for child in resource.children {
        row = row.row(live_row(child));
    }
    row
}

/// Framework app: the files widget is the surface over [`FileExplorer`].
pub struct FileExplorerShell;

impl ResourceApp for FileExplorerShell {
    type Resource = FileExplorer;
    const ID: &'static str = "so.tilo.file-explorer";
    const TITLE: &'static str = "Files";
    const ICON: Option<&'static str> = Some("file-explorer");
    /// A folder to open (`file-explorer ~/Documents`), or a received share
    /// link (`file-explorer tilo-file://v2/…`) to admit and open.
    const ARGUMENTS: usize = 1;
    /// A received share link on standard input
    /// (`--open-share-stdin`), so its secret stays out of the process list.
    const FLAGS: &'static [&'static str] = &[crate::received_shares::STDIN_FLAG];
    const FILE_HISTORY: Option<FileHistory> = Some(FileHistory {
        title: "Selected file",
        preview: crate::framework_history::preview,
    });

    fn view(cx: &mut ViewCx<FileExplorer>) -> Element {
        // Mounted in the shell, Files starts where the shell says and opens
        // documents through the shell's services; in its own process, at
        // the person's home.
        let lent = cx.take_lent::<crate::StartupInputs>();
        let services = cx.take_lent::<crate::files_app::HostedFileServices>();
        let widget = match (cx.fixture::<crate::scenarios::Scene>(), lent) {
            (Some(scene), _) => FileExplorerWidget::for_scene(&scene),
            (None, Some(inputs)) => match services {
                Some(services) => {
                    FileExplorerWidget::with_startup_inputs_and_services(inputs, services)
                }
                None => FileExplorerWidget::with_startup_inputs(inputs),
            },
            (None, None) => FileExplorerWidget::new(),
        };
        // A search hit, or an agent's `open`, shows the row it names.
        cx.on_open(widget.opener());
        cx.surface(widget)
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::scenarios()
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1200, 720)
    }

    /// A capture browses folders written for it, never the machine's own.
    fn fixture(fixture: &mut Fixture<FileExplorer>) {
        crate::scenarios::prepare(fixture);
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }
}

/// The native transfer `task` takes back (`false`) or makes again (`true`),
/// when it reverts a step of one.
fn native_compensation(state: &FileExplorer, task: &FileTask) -> Option<(String, bool)> {
    let FileOp::Revert { op } = &task.op else {
        return None;
    };
    let target = state.ops.outcome(&Id::from_raw(op.clone()))?;
    target.result().iter().find_map(|change| match change {
        Change::ClipboardTransfer { operation } => Some((operation.clone(), false)),
        Change::ClipboardUndo { operation } => Some((operation.clone(), true)),
        _ => None,
    })
}

/// Carry out `task` on this device, given the record as it stands: a revert
/// waits until the change it takes back is carried out.
fn carry_out(state: &FileExplorer, task: &FileTask) -> EffectAnswer {
    let reverting = match &task.op {
        FileOp::Revert { op } => {
            let op = Id::from_raw(op.clone());
            match state.ops.outcome(&op) {
                Some(settled) => Some(settled.result().clone()),
                None if state.ops.get(&op).is_some() => {
                    return Err("waiting for the change it takes back".into());
                }
                None => None,
            }
        }
        _ => None,
    };
    Ok(Effected::settled(&crate::file_ops::execute(
        &task.op,
        reverting.as_deref(),
    )))
}

impl ResourceSurface<FileExplorer, FilesEffect> for FileExplorerWidget {
    fn present(&mut self, state: &Rc<FileExplorer>) {
        self.present_places(&state.bookmarks());
        self.follow_ops(&state.ops);
    }

    fn attach(&mut self, cx: SurfaceCx<FileExplorer>) {
        if let Some(source) = cx.file_history() {
            self.show_file_history_in(source);
        }
        if let Some(argument) = cx.launch_arguments().first() {
            self.open_launch_argument(argument);
        }
        if cx.mode().is_live() && cx.launch_flag(crate::received_shares::STDIN_FLAG) {
            match crate::received_shares::read_link(std::io::stdin()) {
                Ok(link) => self.admit_received_share(link),
                Err(error) => self.set_external_status(error),
            }
        }
        // Each device carries out the filesystem changes queued for it, and
        // records what it changed; a change asked for here runs under the
        // invocation that asked, which completes once it settled.
        let native = self.native_undo_queue();
        let ops = cx.device().map(|device| {
            let me = device.to_owned();
            cx.outbox_awaited(
                FileExplorer::paths().ops,
                move |_, task: &FileTask| task.device == me,
                move |_, task, cx| {
                    let Some((operation, redo)) =
                        cx.live().with(|state| native_compensation(state, &task))
                    else {
                        return cx.live().with(|state| carry_out(state, &task));
                    };
                    // The host acts for the invocation that asked for it.
                    let invocation = cx
                        .invocation()
                        .ok_or("the clipboard host acts only for the change's own invocation")?;
                    if native.borrow().len() >= 16 {
                        return Err("clipboard compensation queue is full".into());
                    }
                    let expected = operation.clone();
                    let reply = cx.reply_then(move |(), output, _| {
                        if output
                            .value
                            .as_ref()
                            .and_then(|value| value.get("operation"))
                            .and_then(serde_json::Value::as_str)
                            != Some(expected.as_str())
                        {
                            return Err(
                                "clipboard Undo receipt differs from its current consequence"
                                    .into(),
                            );
                        }
                        let operation = expected.clone();
                        Ok(Effected::settled(&Settled::Done(vec![if redo {
                            Change::ClipboardTransfer { operation }
                        } else {
                            Change::ClipboardUndo { operation }
                        }])))
                    });
                    native.borrow_mut().push(crate::files_app::UndoRequest {
                        redo,
                        invocation,
                        operation,
                        reply,
                    });
                    Ok(Effected::Pending)
                },
            )
        });
        self.attach_framework(
            cx.dispatcher(),
            cx.settings::<FilesPreferences>(),
            cx.settings::<crate::recent::RecentFiles>(),
            cx.device().map(str::to_owned),
            ops,
        );
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: FilesEffect,
        cx: &mut EffectCx<FileExplorer, Self>,
    ) -> EffectAnswer {
        let recovery =
            matches!(effect, FilesEffect::RecoverClipboardTransfers) && self.hosted_clipboard();
        let receipt = matches!(effect, FilesEffect::Paste | FilesEffect::Restore { .. })
            && self.hosted_clipboard();
        Ok(match self.run_effect_for(effect, cx.invocation())? {
            Ran::Done(Some(answer)) => Effected::value(answer),
            Ran::Done(None) => Effected::done(),
            Ran::Pending => {
                self.await_effect(if recovery {
                    cx.reply_then(|_, output, cx| {
                        let value = output.value.ok_or("clipboard recovery receipt absent")?;
                        let operations: Vec<String> = serde_json::from_value(
                            value
                                .get("operations")
                                .ok_or("clipboard recovery operations absent")?
                                .clone(),
                        )
                        .map_err(|error| error.to_string())?;
                        // An Undo the host carried out before it could answer
                        // (a restart, a killed process) is settled from its
                        // durable receipt.
                        let recovered: Vec<(Id, String)> = cx.live().with(|state| {
                            state
                                .ops
                                .pending()
                                .filter_map(|(id, task)| {
                                    let (operation, redo) = native_compensation(state, task)?;
                                    (!redo && operations.contains(&operation))
                                        .then(|| (id.clone(), operation))
                                })
                                .collect()
                        });
                        for (id, operation) in recovered {
                            cx.submit(FileExplorer::paths().ops.settle(
                                &id,
                                &Settled::Done(vec![Change::ClipboardUndo { operation }]),
                            ));
                        }
                        if let Some(error) = value.get("error").and_then(serde_json::Value::as_str)
                        {
                            return Err(error.to_owned());
                        }
                        Ok(Effected::value(value))
                    })
                } else if receipt {
                    cx.reply_then(|files, output, cx| {
                        let receipt: crate::files_app::ClipboardReceipt = serde_json::from_value(
                            output.value.ok_or("clipboard host receipt absent")?,
                        )
                        .map_err(|error| error.to_string())?;
                        if receipt.completed > 0 {
                            files.follow_clipboard_receipt(&receipt);
                            let device = files
                                .clipboard_device()
                                .ok_or("clipboard receiving device absent")?;
                            cx.submit(if receipt.restore {
                                FileExplorer::RECORD_RESTORE.with(file_explorer::RecordRestore {
                                    device,
                                    receipt: receipt.clone(),
                                })
                            } else {
                                FileExplorer::RECORD_TRANSFER.with(file_explorer::RecordTransfer {
                                    device,
                                    receipt: receipt.clone(),
                                })
                            });
                        }
                        if let Some(error) = receipt.error {
                            return Err(error);
                        }
                        Ok(Effected::done())
                    })
                } else {
                    cx.reply()
                });
                Effected::Pending
            }
        })
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// Pin the folder shown to the Favourites, or unpin it; change the
    /// files of the folder shown.
    fn bound_declarations(&self) -> Vec<ActionDecl<FileExplorer>> {
        let mut declarations = Vec::new();
        if let Some(place) = self.current_place() {
            declarations.extend(if self.current_is_favourite() {
                FileExplorer::REMOVE_BOOKMARK
                    .bind(file_explorer::RemoveBookmark::paths().place, place)
                    .declaration(
                        "remove-current-bookmark",
                        "Remove this folder from Favourites",
                    )
            } else {
                FileExplorer::ADD_BOOKMARK
                    .bind(file_explorer::AddBookmark::paths().place, place)
                    .declaration("add-current-bookmark", "Add this folder to Favourites")
            });
        }
        declarations.extend(self.file_op_declarations());
        declarations
    }

    /// The listing's entries, remote accounts, nearby transfers and a
    /// failed listing's retry.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.published_rows().into_iter().map(live_row).collect()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.published_row(id).map(live_row)
    }

    /// Rename, move and trash the entry a row shows, through the queue.
    fn row_declarations(&self, row: &str) -> Vec<ActionDecl<FileExplorer>> {
        self.entry_declarations(row)
    }

    /// The places, the mount's collaborative records and the remote file
    /// accounts: what the explorer holds, not the document.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.search_entries_shown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::prelude::TestApp;
    use ui::tilo_resource::{Replica, Settled};

    /// A new document holds the standard places, which the window publishes
    /// as rows that open; the filesystem verbs ride the resource.
    #[test]
    fn a_new_document_publishes_the_standard_places() {
        let state = Replica::<FileExplorer>::new().state().unwrap();
        assert_eq!(state.bookmarks(), crate::places::DEFAULT_PLACES);

        let published = ui::tilo_resource::publish::<FileExplorer>("files", "Folder", &state);
        assert_eq!(published.kind, "files.folder");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "add-bookmark",
            "remove-bookmark",
            "field-edit",
            "new-folder",
            "rename",
            "trash",
            "move",
            "copy-into",
            "revert",
        ] {
            assert!(ids.contains(&expected), "missing `{expected}`: {ids:?}");
        }
        let effects: Vec<String> = <FilesEffect as ui::tilo_resource::Effect>::specs()
            .into_iter()
            .map(|spec| spec.id)
            .collect();
        for expected in [
            "refresh",
            "paste",
            "go-to",
            "open-folder",
            "list",
            "grid",
            "sort-name",
        ] {
            assert!(
                effects.iter().any(|id| id == expected),
                "the window's `{expected}` is an effect: {effects:?}"
            );
        }
        for gone in [
            "set-view",
            "set-sort",
            "set-zoom",
            "set-hidden",
            "set-bookmarks",
        ] {
            assert!(!ids.contains(&gone), "`{gone}` is a preference: {ids:?}");
        }

        let mut app = TestApp::<FileExplorerShell>::new().unwrap();
        let rows = app.harness().resources();
        let places: Vec<_> = rows
            .iter()
            .flat_map(|root| std::iter::once(root).chain(&root.children))
            .filter(|row| row.kind == "files.place")
            .collect();
        assert!(!places.is_empty(), "the window publishes its places");
        assert!(
            places
                .iter()
                .all(|place| place.actions.iter().any(|a| a.id == "open")),
            "each place opens"
        );
    }

    fn sync(a: &mut TestApp<FileExplorerShell>, b: &mut TestApp<FileExplorerShell>) {
        let mut doc = b.peer_document();
        a.merge_peer(&mut doc).unwrap();
        b.merge_peer(&mut doc).unwrap();
        a.settle();
        b.settle();
    }

    fn pin(place: &str, at: Option<usize>) -> Invoke<FileExplorer> {
        FileExplorer::ADD_BOOKMARK.with(file_explorer::AddBookmark {
            place: place.into(),
            at,
        })
    }

    /// Bookmarks are edited place by place, so two people pinning at once
    /// both keep their pin, and the same folder pinned twice is one pin.
    #[test]
    fn bookmark_edits_are_granular() {
        let mut laptop = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let mut phone = TestApp::<FileExplorerShell>::on_device("phone").unwrap();
        sync(&mut laptop, &mut phone);
        laptop.invoke(pin("~/Projects", None)).unwrap();
        laptop.invoke(pin("~/Shared", None)).unwrap();
        phone.invoke(pin("/mnt/data", Some(1))).unwrap();
        phone.invoke(pin("~/Shared", None)).unwrap();
        sync(&mut laptop, &mut phone);
        let places = laptop.state().bookmarks();
        assert!(places.contains(&"~/Projects".to_owned()), "{places:?}");
        assert_eq!(places[1], "/mnt/data");
        assert_eq!(
            places.iter().filter(|place| *place == "~/Shared").count(),
            1,
            "{places:?}"
        );
        assert_eq!(phone.state().bookmarks(), places, "both converge");

        laptop
            .invoke(
                Invoke::edit(
                    &FileExplorer::paths()
                        .places
                        .move_to(&bookmark_id("~/Projects"), 0),
                )
                .unwrap(),
            )
            .unwrap();
        laptop
            .invoke(FileExplorer::REMOVE_BOOKMARK.with("xdg:music".to_owned()))
            .unwrap();
        let places = laptop.state().bookmarks();
        assert_eq!(places[0], "~/Projects");
        assert!(!places.contains(&"xdg:music".to_owned()));
        assert!(
            laptop
                .invoke(FileExplorer::REMOVE_BOOKMARK.with("xdg:music".to_owned()))
                .is_err(),
            "a folder that is not a favourite is refused"
        );
    }

    /// Pinning a folder in the window is the resource's action: it lands in
    /// the document, undo takes it back, and the sidebar follows.
    #[test]
    fn pinning_a_folder_is_a_bookmark_action() {
        let folder = tempfile::tempdir().unwrap();
        let mut app = TestApp::<FileExplorerShell>::new().unwrap();
        assert_eq!(app.state().bookmarks(), crate::places::DEFAULT_PLACES);
        app.with_surface(|files: &mut FileExplorerWidget| {
            files.apply_command(crate::FileCommand::AddFavorite(folder.path().to_path_buf()))
        })
        .unwrap();
        app.settle();

        let place = folder.path().display().to_string();
        assert!(
            app.state().bookmarks().contains(&place),
            "{:?}",
            app.state()
        );
        assert!(app.undo());
        assert!(!app.state().bookmarks().contains(&place));
        let shown = app
            .with_surface(|files: &mut FileExplorerWidget| {
                files.favorites().iter().any(|f| f.path == folder.path())
            })
            .unwrap();
        assert!(!shown, "the sidebar shows the stored places");

        app.invoke(pin(&place, None)).unwrap();
        let shown = app
            .with_surface(|files: &mut FileExplorerWidget| {
                files.favorites().iter().any(|f| f.path == folder.path())
            })
            .unwrap();
        assert!(shown, "an agent's pin reaches the sidebar");
    }

    /// The view preferences are the person's settings, not the document.
    #[test]
    fn view_preferences_are_personal_settings() {
        let mut app = TestApp::<FileExplorerShell>::new().unwrap();
        let before = app.peer_document().get_heads();
        app.with_surface(|files: &mut FileExplorerWidget| {
            files.set_view_mode(crate::model::ViewMode::Grid);
            files.apply_command(crate::FileCommand::ZoomIn);
        })
        .unwrap();
        app.settle();
        assert_eq!(
            app.peer_document().get_heads(),
            before,
            "no document change"
        );
        let stored = app.settings::<FilesPreferences>().get();
        assert_eq!(stored.view, "grid");
        assert_eq!(stored.zoom, 1);
    }

    /// A capture browses the folders written for its scenario and reads
    /// nothing of the machine it runs on; its view state is staged once laid
    /// out.
    #[test]
    fn captures_browse_their_own_scene() {
        let guard = crate::model::fixture_io_guard::DenyAmbientIo::new();
        let mut select = TestApp::<FileExplorerShell>::capture(Some("select")).unwrap();
        assert_eq!(select.harness().inspection().viewport, [1200, 720]);
        let frame = select.harness().inspection_json().unwrap();
        assert!(frame.contains("Holiday plan.md"), "{frame}");
        assert!(frame.contains("Development"), "{frame}");
        assert_eq!(guard.attempts(), 0, "a capture read ambient state");
        drop(guard);

        let mut menu = TestApp::<FileExplorerShell>::capture(Some("context-menu")).unwrap();
        let frame = menu.harness().inspection_json().unwrap();
        assert!(
            frame.contains("Meeting notes"),
            "the scene's templates: {frame}"
        );
    }

    /// A launch naming a folder opens it.
    #[test]
    fn a_launch_opens_the_folder_it_names() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("launched.txt"), b"x").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[directory.path().to_str().unwrap()]).unwrap();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("launched.txt"), "{frame}");
    }

    /// Invoke the action `id` of the published resource `on` with `args`
    /// through the door, as an agent does, and let the device carry out
    /// what it queued.
    fn act_on(app: &mut TestApp<FileExplorerShell>, on: &str, id: &str, args: &serde_json::Value) {
        let instance = app.harness().component_instance_id().unwrap();
        app.harness()
            .invoke_bound_action_confirmed(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: instance,
                    address: ui::tilo_actions::ActionAddress::new(on, id),
                },
                args.as_object().unwrap(),
            )
            .unwrap();
        app.settle();
    }

    #[test]
    fn published_batch_copy_is_one_undoable_operation_and_cut_requires_write() {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap();
        let root = tempfile::tempdir_in(home).unwrap();
        let destination = root.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.path().join(name), name.as_bytes()).unwrap();
        }
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[root.path().to_str().unwrap()]).unwrap();
        let entries = ["a.txt", "c.txt"]
            .map(|name| crate::search_provider::entry_resource_id(&root.path().join(name)))
            .to_vec();
        app.effect(&FilesEffect::SelectEntries { entries }).unwrap();
        app.effect(&FilesEffect::CopySelection).unwrap();
        app.effect(&FilesEffect::ClearSelection).unwrap();
        app.effect(&FilesEffect::GoTo {
            path: destination.display().to_string(),
        })
        .unwrap();
        app.effect(&FilesEffect::Paste).unwrap();
        app.settle();
        for name in ["a.txt", "c.txt"] {
            assert_eq!(
                std::fs::read(destination.join(name)).unwrap(),
                name.as_bytes()
            );
        }
        assert!(!destination.join("b.txt").exists());
        assert!(app.undo());
        app.settle();
        for name in ["a.txt", "c.txt"] {
            assert!(!destination.join(name).exists());
            assert_eq!(
                std::fs::read(root.path().join(name)).unwrap(),
                name.as_bytes()
            );
        }

        let mut viewer =
            TestApp::<FileExplorerShell>::with_role(ui::permissions::Role::Viewer).unwrap();
        viewer
            .effect(&FilesEffect::GoTo {
                path: root.path().display().to_string(),
            })
            .unwrap();
        viewer.effect(&FilesEffect::SelectAll).unwrap();
        viewer.effect(&FilesEffect::CopySelection).unwrap();
        assert!(viewer.effect(&FilesEffect::CutSelection).is_err());
        for name in ["a.txt", "b.txt", "c.txt"] {
            assert!(root.path().join(name).exists());
        }
    }

    #[test]
    fn published_selection_capture_observes_the_real_selected_rows() {
        let guard = crate::model::fixture_io_guard::DenyAmbientIo::new();
        let mut app = TestApp::<FileExplorerShell>::capture(Some("selection-actions")).unwrap();
        assert_eq!(
            app.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
            Some(2)
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("Notes.txt"), "{frame}");
        assert!(frame.contains("Recipes.md"), "{frame}");
        assert_eq!(guard.attempts(), 0, "a capture read ambient state");
    }

    #[test]
    fn published_selection_changes_stay_in_the_addressed_files_window() {
        let root = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(root.path().join(name), name.as_bytes()).unwrap();
        }
        let launch = [root.path().to_str().unwrap()];
        let mut source = TestApp::<FileExplorerShell>::launched(&launch).unwrap();
        let mut other = TestApp::<FileExplorerShell>::launched(&launch).unwrap();
        source.effect(&FilesEffect::SelectAll).unwrap();
        other
            .effect(&FilesEffect::SelectEntries {
                entries: vec![crate::search_provider::entry_resource_id(
                    &root.path().join("b.txt"),
                )],
            })
            .unwrap();
        assert_eq!(
            source.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
            Some(2)
        );
        assert_eq!(
            other.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
            Some(1)
        );
        source.effect(&FilesEffect::ClearSelection).unwrap();
        assert_eq!(
            source.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
            Some(0)
        );
        assert_eq!(
            other.with_surface(|files: &mut FileExplorerWidget| files.selected_indices().len()),
            Some(1)
        );
        assert!(source.state().ops.is_empty());
        assert!(other.state().ops.is_empty());
    }

    /// Copying and pasting are queued and undoable like every filesystem
    /// change: undo moves a copy to Trash and a cut-and-paste back. A row
    /// offers rename, move and trash on its entry; undoing the trash puts
    /// the file back.
    #[test]
    fn copies_and_row_edits_are_undoable() {
        // Inside the home, so Trash is the home's and its entries are known.
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap();
        let folder = tempfile::tempdir_in(&home).unwrap();
        std::fs::create_dir(folder.path().join("Projects")).unwrap();
        std::fs::write(folder.path().join("notes.txt"), b"n").unwrap();
        std::fs::write(folder.path().join("plan.txt"), b"p").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        let row = |name: &str| crate::search_provider::entry_resource_id(&folder.path().join(name));
        let projects = folder.path().join("Projects").display().to_string();

        app.effect_on(
            &row("notes.txt"),
            &FilesEffect::Copy {
                entry: row("notes.txt"),
            },
        )
        .unwrap();
        app.effect(&FilesEffect::GoTo {
            path: projects.clone(),
        })
        .unwrap();
        app.effect(&FilesEffect::Paste).unwrap();
        app.settle();
        assert!(folder.path().join("Projects/notes.txt").exists());
        assert!(app.undo());
        app.settle();
        assert!(
            !folder.path().join("Projects/notes.txt").exists(),
            "the copy goes"
        );
        assert!(
            folder.path().join("notes.txt").exists(),
            "the original stays"
        );

        app.effect(&FilesEffect::GoTo {
            path: folder.path().display().to_string(),
        })
        .unwrap();
        app.effect_on(
            &row("plan.txt"),
            &FilesEffect::Cut {
                entry: row("plan.txt"),
            },
        )
        .unwrap();
        app.effect(&FilesEffect::GoTo {
            path: projects.clone(),
        })
        .unwrap();
        app.effect(&FilesEffect::Paste).unwrap();
        app.settle();
        assert!(folder.path().join("Projects/plan.txt").exists());
        assert!(app.undo());
        app.settle();
        assert!(
            folder.path().join("plan.txt").exists(),
            "cut and paste moves back"
        );

        app.effect(&FilesEffect::GoTo {
            path: folder.path().display().to_string(),
        })
        .unwrap();
        act_on(
            &mut app,
            &row("plan.txt"),
            "rename",
            &serde_json::json!({"name": "agenda.txt"}),
        );
        assert!(folder.path().join("agenda.txt").exists());
        act_on(
            &mut app,
            &row("agenda.txt"),
            "move-to",
            &serde_json::json!({"to": projects}),
        );
        assert!(folder.path().join("Projects/agenda.txt").exists());
        act_on(&mut app, &row("notes.txt"), "trash", &serde_json::json!({}));
        assert!(!folder.path().join("notes.txt").exists(), "trashed");
        assert!(app.undo());
        app.settle();
        assert!(
            folder.path().join("notes.txt").exists(),
            "undo restores it from Trash"
        );
    }

    /// Invoke the resource's action `id` with `args` through the door, as
    /// an agent does, and let the device carry out what it queued.
    fn act(app: &mut TestApp<FileExplorerShell>, id: &str, args: &serde_json::Value) {
        let instance = app.harness().component_instance_id().unwrap();
        app.harness()
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: instance,
                    address: ui::tilo_actions::ActionAddress::new(
                        <FileExplorer as ui::tilo_resource::Resource>::KIND,
                        id,
                    ),
                },
                args.as_object().unwrap(),
            )
            .unwrap();
        app.settle();
    }

    /// Creating, renaming and moving files are document actions: queued in
    /// the Files document, carried out by this device's drain, settled with
    /// what they changed, and undone by queueing their revert.
    #[test]
    fn filesystem_changes_are_undoable_document_actions() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("draft.txt"), b"x").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        act(
            &mut app,
            "new-folder-here",
            &serde_json::json!({"name": "Projects"}),
        );
        assert!(folder.path().join("Projects").is_dir());
        act(
            &mut app,
            "rename-here",
            &serde_json::json!({"entry": "draft.txt", "name": "final.txt"}),
        );
        assert!(folder.path().join("final.txt").exists());
        let projects = folder.path().join("Projects").display().to_string();
        act(
            &mut app,
            "move-here",
            &serde_json::json!({"entries": ["final.txt"], "to": projects}),
        );
        assert!(folder.path().join("Projects/final.txt").exists());
        let ops = app.state().ops;
        assert_eq!(ops.len(), 3, "{ops:?}");
        assert!(
            ops.entries()
                .all(|(_, op)| op.settled.as_ref().is_some_and(Settled::is_done))
        );

        // Undo takes the move back on the filesystem, then the rename.
        assert!(app.undo());
        app.settle();
        assert!(folder.path().join("final.txt").exists(), "moved back");
        assert!(app.undo());
        app.settle();
        assert!(folder.path().join("draft.txt").exists(), "named back");
        assert!(app.redo());
        app.settle();
        assert!(folder.path().join("final.txt").exists(), "renamed again");

        let instance = app.harness().component_instance_id().unwrap();
        let refused = app.harness().invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: instance,
                address: ui::tilo_actions::ActionAddress::new(
                    <FileExplorer as ui::tilo_resource::Resource>::KIND,
                    "trash-here",
                ),
            },
            serde_json::json!({"entries": ["missing.txt"]})
                .as_object()
                .unwrap(),
        );
        assert!(refused.is_err(), "a misspelt entry changes nothing");
    }

    /// The window's other work is its effects: copying a row, pasting,
    /// showing a folder, pinning the folder shown.
    #[test]
    fn the_windows_work_is_its_effects() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir(folder.path().join("Projects")).unwrap();
        std::fs::write(folder.path().join("notes.txt"), b"n").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        // A row's effect, bound to it, as the palette offers it on the row.
        let row = crate::search_provider::entry_resource_id(&folder.path().join("notes.txt"));
        app.effect_on(&row, &FilesEffect::Copy { entry: row.clone() })
            .unwrap();
        app.effect(&FilesEffect::GoTo {
            path: folder.path().join("Projects").display().to_string(),
        })
        .unwrap();
        app.effect(&FilesEffect::Paste).unwrap();
        assert!(folder.path().join("Projects/notes.txt").exists());
        assert!(
            app.effect(&FilesEffect::EmptyTrash).is_err(),
            "Trash is emptied from the Trash folder only"
        );
        act(&mut app, "add-current-bookmark", &serde_json::json!({}));
        let place = folder.path().join("Projects").display().to_string();
        assert!(
            app.state().bookmarks().contains(&place),
            "{:?}",
            app.state()
        );
    }

    /// A change queued for another device waits for that device: only the
    /// device whose disk it names carries it out, and both see what it did.
    #[test]
    fn only_the_device_a_change_names_carries_it_out() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().display().to_string();
        let mut laptop = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let mut phone = TestApp::<FileExplorerShell>::on_device("phone").unwrap();
        sync(&mut laptop, &mut phone);
        laptop
            .invoke(FileExplorer::NEW_FOLDER.with(file_explorer::NewFolder {
                device: "phone".into(),
                parent,
                name: "Inbox".into(),
            }))
            .unwrap();
        laptop.settle();
        assert!(!root.path().join("Inbox").exists(), "the laptop waits");
        assert_eq!(laptop.state().ops.pending().count(), 1);

        sync(&mut laptop, &mut phone);
        assert!(
            root.path().join("Inbox").is_dir(),
            "the phone carried it out"
        );
        sync(&mut laptop, &mut phone);
        let state = laptop.state();
        let (id, _) = state.ops.entries().next().unwrap();
        assert_eq!(
            state.ops.outcome(id),
            Some(&Settled::Done(vec![Change::Created {
                path: root.path().join("Inbox").display().to_string()
            }])),
            "what it did reaches every replica"
        );
    }

    /// What the person did to Files reads as the app's sentences.
    #[test]
    fn history_entries_render_as_the_apps_sentences() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("draft.txt"), b"x").unwrap();
        let folder = root.path().display().to_string();
        let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let before = app.versions().len();
        let kind = <FileExplorer as ui::tilo_resource::Resource>::KIND;
        let agent = ui::tilo_actions::ActionInvoker::actor("agent:claude")
            .with_kind(ui::tilo_actions::ActorKind::Agent)
            .acting_for("ada");
        for (verb, args) in [
            ("add-bookmark", serde_json::json!({ "place": "~/Projects" })),
            (
                "new-folder",
                serde_json::json!({ "device": "laptop", "parent": folder, "name": "Inbox" }),
            ),
            (
                "rename",
                serde_json::json!({
                    "device": "laptop",
                    "folder": folder,
                    "entry": "draft.txt",
                    "name": "final.txt",
                }),
            ),
        ] {
            app.call_by(agent.clone(), kind, verb, args)
                .unwrap_or_else(|error| panic!("{verb}: {error:?}"));
        }
        app.settle();
        let english = include_str!("../locales/en-US.ftl");
        let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = app
            .versions()
            .into_iter()
            .skip(before)
            .filter_map(|version| version.context)
            .filter(|entry| entry.change != ui::tilo_actions::ChangeKind::External)
            // A test host names the app after the action type's stem
            // (`files`), not the app that hosts the record; read the entry
            // as the app's own host records it.
            .map(|mut entry| {
                if let Some(ui::tilo_actions::entry::Did::Verb(verb)) = &mut entry.what {
                    FileExplorerShell::ID.clone_into(&mut verb.action.app);
                }
                reader.sentence(&entry.with_app(FileExplorerShell::ID, FileExplorerShell::TITLE))
            })
            .collect();
        assert_eq!(
            sentences,
            [
                "Added ~/Projects to Favourites",
                "Created the folder Inbox",
                "Renamed draft.txt to final.txt",
            ]
        );
    }
}
