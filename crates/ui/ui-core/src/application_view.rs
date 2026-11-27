//! Versioned, device/workspace-local presentation. Never contains document bytes.
use crate::resource::{DocumentId, DocumentPort};
use crate::view::{ThawError, View};
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash, ROOT, ReadDoc as _};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Opaque application-owned local presentation; never document bytes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApplicationLocalView {
    pub app_id: String,
    pub version: u32,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationFocusRestore {
    Generic,
    Application,
}

/// The application-owned presentation of a widget below a component's root: which item is open, what is
/// expanded, where the caret was.
pub trait ApplicationViewState {
    /// Capture the presentation to save with the window.
    fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError>;

    /// Prepare the saved presentation `local` against the latest record.
    /// # Errors
    /// Rejects a payload the application cannot restore.
    fn restore_application_view(
        &mut self,
        local: &ApplicationLocalView,
        doc: &automerge::AutoCommit,
    ) -> Result<ApplicationFocusRestore, ThawError>;

    /// Finish after the ordinary view restored, before the first frame.
    fn finish_application_view_restore(&mut self) -> Result<(), ThawError> {
        Ok(())
    }
}

/// The file or folder a window shows, kept with its saved view as it changes (Files going into a
/// folder): after a restart the window opens on it again, as a launch naming it does.
pub struct WindowShows {
    pub(crate) shown: Rc<std::cell::RefCell<Option<String>>>,
    pub(crate) views: Rc<InstanceViewStore>,
    /// The window's view as saved before it showed anything: kept when no view of it is saved.
    pub(crate) unsaved: SavedInstanceView,
}

/// Written out rather than derived: a derived `clone` is inlined into every
/// app, this one is compiled once.
impl Clone for WindowShows {
    #[inline(never)]
    fn clone(&self) -> Self {
        Self {
            shown: Rc::clone(&self.shown),
            views: Rc::clone(&self.views),
            unsaved: self.unsaved.clone(),
        }
    }
}

impl WindowShows {
    /// The file or folder the window shows now, as [`Self::show`] last set it.
    #[must_use]
    pub fn shown(&self) -> Option<PathBuf> {
        self.shown.borrow().as_deref().map(PathBuf::from)
    }

    /// The window now shows the file or folder at `path`. Kept at once, so a session that ends
    /// without closing the window brings it back there.
    pub fn show(&self, path: &Path) {
        let Some(path) = std::path::absolute(path)
            .ok()
            .and_then(|path| path.to_str().map(str::to_owned))
        else {
            return;
        };
        if self.shown.borrow().as_deref() == Some(path.as_str()) {
            return;
        }
        *self.shown.borrow_mut() = Some(path.clone());
        let saved = match self.views.load(self.unsaved.instance_id) {
            Ok(Some(saved)) if saved.document_id == self.unsaved.document_id => saved,
            _ => self.unsaved.clone(),
        };
        let saved = SavedInstanceView {
            shows: Some(path),
            ..saved
        };
        // The window's frames never wait on the disk: the store commits what it shows.
        let _started_only = crate::storage::waiting::forbid_on_this_thread();
        if let Err(error) = self.views.save(&saved) {
            tracing::warn!(%error, "what the window shows is not remembered");
        }
    }

    /// The window's own view moved (what its [`ApplicationViewState`] captures: the folder it
    /// shows of a tree it reads, by its node): kept at once with its saved view, as [`Self::show`]
    /// keeps a path, so a page reloaded or a session that ends without closing the window brings
    /// it back there, and no other window's view moves.
    pub fn keep(&self, application: ApplicationLocalView) {
        let saved = match self.views.load(self.unsaved.instance_id) {
            Ok(Some(saved)) if saved.document_id == self.unsaved.document_id => saved,
            _ => self.unsaved.clone(),
        };
        if saved.application.as_ref() == Some(&application) {
            return;
        }
        let saved = SavedInstanceView {
            application: Some(application),
            ..saved
        };
        // The window's frames never wait on the disk: the store commits its view.
        let _started_only = crate::storage::waiting::forbid_on_this_thread();
        if let Err(error) = self.views.save(&saved) {
            tracing::warn!(%error, "the window's view is not remembered");
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SavedInstanceView {
    /// [`Self::VERSION`] for a view this build saved.
    pub version: u32,
    pub app_id: String,
    pub device_id: String,
    pub workspace_id: String,
    pub instance_id: u64,
    pub document_id: String,
    pub resource_type: String,
    pub view: View,
    pub application: Option<ApplicationLocalView>,
    /// The file or folder the window shows, by its absolute path: the one it was opened on, or the
    /// one it went to since ([`WindowShows`]). A window brought back opens on it again, as a launch
    /// naming it would.
    pub shows: Option<String>,
}

impl SavedInstanceView {
    pub const VERSION: u32 = 3;

    /// The file or folder the window shows, while it is still there. One that is gone names
    /// nothing: the window opens as the app opens a window on nothing.
    #[must_use]
    pub fn shown(&self) -> Option<&Path> {
        self.shows
            .as_deref()
            .map(Path::new)
            .filter(|path| path.exists() || crate::portal::is_document_reference(path))
    }

    pub(crate) fn validate(&self) -> Result<(), ThawError> {
        if self.version != Self::VERSION {
            return Err(ThawError::UnsupportedVersion {
                found: self.version,
                supported: Self::VERSION,
            });
        }
        if self
            .shows
            .as_deref()
            .is_some_and(|path| !Path::new(path).is_absolute() || path.contains('\0'))
        {
            return Err(ThawError::InvalidView("shows no absolute path".into()));
        }
        for identity in [
            &self.app_id,
            &self.device_id,
            &self.workspace_id,
            &self.document_id,
            &self.resource_type,
        ] {
            validate_identity(identity)?;
        }
        if self
            .application
            .as_ref()
            .is_some_and(|local| local.app_id != self.app_id)
        {
            return Err(ThawError::BindingMismatch);
        }
        self.view.validate()
    }
}

/// The view `bytes` keep, `None` for one an earlier build saved: the window opens as a new one.
fn decode_envelope(bytes: &[u8]) -> Result<Option<SavedInstanceView>, ThawError> {
    let (version, _): (u32, _) = postcard::take_from_bytes(bytes).map_err(ThawError::Envelope)?;
    if version < SavedInstanceView::VERSION {
        return Ok(None);
    }
    if version != SavedInstanceView::VERSION {
        return Err(ThawError::UnsupportedVersion {
            found: version,
            supported: SavedInstanceView::VERSION,
        });
    }
    let (instance, remainder) = postcard::take_from_bytes(bytes).map_err(ThawError::Envelope)?;
    if !remainder.is_empty() {
        return Err(ThawError::InvalidView("trailing envelope bytes".into()));
    }
    Ok(Some(instance))
}

/// The saved views of one app's windows on one device in one workspace, each a document of the
/// app's in that workspace: kept in the workspace's one store where the store has a port, in memory
/// otherwise. A view is its person's alone: what their windows showed is no other member's to
/// read ([`Self::read_by`]).
pub struct InstanceViewStore {
    port: Option<Rc<dyn DocumentPort>>,
    app_id: String,
    device_id: String,
    workspace_id: String,
    /// The reader set the views are written for: the person whose windows they are.
    readers: Option<String>,
    ephemeral: bool,
    /// Each view's document as last read or saved.
    documents: RefCell<HashMap<u64, KeptView>>,
}

/// A window's view document and how far the workspace has it.
struct KeptView {
    doc: AutoCommit,
    /// The heads the workspace holds of it; `None` before it holds any.
    stored: Option<Vec<ChangeHash>>,
    /// The write on its way, and the heads it brings the workspace to.
    writing: Option<(crate::resource::Written, Vec<ChangeHash>)>,
}

impl KeptView {
    fn new(doc: AutoCommit, stored: Option<Vec<ChangeHash>>) -> Self {
        Self {
            doc,
            stored,
            writing: None,
        }
    }
}

/// The key a view's bytes are kept under in its document.
const VIEW: &str = "view";

pub(crate) fn validate_identity(identity: &str) -> Result<(), ThawError> {
    if identity.trim().is_empty() || identity.chars().any(char::is_control) {
        return Err(ThawError::InvalidView("empty or invalid identity".into()));
    }
    Ok(())
}

fn workspace_error(error: impl std::fmt::Display) -> ThawError {
    ThawError::Io(std::io::Error::other(error.to_string()))
}

impl InstanceViewStore {
    /// Views of `app_id`'s windows on `device_id` in `workspace_id`, kept through `port`, or in
    /// memory without one.
    ///
    /// # Errors
    /// Rejects empty/control-character identities.
    pub fn new(
        port: Option<Rc<dyn DocumentPort>>,
        app_id: &str,
        device_id: &str,
        workspace_id: &str,
    ) -> Result<Self, ThawError> {
        validate_identity(app_id)?;
        validate_identity(device_id)?;
        validate_identity(workspace_id)?;
        Ok(Self {
            port,
            app_id: app_id.into(),
            device_id: device_id.into(),
            workspace_id: workspace_id.into(),
            readers: None,
            ephemeral: false,
            documents: RefCell::default(),
        })
    }

    /// These views, written so that `person` alone reads them: the person whose windows they
    /// are, named by their root key as the workspace names readers. A person known by no root
    /// key (a profile of a test's own) is named as they are, which a workspace refuses: the
    /// close says the view was not kept. Views of no person (an agent's or a bot's windows) are
    /// kept in memory alone, as written to the workspace they would be read by every member.
    #[must_use]
    pub fn read_by(mut self, person: Option<&tilo_member_types::PersonId>) -> Self {
        if let Some(person) = person {
            self.readers = Some(
                person
                    .root()
                    .and_then(|root| collab_types::Readers::people([root]))
                    .map_or_else(|| person.as_str().to_owned(), |readers| readers.text()),
            );
        } else {
            self.readers = None;
            self.port = None;
        }
        self
    }

    /// Views kept in memory alone.
    ///
    /// # Errors
    /// Rejects empty/control-character identities.
    pub fn in_memory(app_id: &str, device_id: &str, workspace_id: &str) -> Result<Self, ThawError> {
        Self::new(None, app_id, device_id, workspace_id)
    }

    /// A model-only service keeps presentation ephemeral while retaining the
    /// original app/device/workspace binding. It never reads or replaces
    /// a user's saved window view. Record persistence is unaffected.
    #[must_use]
    pub fn without_persistence(mut self) -> Self {
        self.ephemeral = true;
        self
    }

    /// The same views for another device or workspace of the app, through the same port.
    ///
    /// # Errors
    /// Rejects empty/control-character identities.
    pub(crate) fn beside(&self, device_id: &str, workspace_id: &str) -> Result<Self, ThawError> {
        let mut beside = Self::new(self.port.clone(), &self.app_id, device_id, workspace_id)?;
        beside.readers.clone_from(&self.readers);
        Ok(beside)
    }

    pub(crate) fn matches(&self, app: &str, device: &str, workspace: &str) -> bool {
        self.app_id == app && self.device_id == device && self.workspace_id == workspace
    }

    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// The document `instance`'s view is kept in.
    fn document_id(&self, instance: u64) -> DocumentId {
        DocumentId::window_view(&self.device_id, instance)
    }

    fn validate(&self, instance: &SavedInstanceView, key: u64) -> Result<(), ThawError> {
        instance.validate()?;
        if instance.app_id != self.app_id
            || instance.device_id != self.device_id
            || instance.workspace_id != self.workspace_id
            || instance.instance_id != key
        {
            return Err(ThawError::BindingMismatch);
        }
        Ok(())
    }

    /// `instance`'s document as the workspace holds it, read once.
    fn read(&self, instance: u64) -> Result<(), ThawError> {
        if self.documents.borrow().contains_key(&instance) {
            return Ok(());
        }
        let Some(port) = &self.port else {
            return Ok(());
        };
        let id = self.document_id(instance);
        let read = port
            .read(
                &self.app_id,
                &self.workspace_id,
                Some(vec![crate::resource::DocumentAsk {
                    id: id.route().to_owned(),
                    since: None,
                }]),
            )
            .map_err(ThawError::Unreachable)?
            .pop()
            .map(|(_, read)| read);
        if let Some(crate::resource::DocumentRead::Whole { bytes, .. }) = read {
            let mut doc = AutoCommit::load(&bytes).map_err(workspace_error)?;
            let heads = doc.get_heads();
            self.documents
                .borrow_mut()
                .insert(instance, KeptView::new(doc, Some(heads)));
        }
        Ok(())
    }

    pub fn load(&self, instance_id: u64) -> Result<Option<SavedInstanceView>, ThawError> {
        if self.ephemeral {
            return Ok(None);
        }
        self.read(instance_id)?;
        let documents = self.documents.borrow();
        let Some(kept) = documents.get(&instance_id) else {
            return Ok(None);
        };
        let Some((value, _)) = kept.doc.get(ROOT, VIEW).map_err(workspace_error)? else {
            return Ok(None);
        };
        let Some(bytes) = value.as_bytes() else {
            return Err(ThawError::InvalidView(
                "a saved view that is no view".into(),
            ));
        };
        let Some(instance) = decode_envelope(bytes)? else {
            return Ok(None);
        };
        self.validate(&instance, instance_id)?;
        Ok(Some(instance))
    }

    /// Keep `instance`'s view: sent to the workspace now, without waiting for it to take it, so a
    /// window's frames never wait on it. [`Self::settling`] says when it did.
    ///
    /// # Errors
    /// A view of another app, device, workspace or instance, or one the workspace refused.
    pub fn save(&self, instance: &SavedInstanceView) -> Result<(), ThawError> {
        self.validate(instance, instance.instance_id)?;
        if self.ephemeral {
            return Ok(());
        }
        self.read(instance.instance_id)?;
        let bytes = postcard::to_stdvec(instance).map_err(ThawError::Envelope)?;
        let mut documents = self.documents.borrow_mut();
        let kept = documents
            .entry(instance.instance_id)
            .or_insert_with(|| KeptView::new(AutoCommit::new(), None));
        // A view saved as it already is changes nothing, and the document keeps no copy of it.
        let unchanged = kept
            .doc
            .get(ROOT, VIEW)
            .map_err(workspace_error)?
            .and_then(|(value, _)| value.as_bytes().map(|kept| kept == bytes.as_slice()))
            .unwrap_or(false);
        if unchanged {
            return self.send(instance.instance_id, kept, true);
        }
        kept.doc.put(ROOT, VIEW, bytes).map_err(workspace_error)?;
        self.commit_view(&mut kept.doc);
        self.send(instance.instance_id, kept, true)
    }

    /// The person whose windows these views are, by their root key, when their reader set names
    /// one.
    fn person_root(&self) -> Option<[u8; 32]> {
        let Some(collab_types::Readers::People(people)) =
            collab_types::Readers::parse(self.readers.as_deref()?)
        else {
            return None;
        };
        let mut roots = people.into_iter();
        let root = roots.next()?;
        roots.next().is_none().then_some(root)
    }

    /// The person whose windows these views are: their root key when their reader set names one,
    /// else the one person the set names as they are (a person this device knows no root for).
    fn view_person(&self) -> Option<tilo_member_types::PersonId> {
        if let Some(root) = self.person_root() {
            return Some(tilo_member_types::PersonId::from_root(root));
        }
        let readers = self.readers.as_deref()?;
        let one_person = !readers.is_empty()
            && readers != collab_types::EVERY_MEMBER
            && !readers.starts_with(collab_types::ROLE_READERS)
            && !readers.contains(',');
        one_person.then(|| tilo_member_types::PersonId::from(readers))
    }

    /// Commit what `doc` holds as the view of the person whose window it is: a change the
    /// workspace takes names someone, and a view of no person the workspace refuses anyway.
    fn commit_view(&self, doc: &mut AutoCommit) {
        if doc.pending_ops() == 0 {
            return;
        }
        let Some(person) = self.view_person() else {
            doc.commit();
            return;
        };
        let acting = tilo_member_types::Acting::person(person);
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::ActionContext::whole_edit_by(&acting)),
        );
    }

    /// Take the workspace's answer to `kept`'s write on its way, without waiting for it, and send
    /// what it does not hold yet once none is: one write at a time, so each brings the workspace
    /// what the last one left out. A refusal is said, unless this is a `save` again: what the
    /// refused write carried goes with the next one.
    fn send(&self, instance: u64, kept: &mut KeptView, save: bool) -> Result<(), ThawError> {
        let Some(port) = &self.port else {
            return Ok(());
        };
        if let Some((written, heads)) = &mut kept.writing {
            match written(crate::time::Duration::ZERO) {
                None => return Ok(()),
                Some(Ok(())) => kept.stored = Some(std::mem::take(heads)),
                Some(Err(error)) if !save => {
                    kept.writing = None;
                    return Err(workspace_error(error));
                }
                Some(Err(_)) => {}
            }
            kept.writing = None;
        }
        self.commit_view(&mut kept.doc);
        let heads = kept.doc.get_heads();
        if kept.stored.as_ref() == Some(&heads) {
            return Ok(());
        }
        let changes = match &kept.stored {
            Some(stored) => kept.doc.save_after(stored),
            None => kept.doc.save(),
        };
        let written = port.write(
            &self.app_id,
            &self.workspace_id,
            self.document_id(instance).route(),
            tilo_desktop_model::record::WINDOW_VIEW_KIND,
            changes,
        );
        kept.writing = Some((written, heads));
        Ok(())
    }

    /// Send what the workspace does not hold of every view this store kept, and whether a write
    /// is still on its way, without waiting for one: what a closing window asks each pass until
    /// the workspace took every view, so the next start finds it where it was left.
    ///
    /// # Errors
    /// A view the workspace refused.
    pub fn settling(&self) -> Result<bool, ThawError> {
        let mut waiting = false;
        for (instance, kept) in self.documents.borrow_mut().iter_mut() {
            self.send(*instance, kept, false)?;
            waiting |= kept.writing.is_some();
        }
        Ok(waiting)
    }
}

impl crate::launch_error::LaunchError {
    /// Whether the launch stopped only because the store thread is
    /// still opening a document for a thread that may not wait on the disk
    /// ([`crate::resource::ResourceOpenError::Opening`]): asked again once the
    /// store wakes the caller, it goes on. Not a failure to show.
    #[must_use]
    pub fn is_opening(&self) -> bool {
        match self {
            Self::Restore(crate::view::ThawError::Opening) => true,
            Self::ComponentSetup { source, .. } | Self::ComponentCapture { source, .. } => {
                source.is_opening()
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(instance_id: u64) -> SavedInstanceView {
        SavedInstanceView {
            version: SavedInstanceView::VERSION,
            app_id: "so.tilo.test".into(),
            device_id: "D".into(),
            workspace_id: "A".into(),
            instance_id,
            document_id: "uuid:record".into(),
            resource_type: "note".into(),
            view: View::default(),
            application: None,
            shows: None,
        }
    }

    #[test]
    fn views_round_trip_through_the_workspace_and_refuse_foreign_bindings() {
        let workspace: Rc<dyn DocumentPort> = Rc::new(crate::resource::MemoryWorkspace::default());
        let ws = "44444444-4444-4444-8444-444444444444";
        let views =
            InstanceViewStore::new(Some(workspace.clone()), "so.tilo.test", "D", ws).unwrap();
        assert!(views.load(7).unwrap().is_none());
        let mut seven = saved(7);
        seven.workspace_id = ws.into();
        views.save(&seven).unwrap();
        // Another process of the app reads it from the workspace.
        let again =
            InstanceViewStore::new(Some(workspace.clone()), "so.tilo.test", "D", ws).unwrap();
        assert_eq!(again.load(7).unwrap().unwrap().document_id, "uuid:record");
        let other = InstanceViewStore::new(Some(workspace), "so.tilo.test", "E", ws).unwrap();
        assert!(other.load(7).unwrap().is_none());
        assert!(other.save(&seven).is_err());
    }

    /// A window's view says what its person was looking at: it is written for that person alone,
    /// named by their root key as the workspace's reader groups are, and the close waits until the
    /// workspace took it. A window of no person (an agent's) keeps its view in memory, never
    /// written where every member would read it.
    #[test]
    fn a_view_is_written_for_its_person_alone() {
        // The workspace keeps a window's view by its declared type: read by the person whose
        // window it is, as a device's store declares the records the system keeps.
        let memory = Rc::new(
            crate::resource::MemoryWorkspace::default()
                .declaring(tilo_desktop_model::record::window_view()),
        );
        let workspace: Rc<dyn DocumentPort> = memory.clone();
        let ws = "44444444-4444-4444-8444-444444444444";
        let root = [0xa0; 32];
        let person = tilo_member_types::PersonId::from_root(root);
        let views = InstanceViewStore::new(Some(workspace.clone()), "so.tilo.test", "D", ws)
            .unwrap()
            .read_by(Some(&person));
        let mut seven = saved(7);
        seven.workspace_id = ws.into();
        views.save(&seven).unwrap();
        assert!(!views.settling().unwrap(), "the workspace took it");
        assert_eq!(
            memory.readers("so.tilo.test", ws, views.document_id(7).route()),
            Some(collab_types::Readers::people([root]).unwrap().text()),
        );

        // A person known by no root key is named as they are, which the workspace refuses: their
        // view is never written for every member.
        let unrooted = tilo_member_types::PersonId::from("ada");
        let theirs = InstanceViewStore::new(Some(workspace.clone()), "so.tilo.test", "U", ws)
            .unwrap()
            .read_by(Some(&unrooted));
        let mut nine = saved(9);
        nine.device_id = "U".into();
        nine.workspace_id = ws.into();
        theirs.save(&nine).unwrap();
        assert!(!theirs.settling().unwrap());
        assert_eq!(
            memory.readers("so.tilo.test", ws, theirs.document_id(9).route()),
            Some("ada".to_owned()),
            "the view names its person, never every member"
        );

        let agents = InstanceViewStore::new(Some(workspace), "so.tilo.test", "A", ws)
            .unwrap()
            .read_by(None);
        let writes = memory.writes.get();
        let mut eight = saved(8);
        eight.device_id = "A".into();
        eight.workspace_id = ws.into();
        agents.save(&eight).unwrap();
        assert_eq!(
            memory.writes.get(),
            writes,
            "nothing of it reached the workspace"
        );
        assert_eq!(
            agents.load(8).unwrap().map(|view| view.instance_id),
            Some(8),
            "the window still has it while it runs"
        );
    }
}
