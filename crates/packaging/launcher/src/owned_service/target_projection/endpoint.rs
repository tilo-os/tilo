//! Canonical `NamespaceOwner`'s target endpoint; no public path constructor.
use super::{
    Arc, File, MetadataExt, OsStringExt, Record, SealedSelectedParentScope,
    TargetProjectionAdmission, TargetProjectionAuthority, TargetProjectionMode, UnixStream, Write,
    io, mount_id, receive_record, refused, root, selected_parent_scope,
};
use getrandom::rand_core::TryRng as _;
use runtime_portal::process_binding::{self, ExecutingPeer};
use std::os::fd::AsFd;

/// The root owner retains this together with the actual service child. Exported
/// view FDs are valid only while this endpoint/source authority remains current.
pub struct TargetProjectionEndpoint {
    service: super::super::ServiceEndpoint,
    authority: Arc<dyn TargetProjectionAuthority>,
    mount_namespace: File,
    view: File,
    detached_view: std::sync::Mutex<Option<File>>,
    exposure: Arc<super::exposure::ExposureCustody>,
    pub(super) record: Record,
    ready: std::sync::Mutex<Option<UnixStream>>,
    initialized: std::sync::atomic::AtomicBool,
    observation_failed: std::sync::atomic::AtomicBool,
    observation_peer: std::sync::Mutex<Option<ExecutingPeer>>,
}
impl TargetProjectionEndpoint {
    /// Called only by canonical `NamespaceOwner` after it held the actual init
    /// pidfd/PID namespace. An ordinary callback cannot mint mount authority.
    pub(in crate::owned_service) fn receive(
        service: super::super::ServiceEndpoint,
        admission: &TargetProjectionAdmission,
    ) -> io::Result<Self> {
        admission.check()?;
        service.custody()?;
        let mut stream = admission
            .receiver
            .lock()
            .map_err(|_| io::Error::other("Target ready custody poisoned"))?
            .take()
            .ok_or_else(|| refused("Target ready custody already taken"))?;
        bounded(&stream)?;
        let peer = authenticate(&mut stream, 0)?;
        if peer.pid() != service.init_pid {
            return Err(refused("Target bootstrap is not owned namespace init"));
        }
        let mut held = Vec::new();
        for index in 0..4u8 {
            let (bytes, fd) = runtime_portal::fd_transport::recv_with_fd(stream.as_fd())?;
            let fd = fd.ok_or_else(|| refused("Target root ready descriptor missing"))?;
            if bytes != [index] || !peer.current() {
                return Err(refused("Target root ready descriptor differs"));
            }
            held.push(File::from(fd));
            // Per-descriptor acknowledgement prevents SOCK_STREAM coalescing.
            stream.write_all(&[1])?;
        }
        let detached_view = held.pop().ok_or(io::ErrorKind::InvalidData)?;
        if mount_id(&detached_view)? == mount_id(held.first().ok_or(io::ErrorKind::InvalidData)?)? {
            return Err(refused("Target detached clone mount differs"));
        }
        let receipt = held.pop().ok_or(io::ErrorKind::InvalidData)?;
        let mount_namespace = held.pop().ok_or(io::ErrorKind::InvalidData)?;
        let view = held.pop().ok_or(io::ErrorKind::InvalidData)?;
        let record = receive_record(&receipt)?;
        record.validate(true)?;
        if mount_id(&detached_view)? != record.clone_mount_id.ok_or(io::ErrorKind::InvalidData)? {
            return Err(refused("Target detached clone sealed identity differs"));
        }
        let expected = &admission.launch_record()?;
        if record.selected_parent != expected.selected_parent
            || record.mode != expected.mode
            || record.uid != expected.uid
            || record.gid != expected.gid
            || record.context_sha256 != expected.context_sha256
            || record.source_view_sha256 != expected.source_view_sha256
            || record.name != expected.name
            || record.fuse != expected.fuse
            || record.baseline != expected.baseline
            || record.namespaces.last() != expected.namespaces.last()
            || record
                .namespaces
                .get(..5)
                .ok_or(io::ErrorKind::InvalidData)?
                .iter()
                .zip(
                    expected
                        .namespaces
                        .get(..5)
                        .ok_or(io::ErrorKind::InvalidData)?,
                )
                .any(|(a, b)| a == b)
        {
            return Err(refused("Root ready record differs from native admission"));
        }
        let m = mount_namespace.metadata()?;
        let p = service.namespace.metadata()?;
        if Some(&(m.dev(), m.ino())) != record.namespaces.first()
            || Some(&(p.dev(), p.ino())) != record.namespaces.get(1)
            || mount_id(&view)? != record.mount_id.ok_or(io::ErrorKind::InvalidData)?
            || !peer.current()
        {
            return Err(refused("Root ready kernel endpoint differs"));
        }
        admission.check()?;
        service.custody()?;
        Ok(Self {
            service,
            authority: admission.authority.clone(),
            mount_namespace,
            view,
            detached_view: std::sync::Mutex::new(Some(detached_view)),
            exposure: Arc::clone(&admission.exposure),
            record,
            ready: std::sync::Mutex::new(Some(stream)),
            initialized: std::sync::atomic::AtomicBool::new(false),
            observation_failed: std::sync::atomic::AtomicBool::new(false),
            observation_peer: std::sync::Mutex::new(None),
        })
    }
    /// Independent kernel stop observation remains available after grant or
    /// target-session revocation. It never mints a successful exit receipt.
    pub fn projection_mode(&self) -> TargetProjectionMode {
        self.record.mode
    }
    ///
    /// # Errors
    /// Returns an error if the owned service's stopped state cannot be observed.
    pub fn observed_stopped(&self) -> io::Result<bool> {
        self.service.observed_stopped()
    }
    pub fn check(&self) -> io::Result<()> {
        root()?;
        if self
            .observation_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(refused("Target observation transcript retired"));
        }
        self.authority.check()?;
        let c = self.authority.target_credentials()?;
        if selected_parent_scope(self.authority.as_ref())? != self.record.selected_parent
            || self.authority.projection_mode() != self.record.mode
            || c.uid != self.record.uid
            || c.gid != self.record.gid
            || self.authority.context_sha256() != self.record.context_sha256
            || self.authority.source_view_sha256() != self.record.source_view_sha256
            || self.authority.presentation_name()?.into_vec() != self.record.name
        {
            return Err(refused("Target endpoint authority binding differs"));
        }
        self.service.custody()?;
        let m = self.mount_namespace.metadata()?;
        if Some(&(m.dev(), m.ino())) != self.record.namespaces.first()
            || mount_id(&self.view)? != self.record.mount_id.ok_or(io::ErrorKind::InvalidData)?
        {
            return Err(refused("Target projection mount custody differs"));
        }
        self.service.custody().map(|_| ())
    }
    /// Available before FUSE initialization. The caller starts the actual root
    /// Capnp relay on this stream before waiting for `observe_ready()`. The target
    /// worker completes QPE1 on its connected control stream before first RPC.
    pub fn connect_control(&self) -> io::Result<UnixStream> {
        self.check()?;
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(10))
            .ok_or(io::ErrorKind::InvalidData)?;
        let mut stream = loop {
            self.check()?;
            match self.service.connect() {
                Ok(stream) => break stream,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        };
        let peer = authenticate(&mut stream, self.record.uid)?;
        if peer.pid() != self.service.init_pid || peer.gid() != self.record.gid {
            return Err(refused(
                "Target control peer differs from owned target task",
            ));
        }
        self.check()?;
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        Ok(stream)
    }
    /// Observe actual post-drop worker readiness, then verify the kernel mount.
    /// The relay must already be serving: statfs can require an actual source RPC.
    pub fn observe_ready(&self) -> io::Result<()> {
        self.check()?;
        let mut slot = self
            .ready
            .lock()
            .map_err(|_| io::Error::other("Target ready custody poisoned"))?;
        let stream = slot
            .as_mut()
            .ok_or_else(|| refused("Target ready already observed"))?;
        let peer = crate::retained_view::qpe_exchange(
            stream,
            &self.service.cancellation,
            std::time::Duration::from_secs(10),
            |stream| authenticate(stream, self.record.uid),
        )?;
        if peer.pid() != self.service.init_pid || peer.gid() != self.record.gid {
            return Err(refused("Target ready is not owned target task"));
        }
        *self
            .observation_peer
            .lock()
            .map_err(|_| io::Error::other("Target observation peer poisoned"))? = Some(peer);
        drop(slot);
        let observed = self.observe_object_inner(&self.view, false)?;
        if observed.facts.mode & libc::S_IFMT != libc::S_IFDIR
            || observed.facts.uid != self.record.uid
        {
            return Err(refused("Ready projection root differs"));
        }
        observed.check()?;
        self.initialized
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
    pub(in crate::owned_service) fn mount_namespace_clone(&self) -> io::Result<File> {
        self.check()?;
        self.mount_namespace.try_clone()
    }
    /// Metadata observations come only from the authenticated fully dropped
    /// canonical worker. This token grants no read/write/lookup permission.
    ///
    /// # Errors
    /// Returns an error if the worker is not ready or the held mount cannot be authenticated and measured.
    pub fn observe_held_view(&self) -> io::Result<ObservedTargetObject<'_>> {
        self.observe_object_inner(&self.view, true)
    }
    fn observe_object_inner(
        &self,
        file: &File,
        require_ready: bool,
    ) -> io::Result<ObservedTargetObject<'_>> {
        self.check()?;
        if require_ready && !self.initialized.load(std::sync::atomic::Ordering::Acquire) {
            return Err(refused("Target projection is not initialized"));
        }
        if mount_id(file)? != self.record.mount_id.ok_or(io::ErrorKind::InvalidData)? {
            return Err(refused("Target observed descriptor mount differs"));
        }
        self.check_observer()?;
        let held = file.try_clone()?;
        let mut slot = self
            .ready
            .lock()
            .map_err(|_| io::Error::other("Target observation transport poisoned"))?;
        let stream = slot
            .as_mut()
            .ok_or_else(|| refused("Target observation channel stopped"))?;
        let result =
            super::observation::request(stream, &held, &self.record, &self.service.cancellation);
        let facts = match result {
            Ok(facts) => facts,
            Err(error) => {
                // A stalled worker may be in a genuine kernel FUSE syscall. Cancel
                // the existing canonical namespace; root keeps custody until reap.
                self.fail_observation(stream);
                self.service.cancellation.cancel();
                return Err(error);
            }
        };
        drop(slot);
        self.check_observer()?;
        self.check()?;
        if facts.mount_id != self.record.mount_id.ok_or(io::ErrorKind::InvalidData)?
            || facts.fs_type != 0x6573_5546
        {
            return Err(refused("Target observation facts differ from held mount"));
        }
        let observed = ObservedTargetObject {
            endpoint: self,
            file: held,
            facts,
        };
        observed.check()?;
        Ok(observed)
    }
    pub(super) fn measure_exposure(&self, file: &File) -> io::Result<super::observation::Facts> {
        self.check_lookup_session()?;
        let original = self.observe_held_view()?;
        let mut slot = self
            .ready
            .lock()
            .map_err(|_| io::Error::other("Exposure transport poisoned"))?;
        let stream = slot.as_mut().ok_or(io::ErrorKind::NotConnected)?;
        let result = super::observation::request_exposure(
            stream,
            file,
            &self.record,
            &self.service.cancellation,
        );
        let facts = match result {
            Ok(super::observation::ExposureResponse::Measured(facts)) => facts,
            Ok(super::observation::ExposureResponse::Refused(error)) => {
                drop(slot);
                self.check_lookup_session()?;
                return Err(error);
            }
            Err(error) => {
                self.fail_observation(stream);
                // Exposure owner must remove placed clone before target stop.
                return Err(error);
            }
        };
        drop(slot);
        original.check()?;
        self.check_lookup_session()?;
        if (facts.dev, facts.ino) != (original.dev()?, original.ino()?)
            || facts.mode & libc::S_IFMT != libc::S_IFDIR
            || facts.fs_type != 0x6573_5546
            || (facts.uid, facts.gid) != (self.record.uid, self.record.gid)
            || facts.mount_id != mount_id(file)?
            || facts.mount_id == original.mount_id()?
            || Some(facts.mount_id) != self.record.clone_mount_id
        {
            return Err(refused("Exposure is not exact measured root clone"));
        }
        Ok(facts)
    }
    pub(super) fn exposure_hold(&self) -> io::Result<super::exposure::ExposureHold> {
        self.exposure.acquire()
    }
    pub(super) fn take_detached_view(&self) -> io::Result<File> {
        self.check_lookup_session()?;
        self.detached_view
            .lock()
            .map_err(|_| io::Error::other("Exposure custody poisoned"))?
            .take()
            .ok_or_else(|| io::ErrorKind::AlreadyExists.into())
    }
    fn check_observer(&self) -> io::Result<()> {
        let slot = self
            .observation_peer
            .lock()
            .map_err(|_| io::Error::other("Target observation peer poisoned"))?;
        let peer = slot
            .as_ref()
            .ok_or_else(|| refused("Target observation worker missing"))?;
        if !peer.current()
            || peer.pid() != self.service.init_pid
            || (peer.uid(), peer.gid()) != (self.record.uid, self.record.gid)
        {
            return Err(refused("Target observation worker retired or differs"));
        }
        drop(slot);
        self.service.custody().map(|_| ())
    }
    /// Bounded raw-component lookup only. This is not a content opener or a
    /// source permission grant; root must separately review the selection.
    pub fn lookup_selected(&self, components: &[Vec<u8>]) -> io::Result<LookedUpSelected<'_>> {
        super::selected_lookup::validate_components(components)?;
        if !self.initialized.load(std::sync::atomic::Ordering::Acquire) {
            return Err(refused("Selected lookup before target readiness"));
        }
        self.check_lookup_session()?;
        let mut slot = self
            .ready
            .lock()
            .map_err(|_| io::Error::other("Selected lookup transport poisoned"))?;
        let stream = slot
            .as_mut()
            .ok_or_else(|| refused("Selected lookup channel stopped"))?;
        let response = super::selected_lookup::request(
            stream,
            &self.view,
            components,
            &self.record,
            &self.service.cancellation,
        );
        let facts = match response {
            Ok(super::selected_lookup::Response::Found(facts)) => facts,
            Ok(super::selected_lookup::Response::Refused(error)) => {
                drop(slot);
                self.check_lookup_session()?;
                return Err(error);
            }
            Err(error) => {
                self.fail_observation(stream);
                self.service.cancellation.cancel();
                return Err(error);
            }
        };
        drop(slot);
        self.check_lookup_session()?;
        let result = LookedUpSelected {
            parent: ObservedTargetObject {
                endpoint: self,
                file: facts.parent,
                facts: facts.parent_facts,
            },
            selected: ObservedTargetObject {
                endpoint: self,
                file: facts.selected,
                facts: facts.selected_facts,
            },
            components: components.to_vec(),
            components_sha256: super::selected_lookup::components_sha256(components)?,
        };
        result.check()?;
        Ok(result)
    }
    /// Reopen only an opaque lookup from this same endpoint, with a fresh
    /// bounded worker operation and strict readonly OFD. No root path reopen.
    ///
    /// # Errors
    /// Returns an error if the selection has changed, belongs to another endpoint, or cannot be reopened read-only.
    pub fn reopen_selected<'a>(
        &'a self,
        selection: &LookedUpSelected<'_>,
    ) -> io::Result<ObservedTargetObject<'a>> {
        selection.check()?;
        if !std::ptr::eq(self, selection.parent.endpoint)
            || !std::ptr::eq(self, selection.selected.endpoint)
        {
            return Err(refused("Selected read lookup endpoint differs"));
        }
        self.check_lookup_session()?;
        let mut slot = self
            .ready
            .lock()
            .map_err(|_| io::Error::other("Selected read transport poisoned"))?;
        let stream = slot
            .as_mut()
            .ok_or_else(|| refused("Selected read channel stopped"))?;
        let response = super::selected_read::request(
            stream,
            super::selected_read::ReadRequest {
                view: &self.view,
                parent: &selection.parent.file,
                selected: &selection.selected.file,
                selected_facts: &selection.selected.facts,
                components: &selection.components,
            },
            &self.record,
            &self.service.cancellation,
        );
        let (file, facts) = match response {
            Ok(super::selected_read::Response::Opened(file, facts)) => (file, facts),
            Ok(super::selected_read::Response::Refused(error)) => {
                drop(slot);
                self.check_lookup_session()?;
                return Err(error);
            }
            Err(error) => {
                self.fail_observation(stream);
                self.service.cancellation.cancel();
                return Err(error);
            }
        };
        drop(slot);
        self.check_lookup_session()?;
        let result = ObservedTargetObject {
            endpoint: self,
            file,
            facts,
        };
        result.check()?;
        super::selected_read::check_read_held(&result.file, &result.facts)?;
        Ok(result)
    }
    fn fail_observation(&self, stream: &UnixStream) {
        self.observation_failed
            .store(true, std::sync::atomic::Ordering::Release);
        self.service.cancellation.cancel();
        // Linearizes channel EOF against clone acquisition just like signals.
        // Keep unusable transport owned while any actual exposure is live.
        if self.exposure.begin_stop() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
    fn check_lookup_session(&self) -> io::Result<()> {
        let result = self.check().and_then(|()| self.check_observer());
        if result.is_err() {
            self.service.cancellation.cancel();
            if let Ok(mut slot) = self.ready.lock()
                && let Some(stream) = slot.as_mut()
            {
                self.fail_observation(stream);
            }
        }
        result
    }
    pub fn held_view(&self) -> io::Result<File> {
        self.check()?;
        if !self.initialized.load(std::sync::atomic::Ordering::Acquire) {
            return Err(refused("Target projection is not initialized"));
        }
        let view = self.view.try_clone()?;
        self.check()?;
        Ok(view)
    }
}
fn bounded(stream: &UnixStream) -> io::Result<()> {
    let timeout = Some(std::time::Duration::from_secs(10));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)
}
fn authenticate(stream: &mut UnixStream, uid: u32) -> io::Result<ExecutingPeer> {
    bounded(stream)?;
    process_binding::arm(stream)?;
    let mut challenge = [0; 32];
    getrandom::SysRng
        .try_fill_bytes(&mut challenge)
        .map_err(io::Error::other)?;
    stream.write_all(&process_binding::MAGIC)?;
    stream.write_all(&challenge)?;
    let peer = process_binding::receive(stream, &challenge, uid)?;
    stream.write_all(&[1])?;
    bounded(stream)?;
    Ok(peer)
}

