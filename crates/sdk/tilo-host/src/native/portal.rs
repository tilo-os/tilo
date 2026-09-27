//! The native file-chooser backends behind [`crate::portal`]: the XDG
//! Desktop Portal (`ashpd`) on Unix and the Common Item Dialogs on Windows,
//! with the `portal` feature. Without it every request fails cleanly.

#[cfg(all(feature = "portal", windows))]
mod windows;

#[cfg(all(feature = "portal", freedesktop))]
use std::fs::{File, OpenOptions};
#[cfg(all(feature = "portal", freedesktop))]
use std::path::Path;

#[cfg(not(all(feature = "portal", freedesktop)))]
use crate::portal::spawn_worker;
#[cfg(feature = "portal")]
use crate::portal::{FileFilter, PickKind, map_uri, map_uris, open_selected_uri};
use crate::portal::{
    LaunchChannel, LaunchOutcome, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel,
    OpenFileOutcome, PickRequest, PortalChannel, PortalOutcome,
};
#[cfg(all(feature = "portal", freedesktop))]
use crate::portal::{LaunchKind, open_regular_file};
#[cfg(all(feature = "portal", windows))]
use crate::portal::{LaunchKind, to_local_path};

/// The native [`crate::host::FilePicker`]: the XDG Desktop Portal on Unix,
/// whose D-Bus round-trips run on the app runtime, and the Common Item
/// Dialogs on Windows, each on its own worker thread. Without the `portal`
/// feature every request fails cleanly.
pub struct DesktopFilePicker;

#[cfg(all(feature = "portal", freedesktop))]
impl crate::host::FilePicker for DesktopFilePicker {
    fn pick(&self, request: PickRequest) -> PortalChannel {
        deliver(async move {
            match run(request).await {
                Ok(uris) => map_uri(&uris),
                Err(error) => error.into_single(),
            }
        })
    }

    fn pick_multiple(&self, request: PickRequest) -> MultiChannel {
        deliver(async move {
            match run(request).await {
                Ok(uris) => map_uris(&uris),
                Err(error) => error.into_multi(),
            }
        })
    }

    fn open_file(&self, request: PickRequest) -> OpenFileChannel {
        deliver(async move {
            if !matches!(request.kind, PickKind::Open { multiple: false }) {
                return OpenFileOutcome::Failed(
                    "an already-open file request must be a single-file Open chooser".to_string(),
                );
            }
            match run(request).await {
                Ok(uris) => match uris.into_iter().next() {
                    // Opening may block on a slow filesystem.
                    Some(uri) => tilo_rt::app()
                        .spawn_blocking(move || open_selected_uri(&uri))
                        .await
                        .unwrap_or_else(|error| {
                            OpenFileOutcome::Failed(format!("could not open the file: {error}"))
                        }),
                    None => OpenFileOutcome::Cancelled,
                },
                Err(error) => error.into_open_file(),
            }
        })
    }

    fn launch(&self, request: LaunchRequest) -> LaunchChannel {
        deliver(launch(request))
    }
}

/// Run one portal request on the app runtime and return the channel its
/// outcome arrives on. Sending it wakes the event loop of the wake scope the
/// request was made under.
#[cfg(all(feature = "portal", freedesktop))]
fn deliver<T: Send + 'static>(
    request: impl std::future::Future<Output = T> + Send + 'static,
) -> std::sync::mpsc::Receiver<T> {
    let (reply, rx) = crate::wake::reply();
    drop(tilo_rt::app().spawn_then(request, move |outcome| reply.send(outcome)));
    rx
}

#[cfg(not(all(feature = "portal", freedesktop)))]
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
// The Windows dialog round-trip.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "portal", windows))]
fn single(req: PickRequest) -> PortalOutcome {
    match windows::pick(req) {
        Ok(uris) => map_uri(&uris),
        Err(error) => error.into_single(),
    }
}

#[cfg(all(feature = "portal", windows))]
fn single_open_file(req: PickRequest) -> OpenFileOutcome {
    if !matches!(req.kind, PickKind::Open { multiple: false }) {
        return OpenFileOutcome::Failed(
            "an already-open file request must be a single-file Open chooser".to_string(),
        );
    }
    match windows::pick(req) {
        Ok(uris) => match uris.first() {
            Some(uri) => open_selected_uri(uri),
            None => OpenFileOutcome::Cancelled,
        },
        Err(error) => error.into_open_file(),
    }
}

