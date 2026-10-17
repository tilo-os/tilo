//! Authenticated prospective geometry; never a block or mount grant.
use super::{RetainedHomePlan, invalid};
use crate::setup::{migration_handoff, staging::plan};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;

const REVIEW_BOUND: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedBtrfsResize {
    schema: u32,
    original_plan_sha256: [u8; 32],
    reviewed_plan_sha256: [u8; 32],
    reviewed_plan: plan::Plan,
    filesystem_uuid: String,
    partition_guid: String,
    device_id: u64,
    from_bytes: u64,
    to_bytes: u64,
    new_last_lba: u64,
}
impl PlannedBtrfsResize {
    pub fn from_review(
        original: &RetainedHomePlan,
        reviewed: &plan::Plan,
    ) -> io::Result<Option<Self>> {
        original.validate()?;
        original.as_plan().validate_for_install(reviewed)?;
        let Some(shrink) = &reviewed.shrink else {
            return Ok(None);
        };
        let volume = reviewed
            .basis
            .linux_btrfs_volume
            .as_ref()
            .ok_or_else(invalid)?;
        let source = &original.as_plan().home.volume;
        if !source
            .partition_guid
            .eq_ignore_ascii_case(&shrink.partition_guid)
        {
            if original.as_plan().for_completed_staging(reviewed)? != *original.as_plan() {
                // Embedded application roots also require an explicit resize
                // declaration; this narrower producer cannot omit their change.
                return Err(invalid());
            }
            return Ok(None);
        }
        let value = Self {
            schema: 1,
            original_plan_sha256: Sha256::digest(original.as_plan().bytes()?).into(),
            reviewed_plan_sha256: Sha256::digest(bounded_review(reviewed)?).into(),
            reviewed_plan: reviewed.clone(),
            filesystem_uuid: volume.filesystem_uuid.clone(),
            partition_guid: shrink.partition_guid.clone(),
            device_id: volume.device_id,
            from_bytes: shrink.from_bytes,
            to_bytes: shrink.to_bytes,
            new_last_lba: completed_last(source, shrink.to_bytes)?,
        };
        value.validate(original)?;
        Ok(Some(value))
    }
    pub(super) fn validate(&self, original: &RetainedHomePlan) -> io::Result<()> {
        original.validate()?;
        let source = &original.as_plan().home.volume;
        let volume = self
            .reviewed_plan
            .basis
            .linux_btrfs_volume
            .as_ref()
            .ok_or_else(invalid)?;
        let shrink = self.reviewed_plan.shrink.as_ref().ok_or_else(invalid)?;
        let original_size = source
            .last_lba
            .checked_sub(source.first_lba)
            .and_then(|n| n.checked_add(1))
            .and_then(|n| n.checked_mul(source.sector_bytes))
            .ok_or_else(invalid)?;
        if self.schema != 1
            || self.device_id != 1
            || self.device_id != volume.device_id
            || source.filesystem != migration_handoff::Filesystem::Btrfs
            || self.filesystem_uuid != volume.filesystem_uuid
            || !self
                .filesystem_uuid
                .eq_ignore_ascii_case(&source.filesystem_uuid)
            || self.partition_guid != shrink.partition_guid
            || !self
                .partition_guid
                .eq_ignore_ascii_case(&source.partition_guid)
            || self.from_bytes != shrink.from_bytes
            || self.from_bytes != original_size
            || self.to_bytes != shrink.to_bytes
            || self.to_bytes == 0
            || self.to_bytes >= self.from_bytes
            || !self
                .to_bytes
                .is_multiple_of(crate::setup::staging::ALIGN_BYTES)
            || self.new_last_lba != completed_last(source, self.to_bytes)?
            || self.original_plan_sha256
                != <[u8; 32]>::from(Sha256::digest(original.as_plan().bytes()?))
            || self.reviewed_plan_sha256
                != <[u8; 32]>::from(Sha256::digest(bounded_review(&self.reviewed_plan)?))
        {
            return Err(invalid());
        }
        validate_placement(&self.reviewed_plan)?;
        original
            .as_plan()
            .for_completed_staging(&self.reviewed_plan)?;
        Ok(())
    }
    pub fn effective_plan(&self, original: &RetainedHomePlan) -> io::Result<RetainedHomePlan> {
        self.validate(original)?;
        RetainedHomePlan::new(
            original
                .as_plan()
                .for_completed_staging(&self.reviewed_plan)?,
        )
    }
    /// Maps an authenticated original-copy selection through this exact reviewed
    /// staging plan. This is metadata; physical completion still needs a held RO source.
    pub fn effective_original_copy_plan(
        &self,
        shared: &RetainedHomePlan,
        original: &migration_handoff::Plan,
    ) -> io::Result<migration_handoff::Plan> {
        self.validate(shared)?;
        if original.home_policy != migration_handoff::HomePolicy::Copy {
            return Err(invalid());
        }
        if migration_handoff::OriginalInventoryBinding::parse(original)?.is_some() {
            // The protected sidecar is not an inline inventory. Keep its exact
            // marker unchanged; its real held roots are mapped separately by
            // the completed source runner after OriginalInventoryGrant review.
            let mut physical = original.clone();
            physical.flatpak_inventory = None;
            let mut effective = physical.for_completed_staging(&self.reviewed_plan)?;
            effective
                .flatpak_inventory
                .clone_from(&original.flatpak_inventory);
            effective.validate()?;
            Ok(effective)
        } else {
            original.for_completed_staging(&self.reviewed_plan)
        }
    }
    pub fn filesystem_bytes(&self) -> u64 {
        self.to_bytes
    }
}
fn completed_last(source: &migration_handoff::SourceVolume, bytes: u64) -> io::Result<u64> {
    source
        .first_lba
        .checked_add(bytes / source.sector_bytes)
        .and_then(|end| end.checked_sub(1))
        .ok_or_else(invalid)
}
fn bounded_review(reviewed: &plan::Plan) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(reviewed)?;
    if bytes.len() > REVIEW_BOUND {
        return Err(invalid());
    }
    Ok(bytes)
}
fn validate_placement(reviewed: &plan::Plan) -> io::Result<()> {
    let bytes = |extent: plan::Extent| {
        extent
            .last_lba
            .checked_sub(extent.first_lba)
            .and_then(|n| n.checked_add(1))
            .and_then(|n| n.checked_mul(reviewed.basis.disk.sector_bytes))
            .ok_or_else(invalid)
    };
    let computed = plan::compute(
        &reviewed.basis,
        plan::Requirements {
            staging_esp_bytes: bytes(reviewed.staging_esp.extent())?,
            staging_root_bytes: bytes(reviewed.staging_root.extent())?,
            target_bytes: bytes(reviewed.target_extent)?,
        },
    )
    .map_err(|_| invalid())?;
    if computed.target_extent != reviewed.target_extent
        || computed.staging_esp.extent() != reviewed.staging_esp.extent()
        || computed.staging_root.extent() != reviewed.staging_root.extent()
        || computed.shrink != reviewed.shrink
        || [
            reviewed.staging_esp.partition_guid.as_ref(),
            reviewed.staging_root.partition_guid.as_ref(),
        ]
        .iter()
        .any(|guid| guid.is_none_or(|value| !crate::setup::staging::is_guid(value)))
    {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selection() -> (RetainedHomePlan, plan::Plan) {
        let gib = 1 << 30;
        let mut basis = plan::tests::windows_basis(512, 400, 100, true);
        let windows = basis.system_volume.take().unwrap();
        let partition = basis
            .disk
            .partitions
            .iter_mut()
            .find(|p| p.guid == windows.partition_guid)
            .unwrap();
        partition.type_guid = "0fc63daf-8483-4772-8e79-3d69d8477de4".into();
        let fsid = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        basis.linux_btrfs_volume = Some(crate::setup::staging::platform::LinuxBtrfsVolume {
            partition_guid: windows.partition_guid.clone(),
            filesystem_uuid: fsid.into(),
            device_id: 1,
            mount_id: 37,
            first_lba: partition.first_lba,
            sector_bytes: 512,
            size_bytes: windows.size_bytes,
            filesystem_bytes: windows.size_bytes,
            kernel_partition_bytes: windows.size_bytes,
            supported_min_bytes: 250 * gib,
            reserve_bytes: 20 * gib,
        });
        let mut identity = vec![3];
        identity.extend_from_slice(uuid::Uuid::parse_str(fsid).unwrap().as_bytes());
        identity.extend_from_slice(&[0; 24]);
        let source = serde_json::from_value(serde_json::json!({
            "schema": 1, "operation_id": "retained-btrfs-test",
            "categories": { "home": true, "flatpak": false, "accounts": false },
            "home_policy": "access-read-write",
            "home": { "volume": { "disk_guid": basis.disk.guid,
                "disk_bytes": basis.disk.size_bytes, "sector_bytes": 512,
                "partition_guid": windows.partition_guid, "first_lba": partition.first_lba,
                "last_lba": partition.last_lba, "filesystem_uuid": fsid, "filesystem": "btrfs" },
                "relative": crate::setup::migration_store::NativePath::encode(std::path::Path::new("home/person")),
                "inode": 1234, "original_uid": 1000, "subvolume": { "volume": identity, "inode": 1234 }
            }
        })).unwrap();
        let reviewed = plan::compute(
            &basis,
            plan::Requirements {
                target_bytes: 64 * gib,
                staging_esp_bytes: 600 << 20,
                staging_root_bytes: 40 * gib,
            },
        )
        .unwrap();
        (RetainedHomePlan::new(source).unwrap(), reviewed)
    }
    #[test]
    fn effective_copy_mapping_preserves_requested_plan_and_rejects_unreviewed_extent() {
        let (shared, reviewed) = selection();
        let resize = PlannedBtrfsResize::from_review(&shared, &reviewed)
            .unwrap()
            .unwrap();
        let mut original = shared.as_plan().clone();
        original.home_policy = migration_handoff::HomePolicy::Copy;
        let bytes = original.bytes().unwrap();
        let physical = resize
            .effective_original_copy_plan(&shared, &original)
            .unwrap();
        assert_eq!(original.bytes().unwrap(), bytes);
        assert_eq!(physical.home.volume.last_lba, resize.new_last_lba);
        let mut unchanged_home = physical.home;
        unchanged_home.volume = original.home.volume.clone();
        assert_eq!(unchanged_home, original.home);
        assert!(
            resize
                .effective_original_copy_plan(&shared, shared.as_plan())
                .is_err()
        );
        let mut wrong = resize.clone();
        wrong.to_bytes -= 1 << 20;
        assert!(
            wrong
                .effective_original_copy_plan(&shared, &original)
                .is_err()
        );
        let mut wrong = resize;
        wrong.reviewed_plan.target_extent.first_lba += 1;
        wrong.reviewed_plan_sha256 =
            Sha256::digest(bounded_review(&wrong.reviewed_plan).unwrap()).into();
        assert!(
            wrong
                .effective_original_copy_plan(&shared, &original)
                .is_err()
        );
    }
    #[test]
    fn declaration_binds_original_and_reviewed_resize_without_mutating_original() {
        let (original, reviewed) = selection();
        let original_bytes = original.as_plan().bytes().unwrap();
        let resize = PlannedBtrfsResize::from_review(&original, &reviewed)
            .unwrap()
            .unwrap();
        let effective = resize.effective_plan(&original).unwrap();
        assert_eq!(
            effective.as_plan().home.volume.last_lba,
            resize.new_last_lba
        );
        assert_eq!(original.as_plan().bytes().unwrap(), original_bytes);
        let mut changed = resize.clone();
        changed.to_bytes -= 1 << 20;
        assert!(changed.effective_plan(&original).is_err());
        let mut changed = resize.clone();
        changed.reviewed_plan.target_extent.first_lba += 1;
        changed.reviewed_plan_sha256 =
            Sha256::digest(bounded_review(&changed.reviewed_plan).unwrap()).into();
        assert!(changed.effective_plan(&original).is_err());
        let mut changed_original = original.as_plan().clone();
        changed_original.operation_id = "different-retained-consent".into();
        let changed_original = RetainedHomePlan::new(changed_original).unwrap();
        assert!(resize.effective_plan(&changed_original).is_err());
        let decoded: PlannedBtrfsResize =
            serde_json::from_slice(&serde_json::to_vec(&resize).unwrap()).unwrap();
        assert_eq!(decoded.effective_plan(&original).unwrap(), effective);
    }
}
