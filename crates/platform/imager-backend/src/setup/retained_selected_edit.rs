//! Private editor admission for a fresh, exclusive original-credential source.
//! Labels describe the request; only the installed person proof, exact executing
//! launcher and independently replayed signed command/package can admit it.
use super::installed_retained_actor::InstalledRetainedLaunchInput;
mod replacement_authority;
use launcher::retained_view::ConsumerLaunchBinding;
pub(super) use replacement_authority::{
    AcknowledgedSelectedReplacement, CertifiedNativeHistoryDaemon, ObservedReplacementFinished,
    PendingDaemonEnrollment, PendingSelectedReplacement, RootSelectedReplacementSeed,
};
use runtime_collab::open_selected::v2::{RootRequest, SelectedAccess};
use runtime_portal::process_binding::ExecutingPeer;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

/// No serialized constructor or callbacks can substitute editor admission.
/// Originating Files custody is required through genuine namespace adoption.
/// Afterwards fresh editor admission alone owns current source authority.
pub(super) struct RootSelectedEditor {
    input: Arc<InstalledRetainedLaunchInput>,
    launcher: Arc<ExecutingPeer>,
    binding: ConsumerLaunchBinding,
    request: RootRequest,
    cancel: Arc<AtomicBool>,
    failed: AtomicBool,
}
impl RootSelectedEditor {
    pub(super) fn admit(
        input: Arc<InstalledRetainedLaunchInput>,
        launcher: Arc<ExecutingPeer>,
        binding: ConsumerLaunchBinding,
        request: RootRequest,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Arc<Self>> {
        let value = Arc::new(Self {
            input,
            launcher,
            binding,
            request,
            cancel,
            failed: AtomicBool::new(false),
        });
        value.check()?;
        Ok(value)
    }
    pub(super) fn input(&self) -> &Arc<InstalledRetainedLaunchInput> {
        &self.input
    }
    pub(super) fn binding(&self) -> &ConsumerLaunchBinding {
        &self.binding
    }
    pub(super) fn request(&self) -> &RootRequest {
        &self.request
    }
    pub(super) fn launcher(&self) -> &ExecutingPeer {
        &self.launcher
    }
    pub(super) fn revoke(&self) {
        self.failed.store(true, Ordering::Release);
        // This input belongs exclusively to the fresh edited source worker.
        // It is never the input/connection retained by the originating Files.
        self.input.revoke();
    }
    pub(super) fn check(&self) -> io::Result<()> {
        let result = self.check_current();
        if result.is_err() {
            self.revoke();
        }
        result
    }
    fn check_current(&self) -> io::Result<()> {
        if rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || self.failed.load(Ordering::Acquire)
            || self.cancel.load(Ordering::Acquire)
        {
            return Err(refused());
        }
        self.input.revalidate(&self.cancel)?;
        self.binding.matches_peer(&self.launcher)?;
        self.binding.verify_peer_program(&self.launcher)?;
        let selected = self.input.binding();
        let (uid, gid) = self.input.target_credentials(&self.cancel)?;
        if !self.request.valid()
            || self.request.access != SelectedAccess::EditInParent
            || !self.binding.writable
            || !self.launcher.current()
            || (self.launcher.uid(), self.launcher.gid()) != (uid, gid)
            || self.binding.operation_id != selected.operation_id
            || self.binding.app_id != self.request.app_id
            || self.binding.app_id != selected.app_id
            || self.binding.package_sha256 != selected.package_sha256
            || self.binding.room != selected.room
            || self.binding.resource_record != self.request.resource_record()
            || selected.resource_record != self.request.resource_record()
            || self.binding.shell_context_sha256 != selected.context_sha256
            || self.binding.room
                != runtime_collab::retained_person::personal_room(&self.input.person_public_key())
        {
            return Err(refused());
        }
        // Root verifies the actual ordinary signed GUI package and command;
        // it does not adopt a wire publisher or change normal app policy.
        self.binding
            .verify_shell_context(&selected.context_sha256)?;
        self.binding.verify_signed_gui_package()?;
        self.input.revalidate(&self.cancel)?;
        self.binding.matches_peer(&self.launcher)
    }
}

/// Original Files custody, independent of the new editor proof and source.
/// The lease owns the actual Files namespace and its original target mount.
pub(super) struct RootFilesEditOrigin {
    custody: std::sync::Mutex<Option<super::retained_open_dispatch::SelectedOriginCustody>>,
    provenance: super::retained_open_selection::SelectedOriginProvenance,
    history_read_origin: Arc<super::retained_source_relay::SourceHistoryReadOrigin>,
    parent_identity: [u64; 2],
    selected_identity: [u64; 2],
    context: [u8; 32],
    source_view: [u8; 32],
}
impl RootFilesEditOrigin {
    /// The original bootstrap derives this opaque custody from a genuine
    /// writable Files selection. Arbitrary descriptor/identity tuples are not
    /// a constructor; multiple edits retain the same actual root-owned lease.
    pub(super) fn admit(
        custody: super::retained_open_dispatch::SelectedOriginCustody,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        custody.check(cancel)?;
        let found = custody.endpoint.lookup_selected(&custody.components)?;
        found.revalidate()?;
        // Byte reopening exercises the original source backend expected-inode
        // gate. Its FUSE identity is checked only as a target-view identity.
        let opened = custody.endpoint.reopen_selected(&found)?;
        if [opened.dev()?, opened.ino()?] != [found.selected()?.dev()?, found.selected()?.ino()?] {
            return Err(refused());
        }
        custody.native_origin.check_named()?;
        let parent_identity = [found.parent()?.dev()?, found.parent()?.ino()?];
        let selected_identity = [found.selected()?.dev()?, found.selected()?.ino()?];
        let context = found.selected()?.context_sha256()?;
        let source_view = found.selected()?.source_view_sha256()?;
        drop(opened);
        drop(found);
        custody.check(cancel)?;
        let provenance = custody.freeze_provenance(cancel)?;
        let history_read_origin = custody.native_origin.history_read_origin()?;
        let value = Arc::new(Self {
            provenance,
            history_read_origin,
            parent_identity,
            selected_identity,
            context,
            source_view,
            custody: std::sync::Mutex::new(Some(custody)),
        });
        value.check(cancel)?;
        Ok(value)
    }
    pub(super) fn components(&self) -> &[Vec<u8>] {
        &self.provenance.components
    }
    fn check(&self, cancel: &AtomicBool) -> io::Result<()> {
        let held = self.custody.lock().map_err(|_| refused())?;
        let custody = held.as_ref().ok_or_else(refused)?;
        custody.check(cancel)?;
        let found = custody.endpoint.lookup_selected(self.components())?;
        if [found.parent()?.dev()?, found.parent()?.ino()?] != self.parent_identity
            || [found.selected()?.dev()?, found.selected()?.ino()?] != self.selected_identity
            || found.selected()?.context_sha256()? != self.context
            || found.selected()?.source_view_sha256()? != self.source_view
        {
            return Err(refused());
        }
        found.revalidate()?;
        custody.check(cancel)
    }
    /// Caller has already retained and validated the actual editor namespace,
    /// exact fresh native source and its independent certified editor input.
    fn take_files_custody(
        &self,
    ) -> io::Result<super::retained_open_dispatch::SelectedOriginCustody> {
        self.custody
            .lock()
            .map_err(|_| refused())?
            .take()
            .ok_or_else(refused)
    }
}

/// Live Files custody is mandatory before adoption; its frozen provenance
/// survives afterwards without retaining the original lease. Current native
/// IO authority belongs solely to the fresh exclusive edited-source owner.
/// Original native IDs are never copied from FUSE observations.
pub(super) struct RootSelectedEditAdmission {
    origin: Arc<RootFilesEditOrigin>,
    source: Arc<super::retained_source_relay::selected_parent::SourceSelectedParent>,
    editor: Arc<RootSelectedEditor>,
    independent: AtomicBool,
    consumer: std::sync::Mutex<
        Option<(
            std::os::unix::net::UnixStream,
            launcher::ordinary_namespace::NamespaceWitness,
        )>,
    >,
}
impl RootSelectedEditAdmission {
    pub(super) fn admit(
        origin: Arc<RootFilesEditOrigin>,
        source: Arc<super::retained_source_relay::selected_parent::SourceSelectedParent>,
        editor: Arc<RootSelectedEditor>,
    ) -> io::Result<Arc<Self>> {
        let value = Arc::new(Self {
            origin,
            source,
            editor,
            independent: AtomicBool::new(false),
            consumer: std::sync::Mutex::new(None),
        });
        value.check()?;
        Ok(value)
    }
    pub(super) fn check(&self) -> io::Result<()> {
        let result = self.check_current();
        if result.is_err() {
            self.editor.revoke();
        }
        result
    }
    fn check_current(&self) -> io::Result<()> {
        self.editor.check()?;
        self.check_origin_or_consumer()?;
        self.check_source_provenance()?;
        self.check_origin_or_consumer()?;
        self.editor.check()
    }
    fn check_source_provenance(&self) -> io::Result<()> {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        self.source.check().map_err(io::Error::other)?;
        let original = &self.origin.provenance;
        let edited = self.editor.input.directory().metadata()?;
        if !Arc::ptr_eq(self.source.input(), self.editor.input())
            || original.uses_input(self.editor.input())
            || original.source_root != [edited.dev(), edited.ino()]
            || &original.original != self.editor.input.original()
            || original.person != self.editor.input.person_public_key()
            || original.operation != self.editor.input.binding().operation_id
            || self
                .source
                .components()
                .iter()
                .map(|c| c.as_os_str().as_bytes().to_vec())
                .collect::<Vec<_>>()
                != original.components
            || self.editor.request.components != original.components
            || self.source.parent_identity() != original.native_parent
            || self.source.selected_identity() != original.native_selected
        {
            return Err(refused());
        }
        self.editor.check()
    }
    /// Only the canonical selected-channel handoff calls this after receiving
    /// the actual namespace witness. The exact root lease socket remains owned
    /// through namespace stop; closing Files afterwards does not kill an editor.
    pub(super) fn adopt_consumer(
        &self,
        stream: std::os::unix::net::UnixStream,
        witness: launcher::ordinary_namespace::NamespaceWitness,
    ) -> io::Result<()> {
        // Delivery has already created an owned kernel subject. Transfer its
        // exact witness and lease before checking current authority; a revoked
        // certificate or exited launcher must not erase namespace custody.
        let mut consumer = self
            .consumer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if consumer.is_some() {
            return Err(refused());
        }
        *consumer = Some((stream, witness));
        drop(consumer);
        // A witness is retained even if admission now fails. Live origin proof
        // remains mandatory until both the exact editor namespace and fresh
        // exclusive native source have actually passed their current checks.
        let result = self
            .origin
            .check(&self.editor.cancel)
            .and_then(|()| self.check());
        if result.is_err() {
            self.editor.revoke();
            return result;
        }
        let transition = (|| {
            // Serialize commitment with every origin/witness check: none can
            // observe released Files custody before independent admission.
            let consumer = self.consumer.lock().map_err(|_| refused())?;
            let (_, witness) = consumer.as_ref().ok_or_else(refused)?;
            witness.validate_for_launcher(self.editor.launcher())?;
            if witness.observed_stopped()? || self.independent.load(Ordering::Acquire) {
                return Err(refused());
            }
            self.origin.check(&self.editor.cancel)?;
            self.check_source_provenance()?;
            let custody = self.origin.take_files_custody()?;
            self.independent.store(true, Ordering::Release);
            drop(consumer);
            // Dropping the final Files-only owner may perform conservative
            // cleanup. It runs outside the admission locks.
            drop(custody);
            self.check()
        })();
        if transition.is_err() {
            self.editor.revoke();
        }
        transition
    }
    /// Readiness comes only from a retained canonical namespace witness. A
    /// staged transport or described PID cannot manufacture this observation.
    pub(super) fn consumer_admitted(&self) -> io::Result<bool> {
        self.check()?;
        let consumer = self.consumer.lock().map_err(|_| refused())?;
        // A stored witness retains cleanup custody before fallible adoption.
        // Readiness begins only after the same mutex-protected transition has
        // validated fresh editor/source authority and released Files custody.
        if !self.independent.load(Ordering::Acquire) {
            return Ok(false);
        }

        let Some((_lease, witness)) = consumer.as_ref() else {
            return Ok(false);
        };
        witness.validate_for_launcher(self.editor.launcher())?;
        Ok(!witness.observed_stopped()?)
    }
    /// Kernel stop observation remains usable after authority revocation.
    /// Missing witness or observation failure never proves namespace exit.
    pub(super) fn consumer_observed_stopped(&self) -> io::Result<bool> {
        let consumer = self
            .consumer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        consumer
            .as_ref()
            .map(|(_lease, witness)| witness.observed_stopped())
            .transpose()
            .map(|value| value.unwrap_or(false))
    }
    /// Request shutdown of the exact admitted ordinary namespace. Sending a
    /// signal is not retirement; callers retain this owner until pidfd readiness.
    pub(super) fn cancel_consumer(&self) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(refused());
        }
        let consumer = self
            .consumer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some((_lease, witness)) = consumer.as_ref() else {
            // Possibly delivered view without a witness remains unknown.
            return Err(io::ErrorKind::NotConnected.into());
        };
        if witness.observed_stopped()? {
            return Ok(());
        }
        let init = witness.init_pidfd()?;
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                init.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if !witness.observed_stopped()? {
                return Err(error);
            }
        }
        Ok(())
    }
    fn check_origin_or_consumer(&self) -> io::Result<()> {
        let consumer = self.consumer.lock().map_err(|_| refused())?;
        if let Some((_lease, witness)) = consumer.as_ref() {
            witness.validate_for_launcher(self.editor.launcher())?;
            if witness.observed_stopped()? {
                return Err(refused());
            }
            if self.independent.load(Ordering::Acquire) {
                Ok(())
            } else {
                self.origin.check(&self.editor.cancel)
            }
        } else {
            self.origin.check(&self.editor.cancel)
        }
    }
    pub(super) fn uses_input(&self, input: &Arc<InstalledRetainedLaunchInput>) -> bool {
        Arc::ptr_eq(self.editor.input(), input)
    }
    pub(super) fn uses_source(
        &self,
        source: &Arc<super::retained_source_relay::selected_parent::SourceSelectedParent>,
    ) -> bool {
        Arc::ptr_eq(&self.source, source)
    }
    pub(super) fn selected_name(&self) -> &runtime_fs::EntryName {
        self.source.selected_name()
    }
    pub(super) fn selected_identity(&self) -> [u64; 2] {
        self.source.selected_identity()
    }
    pub(super) fn parent_identity(&self) -> [u64; 2] {
        self.source.parent_identity()
    }
    pub(super) fn origin_context_sha256(&self) -> [u8; 32] {
        self.origin.context
    }
    pub(super) fn editor(&self) -> &Arc<RootSelectedEditor> {
        &self.editor
    }
}

