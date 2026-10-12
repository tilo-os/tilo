//! Explicit offline developer qualification; never a production download route.
use crate::catalog::CatalogPayload;
use crate::{HttpsTransport, ServiceError};
use agent_provider_config::local::PinnedArtifact;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const OFFLINE_PREFIX: &str = "offline:sha256:";

/// Check a declared offline identity, including equality to its signed receipt.
/// # Errors
/// Rejects unknown schemes, malformed digests, zero sizes or mismatched identity.
pub fn validate_offline_pin(artifact: &PinnedArtifact) -> Result<(), ServiceError> {
    crate::validate_sha256(&artifact.sha256)?;
    if artifact.size_bytes == 0
        || artifact.url.strip_prefix(OFFLINE_PREFIX) != Some(artifact.sha256.as_str())
    {
        return Err(ServiceError::InvalidArtifact);
    }
    Ok(())
}

/// A finite map obtained from a verified developer catalogue. No network
/// fallback, arbitrary filenames or caller-chosen destinations are supported.
pub struct OfflineTransport {
    root: PathBuf,
    pins: BTreeMap<String, PinnedArtifact>,
}

impl OfflineTransport {
    /// Construct only after verifying the developer catalogue signature.
    /// # Errors
    /// Refuses conflicting locations or invalid declared receipt identities.
    pub fn new(root: &Path, catalog: &CatalogPayload) -> Result<Self, ServiceError> {
        let mut pins = BTreeMap::new();
        for pin in catalog.models.iter().map(|model| &model.artifact).chain(
            catalog
                .runtimes
                .iter()
                .map(|runtime| &runtime.release.artifact),
        ) {
            if pin.url.starts_with("https://") {
                crate::validate_artifact(pin)?;
            } else {
                validate_offline_pin(pin)?;
            }
            if pins
                .insert(pin.url.clone(), pin.clone())
                .is_some_and(|old| old != *pin)
            {
                return Err(ServiceError::InvalidArtifact);
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            pins,
        })
    }
}

impl HttpsTransport for OfflineTransport {
    fn validate_pin(&self, artifact: &PinnedArtifact) -> Result<(), ServiceError> {
        if self.pins.get(&artifact.url) != Some(artifact) {
            return Err(ServiceError::InvalidArtifact);
        }
        if artifact.url.starts_with("https://") {
            crate::validate_artifact(artifact)
        } else {
            validate_offline_pin(artifact)
        }
    }

    fn resume(
        &mut self,
        url: &str,
        start: u64,
        maximum_bytes: u64,
        progress: &mut dyn FnMut(u64),
        destination: &mut File,
    ) -> Result<u64, ServiceError> {
        let pin = self.pins.get(url).ok_or(ServiceError::InvalidArtifact)?;
        let remaining = pin
            .size_bytes
            .checked_sub(start)
            .ok_or(ServiceError::PartialTooLarge)?;
        if maximum_bytes != remaining {
            return Err(ServiceError::InvalidArtifact);
        }
        let mut source = open_without_symlinks(&self.root.join(&pin.sha256))?;
        let metadata = source.metadata()?;
        if !metadata.is_file() || metadata.len() != pin.size_bytes {
            return Err(ServiceError::InvalidArtifact);
        }
        source.seek(SeekFrom::Start(start))?;
        let mut count = 0_u64;
        let mut buffer = vec![0_u8; 32 * 1024];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            count = count
                .checked_add(u64::try_from(read).map_err(|_| ServiceError::PartialTooLarge)?)
                .ok_or(ServiceError::PartialTooLarge)?;
            if count > maximum_bytes {
                return Err(ServiceError::PartialTooLarge);
            }
            destination.write_all(buffer.get(..read).ok_or(ServiceError::PartialTooLarge)?)?;
            progress(count);
        }
        Ok(count)
    }
}

/// Open a real regular-file candidate without following any path symlink.
/// # Errors
/// Returns the OS error on unsupported secure opening or unsafe path components.
#[cfg(target_os = "linux")]
pub fn open_without_symlinks(path: &Path) -> std::io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("path contains a NUL byte"))?;
    let how = OpenHow {
        flags: u64::try_from(libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .map_err(|_| std::io::Error::other("invalid open flags"))?,
        mode: 0,
        resolve: 0x02 | 0x04, // RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS
    };
    // SAFETY: live C path and exact Linux open_how ABI; successful fd is owned.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            path.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = i32::try_from(fd).map_err(|_| std::io::Error::other("invalid open descriptor"))?;
    // SAFETY: the successful openat2 descriptor has exactly one owner.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(not(target_os = "linux"))]
pub fn open_without_symlinks(_: &Path) -> std::io::Result<File> {
    Err(std::io::Error::other(
        "offline developer import requires Linux openat2",
    ))
}
