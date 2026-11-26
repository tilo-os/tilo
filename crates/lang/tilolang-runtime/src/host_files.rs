//! The host calls on the device's files, answered through its [`crate::files::FilesHost`].

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::files::{DocumentApp, Entry, FileTarget, FilesHost, NO_FILES, NO_LINKS, TrashEntry};
use crate::value::Value;

impl Core {
    /// The disk's answer to `imp`, asked of the device once in a run that may be run again: the
    /// journal answers what the device answered, so a run that goes on after a call it waited on
    /// neither reads a folder again (which its own moves changed) nor moves, renames or trashes a
    /// file a second time.
    pub(crate) fn file_device_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let host = self.files.borrow();
        let Some(host) = host.as_deref() else {
            return self.no_file_host(imp);
        };
        let answer = self.journaled_read(|| self.file_device_answer(host, imp, a))?;
        self.ret_h(answer)
    }

    /// What the disk answers `imp`, as the value the code reads: a `Result` of what it
    /// answered, or the one import that answers a value whole.
    fn file_device_answer(&self, host: &dyn FilesHost, imp: Imp, a: &Args<'_>) -> HResult<Value> {
        let outcome = match imp {
            Imp::FileTrashEntries => host
                .trash_entries()
                .map(|entries| Value::List(entries.iter().map(TrashEntry::value).collect())),
            Imp::FileList => {
                let path = self.text(a.h(0)?)?;
                host.list(&path)
                    .map(|entries| Value::List(entries.iter().map(Entry::value).collect()))
            }
            Imp::FileStat => {
                let path = self.text(a.h(0)?)?;
                host.stat(&path)
                    .map(|entry| entry.map_or_else(Value::none, |entry| Value::some(entry.value())))
            }
            Imp::FileReadPath => {
                let path = self.text(a.h(0)?)?;
                host.read(&path)
                    .map(|bytes| Value::Bytes(std::rc::Rc::new(bytes)))
            }
            Imp::FilePreview => {
                let path = self.text(a.h(0)?)?;
                let size = a.long(1)?;
                if !(1..=4096).contains(&size) {
                    return trap(format!(
                        "a preview is 1 to 4096 pixels a side, and this asks for {size}"
                    ));
                }
                host.preview(&path, u32::try_from(size).unwrap_or(1))
                    .map(|preview| {
                        preview.map_or_else(Value::none, |file| Value::some(Value::file(file)))
                    })
            }
            Imp::FileWatch => {
                let (path, run) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                host.watch(&path, &run).map(Value::Text)
            }
            Imp::FileMkdir => {
                let path = self.text(a.h(0)?)?;
                host.mkdir(&path).map(|entry| entry.value())
            }
            Imp::FileWrite => {
                let (path, bytes) = (self.text(a.h(0)?)?, self.bytes(a.h(1)?)?);
                host.write(&path, &bytes).map(|entry| entry.value())
            }
            Imp::FileRename => {
                let (path, name) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                host.rename(&path, &name).map(|entry| entry.value())
            }
            Imp::FileMove => {
                let (path, folder) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                host.move_entry(&path, &folder).map(|entry| entry.value())
            }
            Imp::FileCopy => {
                let (path, folder) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                host.copy(&path, &folder).map(|entry| entry.value())
            }
            Imp::FileCopyTo => {
                let (path, to) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                host.copy_to(&path, &to).map(|entry| entry.value())
            }
            Imp::FileRemove => {
                let path = self.text(a.h(0)?)?;
                host.remove(&path).map(Value::Bool)
            }
            Imp::FileTrash => {
                let path = self.text(a.h(0)?)?;
                host.trash(&path).map(|entry| entry.value())
            }
            Imp::FileRestore => {
                let path = self.text(a.h(0)?)?;
                host.restore(&path).map(|entry| entry.value())
            }
            Imp::FileOpenPath => {
                let path = self.text(a.h(0)?)?;
                host.open(FileTarget::Path(&path)).map(|()| Value::Unit)
            }
            Imp::FileOpen => {
                let file = self.file_arg(a.h(0)?)?;
                let target = FileTarget::Content {
                    name: &file.name,
                    hash: &file.hash,
                };
                host.open(target).map(|()| Value::Unit)
            }
            Imp::ToolContext => return Ok(Value::Text(host.launch_context())),
            Imp::ToolDocumentApps
            | Imp::ToolOpenWith
            | Imp::ToolTerminal
            | Imp::ToolOpenLink
            | Imp::ToolContainerShell => return self.tool_answer(host, imp, a),
            Imp::FileCompress | Imp::FileExtract => return self.archive_answer(host, imp, a),
            _ => return trap("not a file import"),
        };
        Ok(Self::result_value(outcome))
    }

    /// What a file import answers on a device that serves no files: the files it asks for are
    /// not here, and the tools its links open have no links to them.
    fn no_file_host(&self, imp: Imp) -> HResult<Option<Wasm>> {
        match imp {
            Imp::FileList
            | Imp::FileStat
            | Imp::FileReadPath
            | Imp::FilePreview
            | Imp::FileWatch
            | Imp::FileOpenPath
            | Imp::FileOpen => self.result(Err(NO_FILES.to_owned())),
            Imp::ToolDocumentApps
            | Imp::ToolOpenWith
            | Imp::ToolTerminal
            | Imp::ToolOpenLink
            | Imp::ToolContext
            | Imp::ToolContainerShell => self.result(Err(NO_LINKS.to_owned())),
            _ => trap("not a file import"),
        }
    }

    /// What the tools a person reaches from a file ask: the apps that open it, opening it with
    /// one, a terminal at a folder, and a container's shell.
    fn tool_answer(&self, host: &dyn FilesHost, imp: Imp, a: &Args<'_>) -> HResult<Value> {
        let outcome = match imp {
            Imp::ToolDocumentApps => {
                let path = self.text(a.h(0)?)?;
                host.document_apps(&path)
                    .map(|apps| Value::List(apps.iter().map(DocumentApp::value).collect()))
            }
            Imp::ToolOpenWith => {
                let (path, app) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                host.open_with(&path, &app).map(|()| Value::Unit)
            }
            Imp::ToolTerminal => {
                let folder = self.text(a.h(0)?)?;
                host.terminal_at(&folder).map(|()| Value::Unit)
            }
            Imp::ToolOpenLink => {
                let url = self.text(a.h(0)?)?;
                host.open_link(&url).map(|()| Value::Unit)
            }
            Imp::ToolContainerShell => {
                let (container, engine) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                host.container_shell(&container, &engine)
                    .map(|()| Value::Unit)
            }
            _ => return trap("not a tool import"),
        };
        Ok(Self::result_value(outcome))
    }

    /// Entries compressed into an archive, or an archive extracted into a folder.
    fn archive_answer(&self, host: &dyn FilesHost, imp: Imp, a: &Args<'_>) -> HResult<Value> {
        let outcome = match imp {
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
                host.compress(&paths, &to).map(Value::Int)
            }
            Imp::FileExtract => {
                let (from, to) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                host.extract(&from, &to).map(Value::Int)
            }
            _ => return trap("not an archive import"),
        };
        Ok(Self::result_value(outcome))
    }
}
