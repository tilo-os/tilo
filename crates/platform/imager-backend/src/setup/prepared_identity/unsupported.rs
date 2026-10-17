//! No provider exists on this host. Public metadata never grants identity custody.
use super::{
    CurrentDeviceDeclaration, CurrentDeviceReference, EnrolledDevice, TransportReceipt, unsupported,
};
use std::{io, path::Path, sync::Arc};
use tilo_identity_host::provisioning::IdentityStorage;
use zeroize::Zeroizing;

enum Never {}
/// Opaque identity authority. Only authenticated provider operations can create it.
pub struct PreparedIdentity {
    never: Never,
}
impl PreparedIdentity {
    pub fn recover_created(_directory: &Path) -> io::Result<Self> {
        Err(unsupported())
    }

    pub fn resume_enrolled(
        operation: &Path,
        record: &Path,
        public_key: [u8; 32],
        device_key: [u8; 32],
    ) -> io::Result<Self> {
        let _ = (operation, record, public_key, device_key);
        Err(unsupported())
    }
    pub fn create(
        directory: &Path,
        name: &str,
        color: [u8; 3],
        storage: IdentityStorage,
    ) -> io::Result<Self> {
        let _ = (directory, name, color, storage);
        Err(unsupported())
    }
    pub fn restore_backup(
        directory: &Path,
        backup: &Path,
        password: Zeroizing<String>,
    ) -> io::Result<Self> {
        let _ = (directory, backup, password);
        Err(unsupported())
    }
    pub fn from_enrolled(enrolled: Arc<EnrolledDevice>) -> io::Result<Self> {
        let _ = (enrolled,);
        Err(unsupported())
    }
    pub fn from_current(
        directory: &Path,
        current: Arc<CurrentDeviceReference>,
    ) -> io::Result<Self> {
        let _ = (directory, current);
        Err(unsupported())
    }
    pub fn resume_current(directory: &Path, claim: &CurrentDeviceDeclaration) -> io::Result<Self> {
        let _ = (directory, claim);
        Err(unsupported())
    }
    pub fn review_current() -> io::Result<CurrentDeviceReference> {
        Err(unsupported())
    }
    pub fn current_source(&self) -> Option<Arc<CurrentDeviceReference>> {
        match self.never {}
    }
    pub fn current_declaration(&self) -> io::Result<Option<CurrentDeviceDeclaration>> {
        Err(unsupported())
    }
    pub fn current_device_public_key(&self) -> Option<[u8; 32]> {
        match self.never {}
    }
    pub fn device_public_key(&self) -> Option<[u8; 32]> {
        match self.never {}
    }
    pub fn record_name(&self) -> Option<&str> {
        match self.never {}
    }
    pub fn export_current(
        &self,
        destination: &Path,
        password: &[u8],
        binding: &str,
    ) -> io::Result<TransportReceipt> {
        let _ = (destination, password, binding);
        Err(unsupported())
    }
    pub fn enrolled_device_public_key(&self) -> Option<[u8; 32]> {
        match self.never {}
    }
    pub fn export_enrolled(
        &self,
        destination: &Path,
        password: &[u8],
        binding: &str,
    ) -> io::Result<TransportReceipt> {
        let _ = (destination, password, binding);
        Err(unsupported())
    }
    pub fn metadata(&self) -> io::Result<runtime_identity::IdentityMetadata> {
        Err(unsupported())
    }
    pub fn resume(directory: &Path, expected_public_key: [u8; 32]) -> io::Result<Self> {
        let _ = (directory, expected_public_key);
        Err(unsupported())
    }
    pub fn custody_directory(&self) -> &Path {
        match self.never {}
    }
    pub fn enrolled_device_label(&self) -> Option<&str> {
        match self.never {}
    }
    pub fn enrolled_record_name(&self) -> Option<&str> {
        match self.never {}
    }
    pub fn public_key(&self) -> [u8; 32] {
        match self.never {}
    }
    pub fn update_photo(&self, photo: Option<runtime_identity::ProfilePhoto>) -> io::Result<()> {
        let _ = (photo,);
        Err(unsupported())
    }
    pub fn update_presentation(
        &self,
        name: &str,
        photo: Option<runtime_identity::ProfilePhoto>,
    ) -> io::Result<runtime_identity::IdentityMetadata> {
        let _ = (name, photo);
        Err(unsupported())
    }
    pub fn update_name(&self, name: &str) -> io::Result<runtime_identity::IdentityMetadata> {
        let _ = (name,);
        Err(unsupported())
    }
    pub fn update_profile(
        &self,
        name: &str,
        color: [u8; 3],
    ) -> io::Result<runtime_identity::IdentityMetadata> {
        let _ = (name, color);
        Err(unsupported())
    }
    pub fn export(
        &self,
        destination: &Path,
        password: Zeroizing<String>,
    ) -> io::Result<runtime_identity::PortableBackupInfo> {
        let _ = (destination, password);
        Err(unsupported())
    }
}