/// Nonserializable evidence of the exact descriptor measured by this endpoint's
/// actual signed dropped worker. No public DTO/path/FD constructor exists.
pub struct ObservedTargetObject<'a> {
    pub(super) endpoint: &'a TargetProjectionEndpoint,
    file: File,
    facts: super::observation::Facts,
}
impl ObservedTargetObject<'_> {
    pub fn check(&self) -> io::Result<()> {
        self.endpoint.check()?;
        self.endpoint.check_observer()?;
        if mount_id(&self.file)? != self.facts.mount_id
            || self.facts.mount_id
                != self
                    .endpoint
                    .record
                    .mount_id
                    .ok_or(io::ErrorKind::InvalidData)?
        {
            return Err(refused("Observed target descriptor mount retired"));
        }
        Ok(())
    }
    pub fn file(&self) -> io::Result<&File> {
        self.check()?;
        Ok(&self.file)
    }
    pub fn dev(&self) -> io::Result<u64> {
        self.check()?;
        Ok(self.facts.dev)
    }
    pub fn ino(&self) -> io::Result<u64> {
        self.check()?;
        Ok(self.facts.ino)
    }
    pub fn mount_id(&self) -> io::Result<u64> {
        self.check()?;
        Ok(self.facts.mount_id)
    }
    pub fn uid(&self) -> io::Result<u32> {
        self.check()?;
        Ok(self.facts.uid)
    }
    pub fn gid(&self) -> io::Result<u32> {
        self.check()?;
        Ok(self.facts.gid)
    }
    pub fn is_directory(&self) -> io::Result<bool> {
        self.check()?;
        Ok(self.facts.mode & libc::S_IFMT == libc::S_IFDIR)
    }
    pub fn is_file(&self) -> io::Result<bool> {
        self.check()?;
        Ok(self.facts.mode & libc::S_IFMT == libc::S_IFREG)
    }
    pub fn filesystem_type(&self) -> io::Result<i64> {
        self.check()?;
        Ok(self.facts.fs_type)
    }
    pub fn source_view_sha256(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        Ok(self.endpoint.record.source_view_sha256)
    }
    pub fn context_sha256(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        Ok(self.endpoint.record.context_sha256)
    }
}

