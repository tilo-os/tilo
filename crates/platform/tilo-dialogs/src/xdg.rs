//! The Linux backend: the XDG Desktop Portal (`ashpd`). Its D-Bus
//! round-trips run on the app runtime; outcomes wake the asking event loop.
//!
//! Inside the Tilo sandbox the portal is also where the file-access grant is
//! made, so pickers never fall back to anything else. Opening a URI falls back
//! to the desktop's opener only when no portal answers at all.

use std::fs::{File, OpenOptions};
use std::path::Path;

use crate::PortalFailure;
use crate::portal::{
    FileFilter, LaunchChannel, LaunchKind, LaunchOutcome, LaunchRequest, MultiChannel,
    OpenFileChannel, OpenFileOutcome, PickKind, PickRequest, PortalChannel, map_uri, map_uris,
    open_regular_file, open_selected_uri,
};

/// The portal [`crate::FilePicker`].
pub(crate) struct PortalFilePicker;

impl crate::FilePicker for PortalFilePicker {
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
fn deliver<T: Send + 'static>(
    request: impl std::future::Future<Output = T> + Send + 'static,
) -> std::sync::mpsc::Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let wake = tilo_rt::wake::WakeHandle::new();
    drop(tilo_rt::app().spawn_then(request, move |outcome| {
        // The UI may have moved on (window closed); ignore a dead channel.
        if tx.send(outcome).is_ok() {
            wake.wake();
        }
    }));
    rx
}

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
pub(crate) fn open_uri(uri: &str) -> std::io::Result<()> {
    let text = uri;
    let uri = ashpd::Uri::parse(uri).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("bad URI: {error}"),
        )
    })?;
    // The D-Bus round trip runs on the app runtime; callers are workers,
    // never a thread of that runtime.
    let answered = tilo_rt::try_app()
        .and_then(|runtime| {
            runtime.block_on(async {
                ashpd::desktop::open_uri::OpenFileRequest::default()
                    .send_uri(&uri)
                    .await
                    .and_then(|request| request.response())
            })
        })
        .map_err(std::io::Error::other)?;
    match answered {
        Ok(()) => Ok(()),
        // The portal answered and the user or its policy refused: final.
        Err(error @ (ashpd::Error::Response(_) | ashpd::Error::Portal(_))) => {
            Err(std::io::Error::other(error))
        }
        // No portal on this session (an unsandboxed desktop without
        // xdg-desktop-portal): the desktop's own opener.
        Err(error) => {
            tracing::debug!(%error, "no OpenURI portal; using the desktop opener");
            open::that_detached(text)
        }
    }
}

fn open_directory(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

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
        PickKind::Directory => {
            let mut request = SelectedFiles::open_file()
                .title(req.title.as_str())
                .accept_label(req.accept_label.as_str())
                .modal(true)
                .directory(true);
            if req.is_writable_directory() {
                // `choices` is a standard forwarded option. A private top-level
                // option would be discarded by xdg-desktop-portal. The label
                // comes from the caller's localized destination operation.
                request = request.choice(ashpd::desktop::file_chooser::Choice::boolean(
                    crate::portal::WRITABLE_DIRECTORY_CHOICE,
                    req.accept_label.as_str(),
                    true,
                ));
            }
            request.send().await.and_then(|r| r.response())
        }
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
        Ok(files)
            if req.is_writable_directory()
                && !files.choices().iter().any(|(id, value)| {
                    id == crate::portal::WRITABLE_DIRECTORY_CHOICE && value == "true"
                }) =>
        {
            Err(PortalFailure::Failed(
                "the desktop did not authorize a writable destination directory".into(),
            ))
        }
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

#[cfg(test)]
mod tests {

    use super::*;
    use crate::FilePicker;

    #[test]
    fn directory_requests_never_become_regular_file_descriptors() {
        for request in [
            PickRequest::directory("Read folder", "Choose"),
            PickRequest::writable_directory("Import into folder", "Import here"),
        ] {
            let result = PortalFilePicker
                .open_file(request)
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            assert!(matches!(result, OpenFileOutcome::Failed(reason)
                if reason.contains("single-file Open")));
        }
    }

    /// Portal requests run on the app runtime, never on the thread that asks,
    /// and their outcome wakes the asking event loop. A relative path fails
    /// before any D-Bus traffic, so this needs no desktop session.
    #[test]
    fn launch_outcome_arrives_from_the_app_runtime_and_wakes_the_event_loop() {
        std::thread::Builder::new()
            .name("ui-event-loop".into())
            .spawn(|| {
                tilo_rt::mark_ui_thread();
                let mut scope = tilo_rt::wake::WakeScope::new();
                let (woke, wakes) = std::sync::mpsc::channel();
                scope
                    .bind(move |event| {
                        let _ = woke.send(event);
                    })
                    .unwrap();
                let _entry = scope.enter();
                let outcomes = PortalFilePicker.launch(LaunchRequest::open("relative/file.txt"));
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
