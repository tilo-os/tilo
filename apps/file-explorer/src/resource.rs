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

/// The most text one written file holds. The text is kept in the document
/// with the change until it is pruned, so it is bounded well below a
/// document's size.
pub(crate) const MAX_WRITE_BYTES: usize = 256 * 1024;

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

/// Durable destination evidence for history, not permission to read a source.
/// Fresh admission checks it against the receiving host's actual directory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportDestination {
    pub path: String,
    pub identity: tilo_cloud_files::DestinationIdentity,
}

impl tilo_resource::JsonSchema for ImportDestination {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object", "required": ["path", "identity"],
            "properties": {
                "path": {"type": "string"},
                "identity": {
                    "type": "object", "required": ["device", "inode"],
                    "properties": {
                        "device": {"type": "integer", "minimum": 0},
                        "inode": {"type": "integer", "minimum": 0},
                        "modified": <Option<(i64, i64, u64)> as tilo_resource::JsonSchema>::json_schema()
                    }
                }
            }
        })
    }
}

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

/// Reject sync mounts even when this action bypasses the Files view. The
/// receiving device must repeat this check when it executes the queued task.
#[cfg(test)]
fn independent_destination<'a>(
    folder: &str,
    mounts: impl Iterator<Item = &'a std::path::Path>,
) -> Result<(), Refusal> {
    crate::cloud_transfer::pin_destination(folder, mounts)
        .map(|_| ())
        .map_err(Refusal::new)
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
    /// Pin a folder to the Favorites in the sidebar.
    #[action(label = "Add to Favorites")]
    pub fn add_bookmark(
        &mut self,
        #[param("Folder path on this device, or a stored place such as xdg:documents")]
        place: String,
        #[param("Position among the Favorites, from 0; the end when none")] at: Option<usize>,
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

    /// Unpin a folder from the Favorites.
    #[action(label = "Remove from Favorites")]
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

    /// Copy mounted representations into independent editable files.
    #[action(undo = Self::revert_queued)]
    pub fn import_mounted(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device whose files change")] device: String,
        #[param("The source files' absolute paths")] paths: Vec<String>,
        #[param("Absolute path of the destination folder")] folder: String,
    ) -> Result<(), Refusal> {
        if !cx.is_preview() && !private_source_invocation_permitted() {
            return Err(Refusal::new(
                "Only the receiving host's person may import their private mounted files",
            ));
        }
        if !cx.is_preview()
            && ui::bound_dispatch::executing_host_device().as_deref() != Some(device.as_str())
        {
            return Err(Refusal::new(
                "Invoke Files on the receiving device to import mounted files",
            ));
        }
        let op = FileOp::ImportMounted {
            paths: entry_paths("", &paths)?,
            folder: absolute_folder(&folder)?,
        };
        let entry = cx.id();
        if !cx.is_preview() {
            crate::cloud_jobs::approve_mounted(&device, &op, entry.as_str())
                .map_err(Refusal::new)?;
        }
        ops.push(entry, FileTask { device, op });
        ops.prune(KEPT_OPS);
        Ok(())
    }

    /// Import independent local copies from a cloud account; undo removes
    /// only the local outputs recorded by the transfer.
    #[action(undo = Self::revert_queued)]
    pub fn cloud_import(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device receiving the local copies")] device: String,
        #[param("A unique ID for this transfer attempt")] job_id: String,
        #[param("The connected cloud account")] account: String,
        #[param("Remote entries to import as independent local copies")] entries: Vec<
            serde_json::Value,
        >,
        #[param("Pinned destination evidence; not source authority")]
        destination: ImportDestination,
    ) -> Result<(), Refusal> {
        if !cx.is_preview() && !private_source_invocation_permitted() {
            return Err(Refusal::new(
                "Only the receiving host's person may import from their private cloud accounts",
            ));
        }
        // Destination paths and their pinned identity belong to the executing
        // host. A caller naming another device cannot choose that host's folder.
        if !cx.is_preview()
            && ui::bound_dispatch::executing_host_device().as_deref() != Some(device.as_str())
        {
            return Err(Refusal::new(
                "Invoke Files on the receiving device and choose the destination there; cloud import cannot target another device from this host",
            ));
        }
        if job_id.trim().is_empty() {
            return Err(Refusal::new("a cloud transfer job ID is named"));
        }
        if account.trim().is_empty() {
            return Err(Refusal::new("a cloud account is named"));
        }
        let entries: Vec<tilo_cloud_files::cloud::ImportEntry> =
            serde_json::from_value(serde_json::Value::Array(entries))
                .map_err(|error| Refusal::new(format!("invalid remote entries: {error}")))?;
        if entries.is_empty() || entries.len() > MAX_MOVE_ENTRIES {
            return Err(Refusal::new(format!(
                "name 1 to {MAX_MOVE_ENTRIES} entries"
            )));
        }
        let folder = absolute_folder(&destination.path)?;
        if !cx.is_preview() {
            validate_import_destination(&destination)?;
        }
        let destination = destination.identity;
        let mut ids = std::collections::HashSet::new();
        for entry in &entries {
            if entry.id.trim().is_empty() || !ids.insert(&entry.id) {
                return Err(Refusal::new("remote entries have distinct, nonempty IDs"));
            }
            crate::cloud_transfer::valid_name(&entry.name).map_err(Refusal::new)?;
            if entry.kind == tilo_cloud_files::cloud::ImportKind::Unsupported {
                return Err(Refusal::new("the remote entry cannot be imported"));
            }
        }
        let op = FileOp::CloudImport {
            job_id,
            account,
            entries,
            folder,
            destination,
        };
        let entry = cx.id();
        if !cx.is_preview() {
            crate::cloud_jobs::approve(&device, &op, entry.as_str()).map_err(Refusal::new)?;
        }
        ops.push(entry, FileTask { device, op });
        ops.prune(KEPT_OPS);
        Ok(())
    }

    /// Write a file
    ///
    /// Writes text as a new text file in a folder (Markdown, plain text,
    /// CSV, HTML, code), as given. A PDF or a document (`.docx`, `.odt`,
    /// `.rtf`) is Writer's: write Markdown here and convert it with Writer's
    /// `convert-file`. With `replace`, a file of that name goes to Trash
    /// first; undo puts it back and takes the new one away.
    #[action(label = "Write file", undo = Self::revert_queued)]
    pub fn write_file(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device whose files change")] device: String,
        #[param("Absolute path of the folder to write it in")] folder: String,
        #[param(
            "The file's name with its extension; a text format, not a PDF or document",
            example = "Summary.md"
        )]
        name: String,
        #[param("What it says, written as given")] text: String,
        #[param("Put an existing file of that name in Trash and write over it; default no")]
        replace: Option<bool>,
    ) -> Result<(), Refusal> {
        if text.len() > MAX_WRITE_BYTES {
            return Err(Refusal::new(format!(
                "a file written here holds at most {} KiB of text",
                MAX_WRITE_BYTES / 1024
            )));
        }
        let name = file_name(&name)?;
        crate::documents::writable(&name).map_err(Refusal::new)?;
        let op = FileOp::Write {
            folder: absolute_folder(&folder)?,
            name,
            text,
            replace: replace.unwrap_or(false),
        };
        queue(cx, ops, device, op)
    }

    /// Compress to zip
    ///
    /// Packs entries, folders with all they hold, into one zip archive in
    /// the folder that holds them, under a free name. Undo moves the archive
    /// to Trash.
    #[action(label = "Compress to zip", undo = Self::revert_queued)]
    pub fn compress(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device whose files change")] device: String,
        #[param("Absolute path of the folder holding the entries; the archive goes here")]
        from: String,
        #[param("The entries: names in that folder, or absolute paths")] entries: Vec<String>,
        #[param("The archive's name; the single entry's name, or Archive, when none")] name: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let paths = entry_paths(&from, &entries)?;
        let name = match name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            Some(name) => file_name(name)?,
            None => match paths.as_slice() {
                [only] => std::path::Path::new(only).file_stem().map_or_else(
                    || "Archive".into(),
                    |stem| stem.to_string_lossy().into_owned(),
                ),
                _ => "Archive".into(),
            },
        };
        let name = if std::path::Path::new(&name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        {
            name
        } else {
            format!("{name}.zip")
        };
        let op = FileOp::Compress {
            paths,
            folder: absolute_folder(&from)?,
            name,
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
            FileOp::ImportMounted { paths, folder } => {
                Self::IMPORT_MOUNTED.with(file_explorer::ImportMounted {
                    device,
                    paths,
                    folder,
                })
            }
            FileOp::Copy { paths, folder } => Self::COPY_INTO.with(file_explorer::CopyInto {
                device,
                from: String::new(),
                entries: paths,
                to: folder,
            }),
            FileOp::CloudImport {
                job_id,
                account,
                entries,
                folder,
                destination,
            } => Self::CLOUD_IMPORT.with(file_explorer::CloudImport {
                device,
                job_id,
                account,
                entries: entries
                    .iter()
                    .map(|entry| serde_json::json!(entry))
                    .collect(),
                destination: ImportDestination {
                    path: folder,
                    identity: destination,
                },
            }),
            FileOp::Write {
                folder,
                name,
                text,
                replace,
            } => Self::WRITE_FILE.with(file_explorer::WriteFile {
                device,
                folder,
                name,
                text,
                replace: Some(replace),
            }),
            FileOp::Compress {
                paths,
                folder,
                name,
            } => Self::COMPRESS.with(file_explorer::Compress {
                device,
                from: folder,
                entries: paths,
                name: Some(name),
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
fn reverting_changes(state: &FileExplorer, task: &FileTask) -> Result<Option<Vec<Change>>, String> {
    Ok(match &task.op {
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
    })
}

/// Only fresh admission consults the immutable host identity. Historical
/// structural replay never performs private-source admission or mints consent.
fn private_source_invocation_permitted() -> bool {
    ui::bound_dispatch::host_person_invocation_permitted()
}

fn validate_import_destination(destination: &ImportDestination) -> Result<(), Refusal> {
    let mounts = accounts::config_root()
        .map(|root| crate::remote_accounts::load_in(&root, crate::model::user_home().as_deref()))
        .unwrap_or_default();
    let actual = crate::cloud_transfer::pin_destination(
        &destination.path,
        mounts.iter().filter_map(|account| account.mount.as_deref()),
    )
    .map_err(Refusal::new)?;
    if actual != destination.identity {
        return Err(Refusal::new(
            "the import destination changed since it was chosen",
        ));
    }
    Ok(())
}

/// A copy or move on a worker: its progress reaches the caller's job and
/// the window, and the caller's cancel stops it.
fn carry_out_tracked(
    task: &FileTask,
    reverting: Option<&[Change]>,
    progress: &ui::framework::Progress,
) -> Effected {
    let report = |transfer: &crate::file_ops::LocalTransfer| {
        use std::sync::atomic::Ordering;
        if progress.is_cancelled() {
            transfer.cancel.store(true, Ordering::Relaxed);
        }
        progress.report(
            "copying",
            Some(transfer.done.load(Ordering::Relaxed)),
            Some(transfer.total.load(Ordering::Relaxed)),
        );
    };
    Effected::settled(&crate::file_ops::execute_tracked(
        &task.op, reverting, &report,
    ))
}

fn carry_out(task: &FileTask, reverting: Option<&[Change]>) -> Effected {
    let outcome = crate::file_ops::execute(&task.op, reverting);
    Effected::settled(&outcome)
}

enum SourceConsent {
    Cloud(tilo_cloud_files::ImportConsent),
    Mounted(crate::cloud_jobs::MountedConsent),
}

fn consume_source(task: &FileTask, entry: &Id) -> Result<SourceConsent, String> {
    match task.op {
        FileOp::ImportMounted { .. } => {
            crate::cloud_jobs::consume_mounted(&task.device, &task.op, entry.as_str())
                .map(SourceConsent::Mounted)
        }
        _ => crate::cloud_jobs::consume(&task.device, &task.op, entry.as_str())
            .map(SourceConsent::Cloud),
    }
}

fn carry_out_source(
    task: &FileTask,
    consent: SourceConsent,
    progress: ui::framework::Progress,
) -> Effected {
    match consent {
        SourceConsent::Cloud(consent) => carry_out_cloud(task, consent, progress),
        SourceConsent::Mounted(consent) => Effected::settled(&consent.execute()),
    }
}

/// The cloud runner blocks until its worker finishes. Keep both that wait
/// and progress/cancellation forwarding off the UI thread.
fn carry_out_cloud(
    task: &FileTask,
    consent: tilo_cloud_files::ImportConsent,
    progress: ui::framework::Progress,
) -> Effected {
    let FileOp::CloudImport { job_id, .. } = &task.op else {
        return carry_out(task, None);
    };
    crate::cloud_jobs::register(job_id);
    if progress.is_cancelled() {
        crate::cloud_jobs::cancel(job_id);
    }
    std::thread::scope(|scope| {
        let (done, finished) = std::sync::mpsc::channel::<()>();
        scope.spawn(move || {
            let mut previous = None;
            loop {
                if progress.is_cancelled() {
                    crate::cloud_jobs::cancel(job_id);
                }
                let snapshot = crate::cloud_jobs::snapshot(job_id);
                if previous.as_ref() != Some(&snapshot) {
                    progress.report(snapshot.phase.clone(), Some(snapshot.bytes), None);
                    previous = Some(snapshot);
                }
                match finished.recv_timeout(std::time::Duration::from_millis(80)) {
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
        });
        let answer = Effected::settled(&crate::cloud_jobs::run(&task.op, consent));
        drop(done);
        answer
    })
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
                move |entry, task, cx| {
                    let Some((operation, redo)) =
                        cx.live().with(|state| native_compensation(state, &task))
                    else {
                        let reverting = cx.live().with(|state| reverting_changes(state, &task))?;
                        if matches!(
                            task.op,
                            FileOp::CloudImport { .. } | FileOp::ImportMounted { .. }
                        ) {
                            let consent = match consume_source(&task, entry) {
                                Ok(consent) => consent,
                                Err(reason) => {
                                    return Ok(Effected::settled(&ui::tilo_resource::Settled::<
                                        Vec<Change>,
                                    >::Failed {
                                        reason,
                                        partial: vec![],
                                    }));
                                }
                            };
                            return Ok(cx.thread(
                                move |progress| carry_out_source(&task, consent, progress),
                                |(), answer, _| Ok(answer),
                            ));
                        }
                        if matches!(
                            task.op,
                            FileOp::Copy { .. } | FileOp::Move { create: false, .. }
                        ) {
                            // Copies and moves can take minutes: off the UI
                            // thread, with progress and Cancel.
                            return Ok(cx.thread(
                                move |progress| {
                                    carry_out_tracked(&task, reverting.as_deref(), &progress)
                                },
                                |(), answer, _| Ok(answer),
                            ));
                        }
                        return Ok(carry_out(&task, reverting.as_deref()));
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
        // Cloud accounts and transfer state belong to the local person, not
        // to every collaborator allowed to edit the workspace's files.
        if matches!(
            effect,
            FilesEffect::BrowseCloud { .. }
                | FilesEffect::MoreCloud
                | FilesEffect::SelectCloud { .. }
                | FilesEffect::ImportCloud { .. }
                | FilesEffect::CloudProgress
                | FilesEffect::CancelCloud
                | FilesEffect::RetryCloud
                | FilesEffect::RevealCloud
                | FilesEffect::ImportConverted { .. }
                | FilesEffect::ImportOnce { .. }
                | FilesEffect::OpenAccount { .. }
                | FilesEffect::AccountDetails { .. }
                | FilesEffect::Reauthenticate { .. }
                | FilesEffect::OpenDrive { .. }
        ) && !ui::bound_dispatch::host_person_invocation_permitted()
        {
            return Err("Only the host's person may access their private cloud accounts".into());
        }
        // A handed-over file answers with its content, which the next step
        // takes by address.
        if let FilesEffect::HandOver { file } = &effect {
            return Effected::content(&self.hand_over(file)?);
        }
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

    /// Pin the folder shown to the Favorites, or unpin it; change the
    /// files of the folder shown.
    fn bound_declarations(&self) -> Vec<ActionDecl<FileExplorer>> {
        let mut declarations = Vec::new();
        if let Some(place) = self.current_place() {
            declarations.extend(if self.current_is_favourite() {
                FileExplorer::REMOVE_BOOKMARK
                    .bind(file_explorer::RemoveBookmark::paths().place, place)
                    .declaration(
                        "remove-current-bookmark",
                        "Remove this folder from Favorites",
                    )
            } else {
                FileExplorer::ADD_BOOKMARK
                    .bind(file_explorer::AddBookmark::paths().place, place)
                    .declaration("add-current-bookmark", "Add this folder to Favorites")
            });
        }
        declarations.extend(self.file_op_declarations());
        declarations
    }

    /// The listing's entries, remote accounts, nearby transfers and a
    /// failed listing's retry.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.published_rows()
            .into_iter()
            .map(LiveRow::from_resource)
            .collect()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.published_row(id).map(LiveRow::from_resource)
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
            "cloud-import",
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
    /// the file back where the host can restore from Trash.
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
        let undone = app.undo();
        app.settle();
        if cfg!(target_os = "macos") {
            // macOS lets apps move items to the Trash but not enumerate or
            // restore them (`host_fs::trash::restore`), so the file stays there.
            assert!(
                !folder.path().join("notes.txt").exists(),
                "no Trash restore on macOS"
            );
        } else {
            assert!(undone);
            assert!(
                folder.path().join("notes.txt").exists(),
                "undo restores it from Trash"
            );
        }
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

    /// A rename, a move or a trash queued here reaches the disk, so grants
    /// treat it as an outside effect, yet its undo queues the revert: the
    /// contract says it can be taken back, bound to a folder or not, so a
    /// reviewer is not told it sends anything away.
    #[test]
    fn queued_file_changes_declare_that_they_can_be_undone() {
        use ui::tilo_actions::{ActionKind, COMPENSATED_UNDO_TAG};
        use ui::tilo_resource::Resource as _;
        let declarations = FileExplorer::declarations();
        let tagged = |id: &str| {
            declarations
                .iter()
                .find(|decl| decl.id == id)
                .unwrap()
                .tags
                .iter()
                .any(|tag| tag == COMPENSATED_UNDO_TAG)
        };
        for id in ["rename", "move", "trash", "new-folder", "copy-into"] {
            assert!(tagged(id), "{id} is undone by its compensation");
        }
        assert!(!tagged("add-bookmark"), "a document edit needs no tag");
        let bound = FileExplorer::RENAME
            .bind(file_explorer::Rename::paths().device, "device".to_owned())
            .and(file_explorer::Rename::paths().folder, "/tmp".to_owned())
            .declaration("rename-here", "Rename an entry here")
            .unwrap();
        assert_eq!(bound.kind, ActionKind::Outside);
        let spec = ui::tilo_resource::declaration_spec(&*bound);
        assert!(spec.contract.can_be_undone());
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

    /// A caller with no screen reads what the files say, writes a new file
    /// and compresses entries of the folder shown, through the same
    /// published door, and each change undoes like any other.
    #[test]
    fn files_are_read_written_and_compressed_through_actions() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("notes.md"), "# Notes\n\nBuy milk.").unwrap();
        let mut app =
            TestApp::<FileExplorerShell>::launched(&[folder.path().to_str().unwrap()]).unwrap();
        let read = app
            .effect(&FilesEffect::ReadText {
                file: "notes.md".into(),
                from: None,
            })
            .unwrap()
            .value
            .unwrap();
        assert_eq!(read["format"], "markdown");
        assert_eq!(read["text"], "# Notes\n\nBuy milk.");
        assert_eq!(read["next"], serde_json::Value::Null);
        let handed = app
            .effect(&FilesEffect::HandOver {
                file: "notes.md".into(),
            })
            .unwrap();
        let hash = ui::tilo_actions::content_hash(&handed.resources[0])
            .expect("a content address")
            .to_owned();
        assert_eq!(
            app.content().read(&hash).as_deref(),
            Some(b"# Notes\n\nBuy milk.".as_slice()),
            "the next step reads the file's bytes by their address"
        );
        app.effect(&FilesEffect::SelectAll).unwrap();
        let selected = app
            .effect(&FilesEffect::SelectedEntries)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(
            selected["paths"],
            serde_json::json!([folder.path().join("notes.md").display().to_string()])
        );

        act(
            &mut app,
            "write-file-here",
            &serde_json::json!({"name": "todo.txt", "text": "milk\neggs\n"}),
        );
        assert_eq!(
            std::fs::read_to_string(folder.path().join("todo.txt")).unwrap(),
            "milk\neggs\n"
        );
        act(
            &mut app,
            "compress-here",
            &serde_json::json!({"entries": ["notes.md", "todo.txt"]}),
        );
        assert!(folder.path().join("Archive.zip").exists());

        assert!(app.undo());
        app.settle();
        assert!(
            !folder.path().join("Archive.zip").exists(),
            "undo trashes it"
        );
        assert!(folder.path().join("notes.md").exists());
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

    fn import_destination(path: &std::path::Path) -> ImportDestination {
        let path = path.display().to_string();
        ImportDestination {
            identity: crate::cloud_transfer::pin_destination(&path, std::iter::empty()).unwrap(),
            path,
        }
    }

    // Only for requests whose path intentionally cannot be pinned. This is
    // serializable evidence, not permission to read a private cloud source.
    fn nonexistent_import_destination(path: &std::path::Path) -> ImportDestination {
        ImportDestination {
            path: path.display().to_string(),
            identity: serde_json::from_value(serde_json::json!({"device": 0, "inode": 0})).unwrap(),
        }
    }

    fn import_entry() -> tilo_cloud_files::cloud::ImportEntry {
        tilo_cloud_files::cloud::ImportEntry {
            id: "remote-document".into(),
            name: "document.txt".into(),
            kind: tilo_cloud_files::cloud::ImportKind::File,
            revision: Some("revision-1".into()),
            size: Some(12),
            content_writable: true,
        }
    }

    #[test]
    fn cloud_import_queues_on_its_device_and_undo_reverts_its_record() {
        // Keep this real service socket private to the test: other concurrently
        // running Files tests can also browse the default cloud endpoint.
        let _home = tilo_test_env::TestHome::new().unwrap();
        let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let root = tempfile::tempdir().unwrap();
        let socket = tilo_cloud_files::cloud::default_files_socket().unwrap();
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            use tilo_cloud_files::cloud::{
                CLOUD_FRAMING, CloudRequest, CloudResponse, ImportReadState, ImportReply,
                ImportRequest,
            };
            let (mut stream, _) = listener.accept().unwrap();
            while let Some(CloudRequest::Import { request, .. }) = CLOUD_FRAMING
                .read_value::<_, CloudRequest>(&mut stream)
                .unwrap()
            {
                let reply = match request {
                    ImportRequest::StartRead { .. } => ImportReply::Preparing {
                        session: "session".into(),
                    },
                    ImportRequest::ReadStatus { .. } => {
                        ImportReply::ReadState(ImportReadState::Ready { size: 6 })
                    }
                    ImportRequest::ReadChunk { .. } => ImportReply::Chunk {
                        bytes: b"import".to_vec(),
                        eof: true,
                    },
                    ImportRequest::CloseRead { .. } => ImportReply::Closed,
                    ImportRequest::Browse { .. } => panic!("unexpected request"),
                };
                CLOUD_FRAMING
                    .write_value(&mut stream, &CloudResponse::Import(reply))
                    .unwrap();
            }
            listener
        });
        let folder = root.path().display().to_string();
        let op = FileOp::CloudImport {
            job_id: uuid::Uuid::new_v4().to_string(),
            account: "connected-account".into(),
            entries: vec![import_entry()],
            destination: crate::cloud_transfer::pin_destination(&folder, std::iter::empty())
                .unwrap(),
            folder,
        };
        app.invoke(FileExplorer::queueing("laptop", op.clone()).unwrap())
            .unwrap();
        app.settle();
        let listener = server.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.state().ops.pending().next().is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "import did not settle"
            );
            app.settle();
            std::thread::yield_now();
        }
        let state = app.state();
        let (id, entry) = state.ops.entries().next().unwrap();
        assert_eq!(entry.value.device, "laptop");
        assert_eq!(entry.value.op, op);
        let outcome = entry.settled.as_ref().unwrap();
        assert!(outcome.is_done(), "{outcome:?}");
        assert!(matches!(
            outcome.result().as_slice(),
            [Change::CloudImported { .. }]
        ));
        assert_eq!(
            std::fs::read(root.path().join(import_entry().name)).unwrap(),
            b"import"
        );
        let id = id.as_str().to_owned();

        assert!(app.undo());
        app.settle();
        assert!(app.state().ops.entries().any(|(_, entry)| {
            entry.value.device == "laptop"
                && entry.value.op == FileOp::Revert { op: id.clone() }
                && entry.settled.as_ref().is_some_and(Settled::is_done)
        }));
        assert!(!root.path().join(import_entry().name).exists());

        assert_cloud_import_history_is_structural(&app, &id, &op, root.path());
        assert_forged_cloud_jobs_denied(&mut app, op, root.path(), &listener);
        std::fs::remove_file(socket).unwrap();
    }

    /// Exercise the recorded production verb, not a hand-built outbox edit.
    /// A historical import describes what happened even when its destination
    /// has moved and a different device/person is reading it.
    fn assert_cloud_import_history_is_structural(
        app: &TestApp<FileExplorerShell>,
        entry: &str,
        op: &FileOp,
        folder: &std::path::Path,
    ) {
        use ui::tilo_actions::{ActionEnvelope, ActionId, ActionRegistry, ExecutionLocus};
        use ui::tilo_resource::Resource as _;

        assert!(ui::bound_dispatch::executing_host_scope().is_none());
        let mut document = app.peer_document();
        let recorded = app
            .versions()
            .into_iter()
            .find(|version| {
                version.context.as_ref().is_some_and(|context| {
                    context.action.as_ref().is_some_and(|action| {
                        String::from_utf8_lossy(&action.payload).contains("cloud-import")
                    })
                })
            })
            .expect("the admitted import records its production action");
        let metadata = document
            .get_change_meta_by_hash(&recorded.hash)
            .unwrap()
            .into_owned();
        let context = recorded.context.unwrap();
        let encoded = context.action.clone().expect("recorded replay payload");
        let registry = ActionRegistry::with(ui::tilo_resource::group::register::<FileExplorer>);
        let envelope = ActionEnvelope {
            id: ActionId::new(),
            action: encoded.clone(),
            base_heads: metadata.deps.iter().map(|head| head.0).collect(),
            timestamp: metadata.timestamp,
            locus: ExecutionLocus::ClientLocal,
            actor: Some(metadata.actor.to_bytes().to_vec()),
            context: Some(context),
        };
        let displaced = folder.with_extension("historical-destination");
        std::fs::rename(folder, &displaced).unwrap();
        assert!(
            !folder.exists(),
            "history must not need the old destination"
        );

        let mut base = document.fork_at(&metadata.deps).unwrap();
        base.set_actor(ui::automerge::ActorId::from(
            b"other-device-reader".to_vec(),
        ));
        let action = registry.decode(&encoded).unwrap();
        assert!(
            ui::tilo_actions::executor::inverse_as(
                &mut base,
                action.as_ref(),
                metadata.actor.clone().into_owned(),
            )
            .is_some(),
            "undo preview must evaluate the import without host authority or filesystem IO"
        );
        assert!(FileExplorer::read(&base).unwrap().ops.is_empty());
        let mut peer = Replica::<FileExplorer>::from_doc(base);
        peer.replay(&envelope).unwrap();
        assert_eq!(peer.doc_mut().get_heads(), vec![recorded.hash]);
        let state = peer.state().unwrap();
        let (replayed_id, task) = state.ops.entries().next().unwrap();
        assert_eq!(replayed_id.as_str(), entry);
        assert_eq!(task.value.device, "laptop");
        assert_eq!(
            &task.value.op, op,
            "replay preserves the exact queued task data"
        );
        assert_eq!(
            ui::tilo_actions::executor::verify_change(&mut document, recorded.hash, &registry)
                .unwrap(),
            ui::tilo_actions::executor::Verified::Reproduced,
        );
        let preview = app.past_version(&[recorded.hash]).unwrap();
        let shown = preview.state();
        let (_, historical) = shown.ops.entries().next().unwrap();
        assert_eq!(historical.value, task.value);
        assert!(historical.settled.is_none());
        assert!(ui::bound_dispatch::executing_host_scope().is_none());
        std::fs::rename(displaced, folder).unwrap();
        // The caller now reopens the already consumed exact entry and checks
        // that neither replay nor either preview reminted its local approval.
    }

    fn assert_forged_cloud_jobs_denied(
        app: &mut TestApp<FileExplorerShell>,
        op: FileOp,
        root: &std::path::Path,
        listener: &std::os::unix::net::UnixListener,
    ) {
        // A collaborator can write perfectly valid outbox data without going
        // through cloud-import. Neither replaying the exact approved job under
        // another entry nor changing that job may spend the host's consent.
        let mut changed_account = op.clone();
        let FileOp::CloudImport { account, .. } = &mut changed_account else {
            panic!("the admitted job must be a cloud import")
        };
        *account = "another-private-account".into();
        let mut changed_entries = op.clone();
        let FileOp::CloudImport { entries, .. } = &mut changed_entries else {
            panic!("the admitted job must be a cloud import")
        };
        entries[0].id = "another-private-document".into();
        let mut changed_job = op.clone();
        let FileOp::CloudImport { job_id, .. } = &mut changed_job else {
            panic!("the admitted job must be a cloud import")
        };
        *job_id = uuid::Uuid::new_v4().to_string();
        let admitted_id = app
            .state()
            .ops
            .entries()
            .find(|(_, entry)| entry.value.op == op)
            .map(|(id, _)| id.clone())
            .expect("the original admitted import remains recorded");
        // Re-opening the original consumed entry must not re-approve it either.
        for (attempt, forged) in [
            op.clone(),
            op,
            changed_account,
            changed_entries,
            changed_job,
        ]
        .into_iter()
        .enumerate()
        {
            let forged_id: ui::tilo_resource::Id = if attempt == 0 {
                admitted_id.clone()
            } else {
                uuid::Uuid::new_v4().to_string().into()
            };
            let mut ops = app.state().ops;
            ops.push(
                forged_id.clone(),
                FileTask {
                    device: "laptop".into(),
                    op: forged,
                },
            );
            app.merge_peer_edit(|doc| {
                ui::tilo_resource::model::write_key(doc, &ui::automerge::ROOT, "ops", &ops)
                    .unwrap();
            });
            app.settle();
            let state = app.state();
            let outcome = state.ops.outcome(&forged_id).expect("forged job settled");
            assert!(
                matches!(outcome, Settled::Failed { reason, partial }
                    if reason == "cloud import has no unused local approval" && partial.is_empty()),
                "unapproved outbox entry must fail without local changes: {outcome:?}"
            );
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "a forged entry must not even connect to the cloud source"
            );
            assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
            assert!(!root.join(import_entry().name).exists());
        }
    }

    #[test]
    fn mounted_import_requires_private_admission_and_copies_editable_outputs() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("Google.docx");
        let folder = root.path().join("Copies");
        std::fs::write(&source, b"native Google view").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::create_dir(&folder).unwrap();
        let op = FileOp::ImportMounted {
            paths: vec![source.display().to_string()],
            folder: folder.display().to_string(),
        };
        let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let forged_id: Id = "forged-mounted".into();
        let mut ops = app.state().ops;
        ops.push(
            forged_id.clone(),
            FileTask {
                device: "laptop".into(),
                op: op.clone(),
            },
        );
        app.merge_peer_edit(|doc| {
            ui::tilo_resource::model::write_key(doc, &ui::automerge::ROOT, "ops", &ops).unwrap();
        });
        app.settle();
        assert!(
            matches!(app.state().ops.outcome(&forged_id), Some(Settled::Failed { partial, .. }) if partial.is_empty())
        );
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
        assert!(
            matches!(crate::file_ops::execute(&op, None), Settled::Failed { partial, .. } if partial.is_empty())
        );
        app.invoke(FileExplorer::queueing("laptop", op).unwrap())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !folder.join("Google.docx").exists() || app.state().ops.pending().next().is_some() {
            assert!(std::time::Instant::now() < deadline);
            app.settle();
            std::thread::yield_now();
        }
        assert_eq!(
            std::fs::read(folder.join("Google.docx")).unwrap(),
            b"native Google view"
        );
        assert_ne!(
            std::fs::metadata(folder.join("Google.docx"))
                .unwrap()
                .permissions()
                .mode()
                & 0o200,
            0
        );
        assert_eq!(
            std::fs::metadata(source).unwrap().permissions().mode() & 0o222,
            0
        );
        assert!(app.undo());
        app.settle();
        assert!(!folder.join("Google.docx").exists());
    }

    #[test]
    fn mounted_import_record_verifies_offline_without_host_or_source() {
        use ui::tilo_actions::ActionRegistry;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.docx");
        let destination = root.path().join("Copies");
        std::fs::write(&source, b"private mounted representation").unwrap();
        std::fs::create_dir(&destination).unwrap();
        let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let op = FileOp::ImportMounted {
            paths: vec![source.display().to_string()],
            folder: destination.display().to_string(),
        };
        app.invoke(FileExplorer::queueing("laptop", op).unwrap())
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.state().ops.pending().next().is_some() {
            assert!(std::time::Instant::now() < deadline);
            app.settle();
            std::thread::yield_now();
        }
        assert!(
            app.state()
                .ops
                .entries()
                .next()
                .unwrap()
                .1
                .settled
                .as_ref()
                .unwrap()
                .is_done()
        );
        let recorded = app
            .versions()
            .into_iter()
            .find(|version| {
                version
                    .context
                    .as_ref()
                    .and_then(|context| context.action.as_ref())
                    .is_some_and(|action| {
                        String::from_utf8_lossy(&action.payload).contains("import-mounted")
                    })
            })
            .unwrap();
        root.close().unwrap();
        assert!(ui::bound_dispatch::executing_host_scope().is_none());
        let mut document = app.peer_document();
        document.set_actor(ui::automerge::ActorId::from(b"other-device".to_vec()));
        let registry = ActionRegistry::with(ui::tilo_resource::group::register::<FileExplorer>);
        assert_eq!(
            ui::tilo_actions::executor::verify_change(&mut document, recorded.hash, &registry)
                .unwrap(),
            ui::tilo_actions::executor::Verified::Reproduced,
        );
        let preview = app.past_version(&[recorded.hash]).unwrap();
        let state = preview.state();
        assert_eq!(state.ops.entries().count(), 1);
        assert!(state.ops.entries().next().unwrap().1.settled.is_none());
    }

    fn invoke_confirmed_by(
        app: &mut TestApp<FileExplorerShell>,
        role: ui::permissions::Role,
        invoker: ui::tilo_actions::ActionInvoker,
        invoke: &ui::tilo_resource::Invoke<FileExplorer>,
    ) -> Result<(), ui::tilo_actions::DoorError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(ui::tilo_actions::DoorError::InvalidArguments(
                refusal.to_owned(),
            ));
        }
        // Discover as the local host, then exercise admission under the
        // caller's role: discovery is not an authority grant.
        let address = app
            .actions()?
            .into_iter()
            .find(|action| action.address.action == invoke.id)
            .ok_or_else(|| {
                ui::tilo_actions::DoorError::Failed(format!(
                    "action {} is not published",
                    invoke.id
                ))
            })?
            .address;
        app.set_role(role);
        let instance_id = app.harness().component_instance_id().unwrap();
        app.harness().start_bound_job_for(
            Some(invoker),
            &ui::bound_dispatch::BoundActionAddress {
                instance_id,
                address,
            },
            &invoke.arguments,
            true,
        )?;
        Ok(())
    }

    #[test]
    fn mounted_import_refuses_guest_public_actions() {
        use ui::permissions::Role;
        use ui::tilo_actions::ActionInvoker;
        for role in [Role::Editor, Role::Viewer] {
            let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
            let result = invoke_confirmed_by(
                &mut app,
                role,
                ActionInvoker::actor("guest"),
                &FileExplorer::IMPORT_MOUNTED.with(file_explorer::ImportMounted {
                    device: "test-device".into(),
                    paths: vec!["/private/Google.docx".into()],
                    folder: "/private/Copies".into(),
                }),
            );
            assert!(result.is_err());
            app.settle();
            assert_eq!(app.state().ops.entries().count(), 0);
        }
    }

    #[test]
    fn cloud_import_refuses_another_device_before_pinning_the_destination() {
        let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let root = tempfile::tempdir().unwrap();
        // This path does not exist on the sender: a destination lookup would
        // fail differently, proving the host-device check precedes pinning.
        let folder = root.path().join("receiving-device-folder");
        let error = app
            .invoke(FileExplorer::CLOUD_IMPORT.with(file_explorer::CloudImport {
                job_id: "remote-import-attempt".into(),
                device: "phone".into(),
                account: "connected-account".into(),
                entries: vec![serde_json::json!(import_entry())],
                destination: nonexistent_import_destination(&folder),
            }))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Invoke Files on the receiving device and choose the destination there")
        );
        app.settle();
        assert_eq!(app.state().ops.entries().count(), 0);
        assert!(!folder.exists());
    }

    #[test]
    fn cloud_import_without_a_checked_host_binding_fails_closed() {
        let mut replica = Replica::<FileExplorer>::new();
        let error = replica
            .apply(FileExplorer::CLOUD_IMPORT.with(file_explorer::CloudImport {
                device: "laptop".into(),
                job_id: "unbound-import".into(),
                account: "private-account".into(),
                entries: vec![serde_json::json!(import_entry())],
                destination: nonexistent_import_destination(std::path::Path::new(
                    "not-an-absolute-destination",
                )),
            }))
            .unwrap_err();
        assert!(error.to_string().contains("host's person"), "{error}");
        assert_eq!(replica.state().unwrap().ops.entries().count(), 0);
    }

    #[test]
    fn cloud_guests_cannot_queue_imports_even_with_workspace_ownership() {
        use ui::permissions::Role;
        use ui::tilo_actions::ActionInvoker;

        for (actor, role) in [
            ("guest-owner", Role::Owner),
            ("guest-editor", Role::Editor),
            ("guest-viewer", Role::Viewer),
        ] {
            let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
            let root = tempfile::tempdir().unwrap();
            let folder = root.path().join("must-not-be-pinned");
            let error = invoke_confirmed_by(
                &mut app,
                role,
                ActionInvoker::actor(actor),
                &FileExplorer::CLOUD_IMPORT.with(file_explorer::CloudImport {
                    device: "test-device".into(),
                    job_id: format!("{actor}-attempt"),
                    account: "private-account".into(),
                    entries: vec![serde_json::json!(import_entry())],
                    destination: nonexistent_import_destination(&folder),
                }),
            )
            .unwrap_err();
            // Editors and owners pass workspace permission admission, but
            // neither owns this host's account. Viewers may be denied earlier.
            if role != Role::Viewer {
                assert!(
                    error.to_string().contains("host's person"),
                    "{actor}: {error}"
                );
            }
            app.settle();
            assert_eq!(app.state().ops.entries().count(), 0, "{actor}");
            assert!(!folder.exists(), "{actor}");
        }
    }

    #[test]
    fn a_valid_destination_witness_does_not_authorize_a_fresh_guest_import() {
        let root = tempfile::tempdir().unwrap();
        let destination = import_destination(root.path());
        let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
        app.set_role(ui::permissions::Role::Owner);
        let error = app
            .call_by(
                ui::tilo_actions::ActionInvoker::actor("guest-owner"),
                <FileExplorer as ui::tilo_resource::Resource>::KIND,
                "cloud-import",
                serde_json::json!({
                    "device": "test-device",
                    "job_id": uuid::Uuid::new_v4().to_string(),
                    "account": "private-account",
                    "entries": [import_entry()],
                    "destination": destination,
                }),
            )
            .unwrap_err();
        assert!(error.to_string().contains("host's person"), "{error}");
        app.settle();
        assert!(app.state().ops.is_empty());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn cloud_effects_refuse_guest_invocations_before_accessing_accounts() {
        use ui::permissions::Role;
        use ui::tilo_actions::ActionInvoker;

        for (actor, role) in [
            ("guest-owner", Role::Owner),
            ("guest-editor", Role::Editor),
            ("guest-viewer", Role::Viewer),
        ] {
            let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
            app.set_role(role);
            for effect in [
                FilesEffect::BrowseCloud {
                    account: "private-account".into(),
                    folder: None,
                },
                FilesEffect::MoreCloud,
                FilesEffect::SelectCloud {
                    entry: "private-entry".into(),
                },
                FilesEffect::ImportCloud { folder: None },
                FilesEffect::CloudProgress,
                FilesEffect::CancelCloud,
                FilesEffect::RetryCloud,
                FilesEffect::RevealCloud,
            ] {
                use ui::tilo_resource::Effect as _;
                let error = app
                    .call_by(
                        ActionInvoker::actor(actor),
                        <FileExplorer as ui::tilo_resource::Resource>::KIND,
                        effect.id(),
                        serde_json::Value::Object(effect.arguments()),
                    )
                    .unwrap_err();
                if matches!(effect, FilesEffect::CloudProgress) {
                    assert!(
                        error.to_string().contains("host's person"),
                        "{actor}: {effect:?}: {error}"
                    );
                } else {
                    // Private view admission rejects a guest before the
                    // cloud handler can inspect any account or transfer.
                    assert!(
                        matches!(&error, ui::tilo_actions::DoorError::Unavailable { code, .. }
                            if code == "private_view"),
                        "{actor}: {effect:?}: {error}"
                    );
                }
                assert_eq!(app.state().ops.entries().count(), 0, "{actor}");
            }
        }
    }

    #[test]
    fn cloud_attribution_and_mutable_authority_do_not_grant_personal_account_access() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};
        use ui::tilo_resource::Effect as _;

        for invoker in [
            ActionInvoker::actor("guest-owner").acting_for("local-person"),
            ActionInvoker::actor("guest-agent")
                .with_kind(ActorKind::Agent)
                .acting_for("local-person"),
        ] {
            let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
            // Mutable workspace authority may identify a different owner. It
            // must not replace the immutable person who owns this host.
            app.set_permissions(ui::permissions::PermissionContext::member(
                invoker.actor.clone(),
                ui::permissions::Role::Owner,
            ));
            let root = tempfile::tempdir().unwrap();
            let folder = root.path().join("must-not-be-pinned");
            let error = app
                .call_by(
                    invoker.clone(),
                    <FileExplorer as ui::tilo_resource::Resource>::KIND,
                    "cloud-import",
                    serde_json::json!({
                        "device": "test-device",
                        "job_id": uuid::Uuid::new_v4().to_string(),
                        "account": "private-account",
                        "entries": [import_entry()],
                        "destination": nonexistent_import_destination(&folder),
                    }),
                )
                .unwrap_err();
            assert!(error.to_string().contains("host's person"), "{error}");
            // A read effect reaches the account guard; a private View
            // effect would instead be refused earlier by the SDK.
            let effect = FilesEffect::CloudProgress;
            let error = app
                .call_by(
                    invoker,
                    <FileExplorer as ui::tilo_resource::Resource>::KIND,
                    effect.id(),
                    serde_json::Value::Object(effect.arguments()),
                )
                .unwrap_err();
            assert!(error.to_string().contains("host's person"), "{error}");
            app.settle();
            assert_eq!(app.state().ops.entries().count(), 0);
            assert!(!folder.exists());
        }
    }

    #[test]
    fn workspace_file_actions_remain_available_to_guests() {
        let mut app = TestApp::<FileExplorerShell>::for_person("local-person").unwrap();
        let folder = tempfile::tempdir().unwrap();
        invoke_confirmed_by(
            &mut app,
            ui::permissions::Role::Editor,
            ui::tilo_actions::ActionInvoker::actor("guest-editor"),
            &FileExplorer::NEW_FOLDER.with(file_explorer::NewFolder {
                device: "test-device".into(),
                parent: folder.path().to_string_lossy().into_owned(),
                name: "Shared folder".into(),
            }),
        )
        .unwrap();
        app.settle();
        assert!(folder.path().join("Shared folder").is_dir());
        assert_eq!(app.state().ops.entries().count(), 1);
    }

    #[test]
    fn cloud_import_destination_rejects_sync_mounts_and_aliases() {
        let root = tempfile::tempdir().unwrap();
        let mount = root.path().join("synced");
        std::fs::create_dir(&mount).unwrap();
        let child = mount.join("child");
        std::fs::create_dir(&child).unwrap();
        for destination in [&mount, &child] {
            assert!(
                independent_destination(
                    destination.to_str().unwrap(),
                    std::iter::once(mount.as_path()),
                )
                .is_err()
            );
        }
        #[cfg(unix)]
        {
            let alias = root.path().join("alias");
            std::os::unix::fs::symlink(&child, &alias).unwrap();
            assert!(
                independent_destination(alias.to_str().unwrap(), std::iter::once(mount.as_path()),)
                    .is_err()
            );
        }
        assert!(
            independent_destination(
                root.path().to_str().unwrap(),
                std::iter::once(mount.as_path()),
            )
            .is_ok()
        );
    }

    #[test]
    fn cloud_import_refuses_invalid_requests_before_queueing() {
        let mut app = TestApp::<FileExplorerShell>::on_device("laptop").unwrap();
        let root = tempfile::tempdir().unwrap();
        let valid = file_explorer::CloudImport {
            job_id: "import-attempt".into(),
            device: "laptop".into(),
            account: "connected-account".into(),
            entries: vec![serde_json::json!(import_entry())],
            destination: import_destination(root.path()),
        };
        let mut invalid = vec![
            file_explorer::CloudImport {
                job_id: String::new(),
                ..valid.clone()
            },
            file_explorer::CloudImport {
                entries: vec![serde_json::json!({"not": "entries"})],
                ..valid.clone()
            },
            file_explorer::CloudImport {
                device: String::new(),
                ..valid.clone()
            },
            file_explorer::CloudImport {
                account: String::new(),
                ..valid.clone()
            },
            file_explorer::CloudImport {
                entries: vec![],
                ..valid.clone()
            },
            file_explorer::CloudImport {
                destination: nonexistent_import_destination(std::path::Path::new("relative")),
                ..valid.clone()
            },
            file_explorer::CloudImport {
                destination: nonexistent_import_destination(root.path()),
                ..valid.clone()
            },
            file_explorer::CloudImport {
                entries: vec![serde_json::json!(import_entry()); MAX_MOVE_ENTRIES + 1],
                ..valid.clone()
            },
            file_explorer::CloudImport {
                entries: vec![
                    serde_json::json!(import_entry()),
                    serde_json::json!(import_entry()),
                ],
                ..valid.clone()
            },
        ];
        for (id, name, kind) in [
            ("", "file.txt", tilo_cloud_files::cloud::ImportKind::File),
            (
                "remote",
                "../escape",
                tilo_cloud_files::cloud::ImportKind::File,
            ),
            ("remote", "..", tilo_cloud_files::cloud::ImportKind::File),
            (
                "remote",
                "bad\0name",
                tilo_cloud_files::cloud::ImportKind::File,
            ),
            (
                "remote",
                "file",
                tilo_cloud_files::cloud::ImportKind::Unsupported,
            ),
        ] {
            let mut request = valid.clone();
            let mut entry = import_entry();
            entry.id = id.into();
            entry.name = name.into();
            entry.kind = kind;
            request.entries = vec![serde_json::json!(entry)];
            invalid.push(request);
        }
        for request in invalid {
            assert!(
                app.invoke(FileExplorer::CLOUD_IMPORT.with(request))
                    .is_err()
            );
            assert_eq!(app.state().ops.entries().count(), 0);
        }
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
                "Added ~/Projects to Favorites",
                "Created the folder Inbox",
                "Renamed draft.txt to final.txt",
            ]
        );
    }
}