/// Exact held parent/selected pair from a bracketed raw-leaf lookup by the
/// authenticated worker. No public FD/DTO constructor or source-write grant.
pub struct LookedUpSelected<'a> {
    parent: ObservedTargetObject<'a>,
    selected: ObservedTargetObject<'a>,
    components: Vec<Vec<u8>>,
    components_sha256: [u8; 32],
}
impl LookedUpSelected<'_> {
    pub fn check(&self) -> io::Result<()> {
        self.parent.endpoint.check_lookup_session()?;
        self.parent.check()?;
        self.selected.check()?;
        if !self.parent.is_directory()?
            || !self.selected.is_file()?
            || super::selected_lookup::components_sha256(&self.components)?
                != self.components_sha256
        {
            return Err(refused("Selected lookup object binding differs"));
        }
        super::selected_lookup::check_held(&self.parent.file, &self.parent.facts)?;
        super::selected_lookup::check_held(&self.selected.file, &self.selected.facts)?;
        Ok(())
    }
    /// Repeat the same fixed worker lookup before admission. A changed parent
    /// or raw child never silently retargets the previously held object.
    pub fn revalidate(&self) -> io::Result<()> {
        self.check()?;
        let current = self.parent.endpoint.lookup_selected(&self.components)?;
        if (
            current.parent.facts.dev,
            current.parent.facts.ino,
            current.parent.facts.mount_id,
        ) != (
            self.parent.facts.dev,
            self.parent.facts.ino,
            self.parent.facts.mount_id,
        ) || (
            current.selected.facts.dev,
            current.selected.facts.ino,
            current.selected.facts.mount_id,
        ) != (
            self.selected.facts.dev,
            self.selected.facts.ino,
            self.selected.facts.mount_id,
        ) {
            return Err(refused("Selected lookup parent or child replaced"));
        }
        self.check()
    }
    pub fn parent(&self) -> io::Result<&ObservedTargetObject<'_>> {
        self.check()?;
        Ok(&self.parent)
    }
    pub fn selected(&self) -> io::Result<&ObservedTargetObject<'_>> {
        self.check()?;
        Ok(&self.selected)
    }
    pub fn components(&self) -> io::Result<&[Vec<u8>]> {
        self.check()?;
        Ok(&self.components)
    }
    pub fn components_sha256(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        Ok(self.components_sha256)
    }
}