#[cfg(all(feature = "portal", windows))]
fn multiple(req: PickRequest) -> MultiOutcome {
    match windows::pick(req) {
        Ok(uris) => map_uris(&uris),
        Err(error) => error.into_multi(),
    }
}

#[cfg(all(feature = "portal", windows))]
fn launch(req: LaunchRequest) -> LaunchOutcome {
    windows::launch(req)
}

// ---------------------------------------------------------------------------
// The ashpd round-trip.
// ---------------------------------------------------------------------------

#[cfg(all(feature = "portal", freedesktop))]
async fn launch(req: LaunchRequest) -> LaunchOutcome {
    use std::os::fd::AsFd;

    let target = match req.kind {
        LaunchKind::Open => req.path,
        LaunchKind::Reveal => req.path.parent().map(Path::to_path_buf).unwrap_or(req.path),
    };
    if !target.is_absolute() {
        return LaunchOutcome::Failed("desktop-open path must be absolute".into());
    }
    let kind = req.kind;
    // Opening may block on a slow filesystem.
    let file = tilo_rt::app()
        .spawn_blocking(move || match kind {
            LaunchKind::Open => open_regular_file(&target),
            LaunchKind::Reveal => open_directory(&target),
        })
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error)));
    let file = match file {
        Ok(file) => file,
        Err(error) => return LaunchOutcome::Failed(format!("could not open path: {error}")),
    };
    let request = match kind {
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
}

/// Hand an open picture to the XDG Wallpaper portal; the desktop previews it
/// and asks the user before applying.
#[cfg(all(feature = "portal", freedesktop))]
pub(crate) fn set_wallpaper(
    file: std::fs::File,
    target: crate::portal::WallpaperTarget,
) -> crate::portal::WallpaperChannel {
    use crate::portal::WallpaperTarget;
    use ashpd::desktop::wallpaper::{SetOn, WallpaperRequest};
    let set_on = match target {
        WallpaperTarget::Background => SetOn::Background,
        WallpaperTarget::Lockscreen => SetOn::Lockscreen,
        WallpaperTarget::Both => SetOn::Both,
    };
    deliver(async move {
        WallpaperRequest::default()
            .show_preview(true)
            .set_on(set_on)
            .build_file(&file)
            .await
            .and_then(|request| request.response())
            .map_err(|error| error.to_string())
    })
}

/// Hand `uri` to the XDG `OpenURI` portal, blocking until the desktop has
/// launched its handler or refused the request.
#[cfg(all(feature = "portal", freedesktop))]
pub(crate) fn open_uri(uri: &str) -> std::io::Result<()> {
    let uri = ashpd::Uri::parse(uri).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("bad URI: {error}"),
        )
    })?;
    // The D-Bus round trip runs on the app runtime; callers are workers,
    // never a thread of that runtime.
    tilo_rt::try_app()
        .and_then(|runtime| {
            runtime.block_on(async {
                ashpd::desktop::open_uri::OpenFileRequest::default()
                    .send_uri(&uri)
                    .await
                    .and_then(|request| request.response())
            })
        })
        .map_err(std::io::Error::other)?
        .map_err(std::io::Error::other)
}

#[cfg(all(feature = "portal", freedesktop))]
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

#[cfg(all(feature = "portal", freedesktop))]
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

#[cfg(all(test, feature = "portal", freedesktop))]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]

    use super::*;
    use crate::host::FilePicker;

    /// Portal requests run on the app runtime, never on the thread that asks,
    /// and their outcome wakes the asking event loop. A relative path fails
    /// before any D-Bus traffic, so this needs no desktop session.
    #[test]
    fn launch_outcome_arrives_from_the_app_runtime_and_wakes_the_event_loop() {
        std::thread::Builder::new()
            .name("ui-event-loop".into())
            .spawn(|| {
                tilo_rt::mark_ui_thread();
                let mut scope = crate::wake::WakeScope::new();
                let (woke, wakes) = std::sync::mpsc::channel();
                scope
                    .bind(move |event| {
                        let _ = woke.send(event);
                    })
                    .unwrap();
                let _entry = scope.enter();
                let outcomes = DesktopFilePicker.launch(LaunchRequest::open("relative/file.txt"));
                let event = wakes
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                assert!(scope.accepts(&event));
                assert!(matches!(
                    outcomes.try_recv().unwrap(),
                    LaunchOutcome::Failed(reason) if reason.contains("absolute")
                ));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
