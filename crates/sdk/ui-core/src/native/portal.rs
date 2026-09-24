//! The native file-chooser backends behind [`crate::portal`]: the XDG
//! Desktop Portal (`ashpd`) on Unix and the Common Item Dialogs on Windows,
//! with the `portal` feature. Without it every request fails cleanly.

#[cfg(all(feature = "portal", windows))]
mod windows;

#[cfg(all(feature = "portal", unix))]
use std::fs::{File, OpenOptions};
#[cfg(all(feature = "portal", unix))]
use std::path::Path;

#[cfg(feature = "portal")]
use crate::portal::{FileFilter, PickKind, map_uri, map_uris, open_uri};
use crate::portal::{
    LaunchChannel, LaunchOutcome, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel,
    OpenFileOutcome, PickRequest, PortalChannel, PortalOutcome, spawn_worker,
};
#[cfg(all(feature = "portal", unix))]
use crate::portal::{LaunchKind, open_regular_file};
#[cfg(all(feature = "portal", windows))]
use crate::portal::{LaunchKind, to_local_path};

/// The native [`crate::host::FilePicker`]: the XDG Desktop Portal on Unix and
/// the Common Item Dialogs on Windows, each on its own worker thread. Without
/// the `portal` feature every request fails cleanly.
pub struct DesktopFilePicker;

impl crate::host::FilePicker for DesktopFilePicker {
    fn pick(&self, request: PickRequest) -> PortalChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let name = request.thread_name.clone();
        spawn_worker(&name, move || {
            let _ = tx.send(single(request));
        });
        rx
    }

    fn pick_multiple(&self, request: PickRequest) -> MultiChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let name = request.thread_name.clone();
        spawn_worker(&name, move || {
            let _ = tx.send(multiple(request));
        });
        rx
    }

    fn open_file(&self, request: PickRequest) -> OpenFileChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let name = request.thread_name.clone();
        spawn_worker(&name, move || {
            let _ = tx.send(single_open_file(request));
        });
        rx
    }

    fn launch(&self, request: LaunchRequest) -> LaunchChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let name = request.thread_name.clone();
        spawn_worker(&name, move || {
            let _ = tx.send(launch(request));
        });
        rx
    }
}

// ---------------------------------------------------------------------------
// The ashpd round-trip.
// ---------------------------------------------------------------------------

#[cfg(feature = "portal")]
fn single(req: PickRequest) -> PortalOutcome {
    match run_picker(req) {
        Ok(uris) => map_uri(&uris),
        Err(error) => error.into_single(),
    }
}

#[cfg(feature = "portal")]
fn single_open_file(req: PickRequest) -> OpenFileOutcome {
    if !matches!(req.kind, PickKind::Open { multiple: false }) {
        return OpenFileOutcome::Failed(
            "an already-open file request must be a single-file Open chooser".to_string(),
        );
    }
    match run_picker(req) {
        Ok(uris) => match uris.first() {
            Some(uri) => open_uri(uri),
            None => OpenFileOutcome::Cancelled,
        },
        Err(error) => error.into_open_file(),
    }
}

#[cfg(feature = "portal")]
fn multiple(req: PickRequest) -> MultiOutcome {
    match run_picker(req) {
        Ok(uris) => map_uris(&uris),
        Err(error) => error.into_multi(),
    }
}

#[cfg(all(feature = "portal", unix))]
fn run_picker(req: PickRequest) -> Result<Vec<url::Url>, PortalFailure> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| PortalFailure::Failed(format!("portal runtime: {error}")))?;
    runtime.block_on(run(req))
}

#[cfg(all(feature = "portal", windows))]
fn run_picker(req: PickRequest) -> Result<Vec<url::Url>, PortalFailure> {
    windows::pick(req)
}

#[cfg(all(feature = "portal", windows))]
fn launch(req: LaunchRequest) -> LaunchOutcome {
    windows::launch(req)
}

#[cfg(all(feature = "portal", unix))]
fn launch(req: LaunchRequest) -> LaunchOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(error) => return LaunchOutcome::Failed(format!("portal runtime: {error}")),
    };
    rt.block_on(async move {
        use std::os::fd::AsFd;

        let target = match req.kind {
            LaunchKind::Open => req.path,
            LaunchKind::Reveal => req.path.parent().map(Path::to_path_buf).unwrap_or(req.path),
        };
        if !target.is_absolute() {
            return LaunchOutcome::Failed("desktop-open path must be absolute".into());
        }
        let file = match req.kind {
            LaunchKind::Open => open_regular_file(&target),
            LaunchKind::Reveal => open_directory(&target),
        };
        let file = match file {
            Ok(file) => file,
            Err(error) => return LaunchOutcome::Failed(format!("could not open path: {error}")),
        };
        let request = match req.kind {
            LaunchKind::Open => {
                ashpd::desktop::open_uri::OpenFileRequest::default()
                    .send_file(&file.as_fd())
                    .await
            }
            LaunchKind::Reveal => {
                ashpd::desktop::open_uri::OpenDirectoryRequest::default()
                    .send(&file.as_fd())
                    .await
            }
        };
        match request.and_then(|request| request.response()) {
            Ok(()) => LaunchOutcome::Opened,
            Err(error) => LaunchOutcome::Failed(error.to_string()),
        }
    })
}

