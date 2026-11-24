//! The installer's resource: the reviewed image, the images reviewed before it and the install
//! profile, kept in a file of the person's data directory.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use tilo_resource::replica::Replica;
use tilo_resource::resource::Resource;
use tilo_resource::{Edit, Invoke};
use ui::tilo_actions::{ActionOutput, EncodedAction, ResourceSpec};

use crate::resource::Imager;

struct Kept {
    replica: Replica<Imager>,
    file: Option<PathBuf>,
    revision: u64,
}

/// The one resource every window of a run shows and edits.
#[derive(Clone)]
pub(crate) struct Store(Rc<RefCell<Kept>>);

impl Store {
    /// Whether this resource is kept on this computer at all: a live run keeps it in the
    /// person's data directory, a shot's or a test's lasts only while the run does.
    pub(crate) fn keeps(&self) -> bool {
        self.0.borrow().file.is_some()
    }

    /// The resource as the person last left it on this computer.
    ///
    /// A file that does not read as the resource is not trusted: the installer starts from an
    /// empty resource and says so on its error output.
    pub(crate) fn open() -> Self {
        let Some(file) = tilo_paths::data_dir()
            .map(|dir| dir.join(crate::resource::APP_ID).join("imager.automerge"))
        else {
            return Self::in_memory();
        };
        let replica = match std::fs::read(&file) {
            Ok(bytes) => Replica::load(&bytes).unwrap_or_else(|error| {
                eprintln!(
                    "imager: {} does not hold the installer's resource ({error}); starting over",
                    file.display()
                );
                Replica::new()
            }),
            Err(_) => Replica::new(),
        };
        Self::kept(replica, Some(file))
    }

    /// A resource kept only while this run lasts: a shot's, a test's.
    pub(crate) fn in_memory() -> Self {
        Self::kept(Replica::new(), None)
    }

    fn kept(replica: Replica<Imager>, file: Option<PathBuf>) -> Self {
        Self(Rc::new(RefCell::new(Kept {
            replica,
            file,
            revision: 0,
        })))
    }

    /// Changes every time the resource does.
    pub(crate) fn revision(&self) -> u64 {
        self.0.borrow().revision
    }

    #[expect(
        clippy::expect_used,
        reason = "the replica holds the resource it was made with; one it cannot read is a bug"
    )]
    pub(crate) fn state(&self) -> Imager {
        self.0
            .borrow()
            .replica
            .state()
            .expect("the installer's resource reads")
    }

    #[expect(
        clippy::expect_used,
        reason = "the replica holds the resource it was made with; one it cannot read is a bug"
    )]
    pub(crate) fn published(&self) -> ResourceSpec {
        self.0
            .borrow()
            .replica
            .published()
            .expect("the installer's resource reads")
    }

    /// Run one of the resource's actions, as a control, a console caller or a scenario does.
    ///
    /// # Errors
    ///
    /// Why the resource refused it.
    pub(crate) fn apply(&self, invoke: impl Into<Invoke<Imager>>) -> Result<(), String> {
        self.0
            .borrow_mut()
            .replica
            .apply(invoke.into())
            .map_err(|error| error.to_string())?;
        self.changed();
        Ok(())
    }

    /// Read the stored document itself.
    #[cfg(test)]
    pub(crate) fn read_document<T>(&self, read: impl FnOnce(&tilo_resource::Doc) -> T) -> T {
        read(self.0.borrow().replica.doc())
    }

    /// Merge an edit another member made of the resource, as sync delivers it.
    #[cfg(test)]
    pub(crate) fn merge_peer_edit(&self, edit: impl FnOnce(&mut tilo_resource::Doc)) {
        let mut kept = self.0.borrow_mut();
        let mut peer = kept.replica.fork();
        edit(peer.doc_mut());
        kept.replica
            .sync_with(&mut peer)
            .expect("a peer's edit merges");
        drop(kept);
        self.changed();
    }

    /// Edit a field of the resource as the person types into its control: the profile's Wi-Fi
    /// name, the device's label. Going back a page is the form's own state, so an edit keeps no
    /// history of its own.
    ///
    /// # Errors
    ///
    /// Why the resource refused it.
    pub(crate) fn edit(&self, edit: &Edit<Imager>) -> Result<(), String> {
        self.apply(Invoke::edit(edit).map_err(|error| error.to_string())?)
    }

    /// The type the resource's own actions travel under.
    pub(crate) fn action_type(&self) -> &'static str {
        Imager::ACTION_TYPE
    }

    /// Run an admitted invocation of one of the resource's own actions.
    pub(crate) fn apply_encoded(&self, encoded: &EncodedAction) -> Result<ActionOutput, String> {
        let action: <Imager as Resource>::Action =
            serde_json::from_slice(&encoded.payload).map_err(|error| error.to_string())?;
        self.0
            .borrow_mut()
            .replica
            .apply_action(&action)
            .map_err(|error| error.to_string())?;
        self.changed();
        Ok(ActionOutput::default())
    }

    fn changed(&self) {
        let mut kept = self.0.borrow_mut();
        kept.revision = kept.revision.wrapping_add(1);
        let Some(file) = kept.file.clone() else {
            return;
        };
        let bytes = kept.replica.save();
        drop(kept);
        let written = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| tilo_files::native::atomic::write(&file, &bytes));
        if let Err(error) = written {
            eprintln!("imager: could not keep {}: {error}", file.display());
        }
    }
}