/// Exact endpoint-minted selected edit issuance. No public FD, scope, callback,
/// UID or descriptive grant constructor exists. Root keeps the whole session.
pub struct SelectedParentIssue<'a> {
    pub(super) endpoint: &'a TargetProjectionEndpoint,
    binding: crate::retained_view::ConsumerLaunchBinding,
    peer: &'a ExecutingPeer,
    parent: ObservedTargetObject<'a>,
    selected: LookedUpSelected<'a>,
    scope: SealedSelectedParentScope,
    cancellation: &'a super::super::ServiceCancellation,
}
impl TargetProjectionEndpoint {
    pub fn review_selected_parent_issue<'a>(
        &'a self,
        binding: &crate::retained_view::ConsumerLaunchBinding,
        peer: &'a ExecutingPeer,
        cancellation: &'a super::super::ServiceCancellation,
    ) -> io::Result<SelectedParentIssue<'a>> {
        if cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.check()?;
        let scope = self
            .record
            .selected_parent
            .clone()
            .ok_or_else(|| refused("Selected edit endpoint lacks sealed scope"))?;
        if cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let parent = self.observe_held_view()?;
        if cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let selected = self.lookup_selected(std::slice::from_ref(&scope.raw_leaf))?;
        let issue = SelectedParentIssue {
            endpoint: self,
            binding: binding.clone(),
            peer,
            parent,
            selected,
            scope,
            cancellation,
        };
        issue.check()?;
        Ok(issue)
    }
}
impl SelectedParentIssue<'_> {
    pub(crate) fn check(&self) -> io::Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.endpoint.check()?;
        self.scope.validate(&self.endpoint.record)?;
        self.binding.verify_peer_program(self.peer)?;
        self.binding.verify_signed_gui_package()?;
        if self.endpoint.record.mode != TargetProjectionMode::SelectedParent
            || self.endpoint.record.selected_parent.as_ref() != Some(&self.scope)
            || !self.binding.writable
            || self.binding.resource_record != self.scope.resource_record
            || self.binding.context_sha256()? != hex::encode(self.scope.target_context_sha256)
            || (self.binding.uid, self.binding.gid)
                != (self.endpoint.record.uid, self.endpoint.record.gid)
        {
            return Err(refused(
                "Selected edit consumer differs from sealed endpoint",
            ));
        }
        self.parent.check()?;
        self.selected.revalidate()?;
        let parent = self.selected.parent()?;
        if !self.parent.is_directory()?
            || !self.selected.selected()?.is_file()?
            || (
                self.parent.dev()?,
                self.parent.ino()?,
                self.parent.mount_id()?,
            ) != (parent.dev()?, parent.ino()?, parent.mount_id()?)
            || self.parent.filesystem_type()? != 0x6573_5546
            || self.parent.source_view_sha256()? != self.scope.source_view_sha256
            || self.parent.context_sha256()? != self.scope.target_context_sha256
            || (self.parent.uid()?, self.parent.gid()?) != (self.binding.uid, self.binding.gid)
        {
            return Err(refused(
                "Selected edit actual parent/leaf observation differs",
            ));
        }
        self.endpoint.check()
    }
    pub(crate) fn binding(&self) -> &crate::retained_view::ConsumerLaunchBinding {
        &self.binding
    }
    pub(crate) fn peer(&self) -> &ExecutingPeer {
        self.peer
    }
    pub(crate) fn parent(&self) -> &ObservedTargetObject<'_> {
        &self.parent
    }
    pub(crate) fn selected(&self) -> io::Result<&ObservedTargetObject<'_>> {
        self.selected.selected()
    }
    pub(crate) fn leaf(&self) -> &[u8] {
        &self.scope.raw_leaf
    }
    pub(crate) fn request_id(&self) -> &str {
        &self.scope.request_id
    }
    pub(crate) fn origin_context(&self) -> [u8; 32] {
        self.scope.source_context_sha256
    }
    pub(crate) fn native_parent(&self) -> [u64; 2] {
        self.scope.source_parent_identity
    }
    pub(crate) fn native_selected(&self) -> [u64; 2] {
        self.scope.source_selected_identity
    }
}
