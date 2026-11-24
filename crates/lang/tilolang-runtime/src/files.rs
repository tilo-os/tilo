//! The device's own files, as `File.list`, `File.stat`, `File.read`, `File.watch` and `File.open`
//! reach them.
//!
//! An app changes state through actions, so these are the reads an action makes of the disk it
//! was granted (`uses files`): a folder's entries, one entry, the bytes a path holds, a watch
//! that runs one of the app's actions when a folder changes, and the open of a file in the app
//! that opens its kind. A view reads none of them: it is drawn again from what it is shown, where
//! an action records what it read.
//!
//! The runtime knows nothing of disks: [`FilesHost`] is the platform's, which holds the paths the
//! app was granted and refuses every other. Until a host is set, an app holds no files.

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

/// One entry of the device's folder, as `host.Entry` reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// What it is called, as the folder shows it.
    pub name: String,
    /// The path that reaches it, as the app names it.
    pub path: String,
    /// Whether it is a folder.
    pub folder: bool,
    /// Its size in bytes; 0 for a folder.
    pub size: i64,
    /// When its content last changed, in seconds since the epoch; 0 when the disk does not say.
    pub modified: i64,
    /// Whether its name starts with a dot.
    pub hidden: bool,
}

impl Entry {
    /// The entry as `host.Entry` holds it: its fields, in the order the type declares them.
    #[must_use]
    pub fn value(&self) -> Value {
        Value::Tuple(vec![
            Value::Text(self.name.clone()),
            Value::Text(self.path.clone()),
            Value::Bool(self.folder),
            Value::Int(self.size),
            Value::Int(self.modified),
            Value::Bool(self.hidden),
        ])
    }
}

/// One entry of this device's trash, as `File.trash_entries` answers it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrashEntry {
    /// What it is called in the trash.
    pub name: String,
    /// The path the trashed file has now.
    pub path: String,
    /// Where it stood when it was trashed.
    pub original: String,
    /// When it was trashed, in seconds since the epoch; 0 when the trash does not say.
    pub deleted: i64,
}

impl TrashEntry {
    /// The entry as `host.TrashEntry` holds it: its fields, in the order the type declares them.
    #[must_use]
    pub fn value(&self) -> Value {
        Value::Tuple(vec![
            Value::Text(self.name.clone()),
            Value::Text(self.path.clone()),
            Value::Text(self.original.clone()),
            Value::Int(self.deleted),
        ])
    }
}

/// An app that opens a file, as `tool.document_apps` lists it: the one it opens with today first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentApp {
    /// The app's id, which `tool.open_with` names.
    pub id: String,
    /// What the app is called, in the person's language.
    pub name: String,
    /// Whether this is the app the file opens with today.
    pub is_default: bool,
    /// The app's glyph, as its rows wear it; empty when it has none.
    pub icon: String,
}

impl DocumentApp {
    /// The app as `host.DocumentApp` holds it: its fields, in the order the type declares them.
    #[must_use]
    pub fn value(&self) -> Value {
        Value::Tuple(vec![
            Value::Text(self.id.clone()),
            Value::Text(self.name.clone()),
            Value::Bool(self.is_default),
            Value::Text(self.icon.clone()),
        ])
    }
}

/// The file `File.open` names: a path the app was granted, or a file an action holds, whose bytes
/// the workspace's content store keeps under its address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileTarget<'a> {
    /// A path the app was granted, as `File.list` and `File.read` name one.
    Path(&'a str),
    /// A file an action holds, by its name and the address of its bytes: the same `File` a chooser
    /// gives an action, which has no path of its own.
    Content { name: &'a str, hash: &'a str },
}

/// The device's files, as the platform running the app answers them: the paths the app was
/// granted, and nothing else.
pub trait FilesHost {
    /// The entries of the folder `path`, as the disk lists them.
    ///
    /// # Errors
    ///
    /// Why the folder cannot be read (it is not a folder, it is not granted, it is gone).
    fn list(&self, path: &str) -> Result<Vec<Entry>, String>;

