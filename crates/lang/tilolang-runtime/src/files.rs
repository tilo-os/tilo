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

    /// Open `file` in the app that opens its kind: the person's own view of a file, or an app
    /// whose own action takes that kind. The shell picks the app by the file's first bytes and its
    /// name and starts it, under the app's grants; the file `file` names is not read here.
    ///
    /// # Errors
    ///
    /// Why it did not open: no app here opens its kind, the app may not open files, or the app it
    /// opens in did not start.
    fn open(&self, file: FileTarget<'_>) -> Result<(), String>;
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
