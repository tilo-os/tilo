//! Versioned, device/workspace-local presentation. Never contains document bytes.
use crate::view::{ThawError, View};
use durable_store::{DurableStore, StorageKey, StoreError, WriteOp};
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
        let _started_only = durable_store::waiting::forbid_on_this_thread();
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
        let _started_only = durable_store::waiting::forbid_on_this_thread();
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

/// Exact explicit namespace retained across construction, close and save retry.
pub struct InstanceViewStore {
    store: Rc<dyn DurableStore>,
    /// The same explicit profile/device browser cache the record owner uses.
    portable: Option<crate::portable_storage::PortableStorage>,
    device_id: String,
    workspace_id: String,
    ephemeral: bool,
    _profile_lifetime: Option<std::rc::Rc<dyn std::any::Any>>,
}

pub(crate) fn validate_identity(identity: &str) -> Result<(), ThawError> {
    if identity.trim().is_empty() || identity.chars().any(char::is_control) {
        return Err(ThawError::InvalidView("empty or invalid identity".into()));
    }
    Ok(())
}
fn storage_error(error: StoreError) -> ThawError {
    match error {
        StoreError::Opening(_) => ThawError::Opening,
        error => ThawError::Io(std::io::Error::other(error)),
    }
}
impl InstanceViewStore {
    /// Views stored under a local profile directory.
    ///
    /// # Errors
    /// Rejects a relative profile root or empty/control-character identities.
    pub fn new(
        profile_state_root: PathBuf,
        device_id: &str,
        workspace_id: &str,
    ) -> Result<Self, ThawError> {
        if !profile_state_root.is_absolute() {
            return Err(ThawError::InvalidView(
                "profile root must be absolute".into(),
            ));
        }
        Self::with_store(
            Rc::new(app_state::FsStore::new(profile_state_root)),
            device_id,
            workspace_id,
        )
    }
    /// Views stored through a host-provided store.
    ///
    /// # Errors
    /// Rejects empty/control-character identities.
    pub fn with_store(
        store: Rc<dyn DurableStore>,
        device_id: &str,
        workspace_id: &str,
    ) -> Result<Self, ThawError> {
        validate_identity(device_id)?;
        validate_identity(workspace_id)?;
        Ok(Self {
            store,
            portable: None,
            device_id: device_id.into(),
            workspace_id: workspace_id.into(),
            _profile_lifetime: None,
            ephemeral: false,
        })
    }
    /// Use the same explicit profile/device cache as the record owner.
    pub fn portable(
        cache: crate::portable_storage::PortableStorage,
        workspace: &str,
    ) -> Result<Self, ThawError> {
        validate_identity(workspace)?;
        let crate::portable_storage::ProfileNamespace::Portable { device, .. } = cache.namespace()
        else {
            return Err(ThawError::BindingMismatch);
        };
        Ok(Self {
            // Views over a portable cache never open this store.
            store: Rc::new(durable_store::MemoryStore::new()),
            device_id: device.clone(),
            workspace_id: workspace.into(),
            portable: Some(cache),
            _profile_lifetime: None,
            ephemeral: false,
        })
    }
    /// A model-only service keeps presentation ephemeral while retaining the
    /// original profile/device/workspace binding. It never reads or replaces
    /// a user's saved window view. Record persistence is unaffected.
    #[must_use]
    pub fn without_persistence(mut self) -> Self {
        self.ephemeral = true;
        self
    }