    /// The entry at `path`, or none when nothing is there.
    ///
    /// # Errors
    ///
    /// Why the disk could not be asked (it is not granted, it is unreadable).
    fn stat(&self, path: &str) -> Result<Option<Entry>, String>;

    /// The bytes `path` holds.
    ///
    /// # Errors
    ///
    /// Why they cannot be read (it is not a file, it is not granted, it is gone).
    fn read(&self, path: &str) -> Result<Vec<u8>, String>;

    /// Keep `path` watched, running the action `run` when the folder changes, and answer the
    /// token that names the watch. `run` names one of the package's own actions taking only its
    /// target; the platform invokes it on the app's root, as the person the app runs for.
    ///
    /// # Errors
    ///
    /// Why the folder cannot be watched (it is not granted, it is not a folder).
    fn watch(&self, path: &str, run: &str) -> Result<String, String>;

    /// Stop the watch `token` names.
    fn unwatch(&self, token: &str) {
        let _ = token;
    }

    /// Make the folder `path`, answering the entry it made.
    ///
    /// # Errors
    ///
    /// Why it cannot be made (it exists, its parent is not granted, it is not a name).
    fn mkdir(&self, path: &str) -> Result<Entry, String> {
        let _ = path;
        Err(NO_FILES.to_owned())
    }

    /// Write `bytes` as the new file `path`, answering the entry it made.
    ///
    /// # Errors
    ///
    /// Why it cannot be written (it exists, its folder is not granted, it is a folder).
    fn write(&self, path: &str, bytes: &[u8]) -> Result<Entry, String> {
        let _ = (path, bytes);
        Err(NO_FILES.to_owned())
    }

    /// Rename the entry at `path` to `name` in its own folder, answering the new entry.
    ///
    /// # Errors
    ///
    /// Why it cannot be renamed (the name is taken, the entry is gone, it is not granted).
    fn rename(&self, path: &str, name: &str) -> Result<Entry, String> {
        let _ = (path, name);
        Err(NO_FILES.to_owned())
    }

    /// Move the entry at `path` into `folder`, answering the entry at its new path.
    ///
    /// # Errors
    ///
    /// Why it cannot be moved (the folder is not granted, a name is taken, the entry is gone).
    fn move_entry(&self, path: &str, folder: &str) -> Result<Entry, String> {
        let _ = (path, folder);
        Err(NO_FILES.to_owned())
    }

    /// Copy the entry at `path` into `folder`, answering the copy.
    ///
    /// # Errors
    ///
    /// Why it cannot be copied (the folder is not granted, a name is taken, the entry is gone).
    fn copy(&self, path: &str, folder: &str) -> Result<Entry, String> {
        let _ = (path, folder);
        Err(NO_FILES.to_owned())
    }

    /// Remove the entry at `path` for good, folder and all, answering whether anything was there.
    ///
    /// # Errors
    ///
    /// Why it cannot be removed (it is not granted, it is in use).
    fn remove(&self, path: &str) -> Result<bool, String> {
        let _ = path;
        Err(NO_FILES.to_owned())
    }

    /// Put the entry at `path` in this device's trash, answering where it went.
    ///
    /// # Errors
    ///
    /// Why it cannot be trashed (it is not granted, it is gone, the trash is unreachable).
    fn trash(&self, path: &str) -> Result<TrashEntry, String> {
        let _ = path;
        Err(NO_FILES.to_owned())
    }

    /// What this device's trash holds.
    ///
    /// # Errors
    ///
    /// Why it cannot be read.
    fn trash_entries(&self) -> Result<Vec<TrashEntry>, String> {
        Err(NO_FILES.to_owned())
    }

    /// Restore the entry named `name` in this device's trash to where it came from.
    ///
    /// # Errors
    ///
    /// Why it cannot be restored (it is gone, its place is taken).
    fn restore(&self, name: &str) -> Result<Entry, String> {
        let _ = name;
        Err(NO_FILES.to_owned())
    }

