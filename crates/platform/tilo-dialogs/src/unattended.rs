//! Choosers for a session nobody sits at ([`crate::portal::SAVE_DIRECTORY_ENV`]):
//! a Save answers at once with a free path in the save directory, under the
//! name the app suggested; every Open is refused, since only a person, or an
//! action naming the file, can choose one. Opening a path with its app is the
//! system backend's.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::portal::{
    LaunchChannel, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel, OpenFileOutcome,
    PickKind, PickRequest, PortalChannel, PortalOutcome,
};
use crate::{FilePicker, answered};

const NOBODY: &str = "nobody is at this computer to choose a file; name the file instead";

pub(crate) struct SaveDirectoryPicker {
    directory: PathBuf,
    system: Arc<dyn FilePicker>,
}

impl SaveDirectoryPicker {
    /// The unattended picker when `directory` is absolute, else `None`.
    pub(crate) fn new(directory: PathBuf, system: Arc<dyn FilePicker>) -> Option<Self> {
        directory
            .is_absolute()
            .then_some(Self { directory, system })
    }
}

impl FilePicker for SaveDirectoryPicker {
    fn pick(&self, request: PickRequest) -> PortalChannel {
        answered(match request.kind() {
            PickKind::Save { suggested_name } => {
                PortalOutcome::Opened(unused_path(&self.directory, suggested_name))
            }
            PickKind::Open { .. } | PickKind::Directory => PortalOutcome::Failed(NOBODY.into()),
        })
    }

    fn pick_multiple(&self, _request: PickRequest) -> MultiChannel {
        answered(MultiOutcome::Failed(NOBODY.into()))
    }

    fn open_file(&self, request: PickRequest) -> OpenFileChannel {
        answered(OpenFileOutcome::Failed(
            if matches!(request.kind(), PickKind::Open { multiple: false }) {
                NOBODY.into()
            } else {
                "an already-open file request must be a single-file Open chooser".into()
            },
        ))
    }

    fn launch(&self, request: LaunchRequest) -> LaunchChannel {
        self.system.launch(request)
    }
}

/// `name`'s file name in `directory`, numbered before its extension if taken.
fn unused_path(directory: &Path, name: &str) -> PathBuf {
    let name = Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("untitled");
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (name, None),
    };
    (1_u32..=10_000)
        .map(|n| {
            let numbered = if n == 1 {
                stem.to_owned()
            } else {
                format!("{stem}-{n}")
            };
            directory.join(match extension {
                Some(extension) => format!("{numbered}.{extension}"),
                None => numbered,
            })
        })
        .find(|path| !path.exists())
        .unwrap_or_else(|| directory.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker(directory: &Path) -> SaveDirectoryPicker {
        SaveDirectoryPicker::new(directory.to_path_buf(), crate::unavailable_picker()).unwrap()
    }

    #[test]
    fn the_unattended_picker_keeps_the_picker_contract() {
        let directory = tempfile::tempdir().unwrap();
        crate::conformance::check_picker_contract(&picker(directory.path()));
        assert!(SaveDirectoryPicker::new("out".into(), crate::unavailable_picker()).is_none());
    }

    #[test]
    fn a_save_lands_in_the_directory_under_a_free_name_and_an_open_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let picker = picker(directory.path());
        let save = |name: &str| picker.pick(PickRequest::save("Save", "Save", name)).recv();
        assert!(matches!(
            save("note.md"),
            Ok(PortalOutcome::Opened(path)) if path == directory.path().join("note.md")
        ));
        std::fs::write(directory.path().join("note.md"), b"").unwrap();
        assert!(matches!(
            save("../note.md"),
            Ok(PortalOutcome::Opened(path)) if path == directory.path().join("note-2.md")
        ));
        assert!(matches!(
            save(""),
            Ok(PortalOutcome::Opened(path)) if path == directory.path().join("untitled")
        ));
        assert!(matches!(
            picker.pick(PickRequest::open("Open", "Open")).recv(),
            Ok(PortalOutcome::Failed(_))
        ));
        assert!(matches!(
            picker
                .pick(PickRequest::directory("Folder", "Choose"))
                .recv(),
            Ok(PortalOutcome::Failed(_))
        ));
    }
}
