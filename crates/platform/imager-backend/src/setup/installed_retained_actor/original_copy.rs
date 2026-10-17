//! Installed-copy person admission on the existing proof/QPE channel.
//! Fixed package launch and receiver handoff remain owned by the controller.
use super::*;
use crate::setup::{
    home_capture::OriginalCopyReadAdmission, prepared_setup::AuthenticatedInstalledOriginalCopy,
};
use std::{os::fd::AsFd, sync::atomic::Ordering};
struct CopyOwner {
    installed: Arc<AuthenticatedInstalledOriginalCopy>,
    proof: Mutex<ProofClient>,
    anchor: Arc<dyn RootInstalledPersonAnchor>,
    person: [u8; 32],
    device: [u8; 32],
    observed: Option<tilo_identity_host::enrolled_transport::RootObservedCurrentDevice>,
    revoked: AtomicBool,
}
impl launcher::owned_service::ServiceAuthority for CopyOwner {
    fn check(&self) -> io::Result<()> {
        let result = (|| {
            root()?;
            if self.revoked.load(Ordering::Acquire) {
                return Err(refused());
            }
            self.installed.revalidate()?;
            if let Some(observed) = &self.observed {
                observed.validate()?;
            }
            let certificate = self
                .proof
                .lock()
                .map_err(|_| refused())?
                .prove(self.anchor.as_ref(), &AtomicBool::new(false))?;
            if certificate.user_public_key != self.person
                || certificate.device_public_key != self.device
                || self.installed.manifest().identity_public_key != self.person
                || self.revoked.load(Ordering::Acquire)
            {
                return Err(refused());
            }
            self.installed.revalidate()
        })();
        if result.is_err() {
            self.revoked.store(true, Ordering::Release);
        }
        result
    }
}
pub struct ReviewedInstalledOriginalCopy {
    owner: Arc<CopyOwner>,
    read: Arc<OriginalCopyReadAdmission>,
}
impl ReviewedInstalledOriginalCopy {
    pub fn retirement_pending() -> bool {
        OriginalCopyReadAdmission::retirement_pending()
    }
    /// Scope labels are authenticated by the same real Shell certificate/QPE
    /// exchange; source geometry comes exclusively from the protected record.
    pub(in crate::setup) fn review(
        stream: UnixStream,
        request: LaunchRequest,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        root()?;
        super::super::migration::check_cancel(cancel)?;
        let installed = AuthenticatedInstalledOriginalCopy::open_installed(request.operation_id())?;
        installed.revalidate()?;
        let manifest = installed.manifest();
        let declaration = manifest
            .installed_original_copy_declaration()?
            .ok_or_else(refused)?;
        if declaration.source().operation_id != request.operation_id()
            || request.resource_record() != format!("original-copy:{}", request.operation_id())
        {
            return Err(refused());
        }
        let person = manifest.identity_public_key;
        if request.room() != runtime_collab::retained_person::personal_room(&person) {
            return Err(refused());
        }
        let mut proof = ProofClient::connect(stream, &request, cancel)?;
        let held = proof.stream.try_clone()?;
        let directory = with_proof_deadline(&held, cancel, || {
            let (bytes, descriptor) =
                runtime_portal::fd_transport::recv_with_fd(proof.stream.as_fd())?;
            if bytes != b"TILOCPD1" || !proof.peer.current() {
                return Err(refused());
            }
            descriptor.map(File::from).ok_or_else(refused)
        })?;
        let (device_public_key, observed) = match manifest.identity_kind {
            IdentityKind::EnrolledDevice { device_public_key } => {
                // Keep the enrolled transport's installed-machine authority.
                // This role's private directory descriptor is not a new source grant.
                drop(directory);
                (device_public_key, None)
            }
            IdentityKind::SingleDeviceBackup => {
                let current =
                    tilo_identity_host::enrolled_transport::RootObservedCurrentDevice::review(
                        directory,
                        proof.peer.uid(),
                    )?;
                crate::setup::prepared_setup::publish_original_copy_owner(
                    &installed,
                    &current,
                    &proof.peer,
                )?;
                (current.device_public_key(), Some(current))
            }
            IdentityKind::CurrentDevice { .. } => return Err(refused()),
        };
        let anchor: Arc<dyn RootInstalledPersonAnchor> =
            InstalledPersonAnchor::open(person, device_public_key)?;
        let certificate = proof.prove(anchor.as_ref(), cancel)?;
        if certificate.user_public_key != person
            || certificate.device_public_key != device_public_key
        {
            return Err(refused());
        }
        installed.revalidate()?;
        let owner = Arc::new(CopyOwner {
            installed: installed.clone(),
            proof: Mutex::new(proof),
            anchor,
            person,
            device: device_public_key,
            observed,
            revoked: AtomicBool::new(false),
        });
        let read =
            OriginalCopyReadAdmission::review_installed(installed, &person, owner.clone(), cancel)?;
        Ok(Arc::new(Self { owner, read }))
    }
    pub fn launch_binding(&self) -> io::Result<RetainedLaunchBinding> {
        use launcher::owned_service::ServiceAuthority;
        self.owner.check()?;
        let binding = self
            .owner
            .proof
            .lock()
            .map_err(|_| refused())?
            .binding
            .clone();
        self.owner.check()?;
        Ok(binding)
    }
    pub fn source_admission(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<OriginalCopyReadAdmission>> {
        use launcher::owned_service::ServiceAuthority;
        self.owner.check()?;
        self.read.revalidate(cancel)?;
        Ok(self.read.clone())
    }
    pub fn target_peer(&self, cancel: &AtomicBool) -> io::Result<Arc<ExecutingPeer>> {
        use launcher::owned_service::ServiceAuthority;
        super::super::migration::check_cancel(cancel)?;
        self.owner.check()?;
        let peer = self.owner.proof.lock().map_err(|_| refused())?.peer.clone();
        self.owner.check()?;
        if !peer.current() {
            return Err(refused());
        }
        Ok(peer)
    }
    pub fn revoke(&self) {
        self.owner.revoked.store(true, Ordering::Release);
    }
    /// Call only after actual reader/receiver stop and all controller guards drop.
    pub fn retire(self: Arc<Self>) -> io::Result<()> {
        let owned = match Arc::try_unwrap(self) {
            Ok(owned) => owned,
            Err(shared) => {
                std::mem::forget(shared);
                return Err(refused());
            }
        };
        let result = owned.read.retire();
        if result.is_err() {
            std::mem::forget(owned.owner);
        } else {
            drop(owned.owner);
        }
        result
    }
}

impl launcher::owned_service::ServiceAuthority for ReviewedInstalledOriginalCopy {
    fn check(&self) -> io::Result<()> {
        self.owner.check()?;
        self.read.revalidate(&AtomicBool::new(false))
    }
}
