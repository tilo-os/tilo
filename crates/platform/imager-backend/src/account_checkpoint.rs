//! Device-local prepared account custody and authenticated restart checkpoint declarations.
use crate::account_transfer::{AccountActions, ConnectionError, Connector, DestinationGuard};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
// The envelope permits 256 bytes per 1 MiB chunk and 2048 bytes of fixed framing.
const SEALED_LIMIT: u64 = 32 * 1024 * 1024 + 32 * 256 + 2048;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    schema: u32,
    filename: String,
    sealed_bytes: u64,
    ciphertext_sha256: [u8; 32],
    binding: String,
    group: [u8; 32],
    accounts: usize,
}
impl Receipt {
    pub fn validate(&self) -> io::Result<()> {
        let name = self.filename.strip_suffix(".sealed").ok_or_else(invalid)?;
        if self.schema != 1
            || uuid::Uuid::parse_str(name).is_err()
            || format!(
                "{}.sealed",
                uuid::Uuid::parse_str(name).map_err(|_| invalid())?
            ) != self.filename
            || self.sealed_bytes == 0
            || self.sealed_bytes > SEALED_LIMIT
            || self.accounts > 64
            || self.binding.is_empty()
            || self.binding.len() > 256
            || self.binding.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
impl Receipt {
    pub fn binding(&self) -> &str {
        &self.binding
    }
    pub fn group(&self) -> [u8; 32] {
        self.group
    }
    pub fn accounts(&self) -> usize {
        self.accounts
    }
    pub fn filename(&self) -> &str {
        &self.filename
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedEntry {
    pub id: String,
    pub source_id: Option<String>,
    pub label: String,
    pub revoke_on_remove: bool,
}

/// Retained by the install plan until the target's private home is available.
/// Never serialize this handle or put its path/key in an InstallProfile.
#[derive(Clone)]
pub struct PreparedAccounts {
    stage: Arc<tempfile::TempDir>,
    entries: Vec<PreparedEntry>,
    guard: DestinationGuard,
}
impl PreparedAccounts {
    pub fn home(&self) -> &Path {
        self.stage.path()
    }
    pub fn config_root(&self) -> PathBuf {
        self.home().join(".config/tilo")
    }
    pub fn count(&self) -> usize {
        self.entries.len()
    }
    pub fn selected_ids(&self) -> Vec<String> {
        self.entries.iter().map(|entry| entry.id.clone()).collect()
    }
    pub fn validate_artifact_destination(&self, destination: &Path) -> Result<(), ConnectionError> {
        self.guard
            .validate_destination(
                destination
                    .parent()
                    .ok_or(ConnectionError::InvalidConfiguration)?,
            )
            .map(drop)
            .map_err(|_| ConnectionError::InvalidConfiguration)
    }
    /// Export only consented staged accounts to a new private artifact outside
    /// the original computer's source grant. Password/key custody stays private.
    pub fn export_password_sealed(
        &self,
        destination: &Path,
        password: &[u8],
        binding: &[u8],
    ) -> Result<usize, ConnectionError> {
        self.validate_artifact_destination(destination)?;
        let selected: Vec<_> = self
            .entries
            .iter()
            .map(|entry| accounts::AccountId::new(&entry.id))
            .collect();
        Connector::new(self.config_root()).export_selected_accounts(
            &selected,
            destination,
            password,
            binding,
        )?;
        Ok(selected.len())
    }
    /// Run after full home copy settles. Reseal each selected account into the
    /// target vault, preserving existing unrelated accounts and the stage for
    /// retry if any account cannot be delivered. No media-profile secret fields.
    pub fn merge_into(&self, target_home: &Path) -> Result<(), ConnectionError> {
        let target = self
            .guard
            .validate_destination(target_home)
            .map_err(|_| ConnectionError::InvalidConfiguration)?;
        let connector = Connector::new(target);
        for entry in &self.entries {
            connector
                .handoff_prepared(&self.config_root(), &accounts::AccountId::new(&entry.id))?;
        }
        Ok(())
    }
}

impl PreparedAccounts {
    pub fn stage(&self) -> Arc<tempfile::TempDir> {
        self.stage.clone()
    }
    pub fn entries(&self) -> &[PreparedEntry] {
        &self.entries
    }
    pub fn destination_guard(&self) -> &DestinationGuard {
        &self.guard
    }
}
impl AccountActions {
    /// Capture the selected staged accounts under this action session's destination fence.
    pub fn prepared_accounts(
        &self,
        stage: Arc<tempfile::TempDir>,
        entries: Vec<PreparedEntry>,
    ) -> PreparedAccounts {
        PreparedAccounts {
            stage,
            entries,
            guard: self.destination_snapshot(),
        }
    }
}

#[cfg(target_os = "linux")]
mod native;

pub fn require_supported() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Prepared account checkpoints are unavailable on this host",
        ))
    }
}
fn invalid() -> io::Error {
    io::Error::other("Prepared connections could not be authenticated")
}
pub fn empty(guard: DestinationGuard) -> io::Result<PreparedAccounts> {
    require_supported()?;
    #[cfg(target_os = "linux")]
    {
        Ok(PreparedAccounts {
            stage: Arc::new(native::private_temp()?),
            entries: Vec::new(),
            guard,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = guard;
        unreachable!()
    }
}
/// Seal a snapshot while retaining the current draft generation lease when the
/// reserved checkpoint is inside the original home's preparation namespace.
pub fn save(
    handle: &PreparedAccounts,
    root: &Path,
    binding: &str,
    group: [u8; 32],
    password: &[u8],
) -> io::Result<Receipt> {
    require_supported()?;
    #[cfg(target_os = "linux")]
    {
        native::save(handle, root, binding, group, password)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (handle, root, binding, group, password);
        unreachable!()
    }
}
pub fn restore(root: &Path, receipt: &Receipt, password: &[u8]) -> io::Result<PreparedAccounts> {
    require_supported()?;
    #[cfg(target_os = "linux")]
    {
        native::restore(root, receipt, password)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (root, receipt, password);
        unreachable!()
    }
}

/// Check the ordinary destination fence before reviewing the reserved preparation
/// namespace. No source-home exception is granted by this check.
fn outside_source(handle: &PreparedAccounts, root: &Path) -> io::Result<bool> {
    require_supported()?;
    if root.file_name() != Some(std::ffi::OsStr::new("accounts")) {
        return Err(invalid());
    }
    let operation = root.parent().ok_or_else(invalid)?;
    if handle.guard.validate_destination(operation).is_ok() {
        return Ok(true);
    }
    let preparations = operation.parent().ok_or_else(invalid)?;
    if crate::session::preparation_namespace().as_deref() != Some(preparations) {
        return Err(invalid());
    }
    #[cfg(target_os = "linux")]
    {
        let admitted = crate::migration::PinnedDirectory::open_absolute(preparations)?;
        admitted.validate_destination()?;
    }
    Ok(false)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DestinationChoice {
    Computer,
    Another,
    Desktop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IdentityKind {
    New,
    Backup,
    Enrolled,
    CurrentDevice,
}
/// Mirror of the journey's private restart choices. Unknown fields are refused,
/// so this read model must accept every field that record writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicChoices {
    destination: Option<DestinationChoice>,
    migrate: Option<bool>,
    categories: [bool; 3],
    #[serde(default = "default_browser_domains")]
    browser_domains: [bool; 2],
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    excluded_flatpak_apps: std::collections::BTreeSet<String>,
    device_label: String,
    #[serde(default = "install_profile_model::default_purpose")]
    purpose: String,
    #[serde(default)]
    deferred: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<TargetDraft>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetDraft {
    manufacturer: String,
    model: String,
    variant: String,
    board: String,
    sku: String,
    architecture: String,
    #[serde(default, skip_serializing_if = "is_false")]
    report_omitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report: Option<crate::support_report::SupportReport>,
}
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}
fn default_browser_domains() -> [bool; 2] {
    [true; 2]
}
impl PublicChoices {
    fn validate(&self) -> io::Result<()> {
        if !install_profile_model::valid_purpose(&self.purpose) {
            return Err(invalid());
        }
        if self.excluded_flatpak_apps.len() > 4096
            || self
                .excluded_flatpak_apps
                .iter()
                .any(|id| id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control))
        {
            return Err(invalid());
        }
        if self.device_label.len() > 64 || self.device_label.chars().any(char::is_control) {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    operation: String,
    kind: IdentityKind,
    root_key: Option<[u8; 32]>,
    device_key: Option<[u8; 32]>,
    record_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_device: Option<crate::prepared_identity::CurrentDeviceDeclaration>,
}
impl Pointer {
    fn validate(&self) -> io::Result<()> {
        if uuid::Uuid::parse_str(&self.operation)
            .map_err(|_| invalid())?
            .to_string()
            != self.operation
        {
            return Err(invalid());
        }
        if self.current_device.is_some()
            != (self.kind == IdentityKind::CurrentDevice && self.root_key.is_some())
        {
            return Err(invalid());
        }
        if let Some(current) = &self.current_device {
            if Some(current.public_key()) != self.root_key
                || Some(current.device_public_key()) != self.device_key
                || Some(current.record_name()) != self.record_name.as_deref()
            {
                return Err(invalid());
            }
        }
        match (self.kind, self.root_key, self.device_key, &self.record_name) {
            (_, None, None, None)
            | (IdentityKind::New | IdentityKind::Backup, Some(_), None, None) => Ok(()),
            (
                IdentityKind::Enrolled | IdentityKind::CurrentDevice,
                Some(_),
                Some(_),
                Some(name),
            ) if Path::new(name).extension() == Some(OsStr::new("json"))
                && name.len() <= 128
                && !name.contains(['/', '\\'])
                && name != "."
                && name != ".."
                && !name.chars().any(char::is_control) =>
            {
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
}

/// Descriptive restart source read under a held backend storage lock. It is
/// deliberately not a constructor for identity or original-home authority.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftSource {
    schema: u32,
    choices: Option<PublicChoices>,
    preparation: Option<Pointer>,
    #[serde(default)]
    account_checkpoint: Option<Receipt>,
}
impl DraftSource {
    pub fn validate(&self) -> io::Result<()> {
        if self.schema != 1 || self.preparation.is_some() != self.choices.is_some() {
            return Err(invalid());
        }
        if let Some(choices) = &self.choices {
            choices.validate()?;
        }
        if let Some(pointer) = &self.preparation {
            pointer.validate()?;
        }
        if let Some(receipt) = &self.account_checkpoint {
            receipt.validate()?;
            let pointer = self.preparation.as_ref().ok_or_else(invalid)?;
            if receipt.binding() != pointer.operation || Some(receipt.group()) != pointer.root_key {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> serde_json::Value {
        let operation = uuid::Uuid::new_v4().to_string();
        serde_json::json!({
            "schema": 1,
            "choices": {
                "destination": "another",
                "migrate": true,
                "categories": [true, false, true],
                "device_label": "Laptop"
            },
            "preparation": {
                "operation": operation,
                "kind": "new",
                "root_key": vec![7; 32],
                "device_key": null,
                "record_name": null
            },
            "account_checkpoint": {
                "schema": 1,
                "filename": format!("{}.sealed", uuid::Uuid::new_v4()),
                "sealed_bytes": 256,
                "ciphertext_sha256": vec![1; 32],
                "binding": operation,
                "group": vec![7; 32],
                "accounts": 2
            }
        })
    }

    #[test]
    fn draft_source_requires_matching_checkpoint_preparation() {
        let value = source();
        let reviewed: DraftSource = serde_json::from_value(value.clone()).unwrap();
        reviewed.validate().unwrap();
        let mut foreign = value;
        foreign["account_checkpoint"]["group"] = serde_json::json!(vec![8; 32]);
        let foreign: DraftSource = serde_json::from_value(foreign).unwrap();
        assert!(foreign.validate().is_err());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_host_never_creates_account_checkpoint_custody() {
        assert_eq!(
            require_supported().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let guard = AccountActions::default().destination_snapshot();
        assert_eq!(
            empty(guard).err().unwrap().kind(),
            io::ErrorKind::Unsupported
        );
        let root = tempfile::tempdir().unwrap();
        let receipt: Receipt =
            serde_json::from_value(source()["account_checkpoint"].clone()).unwrap();
        assert_eq!(
            restore(root.path(), &receipt, b"password")
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_sealed_bound_matches_portable_format() {
        assert_eq!(native::sealed_bound(), Some(SEALED_LIMIT));
    }
}
