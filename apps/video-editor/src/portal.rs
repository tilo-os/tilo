//! Media import, project open/save, and the export destination — everything
//! the editor reaches through the host file chooser.
//!
//! The portal client — the worker thread, the Tokio runtime, the `ashpd`
//! round-trip, and the URI→path rule that makes this the file-access security
//! boundary — lives in [`ui::portal`]. All this module carries is the filter
//! sets, the labels this app wants on each dialog, and the open-vs-save
//! distinction the project flow needs when a chosen path comes back.
//!
//! Import is *multi-select*: the user picks arbitrarily many clips at once and
//! every local one comes back to be probed and added to the media bin. A
//! `/var/workspace/...` pick resolves through the FUSE mount and hydrates on
//! read exactly like a local file (see [`crate::media`]).
//!
//! Reusing the shared primitive is not merely DRY: the previous per-app copy
//! mapped URIs with `strip_prefix("file://")`, which turns
//! `file://evil-host/etc/passwd` into a working-directory-relative path from a
//! URI that named a *remote host*. The shared [`ui::portal::to_local_path`]
//! rejects a foreign host outright, so converging on it closes that hole.

use ui::portal::{FileFilter, PickRequest};

pub use ui::portal::{MultiChannel as ImportChannel, MultiOutcome as ImportOutcome};
pub use ui::portal::{PortalChannel as ExportDestChannel, PortalOutcome as ExportDestOutcome};

/// The container / still formats the editor can bring into the bin. Both globs
/// and MIME types are supplied because portal backends differ in which they
/// honour.
fn media_filter() -> FileFilter {
    // Built from the very lists the probe accepts, so the chooser cannot
    // advertise less than the editor can open. It used to name five video
    // extensions, two audio and three image while `probe` recognised
    // twenty-eight, twenty-three and fifteen — so a `.flac`, an `.avi` or a
    // `.heic` was filtered out of the picker entirely, and a user with one had
    // no way to discover the editor would have taken it happily.
    //
    // MIME types stay hand-written: they are a small, stable set covering the
    // formats whose types portal backends actually honour, and there is no
    // extension-to-MIME table here to derive them from. The globs are the
    // authoritative half — a backend that honours neither shows everything,
    // which is also fine.
    let mut f = FileFilter::new("Media")
        .mimetype("video/mp4")
        .mimetype("video/webm")
        .mimetype("video/x-matroska")
        .mimetype("video/quicktime")
        .mimetype("video/x-msvideo")
        .mimetype("video/mpeg")
        .mimetype("audio/wav")
        .mimetype("audio/mpeg")
        .mimetype("audio/flac")
        .mimetype("audio/ogg")
        .mimetype("audio/opus")
        .mimetype("audio/mp4")
        .mimetype("audio/aac")
        .mimetype("image/png")
        .mimetype("image/jpeg")
        .mimetype("image/webp")
        .mimetype("image/gif")
        .mimetype("image/tiff")
        .mimetype("image/avif")
        .mimetype("image/heif");
    for ext in crate::probe::VIDEO_EXTS
        .iter()
        .chain(crate::probe::AUDIO_EXTS)
        .chain(crate::probe::IMAGE_EXTS)
    {
        f = f.glob(&format!("*.{ext}"));
    }
    f
}

/// Open the host multi-select chooser for media to import, on a worker thread.
/// Every local path the user picked arrives over the returned channel, which
/// the editor drains each frame (see `VideoEditorApp::drain_import`). The UI
/// thread is never blocked.
pub fn request_import() -> ImportChannel {
    PickRequest::open_multiple("Import media", "Import")
        .thread_name("video-editor-import")
        .filter(media_filter())
        .all_files()
        .send_multiple()
}

/// The same chooser delivering the chosen files' contents, for a host whose
/// files have no path (a browser page).
pub fn request_import_contents() -> ui::portal::ContentsChannel {
    PickRequest::open_multiple("Import media", "Import")
        .filter(media_filter())
        .all_files()
        .send_contents()
}

/// The chooser filter for one export container, so the Save dialog offers the
/// extension the encode will actually produce.
fn container_filter(ext: &str) -> FileFilter {
    let label = match ext {
        "mp4" => "MP4 video",
        "mov" => "QuickTime video",
        "webm" => "WebM video",
        other => return FileFilter::new(other).glob(&format!("*.{other}")),
    };
    let f = FileFilter::new(label).glob(&format!("*.{ext}"));
    match ext {
        "mp4" => f.mimetype("video/mp4"),
        "mov" => f.mimetype("video/quicktime"),
        "webm" => f.mimetype("video/webm"),
        _ => f,
    }
}