    fn namespace(&self) -> String {
        match &self.portable {
            Some(cache) => crate::resource::portable_namespace(cache),
            None => self.store.namespace().to_owned(),
        }
    }
    fn cache_key(&self, instance: u64) -> crate::portable_storage::StorageKey {
        crate::portable_storage::StorageKey::View {
            workspace: self.workspace_id.clone(),
            instance,
        }
    }
    pub(crate) fn with_profile_lifetime(
        mut self,
        lifetime: std::rc::Rc<dyn std::any::Any>,
    ) -> Self {
        self._profile_lifetime = Some(lifetime);
        self
    }
    pub(crate) fn matches_namespace(&self, namespace: &str, device: &str, workspace: &str) -> bool {
        self.namespace() == namespace && self.device_id == device && self.workspace_id == workspace
    }
    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// The store the views are kept in: what another window of the same
    /// profile keeps its own in.
    pub(crate) fn durable_store(&self) -> Rc<dyn DurableStore> {
        Rc::clone(&self.store)
    }
    fn key(&self, instance: u64) -> StorageKey {
        StorageKey::view(&self.device_id, &self.workspace_id, instance)
    }
    fn validate(&self, instance: &SavedInstanceView, key: u64) -> Result<(), ThawError> {
        instance.validate()?;
        if instance.device_id != self.device_id
            || instance.workspace_id != self.workspace_id
            || instance.instance_id != key
        {
            return Err(ThawError::BindingMismatch);
        }
        Ok(())
    }
    pub fn load(&self, instance_id: u64) -> Result<Option<SavedInstanceView>, ThawError> {
        if self.ephemeral {
            return Ok(None);
        }
        let bytes = if let Some(cache) = &self.portable {
            let Some(value) = cache.get(&self.cache_key(instance_id)) else {
                return Ok(None);
            };
            value.bytes
        } else {
            let Some(stored) = self
                .store
                .read(&self.key(instance_id))
                .map_err(storage_error)?
            else {
                return Ok(None);
            };
            stored.bytes
        };
        let Some(instance) = decode_envelope(&bytes)? else {
            return Ok(None);
        };
        self.validate(&instance, instance_id)?;
        Ok(Some(instance))
    }
    pub fn save(&self, instance: &SavedInstanceView) -> Result<(), ThawError> {
        self.save_landing(instance).map(drop)
    }

    /// [`Self::save`], and the write a durable store is committing: `None` where nothing waits on a
    /// commit (an ephemeral or portable View).
    pub(crate) fn save_landing(
        &self,
        instance: &SavedInstanceView,
    ) -> Result<Option<crate::resource::transfer::Landing>, ThawError> {
        self.validate(instance, instance.instance_id)?;
        if self.ephemeral {
            return Ok(None);
        }
        let bytes = postcard::to_stdvec(instance).map_err(ThawError::Envelope)?;
        if let Some(cache) = &self.portable {
            let key = self.cache_key(instance.instance_id);
            return cache
                .put(key.clone(), bytes, None)
                .and_then(|()| cache.flush(&key))
                .map(|()| None)
                .map_err(|e| ThawError::Io(std::io::Error::other(e)));
        }
        let key = self.key(instance.instance_id);
        let revision = self
            .store
            .write(&key, WriteOp::Replace(bytes), None)
            .map_err(storage_error)?;
        // Never waits on a frame loop, which marks itself: there the store commits on its own
        // thread and a later read already sees the saved View. Anywhere else the save is on
        // disk before it returns, and a refused commit is this call's failure.
        match self.store.flush_where_allowed(&key) {
            Ok(()) | Err(StoreError::Pending(_)) => {
                Ok(Some(crate::resource::transfer::Landing::new(key, revision)))
            }
            Err(error) => Err(storage_error(error)),
        }
    }
}

impl crate::launch::LaunchError {
    /// Whether the launch stopped only because the store thread is
    /// still opening a document for a thread that may not wait on the disk
    /// ([`crate::resource::ResourceOpenError::Opening`]): asked again once the
    /// store wakes the caller, it goes on. Not a failure to show.
    #[must_use]
    pub fn is_opening(&self) -> bool {
        match self {
            Self::Record(crate::resource::ResourceOpenError::Opening)
            | Self::Restore(crate::view::ThawError::Opening) => true,
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
    fn views_round_trip_through_any_store_and_refuse_foreign_bindings() {
        let store: Rc<dyn DurableStore> = Rc::new(durable_store::MemoryStore::new());
        let views = InstanceViewStore::with_store(Rc::clone(&store), "D", "A").unwrap();
        assert!(views.load(7).unwrap().is_none());
        views.save(&saved(7)).unwrap();
        assert_eq!(views.load(7).unwrap().unwrap().document_id, "uuid:record");
        assert!(views.matches_namespace(store.namespace(), "D", "A"));
        let other = InstanceViewStore::with_store(store, "D", "B").unwrap();
        assert!(other.load(7).unwrap().is_none());
        assert!(other.save(&saved(7)).is_err());
    }
}
