//! Windows Common Item Dialogs, hosted on the existing picker worker thread.
//!
//! COM objects and their apartment stay on that thread. Only validated file
//! URIs cross the channel back into the shared outcome/drain implementation.

use crate::PortalFailure;
use crate::portal::{
    LaunchChannel, LaunchKind, LaunchOutcome, LaunchRequest, MultiChannel, OpenFileChannel,
    OpenFileOutcome, PickKind, PickRequest, PortalChannel, map_uri, map_uris, open_selected_uri,
    spawn_worker,
};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR,
    FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog,
    IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, Interface, PCWSTR};

/// The Windows [`crate::FilePicker`]: each request on its own worker thread.
pub(crate) struct WindowsFilePicker;

impl crate::FilePicker for WindowsFilePicker {
    fn pick(&self, request: PickRequest) -> PortalChannel {
        let name = request.worker_name().to_owned();
        on_worker(&name, move || match pick(&request) {
            Ok(uris) => map_uri(&uris),
            Err(error) => error.into_single(),
        })
    }

    fn pick_multiple(&self, request: PickRequest) -> MultiChannel {
        let name = request.worker_name().to_owned();
        on_worker(&name, move || match pick(&request) {
            Ok(uris) => map_uris(&uris),
            Err(error) => error.into_multi(),
        })
    }

    fn open_file(&self, request: PickRequest) -> OpenFileChannel {
        let name = request.worker_name().to_owned();
        on_worker(&name, move || {
            if !matches!(request.kind, PickKind::Open { multiple: false }) {
                return OpenFileOutcome::Failed(
                    "an already-open file request must be a single-file Open chooser".to_string(),
                );
            }
            match pick(&request) {
                Ok(uris) => match uris.first() {
                    Some(uri) => open_selected_uri(uri),
                    None => OpenFileOutcome::Cancelled,
                },
                Err(error) => error.into_open_file(),
            }
        })
    }

    fn launch(&self, request: LaunchRequest) -> LaunchChannel {
        let name = request.thread_name.clone();
        on_worker(&name, move || launch(request))
    }
}

fn on_worker<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> std::sync::mpsc::Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    spawn_worker(name, move || {
        let _ = tx.send(work());
    });
    rx
}

struct Apartment;

impl Apartment {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: Each request runs on a dedicated worker. S_OK and S_FALSE
        // both acquire one COM initialization reference, balanced by Drop.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: This guard never leaves its initialized worker thread.
        unsafe { CoUninitialize() };
    }
}

fn pick(req: &PickRequest) -> Result<Vec<url::Url>, PortalFailure> {
    pick_inner(req).map_err(|error| {
        if error.code() == ERROR_CANCELLED.to_hresult() {
            PortalFailure::Cancelled
        } else {
            PortalFailure::Failed(format!("Windows file picker: {error}"))
        }
    })
}

fn pick_inner(req: &PickRequest) -> windows::core::Result<Vec<url::Url>> {
    let _apartment = Apartment::new()?;
    // SAFETY: COM is initialized on this thread and the generated bindings
    // retain ownership of every interface. All string buffers outlive calls.
    unsafe {
        match &req.kind {
            PickKind::Save { suggested_name } => {
                let dialog: IFileSaveDialog =
                    CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
                configure(&dialog.cast()?, req)?;
                dialog.SetFileName(&HSTRING::from(suggested_name))?;
                dialog.Show(None)?;
                Ok(vec![item_uri(&dialog.GetResult()?)?])
            }
            PickKind::Open { .. } | PickKind::Directory => {
                let dialog: IFileOpenDialog =
                    CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
                configure(&dialog.cast()?, req)?;
                dialog.Show(None)?;
                let items = dialog.GetResults()?;
                (0..items.GetCount()?)
                    .map(|index| item_uri(&items.GetItemAt(index)?))
                    .collect()
            }
        }
    }
}

fn configure(dialog: &IFileDialog, req: &PickRequest) -> windows::core::Result<()> {
    let filters: Vec<(HSTRING, HSTRING)> = req
        .filters
        .iter()
        .map(|filter| {
            (
                HSTRING::from(&filter.name),
                HSTRING::from(filter_pattern(filter)),
            )
        })
        .collect();
    let specs: Vec<COMDLG_FILTERSPEC> = filters
        .iter()
        .map(|(name, pattern)| COMDLG_FILTERSPEC {
            pszName: PCWSTR(name.as_ptr()),
            pszSpec: PCWSTR(pattern.as_ptr()),
        })
        .collect();
    // SAFETY: The interface belongs to this STA; filters and HSTRING values
    // remain allocated for the full calls, which copy their input strings.
    unsafe {
        let mut options =
            dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR | FOS_PATHMUSTEXIST;
        options |= match req.kind {
            PickKind::Open { multiple: true } => FOS_FILEMUSTEXIST | FOS_ALLOWMULTISELECT,
            PickKind::Open { multiple: false } => FOS_FILEMUSTEXIST,
            PickKind::Directory => FOS_PICKFOLDERS,
            PickKind::Save { .. } => FOS_OVERWRITEPROMPT,
        };
        dialog.SetOptions(options)?;
        dialog.SetTitle(&HSTRING::from(&req.title))?;
        dialog.SetOkButtonLabel(&HSTRING::from(&req.accept_label))?;
        if !specs.is_empty() && !matches!(req.kind, PickKind::Directory) {
            dialog.SetFileTypes(&specs)?;
        }
    }
    Ok(())
}