#[cfg(all(feature = "portal", unix))]
fn open_directory(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// A portal round-trip that did not return URIs.
#[cfg(feature = "portal")]
enum PortalFailure {
    /// The user dismissed the picker. Surfaces from ashpd as a "cancelled"
    /// response error, not a bug.
    Cancelled,
    Failed(String),
}

#[cfg(feature = "portal")]
impl PortalFailure {
    fn into_single(self) -> PortalOutcome {
        match self {
            PortalFailure::Cancelled => PortalOutcome::Cancelled,
            PortalFailure::Failed(e) => PortalOutcome::Failed(e),
        }
    }
    fn into_multi(self) -> MultiOutcome {
        match self {
            PortalFailure::Cancelled => MultiOutcome::Cancelled,
            PortalFailure::Failed(e) => MultiOutcome::Failed(e),
        }
    }
    fn into_open_file(self) -> OpenFileOutcome {
        match self {
            PortalFailure::Cancelled => OpenFileOutcome::Cancelled,
            PortalFailure::Failed(e) => OpenFileOutcome::Failed(e),
        }
    }
}

#[cfg(all(feature = "portal", unix))]
async fn run(req: PickRequest) -> Result<Vec<url::Url>, PortalFailure> {
    use ashpd::desktop::file_chooser::SelectedFiles;

    let to_ashpd = |f: &FileFilter| {
        let mut out = ashpd::desktop::file_chooser::FileFilter::new(&f.name);
        for g in &f.globs {
            out = out.glob(g);
        }
        for m in &f.mimetypes {
            out = out.mimetype(m);
        }
        out
    };

    let response = match &req.kind {
        PickKind::Open { multiple } => {
            let mut r = SelectedFiles::open_file()
                .title(req.title.as_str())
                .accept_label(req.accept_label.as_str())
                .modal(true)
                .multiple(*multiple);
            for f in &req.filters {
                r = r.filter(to_ashpd(f));
            }
            r.send().await.and_then(|r| r.response())
        }
        PickKind::Directory => SelectedFiles::open_file()
            .title(req.title.as_str())
            .accept_label(req.accept_label.as_str())
            .modal(true)
            .directory(true)
            .send()
            .await
            .and_then(|r| r.response()),
        PickKind::Save { suggested_name } => {
            let mut r = SelectedFiles::save_file()
                .title(req.title.as_str())
                .accept_label(req.accept_label.as_str())
                .modal(true)
                .current_name(suggested_name.as_str());
            for f in &req.filters {
                r = r.filter(to_ashpd(f));
            }
            r.send().await.and_then(|r| r.response())
        }
    };

    match response {
        Ok(files) => files
            .uris()
            .iter()
            .map(|uri| url::Url::parse(uri.as_str()))
            .collect::<Result<_, _>>()
            .map_err(|e| PortalFailure::Failed(format!("portal returned a malformed URI: {e}"))),
        Err(ashpd::Error::Response(_)) => Err(PortalFailure::Cancelled),
        Err(e) => Err(PortalFailure::Failed(e.to_string())),
    }
}

/// Without the `portal` feature there is no picker: report a failure so the
/// caller still gets exactly one drained outcome and never hangs waiting for a
/// picker that cannot open.
#[cfg(not(feature = "portal"))]
fn single(_req: PickRequest) -> PortalOutcome {
    PortalOutcome::Failed(NO_PORTAL.to_string())
}

#[cfg(not(feature = "portal"))]
fn single_open_file(_req: PickRequest) -> OpenFileOutcome {
    OpenFileOutcome::Failed(NO_PORTAL.to_string())
}

#[cfg(not(feature = "portal"))]
fn multiple(_req: PickRequest) -> MultiOutcome {
    MultiOutcome::Failed(NO_PORTAL.to_string())
}

#[cfg(not(feature = "portal"))]
fn launch(_req: LaunchRequest) -> LaunchOutcome {
    LaunchOutcome::Failed(NO_PORTAL.to_string())
}

#[cfg(not(feature = "portal"))]
const NO_PORTAL: &str = "the file picker needs the real-desktop build (`--features portal`)";
