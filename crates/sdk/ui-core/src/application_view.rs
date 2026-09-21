//! Versioned, device/workspace-local presentation. Never contains document bytes.
use crate::view::{ThawError, View};
use std::path::{Path, PathBuf};

/// Opaque application-owned local presentation; never document bytes.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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

// Postcard is positional: preserve the exact original eight-field schema.
#[derive(serde::Deserialize)]
struct SavedInstanceViewV1 {
    version: u32,
    app_id: String,
    device_id: String,
    workspace_id: String,
    instance_id: u64,
    record_id: String,
    record_kind: String,
    view: View,
}
impl SavedInstanceViewV1 {
    fn upgrade(self) -> Result<SavedInstanceView, ThawError> {
        if self.version != 1 {
            return Err(ThawError::UnsupportedVersion {
                found: self.version,
                supported: 1,
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
        self.view.validate()?;
        Ok(SavedInstanceView {
            version: 2,
            app_id: self.app_id,
            device_id: self.device_id,
            workspace_id: self.workspace_id,
            instance_id: self.instance_id,
            record_id: self.record_id,
            record_kind: self.record_kind,
            view: self.view,
            application: None,
        })
    }
}
fn decode_envelope(bytes: &[u8]) -> Result<SavedInstanceView, ThawError> {
    let (version, _): (u32, _) = postcard::take_from_bytes(bytes).map_err(ThawError::Envelope)?;
    let (instance, remainder) = match version {
        1 => {
            let (old, remainder): (SavedInstanceViewV1, _) =
                postcard::take_from_bytes(bytes).map_err(ThawError::Envelope)?;
            (old.upgrade()?, remainder)
        }
        2 => postcard::take_from_bytes(bytes).map_err(ThawError::Envelope)?,
        found => {
            return Err(ThawError::UnsupportedVersion {
                found,
                supported: 2,
            });
        }
    };
    if !remainder.is_empty() {
        return Err(ThawError::InvalidView("trailing envelope bytes".into()));
    }
    Ok(instance)
}

/// Exact explicit namespace retained across construction, close and save retry.
pub struct InstanceViewStore {
    profile_state_root: PathBuf,
    directory: PathBuf,
    device_id: String,
    workspace_id: String,
    #[allow(
        dead_code,
        reason = "ownership of the profile resource, rather than access to it, keeps it alive"
    )]
    profile_lifetime: Option<std::rc::Rc<dyn std::any::Any>>,
}

pub(crate) fn validate_identity(identity: &str) -> Result<(), ThawError> {
    if identity.trim().is_empty() || identity.chars().any(char::is_control) {
        return Err(ThawError::InvalidView("empty or invalid identity".into()));
    }
    Ok(())
}
fn hex_segment(value: &str) -> String {
    use std::fmt::Write;
    let mut output = String::new();
    for byte in value.as_bytes() {
        // String writes cannot fail.
        let _ = write!(output, "{byte:02x}");
    }
    output
}
impl InstanceViewStore {
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
        validate_identity(device_id)?;
        validate_identity(workspace_id)?;
        let directory = profile_state_root
            .join("instance-views")
            .join(hex_segment(device_id))
            .join(hex_segment(workspace_id));
        Ok(Self {
            profile_state_root,
            directory,
            device_id: device_id.into(),
            workspace_id: workspace_id.into(),
            profile_lifetime: None,
        })
    }
    pub(crate) fn with_profile_lifetime(
        mut self,
        lifetime: std::rc::Rc<dyn std::any::Any>,
    ) -> Self {
        self.profile_lifetime = Some(lifetime);
        self
    }
    pub(crate) fn matches_namespace(&self, root: &Path, device: &str, workspace: &str) -> bool {
        self.profile_state_root == root
            && self.device_id == device
            && self.workspace_id == workspace
    }
    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    fn path(&self, instance: u64) -> PathBuf {
        self.directory.join(format!("{instance}.postcard"))
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
        let bytes = match std::fs::read(self.path(instance_id)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ThawError::Io(error)),
        };
        let instance = decode_envelope(&bytes)?;
        self.validate(&instance, instance_id)?;
        Ok(Some(instance))
    }
    /// # Errors
    /// Refuses invalid/foreign envelopes before atomically replacing the local View file.
    pub fn save(&self, instance: &SavedInstanceView) -> Result<(), ThawError> {
        self.validate(instance, instance.instance_id)?;
        let bytes = postcard::to_stdvec(instance).map_err(ThawError::Envelope)?;
        std::fs::create_dir_all(&self.directory).map_err(ThawError::Io)?;
        atomic_file::write(&self.path(instance.instance_id), &bytes).map_err(ThawError::Io)
    }
}
