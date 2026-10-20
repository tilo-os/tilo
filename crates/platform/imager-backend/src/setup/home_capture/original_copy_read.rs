//! Root-only source custody for a genuinely authenticated installed copy.
//! This contains no output grant, launch permit or target credential mapping.
use super::{OriginalReadClaim, SourceReadContext, store::original::mounted::MountedReadEpoch};
use crate::setup::{
    flatpak_transfer::offline_original::{OriginalFlatpakSources, OriginalInventoryGrant},
    migration, migration_handoff,
    prepared_setup::{AuthenticatedInstalledOriginalCopy, OriginalCopyDeclaration},
};
use std::{
    io,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

static UNCERTAIN_RETIREMENT: AtomicBool = AtomicBool::new(false);
fn uncertain() {
    UNCERTAIN_RETIREMENT.store(true, std::sync::atomic::Ordering::Release);
}
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
// Consuming native retirement APIs retain failed mount leases themselves. Their
// errors must also retain the authenticated installed owner, including failures
// before a ReviewedInstalledOriginalCopy wrapper has been constructed.
struct RetirementCustody {
    owner: Option<Arc<dyn launcher::owned_service::ServiceAuthority>>,
    installed: Option<Arc<AuthenticatedInstalledOriginalCopy>>,
}
impl RetirementCustody {
    fn new(
        owner: &Arc<dyn launcher::owned_service::ServiceAuthority>,
        installed: &Arc<AuthenticatedInstalledOriginalCopy>,
    ) -> Self {
        Self {
            owner: Some(owner.clone()),
            installed: Some(installed.clone()),
        }
    }
    fn completed(mut self) {
        self.owner.take();
        self.installed.take();
    }
}
impl Drop for RetirementCustody {
    fn drop(&mut self) {
        if let (Some(owner), Some(installed)) = (self.owner.take(), self.installed.take()) {
            uncertain();
            // The controller must stay alive when retirement_pending is true.
            // Never release live proof custody after an unobserved retirement.
            std::mem::forget((owner, installed));
        }
    }
}
struct Sources {
    original: SourceReadContext,
    mounted: Option<Arc<MountedReadEpoch>>,
    flatpak: Option<OriginalFlatpakSources>,
    inventory: Option<OriginalInventoryGrant>,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct OriginalCopyScratchScope {
    pub(super) operation: String,
    pub(super) caller: (u32, u32),
    pub(super) reservation: u64,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct OriginalCopyAttemptScope {
    pub(super) operation: String,
    pub(super) declaration: [u8; 32],
    pub(super) person: [u8; 32],
    pub(super) setup_binding: String,
    pub(super) caller: (u32, u32),
    pub(super) reservation: u64,
}
pub struct OriginalCopyReadAdmission {
    installed: Arc<AuthenticatedInstalledOriginalCopy>,
    declaration: OriginalCopyDeclaration,
    owner: Arc<dyn launcher::owned_service::ServiceAuthority>,
    sources: Option<Sources>,
    validation: Mutex<()>,
}
impl OriginalCopyReadAdmission {
    pub fn retirement_pending() -> bool {
        UNCERTAIN_RETIREMENT.load(std::sync::atomic::Ordering::Acquire)
    }
    /// The root installed actor must authenticate the active person separately;
    /// its identity is never inferred from filesystem ownership or request JSON.
    pub(in crate::setup) fn review_installed(
        installed: Arc<AuthenticatedInstalledOriginalCopy>,
        active_person: &[u8; 32],
        owner: Arc<dyn launcher::owned_service::ServiceAuthority>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        migration::check_cancel(cancel)?;
        owner.check()?;
        installed.revalidate()?;
        let manifest = installed.manifest();
        let declaration = manifest
            .installed_original_copy_declaration()?
            .ok_or_else(|| refused("Authenticated original copy is absent"))?
            .clone();
        if manifest.identity_public_key != *active_person
            || declaration.identity_public_key() != *active_person
            || declaration.setup_binding() != manifest.setup_binding
        {
            return Err(refused(
                "Installed copy differs from authenticated active person",
            ));
        }
        declaration.validate()?;
        let construction_custody = RetirementCustody::new(&owner, &installed);
        // The constructor preserves the immutable requested declaration while
        // independently verifying any authenticated completed resize against
        // the actual held physical source before a reader can be admitted.
        let original = SourceReadContext::review_installed_copy(&installed)?;
        let mut owned = Self {
            installed,
            declaration,
            owner,
            sources: Some(Sources {
                original,
                mounted: None,
                flatpak: None,
                inventory: None,
            }),
            validation: Mutex::new(()),
        };
        construction_custody.completed();
        let reviewed = (|| {
            let sources = owned.sources.as_mut().ok_or(io::ErrorKind::NotConnected)?;
            sources.original.verify_requested(
                owned.declaration.source(),
                owned.declaration.source_witness(),
                owned.declaration.requested_caller(),
            )?;
            sources.inventory = OriginalInventoryGrant::review(&sources.original)?;
            sources.flatpak = OriginalFlatpakSources::review(&sources.original)?;
            if let Some(witness) = owned.declaration.mounted_witness() {
                sources.mounted = Some(Arc::new(
                    sources
                        .original
                        .retain_mounted_for_export(witness, cancel)?,
                ));
            }
            owned.revalidate(cancel)
        })();
        if let Err(error) = reviewed {
            Arc::new(owned).retire()?;
            return Err(error);
        }
        Ok(Arc::new(owned))
    }
    fn check_locked(&self, cancel: &AtomicBool) -> io::Result<()> {
        migration::check_cancel(cancel)?;
        self.owner.check()?;
        self.installed.revalidate()?;
        if self
            .installed
            .manifest()
            .installed_original_copy_declaration()?
            != Some(&self.declaration)
        {
            return Err(refused("Installed source declaration changed"));
        }
        let sources = self.sources.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        sources.original.verify_requested(
            self.declaration.source(),
            self.declaration.source_witness(),
            self.declaration.requested_caller(),
        )?;
        match (&sources.mounted, self.declaration.mounted_witness()) {
            (Some(epoch), Some(_)) => epoch.with_sources(cancel, |_| Ok(()))?,
            (None, None) => {}
            _ => return Err(refused("Original copy mounted source custody differs")),
        }
        if let Some(sources) = &sources.flatpak {
            sources.files()?;
        }
        self.installed.revalidate()?;
        self.owner.check()?;
        migration::check_cancel(cancel)
    }
    /// Authenticated output scope, never caller-provided budget or credentials.
    pub(super) fn output_scratch_scope(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<OriginalCopyScratchScope> {
        let _guard = self
            .validation
            .lock()
            .map_err(|_| refused("Original copy validation poisoned"))?;
        self.check_locked(cancel)?;
        let caller = self.declaration.requested_caller();
        let result = OriginalCopyScratchScope {
            operation: self.declaration.source().operation_id.clone(),
            caller: (caller.uid(), caller.primary_gid()),
            reservation: self.declaration.reservation(),
        };
        self.check_locked(cancel)?;
        Ok(result)
    }
    pub(super) fn output_attempt_scope(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<OriginalCopyAttemptScope> {
        use sha2::Digest;
        let _guard = self
            .validation
            .lock()
            .map_err(|_| refused("Original copy validation poisoned"))?;
        self.check_locked(cancel)?;
        let caller = self.declaration.requested_caller();
        let result = OriginalCopyAttemptScope {
            operation: self.declaration.source().operation_id.clone(),
            declaration: sha2::Sha256::digest(self.declaration.bytes()?).into(),
            person: self.declaration.identity_public_key(),
            setup_binding: self.declaration.setup_binding().into(),
            caller: (caller.uid(), caller.primary_gid()),
            reservation: self.declaration.reservation(),
        };
        self.check_locked(cancel)?;
        Ok(result)
    }
    /// Validate output against a freshly rebuilt claim from the actual held
    /// source admission, before the durable Store accepts export expectations.
    pub(super) fn validate_output_expectations(
        &self,
        home: Option<&crate::setup::portable_home_media::Expectation>,
        applications: Option<&crate::setup::flatpak_transfer::portable::Expectation>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.claim(cancel)?
            .validate_installed_output(home, applications)?;
        self.revalidate(cancel)
    }
    pub(super) fn output_original_home(&self, cancel: &AtomicBool) -> io::Result<std::fs::File> {
        let _guard = self
            .validation
            .lock()
            .map_err(|_| refused("Original copy validation poisoned"))?;
        self.check_locked(cancel)?;
        let file = self
            .sources
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .original
            .clone_original_home()?;
        self.check_locked(cancel)?;
        Ok(file)
    }
    pub(super) fn output_read_claim_digest(&self, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
        self.claim(cancel)?.binding_sha256()
    }
    pub fn revalidate(&self, cancel: &AtomicBool) -> io::Result<()> {
        let _guard = self
            .validation
            .lock()
            .map_err(|_| refused("Original copy validation poisoned"))?;
        self.check_locked(cancel)
    }
    /// Geometry only from the retained protected Home, signed inventory and
    /// authenticated mounted record. The actual source lease remains held.
    pub fn output_source_plans(&self, cancel: &AtomicBool) -> io::Result<Vec<Vec<u8>>> {
        let _guard = self
            .validation
            .lock()
            .map_err(|_| refused("Original copy validation poisoned"))?;
        self.check_locked(cancel)?;
        let sources = self.sources.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        let mut home = self.declaration.source().clone();
        home.flatpak_inventory = None;
        let mut plans = vec![home.bytes()?];
        if let Some(flatpak) = &sources.flatpak {
            plans.extend(flatpak.source_plans()?);
        }
        if let Some(mounted) = &sources.mounted {
            mounted.append_source_geometry(&mut plans, cancel)?;
        }
        self.check_locked(cancel)?;
        Ok(plans)
    }
    fn claim(&self, cancel: &AtomicBool) -> io::Result<OriginalReadClaim> {
        let _guard = self
            .validation
            .lock()
            .map_err(|_| refused("Original copy validation poisoned"))?;
        self.check_locked(cancel)?;
        let sources = self.sources.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        let home = sources.original.clone_original_home()?;
        let omissions = sources.original.original_omissions()?;
        let exclusions = sources.original.original_exclusions()?;
        let mounted = match (&sources.mounted, self.declaration.mounted_witness()) {
            (Some(epoch), Some(witness)) => Some(super::worker::OriginalMountedClaim::admitted(
                witness,
                epoch.table_sha256(cancel)?,
            )?),
            (None, None) => None,
            _ => return Err(refused("Original copy mounted claim differs")),
        };
        let result = OriginalReadClaim::admitted_copy(
            &self.declaration,
            &home,
            self.declaration.setup_binding(),
            self.declaration.identity_public_key(),
            &omissions,
            &exclusions,
            sources.flatpak.as_ref().map(|sources| sources.claim()),
            mounted.as_ref(),
        )?;
        self.check_locked(cancel)?;
        Ok(result)
    }
    /// Uses the same signed service/credential/projection builder as boot read.
    /// The actual namespace controller must retain this admission until stop.
    pub fn with_original_reader<T>(
        self: &Arc<Self>,
        cancel: &AtomicBool,
        run: impl FnOnce(
            launcher::owned_service::CaptureAdmission,
            super::OriginalReaderRequest,
            [u8; 32],
        ) -> io::Result<T>,
    ) -> io::Result<T> {
        let authority = self.claim(cancel)?;
        let service = super::runtime::TrustedCaptureService::native_for_protocol(
            authority.required_protocol(),
        )?;
        let digest = authority.binding_sha256()?;
        let request = super::worker::Request {
            schema: authority.required_protocol(),
            operation: self.declaration.source().operation_id.clone(),
            custody: None,
            command: super::worker::Command::ReviewOriginal {
                authority: authority.clone(),
            },
        };
        request.validate_protocol()?;
        self.revalidate(cancel)?;
        let sources = self.sources.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        let home = {
            let _guard = self
                .validation
                .lock()
                .map_err(|_| refused("Original copy validation poisoned"))?;
            sources.original.clone_original_home()?
        };
        let admission = super::original_reader::admit(
            &service.root,
            &service.publisher,
            super::original_reader::Source {
                home,
                caller: sources.original.original(),
                mounted: sources.mounted.as_ref(),
                flatpak: sources.flatpak.as_ref(),
            },
            &authority,
            Arc::new(ReadCustody {
                read: self.clone(),
                mounted: authority.mounted_binding()?,
            }),
            cancel,
        )?;
        let result = run(admission, request, digest)?;
        self.revalidate(cancel)?;
        Ok(result)
    }
    /// Only after actual reader namespace death and all read guards have left.
    /// Outstanding guards retain custody rather than report successful teardown.
    pub fn retire(self: Arc<Self>) -> io::Result<()> {
        let mut owned = match Arc::try_unwrap(self) {
            Ok(owned) => owned,
            Err(shared) => {
                uncertain();
                std::mem::forget(shared);
                return Err(refused("Original copy still has reader custody"));
            }
        };
        let custody = RetirementCustody::new(&owned.owner, &owned.installed);
        let sources = owned.sources.take().ok_or(io::ErrorKind::NotConnected)?;
        let Sources {
            original,
            mounted,
            flatpak,
            inventory,
        } = sources;
        drop(inventory);
        if let Some(epoch) = mounted {
            if let Err(error) = epoch.retire() {
                uncertain();
                std::mem::forget((original, flatpak));
                return Err(error);
            }
        }
        let mut roots = match flatpak
            .map(|sources| sources.into_retirement_sources())
            .transpose()
        {
            Ok(roots) => roots.unwrap_or_default(),
            Err(error) => {
                uncertain();
                std::mem::forget(original);
                return Err(error);
            }
        };
        roots.push(Arc::new(original.into_retirement_source()));
        let result = migration_handoff::ResolvedSource::retire_many(roots);
        if result.is_ok() {
            custody.completed();
        }
        result
    }
}
struct ReadCustody {
    read: Arc<OriginalCopyReadAdmission>,
    mounted: Option<launcher::owned_service::OriginalMountedBinding>,
}
impl launcher::owned_service::ServiceAuthority for ReadCustody {
    fn original_mounted_binding(
        &self,
    ) -> io::Result<Option<launcher::owned_service::OriginalMountedBinding>> {
        self.check()?;
        Ok(self.mounted)
    }
    fn check(&self) -> io::Result<()> {
        self.read.revalidate(&AtomicBool::new(false))
    }
}
impl Drop for OriginalCopyReadAdmission {
    fn drop(&mut self) {
        if let Some(sources) = self.sources.take() {
            uncertain();
            struct Unknown {
                _sources: Sources,
                _owner: Arc<dyn launcher::owned_service::ServiceAuthority>,
                _installed: Arc<AuthenticatedInstalledOriginalCopy>,
            }
            let unknown = Unknown {
                _sources: sources,
                _owner: self.owner.clone(),
                _installed: self.installed.clone(),
            };
            static UNKNOWN: Mutex<Vec<Unknown>> = Mutex::new(Vec::new());
            match UNKNOWN.lock() {
                Ok(mut roots) if roots.len() < 64 => roots.push(unknown),
                _ => std::mem::forget(unknown),
            }
        }
    }
}
