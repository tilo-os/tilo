//! Verified installer GPT extension, on the already-authorized target descriptor.
//! Boot payloads stay untouched; profile storage precedes the relocated backup.
use std::{fs::File, io, os::fd::AsRawFd};
pub mod gpt;
pub const DATA_PARTITION_TYPE: &str = "7c4f89e9-0bd4-45ac-8ea6-319ab6795a91";
pub const DATA_PARTITION_NAME: &str = "Tilo migration";
const SECTOR: u64 = 512;
pub use crate::media_model::MediaPartitionBinding;
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
pub struct PreparedLayout {
    layout: gpt::Layout,
    target_bytes: u64,
    stable_id: String,
    // The caller supplies its package framing/reservation policy; native GPT
    // geometry and ext4 free-space accounting remain owned by this actor.
    capacity_for_free_bytes: fn(u64) -> io::Result<u64>,
}
/// # Errors
/// Rejects changed target geometry, missing stable identity, invalid image GPT, or insufficient tail space.
pub fn prepare(
    image: &File,
    target: &File,
    image_bytes: u64,
    target_bytes: u64,
    stable_id: &str,
    capacity_for_free_bytes: fn(u64) -> io::Result<u64>,
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
        capacity_for_free_bytes,
    })
}
pub fn prepare_expected(
    image: &File,
    target: &File,
    image_bytes: u64,
    target_bytes: u64,
    expected: &MediaPartitionBinding,
    reservation: u64,
    capacity_for_free_bytes: fn(u64) -> io::Result<u64>,
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
        capacity_for_free_bytes,
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
    pub fn planned_binding(&self) -> io::Result<MediaPartitionBinding> {
        self.layout
            .planned_binding(self.target_bytes, &self.stable_id)
    }
    pub fn payload_capacity(&self) -> io::Result<u64> {
        let binding = self.planned_binding()?;
        let available = binding
            .last_lba
            .checked_sub(binding.first_lba)
            .and_then(|blocks| blocks.checked_add(1))
            .and_then(|blocks| blocks.checked_mul(binding.sector_bytes))
            .ok_or_else(|| invalid("Migration capacity overflows"))?;
        let free = block_devices::migration_ext4::available_bytes(available)?;
        (self.capacity_for_free_bytes)(free)
    }
    /// # Errors
    /// Rejects an empty or oversized reservation, or unavailable formatted payload capacity.
    pub fn require_capacity(&self, bytes: u64) -> io::Result<()> {
        if bytes == 0 || bytes > self.payload_capacity()? {
            return Err(invalid(
                "Reviewed migration reservation exceeds planned partition",
            ));
        }
        Ok(())
    }
    /// # Errors
    /// Rejects changed target geometry or GPT contents, cancellation, write failures, or inconsistent read-back.
    pub fn commit(
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
        #[cfg(any(test, feature = "test-support"))]
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
