//! Physical completion companion for an actual installed original-copy owner.
//! Original record, witness and inventory bytes remain the requested geometry.
use crate::setup::{
    migration_handoff::{Plan, ResolvedSource},
    prepared_setup::{AuthenticatedInstalledOriginalCopy, PlannedBtrfsResize, RetainedHomePlan},
};
use std::{io, sync::Arc};

pub struct CompletedOriginalGeometry {
    installed: Arc<AuthenticatedInstalledOriginalCopy>,
    original: Plan,
    shared: RetainedHomePlan,
    resize: PlannedBtrfsResize,
    effective: Plan,
}
impl CompletedOriginalGeometry {
    pub(super) fn admit(
        installed: &Arc<AuthenticatedInstalledOriginalCopy>,
    ) -> io::Result<Option<Arc<Self>>> {
        installed.revalidate()?;
        let manifest = installed.manifest();
        let Some(resize) = &manifest.planned_btrfs_resize else {
            return Ok(None);
        };
        let shared = manifest
            .retained_home
            .as_ref()
            .ok_or_else(|| super::failure("Resize lacks authenticated shared source"))?;
        let original = manifest
            .installed_original_copy_declaration()?
            .ok_or_else(|| super::failure("Resize lacks authenticated original copy"))?
            .source();
        let effective = resize.effective_original_copy_plan(shared, original)?;
        let result = Arc::new(Self {
            installed: installed.clone(),
            original: original.clone(),
            shared: shared.clone(),
            resize: resize.clone(),
            effective,
        });
        result.check_record()?;
        Ok(Some(result))
    }
    fn check_record(&self) -> io::Result<()> {
        self.installed.revalidate()?;
        let manifest = self.installed.manifest();
        if manifest
            .installed_original_copy_declaration()?
            .map(|d| d.source())
            != Some(&self.original)
            || manifest.retained_home.as_ref() != Some(&self.shared)
            || manifest.planned_btrfs_resize.as_ref() != Some(&self.resize)
            || self
                .resize
                .effective_original_copy_plan(&self.shared, &self.original)?
                != self.effective
        {
            return Err(super::failure(
                "Completed original geometry differs from installed record",
            ));
        }
        self.installed.revalidate()
    }
    pub fn original(&self) -> &Plan {
        &self.original
    }
    pub fn effective(&self) -> &Plan {
        &self.effective
    }
    pub fn check(&self, source: &ResolvedSource) -> io::Result<()> {
        self.check_record()?;
        if source.original_plan() != &self.effective {
            return Err(super::failure(
                "Completed source differs from effective physical geometry",
            ));
        }
        // Verify the actual shrunk shared filesystem even if this copy's primary
        // root is elsewhere. Such a selection needs separately held completion.
        if self.effective.home.volume
            != self
                .resize
                .effective_plan(&self.shared)?
                .as_plan()
                .home
                .volume
        {
            return Err(super::failure(
                "Completed original copy lacks the same held resized volume",
            ));
        }
        source.verify_completed_btrfs_resize(self.resize.filesystem_bytes())?;
        self.check_record()
    }
    pub fn map_related(&self, original: &Plan) -> io::Result<Plan> {
        self.check_record()?;
        if original.operation_id != self.original.operation_id {
            return Err(super::failure(
                "Related original source differs from authenticated copy operation",
            ));
        }
        let effective = self
            .resize
            .effective_original_copy_plan(&self.shared, original)?;
        self.check_record()?;
        Ok(effective)
    }
}

#[cfg(all(test, feature = "native-capture-qualification"))]
mod native_tests;