/// The live setup-private adapter seals the actual filtered source admission,
/// never a caller-supplied projection-mode enum or serialized scope.
pub(super) struct SelectedEditProjectionAuthority {
    admission: Arc<RootSelectedEditAdmission>,
    source: Arc<
        std::sync::Mutex<super::retained_source_relay::selected_parent::RootSelectedEditSource>,
    >,
    source_view: [u8; 32],
    target_context: [u8; 32],
}
impl SelectedEditProjectionAuthority {
    pub(super) fn admit(
        admission: Arc<RootSelectedEditAdmission>,
        source: Arc<
            std::sync::Mutex<super::retained_source_relay::selected_parent::RootSelectedEditSource>,
        >,
    ) -> io::Result<Arc<Self>> {
        admission.check()?;
        let held = source.lock().map_err(|_| refused())?;
        if !held.uses_editor(admission.editor()) || !held.owns_source(&admission.source) {
            return Err(refused());
        }
        held.check_current()?;
        let source_view = held.source_view_sha256()?;
        drop(held);
        if source_view == [0; 32] {
            return Err(refused());
        }
        let target_context: [u8; 32] = hex::decode(admission.editor().binding().context_sha256()?)
            .map_err(io::Error::other)?
            .try_into()
            .map_err(|_| refused())?;
        let value = Arc::new(Self {
            admission,
            source,
            source_view,
            target_context,
        });
        launcher::owned_service::ServiceAuthority::check(value.as_ref())?;
        Ok(value)
    }
}
impl launcher::owned_service::ServiceAuthority for SelectedEditProjectionAuthority {
    fn check(&self) -> io::Result<()> {
        self.admission.check()?;
        let source = self.source.lock().map_err(|_| refused())?;
        if !source.uses_editor(self.admission.editor())
            || !source.owns_source(&self.admission.source)
        {
            return Err(refused());
        }
        source.check_current()
    }
}
impl launcher::owned_service::SelectedParentEditAuthority for SelectedEditProjectionAuthority {
    fn scope_facts(&self) -> io::Result<launcher::owned_service::SelectedParentScopeFacts> {
        launcher::owned_service::ServiceAuthority::check(self)?;
        let request = self.admission.editor().request();
        use std::os::unix::ffi::OsStrExt;
        Ok(launcher::owned_service::SelectedParentScopeFacts {
            request_id: request.request_id.clone(),
            resource_record: request.resource_record(),
            raw_leaf: self
                .admission
                .selected_name()
                .as_os_str()
                .as_bytes()
                .to_vec(),
            source_parent_identity: self.admission.parent_identity(),
            source_selected_identity: self.admission.selected_identity(),
            source_context_sha256: self.admission.origin_context_sha256(),
            source_view_sha256: self.source_view,
            target_context_sha256: self.target_context,
        })
    }
}
impl launcher::owned_service::TargetProjectionAuthority for SelectedEditProjectionAuthority {
    fn projection_mode(&self) -> launcher::owned_service::TargetProjectionMode {
        launcher::owned_service::TargetProjectionMode::SelectedParent
    }
    fn selected_parent_edit(
        &self,
    ) -> io::Result<Option<&dyn launcher::owned_service::SelectedParentEditAuthority>> {
        launcher::owned_service::ServiceAuthority::check(self)?;
        Ok(Some(self))
    }
    fn target_credentials(
        &self,
    ) -> io::Result<launcher::owned_service::TargetProjectionCredentials> {
        launcher::owned_service::ServiceAuthority::check(self)?;
        let (uid, gid) = self
            .admission
            .editor
            .input
            .target_credentials(&self.admission.editor.cancel)?;
        launcher::owned_service::TargetProjectionCredentials::admitted(uid, gid)
    }
    fn context_sha256(&self) -> [u8; 32] {
        self.target_context
    }
    fn source_view_sha256(&self) -> [u8; 32] {
        self.source_view
    }
    fn presentation_name(&self) -> io::Result<std::ffi::OsString> {
        launcher::owned_service::ServiceAuthority::check(self)?;
        Ok(self.admission.selected_name().as_os_str().to_owned())
    }
}