    /// Pack the entries at `paths`, each with its name and everything in it, into the new
    /// archive `to`, answering how many entries went.
    ///
    /// # Errors
    ///
    /// Why it cannot be packed (a path is not granted or gone, the name is taken, it is no
    /// archive's name).
    fn compress(&self, paths: &[String], to: &str) -> Result<i64, String> {
        let _ = (paths, to);
        Err(NO_FILES.to_owned())
    }

    /// Extract the archive `from` into the new folder `to`, answering how many entries came
    /// out.
    ///
    /// # Errors
    ///
    /// Why it cannot be extracted (it is not granted or gone, the folder is taken, the archive
    /// is no archive).
    fn extract(&self, from: &str, to: &str) -> Result<i64, String> {
        let _ = (from, to);
        Err(NO_FILES.to_owned())
    }

    /// The apps that open the file at `path`, the one it opens with today first: what
    /// `tool.document_apps` answers. The shell decides, by the file's first bytes and its name.
    ///
    /// # Errors
    ///
    /// Why the shell could not answer.
    fn document_apps(&self, path: &str) -> Result<Vec<DocumentApp>, String> {
        let _ = path;
        Err(NO_FILES.to_owned())
    }

    /// Open the file at `path` in `app`, an id [`Self::document_apps`] answered, or the app the
    /// workspace opens its kind with today: what `tool.open_with` does.
    ///
    /// # Errors
    ///
    /// Why nothing opened it.
    fn open_with(&self, path: &str, app: &str) -> Result<(), String> {
        let _ = (path, app);
        Err(NO_FILES.to_owned())
    }

    /// Open a terminal in the folder `folder`: what `tool.terminal` does.
    ///
    /// # Errors
    ///
    /// Why no terminal opened.
    fn terminal_at(&self, folder: &str) -> Result<(), String> {
        let _ = folder;
        Err(NO_FILES.to_owned())
    }

    /// Open `file` in the app that opens its kind: the person's own view of a file, or an app
    /// whose own action takes that kind. The shell picks the app by the file's first bytes and its
    /// name and starts it, under the app's grants; the file `file` names is not read here.
    ///
    /// # Errors
    ///
    /// Why it did not open: no app here opens its kind, the app may not open files, or the app it
    /// opens in did not start.
    fn open(&self, file: FileTarget<'_>) -> Result<(), String> {
        let _ = file;
        Err(NO_FILES.to_owned())
    }
}

