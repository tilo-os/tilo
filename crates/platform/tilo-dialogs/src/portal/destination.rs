//! Saving where the person chooses, chosen now and written later: for an app
//! that has to produce the bytes after the choice (an export it renders, a
//! document a worker flattens). The choice is made inside the person's
//! gesture; the write follows whenever the bytes are ready. On the web the
//! destination is the browser's own Save dialog where it has one, else a
//! download; elsewhere it is the path the host chooser answered. The app
//! writes through [`SaveDestination::write`] either way, and can keep the
//! destination to write there again on the next Save without asking.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvError, TryRecvError};

use super::{PortalChannel, PortalOutcome, SaveChannel, SaveOutcome};

/// Where a save goes. Dropping it unwritten saves nothing; kept, it takes
/// every later save of the same document without asking again.
#[derive(Clone)]
pub struct SaveDestination {
    name: String,
    target: Target,
}

/// Writes a kept destination's bytes.
type Writer = Arc<dyn Fn(Vec<u8>) -> SaveChannel + Send + Sync>;

#[derive(Clone)]
enum Target {
    Path(PathBuf),
    /// A destination the host keeps (a browser page's Save dialog file or a
    /// download), written by the host's writer.
    Kept(Writer),
}

impl std::fmt::Debug for SaveDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let target = match &self.target {
            Target::Path(path) => format!("{}", path.display()),
            Target::Kept(_) => "kept by the host".to_owned(),
        };
        f.debug_struct("SaveDestination")
            .field("name", &self.name)
            .field("target", &target)
            .finish()
    }
}

impl SaveDestination {
    /// The destination at `path` on this device.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            name: super::file_name_of(&path),
            target: Target::Path(path),
        }
    }

    /// A destination the host keeps and the app cannot reach by path, named
    /// `name`: [`write`](Self::write) hands the bytes to `write`, once per
    /// save.
    pub fn kept(
        name: impl Into<String>,
        write: impl Fn(Vec<u8>) -> SaveChannel + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            target: Target::Kept(Arc::new(write)),
        }
    }

    /// The file name the person sees.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where the saved file is on this device, when it has a path: to save
    /// there again in place, never for showing. `None` on the web.
    pub fn location(&self) -> Option<&Path> {
        match &self.target {
            Target::Path(path) => Some(path),
            Target::Kept(_) => None,
        }
    }

    /// Write `bytes` there. A local destination is replaced atomically
    /// ([`super::replace_saved`]) on background work; a browser page writes
    /// through its Save dialog's file (again on every save) or downloads.
    pub fn write(&self, bytes: Vec<u8>) -> SaveChannel {
        match &self.target {
            Target::Path(path) => write_path(path.clone(), bytes),
            Target::Kept(write) => write(bytes),
        }
    }
}

fn answered<T>(outcome: T) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(outcome);
    rx
}

fn write_path(path: PathBuf, bytes: Vec<u8>) -> SaveChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let refused = tx.clone();
    let spawned =
        tilo_rt::background::Background::current().spawn("tilo-portal-write", move || {
            let _ = tx.send(match super::replace_saved(&path, &bytes) {
                Ok(()) => SaveOutcome::Saved,
                Err(error) => SaveOutcome::Failed(format!("{}: {error}", path.display())),
            });
        });
    if spawned.is_err() {
        let _ = refused.send(SaveOutcome::Failed(
            "this host runs no background work".to_owned(),
        ));
    }
    rx
}

/// The result of [`PickRequest::send_save_destination`](super::PickRequest::send_save_destination).
#[derive(Debug)]
pub enum DestinationOutcome {
    Chosen(SaveDestination),
    Cancelled,
    Failed(String),
}

/// A pending [`DestinationOutcome`], drained by the UI thread like the other
/// chooser channels.
#[derive(Debug)]
pub struct DestinationChannel(Pending);

#[derive(Debug)]
enum Pending {
    /// A host chooser that answers Save with a path.
    Path(PortalChannel),
    Direct(Receiver<DestinationOutcome>),
}

