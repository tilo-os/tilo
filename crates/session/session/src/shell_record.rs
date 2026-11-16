//! The shell's own record, kept as the person's real record: where its document lives on this
//! device, and how it reaches the person's other devices.
//!
//! The shell keeps the person's shell choices in its own record (`records: .own`): the style,
//! the bar and the dock, the strips' and the frames' settings, and the person's layout. That
//! record is the person's as any app's is, so it is kept where an app's own record of the person's
//! is kept: one document opened through `app_state` — the store `ui::record` opens a record
//! through — under the app's own person scope (`ui::record::person::PERSONAL_SCOPE`), in the
//! directory the launcher binds into every app as `TILO_PERSON_DIR`. What the shell's parts commit
//! is appended through the same `DocWriter` an app's record uses, off the frame
//! (`ShellRecord::follow`).
//!
//! Between the person's devices it travels on the person's own cluster: their store
//! (`crate::personal::Personal`, the `PersonalSync` the session already runs for their own
//! records) carries it, exactly as it carries every app's person record — what this device writes
//! is published there, and what another of their devices wrote is merged here
//! (`Shell::sync_record`).
//!
//! A document this build cannot read is never written over: the session keeps the shell's record
//! in memory for that session alone and says so (`ShellRecord::take_refusal`), so a person is
//! never shown a shell whose choices were quietly reset.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use automerge::{AutoCommit, ChangeHash};
use tilo_automerge::ApplyIncoming as _;

/// The app whose record this is: the shell package the session runs, which declares
/// `records: .own` (`apps/tl/shell/app.tl`).
pub(crate) const APP: &str = "so.tilo.shell";

/// Where the shell's record lives in this session, and what became of the one it kept.
pub(crate) struct ShellRecord {
    /// The document the shell's parts draw from and commit into: the record every part shares.
    doc: std::rc::Rc<RefCell<AutoCommit>>,
    /// The writer that appends what the parts commit, off the frame: `None` for a record this
    /// session keeps in memory only (no store to keep a copy in, or a document this build did
    /// not open).
    saver: RefCell<Option<app_state::DocWriter>>,
    /// Where the document is kept on this device, while it is.
    path: Option<PathBuf>,
    /// The record's id in its app, as the store and the person's other devices name it.
    id: String,
    /// Why the record on disk was not opened, once: the person is told, and is not told again.
    refused: RefCell<Option<String>>,
}

impl ShellRecord {
    /// The record kept for `person` when this device keeps state at `state` (`None` on a session
    /// that keeps nothing — a test's, a screenshot's — which draws the defaults and writes nothing),
    /// read back from what an earlier session kept on this device; an empty one on a device that
    /// never showed the shell, whose defaults this session's first part seeds by the package's
    /// migrations.
    ///
    /// The document is opened through the store an app's own record of the person's is opened
    /// through, at the same key and scope, so the shell's record is a record of the person's like
    /// any other: a person with no record yet gets the shell's defaults, and an existing one is
    /// never replaced, rewritten or reset.
    pub(crate) fn open(state: Option<&Path>, person: &str) -> Self {
        let id = record_id(person);
        // A session that keeps no state keeps nothing: it draws the shell's defaults and writes no
        // copy beside them, as every other thing the session keeps follows the state directory.
        let Some(store) = state.and_then(|_| person_store()) else {
            return Self::keeping(AutoCommit::new(), None, id);
        };
        let key = durable_store::StorageKey::document(
            APP,
            ui::record::person::PERSONAL_SCOPE,
            ui::record::DocumentId::parse(&id)
                .map_or_else(|| id.clone(), |parsed| parsed.storage_key()),
        );
        let mut opened = match app_state::DocStore::shared(store, key) {
            Ok(opened) => opened,
            Err(error) => return Self::refusing(error.to_string(), id),
        };
        let doc = match opened.load() {
            Ok(Some(doc)) => doc,
            Ok(None) => opened.new_document(),
            Err(error) => return Self::refusing(error.to_string(), id),
        };
        let path = Some(opened.document_path().to_path_buf());
        let mut saver = opened.writer();
        // What the document holds is what is already stored: a later request appends only what
        // follows it, as an app's record writer does.
        saver.seed(&doc);
        Self {
            doc: std::rc::Rc::new(RefCell::new(doc)),
            saver: RefCell::new(Some(saver)),
            path,
            id,
            refused: RefCell::new(None),
        }
    }

    /// `doc` as the record, kept at `path` when there is one: what [`Self::open`] builds once the
    /// document is had.
    fn keeping(doc: AutoCommit, path: Option<PathBuf>, id: String) -> Self {
        Self {
            doc: std::rc::Rc::new(RefCell::new(doc)),
            saver: RefCell::new(None),
            path,
            id,
            refused: RefCell::new(None),
        }
    }

