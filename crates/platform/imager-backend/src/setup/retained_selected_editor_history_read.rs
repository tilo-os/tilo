//! Independent Reader for one actual selected editor, never a Files derivative.
use super::{
    installed_retained_actor::InstalledRetainedLaunchInput,
    retained_history_read::RootFilesHistoryReadEnrollment,
    retained_selected_edit::{
        CertifiedNativeHistoryDaemon, PendingDaemonEnrollment, RootSelectedEditor,
    },
};
use std::os::fd::AsFd;
use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

/// Transport/certified enrollment before the ordinary launcher exists. Only
/// refinement with the genuine current editor can mint a native Read producer.
pub struct PendingSelectedEditorHistoryRead {
    input: Arc<InstalledRetainedLaunchInput>,
    daemon: Arc<CertifiedNativeHistoryDaemon>,
    request_id: String,
}
impl PendingSelectedEditorHistoryRead {
    pub(in crate::setup) fn admit_editor(
        &self,
        editor: Arc<RootSelectedEditor>,
    ) -> io::Result<Arc<RootHistoryReadEnrollment>> {
        editor.check()?;
        if !Arc::ptr_eq(&self.input, editor.input())
            || self.request_id != editor.request().request_id
            || !self.daemon.is_selected_editor_history_read()
            || !self.daemon.uses_input(editor.input())
        {
            return Err(refused());
        }
        let reader = Arc::new(RootSelectedEditorHistoryReadEnrollment {
            editor,
            daemon: self.daemon.clone(),
            read_request_id: uuid::Uuid::new_v4().to_string(),
        });
        reader.check()?;
        Ok(Arc::new(RootHistoryReadEnrollment::SelectedEditor(reader)))
    }
}
pub struct RootSelectedEditorHistoryReadEnrollment {
    editor: Arc<RootSelectedEditor>,
    daemon: Arc<CertifiedNativeHistoryDaemon>,
    read_request_id: String,
}
impl RootSelectedEditorHistoryReadEnrollment {
    fn check(&self) -> io::Result<()> {
        self.editor.check()?;
        self.daemon.check()?;
        if !self.daemon.is_selected_editor_history_read()
            || !self.daemon.uses_input(self.editor.input())
        {
            return Err(refused());
        }
        self.editor.check()
    }
}
/// Opaque, independently admitted Reader owners. Files checks remain unchanged;
/// an editor variant requires an actual RootSelectedEditor, never a wire role.
pub enum RootHistoryReadEnrollment {
    Files(Arc<RootFilesHistoryReadEnrollment>),
    SelectedEditor(Arc<RootSelectedEditorHistoryReadEnrollment>),
}
impl RootHistoryReadEnrollment {
    pub(in crate::setup) fn files(
        reader: Arc<RootFilesHistoryReadEnrollment>,
    ) -> io::Result<Arc<Self>> {
        reader.check()?;
        Ok(Arc::new(Self::Files(reader)))
    }
    pub(in crate::setup) fn check(&self) -> io::Result<()> {
        match self {
            Self::Files(v) => v.check(),
            Self::SelectedEditor(v) => v.check(),
        }
    }
    pub(in crate::setup) fn input(&self) -> &Arc<InstalledRetainedLaunchInput> {
        match self {
            Self::Files(v) => v.input(),
            Self::SelectedEditor(v) => v.editor.input(),
        }
    }
    pub(in crate::setup) fn uses_input(&self, input: &Arc<InstalledRetainedLaunchInput>) -> bool {
        Arc::ptr_eq(self.input(), input)
    }
    pub(in crate::setup) fn read_request_id(&self) -> &str {
        match self {
            Self::Files(v) => v.read_request_id(),
            Self::SelectedEditor(v) => &v.read_request_id,
        }
    }
    pub(in crate::setup) fn completed_request_id(&self) -> Option<&str> {
        match self {
            Self::Files(v) => v.completed_request_id(),
            Self::SelectedEditor(_) => None,
        }
    }
    pub(in crate::setup) fn daemon(&self) -> &Arc<CertifiedNativeHistoryDaemon> {
        match self {
            Self::Files(v) => v.daemon(),
            Self::SelectedEditor(v) => &v.daemon,
        }
    }
    pub(in crate::setup) fn enrollment_context_digest(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        let value = match self {
            Self::Files(enrollment) => enrollment.enrollment_context_digest()?,
            Self::SelectedEditor(_) => self.daemon().context_digest()?,
        };
        self.check()?;
        Ok(value)
    }
    pub(in crate::setup) fn source_owner(&self) -> io::Result<[u8; 32]> {
        self.check()?;
        let value = match self {
            Self::Files(enrollment) => enrollment.source_owner()?,
            Self::SelectedEditor(_) => self.daemon().source_owner()?,
        };
        self.check()?;
        Ok(value)
    }
    pub(in crate::setup) fn for_subject(self: &Arc<Self>) -> io::Result<Arc<Self>> {
        self.check()?;
        let result = match self.as_ref() {
            Self::Files(v) => Self::Files(v.for_subject()?),
            Self::SelectedEditor(v) => {
                Self::SelectedEditor(Arc::new(RootSelectedEditorHistoryReadEnrollment {
                    editor: v.editor.clone(),
                    daemon: v.daemon.clone(),
                    read_request_id: uuid::Uuid::new_v4().to_string(),
                }))
            }
        };
        result.check()?;
        Ok(Arc::new(result))
    }
    pub(in crate::setup) fn selected_editor(&self) -> Option<&Arc<RootSelectedEditor>> {
        match self {
            Self::Files(_) => None,
            Self::SelectedEditor(v) => Some(&v.editor),
        }
    }
}

