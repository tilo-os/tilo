//! Live root-worker proof from the existing certified native personal session.
//! No discovery socket, account groups, alternate issuer or launch grant.
use super::NativePersonalSession;
use ed25519_dalek::Signer;
use runtime_collab::retained_person::{
    MAX_PROOF_BYTES, RetainedLaunchBinding, RetainedPersonClaims, SignedRetainedPersonProof,
};
use runtime_portal::process_binding;
use std::os::fd::AsFd;
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    sync::Arc,
    time::Duration,
};

/// A daemon a launch helper admitted on the session's behalf, proved again
/// against the peer on the proof stream. The launcher mints it; the session
/// only asks it to check, so it stays below the launcher.
pub(crate) trait RetainedProofRecipient {
    fn check(
        &self,
        peer: &process_binding::ExecutingPeer,
        binding: &RetainedLaunchBinding,
    ) -> io::Result<()>;
}

impl<T: RetainedProofRecipient + ?Sized> RetainedProofRecipient for Arc<T> {
    fn check(
        &self,
        peer: &process_binding::ExecutingPeer,
        binding: &RetainedLaunchBinding,
    ) -> io::Result<()> {
        (**self).check(peer, binding)
    }
}

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "retained native session unavailable",
    )
}
/// Whether `binding` is the original copy's root launch lane, and whether
/// it is a home lane (the retained home, or an edit in place of a selected
/// file): only these receive the current identity directory.
fn identity_lanes(binding: &RetainedLaunchBinding) -> (bool, bool) {
    let copy = binding.resource_record == format!("original-copy:{}", binding.operation_id);
    let home = binding.resource_record == "retained-home"
        || binding
            .resource_record
            .strip_prefix("selected-open-v2:edit-in-parent:")
            .is_some_and(|request| {
                uuid::Uuid::parse_str(request)
                    .is_ok_and(|id| !id.is_nil() && id.to_string() == request)
            });
    (copy, home)
}

impl NativePersonalSession {
    /// The ordinary Shell selection checks its original signed package and
    /// current author role alongside every certificate refresh.
    pub(crate) fn serve_retained_launch_proof_checked(
        self: Arc<Self>,
        stream: UnixStream,
        binding: &RetainedLaunchBinding,
        selection: impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        self.serve_retained_launch_proof_recipient(stream, binding, selection, None)
    }

    /// Separate fresh proof stream for an owned-helper-delegated daemon. The
    /// helper-bound signed selection is unchanged; only the exact recipient is
    /// narrowed by genuine transferred pidfd and direct daemon QPE custody.
    pub(crate) fn serve_retained_daemon_proof_checked(
        self: Arc<Self>,
        stream: UnixStream,
        binding: &RetainedLaunchBinding,
        recipient: &impl RetainedProofRecipient,
        selection: impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        self.serve_retained_launch_proof_recipient(
            stream,
            binding,
            selection,
            Some(recipient as &dyn RetainedProofRecipient),
        )
    }

    fn serve_retained_launch_proof_recipient(
        self: Arc<Self>,
        mut stream: UnixStream,
        binding: &RetainedLaunchBinding,
        selection: impl Fn() -> io::Result<()>,
        recipient: Option<&dyn RetainedProofRecipient>,
    ) -> io::Result<()> {
        let check_recipient = |peer: &process_binding::ExecutingPeer| match recipient {
            Some(recipient) => recipient.check(peer, binding),
            None if peer.pid() == binding.root_pid && peer.current() => Ok(()),
            None => Err(refused()),
        };
        binding.validate()?;
        selection()?;
        if binding.target_uid != rustix::process::geteuid().as_raw()
            || binding.target_pid != std::process::id()
            || binding.room.as_str() != self.caller.room().ok_or_else(refused)?
        {
            return Err(refused());
        }
        self.check().map_err(io::Error::other)?;
        selection()?;
        // The delegated stream is bounded before its first blocking handshake.
        if recipient.is_some() {
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        }
        // First the verifier authenticates the actual Shell sender.
        process_binding::complete(&mut stream)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        process_binding::arm(&stream)?;
        let mut challenge = [0; 32];
        challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        stream.write_all(&process_binding::MAGIC)?;
        stream.write_all(&challenge)?;
        let root = process_binding::receive(&stream, &challenge, 0)?;
        check_recipient(&root)?;
        self.check().map_err(io::Error::other)?;
        selection()?;
        stream.write_all(&[1])?;
        // Only the original root launch lane receives the current identity
        // directory. A daemon proof never receives this first-login observer.
        let (copy, home) = identity_lanes(binding);
        if copy && recipient.is_some() {
            return Err(refused());
        }
        if recipient.is_none() && (copy || home) {
            self.check().map_err(io::Error::other)?;
            selection()?;
            check_recipient(&root)?;
            let identity = runtime_identity::identity_dir_from_env()?;
            let directory = crate::onboarding_migration::directory(&identity)?;
            crate::onboarding_migration::private(&directory)?;
            tilo_files::native::atomic::StorageDirectory::from_file(directory.try_clone()?)?
                .validate_privacy(tilo_files::native::atomic::Privacy::OwnerOnly)?;
            runtime_portal::fd_transport::send_with_fd(
                stream.as_fd(),
                if copy { b"TILOCPD1" } else { b"TILORHD1" },
                directory.as_fd(),
            )?;
            self.check().map_err(io::Error::other)?;
            selection()?;
            check_recipient(&root)?;
        }
        let framing = wire_frame::Framing::little_endian(MAX_PROOF_BYTES).reject_empty();
        loop {
            self.check().map_err(io::Error::other)?;
            selection()?;
            check_recipient(&root)?;
            let mut challenge = [0; 32];
            stream.read_exact(&mut challenge)?;
            self.check().map_err(io::Error::other)?;
            selection()?;
            check_recipient(&root)?;
            let (certificate, revocations) = self
                .host
                .current_native_material(self.workspace)
                .map_err(io::Error::other)?;
            if binding.room != collab_types::person_room(&certificate.user_public_key).to_string() {
                return Err(refused());
            }
            let claims = RetainedPersonClaims {
                binding: binding.clone(),
                challenge,
                certificate: serde_json::to_vec(&certificate).map_err(io::Error::other)?,
                revocations,
                execution_id: self.caller.execution_id().ok_or_else(refused)?.to_string(),
                person_room: collab_types::person_room(&certificate.user_public_key).to_string(),
            };
            let signature = self
                .host
                .device_signing_key
                .sign(&claims.signing_bytes()?)
                .to_bytes()
                .to_vec();
            let proof = SignedRetainedPersonProof { claims, signature };
            self.check().map_err(io::Error::other)?;
            selection()?;
            check_recipient(&root)?;
            framing
                .write(&mut stream, &proof.encode()?)
                .map_err(io::Error::other)?;
        }
    }
}
