//! The macOS backend: `NSOpenPanel` and `NSSavePanel` as modeless windows on
//! the main thread, and `NSWorkspace` for opening URIs and files.
//!
//! A request may come from any thread. It is queued onto the main dispatch
//! queue, which the app's run loop drains; the panel's completion handler then
//! sends the outcome and wakes the event loop that asked. Nothing blocks, and
//! no nested modal loop runs under the windowing library.

use std::sync::mpsc::{Receiver, Sender};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSModalResponse, NSModalResponseOK, NSOpenPanel, NSSavePanel, NSWorkspace};
use objc2_foundation::{NSArray, NSString, NSURL};

use crate::PortalFailure;
use crate::portal::{
    FileFilter, LaunchChannel, LaunchKind, LaunchOutcome, LaunchRequest, MultiChannel,
    OpenFileChannel, OpenFileOutcome, PickKind, PickRequest, PortalChannel, map_uri, map_uris,
    open_selected_uri,
};

/// The panel [`crate::FilePicker`].
pub(crate) struct PanelFilePicker;

impl crate::FilePicker for PanelFilePicker {
    fn pick(&self, request: PickRequest) -> PortalChannel {
        choose(request, |picked| match picked {
            Ok(uris) => map_uri(&uris),
            Err(error) => error.into_single(),
        })
    }

    fn pick_multiple(&self, request: PickRequest) -> MultiChannel {
        choose(request, |picked| match picked {
            Ok(uris) => map_uris(&uris),
            Err(error) => error.into_multi(),
        })
    }

    fn open_file(&self, request: PickRequest) -> OpenFileChannel {
        if !matches!(request.kind, PickKind::Open { multiple: false }) {
            return answered(OpenFileOutcome::Failed(
                "an already-open file request must be a single-file Open chooser".to_string(),
            ));
        }
        choose(request, |picked| match picked {
            // Open the descriptor at once, so no later frame can turn the
            // accepted path into a reopen of something else.
            Ok(uris) => uris
                .first()
                .map_or(OpenFileOutcome::Cancelled, open_selected_uri),
            Err(error) => error.into_open_file(),
        })
    }

    fn launch(&self, request: LaunchRequest) -> LaunchChannel {
        answered(launch(&request))
    }
}

fn answered<T>(outcome: T) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(outcome);
    rx
}

/// Show the panel `request` describes on the main thread and deliver
/// `finish(picked URIs)` over the returned channel.
fn choose<T: Send + 'static>(
    request: PickRequest,
    finish: impl FnOnce(Result<Vec<url::Url>, PortalFailure>) -> T + Send + 'static,
) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let reply = Reply {
        tx,
        wake: tilo_rt::wake::WakeHandle::new(),
    };
    dispatch2::DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            reply.send(finish(Err(PortalFailure::Failed(
                "the file panel must run on the main thread".into(),
            ))));
            return;
        };
        show(mtm, &request, move |picked| reply.send(finish(picked)));
    });
    rx
}

/// Where a panel's outcome goes, and the event loop it wakes.
struct Reply<T> {
    tx: Sender<T>,
    wake: tilo_rt::wake::WakeHandle,
}

impl<T> Reply<T> {
    fn send(self, outcome: T) {
        // The UI may have moved on (window closed); ignore a dead channel.
        if self.tx.send(outcome).is_ok() {
            self.wake.wake();
        }
    }
}

fn show(
    mtm: MainThreadMarker,
    request: &PickRequest,
    done: impl FnOnce(Result<Vec<url::Url>, PortalFailure>) + 'static,
) {
    let panel = panel(mtm, request);
    let is_open = !matches!(request.kind, PickKind::Save { .. });
    let held = panel.clone();
    let done = std::cell::Cell::new(Some(done));
    let handler = RcBlock::new(move |response: NSModalResponse| {
        let Some(done) = done.take() else {
            return;
        };
        done(picked(&held, is_open, response));
    });
    panel.beginWithCompletionHandler(&handler);
}