/// Ask the host where to write an export, on a worker thread.
///
/// Export previously had no destination UI at all: the output path came from
/// `TILO_VE_EXPORT_OUT` / `TILO_VE_EXPORT_DIR` or fell back to the temp
/// directory, so a user could not choose where their render landed and, under
/// `landlock = "strict"`, the app has no standing grant to write into
/// `~/Videos` anyway. Routing the destination through the Save portal is what
/// *earns* the write: the grant follows the file the user picked, which is the
/// same boundary [`request_import`] relies on for reads.
///
/// `suggested_name` seeds the filename field (the project name plus the
/// container's extension); `ext` also selects the type filter so the dialog
/// does not offer to save an H.264 file as `.webm`.
pub fn request_export_destination(suggested_name: &str, ext: &str) -> ExportDestChannel {
    PickRequest::save("Export video", "Export", suggested_name)
        .thread_name("video-editor-export-dest")
        .filter(container_filter(ext))
        .all_files()
        .send()
}

// ---------------------------------------------------------------------------
// Project files
// ---------------------------------------------------------------------------

/// The chooser filter for the editor's own project file. Both the glob and a
/// MIME type, for the same reason the media filter carries both.
fn project_filter() -> FileFilter {
    FileFilter::new(crate::project_file::KIND_LABEL)
        .glob(&format!("*.{}", crate::project_file::EXTENSION))
        .mimetype("application/x-tilo-video-project")
}

/// Which of the two project dialogs a chosen path came back from.
///
/// The portal hands back the same shape either way — one path — and the two
/// mean opposite things: one is a file to read, the other a file to write.
/// Getting them the wrong way round would overwrite the project the user
/// meant to open, so the distinction is carried in the type rather than
/// remembered in a flag beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectOutcome {
    Open(std::path::PathBuf),
    Save(std::path::PathBuf),
    Cancelled,
    Failed(String),
}

/// Receiver the UI thread drains for an in-flight project open/save.
pub struct ProjectChannel {
    rx: ui::portal::PortalChannel,
    save: bool,
}

impl ProjectChannel {
    /// Non-blocking drain, mirroring [`std::sync::mpsc::Receiver::try_recv`].
    ///
    /// # Errors
    ///
    /// [`std::sync::mpsc::TryRecvError::Empty`] while the dialog is still
    /// open, and `Disconnected` once it can no longer answer.
    pub fn try_recv(&self) -> Result<ProjectOutcome, std::sync::mpsc::TryRecvError> {
        self.rx.try_recv().map(|outcome| match outcome {
            ui::portal::PortalOutcome::Opened(p) if self.save => ProjectOutcome::Save(p),
            ui::portal::PortalOutcome::Opened(p) => ProjectOutcome::Open(p),
            ui::portal::PortalOutcome::Cancelled => ProjectOutcome::Cancelled,
            ui::portal::PortalOutcome::Failed(e) => ProjectOutcome::Failed(e),
        })
    }
}

/// Open the host chooser for a project file to load.
pub fn request_open_project() -> ProjectChannel {
    ProjectChannel {
        rx: PickRequest::open("Open project", "Open")
            .thread_name("video-editor-open-project")
            .filter(project_filter())
            .all_files()
            .send(),
        save: false,
    }
}

