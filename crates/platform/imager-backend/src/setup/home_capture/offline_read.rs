//! Root-only read-admission for authenticated pending original-source export.
//! No source bytes, output capability, credentials or completion cross here.
use super::store::original::SourceReadContext;
use crate::setup::{
    migration, migration_handoff,
    prepared_setup::{
        OfflineApplicationsPlan, OfflineHomePlan, OriginalSelectionPlan, PreparedSetupReview,
    },
};
use crate::{
    imager_model::TargetDrive,
    media_layout::{self, MediaPartitionBinding},
};
use std::{
    fs::File,
    io,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

pub struct OfflineReadAdmission {
    setup: Arc<PreparedSetupReview>,
    declaration: OriginalSelectionPlan,
    media: File,
    target: TargetDrive,
    binding: MediaPartitionBinding,
    flatpak: Option<crate::setup::flatpak_transfer::offline_original::OriginalFlatpakSources>,
    // Retain selection custody even when the authenticated selected system
    // closure is empty. Home-contained user state is not missing selection.
    flatpak_inventory:
        Option<crate::setup::flatpak_transfer::offline_original::OriginalInventoryGrant>,
    original: SourceReadContext,
    mounted: Option<Arc<super::store::original::mounted::MountedReadEpoch>>,
    validation: Mutex<()>,
    pending_setup: Option<Arc<crate::setup::media_setup::boot_review::AuthenticatedPendingSetup>>,
}
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
impl OfflineReadAdmission {
    /// Caller has confirmed actual namespace death and dropped every other
    /// admission/proof lease. Observe source unmounts rather than delegating
    /// successful cleanup to field Drop.
    pub(super) fn retire(self: Arc<Self>) -> io::Result<()> {
        let owned = match Arc::try_unwrap(self) {
            Ok(owned) => owned,
            Err(shared) => {
                // Keep exact unresolved source custody; never let this error
                // accidentally become a final Drop-based unmount acknowledgement.
                std::mem::forget(shared);
                return Err(refused("Original read admission still has custody leases"));
            }
        };
        let Self {
            setup,
            declaration,
            media,
            target,
            binding,
            original,
            mounted,
            flatpak,
            flatpak_inventory,
            validation,
            pending_setup,
        } = owned;
        drop((
            setup,
            declaration,
            media,
            target,
            binding,
            flatpak_inventory,
            validation,
            pending_setup,
        ));
        if let Some(epoch) = mounted {
            if let Err(error) = epoch.retire() {
                std::mem::forget((original, flatpak));
                return Err(error);
            }
        }
        let roots = match flatpak {
            Some(sources) => sources.into_retirement_sources(),
            None => Ok(Vec::new()),
        };
        let mut roots = match roots {
            Ok(roots) => roots,
            Err(error) => {
                std::mem::forget(original);
                return Err(error);
            }
        };
        roots.push(Arc::new(original.into_retirement_source()));
        migration_handoff::ResolvedSource::retire_many(roots)
    }
    /// The boot controller supplies its explicitly selected held medium. Never
    /// discover another disk from declaration UUIDs or deserialize a media grant.
    /// The setup has already been password-authenticated by the canonical parser.
    pub(in crate::setup) fn review(
        setup: Arc<PreparedSetupReview>,
        media: File,
        target: TargetDrive,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        // SAFETY: reads current effective credentials only; never changes groups.
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(refused(
                "Original offline read admission requires the boot root controller",
            ));
        }
        migration::check_cancel(cancel)?;
        let declaration = setup.pending_original_declaration()?;
        let binding = attest_medium(&media, &target)?;
        if declaration.media() != &binding {
            return Err(refused(
                "Pending export is bound to a different actual medium",
            ));
        }
        // Exact operation/setup/identity/media metadata has authenticated before
        // any source mount. The existing resolver rejects duplicates, writable
        // aliases and unsupported layers; no request path replaces source facts.
        migration::check_cancel(cancel)?;
        let source = migration_handoff::mount_authenticated_read_only(declaration.source())?;
        let witness = declaration
            .source_witness()
            .ok_or_else(|| refused("Source witness missing"))?;
        let original = match SourceReadContext::review_witness(source.clone(), witness) {
            Ok(original) => {
                drop(source);
                original
            }
            Err(error) => {
                source.retire()?;
                return Err(error);
            }
        };
        if let Err(error) = original.verify_requested(
            declaration.source(),
            witness,
            declaration.requested_caller(),
        ) {
            original.retire()?;
            return Err(error);
        }
        let mut admission = Self {
            setup,
            declaration,
            media,
            target,
            binding,
            original,
            mounted: None,
            flatpak: None,
            flatpak_inventory: None,
            validation: Mutex::new(()),
            pending_setup: None,
        };
        let reviewed = (|| {
            admission.flatpak_inventory =
                crate::setup::flatpak_transfer::offline_original::OriginalInventoryGrant::review(
                    &admission.original,
                )?;
            admission.flatpak =
                crate::setup::flatpak_transfer::offline_original::OriginalFlatpakSources::review(
                    &admission.original,
                )?;
            if let Some(witness) = admission.declaration.mounted_witness() {
                admission.mounted = Some(Arc::new(
                    admission
                        .original
                        .retain_mounted_for_export(witness, cancel)?,
                ));
            }
            admission.revalidate(cancel)
        })();
        if let Err(error) = reviewed {
            Arc::new(admission).retire()?;
            return Err(error);
        }
        Ok(admission)
    }
    /// Retain the exact authenticated quarantine before the initial readonly
    /// medium mount retires. No source or medium FD comes from public metadata.
    pub(in crate::setup) fn review_boot_pending(
        pending: &crate::setup::media_setup::boot_review::PendingBootReview<'_>,
        media: File,
        target: TargetDrive,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        pending.validate()?;
        let retained = pending.retained_setup()?;
        let mut admission = Self::review(retained.authenticated_setup(), media, target, cancel)?;
        if &admission.binding != pending.selected_binding() {
            return Err(refused("Pending setup actual medium differs"));
        }
        pending.validate()?;
        retained.validate()?;
        admission.pending_setup = Some(retained);
        admission.revalidate(cancel)?;
        Ok(admission)
    }
    pub(super) fn qualification_owned_sources(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<String>> {
        self.revalidate(cancel)?;
        let caller = self.declaration.requested_caller();
        if caller.uid() != 1000
            || caller.primary_gid() != 1001
            || caller.supplementary_gids() != [1002, 1003]
        {
            return Err(refused(
                "Qualification requires authenticated original fixture caller",
            ));
        }
        self.original.qualification_owned_root()?;
        self.mounted
            .as_ref()
            .ok_or_else(|| refused("Qualification lacks approved mounted source witness"))?
            .qualification_owned_roots(cancel)?;
        if !self.output_flatpak_required(cancel)? {
            return Err(refused(
                "Qualification requires selected native applications",
            ));
        }
        let mut disks = Vec::new();
        for bytes in self.output_source_plans(cancel)? {
            let plan = migration_handoff::Plan::parse(&bytes)?;
            if !disks.contains(&plan.home.volume.disk_guid) {
                disks.push(plan.home.volume.disk_guid);
            }
        }
        disks.sort();
        if disks.len() != 2
            || disks
                .iter()
                .any(|guid| guid.eq_ignore_ascii_case(&self.binding.disk_guid))
        {
            return Err(refused(
                "Qualification requires three genuine disjoint GPT subjects",
            ));
        }
        self.revalidate(cancel)?;
        Ok(disks)
    }
    pub(super) fn authenticated_setup(&self) -> &Arc<PreparedSetupReview> {
        &self.setup
    }
    pub(super) fn pending_setup_expectation(
        &self,
    ) -> io::Result<&crate::imager_model::PreparedSetupExpectation> {
        let pending = self.pending_setup.as_ref().ok_or_else(|| {
            refused("Original output lacks authenticated pending package custody")
        })?;
        pending.validate()?;
        Ok(pending.pending_expectation())
    }
    /// Keep setup, selected raw medium, readonly source mount and protected
    /// record alive through canonical namespace supervision, including an owned
    /// service returned by the callback. No source
    /// fd/path or groups are returned to a serialized frontend request.
    /// This reader has NO output authority or completed-export transaction.
    fn with_reader<T>(
        self: &Arc<Self>,
        runtime: &std::path::Path,
        publisher: &str,
        cancel: &AtomicBool,
        authority: &super::worker::OriginalReadClaim,
        run: impl FnOnce(launcher::owned_service::CaptureAdmission) -> io::Result<T>,
    ) -> io::Result<T> {
        self.revalidate(cancel)?;
        let admission = super::original_reader::admit(
            runtime,
            publisher,
            super::original_reader::Source {
                home: self.clone_original_home()?,
                caller: self.original.original(),
                mounted: self.mounted.as_ref(),
                flatpak: self.flatpak.as_ref(),
            },
            authority,
            Arc::new(OriginalReadCustody {
                read: self.clone(),
                mounted_binding: authority.mounted_binding()?,
            }),
            cancel,
        )?;
        let result = run(admission)?;
        self.revalidate(cancel)?;
        Ok(result)
    }
    /// Distinct read command is minted while the genuine original admission
    /// stays held through supervision. It has no output or completion authority.
    pub fn with_original_reader<T>(
        self: &Arc<Self>,
        runtime: &std::path::Path,
        publisher: &str,
        cancel: &AtomicBool,
        run: impl FnOnce(
            launcher::owned_service::CaptureAdmission,
            super::OriginalReaderRequest,
            [u8; 32],
        ) -> io::Result<T>,
    ) -> io::Result<T> {
        let authority = self.mint_original_read_claim(cancel)?;
        // Compiled pins and canonical selected sidecar, never a caller hint.
        if authority.is_applications_only() {
            let service = super::runtime::TrustedCaptureService::native_for_protocol(
                authority.required_protocol(),
            )?;
            if service.root != runtime || service.publisher != publisher {
                return Err(refused(
                    "Original reader runtime differs from selected signed service",
                ));
            }
        }
        let request_binding_sha256 = authority.binding_sha256()?;
        let request = super::worker::Request {
            schema: authority.required_protocol(),
            operation: self.declaration.source().operation_id.clone(),
            custody: None,
            command: super::worker::Command::ReviewOriginal {
                authority: authority.clone(),
            },
        };
        request.validate_protocol()?;
        let result = self.with_reader(runtime, publisher, cancel, &authority, |admission| {
            run(admission, request, request_binding_sha256)
        });
        self.revalidate(cancel)?;
        result
    }
    /// Cleanup-only physical custody. Does not renew source/setup permission,
    /// mint output or depend on a cancellation token. Allows owned unmount after
    /// a cancelled/revoked worker is canonically confirmed quiescent.
    pub(super) fn check_output_medium_for_cleanup(&self, media: &File) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let actual = media.metadata()?;
        let held = self.media.metadata()?;
        if (actual.dev(), actual.ino(), actual.rdev()) != (held.dev(), held.ino(), held.rdev())
            || attest_medium(media, &self.target)? != self.binding
        {
            return Err(refused("Original cleanup medium custody differs"));
        }
        Ok(())
    }
    pub(super) fn output_raw_medium(&self, cancel: &AtomicBool) -> io::Result<File> {
        self.revalidate(cancel)?;
        let file = self.media.try_clone()?;
        self.check_output_medium(&file, cancel)?;
        Ok(file)
    }
    pub(super) fn output_declaration(&self, cancel: &AtomicBool) -> io::Result<OfflineHomePlan> {
        self.revalidate(cancel)?;
        Ok(self.declaration.as_home()?.clone())
    }
    pub(super) fn output_selection_declaration(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<OriginalSelectionPlan> {
        self.revalidate(cancel)?;
        Ok(self.declaration.clone())
    }
    pub(super) fn output_applications_declaration(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<OfflineApplicationsPlan> {
        self.revalidate(cancel)?;
        Ok(self.declaration.as_applications()?.clone())
    }
    pub(super) fn output_original_home(&self, cancel: &AtomicBool) -> io::Result<File> {
        self.revalidate(cancel)?;
        self.clone_original_home()
    }
    pub(super) fn output_source_plans(&self, cancel: &AtomicBool) -> io::Result<Vec<Vec<u8>>> {
        self.revalidate(cancel)?;
        let mut home = self.declaration.source().clone();
        // Geometry comes from the protected selection; marker bytes are not a
        // source grant and need not be repeated in the collision checker.
        home.flatpak_inventory = None;
        let mut plans = vec![home.bytes()?];
        if let Some(applications) = &self.flatpak {
            plans.extend(applications.source_plans()?);
        }
        if let Some(epoch) = &self.mounted {
            epoch.append_source_geometry(&mut plans, cancel)?;
        }
        self.revalidate(cancel)?;
        Ok(plans)
    }
    pub(super) fn output_flatpak_required(&self, cancel: &AtomicBool) -> io::Result<bool> {
        self.revalidate(cancel)?;
        Ok(self.flatpak.is_some())
    }
    pub(super) fn output_flatpak_operation(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<Option<String>> {
        let claim = self.mint_original_read_claim(cancel)?;
        if self.flatpak.is_some() {
            Ok(Some(claim.applications_operation()?.into()))
        } else {
            Ok(None)
        }
    }
    pub(super) fn output_read_claim_digest(&self, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
        self.mint_original_read_claim(cancel)?.binding_sha256()
    }
    pub(super) fn check_output_medium(&self, media: &File, cancel: &AtomicBool) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        self.revalidate(cancel)?;
        let original = self.media.metadata()?;
        let supplied = media.metadata()?;
        if (original.dev(), original.ino(), original.rdev())
            != (supplied.dev(), supplied.ino(), supplied.rdev())
            || attest_medium(media, &self.target)? != self.binding
        {
            return Err(refused(
                "Original output did not retain the selected medium",
            ));
        }
        self.revalidate(cancel)
    }
    /// Reader and output proof share one minting path. Hold the record offset
    /// lock through every reattestation, inode clone and omission translation;
    /// neither can bind a partially renewed or differently observed claim.
    fn mint_original_read_claim(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<super::worker::OriginalReadClaim> {
        let _validation = self
            .validation
            .lock()
            .map_err(|_| io::Error::other("Original read validation poisoned"))?;
        self.revalidate_locked(cancel)?;
        let home = self.original.clone_original_home()?;
        let omissions = self.original.original_omissions()?;
        let manifest = self.setup.manifest();
        let mounted = match (&self.mounted, self.declaration.mounted_witness()) {
            (Some(epoch), Some(witness)) => Some(super::worker::OriginalMountedClaim::admitted(
                witness,
                epoch.table_sha256(cancel)?,
            )?),
            (None, None) => None,
            _ => return Err(refused("Mounted declaration and protected epoch differ")),
        };
        let claim = super::worker::OriginalReadClaim::admitted(
            &self.declaration,
            &home,
            &manifest.setup_binding,
            manifest.identity_public_key,
            &omissions,
            self.flatpak.as_ref().map(|sources| sources.claim()),
            mounted.as_ref(),
        )?;
        self.revalidate_locked(cancel)?;
        Ok(claim)
    }
    fn clone_original_home(&self) -> io::Result<File> {
        let _validation = self
            .validation
            .lock()
            .map_err(|_| io::Error::other("Original read validation poisoned"))?;
        self.original.clone_original_home()
    }
    pub fn revalidate(&self, cancel: &AtomicBool) -> io::Result<()> {
        // Endpoints and supervision can check concurrently. SourceReadContext
        // uses cloned descriptors sharing a file offset; serialize its checks.
        let _validation = self
            .validation
            .lock()
            .map_err(|_| io::Error::other("Original read validation poisoned"))?;
        self.revalidate_locked(cancel)
    }
    /// Mint output selection authority from the retained root-controlled record.
    /// Public expectations, request JSON and password-authenticated metadata do
    /// not construct this proof. The original read Arc owns every source lease.
    pub(super) fn admit_application_output(
        self: &Arc<Self>,
        cancel: &AtomicBool,
    ) -> io::Result<Option<OriginalApplicationsOutput>> {
        let _validation = self
            .validation
            .lock()
            .map_err(|_| io::Error::other("Original read validation poisoned"))?;
        self.revalidate_locked(cancel)?;
        let Some(inventory) = &self.flatpak_inventory else {
            return Ok(None);
        };
        let proof = OriginalApplicationsOutput {
            read: self.clone(),
            inventory_digest: inventory.digest(),
            projections: self.flatpak.as_ref().map(|sources| sources.claim().clone()),
        };
        self.revalidate_locked(cancel)?;
        Ok(Some(proof))
    }
    /// Caller retains `validation`. Exact selected system scopes must have
    /// projected leases; verified empty selection is carried wholly by Home.
    fn check_application_selection_locked(&self) -> io::Result<()> {
        let selected = self.declaration.source().categories.flatpak;
        if selected != self.flatpak_inventory.is_some() {
            return Err(refused(
                "Original application consent and inventory custody differ",
            ));
        }
        let Some(inventory) = &self.flatpak_inventory else {
            if self.flatpak.is_some() {
                return Err(refused("Original applications lack selection consent"));
            }
            return Ok(());
        };
        if self.declaration.is_applications_only() {
            inventory.matches_applications_plan(self.declaration.source())?;
        } else {
            inventory.matches_plan(self.declaration.source())?;
        }
        let projected = !inventory.inventory().selected().is_empty();
        if projected != self.flatpak.is_some() {
            return Err(refused(
                "Original selected system scopes lack complete projected custody",
            ));
        }
        if let Some(sources) = &self.flatpak {
            let claim = sources.claim();
            if claim.selected_data_ids().is_some() != self.declaration.is_applications_only()
                || claim.inventory_sha256() != inventory.digest()
                || claim.original_uid() != self.original.original().uid()
            {
                return Err(refused("Original projected inventory or owner differs"));
            }
        }
        Ok(())
    }
    /// Caller holds `validation`; never acquire it recursively.
    fn revalidate_locked(&self, cancel: &AtomicBool) -> io::Result<()> {
        migration::check_cancel(cancel)?;
        if let Some(pending) = &self.pending_setup {
            pending.validate()?;
            if !Arc::ptr_eq(&self.setup, &pending.authenticated_setup()) {
                return Err(refused("Original pending setup custody differs"));
            }
        }
        if self.setup.pending_original_declaration()? != self.declaration
            || attest_medium(&self.media, &self.target)? != self.binding
        {
            return Err(refused(
                "Authenticated setup or held medium changed during source admission",
            ));
        }
        let witness = self
            .declaration
            .source_witness()
            .ok_or_else(|| refused("Source witness missing"))?;
        self.original.verify_requested(
            self.declaration.source(),
            witness,
            self.declaration.requested_caller(),
        )?;
        match (&self.mounted, self.declaration.mounted_witness()) {
            (Some(epoch), Some(_)) => epoch.with_sources(cancel, |_| Ok(()))?,
            (None, None) => {}
            _ => return Err(refused("Mounted declaration and epoch custody differ")),
        }
        self.check_application_selection_locked()?;
        if let Some(sources) = &self.flatpak {
            sources.files()?;
        }
        migration::check_cancel(cancel)
    }
}
/// Nonserializable proof tied to the exact retained original read admission.
/// It is not an output capability; the actual native medium must admit it too.
pub struct OriginalApplicationsOutput {
    read: Arc<OfflineReadAdmission>,
    inventory_digest: [u8; 32],
    projections: Option<crate::setup::flatpak_transfer::offline_original::OriginalFlatpakClaim>,
}
impl OriginalApplicationsOutput {
    pub(in crate::setup::home_capture) fn validate_for(
        &self,
        read: &Arc<OfflineReadAdmission>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if !Arc::ptr_eq(&self.read, read) {
            return Err(refused(
                "Original application output belongs to a different read admission",
            ));
        }
        let _validation = read
            .validation
            .lock()
            .map_err(|_| io::Error::other("Original read validation poisoned"))?;
        read.revalidate_locked(cancel)?;
        let inventory = read
            .flatpak_inventory
            .as_ref()
            .ok_or_else(|| refused("Original application inventory custody was removed"))?;
        if inventory.digest() != self.inventory_digest
            || read.flatpak.as_ref().map(|sources| sources.claim()) != self.projections.as_ref()
        {
            return Err(refused("Original application output selection changed"));
        }
        read.revalidate_locked(cancel)
    }
    /// Preflight the real signed archive/OpenPGP helper closure without launching
    /// a worker or mutating the output. Missing signed tools cannot become a
    /// successfully admitted application-output transaction.
    pub(in crate::setup::home_capture) fn verify_signed_capsule(&self) -> io::Result<()> {
        let service = super::runtime::TrustedCaptureService::native()?;
        self.read.with_original_reader(
            &service.root,
            &service.publisher,
            &AtomicBool::new(false),
            |_, _, _| Ok(()),
        )
    }
}
// Native, private attachment: callback code cannot replace the exact context
// with an unrelated ServiceAuthority. Canonical cancellation remains owned by
// ServiceCancellation; this guard checks the retained read facts independently.
struct OriginalReadCustody {
    read: Arc<OfflineReadAdmission>,
    mounted_binding: Option<launcher::owned_service::OriginalMountedBinding>,
}
impl launcher::owned_service::ServiceAuthority for OriginalReadCustody {
    fn original_mounted_binding(
        &self,
    ) -> io::Result<Option<launcher::owned_service::OriginalMountedBinding>> {
        self.check()?;
        Ok(self.mounted_binding)
    }
    fn check(&self) -> io::Result<()> {
        self.read.revalidate(&AtomicBool::new(false))
    }
}
fn attest_medium(media: &File, target: &TargetDrive) -> io::Result<MediaPartitionBinding> {
    // Read-only custody validation deliberately allows the booted medium to be
    // the current system disk; it does not call writer prepare/unmount/format.
    crate::write::attest_target_descriptor(target, media)?;
    let stable = target
        .stable_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| refused("Explicitly selected boot medium has no stable identity"))?;
    let binding = media_layout::binding(media, target.size, stable)?;
    crate::write::attest_target_descriptor(target, media)?;
    Ok(binding)
}