/// Existing exclusive Stage transaction: exact fresh editor proof stream, same
/// owned helper and fixed storage lane. No actual Read subject is opened here.
pub fn enroll_selected_editor_read(
    stream: &mut std::os::unix::net::UnixStream,
    input: Arc<InstalledRetainedLaunchInput>,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
) -> io::Result<Option<Arc<PendingSelectedEditorHistoryRead>>> {
    use runtime_fs::external_history::daemon_control::{
        EnrollmentControl, EnrollmentReply, MAX_CONTROL_BYTES,
    };
    use runtime_portal::fd_transport;
    use std::{
        fs::File,
        io::{Read, Write},
    };
    input.check_selected_editor_history_read(&cancel)?;
    let held = stream.try_clone()?;
    super::installed_retained_actor::with_proof_deadline_at(&held, &cancel, deadline, || {
        let framing = wire_frame::Framing::little_endian(MAX_CONTROL_BYTES).reject_empty();
        let bytes = framing
            .read(stream)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        let EnrollmentControl::EnrollSelectedEditorHistoryReadDaemon(request) =
            EnrollmentControl::decode(&bytes)?
        else {
            return Err(refused());
        };
        if request.person_binding
            != serde_json::to_vec(input.binding()).map_err(io::Error::other)?
            || input.binding().resource_record
                != format!("selected-open-v2:edit-in-parent:{}", request.request_id)
        {
            return Err(refused());
        }
        let pending = match PendingDaemonEnrollment::begin_editor_read(
            input.clone(),
            &request,
            cancel.clone(),
            deadline,
        ) {
            Ok(pending) => pending,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::Unsupported
                ) =>
            {
                input.check_selected_editor_history_read(&cancel)?;
                framing
                    .write(
                        stream,
                        &EnrollmentReply::Refused {
                            schema: 1,
                            request_id: request.request_id.clone(),
                        }
                        .encode(&request)?,
                    )
                    .map_err(io::Error::other)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        framing
            .write(
                stream,
                &EnrollmentReply::Delegated {
                    delegation: Box::new(pending.delegation(&request)?),
                }
                .encode(&request)?,
            )
            .map_err(io::Error::other)?;
        fd_transport::send_with_fd(stream.as_fd(), &[61], pending.peer().pidfd())?;
        let mut ack = [0];
        stream.read_exact(&mut ack)?;
        if ack != [61] {
            return Err(refused());
        }
        let (tag, fd) = fd_transport::recv_with_fd(stream.as_fd())?;
        if tag != [62] {
            return Err(refused());
        }
        let proof = File::from(fd.ok_or_else(refused)?);
        stream.write_all(&[62])?;
        let daemon = pending.finish(proof)?;
        input.check_selected_editor_history_read(&cancel)?;
        Ok(Some(Arc::new(PendingSelectedEditorHistoryRead {
            input: input.clone(),
            daemon,
            request_id: request.request_id,
        })))
    })
}
