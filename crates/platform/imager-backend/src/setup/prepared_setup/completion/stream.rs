//! Only the fixed native-issued installer selection may reacquire payload custody.
use super::*;
use super::{
    boot_review::review_declared_source,
    handoff::{Member, Payload, RunCustody, open_payload, snapshot},
};
use crate::setup::{
    media_setup::{boot_medium::with_boot_medium, protected_output::PendingArea},
    migration::{changed, check_cancel},
};
use std::{
    fs::File,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
};
use tilo_files::native::atomic::{Privacy, StorageDirectory};
pub fn run(kind: &str, output: &mut impl Write, cancel: &AtomicBool) -> io::Result<()> {
    let custody = RunCustody::open()?.ok_or_else(invalid)?;
    let member = custody
        .record
        .members
        .iter()
        .find(|m| m.payload.kind() == kind)
        .ok_or_else(invalid)?;
    let mut targets = crate::probe::probe()
        .ok_or_else(invalid)?
        .into_iter()
        .filter(|t| {
            t.stable_id.as_deref()
                == Some(custody.record.selection.medium.target_stable_id.as_str())
        });
    let target = targets.next().ok_or_else(invalid)?;
    if targets.next().is_some() {
        return Err(invalid());
    }
    // The only device pathname comes from a fresh canonical probe whose exact
    // selected descriptor is independently re-attested by BootPending policy.
    let raw = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(target.dev_path())?;
    crate::setup::read_media::attest_boot_descriptor(&target, &raw)?;
    let result = with_boot_medium(&target, &raw, cancel, |medium| {
        medium.with_root(|root, binding| {
            let (selected, home, _, _lease) = review_declared_source(
                root,
                binding,
                &custody.record.declaration()?,
                &custody.record.selection.pending_setup,
                cancel,
            )?;
            if selected != custody.record.selection {
                return Err(changed("Completed selected medium changed"));
            }
            match &member.payload {
                Payload::Home(_) | Payload::Applications(_) => {
                    stream(&home.file, member, output, cancel)?
                }
                Payload::Browser(_) => {
                    with_inbox(root, binding, &selected.pending_setup, |inbox| {
                        stream(inbox, member, output, cancel)
                    })?
                }
            }
            let (after, _, _, _after_lease) = review_declared_source(
                root,
                binding,
                &custody.record.declaration()?,
                &custody.record.selection.pending_setup,
                cancel,
            )?;
            if after != selected {
                return Err(invalid());
            }
            Ok(())
        })
    });
    // with_boot_medium returned only after the actual owned mount retired.
    result?;
    custody.validate()?;
    crate::setup::read_media::attest_boot_descriptor(&target, &raw)?;
    check_cancel(cancel)?;
    output.flush()
}
fn with_inbox<T>(
    root: &Arc<PinnedDirectory>,
    binding: &crate::media_layout::MediaPartitionBinding,
    pending: &crate::imager_model::PreparedSetupExpectation,
    callback: impl FnOnce(&File) -> io::Result<T>,
) -> io::Result<T> {
    let private = super::boot_review::child(root, crate::setup::media_setup::DIRECTORY)?;
    let storage =
        StorageDirectory::from_file(migration_durability::readable_directory(&private.file)?)?;
    let mut receipt = storage.open_read(
        std::ffi::OsStr::new("original-pending-area.json"),
        Privacy::OwnerOnly,
    )?;
    let before = cap_primitives::fs::Metadata::from_file(&receipt)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut receipt)
        .take(4097)
        .read_to_end(&mut bytes)?;
    let area = PendingArea::parse(&bytes)?;
    if &area.medium != binding || &area.setup != pending {
        return Err(invalid());
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK);
    let inbox = cap_primitives::fs::open(&root.file, Path::new(&area.name), &options)?;
    let initial = cap_primitives::fs::Metadata::from_file(&inbox)?;
    let m = inbox.metadata()?;
    if m.uid() != area.original_uid
        || m.gid() != 0
        || m.mode() & 0o7777 != 0o700
        || m.ino() != area.inode
        || m.dev() != root.file.metadata()?.dev()
    {
        return Err(invalid());
    }
    let result = callback(&inbox);
    if !crate::setup::migration::same_snapshot(
        &initial,
        &cap_primitives::fs::Metadata::from_file(&inbox)?,
    ) || !crate::setup::migration::same_snapshot(
        &initial,
        &crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(&area.name))?,
    ) || !crate::setup::migration::same_snapshot(
        &before,
        &cap_primitives::fs::Metadata::from_file(&receipt)?,
    ) {
        return Err(invalid());
    }
    private.validate_destination()?;
    root.validate_destination()?;
    result
}
pub(super) fn stream(
    parent: &File,
    member: &Member,
    output: &mut impl Write,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut file = open_payload(parent, &member.payload)?;
    if snapshot(&file)? != member.snapshot {
        return Err(changed("Completed ciphertext source identity changed"));
    }
    let before = cap_primitives::fs::Metadata::from_file(&file)?;
    let mut hash = Sha256::new();
    let mut n = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        n = n
            .checked_add(count as u64)
            .filter(|n| *n <= member.payload.length())
            .ok_or_else(invalid)?;
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if n != member.payload.length()
        || <[u8; 32]>::from(hash.finalize()) != member.payload.digest()
        || snapshot(&file)? != member.snapshot
        || !crate::setup::migration::same_snapshot(
            &before,
            &crate::setup::nofollow::metadata_nofollow(
                parent,
                Path::new(member.payload.filename()),
            )?,
        )
    {
        return Err(invalid());
    }
    check_cancel(cancel)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> (tempfile::TempDir, File, Member) {
        let directory = tempfile::tempdir().unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let filename = format!("home-{operation}.sealed");
        let bytes = b"TILOMIG1 synthetic bounded ciphertext";
        let path = directory.path().join(&filename);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let payload = Payload::Home(crate::setup::portable_home_media::Expectation {
            schema: 1,
            operation,
            purpose: crate::setup::portable_transfer::Purpose::Home,
            filename,
            setup_binding: "synthetic-bounded-source".into(),
            sealed_bytes: bytes.len() as u64,
            plaintext_bytes: 16,
            entries: 1,
            runtime_sockets: 0,
            ciphertext_sha256: Sha256::digest(bytes).into(),
            limits: crate::setup::portable_transfer::Limits {
                plaintext_bytes: 1024,
                entries: 4,
                metadata_bytes: 1024,
            },
        });
        let file = File::open(path).unwrap();
        let member = Member {
            payload,
            snapshot: snapshot(&file).unwrap(),
        };
        let parent = File::open(directory.path()).unwrap();
        (directory, parent, member)
    }
    #[test]
    fn completed_payload_stream_checks_real_file_digest_identity_and_cancellation() {
        let (_dir, parent, mut member) = source();
        let mut bytes = Vec::new();
        stream(&parent, &member, &mut bytes, &AtomicBool::new(false)).unwrap();
        assert_eq!(bytes, b"TILOMIG1 synthetic bounded ciphertext");
        if let Payload::Home(e) = &mut member.payload {
            e.ciphertext_sha256[0] ^= 1;
        }
        assert!(stream(&parent, &member, &mut Vec::new(), &AtomicBool::new(false)).is_err());
        assert_eq!(
            stream(&parent, &member, &mut Vec::new(), &AtomicBool::new(true))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        member.snapshot[1] = member.snapshot[1].checked_add(1).unwrap();
        let mut output = Vec::new();
        assert!(stream(&parent, &member, &mut output, &AtomicBool::new(false)).is_err());
        assert!(output.is_empty());
    }
    struct ReplaceOnWrite {
        path: PathBuf,
        replaced: bool,
    }
    impl Write for ReplaceOnWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.replaced {
                let replacement = self.path.with_extension("replacement");
                std::fs::write(&replacement, bytes)?;
                std::fs::rename(replacement, &self.path)?;
                self.replaced = true;
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn completed_payload_stream_refuses_named_source_replacement_even_when_original_fd_bytes_match()
    {
        let (directory, parent, member) = source();
        let mut writer = ReplaceOnWrite {
            path: directory.path().join(member.payload.filename()),
            replaced: false,
        };
        assert!(stream(&parent, &member, &mut writer, &AtomicBool::new(false)).is_err());
        assert!(writer.replaced);
    }
}
