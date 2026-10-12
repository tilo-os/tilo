//! The canonical compiled baseline, retained only through signed root projection.
use super::{refused, root};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};

const CHILD_FD: RawFd = crate::services::BASELINE_SECCOMP_CHILD_FD;
const MAX_BYTES: usize = 4096 * 8;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub sha256: [u8; 32],
    pub bytes: u32,
}
impl Binding {
    pub(super) fn validate(&self) -> io::Result<()> {
        if self.sha256 == [0; 32]
            || self.bytes == 0
            || self.bytes as usize > MAX_BYTES
            || !self.bytes.is_multiple_of(8)
        {
            return Err(refused("Original baseline binding shape differs"));
        }
        Ok(())
    }
}
pub(super) struct Filter {
    file: File,
    binding: Binding,
}
fn read_bytes(file: &File) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    let length = usize::try_from(metadata.len()).map_err(io::Error::other)?;
    if !metadata.is_file() || length == 0 || length > MAX_BYTES || !length.is_multiple_of(8) {
        return Err(refused("Original baseline filter shape differs"));
    }
    let mut bytes = vec![0; length];
    file.read_exact_at(&mut bytes, 0)?;
    if file.metadata()?.len() != metadata.len() {
        return Err(refused("Original baseline filter length changed"));
    }
    Ok(bytes)
}
fn binding(bytes: &[u8]) -> Binding {
    Binding {
        sha256: Sha256::digest(bytes).into(),
        bytes: bytes.len() as u32,
    }
}
fn verify(file: &File, expected: &Binding) -> io::Result<Vec<u8>> {
    expected.validate()?;
    let metadata = file.metadata()?;
    // SAFETY: fcntl inspects the held descriptor; no pointer argument is used.
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let name = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    use std::os::unix::ffi::OsStrExt;
    if metadata.uid() != 0
        || metadata.nlink() != 0
        || metadata.len() != u64::from(expected.bytes)
        || seals < 0
        || seals & SEALS != SEALS
        || !name.as_os_str().as_bytes().starts_with(b"/memfd:")
    {
        return Err(refused(
            "Original baseline filter is not sealed root custody",
        ));
    }
    let bytes = read_bytes(file)?;
    if binding(&bytes) != *expected {
        return Err(refused("Original baseline filter binding differs"));
    }
    Ok(bytes)
}
impl Filter {
    /// Called only by the root runner with its canonical compiler descriptor.
    pub(super) fn capture(source: &OwnedFd) -> io::Result<Self> {
        root()?;
        let source = File::from(source.try_clone()?);
        if source.metadata()?.uid() != 0 {
            return Err(refused("Original compiler filter is not root owned"));
        }
        let bytes = read_bytes(&source)?;
        let binding = binding(&bytes);
        // SAFETY: the literal name and fixed flags create a private sealable memfd.
        let raw = unsafe {
            libc::memfd_create(
                c"tilo-original-baseline".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a new uniquely owned descriptor.
        let mut file = unsafe { File::from_raw_fd(raw) };
        file.write_all(&bytes)?;
        // SAFETY: these fixed seals irreversibly freeze the held private memfd.
        if unsafe { libc::fcntl(raw, libc::F_ADD_SEALS, SEALS) } < 0 {
            return Err(io::Error::last_os_error());
        }
        verify(&file, &binding)?;
        Ok(Self { file, binding })
    }
    pub(super) fn binding(&self) -> Binding {
        self.binding.clone()
    }
    pub(super) fn descriptor(&self) -> RawFd {
        self.file.as_raw_fd()
    }
    /// Binding comes exclusively from the already authenticated sealed Bootstrap.
    pub(super) fn receive(binding: &Binding) -> io::Result<Self> {
        root()?;
        // SAFETY: duplicate exactly the reserved inherited baseline descriptor.
        let raw = unsafe { libc::fcntl(CHILD_FD, libc::F_DUPFD_CLOEXEC, 128) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fcntl returned a new uniquely owned descriptor.
        let file = unsafe { File::from_raw_fd(raw) };
        verify(&file, binding)?;
        Ok(Self {
            file,
            binding: binding.clone(),
        })
    }
    /// Consumes custody and reinstalls the exact full baseline before worker drop.
    pub(super) fn install(self) -> io::Result<()> {
        root()?;
        let bytes = verify(&self.file, &self.binding)?;
        sandbox_core::seccomp::install_bytes(&bytes).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_and_unsealed_filters_cannot_satisfy_sealed_baseline_binding() {
        let ordinary = tempfile::tempfile().unwrap();
        assert!(verify(&ordinary, &binding(&[0; 8])).is_err());
        // SAFETY: literal name and fixed flags create an owned test-only memfd.
        let raw =
            unsafe { libc::memfd_create(c"unsealed-baseline-test".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(raw >= 0);
        // SAFETY: the successful syscall transferred a new descriptor to this test.
        let mut unsealed = unsafe { File::from_raw_fd(raw) };
        unsealed.write_all(&[0; 8]).unwrap();
        assert!(verify(&unsealed, &binding(&[0; 8])).is_err());
        assert!(read_bytes(&ordinary).is_err());
        unsealed.set_len(9).unwrap();
        assert!(read_bytes(&unsealed).is_err());
    }
    #[test]
    #[cfg(feature = "managed-tree-test")]
    fn root_captured_baseline_is_exact_sealed_and_refuses_substitution() {
        // This opt-in native fixture requires real root; absence is a failure.
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let bytes = sandbox_core::seccomp::compile_baseline(
            sandbox_core::seccomp::SeccompProfile::TiloDefault,
        )
        .unwrap();
        let mut source = tempfile::tempfile().unwrap();
        source.write_all(&bytes).unwrap();
        let source: OwnedFd = source.into();
        let held = Filter::capture(&source).unwrap();
        assert_eq!(verify(&held.file, &held.binding()).unwrap(), bytes);
        let mut changed = held.binding();
        changed.sha256[0] ^= 1;
        assert!(verify(&held.file, &changed).is_err());
        changed = held.binding();
        changed.bytes += 8;
        assert!(verify(&held.file, &changed).is_err());
        assert_eq!(
            unsafe { libc::fcntl(held.descriptor(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            libc::FD_CLOEXEC
        );
        assert!(held.file.set_len(8).is_err());
    }
}
