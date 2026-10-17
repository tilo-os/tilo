//! Selected-open commands on the existing authenticated Shell bootstrap.
//! Wire names route an observation; fresh Files QPE and the held consumer lease
//! admit descriptors. An admission receipt never means another app was opened.
pub(super) use super::retained_open_selection::SelectedOriginCustody;
use super::retained_selected_editor_owner::{EditorStatus, SelectedEditorOwner, StageStatus};
use super::{
    retained_consumer_lease::RetainedConsumerLease,
    retained_open_selection::{RetainedOpenSelection, SelectedObjects},
};
use launcher::owned_service::ServiceCancellation;
use runtime_collab::open_selected::{
    self, RootReceipt, RootRefusal, RootReply, RootRequest, discovery, encode_reply, v2,
};
use runtime_portal::{fd_transport, process_binding};
use std::os::fd::AsFd;
use std::{
    collections::VecDeque,
    ffi::OsString,
    fs::File,
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStringExt, net::UnixStream},
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
const REQUEST_BUDGET: Duration = Duration::from_secs(10);
const PENDING_LIFETIME: Duration = Duration::from_secs(30);
const PENDING_LIMIT: usize = 16;
const EDITOR_LIMIT: usize = 8;
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
struct ObservedFiles {
    selected: File,
    parent: File,
    peer: Arc<process_binding::ExecutingPeer>,
}
struct Pending {
    request_id: String,
    app_id: String,
    access: Option<v2::SelectedAccess>,
    request: Option<v2::RootRequest>,
    claimed: bool,
    selection: RetainedOpenSelection,
    expires: Instant,
}
pub(super) struct RetainedOpenDispatcher {
    stream: UnixStream,
    cancel: Arc<AtomicBool>,
    cancellation: ServiceCancellation,
    pending: VecDeque<Pending>,
    editors: Vec<SelectedEditorOwner>,
    intake_closed: bool,
    admissions_closed: bool,
    // Routing IDs refer only to already-owned registry entries; they cannot
    // mint admission. Removed only after that actual owner proves cleanup.
    retiring_unadopted: Vec<String>,
    original_proof: Option<Arc<super::installed_retained_actor::InstalledProofInterrupt>>,
}
impl RetainedOpenDispatcher {
    pub(super) fn new(
        stream: UnixStream,
        cancel: Arc<AtomicBool>,
        cancellation: ServiceCancellation,
        original_proof: Arc<super::installed_retained_actor::InstalledProofInterrupt>,
    ) -> io::Result<Self> {
        stream.set_read_timeout(Some(REQUEST_BUDGET))?;
        stream.set_write_timeout(Some(REQUEST_BUDGET))?;
        Ok(Self {
            stream,
            cancel,
            cancellation,
            pending: VecDeque::new(),
            editors: Vec::new(),
            intake_closed: false,
            admissions_closed: false,
            retiring_unadopted: Vec::new(),
            original_proof: Some(original_proof),
        })
    }
    /// One finite request per supervision turn; the existing namespace/epoch
    /// owner remains the sole authority and cancellation owner.
    pub(super) fn poll(
        &mut self,
        shared: &Arc<Mutex<RetainedConsumerLease>>,
        files_active: bool,
    ) -> io::Result<()> {
        if self.intake_closed {
            return Ok(());
        }
        self.pending.retain(|item| item.expires > Instant::now());
        match request_boundary(&self.stream)? {
            RequestBoundary::Idle => return Ok(()),
            RequestBoundary::Closed => {
                self.intake_closed = true;
                self.close_files_admissions();
                return Ok(());
            }
            RequestBoundary::Data => {}
        }
        let files_active = files_active && !self.admissions_closed;
        let fence = RequestFence::start(
            &self.stream,
            Arc::clone(&self.cancel),
            self.cancellation.clone(),
        )?;
        if files_active {
            fence.add_original_proof(Arc::clone(
                self.original_proof.as_ref().ok_or_else(refused)?,
            ))?;
        }
        let bytes = read_frame(&mut self.stream, 16 * 1024)?;
        match decode_mux(&bytes)? {
            MuxCommand::Support(request) => {
                let operation =
                    if files_active && request.access == v2::SelectedAccess::EditInParent {
                        shared
                            .lock()
                            .map_err(|_| refused())?
                            .selected_edit_support(&self.cancel)
                            .ok()
                    } else {
                        None
                    };
                write_support(&mut self.stream, &request, operation)?;
            }
            MuxCommand::Legacy(open_selected::RootCommand::OpenSelected(request)) => {
                let observed = if files_active {
                    self.authenticate_files(shared, &fence)?
                } else {
                    self.refuse_retired_files(&fence)?;
                    None
                };
                let Some(observed) = observed else {
                    let reply = RootReply::Refused {
                        schema: 1,
                        request_id: request.request_id,
                        reason: RootRefusal::Unavailable,
                    };
                    write_frame(&mut self.stream, &encode_reply(&reply)?)?;
                    return Ok(());
                };
                let reply = self.admit_request(
                    &*shared.lock().map_err(|_| refused())?,
                    request,
                    None,
                    None,
                    observed,
                )?;
                write_frame(&mut self.stream, &encode_reply(&reply)?)?;
            }
            MuxCommand::Current(v2::RootCommand::OpenSelected(request)) => {
                let observed = if files_active {
                    self.authenticate_files(shared, &fence)?
                } else {
                    self.refuse_retired_files(&fence)?;
                    None
                };
                let Some(observed) = observed else {
                    let reply = v2::AdmissionReply {
                        schema: 2,
                        access: request.access,
                        reply: RootReply::Refused {
                            schema: 1,
                            request_id: request.request_id,
                            reason: RootRefusal::Unavailable,
                        },
                    };
                    write_frame(
                        &mut self.stream,
                        &serde_json::to_vec(&reply).map_err(io::Error::other)?,
                    )?;
                    return Ok(());
                };
                let access = request.access;
                // navigation() transports read-only object identity only. The
                // request access remains explicit in the bounded reply; no
                // editable filesystem or successful target launch is issued.
                let reply = self.admit_request(
                    &*shared.lock().map_err(|_| refused())?,
                    request.clone().navigation(),
                    Some(access),
                    Some(request),
                    observed,
                )?;
                write_frame(
                    &mut self.stream,
                    &serde_json::to_vec(&v2::AdmissionReply {
                        schema: 2,
                        access,
                        reply,
                    })
                    .map_err(io::Error::other)?,
                )?;
            }
            MuxCommand::Current(v2::RootCommand::StageSelectedConsumer(request)) => {
                if files_active {
                    self.stage_editor(shared, request, &fence)?;
                } else {
                    refuse_unavailable_stage(&mut self.stream, request)?;
                }
            }
            MuxCommand::Legacy(open_selected::RootCommand::ClaimSelected(request)) => {
                write_frame(
                    &mut self.stream,
                    &serde_json::to_vec(&open_selected::ClaimReply::Refused {
                        schema: 1,
                        request_id: request.request_id,
                        reason: RootRefusal::Unavailable,
                    })
                    .map_err(io::Error::other)?,
                )?;
            }
            MuxCommand::Current(v2::RootCommand::ClaimSelected(request)) => {
                let reply = if files_active {
                    self.claim_editor(shared, &request)?
                } else {
                    open_selected::ClaimReply::Refused {
                        schema: 1,
                        request_id: request.request_id.clone(),
                        reason: RootRefusal::Unavailable,
                    }
                };
                write_frame(
                    &mut self.stream,
                    &serde_json::to_vec(&v2::ClaimResult {
                        schema: 2,
                        access: request.access,
                        reply,
                    })
                    .map_err(io::Error::other)?,
                )?;
            }
            MuxCommand::Current(v2::RootCommand::PollSelected(request)) => {
                let reply = self.poll_editor(&request, &fence)?;
                write_frame(
                    &mut self.stream,
                    &serde_json::to_vec(&v2::TargetResult {
                        schema: 2,
                        access: request.access,
                        reply,
                    })
                    .map_err(io::Error::other)?,
                )?;
            }
            MuxCommand::Current(v2::RootCommand::CancelSelected(request)) => {
                if let Some(owner) = self.editors.iter().find(|owner| {
                    owner.request().request_id == request.request_id
                        && owner.request().access == request.access
                }) {
                    owner.cancel();
                }
                write_frame(
                    &mut self.stream,
                    &serde_json::to_vec(&v2::TargetResult {
                        schema: 2,
                        access: request.access,
                        reply: unavailable_target(&request.request_id),
                    })
                    .map_err(io::Error::other)?,
                )?;
            }
            // No target factory/namespace owner exists in this foundation.
            // Unknown or unsupported commands cannot become positive evidence.
            _ => return Err(refused()),
        }
        fence.check()?;
        Ok(())
    }
    fn refuse_retired_files(&mut self, fence: &RequestFence) -> io::Result<()> {
        for tag in [80, 81, 82] {
            drop(receive_file(&mut self.stream, tag)?);
        }
        let proof = receive_file(&mut self.stream, 83)?;
        require_stream(&proof)?;
        let proof = UnixStream::from(std::os::fd::OwnedFd::from(proof));
        fence.add(&proof)?;
        proof.shutdown(std::net::Shutdown::Both)?;
        Ok(())
    }
    fn authenticate_files(
        &mut self,
        shared: &Arc<Mutex<RetainedConsumerLease>>,
        fence: &RequestFence,
    ) -> io::Result<Option<ObservedFiles>> {
        let broker_task = receive_file(&mut self.stream, 80)?;
        let selected = receive_file(&mut self.stream, 81)?;
        let parent = receive_file(&mut self.stream, 82)?;
        let mut proof = UnixStream::from(std::os::fd::OwnedFd::from(receive_file(
            &mut self.stream,
            83,
        )?));
        proof.peer_addr()?;
        let mut kind = 0i32;
        let mut len = std::mem::size_of_val(&kind) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                proof.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut len,
            )
        } != 0
            || kind != libc::SOCK_STREAM
        {
            return Err(refused());
        }
        fence.add(&proof)?;
        process_binding::arm(&proof)?;
        let mut challenge = [0u8; 32];
        challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        proof.write_all(&process_binding::MAGIC)?;
        proof.write_all(&challenge)?;
        let target_uid = shared
            .lock()
            .map_err(|_| refused())?
            .open_target_uid(&self.cancel);
        let target_uid = match target_uid {
            Ok(uid) => uid,
            Err(_) => return Ok(None),
        };
        let peer = Arc::new(process_binding::receive(&proof, &challenge, target_uid)?);
        peer.matches_transferred_pidfd(std::os::fd::AsFd::as_fd(&broker_task))?;
        proof.write_all(&[1])?;
        if shared
            .lock()
            .map_err(|_| refused())?
            .check_open_origin(&self.cancel, &peer)
            .is_err()
        {
            return Ok(None);
        }
        Ok(Some(ObservedFiles {
            selected,
            parent,
            peer,
        }))
    }
    fn admit_request(
        &mut self,
        lease: &RetainedConsumerLease,
        request: RootRequest,
        access: Option<v2::SelectedAccess>,
        current_request: Option<v2::RootRequest>,
        observed: ObservedFiles,
    ) -> io::Result<RootReply> {
        let duplicate = self
            .pending
            .iter()
            .find(|item| item.request_id == request.request_id);
        let repeated = duplicate.map(|item| {
            if item.app_id != request.app_id
                || item.access != access
                || item.selection.check(lease, &self.cancel).is_err()
            {
                RootRefusal::Unavailable
            } else {
                RootRefusal::ResidentLimit
            }
        });
        if lease.check(&self.cancel).is_err() {
            return Ok(RootReply::Refused {
                schema: 1,
                request_id: request.request_id,
                reason: RootRefusal::Unavailable,
            });
        }
        let reply = if self.pending.len() >= PENDING_LIMIT || repeated.is_some() {
            RootReply::Refused {
                schema: 1,
                request_id: request.request_id,
                reason: repeated.unwrap_or(RootRefusal::ResidentLimit),
            }
        } else {
            let objects = SelectedObjects {
                selected: observed.selected,
                parent: observed.parent,
                components: request
                    .components
                    .into_iter()
                    .map(OsString::from_vec)
                    .collect(),
            };
            match lease.admit_open_selection(&self.cancel, observed.peer, objects) {
                Ok(selection) => {
                    let receipt = RootReceipt {
                        schema: 1,
                        request_id: request.request_id.clone(),
                        source_operation: selection.operation_id().to_owned(),
                    };
                    self.pending.push_back(Pending {
                        request_id: request.request_id,
                        app_id: request.app_id,
                        access,
                        request: current_request,
                        claimed: false,
                        selection,
                        expires: Instant::now()
                            .checked_add(PENDING_LIFETIME)
                            .ok_or(io::ErrorKind::InvalidInput)?,
                    });
                    RootReply::Admitted { receipt }
                }
                Err(_) => {
                    // Retired original Files scope refuses this request only;
                    // independent editor proof/source custody is unaffected.
                    RootReply::Refused {
                        schema: 1,
                        request_id: request.request_id,
                        reason: RootRefusal::Unavailable,
                    }
                }
            }
        };
        Ok(reply)
    }
    fn claim_editor(
        &mut self,
        shared: &Arc<Mutex<RetainedConsumerLease>>,
        request: &v2::ClaimSelected,
    ) -> io::Result<open_selected::ClaimReply> {
        let pending = self.pending.iter_mut().find(|pending| {
            pending.request_id == request.request_id
                && pending.app_id == request.app_id
                && pending.access == Some(request.access)
                && pending.selection.operation_id() == request.source_operation
                && pending.expires > Instant::now()
        });
        let accepted = if request.access == v2::SelectedAccess::EditInParent {
            if let Some(pending) = pending {
                let lease = shared.lock().map_err(|_| refused())?;
                if pending.selection.check(&lease, &self.cancel).is_ok()
                    && pending.request.is_some()
                {
                    pending.claimed = true;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        Ok(if accepted {
            open_selected::ClaimReply::Claimed {
                schema: 1,
                request_id: request.request_id.clone(),
            }
        } else {
            open_selected::ClaimReply::Refused {
                schema: 1,
                request_id: request.request_id.clone(),
                reason: RootRefusal::Unavailable,
            }
        })
    }
    fn stage_editor(
        &mut self,
        shared: &Arc<Mutex<RetainedConsumerLease>>,
        launch: v2::LaunchSelected,
        fence: &RequestFence,
    ) -> io::Result<()> {
        let context = read_frame(&mut self.stream, 64 * 1024)?;
        let proof = receive_file(&mut self.stream, 84)?;
        let endpoint = receive_file(&mut self.stream, 85)?;
        for file in [&proof, &endpoint] {
            require_stream(file)?;
        }
        let proof = UnixStream::from(std::os::fd::OwnedFd::from(proof));
        let endpoint = UnixStream::from(std::os::fd::OwnedFd::from(endpoint));
        fence.add(&proof)?;
        let duplicate = self
            .editors
            .iter()
            .any(|owner| owner.request().request_id == launch.request_id);
        let pending = self.pending.iter().find(|pending| {
            pending.claimed
                && pending.request_id == launch.request_id
                && pending.app_id == launch.binding.app_id
                && pending.selection.operation_id() == launch.binding.operation_id
                && pending.access == Some(launch.access)
                && pending.expires > Instant::now()
        });
        let request = pending.and_then(|pending| pending.request.clone());
        let result = if duplicate || self.editors.len() >= EDITOR_LIMIT {
            Err(refused())
        } else {
            match request {
                Some(request) => self
                    .pending_edit_origin(shared, &launch.request_id, &launch.binding.app_id)
                    .and_then(|custody| {
                        SelectedEditorOwner::stage(
                            custody,
                            request,
                            launch.clone(),
                            context,
                            proof,
                            endpoint,
                            Arc::clone(&self.cancel),
                        )
                    }),
                None => Err(refused()),
            }
        };
        let mut staged = false;
        let mut staged_owner = None;
        if let Ok(owner) = result {
            // Insert the real owner BEFORE any bounded preparation wait. Every
            // failure thereafter stays registered through observed retirement.
            self.editors.push(owner);
            let index = self.editors.len() - 1;
            staged_owner = Some(index);
            loop {
                fence.check()?;
                match self.editors[index].stage_status() {
                    StageStatus::Prepared => {
                        staged = true;
                        break;
                    }
                    StageStatus::Refused => break,
                    StageStatus::Preparing => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        }
        let reply = if staged {
            v2::StageResult::Staged {
                schema: 2,
                request_id: launch.request_id.clone(),
                access: launch.access,
                context_sha256: launch.binding.context_sha256.clone(),
            }
        } else {
            v2::StageResult::Refused {
                schema: 2,
                request_id: launch.request_id.clone(),
                access: launch.access,
                context_sha256: launch.binding.context_sha256.clone(),
                reason: RootRefusal::Unavailable,
            }
        };
        fence.check()?;
        write_frame(&mut self.stream, &v2::encode_stage(&reply)?)?;
        fence.check()?;
        if staged {
            if let Some(owner) = staged_owner.and_then(|index| self.editors.get_mut(index)) {
                // Staged reports retained preparation only. A racing refusal
                // belongs to this actual owner; next Poll reports Refused.
                // Keep it registered through native stop without cancelling
                // independent editors on the authenticated bootstrap.
                if !owner.enroll_history(&mut self.stream, fence.deadline)? {
                    owner.cancel();
                    return Ok(());
                }
                if !owner.enroll_history_read(&mut self.stream, fence.deadline)? {
                    owner.cancel();
                    return Ok(());
                }
                match owner.delegate_shell(&mut self.stream, fence.deadline)? {
                    launcher::ordinary_child::DelegationOutcome::Delegated => {
                        if owner.release_after_staged().is_err() {
                            owner.cancel();
                        }
                    }
                    launcher::ordinary_child::DelegationOutcome::Refused => owner.cancel(),
                }
            }
        }
        Ok(())
    }
    fn poll_editor(
        &self,
        request: &v2::PollSelected,
        fence: &RequestFence,
    ) -> io::Result<open_selected::TargetReply> {
        let Some(owner) = self.editors.iter().find(|owner| {
            owner.request().request_id == request.request_id
                && owner.request().access == request.access
        }) else {
            return Ok(unavailable_target(&request.request_id));
        };
        fence.add(owner.proof_transport())?;
        let reply = match owner.poll() {
            Ok(EditorStatus::Pending) => open_selected::TargetReply::Pending {
                schema: 1,
                request_id: request.request_id.clone(),
            },
            Ok(EditorStatus::NamespaceReady(receipt)) => {
                open_selected::TargetReply::NamespaceReady { receipt }
            }
            Ok(EditorStatus::Refused) | Err(_) => {
                owner.cancel();
                unavailable_target(&request.request_id)
            }
        };
        fence.check()?;
        Ok(reply)
    }
    /// Called only after main lease cleanup and unadopted worker stop. This
    /// exact original socket has no role in independently admitted editors.
    pub(super) fn retire_files_proof(&mut self) {
        if let Some(proof) = self.original_proof.take() {
            proof.interrupt();
        }
    }
    pub(super) fn intake_closed(&self) -> bool {
        self.intake_closed
    }
    pub(super) fn close_files_admissions(&mut self) {
        if self.admissions_closed {
            return;
        }
        self.admissions_closed = true;
        self.pending.clear();
        for owner in &self.editors {
            // Only independently current kernel/person/source admission allows
            // an editor to survive the original Files lifecycle. Errors cancel
            // that actual owner, never turn a receipt into proof.
            if !matches!(owner.consumer_admitted(), Ok(true)) {
                owner.cancel();
                self.retiring_unadopted
                    .push(owner.request().request_id.clone());
            }
        }
    }
    pub(super) fn unadopted_observed_stopped(&self) -> bool {
        self.editors.iter().all(|owner| {
            !self
                .retiring_unadopted
                .contains(&owner.request().request_id)
                || owner.observed_stopped()
        })
    }
    pub(super) fn editors_observed_stopped(&self) -> bool {
        self.editors
            .iter()
            .all(SelectedEditorOwner::observed_stopped)
    }
    pub(super) fn cancel_editors(&self) {
        for owner in &self.editors {
            owner.cancel();
        }
    }
    pub(super) fn supervise_editors(&mut self) {
        if self.cancel.load(Ordering::Acquire) {
            self.cancel_editors();
        }
        // Only explicit session/source/namespace stop permits removing an owner;
        // a finished/panicked thread or a refusal message cannot release it.
        self.editors.retain(|owner| !owner.observed_stopped());
        self.retiring_unadopted.retain(|request_id| {
            self.editors
                .iter()
                .any(|owner| &owner.request().request_id == request_id)
        });
    }
    /// Sibling typed stage factory only. Stage names cannot override the actual
    /// Files-selected object or upgrade a previous read-only admission.
    pub(super) fn pending_edit_origin(
        &self,
        shared: &Arc<Mutex<RetainedConsumerLease>>,
        request_id: &str,
        app_id: &str,
    ) -> io::Result<super::retained_open_selection::SelectedOriginCustody> {
        let pending = self
            .pending
            .iter()
            .find(|item| {
                item.request_id == request_id
                    && item.app_id == app_id
                    && item.expires > Instant::now()
                    && item.access == Some(v2::SelectedAccess::EditInParent)
            })
            .ok_or_else(refused)?;
        pending.selection.edit_origin_custody(shared, &self.cancel)
    }
    pub(super) fn retire(&mut self) {
        self.cancel_editors();
        for owner in &mut self.editors {
            owner.retire_before_exit();
        }
        self.editors.clear();
        self.pending.clear();
    }
}
fn unavailable_target(request_id: &str) -> open_selected::TargetReply {
    open_selected::TargetReply::Refused {
        schema: 1,
        request_id: request_id.into(),
        reason: RootRefusal::Unavailable,
    }
}
enum MuxCommand {
    Support(discovery::QuerySelectedSupport),
    Legacy(open_selected::RootCommand),
    Current(v2::RootCommand),
}
fn decode_mux(bytes: &[u8]) -> io::Result<MuxCommand> {
    if bytes.is_empty() || bytes.len() > 16 * 1024 {
        return Err(refused());
    }
    if let Ok(discovery::RootCommand::QuerySelectedSupport(request)) =
        serde_json::from_slice::<discovery::RootCommand>(bytes)
    {
        if !request.valid() {
            return Err(refused());
        }
        return Ok(MuxCommand::Support(request));
    }
    if let Ok(command) = v2::decode_command(bytes) {
        return Ok(MuxCommand::Current(command));
    }
    open_selected::decode_command(bytes).map(MuxCommand::Legacy)
}
#[derive(Debug, PartialEq, Eq)]
enum RequestBoundary {
    Idle,
    Data,
    Closed,
}
/// EOF is clean only before the next frame's first byte. Buffered data still
/// takes the exact framed reader; EOF inside a header/body is a strict error.
fn request_boundary(stream: &UnixStream) -> io::Result<RequestBoundary> {
    let mut ready = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLRDHUP,
        revents: 0,
    };
    if unsafe { libc::poll(&raw mut ready, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if ready.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::ErrorKind::BrokenPipe.into());
    }
    if ready.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLRDHUP) == 0 {
        return Ok(RequestBoundary::Idle);
    }
    let mut byte = 0u8;
    let count = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    match count {
        0 => Ok(RequestBoundary::Closed),
        1 => Ok(RequestBoundary::Data),
        _ => {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                Ok(RequestBoundary::Idle)
            } else {
                Err(error)
            }
        }
    }
}
fn read_frame(stream: &mut UnixStream, limit: usize) -> io::Result<Vec<u8>> {
    wire_frame::Framing::little_endian(limit)
        .reject_empty()
        .read(stream)
        .map_err(io::Error::other)?
        .ok_or_else(refused)
}
fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    wire_frame::Framing::little_endian(16 * 1024)
        .reject_empty()
        .write(stream, bytes)
        .map_err(io::Error::other)
}
fn write_support(
    stream: &mut UnixStream,
    request: &discovery::QuerySelectedSupport,
    operation: Option<String>,
) -> io::Result<()> {
    if !request.valid() {
        return Err(refused());
    }
    write_frame(
        stream,
        &serde_json::to_vec(&operation.map_or(
            discovery::RootReply::Unavailable,
            |source_operation| {
                discovery::RootReply::Supported(discovery::Supported {
                    schema: 1,
                    request_id: request.request_id.clone(),
                    source_operation,
                    app_id: request.app_id.clone(),
                    package_sha256: request.package_sha256.clone(),
                    access: request.access,
                })
            },
        ))
        .map_err(io::Error::other)?,
    )
}
fn refuse_unavailable_stage(
    stream: &mut UnixStream,
    request: v2::LaunchSelected,
) -> io::Result<()> {
    if !v2::RootCommand::StageSelectedConsumer(request.clone()).valid() {
        return Err(refused());
    }
    let _context = read_frame(stream, 64 * 1024)?;
    // Transport ACKs must precede any future target QPE. There is no typed
    // factory here, so retain no editor and report only per-request refusal.
    let proof = receive_file(stream, 84)?;
    let endpoint = receive_file(stream, 85)?;
    for file in [&proof, &endpoint] {
        require_stream(file)?;
    }
    let reply = v2::StageResult::Refused {
        schema: 2,
        request_id: request.request_id,
        access: request.access,
        context_sha256: request.binding.context_sha256,
        reason: RootRefusal::Unavailable,
    };
    write_frame(stream, &v2::encode_stage(&reply)?)
}
fn require_stream(file: &File) -> io::Result<()> {
    let mut kind = 0i32;
    let mut len = std::mem::size_of_val(&kind) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            file.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut len,
        )
    } != 0
        || kind != libc::SOCK_STREAM
    {
        return Err(refused());
    }
    UnixStream::from(std::os::fd::OwnedFd::from(file.try_clone()?)).peer_addr()?;
    Ok(())
}
fn receive_file(stream: &mut UnixStream, tag: u8) -> io::Result<File> {
    let (bytes, descriptor) = fd_transport::recv_with_fd(stream.as_fd())?;
    if bytes != [tag] {
        return Err(refused());
    }
    let file = File::from(descriptor.ok_or_else(refused)?);
    stream.write_all(&[tag])?;
    Ok(file)
}
/// Absolute request fence covers partial frame IO, all SCM handoffs and fresh
/// Files authentication before the first read. No progress renews its budget.
struct RequestFence {
    sockets: Arc<Mutex<Vec<UnixStream>>>,
    original_proofs: Arc<Mutex<Vec<Arc<super::installed_retained_actor::InstalledProofInterrupt>>>>,
    stop: Arc<AtomicBool>,
    expired: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    deadline: Instant,
    cancel: Arc<AtomicBool>,
    control: ServiceCancellation,
}
impl RequestFence {
    fn start(
        stream: &UnixStream,
        cancel: Arc<AtomicBool>,
        control: ServiceCancellation,
    ) -> io::Result<Self> {
        Self::start_with_budget(stream, cancel, control, REQUEST_BUDGET)
    }
    fn start_with_budget(
        stream: &UnixStream,
        cancel: Arc<AtomicBool>,
        control: ServiceCancellation,
        budget: Duration,
    ) -> io::Result<Self> {
        if budget.is_zero() || budget > REQUEST_BUDGET {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let sockets = Arc::new(Mutex::new(vec![stream.try_clone()?]));
        let original_proofs: Arc<
            Mutex<Vec<Arc<super::installed_retained_actor::InstalledProofInterrupt>>>,
        > = Arc::new(Mutex::new(Vec::new()));
        let proof_interrupts = Arc::clone(&original_proofs);
        let stop = Arc::new(AtomicBool::new(false));
        let expired = Arc::new(AtomicBool::new(false));
        let held = Arc::clone(&sockets);
        let done = Arc::clone(&stop);
        let timed_out = Arc::clone(&expired);
        let deadline = Instant::now()
            .checked_add(budget)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let worker_cancel = Arc::clone(&cancel);
        let worker_control = control.clone();
        let worker = std::thread::Builder::new()
            .name("retained-open-request-fence".into())
            .spawn(move || {
                while !done.load(Ordering::Acquire) {
                    if worker_cancel.load(Ordering::Acquire)
                        || worker_control.is_cancelled()
                        || Instant::now() >= deadline
                    {
                        timed_out.store(true, Ordering::Release);
                        if let Ok(proofs) = proof_interrupts.lock() {
                            for proof in proofs.iter() {
                                proof.interrupt();
                            }
                        }
                        if let Ok(sockets) = held.lock() {
                            for socket in sockets.iter() {
                                let _ = socket.shutdown(std::net::Shutdown::Both);
                            }
                        }
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })?;
        Ok(Self {
            sockets,
            original_proofs,
            stop,
            expired,
            worker: Some(worker),
            deadline,
            cancel,
            control,
        })
    }
    fn add_original_proof(
        &self,
        proof: Arc<super::installed_retained_actor::InstalledProofInterrupt>,
    ) -> io::Result<()> {
        let mut proofs = self.original_proofs.lock().map_err(|_| refused())?;
        self.check()?;
        if !proofs.is_empty() {
            return Err(refused());
        }
        proofs.push(proof);
        self.check()
    }
    fn add(&self, stream: &UnixStream) -> io::Result<()> {
        let mut sockets = self.sockets.lock().map_err(|_| refused())?;
        self.check()?;
        if sockets.len() != 1 {
            return Err(refused());
        }
        sockets.push(stream.try_clone()?);
        self.check()
    }
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) || self.control.is_cancelled() {
            Err(io::ErrorKind::Interrupted.into())
        } else if self.expired.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            Err(io::ErrorKind::TimedOut.into())
        } else {
            Ok(())
        }
    }
}
impl Drop for RequestFence {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    #[test]
    fn complete_request_boundary_distinguishes_idle_clean_eof_and_truncation() {
        let (mut root, mut shell) = UnixStream::pair().unwrap();
        assert_eq!(request_boundary(&root).unwrap(), RequestBoundary::Idle);
        write_frame(&mut shell, b"complete").unwrap();
        shell.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(request_boundary(&root).unwrap(), RequestBoundary::Data);
        assert_eq!(read_frame(&mut root, 1024).unwrap(), b"complete");
        assert_eq!(request_boundary(&root).unwrap(), RequestBoundary::Closed);
        for bytes in [&[1u8][..], &[4u8, 0, 0, 0, b'a'][..]] {
            let (mut root, mut shell) = UnixStream::pair().unwrap();
            shell.write_all(bytes).unwrap();
            shell.shutdown(std::net::Shutdown::Write).unwrap();
            assert_eq!(request_boundary(&root).unwrap(), RequestBoundary::Data);
            assert!(read_frame(&mut root, 1024).is_err());
        }
    }
    fn support_query() -> discovery::QuerySelectedSupport {
        discovery::QuerySelectedSupport {
            schema: 1,
            request_id: "11111111-1111-4111-8111-111111111111".into(),
            app_id: "so.tilo.text-editor".into(),
            package_sha256: "a".repeat(64),
            access: v2::SelectedAccess::EditInParent,
        }
    }
    #[test]
    fn closed_mux_support_query_returns_unavailable_over_actual_connection() {
        let (mut root, mut shell) = UnixStream::pair().unwrap();
        let command = discovery::RootCommand::QuerySelectedSupport(support_query());
        let bytes = serde_json::to_vec(&command).unwrap();
        write_frame(&mut shell, &bytes).unwrap();
        let fence = RequestFence::start(
            &root,
            Arc::new(AtomicBool::new(false)),
            ServiceCancellation::default(),
        )
        .unwrap();
        let MuxCommand::Support(query) =
            decode_mux(&read_frame(&mut root, 16 * 1024).unwrap()).unwrap()
        else {
            panic!("query routed into selected descriptor admission")
        };
        write_support(&mut root, &query, None).unwrap();
        fence.check().unwrap();
        let reply: discovery::RootReply =
            serde_json::from_slice(&read_frame(&mut shell, 16 * 1024).unwrap()).unwrap();
        assert_eq!(reply, discovery::RootReply::Unavailable);
        let mut changed = serde_json::to_value(command).unwrap();
        changed["request"]["grant"] = true.into();
        assert!(decode_mux(&serde_json::to_vec(&changed).unwrap()).is_err());
        changed["request"].as_object_mut().unwrap().remove("grant");
        changed["request"]["package_sha256"] = "self-labelled".into();
        assert!(decode_mux(&serde_json::to_vec(&changed).unwrap()).is_err());
        assert!(decode_mux(br#"{"kind":"supported","request":{}}"#).is_err());
    }
    #[test]
    fn unavailable_stage_acks_transport_then_refuses_without_launcher_qpe() {
        let (mut root, mut shell) = UnixStream::pair().unwrap();
        let request_id = "11111111-1111-4111-8111-111111111111".to_owned();
        let request = v2::LaunchSelected {
            schema: 2,
            request_id: request_id.clone(),
            access: v2::SelectedAccess::EditInParent,
            publisher_hex: "a".repeat(64),
            binding: runtime_collab::retained_person::RetainedLaunchBinding {
                operation_id: "22222222-2222-4222-8222-222222222222".into(),
                app_id: "so.tilo.text-editor".into(),
                package_sha256: "b".repeat(64),
                room: "33333333-3333-4333-8333-333333333333".into(),
                resource_record: format!("selected-open-v2:edit-in-parent:{request_id}"),
                target_uid: 1000,
                target_pid: 42,
                root_pid: 1,
                context_sha256: "c".repeat(64),
            },
        };
        // Wire labels are deliberately not authenticated identity. This tests
        // only actual bounded transport refusal, never source admission.
        let server_request = request.clone();
        let server = std::thread::spawn(move || {
            let fence = RequestFence::start(
                &root,
                Arc::new(AtomicBool::new(false)),
                ServiceCancellation::default(),
            )
            .unwrap();
            refuse_unavailable_stage(&mut root, server_request).unwrap();
            fence.check().unwrap();
        });
        let (proof, _proof_peer) = UnixStream::pair().unwrap();
        let (launcher, _launcher_peer) = UnixStream::pair().unwrap();
        shell
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        wire_frame::Framing::little_endian(64 * 1024)
            .reject_empty()
            .write(&mut shell, b"closed context observation")
            .unwrap();
        for (tag, fd) in [(84, proof.as_fd()), (85, launcher.as_fd())] {
            fd_transport::send_with_fd(shell.as_fd(), &[tag], fd).unwrap();
            let mut ack = [0];
            shell.read_exact(&mut ack).unwrap();
            assert_eq!(ack, [tag]);
        }
        let reply =
            v2::decode_stage(&read_frame(&mut shell, 16 * 1024).unwrap(), &request).unwrap();
        assert!(matches!(
            reply,
            v2::StageResult::Refused {
                reason: RootRefusal::Unavailable,
                ..
            }
        ));
        server.join().unwrap();
    }
    #[test]
    fn partial_mux_query_cannot_renew_absolute_request_budget() {
        let (mut root, mut shell) = UnixStream::pair().unwrap();
        let fence = RequestFence::start_with_budget(
            &root,
            Arc::new(AtomicBool::new(false)),
            ServiceCancellation::default(),
            Duration::from_millis(80),
        )
        .unwrap();
        shell.write_all(&[8, 0, 0, 0, b'{']).unwrap();
        let started = Instant::now();
        assert!(read_frame(&mut root, 16 * 1024).is_err());
        assert_eq!(fence.check().unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn total_request_deadline_stops_a_real_stalled_files_authentication() {
        let (mut bootstrap, mut shell) = UnixStream::pair().unwrap();
        let (mut proof, mut files) = UnixStream::pair().unwrap();
        let cancellation = ServiceCancellation::default();
        let started = Instant::now();
        let fence = RequestFence::start_with_budget(
            &bootstrap,
            Arc::new(AtomicBool::new(false)),
            cancellation,
            Duration::from_millis(80),
        )
        .unwrap();
        fence.add(&proof).unwrap();
        // Genuine transport stalls partway through the real QPE header; the
        // same absolute fence already owns both exact connection endpoints.
        files.write_all(&process_binding::MAGIC[..3]).unwrap();
        assert!(process_binding::complete(&mut proof).is_err());
        assert_eq!(fence.check().unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(bootstrap.read(&mut [0]).unwrap(), 0);
        assert_eq!(shell.read(&mut [0]).unwrap(), 0);
        assert_eq!(files.read(&mut [0]).unwrap(), 0);
    }
    #[test]
    fn actual_owner_cancellation_interrupts_stalled_selected_open_qpe() {
        let (bootstrap, mut shell) = UnixStream::pair().unwrap();
        let (mut proof, mut files) = UnixStream::pair().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let fence = RequestFence::start(
            &bootstrap,
            Arc::clone(&cancel),
            ServiceCancellation::default(),
        )
        .unwrap();
        fence.add(&proof).unwrap();
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            cancel.store(true, Ordering::Release);
        });
        let started = Instant::now();
        assert!(process_binding::complete(&mut proof).is_err());
        trigger.join().unwrap();
        assert_eq!(
            fence.check().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(shell.read(&mut [0]).unwrap(), 0);
        assert_eq!(files.read(&mut [0]).unwrap(), 0);
    }
}
