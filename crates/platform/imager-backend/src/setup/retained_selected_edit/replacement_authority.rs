//! Source-bootstrap seed from genuine current editor and held Files origin.
//! It does not authorize a native start or manufacture certified history.
mod daemon;
mod job;
pub use job::ObservedReplacementFinished;
mod pending;
use super::{RootFilesEditOrigin, RootSelectedEditor, refused};
pub use daemon::{CertifiedNativeHistoryDaemon, PendingDaemonEnrollment};
pub use pending::{AcknowledgedSelectedReplacement, PendingSelectedReplacement};
use runtime_portal::retained_replacement::{
    SelectedReplacementPermitRecord, SelectedReplacementPurpose,
};
use std::{
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    sync::Arc,
};

pub struct RootSelectedReplacementSeed {
    editor: Arc<RootSelectedEditor>,
    record: SelectedReplacementPermitRecord,
    sealed: File,
    daemon: Arc<CertifiedNativeHistoryDaemon>,
    history_read_origin: Arc<crate::setup::retained_source_relay::SourceHistoryReadOrigin>,
    transfer_failed: std::sync::atomic::AtomicBool,
    replacements:
        std::sync::Mutex<std::collections::HashMap<[u8; 16], Arc<job::OwnedReplacementJob>>>,
}
impl RootSelectedReplacementSeed {
    pub(in crate::setup) fn retire_history_preceding(
        &self,
        older: &AcknowledgedSelectedReplacement,
        latest: &crate::setup::retained_history_read::RootHistoryReadContinuity,
        latest_proof: &AcknowledgedSelectedReplacement,
        source: &crate::setup::retained_source_relay::selected_parent::SourceSelectedParent,
    ) -> io::Result<()> {
        if !std::ptr::eq(self, source.replacement_seed().as_ref())
            || !Arc::ptr_eq(older.pending().seed(), source.replacement_seed())
            || !Arc::ptr_eq(latest_proof.pending().seed(), source.replacement_seed())
            || older.observation().operation == latest_proof.observation().operation
        {
            return Err(refused());
        }
        latest.check_acknowledged(latest_proof, source)?;
        source.retire_history_preceding(older)?;
        self.history_read_origin.retire_history_preceding(older)?;
        latest.check_acknowledged(latest_proof, source)
    }
    pub(in crate::setup) fn adopt_history_acknowledged(
        &self,
        proof: &AcknowledgedSelectedReplacement,
        source: &crate::setup::retained_source_relay::selected_parent::SourceSelectedParent,
    ) -> io::Result<Option<Arc<crate::setup::retained_history_read::RootHistoryReadContinuity>>>
    {
        self.check()?;
        if !std::ptr::eq(self, source.replacement_seed().as_ref()) {
            return Err(refused());
        }
        if let Some(token) = source.adopt_history_acknowledged(proof)? {
            token.check_acknowledged(proof, source)?;
            self.check()?;
            return Ok(Some(token));
        }
        self.history_read_origin.adopt_acknowledged(proof, source)
    }
    pub(in crate::setup) fn admit(
        editor: Arc<RootSelectedEditor>,
        origin: &RootFilesEditOrigin,
        daemon: Arc<CertifiedNativeHistoryDaemon>,
    ) -> io::Result<Arc<Self>> {
        editor.check()?;
        origin.check(&editor.cancel)?;
        let input = editor.input();
        input.check_selected_parent_write(&editor.cancel)?;
        daemon.check()?;
        if !daemon.uses_input(input) {
            return Err(refused());
        }
        let root = input.directory().metadata()?;
        let native = &origin.provenance;
        if native.uses_input(input)
            || native.source_root != [root.dev(), root.ino()]
            || native.person != input.person_public_key()
            || native.original != *input.original()
            || native.operation != input.binding().operation_id
            || native.components != editor.request().components
        {
            return Err(refused());
        }
        // This root-sealed derivative permits only the canonical atomic Save
        // operation. A generic rename cannot select a document history identity;
        // the source must additionally consume the actual Stage Save marker.
        let record = SelectedReplacementPermitRecord {
            schema: 2,
            purpose: SelectedReplacementPurpose::AtomicSave,
            source_owner: input.native_owner_sha256(&editor.cancel)?,
            source_operation: native.operation.clone(),
            person: native.person,
            personal_room: input.binding().room.clone(),
            editor_request_id: editor.request().request_id.clone(),
            editor_operation: editor.binding().operation_id.clone(),
            editor_context_sha256: context_digest(input.binding())?,
            package_sha256: editor.binding().package_sha256.clone(),
            source_root: native.source_root,
            parent: native.native_parent,
            selected: native.native_selected,
            components: native.components.clone(),
        };
        let bytes = record.encode()?;
        let sealed = seal(&bytes)?;
        origin.check(&editor.cancel)?;
        editor.check()?;
        let value = Arc::new(Self {
            editor,
            record,
            sealed,
            daemon,
            history_read_origin: origin.history_read_origin.clone(),
            transfer_failed: std::sync::atomic::AtomicBool::new(false),
            replacements: std::sync::Mutex::new(std::collections::HashMap::new()),
        });
        value.check()?;
        Ok(value)
    }
    pub(in crate::setup) fn uses_input(
        &self,
        input: &Arc<super::InstalledRetainedLaunchInput>,
    ) -> bool {
        Arc::ptr_eq(self.editor.input(), input)
    }
    pub(in crate::setup) fn check(&self) -> io::Result<()> {
        if self
            .transfer_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(refused());
        }
        self.daemon.check()?;
        self.editor.check()?;
        let input = self.editor.input();
        input.check_selected_parent_write(&self.editor.cancel)?;
        let root = input.directory().metadata()?;
        if self.record.purpose != SelectedReplacementPurpose::AtomicSave
            || self.record.source_owner != input.native_owner_sha256(&self.editor.cancel)?
            || self.record.source_root != [root.dev(), root.ino()]
            || self.record.source_operation != input.binding().operation_id
            || self.record.person != input.person_public_key()
            || self.record.personal_room != input.binding().room
            || self.record.editor_request_id != self.editor.request().request_id
            || self.record.components != self.editor.request().components
            || self.record.editor_context_sha256 != context_digest(input.binding())?
            || self.record.package_sha256 != self.editor.binding().package_sha256
        {
            return Err(refused());
        }
        self.record.validate()?;
        self.editor.check()
    }
    pub(in crate::setup) fn history_context_digest(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        let digest = self.daemon.context_digest()?;
        self.check()?;
        Ok(digest)
    }
    pub(in crate::setup) fn verifier_file(&self) -> io::Result<File> {
        self.check()?;
        let file = self.daemon.verifier_file()?;
        self.check()?;
        Ok(file)
    }
    pub(in crate::setup) fn permit_file(&self) -> io::Result<File> {
        self.check()?;
        let file = self.sealed.try_clone()?;
        self.check()?;
        Ok(file)
    }
}
fn context_digest(
    binding: &runtime_collab::retained_person::RetainedLaunchBinding,
) -> io::Result<[u8; 32]> {
    binding.validate()?;
    let mut digest = [0; 32];
    hex::decode_to_slice(&binding.context_sha256, &mut digest).map_err(|_| refused())?;
    Ok(digest)
}
fn seal(bytes: &[u8]) -> io::Result<File> {
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(refused());
    }
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-selected-replacement".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(bytes)?;
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o400) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 0
        || metadata.mode() & 0o7777 != 0o400
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } != seals
    {
        return Err(refused());
    }
    Ok(file)
}