fn filter_pattern(filter: &crate::portal::FileFilter) -> String {
    let mut globs = filter.globs.clone();
    for mime in &filter.mimetypes {
        if let Some(extensions) = mime_guess::get_mime_extensions_str(mime) {
            for extension in extensions {
                let glob = format!("*.{extension}");
                if !globs.contains(&glob) {
                    globs.push(glob);
                }
            }
        }
    }
    if globs.is_empty() {
        "*.*".into()
    } else {
        globs.join(";")
    }
}

fn item_uri(item: &IShellItem) -> windows::core::Result<url::Url> {
    // SAFETY: SIGDN_FILESYSPATH returns a null-terminated COM allocation. Copy
    // it before freeing it, including on malformed UTF-16/error outcomes.
    let path = unsafe {
        let allocated = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = allocated.to_string();
        CoTaskMemFree(Some(allocated.0.cast()));
        PathBuf::from(path?)
    };
    let uri = url::Url::from_file_path(path).map_err(|()| invalid_path())?;
    crate::portal::to_local_path(&uri).ok_or_else(invalid_path)?;
    Ok(uri)
}

fn invalid_path() -> windows::core::Error {
    windows::core::Error::new(
        windows::Win32::Foundation::E_INVALIDARG,
        "the Windows picker did not return a local absolute file path",
    )
}

fn launch(req: LaunchRequest) -> LaunchOutcome {
    let target = match req.kind {
        LaunchKind::Open => req.path,
        LaunchKind::Reveal => req.path.parent().map(Path::to_path_buf).unwrap_or(req.path),
    };
    let Ok(uri) = url::Url::from_file_path(&target) else {
        return LaunchOutcome::Failed("desktop-open path must be absolute".into());
    };
    if crate::portal::to_local_path(&uri).is_none() {
        return LaunchOutcome::Failed("desktop-open path must be local".into());
    }
    // Hold the validated handle through dispatch. Refuse delete sharing so
    // the final path cannot be replaced between validation and dispatch.
    let _target_handle = match open_launch_target(&target, req.kind) {
        Ok(file) => file,
        Err(error) => return LaunchOutcome::Failed(format!("could not open path: {error}")),
    };
    match open::that(&target) {
        Ok(()) => LaunchOutcome::Opened,
        Err(error) => LaunchOutcome::Failed(error.to_string()),
    }
}

fn open_launch_target(path: &Path, kind: LaunchKind) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    let expected_kind = match kind {
        LaunchKind::Open => metadata.is_file(),
        LaunchKind::Reveal => metadata.is_dir(),
    };
    if !expected_kind || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "desktop-open target is a link or has the wrong file type",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_dialogs_check_handles_filters_and_configuration() {
        // phase: launch_handle_checks_type_and_prevents_replacement_during_dispatch
        {
            let temporary = tempfile::tempdir().unwrap();
            let path = temporary.path().join("chosen.txt");
            std::fs::write(&path, b"chosen").unwrap();
            assert!(open_launch_target(&path, LaunchKind::Reveal).is_err());
            assert!(open_launch_target(temporary.path(), LaunchKind::Open).is_err());
            let handle = open_launch_target(&path, LaunchKind::Open).unwrap();
            assert!(std::fs::rename(&path, path.with_extension("moved")).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"chosen");
            drop(handle);
            std::fs::rename(&path, path.with_extension("moved")).unwrap();
        }
        // phase: filters_preserve_globs_and_expand_mime_types
        {
            let filter = crate::portal::FileFilter::new("PDF")
                .glob("*.pdf")
                .mimetype("application/pdf");
            assert_eq!(filter_pattern(&filter), "*.pdf");
            assert!(
                filter_pattern(&crate::portal::FileFilter::new("PNG").mimetype("image/png"))
                    .split(';')
                    .any(|glob| glob == "*.png")
            );
        }
        // phase: native_dialog_configuration_roundtrips_without_showing_a_window
        {
            let _apartment = Apartment::new().unwrap();
            for request in [
                PickRequest::open("Open", "Open").all_files(),
                PickRequest::open_multiple("Import", "Import"),
                PickRequest::directory("Folder", "Choose"),
                PickRequest::save("Save", "Save", "report.txt"),
            ] {
                // SAFETY: Same COM apartment/ownership contract as the live path.
                unsafe {
                    let dialog: IFileDialog = CoCreateInstance(
                        if matches!(request.kind, PickKind::Save { .. }) {
                            &FileSaveDialog
                        } else {
                            &FileOpenDialog
                        },
                        None,
                        CLSCTX_INPROC_SERVER,
                    )
                    .unwrap();
                    configure(&dialog, &request).unwrap();
                    let options = dialog.GetOptions().unwrap();
                    assert!(options.contains(FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR));
                    assert_eq!(
                        options.contains(FOS_ALLOWMULTISELECT),
                        request.is_multiple()
                    );
                    assert_eq!(
                        options.contains(FOS_PICKFOLDERS),
                        matches!(request.kind, PickKind::Directory)
                    );
                }
            }
        }
    }
}