    /// A record this build did not open, kept in memory for this session and never written over:
    /// the person's shell starts from the shell's defaults, and they are told why.
    fn refusing(reason: String, id: String) -> Self {
        tracing::error!(
            %reason,
            "the shell's record is not opened; the shell starts from the defaults this session"
        );
        Self {
            doc: std::rc::Rc::new(RefCell::new(AutoCommit::new())),
            saver: RefCell::new(None),
            path: None,
            id,
            refused: RefCell::new(Some(reason)),
        }
    }

    /// The record the shell's parts draw from and commit into.
    #[must_use]
    pub(crate) fn record(&self) -> std::rc::Rc<RefCell<AutoCommit>> {
        std::rc::Rc::clone(&self.doc)
    }

    /// The record's id in its app, as the store and the person's other devices name it.
    #[must_use]
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Where the document is kept on this device, while it is.
    #[must_use]
    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Why the record on disk was not opened, once: the person is told, and is not told again.
    pub(crate) fn take_refusal(&self) -> Option<String> {
        self.refused.borrow_mut().take()
    }

    /// Write what the shell's parts committed since the last look into the record's copy,
    /// appending only what changed. What every pass runs: cheap when nothing changed.
    pub(crate) fn follow(&self) {
        let Ok(doc) = self.doc.try_borrow() else {
            return;
        };
        if let Some(saver) = self.saver.borrow_mut().as_mut() {
            saver.request(&doc);
        }
    }

    /// Write the copy out now and wait for it to land: what ends the session, so the next one
    /// starts from exactly the shell this one left.
    pub(crate) fn flush(&self) {
        self.follow();
        if let Some(saver) = self.saver.borrow_mut().as_mut()
            && let Err(error) = saver.try_flush()
        {
            tracing::error!(%error, "the shell's record was not written");
        }
    }

    /// The record's heads: they change with every commit a part makes and every change another of
    /// the person's devices wrote, merged here.
    #[must_use]
    pub(crate) fn heads(&self) -> Vec<ChangeHash> {
        self.doc
            .try_borrow_mut()
            .map(|mut doc| doc.get_heads())
            .unwrap_or_default()
    }

    /// The record as it stands, whole: the bytes and the heads they are at, for the person's other
    /// devices. `None` while a part holds the document, which the next pass asks again.
    #[must_use]
    pub(crate) fn snapshot(&self) -> Option<(Vec<u8>, Vec<ChangeHash>)> {
        let mut doc = self.doc.try_borrow_mut().ok()?;
        let heads = doc.get_heads();
        Some((doc.save(), heads))
    }

    /// The change in the record past `heads` — what the person's store already holds — as the
    /// incremental frame that carries the rest to their other devices. `None` while a part holds
    /// the document.
    #[must_use]
    pub(crate) fn since(&self, heads: &[ChangeHash]) -> Option<Vec<u8>> {
        let mut doc = self.doc.try_borrow_mut().ok()?;
        Some(doc.save_after(heads))
    }

    /// Take in the document `other` holds of the record — what the person arranged on one of their
    /// other devices — so this session's shell draws it. Whether the record changed.
    pub(crate) fn take_in(&self, other: &mut AutoCommit) -> bool {
        let Ok(mut doc) = self.doc.try_borrow_mut() else {
            return false;
        };
        match doc.merge_incoming(other) {
            Ok(applied) => {
                let changed = !applied.is_empty();
                if changed {
                    doc.commit();
                }
                changed
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the shell's record did not take in what another device holds"
                );
                false
            }
        }
    }
}

/// The record id the shell's own record takes: the one an app declaring `records: .own` names for
/// `person` (of the package kind `tl.so.tilo.shell`, read and changed by that person's devices alone),
/// and, for a person without a root identity, the id every device derives alike.
fn record_id(person: &str) -> String {
    tilolang_tilo::member_record_of(APP, person).unwrap_or_else(|| {
        ui::record::person::record_id(&ui::record::ResourceType::new(APP))
            .as_str()
            .to_owned()
    })
}

/// The store the person's own records are kept in on this device: the directory the launcher binds
/// into an app as `TILO_PERSON_DIR`, else `$XDG_DATA_HOME/tilo-person`, where the launcher keeps it
/// (`launcher::appdir`). What every app's own record of the person's is kept in, so the shell's is
/// there with them.
fn person_store() -> Option<std::rc::Rc<dyn durable_store::DurableStore>> {
    let root = std::env::var_os("TILO_PERSON_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            launcher::appdir::host_data_home().map(|home| home.join(launcher::appdir::PERSON_DIR))
        })?;
    Some(std::rc::Rc::new(app_state::FsStore::new(root)))
}

impl Drop for ShellRecord {
    /// The last changes the shell's parts committed, written before the record lets go: a session
    /// that ends on its own flushes them through [`Self::flush`], and one that is dropped without
    /// that — a test, a process taken down — still leaves the shell the person chose on disk rather
    /// than the one from before their last change.
    fn drop(&mut self) {
        self.flush();
    }
}