#[cfg(test)]
mod context_tests {
    use super::*;
    #[test]
    fn actual_launch_binding_context_decodes_exact_bytes_and_refuses_noncanonical_binding() {
        let mut binding = runtime_collab::retained_person::RetainedLaunchBinding {
            operation_id: "12345678-1234-1234-1234-123456789abc".into(),
            app_id: "WordProcessor".into(),
            package_sha256: "11".repeat(32),
            room: "12345678-1234-1234-1234-123456789abd".into(),
            resource_record: "selected-open-v2:edit-in-parent:12345678-1234-1234-1234-123456789abe"
                .into(),
            target_uid: 1004,
            target_pid: 41,
            root_pid: 42,
            context_sha256: "ab".repeat(32),
        };
        assert_eq!(context_digest(&binding).unwrap(), [0xab; 32]);
        binding.context_sha256 = "AB".repeat(32);
        assert!(context_digest(&binding).is_err());
        binding.context_sha256 = "ab".repeat(31);
        assert!(context_digest(&binding).is_err());
        binding.context_sha256 = "ab".repeat(33);
        assert!(context_digest(&binding).is_err());
        binding.context_sha256 = "ab".repeat(32);
        binding.target_uid = 0;
        assert!(context_digest(&binding).is_err());
    }
}