impl DestinationChannel {
    /// Answered by a path chooser's channel.
    pub fn from_path(channel: PortalChannel) -> Self {
        Self(Pending::Path(channel))
    }

    /// Answered directly by `channel`.
    pub fn from_outcomes(channel: Receiver<DestinationOutcome>) -> Self {
        Self(Pending::Direct(channel))
    }

    pub fn answered(outcome: DestinationOutcome) -> Self {
        Self::from_outcomes(answered(outcome))
    }

    /// The outcome, if the chooser has answered.
    ///
    /// # Errors
    ///
    /// Not answered yet, or the chooser went away without answering.
    pub fn try_recv(&self) -> Result<DestinationOutcome, TryRecvError> {
        match &self.0 {
            Pending::Path(channel) => channel.try_recv().map(from_path),
            Pending::Direct(channel) => channel.try_recv(),
        }
    }

    /// Wait for the outcome. Only for tests and worker threads.
    ///
    /// # Errors
    ///
    /// The chooser went away without answering.
    pub fn recv(&self) -> Result<DestinationOutcome, RecvError> {
        match &self.0 {
            Pending::Path(channel) => channel.recv().map(from_path),
            Pending::Direct(channel) => channel.recv(),
        }
    }
}

fn from_path(outcome: PortalOutcome) -> DestinationOutcome {
    match outcome {
        PortalOutcome::Opened(path) => DestinationOutcome::Chosen(SaveDestination::at(path)),
        PortalOutcome::Cancelled => DestinationOutcome::Cancelled,
        PortalOutcome::Failed(reason) => DestinationOutcome::Failed(reason),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_path_is_written_in_place_and_named_by_its_file() {
        let dir = std::env::temp_dir().join(format!("tilo-dest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.pdf");
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(PortalOutcome::Opened(path.clone())).unwrap();
        let DestinationOutcome::Chosen(destination) =
            DestinationChannel::from_path(rx).try_recv().unwrap()
        else {
            panic!("a path was chosen");
        };
        assert_eq!(destination.name(), "report.pdf");
        assert_eq!(destination.location(), Some(path.as_path()));
        let work = tilo_rt::background::Pumped::new();
        let _work = work.background().enter();
        let saved = destination.write(b"%PDF".to_vec());
        work.run_pending();
        assert_eq!(saved.recv().unwrap(), SaveOutcome::Saved);
        assert_eq!(std::fs::read(&path).unwrap(), b"%PDF");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_kept_destination_hands_its_bytes_to_the_host() {
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let into = std::sync::Arc::clone(&written);
        let destination = SaveDestination::kept("notes.pdf", move |bytes| {
            into.lock().unwrap().extend(bytes);
            answered(SaveOutcome::Saved)
        });
        assert_eq!(destination.name(), "notes.pdf");
        assert_eq!(destination.location(), None, "no path to save in place");
        assert_eq!(
            destination.write(b"%PDF".to_vec()).recv().unwrap(),
            SaveOutcome::Saved
        );
        assert_eq!(*written.lock().unwrap(), b"%PDF");
        assert_eq!(
            destination.write(b"!".to_vec()).recv().unwrap(),
            SaveOutcome::Saved,
            "a kept destination takes the next save too"
        );
        assert_eq!(*written.lock().unwrap(), b"%PDF!");
    }

    #[test]
    fn cancelling_or_failing_the_chooser_chooses_nothing() {
        for (outcome, cancelled) in [
            (PortalOutcome::Cancelled, true),
            (PortalOutcome::Failed("gone".into()), false),
        ] {
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(outcome).unwrap();
            match DestinationChannel::from_path(rx).recv().unwrap() {
                DestinationOutcome::Cancelled => assert!(cancelled),
                DestinationOutcome::Failed(reason) => assert_eq!(reason, "gone"),
                DestinationOutcome::Chosen(_) => panic!("nothing was chosen"),
            }
        }
    }
}
