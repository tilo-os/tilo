//! Versioned, device/workspace-local presentation. Never contains document bytes.
use crate::view::{ThawError, View};
use durable_store::{DurableStore, StorageKey, StoreError, WriteOp};
use std::path::PathBuf;
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

/// The application-owned presentation of a widget below a component's root:
/// which item is open, what is expanded, where the caret was. A root that
/// hosts the widget (a framework app's root over its surface) saves and
/// restores it as its own [`AppWidget`](crate::widget::AppWidget) view, so a
/// reopened window shows what it showed.
pub trait ApplicationViewState {
    /// Capture the presentation to save with the window.
    /// # Errors
    /// Rejects an invalid contribution before anything is saved.
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
    /// # Errors
    /// Rejects a prepared plan without publishing a partial view.
    fn finish_application_view_restore(&mut self) -> Result<(), ThawError> {
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SavedInstanceView {
    pub version: u32,
    pub app_id: String,
    pub device_id: String,
    pub workspace_id: String,
    pub instance_id: u64,
    pub record_id: String,
    pub record_kind: String,
    pub view: View,
    pub application: Option<ApplicationLocalView>,
}

impl SavedInstanceView {
    pub(crate) fn validate(&self) -> Result<(), ThawError> {
        if self.version != 2 {
            return Err(ThawError::UnsupportedVersion {
                found: self.version,
                supported: 2,
            });
        }
        for identity in [
            &self.app_id,
            &self.device_id,
            &self.workspace_id,
            &self.record_id,
            &self.record_kind,
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

fn decode_envelope(bytes: &[u8]) -> Result<SavedInstanceView, ThawError> {
    let (version, _): (u32, _) = postcard::take_from_bytes(bytes).map_err(ThawError::Envelope)?;
    if version != 2 {
        return Err(ThawError::UnsupportedVersion {
            found: version,
            supported: 2,
        });
    }
    let (instance, remainder) = postcard::take_from_bytes(bytes).map_err(ThawError::Envelope)?;
    if !remainder.is_empty() {
        return Err(ThawError::InvalidView("trailing envelope bytes".into()));
    }
    Ok(instance)
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

    /// The explicit profile namespace these views are bound to; equal to the
    /// record owner's canonical namespace for the same profile.
    fn namespace(&self) -> String {
        match &self.portable {
            Some(cache) => crate::record::portable_namespace(cache),
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
    /// # Errors
    /// Reports I/O, malformed/versioned envelopes, invalid geometry and foreign bindings.
    /// Invalid source bytes are never repaired or replaced.
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
        let instance = decode_envelope(&bytes)?;
        self.validate(&instance, instance_id)?;
        Ok(Some(instance))
    }
    /// # Errors
    /// Refuses invalid/foreign envelopes before replacing the stored View,
    /// and reports a replacement the store failed to commit. A store waiting
    /// on its host to commit is not a failure.
    pub fn save(&self, instance: &SavedInstanceView) -> Result<(), ThawError> {
        self.save_landing(instance).map(drop)
    }

    /// [`Self::save`], and the write a durable store is committing: `None`
    /// where nothing waits on a commit (an ephemeral or portable View).
    pub(crate) fn save_landing(
        &self,
        instance: &SavedInstanceView,
    ) -> Result<Option<crate::record::transfer::Landing>, ThawError> {
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
        // Never waits on the frame loop: there the store commits on its own
        // thread, and a later read already sees the saved View.
        match self.store.flush_where_allowed(&key) {
            Ok(()) | Err(StoreError::Pending(_)) => {
                Ok(Some(crate::record::transfer::Landing::new(key, revision)))
            }
            Err(error) => Err(storage_error(error)),
        }
    }
}

impl crate::launch::LaunchError {
    /// Whether the launch stopped only because the store's own thread is
    /// still opening a document for a thread that may not wait on the disk
    /// ([`crate::record::RecordOpenError::Opening`]): asked again once the
    /// store wakes the caller, it goes on. Not a failure to show.
    #[must_use]
    pub fn is_opening(&self) -> bool {
        match self {
            Self::Record(crate::record::RecordOpenError::Opening)
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
            version: 2,
            app_id: "so.tilo.test".into(),
            device_id: "D".into(),
            workspace_id: "A".into(),
            instance_id,
            record_id: "uuid:record".into(),
            record_kind: "note".into(),
            view: View::default(),
            application: None,
        }
    }

    #[test]
    fn views_round_trip_through_any_store_and_refuse_foreign_bindings() {
        let store: Rc<dyn DurableStore> = Rc::new(durable_store::MemoryStore::new());
        let views = InstanceViewStore::with_store(Rc::clone(&store), "D", "A").unwrap();
        assert!(views.load(7).unwrap().is_none());
        views.save(&saved(7)).unwrap();
        assert_eq!(views.load(7).unwrap().unwrap().record_id, "uuid:record");
        assert!(views.matches_namespace(store.namespace(), "D", "A"));
        let other = InstanceViewStore::with_store(store, "D", "B").unwrap();
        assert!(other.load(7).unwrap().is_none());
        assert!(other.save(&saved(7)).is_err());
    }
}