/// Show `request`'s panel modally in a process of its own, which becomes a
/// foreground app for as long as it shows: the runtime's chooser for a
/// confined app. The picked local paths, or `None` when cancelled.
pub(crate) fn show_modal(request: &PickRequest) -> Result<Option<Vec<std::path::PathBuf>>, String> {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    let mtm = MainThreadMarker::new().ok_or("the file panel must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    #[allow(
        deprecated,
        reason = "a helper process has no other app to cooperate with"
    )]
    app.activateIgnoringOtherApps(true);
    let panel = panel(mtm, request);
    let response = panel.runModal();
    match picked(
        &panel,
        !matches!(request.kind, PickKind::Save { .. }),
        response,
    ) {
        Ok(uris) => uris
            .iter()
            .map(|uri| {
                crate::portal::to_local_path(uri).ok_or_else(|| format!("not a local file: {uri}"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Err(PortalFailure::Cancelled) => Ok(None),
        Err(PortalFailure::Failed(reason)) => Err(reason),
    }
}

/// Ask the person a yes-or-no question in an alert of its own, modally, in a
/// process of its own that becomes a foreground app while it asks: the
/// runtime's consent prompt for a confined app. The refusing button comes
/// first, so Return refuses. `true` when they chose `allow`.
///
/// # Errors
///
/// Not on the main thread.
pub(crate) fn ask_modal(
    message: &str,
    detail: &str,
    allow: &str,
    refuse: &str,
) -> Result<bool, String> {
    use objc2_app_kit::{
        NSAlert, NSAlertSecondButtonReturn, NSApplication, NSApplicationActivationPolicy,
    };
    let mtm = MainThreadMarker::new().ok_or("the alert must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    #[allow(
        deprecated,
        reason = "a helper process has no other app to cooperate with"
    )]
    app.activateIgnoringOtherApps(true);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(detail));
    alert.addButtonWithTitle(&NSString::from_str(refuse));
    alert.addButtonWithTitle(&NSString::from_str(allow));
    Ok(alert.runModal() == NSAlertSecondButtonReturn)
}

/// What `panel` answered with `response`.
fn picked(
    panel: &NSSavePanel,
    is_open: bool,
    response: NSModalResponse,
) -> Result<Vec<url::Url>, PortalFailure> {
    if response != NSModalResponseOK {
        return Err(PortalFailure::Cancelled);
    }
    let urls: Vec<Retained<NSURL>> = if is_open {
        match panel.downcast_ref::<NSOpenPanel>() {
            Some(open) => open.URLs().to_vec(),
            None => Vec::new(),
        }
    } else {
        panel.URL().into_iter().collect()
    };
    urls.iter()
        .filter_map(|url| url.absoluteString())
        .map(|text| url::Url::parse(&text.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            PortalFailure::Failed(format!("the panel returned a malformed URL: {error}"))
        })
}

/// The panel `request` describes.
fn panel(mtm: MainThreadMarker, request: &PickRequest) -> Retained<NSSavePanel> {
    let title = NSString::from_str(&request.title);
    let prompt = NSString::from_str(&request.accept_label);
    let panel: Retained<NSSavePanel> = match &request.kind {
        PickKind::Open { multiple } => {
            let open = NSOpenPanel::openPanel(mtm);
            open.setCanChooseFiles(true);
            open.setCanChooseDirectories(false);
            open.setAllowsMultipleSelection(*multiple);
            Retained::into_super(open)
        }
        PickKind::Directory => {
            let open = NSOpenPanel::openPanel(mtm);
            open.setCanChooseFiles(false);
            open.setCanChooseDirectories(true);
            open.setAllowsMultipleSelection(false);
            open.setCanCreateDirectories(true);
            Retained::into_super(open)
        }
        PickKind::Save { suggested_name } => {
            let save = NSSavePanel::savePanel(mtm);
            save.setNameFieldStringValue(&NSString::from_str(suggested_name));
            save.setCanCreateDirectories(true);
            save
        }
    };
    panel.setTitle(Some(&title));
    panel.setMessage(Some(&title));
    panel.setPrompt(Some(&prompt));
    if let Some(extensions) = extensions(&request.filters) {
        let types: Vec<Retained<NSString>> = extensions
            .iter()
            .map(|ext| NSString::from_str(ext))
            .collect();
        #[allow(
            deprecated,
            reason = "content types need UTType lookups; extensions say the same for pickers"
        )]
        panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&types)));
    }
    panel
}

/// The extensions an `NSSavePanel` may restrict to, or `None` for any file:
/// no filter, an "All files" filter, or a glob that is not a plain `*.ext`.
/// A narrower chooser than the reader would hide files the app could open.
fn extensions(filters: &[FileFilter]) -> Option<Vec<String>> {
    let mut extensions = Vec::new();
    for filter in filters {
        if filter.globs.is_empty() && !filter.mimetypes.is_empty() {
            // A MIME-only filter needs a type lookup; admit everything.
            return None;
        }
        for glob in &filter.globs {
            let ext = glob.strip_prefix("*.")?;
            if ext.is_empty() || ext.contains(['*', '?', '[', '/']) {
                return None;
            }
            extensions.push(ext.to_owned());
        }
    }
    extensions.dedup();
    (!extensions.is_empty()).then_some(extensions)
}

fn launch(request: &LaunchRequest) -> LaunchOutcome {
    if !request.path.is_absolute() {
        return LaunchOutcome::Failed("desktop-open path must be absolute".into());
    }
    let url = NSURL::fileURLWithPath(&NSString::from_str(&request.path.to_string_lossy()));
    let workspace = NSWorkspace::sharedWorkspace();
    match request.kind {
        LaunchKind::Open => {
            if workspace.openURL(&url) {
                LaunchOutcome::Opened
            } else {
                LaunchOutcome::Failed(format!("no app opens {}", request.path.display()))
            }
        }
        LaunchKind::Reveal => {
            workspace.activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
            LaunchOutcome::Opened
        }
    }
}

/// Hand `uri` to the app Launch Services registers for its scheme.
pub(crate) fn open_uri(uri: &str) -> std::io::Result<()> {
    let Some(url) = NSURL::URLWithString(&NSString::from_str(uri)) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("bad URI: {uri}"),
        ));
    };
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("no app opens {uri}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_restrict_to_plain_extensions_or_admit_everything() {
        let images = FileFilter::new("Images").glob("*.png").glob("*.jpg");
        assert_eq!(
            extensions(std::slice::from_ref(&images)),
            Some(vec!["png".to_owned(), "jpg".to_owned()])
        );
        assert_eq!(extensions(&[images, FileFilter::all_files()]), None);
        assert_eq!(extensions(&[]), None);
        assert_eq!(
            extensions(&[FileFilter::new("Text").mimetype("text/plain")]),
            None
        );
        assert_eq!(extensions(&[FileFilter::new("C").glob("*.[ch]")]), None);
    }

    #[test]
    fn a_relative_launch_fails_before_reaching_the_workspace() {
        assert!(matches!(
            launch(&LaunchRequest::open("relative/file.txt")),
            LaunchOutcome::Failed(reason) if reason.contains("absolute")
        ));
    }

    #[test]
    fn a_non_open_descriptor_request_fails_without_a_panel() {
        use crate::FilePicker as _;
        let outcome = PanelFilePicker
            .open_file(PickRequest::save("Save", "Save", "x"))
            .recv()
            .unwrap();
        assert!(matches!(outcome, OpenFileOutcome::Failed(_)));
    }
}