/// Open the host Save dialog for a project file.
///
/// Under `landlock = "strict"` this round-trip is not a nicety — the app has
/// no standing grant to write anywhere, so the grant that follows the file
/// the user picked is what *earns* the write. Exactly the boundary
/// [`request_import`] relies on for reads and
/// [`request_export_destination`] for the rendered video.
///
/// `suggested_name` seeds the filename field with the project's name plus
/// the project extension, so the user is not made to invent either.
pub fn request_save_project(suggested_name: &str) -> ProjectChannel {
    ProjectChannel {
        rx: PickRequest::save("Save project", "Save", suggested_name)
            .thread_name("video-editor-save-project")
            .filter(project_filter())
            .send(),
        save: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dialogs_are_configured_and_drain_their_channel() {
        // Case `the_project_dialogs_are_each_configured_for_their_own_job`.
        {
            let open = PickRequest::open("Open project", "Open")
                .filter(project_filter())
                .all_files();
            assert_eq!(open.title(), "Open project");
            assert!(!open.is_multiple(), "one project is opened at a time");
            assert_eq!(open.suggested_name(), None);
            // The escape hatch stays on Open: a project someone renamed away from
            // .qvp is still openable, and the header is what decides.
            assert_eq!(open.filter_names(), vec!["Video project", "All files"]);

            // Save deliberately has no "All files": a project is always written as
            // .qvp, and an extensionless save is a file the Open dialog then hides.
            let save = PickRequest::save("Save project", "Save", "Coastline_Doc_v3.qvp")
                .filter(project_filter());
            assert_eq!(save.suggested_name(), Some("Coastline_Doc_v3.qvp"));
            assert_eq!(save.filter_names(), vec!["Video project"]);
        }
        // Case `the_project_channel_remembers_which_flow_asked_for_the_path`.
        {
            // The same chosen path means "load this" for one flow and "write
            // here" for the other. Confusing them destroys the file the user
            // meant to read, so the channel is what keeps them apart.
            let p = std::path::PathBuf::from("/home/alice/Holiday.qvp");
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(p.clone()));
            assert_eq!(
                request_open_project().try_recv(),
                Ok(ProjectOutcome::Open(p.clone())),
            );
            recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(p.clone()));
            assert_eq!(
                request_save_project("Holiday.qvp").try_recv(),
                Ok(ProjectOutcome::Save(p)),
            );
            assert_eq!(
                request_open_project().try_recv(),
                Ok(ProjectOutcome::Cancelled),
            );
            let titles: Vec<String> = recorder
                .pick_requests()
                .iter()
                .map(|r| r.title().to_string())
                .collect();
            assert_eq!(titles, ["Open project", "Save project", "Open project"]);
        }
        // Case `an_empty_project_channel_reports_empty_rather_than_blocking`.
        {
            let (_tx, rx) = std::sync::mpsc::channel::<ui::portal::PortalOutcome>();
            let ch = ProjectChannel { rx, save: false };
            assert_eq!(ch.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        }
    }

    #[test]
    fn export_and_import_dialogs_offer_formats_and_drain_cancellation() {
        // Case `export_destination_is_a_save_dialog_seeded_with_the_project_name`.
        {
            let r = PickRequest::save("Export video", "Export", "Coastline_Doc_v3.mp4")
                .filter(container_filter("mp4"))
                .all_files();
            assert_eq!(r.title(), "Export video");
            assert!(!r.is_multiple(), "an export writes exactly one file");
            assert_eq!(
                r.suggested_name(),
                Some("Coastline_Doc_v3.mp4"),
                "the filename field is seeded so the user is not made to invent a name",
            );
            assert_eq!(r.filter_names(), vec!["MP4 video", "All files"]);
        }
        // Case `each_container_offers_its_own_extension`.
        {
            // The dialog must not invite saving an H.264 stream as .webm — the
            // filter follows the container the encode chain will actually mux.
            assert_eq!(container_filter("mp4").name(), "MP4 video");
            assert_eq!(container_filter("mov").name(), "QuickTime video");
            assert_eq!(container_filter("webm").name(), "WebM video");
        }
        // Case `a_cancelled_export_dialog_drains_like_an_import_does`.
        {
            let (services, recorder) = ui::host::HostServices::recording();
            let rx = ui::host::scoped(&services, || {
                request_export_destination("Coastline_Doc_v3.mp4", "mp4")
            });
            assert_eq!(rx.try_recv(), Ok(ExportDestOutcome::Cancelled));
            assert_eq!(recorder.pick_requests()[0].title(), "Export video");
        }
        // Case `import_is_multi_select_with_media_then_an_escape_hatch`.
        {
            let r = PickRequest::open_multiple("Import media", "Import")
                .filter(media_filter())
                .all_files();
            assert_eq!(r.title(), "Import media");
            assert!(r.is_multiple(), "a user may import many clips at once");
            assert_eq!(r.filter_names(), vec!["Media", "All files"]);
        }
        // Case `a_cancelled_import_drains_through_the_channel_the_worker_feeds`.
        {
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick_multiple(ImportOutcome::Cancelled);
            let rx = ui::host::scoped(&services, request_import);
            assert_eq!(rx.try_recv(), Ok(ImportOutcome::Cancelled));
            assert!(recorder.pick_requests()[0].is_multiple());
        }
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    /// The chooser must offer everything the editor can actually open.
    ///
    /// It used to name five video extensions against the probe's twenty-eight,
    /// so a `.flac`, an `.avi` or a `.heic` was filtered out of the picker and
    /// the user had no way to learn the editor would have taken it. Deriving
    /// the globs from the probe's own lists is what keeps the two honest; this
    /// asserts they have not drifted apart again.
    #[test]
    fn the_chooser_offers_every_format_the_probe_accepts() {
        let f = media_filter();
        let globs: Vec<&str> = f.globs().iter().map(String::as_str).collect();
        for ext in crate::probe::VIDEO_EXTS
            .iter()
            .chain(crate::probe::AUDIO_EXTS)
            .chain(crate::probe::IMAGE_EXTS)
        {
            let want = format!("*.{ext}");
            assert!(
                globs.contains(&want.as_str()),
                "the probe reads {ext} but the chooser hides it: {globs:?}",
            );
        }
        // And a few by name, so the test says something even if the lists are
        // ever restructured.
        for want in ["*.flac", "*.avi", "*.heic", "*.opus", "*.mkv"] {
            assert!(globs.contains(&want), "missing {want}");
        }
    }
}
