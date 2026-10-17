//! Application controller: native work is delegated; personal edits stay on the view.
use super::Status;
use crate::history::BrowsingHistory;
use crate::personal_bookmarks::{PersonalBookmarkAction, PersonalBookmarks};
use browser_continuity::handoff::{self as backend, Captured, Completion, Prepared, Source};
#[cfg(all(test, target_os = "linux"))]
use browser_continuity::{Plan, handoff::prepare_given};
#[cfg(all(test, target_os = "linux"))]
use std::fs::File;
use std::io;
use std::sync::Arc;
#[cfg(all(test, target_os = "linux"))]
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

fn failed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Browser continuity could not be verified",
    )
}
trait ApplyCaptured {
    fn apply(
        &self,
        bookmarks: &ui::framework::PersonRecord<PersonalBookmarks>,
        history: &ui::framework::PersonRecord<BrowsingHistory>,
    ) -> io::Result<()>;
}
impl ApplyCaptured for Captured {
    fn apply(
        &self,
        bookmarks: &ui::framework::PersonRecord<PersonalBookmarks>,
        history: &ui::framework::PersonRecord<BrowsingHistory>,
    ) -> io::Result<()> {
        for capture in &self.profiles {
            if let Some(bundle) = &capture.bookmarks {
                bookmarks
                    .apply(&PersonalBookmarkAction::Import {
                        bundle: bundle.clone(),
                    })
                    .map_err(|_| failed())?;
            }
            if let Some(bundle) = &capture.history {
                history
                    .invoke(
                        &BrowsingHistory::IMPORT_HISTORY_BUNDLE
                            .with(serde_json::to_string(bundle).map_err(|_| failed())?),
                    )
                    .map_err(|_| failed())?;
            }
        }
        Ok(())
    }
}
/// Native work remains on its worker; only typed personal-record edits run on the view.
pub(crate) struct Controller {
    person: [u8; 32],
    source: Source,
    waker: ui::WakeHandle,
    receiver: Option<std::sync::mpsc::Receiver<Completion>>,
    ready: Option<Prepared>,
    status: Status,
}
impl Controller {
    pub(crate) fn start(
        person: [u8; 32],
        files: ui::storage::AppFiles,
        waker: ui::WakeHandle,
    ) -> Self {
        let mut controller = Self {
            person,
            source: Source::sandbox(Arc::new(move || {
                files
                    .paths()
                    .state()
                    .native_path()
                    .map(std::path::Path::to_path_buf)
                    .map_err(|_| failed())
            })),
            waker,
            receiver: None,
            ready: None,
            status: Status::Absent,
        };
        controller.retry();
        controller
    }
    pub(crate) fn status(&self) -> Status {
        self.status
    }
    pub(crate) fn retry(&mut self) -> bool {
        if self.receiver.is_some() {
            return false;
        }
        if !backend::supported() {
            return false;
        }
        let waker = self.waker.clone();
        self.status = Status::Loading;
        self.ready = None;
        self.receiver = Some(backend::prepare(
            self.source.clone(),
            self.person,
            Arc::new(move || waker.wake()),
        ));
        true
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn load_granted(&mut self, root: File, state: StorageDirectory) {
        assert!(!matches!(self.status, Status::Ready | Status::Publishing));
        self.source = Source::granted(root, state);
        // Retire only the previous acquisition result. Persona mutations still
        // run exclusively in poll; this hook cannot detach an applying job.
        self.receiver = None;
        assert!(self.retry());
    }
    pub(crate) fn poll(
        &mut self,
        bookmarks: &ui::framework::PersonRecord<PersonalBookmarks>,
        history: &ui::framework::PersonRecord<BrowsingHistory>,
    ) -> bool {
        let Some(receiver) = &self.receiver else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(value) => value,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(_) => Completion::Published(Err(failed())),
        };
        self.receiver = None;
        match result {
            Completion::Prepared(Ok(None)) => self.status = Status::Absent,
            Completion::Prepared(Ok(Some(prepared))) if prepared.complete => {
                self.status = Status::Complete;
            }
            Completion::Prepared(Ok(Some(prepared))) => {
                self.status = Status::Ready;
                if prepared.capture.apply(bookmarks, history).is_err() {
                    self.ready = Some(prepared);
                    self.status = Status::Failed;
                    return true;
                }
                let waker = self.waker.clone();
                self.status = Status::Publishing;
                self.receiver = Some(backend::publish(prepared, Arc::new(move || waker.wake())));
            }
            Completion::Published(Ok(())) => self.status = Status::Complete,
            Completion::Prepared(Err(_)) | Completion::Published(Err(_)) => {
                self.status = Status::Failed;
            }
        }
        true
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "tests.rs"]
mod tests;
