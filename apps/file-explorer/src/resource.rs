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
                "a file written here holds at most {} of text",
                ui::format::memory(MAX_WRITE_BYTES as u64)
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

    /// Compress
    ///
    /// Packs entries, folders with all they hold, into one archive in the
    /// folder that holds them, under a free name: zip by default, which
    /// opens everywhere, or tar compressed with zstd, which keeps
    /// permissions and is smaller. Undo moves the archive to Trash.
    #[action(label = "Compress", undo = Self::revert_queued)]
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
        #[param("`zip` (default) or `tar.zst`", example = "zip")] format: Option<String>,
    ) -> Result<(), Refusal> {
        let paths = entry_paths(&from, &entries)?;
        let name = match name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            Some(name) => file_name(name)?,
            None => match paths.as_slice() {
                [only] => std::path::Path::new(only).file_name().map_or_else(
                    || "Archive".into(),
                    |name| crate::archive::stem(std::path::Path::new(name)),
                ),
                _ => "Archive".into(),
            },
        };
        let packing = match format.as_deref().map(str::trim) {
            None | Some("") => crate::archive::packing_of(&name),
            Some(format) if format.eq_ignore_ascii_case("zip") => crate::archive::Packing::Zip,
            Some(format)
                if format.eq_ignore_ascii_case("tar.zst")
                    || format.eq_ignore_ascii_case("tzst") =>
            {
                crate::archive::Packing::TarZst
            }
            Some(_) => return Err(Refusal::new("the format is zip or tar.zst")),
        };
        let extension = format!(".{}", packing.extension());
        let name = if name.to_ascii_lowercase().ends_with(&extension) {
            name
        } else {
            format!("{name}{extension}")
        };
        let op = FileOp::Compress {
            paths,
            folder: absolute_folder(&from)?,
            name,
        };
        queue(cx, ops, device, op)
    }

    /// Extract an archive
    ///
    /// Extracts a zip, 7z or tar archive (plain, .gz, .xz, .zst or .bz2)
    /// into a folder: as the one folder the archive holds at its top, or
    /// else into a new folder named after the archive. Nothing lands
    /// outside that folder; links pointing out, device files and setuid
    /// bits are refused. Undo moves the extracted folder to Trash.
    #[action(id = "extract", label = "Extract", undo = Self::revert_queued)]
    pub fn extract(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileTask, Vec<Change>>,
        #[param("The device whose files change")] device: String,
        #[param("Absolute path of the archive")] archive: String,
        #[param("Absolute path of the folder to extract into; default the archive's folder")]
        to: Option<String>,
        #[param(
            "When that name is taken: `keep-both` (default, a free name), `replace` (the existing item goes to Trash) or `skip`",
            example = "keep-both"
        )]
        if_exists: Option<String>,
    ) -> Result<(), Refusal> {
        use crate::file_ops::IfExists;
        let if_exists = match if_exists.as_deref().map(str::trim) {
            None | Some("" | "keep-both") => IfExists::KeepBoth,
            Some("replace") => IfExists::Replace,
            Some("skip") => IfExists::Skip,
            Some(_) => return Err(Refusal::new("if_exists is keep-both, replace or skip")),
        };
        let path = std::path::Path::new(&archive);
        if !path.is_absolute() {
            return Err(Refusal::new("the archive is an absolute path"));
        }
        if !crate::archive::is_supported(path) {
            return Err(Refusal::new(
                "the archive is a zip, 7z or tar file (plain, .gz, .xz, .zst or .bz2)",
            ));
        }
        let folder = match to.as_deref().map(str::trim).filter(|to| !to.is_empty()) {
            Some(to) => absolute_folder(to)?,
            None => path
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .ok_or_else(|| Refusal::new("the archive has no folder"))?,
        };
        let op = FileOp::Extract {
            archive,
            folder,
            if_exists,
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
            op @ (FileOp::Compress { .. } | FileOp::Extract { .. }) => {
                return Self::archive_queueing(device, op);
            }
            FileOp::Revert { op } => Self::REVERT.with(Id::from_raw(op)),
            FileOp::ClipboardReceipt { .. } => return None,
        })
    }

    /// The verb that queues an archive operation for `device`.
    fn archive_queueing(device: String, op: FileOp) -> Option<Invoke<Self>> {
        Some(match op {
            FileOp::Compress {
                paths,
                folder,
                name,
            } => Self::COMPRESS.with(file_explorer::Compress {
                device,
                from: folder,
                entries: paths,
                format: Some(crate::archive::packing_of(&name).extension().into()),
                name: Some(name),
            }),
            FileOp::Extract {
                archive,
                folder,
                if_exists,
            } => Self::EXTRACT.with(file_explorer::Extract {
                device,
                archive,
                to: Some(folder),
                if_exists: Some(
                    match if_exists {
                        crate::file_ops::IfExists::KeepBoth => "keep-both",
                        crate::file_ops::IfExists::Replace => "replace",
                        crate::file_ops::IfExists::Skip => "skip",
                    }
                    .into(),
                ),
            }),
            _ => return None,
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
    /// The width of each column the person set by dragging its edge, by
    /// column key, in whole pixels. A column never resized keeps the width
    /// the list gives it.
    pub widths: std::collections::BTreeMap<String, u32>,
}

impl Default for FilesPreferences {
    fn default() -> Self {
        Self {
            view: "list".into(),
            sort: "name".into(),
            order: "ascending".into(),
            zoom: 0,
            hidden: false,
            widths: std::collections::BTreeMap::new(),
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
    const READS_HOST: &'static [ui::launch::HostSource] = &[ui::launch::HostSource::Files];
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

/// Longest an agent's change waits to be attributed before writing anyway:
/// the host answers at once, so this only bounds a host that went away.
const WRITE_ATTRIBUTION_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether `op` runs on a worker with progress and Cancel: a copy, a move
/// into an existing folder, an extraction or a packing.
fn runs_tracked(op: &FileOp) -> bool {
    matches!(
        op,
        FileOp::Copy { .. }
            | FileOp::Move { create: false, .. }
            | FileOp::Extract { .. }
            | FileOp::Compress { .. }
    )
}

/// A copy, move, extraction or packing on a worker: its progress reaches
/// the caller's job and the window, and the caller's cancel stops it.
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
        let phase = match transfer.kind {
            crate::file_ops::TransferKind::Copy => "copying",
            crate::file_ops::TransferKind::Move => "moving",
            crate::file_ops::TransferKind::Extract => "extracting",
            crate::file_ops::TransferKind::Compress => "compressing",
        };
        progress.report(
            phase,
            Some(transfer.done.load(Ordering::Relaxed)),
            Some(transfer.total.load(Ordering::Relaxed)),
        );
    };
    Effected::settled(&crate::file_ops::execute_tracked(
        &task.op, reverting, &report,
    ))
}

/// Carry out `task` on a worker once the host answered its attribution:
/// whatever it answered, the writes go ahead.
fn carry_out_attributed(
    (task, reverting, attribution): (
        FileTask,
        Option<Vec<Change>>,
        Option<ui::invocation_scope::WriteAttributionReceipt>,
    ),
    progress: &ui::framework::Progress,
) -> Effected {
    if let Some(attribution) = attribution
        && let Err(error) = attribution.wait(WRITE_ATTRIBUTION_WAIT)
    {
        tracing::debug!(%error, "these writes stay the person's");
    }
    if runs_tracked(&task.op) {
        carry_out_tracked(&task, reverting.as_deref(), progress)
    } else {
        carry_out(&task, reverting.as_deref())
    }
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
                        // An agent's change names the paths it writes before
                        // writing them, so their versions record the agent,
                        // for its person; the host takes who acted from its
                        // own custody of the invocation.
                        let attribution =
                            cx.attribute_writes(task.op.written_paths(reverting.as_deref()));
                        if runs_tracked(&task.op) || attribution.is_some() {
                            // Copies and moves can take minutes: off the UI
                            // thread, with progress and Cancel. So is a
                            // change waiting for its attribution.
                            let work = (task, reverting, attribution);
                            return Ok(cx.thread(
                                move |progress| carry_out_attributed(work, &progress),
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
        if host_person_only(&effect) && !ui::bound_dispatch::host_person_invocation_permitted() {
            return Err("Only the host's person may access their private cloud accounts".into());
        }
        // A handed-over file answers with its content, which the next step
        // takes by address.
        if let FilesEffect::HandOver { file } = &effect {
            return Effected::content(&self.hand_over(file)?);
        }
        let recovery =
            matches!(effect, FilesEffect::RecoverClipboardTransfers) && self.hosted_clipboard();
        let receipt = matches!(
            effect,
            FilesEffect::Paste { .. } | FilesEffect::Restore { .. }
        ) && self.hosted_clipboard();
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
        self.published_live_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.published_row(id).map(LiveRow::from_resource)
    }

    const ROW_DECLARATIONS: bool = true;

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

/// Whether `effect` is the local person's alone: their cloud accounts,
/// transfers, drives and apps, not every collaborator's who may edit the
/// workspace's files.
fn host_person_only(effect: &FilesEffect) -> bool {
    matches!(
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
            | FilesEffect::UninstallApp { .. }
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::FileExplorerShell::scenarios() {
            let mut app = TestApp::<super::FileExplorerShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
