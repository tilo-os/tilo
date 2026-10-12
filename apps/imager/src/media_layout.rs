//! Verified installer GPT extension, on the already-authorized target descriptor.
//! Boot payloads stay untouched; profile storage precedes the relocated backup.
use std::{fs::File, io, os::fd::AsRawFd};
mod gpt;
#[cfg(all(test, feature = "native-capture-qualification"))]
mod native_capacity_tests;
#[cfg(test)]
pub(crate) mod tests;
pub const DATA_PARTITION_TYPE: &str = "7c4f89e9-0bd4-45ac-8ea6-319ab6795a91";
pub const DATA_PARTITION_NAME: &str = "Tilo migration";
const SECTOR: u64 = 512;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaPartitionBinding {
    pub schema: u32,
    pub target_stable_id: String,
    pub disk_guid: String,
    pub partition_guid: String,
    pub number: u32,
    pub first_lba: u64,
    pub last_lba: u64,
    pub sector_bytes: u64,
    pub profile_offset: u64,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
pub(crate) struct PreparedLayout {
    layout: gpt::Layout,
    target_bytes: u64,
    stable_id: String,
}
pub(crate) fn prepare(
    image: &File,
    target: &File,
    image_bytes: u64,
    target_bytes: u64,
    stable_id: &str,
) -> io::Result<PreparedLayout> {
    let (bytes, sector) = geometry(target)?;
    if bytes != target_bytes || sector != SECTOR || stable_id.is_empty() {
        return Err(invalid(
            "Migration media requires stable custody and native 512-byte geometry",
        ));
    }
    Ok(PreparedLayout {
        layout: gpt::Layout::plan(image, image_bytes, target_bytes, stable_id)?,
        target_bytes,
        stable_id: stable_id.into(),
    })
}
pub(crate) fn prepare_expected(
    image: &File,
    target: &File,
    image_bytes: u64,
    target_bytes: u64,
    expected: &MediaPartitionBinding,
    reservation: u64,
) -> io::Result<PreparedLayout> {
    let (bytes, sector) = geometry(target)?;
    if bytes != target_bytes || sector != SECTOR || expected.target_stable_id.is_empty() {
        return Err(invalid("Migration media geometry or custody changed"));
    }
    let guid = uuid::Uuid::parse_str(&expected.partition_guid).map_err(io::Error::other)?;
    let prepared = PreparedLayout {
        layout: gpt::Layout::plan_with_guid(
            image,
            image_bytes,
            target_bytes,
            &expected.target_stable_id,
            guid,
        )?,
        target_bytes,
        stable_id: expected.target_stable_id.clone(),
    };
    if &prepared.planned_binding()? != expected {
        return Err(invalid(
            "Prepared migration layout differs from canonical writer plan",
        ));
    }
    prepared.require_capacity(reservation)?;
    Ok(prepared)
}
impl PreparedLayout {
    pub(crate) fn planned_binding(&self) -> io::Result<MediaPartitionBinding> {
        self.layout
            .planned_binding(self.target_bytes, &self.stable_id)
    }
    pub(crate) fn payload_capacity(&self) -> io::Result<u64> {
        let binding = self.planned_binding()?;
        let available = binding
            .last_lba
            .checked_sub(binding.first_lba)
            .and_then(|blocks| blocks.checked_add(1))
            .and_then(|blocks| blocks.checked_mul(binding.sector_bytes))
            .ok_or_else(|| invalid("Migration capacity overflows"))?;
        let free = block_devices::migration_ext4::available_bytes(available)?;
        crate::setup::portable_home_media::payload_capacity_for_free_bytes(free)
    }
    pub(crate) fn require_capacity(&self, bytes: u64) -> io::Result<()> {
        if bytes == 0 || bytes > self.payload_capacity()? {
            return Err(invalid(
                "Reviewed migration reservation exceeds planned partition",
            ));
        }
        Ok(())
    }
    pub(crate) fn commit(
        self,
        target: &File,
        cancelled: &impl Fn() -> bool,
    ) -> io::Result<MediaPartitionBinding> {
        let (bytes, sector) = geometry(target)?;
        if bytes != self.target_bytes || sector != SECTOR {
            return Err(invalid("Held media geometry changed"));
        }
        self.layout.commit(target, cancelled)?;
        binding(target, self.target_bytes, &self.stable_id)
    }
}
/// Extend only a verified image's declared GPT into unused tail capacity.
///
/// # Errors
/// Refuses incompatible geometry, inconsistent GPT copies, missing custody,
/// insufficient tail capacity, cancellation, or unverified read-back.
pub fn provision(
    file: &File,
    image_bytes: u64,
    target_bytes: u64,
    stable_id: &str,
    cancelled: &impl Fn() -> bool,
) -> io::Result<MediaPartitionBinding> {
    let (bytes, sector) = geometry(file)?;
    if bytes != target_bytes || sector != SECTOR || stable_id.is_empty() {
        return Err(invalid(
            "Migration media requires stable custody and native 512-byte geometry",
        ));
    }
    if cancelled() {
        return Err(io::Error::from(io::ErrorKind::Interrupted));
    }
    let layout = gpt::Layout::plan(file, image_bytes, target_bytes, stable_id)?;
    layout.commit(file, cancelled)?;
    gpt::binding(file, target_bytes, stable_id)
}
/// Recover only a fully consistent provisioned GPT, independently read from the fd.
///
/// # Errors
/// Refuses missing markers, inconsistent copies, geometry or invalid profile gap.
pub fn binding(
    file: &File,
    target_bytes: u64,
    stable_id: &str,
) -> io::Result<MediaPartitionBinding> {
    let (bytes, sector) = geometry(file)?;
    if bytes != target_bytes || sector != SECTOR || stable_id.is_empty() {
        return Err(invalid("Migration media geometry or custody changed"));
    }
    gpt::binding(file, target_bytes, stable_id)
}
fn geometry(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::FileTypeExt;
    if !file.metadata()?.file_type().is_block_device() {
        #[cfg(test)]
        if file.metadata()?.is_file() {
            return Ok((file.metadata()?.len(), SECTOR));
        }
        return Err(invalid(
            "Migration layout requires the held authorized block descriptor",
        ));
    }
    let mut bytes = 0u64;
    let mut sector = 0u32;
    // SAFETY: live block fd and correct writable BLKGETSIZE64/BLKSSZGET buffers.
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x8008_1272, &raw mut bytes) } != 0
        || unsafe { libc::ioctl(file.as_raw_fd(), 0x1268, &raw mut sector) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((bytes, u64::from(sector)))
}
