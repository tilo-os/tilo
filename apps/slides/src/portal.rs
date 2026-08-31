//! Presentation open/save and image insertion through the host file chooser.
//!
//! The portal client — worker thread, Tokio runtime, ashpd round-trip, and the
//! URI→path rule that makes this the file-access security boundary — lives in
//! [`ui::portal`]. All this module carries is the filter sets, the labels this
//! app wants on each dialog, and the open-vs-save distinction the deck flow
//! needs when the chosen path comes back.

use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;

use ui::portal::{FileFilter, PickRequest};

pub use ui::portal::{PortalChannel, PortalOutcome, inject_for_test};

fn image_filter() -> FileFilter {
    FileFilter::new("Images")
        .glob("*.png")
        .glob("*.jpg")
        .glob("*.jpeg")
        .glob("*.gif")
        .glob("*.webp")
        .glob("*.bmp")
        .glob("*.svg")
}

fn presentation_filter() -> FileFilter {
    FileFilter::new("Presentations").glob("*.odp")
}

/// Open the host image chooser to insert a picture into the current slide.
pub fn request_open() -> PortalChannel {
    PickRequest::open("Insert image", "Insert")
        .thread_name("slides-portal-open")
        .filter(image_filter())
        .all_files()
        .send()
}

/// The result of an `OpenDocument` Presentation open/save interaction. The two
/// success variants are the same path with different meanings: one to load
/// from, one to write to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocOutcome {
    Open(PathBuf),
    Save(PathBuf),
    Cancelled,
    Failed(String),
}

/// Receiver the UI thread drains for an in-flight `.odp` open/save.
///
/// Wraps the shared single-path channel and remembers which of the two flows
/// asked for it, so the drain site still gets `Open`/`Save` without the shared
/// portal having to know what a deck is.
pub struct DocPortalChannel {
    rx: PortalChannel,
    save: bool,
}

impl DocPortalChannel {
    /// Non-blocking drain, mirroring [`std::sync::mpsc::Receiver::try_recv`].
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError::Empty`] while the host dialog is still open,
    /// or [`TryRecvError::Disconnected`] if the portal worker thread ended
    /// without sending an outcome.
    pub fn try_recv(&self) -> Result<DocOutcome, TryRecvError> {
        self.rx.try_recv().map(|outcome| match outcome {
            PortalOutcome::Opened(p) if self.save => DocOutcome::Save(p),
            PortalOutcome::Opened(p) => DocOutcome::Open(p),
            PortalOutcome::Cancelled => DocOutcome::Cancelled,
            PortalOutcome::Failed(e) => DocOutcome::Failed(e),
        })
    }
}

/// Open the host file chooser for a presentation to load. The chosen path
/// arrives over the returned channel; the UI thread reads + decodes the file
/// when it drains it.
pub fn request_open_document() -> DocPortalChannel {
    DocPortalChannel {
        rx: PickRequest::open("Open presentation", "Open")
            .thread_name("slides-portal-open-doc")
            .filter(presentation_filter())
            .all_files()
            .send(),
        save: false,
    }
}

/// Open the host Save dialog for a presentation. `suggested_name` seeds the
/// filename field; the UI thread serialises + writes the deck when it drains
/// the chosen destination.
pub fn request_save_document(suggested_name: &str) -> DocPortalChannel {
    DocPortalChannel {
        rx: PickRequest::save("Save presentation", "Save", suggested_name)
            .thread_name("slides-portal-save-doc")
            .filter(presentation_filter())
            .send(),
        save: true,
    }
}

/// Test-only: deliver a `.odp` `outcome` on the exact per-frame drain path the
/// real worker feeds.
#[cfg(test)]
pub fn inject_doc_for_test(outcome: DocOutcome) -> DocPortalChannel {
    let (single, save) = match outcome {
        DocOutcome::Open(p) => (PortalOutcome::Opened(p), false),
        DocOutcome::Save(p) => (PortalOutcome::Opened(p), true),
        DocOutcome::Cancelled => (PortalOutcome::Cancelled, false),
        DocOutcome::Failed(e) => (PortalOutcome::Failed(e), false),
    };
    DocPortalChannel {
        rx: inject_for_test(single),
        save,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_dialog_is_configured_for_its_own_job() {
        let img = PickRequest::open("Insert image", "Insert")
            .filter(image_filter())
            .all_files();
        assert_eq!(img.title(), "Insert image");
        assert_eq!(img.filter_names(), vec!["Images", "All files"]);

        let open = PickRequest::open("Open presentation", "Open")
            .filter(presentation_filter())
            .all_files();
        assert_eq!(open.filter_names(), vec!["Presentations", "All files"]);
        assert_eq!(open.suggested_name(), None);

        // The Save dialog deliberately has no "All files" escape hatch: a deck
        // is always written as .odp.
        let save = PickRequest::save("Save presentation", "Save", "deck.odp")
            .filter(presentation_filter());
        assert_eq!(save.suggested_name(), Some("deck.odp"));
        assert_eq!(save.filter_names(), vec!["Presentations"]);
    }

    #[test]
    fn the_doc_channel_remembers_which_flow_asked_for_the_path() {
        // The same chosen path means "load this" for one flow and "write here"
        // for the other; the channel is what carries that difference.
        let p = PathBuf::from("/home/alice/deck.odp");
        let opened = inject_doc_for_test(DocOutcome::Open(p.clone()));
        assert_eq!(opened.try_recv(), Ok(DocOutcome::Open(p.clone())));

        let saved = inject_doc_for_test(DocOutcome::Save(p.clone()));
        assert_eq!(saved.try_recv(), Ok(DocOutcome::Save(p)));

        let cancelled = inject_doc_for_test(DocOutcome::Cancelled);
        assert_eq!(cancelled.try_recv(), Ok(DocOutcome::Cancelled));
    }

    #[test]
    fn an_empty_doc_channel_reports_empty_rather_than_blocking() {
        let (_tx, rx) = std::sync::mpsc::channel::<PortalOutcome>();
        let ch = DocPortalChannel { rx, save: false };
        assert_eq!(ch.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn inject_drives_the_same_drain_path_the_worker_feeds() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));
    }
}
