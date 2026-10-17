//! One canonical original-credential reader construction path. Both boot
//! export and installed-copy admission retain their protected source authority.
use super::{OriginalCallerFacts, OriginalReadClaim, store::original::mounted::MountedReadEpoch};
use crate::setup::flatpak_transfer::offline_original::OriginalFlatpakSources;
use std::{
    fs::File,
    io,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

pub(super) struct Source<'a> {
    pub(super) home: File,
    pub(super) caller: &'a OriginalCallerFacts,
    pub(super) mounted: Option<&'a Arc<MountedReadEpoch>>,
    pub(super) flatpak: Option<&'a OriginalFlatpakSources>,
}
pub(super) fn admit(
    runtime: &Path,
    publisher: &str,
    source: Source<'_>,
    authority: &OriginalReadClaim,
    custody: Arc<dyn launcher::owned_service::ServiceAuthority>,
    cancel: &AtomicBool,
) -> io::Result<launcher::owned_service::CaptureAdmission> {
    custody.check()?;
    let facts = source.caller;
    let mut admission = launcher::owned_service::CaptureAdmission::verify_original_source(
        runtime,
        publisher,
        source.home,
        facts.uid(),
        facts.primary_gid(),
        facts.supplementary_gids(),
        authority.binding_sha256()?,
    )?
    .with_archive_helper()?
    .retain_original_source_custody(custody.clone())?;
    if let Some(epoch) = source.mounted {
        let binding = authority.mounted_binding()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Protected mounted claim missing",
            )
        })?;
        admission =
            admission.with_original_mounted_roots_bound(epoch.launch_inputs(cancel)?, binding)?;
    } else if authority.mounted_binding()?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Mounted claim has no protected epoch",
        ));
    }
    if let Some(sources) = source.flatpak {
        use launcher::owned_service::{OriginalFlatpakBinding, OriginalFlatpakProjection};
        let claim = sources.claim();
        let binding = OriginalFlatpakBinding {
            read_claim_digest: authority.binding_sha256()?,
            inventory_sha256: claim.inventory_sha256(),
            inventory_len: claim.inventory_len(),
            original_uid: claim.original_uid(),
            projections: claim
                .projections()
                .iter()
                .map(|item| OriginalFlatpakProjection {
                    index: item.index,
                    device: item.device,
                    inode: item.inode,
                    owner: item.owner,
                })
                .collect(),
        };
        admission = admission.with_original_flatpak_sources(
            binding,
            sources.inventory_file()?,
            sources.files()?,
            custody.clone(),
        )?;
    }
    custody.check()?;
    Ok(admission)
}
