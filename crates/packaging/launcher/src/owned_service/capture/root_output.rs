//! Root-host original-source output ownership. Never a live non-root broker proof.
use super::{
    contains, directory_identity as identity, writable_capacity as actual_capacity,
    writable_directory,
};
use crate::owned_service::ServiceAuthority;
use std::{
    fs::File,
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt, FileTypeExt, MetadataExt},
    },
    sync::Arc,
};

/// Immutable host facts, copied only from authenticated pending setup/ownership.
#[derive(Clone, PartialEq, Eq)]
pub struct OriginalOutputBinding {
    pub read_claim_digest: [u8; 32],
    pub medium_claim_digest: [u8; 32],
    pub operation: String,
    pub attempt: String,
    pub source: (u64, u64),
    pub output: (u64, u64),
    pub original_uid: u32,
    pub disk_rdev: u64,
    pub partition_rdev: u64,
    pub mount_id: u64,
    pub mount_root: (u64, u64),
    pub filesystem_uuid: [u8; 16],
    pub reserve_bytes: u64,
}
/// Strong native ownership, never inherited into the worker or serialized.
pub struct OriginalOutputFiles {
    pub source: File,
    pub output: File,
    pub disk: File,
    pub partition: File,
    pub mount_root: File,
}
/// Trusted native root-controller boundary.
pub trait OriginalOutputOwnership: ServiceAuthority {
    /// Clone the controller's held source and output descriptors.
    fn held_files(&self) -> io::Result<OriginalOutputFiles>;
    /// Return the authenticated output transaction binding.
    fn binding(&self) -> io::Result<OriginalOutputBinding>;
}
pub struct OriginalOutputAdmission {
    ownership: Arc<dyn OriginalOutputOwnership>,
    files: OriginalOutputFiles,
    binding: OriginalOutputBinding,
}
fn refuse(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn mount_id(file: &File) -> io::Result<u64> {
    crate::fd::mount_id(
        file,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        "Original output mount identity is unavailable",
    )
}
fn ext4_uuid(partition: &File) -> io::Result<[u8; 16]> {
    let mut superblock = [0u8; 120];
    // Exact held partition, never a filename or discovered UUID device alias.
    partition.read_exact_at(&mut superblock, 1024)?;
    if superblock[56..58] != [0x53, 0xef] {
        return Err(refuse("Original output partition is not ext4"));
    }
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&superblock[104..120]);
    if uuid == [0; 16] {
        return Err(refuse("Original output filesystem has no identity"));
    }
    Ok(uuid)
}
fn validate_files(files: &OriginalOutputFiles, binding: &OriginalOutputBinding) -> io::Result<()> {
    let disk = files.disk.metadata()?;
    let partition = files.partition.metadata()?;
    if !disk.file_type().is_block_device()
        || !partition.file_type().is_block_device()
        || disk.rdev() != binding.disk_rdev
        || partition.rdev() != binding.partition_rdev
        || disk.rdev() == partition.rdev()
    {
        return Err(refuse("Original output block ownership differs"));
    }
    let root = files.mount_root.metadata()?;
    if identity(&files.source)? != binding.source
        || identity(&files.output)? != binding.output
        || identity(&files.mount_root)? != binding.mount_root
        || root.ino() != 2
        || root.dev() != partition.rdev()
        || files.output.metadata()?.dev() != partition.rdev()
        || files.output.metadata()?.uid() != binding.original_uid
        || files.output.metadata()?.mode() & 0o7777 != 0o700
        || mount_id(&files.mount_root)? != binding.mount_id
        || mount_id(&files.output)? != binding.mount_id
    {
        return Err(refuse("Original output filesystem projection differs"));
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: live directory and valid output buffer.
    if unsafe { libc::fstatfs(files.mount_root.as_raw_fd(), filesystem.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatfs initialized the buffer.
    if unsafe { filesystem.assume_init() }.f_type != 0xef53
        || ext4_uuid(&files.partition)? != binding.filesystem_uuid
    {
        return Err(refuse("Original output mounted filesystem differs"));
    }
    writable_directory(&files.output)?;
    if !contains(&files.mount_root, &files.output)?
        || contains(&files.source, &files.output)?
        || contains(&files.output, &files.source)?
    {
        return Err(refuse(
            "Original output overlaps its source or escapes medium",
        ));
    }
    // Free bytes are rechecked at admission.
    Ok(())
}
impl OriginalOutputAdmission {
    /// Admit output held by the authenticated root transaction controller.
    pub fn admit(ownership: Arc<dyn OriginalOutputOwnership>) -> io::Result<Self> {
        // SAFETY: observes current real/effective/filesystem UIDs, no change.
        if rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || unsafe { libc::setfsuid(u32::MAX) } != 0
        {
            return Err(refuse("Original output requires native root ownership"));
        }
        ownership.check()?;
        let binding = ownership.binding()?;
        if binding.read_claim_digest == [0; 32]
            || binding.medium_claim_digest == [0; 32]
            || binding.original_uid == 0
            || binding.reserve_bytes == 0
            || uuid::Uuid::parse_str(&binding.operation)
                .ok()
                .map(|id| id.to_string())
                .as_deref()
                != Some(binding.operation.as_str())
            || uuid::Uuid::parse_str(&binding.attempt)
                .ok()
                .map(|id| id.to_string())
                .as_deref()
                != Some(binding.attempt.as_str())
        {
            return Err(refuse("Original output authenticated binding is invalid"));
        }
        let files = ownership.held_files()?;
        validate_files(&files, &binding)?;
        if binding.reserve_bytes > actual_capacity(&files.output)? {
            return Err(refuse(
                "Original output reservation exceeds actual free bytes",
            ));
        }
        let admission = Self {
            ownership,
            files,
            binding,
        };
        admission.check()?;
        Ok(admission)
    }
    pub(crate) fn check(&self) -> io::Result<()> {
        self.ownership.check()?;
        if self.ownership.binding()? != self.binding {
            return Err(refuse("Original output transaction retired or changed"));
        }
        validate_files(&self.files, &self.binding)
    }
    pub(super) fn bind_source(
        &self,
        source: &File,
        uid: u32,
        read_claim: [u8; 32],
    ) -> io::Result<()> {
        self.check()?;
        if read_claim != self.binding.read_claim_digest
            || identity(source)? != self.binding.source
            || uid != self.binding.original_uid
        {
            return Err(refuse(
                "Original output belongs to another source transaction",
            ));
        }
        Ok(())
    }
    pub(super) fn output(&self) -> io::Result<File> {
        self.check()?;
        self.files.output.try_clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::os::unix::fs::OpenOptionsExt;

    tilo_test_env::scenario! {
        fn original_output_admission_needs_held_native_ownership;

        phase original_output_real_held_ancestry_rejects_overlap_and_alias;
        // Exercises the same descriptor ancestry boundary used by admission; no fake setup/GPT or
        // successful output authority is manufactured.
        {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            let nested = source.join("output");
            fs::create_dir_all(&nested).unwrap();
            let open = |path: &std::path::Path| {
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)
                    .unwrap()
            };
            let source = open(&source);
            let output = open(&nested);
            assert!(contains(&source, &output).unwrap());
            assert!(!contains(&output, &source).unwrap());
            assert!(contains(&output, &output.try_clone().unwrap()).unwrap());
            // Holding the descriptor keeps the inode, rather than switching to an application-selected
            // path replacement during rename.
            let held = identity(&output).unwrap();
            fs::rename(&nested, root.path().join("held")).unwrap();
            fs::create_dir(&nested).unwrap();
            assert_eq!(identity(&output).unwrap(), held);
            assert_ne!(identity(&open(&nested)).unwrap(), held);
            assert!(!contains(&source, &output).unwrap());
        }

        phase original_output_retired_native_ownership_never_requests_files_or_claim;
        {
            struct Retired(std::sync::atomic::AtomicUsize);
            impl ServiceAuthority for Retired {
                fn check(&self) -> io::Result<()> {
                    Err(refuse("Original transaction retired"))
                }
            }
            impl OriginalOutputOwnership for Retired {
                fn held_files(&self) -> io::Result<OriginalOutputFiles> {
                    self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err(refuse(
                        "Refused authority must not request output descriptors",
                    ))
                }
                fn binding(&self) -> io::Result<OriginalOutputBinding> {
                    self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err(refuse(
                        "Refused authority must not request a transaction binding",
                    ))
                }
            }
            let ownership = Arc::new(Retired(std::sync::atomic::AtomicUsize::new(0)));
            let result = OriginalOutputAdmission::admit(ownership.clone());
            assert_eq!(ownership.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            let Err(error) = result else {
                panic!("Retired authority admitted");
            };
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            // The admission refuses anyone but root before it asks the
            // authority anything, and this runs as the account it names.
            tilo_test_env::require_capability!(
                NonRoot,
                tilo_test_env::non_root_identity(),
                "the refusal before the authority is only reached by an account that is not root"
            );
            assert_eq!(error.to_string(), "Original output requires native root ownership");
        }

        phase original_output_regular_file_cannot_stand_in_for_partition;
        {
            let root = tempfile::tempdir().unwrap();
            let partition_path = root.path().join("partition");
            let mut bytes = [0u8; 4096];
            bytes[1024 + 56..1024 + 58].copy_from_slice(&[0x53, 0xef]);
            bytes[1024 + 104..1024 + 120].copy_from_slice(&[7; 16]);
            fs::write(&partition_path, bytes).unwrap();
            let directory = File::open(root.path()).unwrap();
            let facts = directory.metadata().unwrap();
            let partition = File::open(&partition_path).unwrap();
            // A convincing ext4 header does NOT turn a regular file into the held kernel partition
            // accepted by the actual admission validator.
            assert_eq!(ext4_uuid(&partition).unwrap(), [7; 16]);
            let files = OriginalOutputFiles {
                source: directory.try_clone().unwrap(),
                output: directory.try_clone().unwrap(),
                mount_root: directory,
                disk: partition.try_clone().unwrap(),
                partition,
            };
            let binding = OriginalOutputBinding {
                read_claim_digest: [1; 32],
                medium_claim_digest: [2; 32],
                operation: uuid::Uuid::new_v4().to_string(),
                attempt: uuid::Uuid::new_v4().to_string(),
                source: (facts.dev(), facts.ino()),
                output: (facts.dev(), facts.ino()),
                original_uid: facts.uid(),
                disk_rdev: 1,
                partition_rdev: 2,
                mount_id: 1,
                mount_root: (facts.dev(), facts.ino()),
                filesystem_uuid: [7; 16],
                reserve_bytes: 1,
            };
            let error = validate_files(&files, &binding).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), "Original output block ownership differs");
        }
    }
}