impl Core {
    pub(crate) fn file_device_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let host = self.files.borrow();
        let Some(host) = host.as_ref() else {
            return match imp {
                Imp::FileList
                | Imp::FileStat
                | Imp::FileReadPath
                | Imp::FileWatch
                | Imp::FileOpenPath
                | Imp::FileOpen => self.result(Err(NO_FILES.to_owned())),
                _ => trap("not a file import"),
            };
        };
        if imp == Imp::FileTrashEntries {
            return self.result(
                host.trash_entries()
                    .map(|entries| Value::List(entries.iter().map(TrashEntry::value).collect())),
            );
        }
        match imp {
            Imp::FileList => {
                let path = self.text(a.h(0)?)?;
                self.result(
                    host.list(&path)
                        .map(|entries| Value::List(entries.iter().map(Entry::value).collect())),
                )
            }
            Imp::FileStat => {
                let path = self.text(a.h(0)?)?;
                self.result(host.stat(&path).map(|entry| {
                    entry.map_or_else(Value::none, |entry| Value::some(entry.value()))
                }))
            }
            Imp::FileReadPath => {
                let path = self.text(a.h(0)?)?;
                self.result(
                    host.read(&path)
                        .map(|bytes| Value::Bytes(std::rc::Rc::new(bytes))),
                )
            }
            Imp::FileWatch => {
                let (path, run) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                self.result(host.watch(&path, &run).map(Value::Text))
            }
            Imp::FileMkdir => {
                let path = self.text(a.h(0)?)?;
                self.result(host.mkdir(&path).map(|entry| entry.value()))
            }
            Imp::FileWrite => {
                let (path, bytes) = (self.text(a.h(0)?)?, self.bytes(a.h(1)?)?);
                self.result(host.write(&path, &bytes).map(|entry| entry.value()))
            }
            Imp::FileRename => {
                let (path, name) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                self.result(host.rename(&path, &name).map(|entry| entry.value()))
            }
            Imp::FileMove => {
                let (path, folder) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                self.result(host.move_entry(&path, &folder).map(|entry| entry.value()))
            }
            Imp::FileCopy => {
                let (path, folder) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                self.result(host.copy(&path, &folder).map(|entry| entry.value()))
            }
            Imp::FileRemove => {
                let path = self.text(a.h(0)?)?;
                self.result(host.remove(&path).map(Value::Bool))
            }
            Imp::FileTrash => {
                let path = self.text(a.h(0)?)?;
                self.result(host.trash(&path).map(|entry| entry.value()))
            }
            Imp::ToolDocumentApps => {
                let path = self.text(a.h(0)?)?;
                self.result(
                    host.document_apps(&path)
                        .map(|apps| Value::List(apps.iter().map(DocumentApp::value).collect())),
                )
            }
            Imp::ToolOpenWith => {
                let (path, app) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                self.result(host.open_with(&path, &app).map(|()| Value::Unit))
            }
            Imp::ToolTerminal => {
                let folder = self.text(a.h(0)?)?;
                self.result(host.terminal_at(&folder).map(|()| Value::Unit))
            }
            Imp::FileCompress => {
                let paths = self.with_val(a.h(0)?, |value| match value {
                    Value::List(items) => items
                        .iter()
                        .map(|item| match item {
                            Value::Text(path) => Ok(path.clone()),
                            other => trap(format!(
                                "a path is text, and this is a {}",
                                other.kind_name()
                            )),
                        })
                        .collect::<HResult<Vec<String>>>(),
                    other => trap(format!(
                        "expected a list of paths, got {}",
                        other.kind_name()
                    )),
                })?;
                let to = self.text(a.h(1)?)?;
                self.result(host.compress(&paths, &to).map(Value::Int))
            }
            Imp::FileExtract => {
                let (from, to) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                self.result(host.extract(&from, &to).map(Value::Int))
            }
            Imp::FileRestore => {
                let path = self.text(a.h(0)?)?;
                self.result(host.restore(&path).map(|entry| entry.value()))
            }
            Imp::FileOpenPath => {
                let path = self.text(a.h(0)?)?;
                self.result(host.open(FileTarget::Path(&path)).map(|()| Value::Unit))
            }
            Imp::FileOpen => {
                let file = self.file_arg(a.h(0)?)?;
                let target = FileTarget::Content {
                    name: &file.name,
                    hash: &file.hash,
                };
                self.result(host.open(target).map(|()| Value::Unit))
            }
            _ => trap("not a file import"),
        }
    }
}

/// What an app is told when the platform running it holds no files.
pub const NO_FILES: &str = "this host keeps no device files";

/// The fields `host.Entry` declares, in their order: what a listing's entries read as.
pub const TRASH_ENTRY_FIELDS: &[(&str, &str, &str)] = &[
    ("name", "Text", "What it is called in the trash."),
    ("path", "Text", "The path the trashed file has now."),
    ("original", "Text", "Where it stood when it was trashed."),
    (
        "deleted",
        "Time",
        "When it was trashed; 0 when the trash does not say.",
    ),
];

pub const ENTRY_FIELDS: &[(&str, &str, &str)] = &[
    ("name", "Text", "What it is called, as its folder shows it."),
    ("path", "Text", "The path that reaches it."),
    ("folder", "Bool", "Whether it is a folder."),
    ("size", "Int", "Its size in bytes; 0 for a folder."),
    (
        "modified",
        "Time",
        "When its content last changed; 0 when the disk does not say.",
    ),
    ("hidden", "Bool", "Whether its name starts with a dot."),
];
