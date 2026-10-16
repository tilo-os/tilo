//! Private cooperative cache. Linux check-then-unlink/rename cannot confine
//! arbitrary hostile same-UID or privileged mutation. No support activation.
fn raw_components(path: &[u8]) -> Result<Vec<&[u8]>, String> {
    if path.len() > 4096 || path.first() != Some(&b'/') || path.contains(&0) {
        return Err("Unsupported absolute cache path".into());
    }
    let mut parts = Vec::new();
    for part in path[1..].split(|b| *b == b'/') {
        if parts.len() >= 64 || part.is_empty() || part.len() > 255 || part == b"." || part == b".."
        {
            return Err("Unsafe or excessive raw cache ancestry".into());
        }
        parts.push(part);
    }
    Ok(parts)
}

fn effective_mode_allowed(
    owner: u32,
    current: u32,
    mode: u32,
    directory: bool,
    private: bool,
) -> bool {
    mode & 0o7000 == 0
        && if private {
            owner == current && mode & 0o777 == if directory { 0o700 } else { 0o600 }
        } else {
            (owner == 0 || owner == current) && mode & 0o022 == 0
        }
}

#[cfg(target_os = "linux")]
pub(in crate::setup) use native::*;
#[cfg(target_os = "linux")]
mod native {
    use super::super::{
        FreshProvider, MAX_COLLISIONS, MAX_RECORD, MAX_SCAN, Ownership, PreparationComplete,
        Provider, checkpoint::Record, initialize_fresh, working_leaf,
    };
    use super::{effective_mode_allowed, raw_components};
    use crate::setup::internal_download::SelectedManifest;
    use advisory_lock::FileLock;
    use sha2::{Digest, Sha256};
    use std::{
        ffi::{CStr, CString},
        fs::{File, Metadata},
        io::{Read, Seek, SeekFrom, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    type Result<T> = std::result::Result<T, String>;
    fn io(e: std::io::Error) -> String {
        format!("Linux private cache: {e}")
    }
    fn require(ok: bool, why: &str) -> Result<()> {
        if ok { Ok(()) } else { Err(why.into()) }
    }
    fn stop(cancel: &AtomicBool) -> Result<()> {
        require(!cancel.load(Ordering::Acquire), "Cache operation cancelled")
    }
    fn name(bytes: &[u8]) -> Result<CString> {
        require(
            !bytes.is_empty()
                && bytes.len() <= 255
                && !bytes.contains(&b'/')
                && bytes != b"."
                && bytes != b"..",
            "Unsafe cache leaf",
        )?;
        CString::new(bytes).map_err(|_| "NUL cache leaf".into())
    }
    fn open(parent: &File, leaf: &[u8], flags: i32) -> std::io::Result<File> {
        let n = name(leaf).map_err(std::io::Error::other)?;
        // SAFETY: live directory descriptor and bounded NUL-terminated leaf.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                n.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: unique descriptor returned by openat.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Identity {
        dev: u64,
        ino: u64,
    }
    impl Identity {
        fn of(file: &File) -> Result<Self> {
            let m = file.metadata().map_err(io)?;
            Ok(Self {
                dev: m.dev(),
                ino: m.ino(),
            })
        }
        fn digest(self) -> [u8; 32] {
            let mut h = Sha256::new();
            h.update(b"tilo-linux-dev-ino-v1");
            h.update(self.dev.to_le_bytes());
            h.update(self.ino.to_le_bytes());
            h.finalize().into()
        }
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Security {
        uid: u32,
        gid: u32,
        mode: u32,
        acl: [u8; 32],
    }
    fn acl(file: &File) -> Result<[u8; 32]> {
        let mut hash = Sha256::new();
        for key in [c"system.posix_acl_access", c"system.posix_acl_default"] {
            let mut bytes = [0u8; MAX_RECORD];
            // SAFETY: live descriptor, static key, valid output buffer.
            let n = unsafe {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    key.as_ptr(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::ENODATA) {
                    hash.update(0u64.to_le_bytes());
                    continue;
                }
                return Err(io(e));
            }
            let bytes = &bytes[..n as usize];
            require(
                bytes.len() >= 4 && (bytes.len() - 4) % 8 == 0 && bytes[..4] == 2u32.to_le_bytes(),
                "Unsupported POSIX ACL encoding",
            )?;
            for entry in bytes[4..].chunks_exact(8) {
                let tag = u16::from_le_bytes(entry[..2].try_into().unwrap());
                let perm = u16::from_le_bytes(entry[2..4].try_into().unwrap());
                require(
                    matches!(tag, 1 | 2 | 4 | 8 | 16 | 32) && perm <= 7,
                    "Unsupported ACL entry",
                )?;
            }
            // On the explicitly supported Linux filesystems, mode group bits
            // are the POSIX ACL effective mask. Safe named/default ACLs remain
            // supported; private create mode constrains inherited effective grants.
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        Ok(hash.finalize().into())
    }
    fn security(file: &File, directory: bool, private: bool) -> Result<Security> {
        let m = file.metadata().map_err(io)?;
        let uid = tilo_process::effective_uid();
        require(
            if directory {
                m.is_dir()
            } else {
                m.is_file() && m.nlink() == 1
            },
            "Cache type/link refusal",
        )?;
        require(
            effective_mode_allowed(m.uid(), uid, m.mode(), directory, private),
            "Cache owner/effective permissions refusal",
        )?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: valid descriptor and writable statfs storage.
        if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io(std::io::Error::last_os_error()));
        }
        // SAFETY: successful fstatfs initialized storage.
        let kind = unsafe { stat.assume_init() }.f_type;
        require(
            matches!(kind as u64, 0xef53 | 0x58465342 | 0x9123683e),
            "Unqualified cache filesystem (requires Linux ext4/XFS/Btrfs semantics)",
        )?;
        Ok(Security {
            uid: m.uid(),
            gid: m.gid(),
            mode: m.mode(),
            acl: acl(file)?,
        })
    }
    struct Held {
        file: File,
        id: Identity,
        security: Security,
        leaf: Vec<u8>,
        directory: bool,
        private: bool,
    }
    impl Held {
        fn new(file: File, leaf: &[u8], directory: bool, private: bool) -> Result<Self> {
            let id = Identity::of(&file)?;
            let security = security(&file, directory, private)?;
            Ok(Self {
                file,
                id,
                security,
                leaf: leaf.to_vec(),
                directory,
                private,
            })
        }
        fn check(&self, parent: Option<&File>) -> Result<()> {
            require(
                Identity::of(&self.file)? == self.id
                    && security(&self.file, self.directory, self.private)? == self.security,
                "Held cache identity/security changed",
            )?;
            if let Some(parent) = parent {
                let named = open(
                    parent,
                    &self.leaf,
                    libc::O_RDONLY | if self.directory { libc::O_DIRECTORY } else { 0 },
                )
                .map_err(io)?;
                require(
                    Identity::of(&named)? == self.id
                        && security(&named, self.directory, self.private)? == self.security,
                    "Named cache identity/security changed",
                )?;
            }
            Ok(())
        }
    }
    fn directory(parent: &File, leaf: &[u8], create: bool, private: bool) -> Result<Held> {
        let file = match open(parent, leaf, libc::O_RDONLY | libc::O_DIRECTORY) {
            Ok(f) => f,
            Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => {
                let n = name(leaf)?;
                // SAFETY: live descriptor and leaf. Existing collisions are never repaired.
                if unsafe { libc::mkdirat(parent.as_raw_fd(), n.as_ptr(), 0o700) } != 0 {
                    return Err(io(std::io::Error::last_os_error()));
                }
                let file = open(parent, leaf, libc::O_RDONLY | libc::O_DIRECTORY).map_err(io)?;
                security(&file, true, true)?;
                file.sync_all().map_err(io)?;
                parent.sync_all().map_err(io)?;
                file
            }
            Err(e) => return Err(io(e)),
        };
        let held = Held::new(file, leaf, true, private)?;
        held.check(Some(parent))?;
        Ok(held)
    }
    // A fresh open of "." gives each enumeration its own directory offset.
    fn entries(parent: &File) -> Result<Vec<Vec<u8>>> {
        let dot = c".";
        // SAFETY: live descriptor and static path.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io(std::io::Error::last_os_error()));
        }
        // SAFETY: fd ownership transfers to DIR on success.
        let ptr = unsafe { libc::fdopendir(fd) };
        if ptr.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(io(std::io::Error::last_os_error()));
        }
        struct Dir(*mut libc::DIR);
        impl Drop for Dir {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let dir = Dir(ptr);
        let mut out = Vec::new();
        loop {
            // SAFETY: thread-local errno and exclusively owned live DIR.
            unsafe {
                *libc::__errno_location() = 0;
            }
            let entry = unsafe { libc::readdir(dir.0) };
            if entry.is_null() {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() != Some(0) {
                    return Err(io(e));
                }
                break;
            }
            // SAFETY: readdir d_name remains valid until next call, NUL terminated.
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            require(
                out.len() < MAX_SCAN && bytes.len() <= 255,
                "Cache namespace scan bound exceeded",
            )?;
            name(bytes)?;
            out.push(bytes.to_vec());
        }
        out.sort();
        Ok(out)
    }
    struct LockedRoot {
        ancestry: Vec<Held>,
        lock: Held,
        _exclusive: FileLock,
        payload: Held,
        journals: [Held; 2],
    }
    impl LockedRoot {
        fn root(&self) -> &File {
            &self.ancestry.last().unwrap().file
        }
        fn check(&self) -> Result<()> {
            for (i, held) in self.ancestry.iter().enumerate() {
                held.check(i.checked_sub(1).map(|n| &self.ancestry[n].file))?;
            }
            self.lock.check(Some(self.root()))?;
            self.payload.check(Some(self.root()))?;
            for journal in &self.journals {
                journal.check(Some(self.root()))?;
            }
            require(
                entries(self.root())?
                    == vec![
                        b"journal-0".to_vec(),
                        b"journal-1".to_vec(),
                        b"ownership.lock".to_vec(),
                        b"payload".to_vec(),
                    ],
                "Unknown manifest-cache residue",
            )
        }
    }
    /// The per-user cache directory that holds `tilo-internal-install`.
    pub(in crate::setup) fn cache_base() -> Result<std::path::PathBuf> {
        if let Some(cache) = tilo_paths::env_dir("XDG_CACHE_HOME") {
            Ok(cache)
        } else {
            let home = tilo_paths::home().ok_or("Current-user home directory unavailable")?;
            raw_components(home.as_os_str().as_bytes())?;
            Ok(home.join(".cache"))
        }
    }
    pub(in crate::setup) struct CacheRoot {
        lease: Arc<LockedRoot>,
        manifest: String,
    }
    impl CacheRoot {
        #[cfg_attr(
            test,
            expect(
                dead_code,
                reason = "coordinator tests use the deterministic fixture platform"
            )
        )]
        pub(in crate::setup) fn acquire(selected: &SelectedManifest) -> Result<Self> {
            Self::acquire_at(&cache_base()?, selected)
        }
        fn acquire_at(cache: &Path, selected: &SelectedManifest) -> Result<Self> {
            let parts = raw_components(cache.as_os_str().as_bytes())?;
            let root = File::open("/").map_err(io)?;
            let mut ancestry = vec![Held::new(root, b"", true, false)?];
            for (index, part) in parts.iter().enumerate() {
                let parent = &ancestry.last().unwrap().file;
                ancestry.push(directory(parent, part, index + 1 == parts.len(), false)?);
            }
            Self::acquire_below(ancestry, selected)
        }
        fn acquire_below(mut ancestry: Vec<Held>, selected: &SelectedManifest) -> Result<Self> {
            for leaf in [
                b"tilo-internal-install".as_slice(),
                selected.manifest_sha256().as_bytes(),
            ] {
                let parent = &ancestry.last().unwrap().file;
                ancestry.push(directory(parent, leaf, true, true)?);
            }
            let root = &ancestry.last().unwrap().file;
            // Classify before creating. The permanent lock is never unlinked.
            let known = [
                b"ownership.lock".as_slice(),
                b"payload",
                b"journal-0",
                b"journal-1",
            ];
            require(
                entries(root)?.iter().all(|n| known.contains(&n.as_slice())),
                "Unknown cache-root residue",
            )?;
            let lock_file = match open(
                root,
                b"ownership.lock",
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            ) {
                Ok(f) => {
                    f.sync_all().map_err(io)?;
                    root.sync_all().map_err(io)?;
                    f
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    open(root, b"ownership.lock", libc::O_RDWR).map_err(io)?
                }
                Err(e) => return Err(io(e)),
            };
            let lock = Held::new(lock_file, b"ownership.lock", false, true)?;
            require(
                lock.file.metadata().map_err(io)?.len() == 0,
                "Unknown lock contents",
            )?;
            // Nonblocking cooperative exclusive lease, held through a duplicate
            // of the lock descriptor that releases it explicitly when dropped.
            let exclusive = match FileLock::try_exclusive(lock.file.try_clone().map_err(io)?) {
                Ok(exclusive) => exclusive,
                Err(std::fs::TryLockError::WouldBlock) => return Err("Cache Busy".into()),
                Err(std::fs::TryLockError::Error(e)) => return Err(io(e)),
            };
            lock.check(Some(root))?;
            let payload = directory(root, b"payload", true, true)?;
            let journals = [
                directory(root, b"journal-0", true, true)?,
                directory(root, b"journal-1", true, true)?,
            ];
            let lease = Arc::new(LockedRoot {
                ancestry,
                lock,
                _exclusive: exclusive,
                payload,
                journals,
            });
            lease.check()?;
            Ok(Self {
                lease,
                manifest: selected.manifest_sha256().into(),
            })
        }
    }

    type Fingerprint = (u64, u64, u64, i64, i64, i64, i64);
    struct Journal {
        snapshot: Fingerprint,
        held: Held,
        bytes: Vec<u8>,
        generation: u64,
    }
    fn journals(
        root: &LockedRoot,
        selected: &SelectedManifest,
        index: usize,
    ) -> Result<Vec<Journal>> {
        journals_visit(root, selected, index, &mut |_| Ok(()))
    }
    fn journals_visit(
        root: &LockedRoot,
        selected: &SelectedManifest,
        index: usize,
        visit: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<Vec<Journal>> {
        root.check()?;
        let parent = &root.journals[index].file;
        let names = entries(parent)?;
        require(names.len() <= 3, "Excessive journal survivors")?;
        let mut found = Vec::new();
        for leaf in names {
            let file = open(parent, &leaf, libc::O_RDONLY).map_err(io)?;
            let mut held = Held::new(file, &leaf, false, true)?;
            held.check(Some(parent))?;
            let before = fingerprint(&held.file.metadata().map_err(io)?);
            let len = held.file.metadata().map_err(io)?.len();
            require(
                len > 0 && len <= MAX_RECORD as u64,
                "Unknown/torn/oversized journal",
            )?;
            let mut bytes = vec![0; len as usize];
            held.file.read_exact(&mut bytes).map_err(io)?;
            let mut end = [0];
            require(held.file.read(&mut end).map_err(io)? == 0, "Journal grew")?;
            let record = Record::decode(&bytes)?;
            let hint = record.hint_for(selected, index)?;
            require(
                leaf == format!("record-{:010}", hint.generation).as_bytes(),
                "Unknown journal leaf/generation",
            )?;
            held.check(Some(parent))?;
            require(
                fingerprint(&held.file.metadata().map_err(io)?) == before,
                "Journal changed while reading",
            )?;
            visit(&bytes)?;
            held.check(Some(parent))?;
            found.push(Journal {
                snapshot: before,
                held,
                bytes,
                generation: hint.generation,
            });
        }
        root.check()?;
        Ok(found)
    }
    fn fingerprint(m: &Metadata) -> Fingerprint {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    }
    fn same_scan(a: &[Journal], b: &[Journal]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(a, b)| {
                a.snapshot == b.snapshot
                    && a.held.id == b.held.id
                    && a.held.security == b.held.security
                    && a.held.leaf == b.held.leaf
                    && a.bytes == b.bytes
            })
    }
    fn candidate(
        records: &[Journal],
        selected: &SelectedManifest,
        index: usize,
    ) -> Result<Option<(String, [u8; 32], [u8; 32])>> {
        let mut result = None;
        for journal in records {
            let record = Record::decode(&journal.bytes)?;
            let h = record.hint_for(selected, index)?;
            let item = (h.leaf.to_owned(), *h.parent_digest, *h.file_digest);
            require(
                result.as_ref().is_none_or(|prior| prior == &item),
                "Conflicting journal candidates",
            )?;
            result = Some(item);
        }
        Ok(result)
    }
    pub(in crate::setup) struct LinuxProvider<'a> {
        lease: Arc<LockedRoot>,
        selected: &'a SelectedManifest,
        index: usize,
        payload: Held,
        own: Ownership,
        records: Vec<Journal>,
        scanned: bool,
        prepared: bool,
        uncertain: bool,
        fresh: Option<FreshCreation>,
    }
    // Constructed only by the validated exclusive-create branch; never reopened.
    struct FreshCreation;
    pub(in crate::setup) struct Partial<'a> {
        pub(in crate::setup) provider: LinuxProvider<'a>,
        pub(in crate::setup) file: File,
    }
    pub(in crate::setup) struct FinalHandle<'a> {
        lease: Arc<LockedRoot>,
        held: Held,
        selected: &'a SelectedManifest,
        index: usize,
        residue: bool,
    }
    pub(in crate::setup) enum Component<'a> {
        Partial(Partial<'a>),
        Final(FinalHandle<'a>),
    }
    fn verify_bytes(
        file: &mut File,
        selected: &SelectedManifest,
        index: usize,
        cancel: &AtomicBool,
    ) -> Result<()> {
        stop(cancel)?;
        let c = selected
            .bundle()
            .component(index)
            .ok_or("Invalid cache component")?;
        let before = fingerprint(&file.metadata().map_err(io)?);
        require(before.2 == c.byte_size, "Final component length mismatch")?;
        file.seek(SeekFrom::Start(0)).map_err(io)?;
        let mut hash = Sha256::new();
        let mut left = c.byte_size;
        let mut buffer = [0; 64 * 1024];
        while left > 0 {
            stop(cancel)?;
            let n = file
                .read(&mut buffer[..left.min(65536) as usize])
                .map_err(io)?;
            require(n > 0, "Truncated final component")?;
            hash.update(&buffer[..n]);
            left -= n as u64;
        }
        stop(cancel)?;
        let mut eof = [0];
        require(
            file.read(&mut eof).map_err(io)? == 0 && hex::encode(hash.finalize()) == c.sha256,
            "Final component digest/EOF mismatch",
        )?;
        require(
            fingerprint(&file.metadata().map_err(io)?) == before,
            "Final component changed during authentication",
        )?;
        stop(cancel)
    }
    fn final_handle<'a>(
        lease: Arc<LockedRoot>,
        selected: &'a SelectedManifest,
        index: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<FinalHandle<'a>>> {
        lease.check()?;
        let leaf = selected
            .bundle()
            .component(index)
            .ok_or("Invalid component")?
            .filename;
        let file = match open(&lease.payload.file, leaf.as_bytes(), libc::O_RDONLY) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(e)),
        };
        let mut held = Held::new(file, leaf.as_bytes(), false, true)?;
        held.check(Some(&lease.payload.file))?;
        verify_bytes(&mut held.file, selected, index, cancel)?;
        held.check(Some(&lease.payload.file))?;
        lease.payload.file.sync_all().map_err(io)?;
        held.check(Some(&lease.payload.file))?;
        lease.check()?;
        stop(cancel)?;
        Ok(Some(FinalHandle {
            lease,
            held,
            selected,
            index,
            residue: true,
        }))
    }
    fn generated() -> Result<String> {
        let mut bytes = [0u8; 16];
        // SAFETY: valid fixed buffer. Short/interrupted entropy requests refuse.
        let n =
            unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), libc::GRND_NONBLOCK) };
        require(
            n == bytes.len() as isize,
            "Bounded cache name entropy unavailable",
        )?;
        Ok(working_leaf(u128::from_le_bytes(bytes)))
    }
    impl CacheRoot {
        pub(in crate::setup) fn bootstrap<'a>(
            self,
            selected: &'a SelectedManifest,
            cancel: &AtomicBool,
        ) -> Result<[Component<'a>; 2]> {
            stop(cancel)?;
            require(
                self.manifest == selected.manifest_sha256(),
                "Cache selection changed",
            )?;
            self.lease.check()?;
            // Finals authenticate first, independently of stale partial records.
            let mut finals = [
                final_handle(self.lease.clone(), selected, 0, cancel)?,
                final_handle(self.lease.clone(), selected, 1, cancel)?,
            ];
            let mut scans = [
                journals(&self.lease, selected, 0)?,
                journals(&self.lease, selected, 1)?,
            ];
            let candidates = [
                candidate(&scans[0], selected, 0)?,
                candidate(&scans[1], selected, 1)?,
            ];
            require(
                candidates[0]
                    .as_ref()
                    .zip(candidates[1].as_ref())
                    .is_none_or(|(a, b)| a.0 != b.0 && a.2 != b.2),
                "Component partial alias",
            )?;
            let names = entries(&self.lease.payload.file)?;
            for leaf in &names {
                let final_known = (0..2).any(|i| {
                    finals[i].is_some()
                        && selected.bundle().component(i).unwrap().filename.as_bytes() == leaf
                });
                let partial_known = candidates.iter().flatten().any(|c| c.0.as_bytes() == leaf);
                require(
                    final_known || partial_known,
                    "Unknown payload residue; no adoption or cleanup",
                )?;
            }
            let mut result = Vec::with_capacity(2);
            for index in 0..2 {
                stop(cancel)?;
                self.lease.check()?;
                #[cfg(test)]
                if index == 1 {
                    super::tests::boundary("component-acquire-1")?;
                }
                if let Some(mut final_file) = finals[index].take() {
                    final_file.residue = !scans[index].is_empty();
                    result.push(Component::Final(final_file));
                    continue;
                }
                let parent = &self.lease.payload.file;
                let (held, fresh) = if let Some((leaf, parent_id, file_id)) = &candidates[index] {
                    let file = open(parent, leaf.as_bytes(), libc::O_RDWR).map_err(io)?;
                    let held = Held::new(file, leaf.as_bytes(), false, true)?;
                    held.check(Some(parent))?;
                    require(
                        self.lease.payload.id.digest() == *parent_id
                            && held.id.digest() == *file_id,
                        "Journal candidate native identity mismatch",
                    )?;
                    (held, None)
                } else {
                    let mut created = None;
                    for _ in 0..MAX_COLLISIONS {
                        let leaf = generated()?;
                        stop(cancel)?;
                        match open(
                            parent,
                            leaf.as_bytes(),
                            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                        ) {
                            Ok(file) => {
                                let held = Held::new(file, leaf.as_bytes(), false, true)?;
                                held.check(Some(parent))?;
                                require(
                                    held.file.metadata().map_err(io)?.len() == 0,
                                    "Created partial not empty",
                                )?;
                                held.file.sync_all().map_err(io)?;
                                parent.sync_all().map_err(io)?;
                                #[cfg(test)]
                                super::tests::boundary("partial-durable")?;
                                created = Some(held);
                                break;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                            Err(e) => return Err(io(e)),
                        }
                    }
                    (
                        created.ok_or("Cache exclusive-create collision limit")?,
                        Some(FreshCreation),
                    )
                };
                require(
                    held.file.metadata().map_err(io)?.len()
                        <= selected.bundle().component(index).unwrap().byte_size,
                    "Partial exceeds selected length",
                )?;
                let own = Ownership {
                    parent: self.lease.payload.id.digest(),
                    file: held.id.digest(),
                    leaf: String::from_utf8(held.leaf.clone())
                        .map_err(|_| "Invalid partial leaf")?,
                };
                let mut file = held.file.try_clone().map_err(io)?;
                let mut provider = LinuxProvider {
                    lease: self.lease.clone(),
                    selected,
                    index,
                    payload: held,
                    own,
                    records: std::mem::take(&mut scans[index]),
                    scanned: false,
                    prepared: false,
                    uncertain: false,
                    fresh,
                };
                if provider.fresh.is_some() {
                    initialize_fresh(selected, index, &mut provider, &mut file)?;
                }
                stop(cancel)?;
                result.push(Component::Partial(Partial { provider, file }));
            }
            self.lease.check()?;
            result
                .try_into()
                .map_err(|_| "Internal component count".into())
        }
    }
    impl FreshProvider for LinuxProvider<'_> {
        fn take_fresh(&mut self, selected: &SelectedManifest, index: usize) -> Result<Ownership> {
            let fresh = self.fresh.take();
            require(
                fresh.is_some()
                    && !self.uncertain
                    && selected.manifest_sha256() == self.selected.manifest_sha256()
                    && index == self.index,
                "No fresh cache ownership for selection",
            )?;
            Ok(self.own.clone())
        }
        fn initialization_failed(&mut self) {
            self.fresh = None;
            self.uncertain = true;
            self.revoke_preparation();
        }
    }
    impl Provider for LinuxProvider<'_> {
        fn revoke_preparation(&mut self) {
            self.prepared = false;
            self.scanned = false;
            // Uncertain publication is deliberately not cleared by revocation.
        }
        fn prepared(&mut self, proof: PreparationComplete<'_>) -> Result<()> {
            self.prepared = false;
            require(
                self.scanned
                    && !self.uncertain
                    && proof.authorizes(self.selected, self.index, &self.own)?,
                "No successful cache preparation receipt",
            )?;
            self.lease.check()?;
            self.payload.check(Some(&self.lease.payload.file))?;
            self.prepared = true;
            Ok(())
        }
        fn ownership(&self) -> Ownership {
            self.own.clone()
        }
        fn verify(&mut self, file: &File, ownership: &Ownership) -> Result<()> {
            require(
                !self.uncertain && ownership == &self.own,
                "Cache ownership mismatch or uncertain operation",
            )?;
            self.lease.check()?;
            self.payload.check(Some(&self.lease.payload.file))?;
            require(
                Identity::of(file)? == self.payload.id
                    && security(file, false, true)? == self.payload.security
                    && file.metadata().map_err(io)?.len()
                        <= self
                            .selected
                            .bundle()
                            .component(self.index)
                            .unwrap()
                            .byte_size,
                "Borrowed payload changed",
            )
        }
        fn scan(&mut self, visit: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
            self.revoke_preparation();
            require(!self.uncertain, "Uncertain cache operation")?;
            let mut count = 0;
            let fresh = journals_visit(&self.lease, self.selected, self.index, &mut |bytes| {
                require(
                    self.records.get(count).is_some_and(|r| r.bytes == bytes),
                    "Bootstrap journal bytes changed",
                )?;
                count += 1;
                visit(bytes)
            })?;
            require(
                same_scan(&fresh, &self.records),
                "Bootstrap journal substitution",
            )?;
            let parent = &self.lease.journals[self.index].file;
            require(
                entries(parent)?
                    == fresh
                        .iter()
                        .map(|r| r.held.leaf.clone())
                        .collect::<Vec<_>>(),
                "Journal namespace changed across visitor",
            )?;
            for journal in &fresh {
                journal.held.check(Some(parent))?;
                require(
                    fingerprint(&journal.held.file.metadata().map_err(io)?) == journal.snapshot,
                    "Journal bytes changed across visitor",
                )?;
            }
            self.records = fresh;
            self.scanned = true;
            Ok(())
        }
        fn retain(&mut self, keep: &[u64]) -> Result<()> {
            require(
                self.scanned && !self.uncertain && keep.len() <= 2,
                "No validated journal retirement authority",
            )?;
            let fresh = journals(&self.lease, self.selected, self.index)?;
            require(
                same_scan(&fresh, &self.records),
                "Journal changed before retirement",
            )?;
            self.uncertain = true;
            let parent = &self.lease.journals[self.index].file;
            for journal in &self.records {
                if !keep.contains(&journal.generation) {
                    unlink(parent, &journal.held)?;
                }
            }
            parent.sync_all().map_err(io)?;
            self.records.retain(|r| keep.contains(&r.generation));
            self.uncertain = false;
            Ok(())
        }
        fn publish(&mut self, generation: u64, bytes: &[u8], keep: &[u64]) -> Result<()> {
            let mut created = false;
            (|| {
                require(
                    self.scanned
                        && !self.uncertain
                        && bytes.len() <= MAX_RECORD
                        && self.records.len() < 3
                        && keep.len() <= 2
                        && keep.contains(&generation),
                    "Invalid journal publication",
                )?;
                self.lease.check()?;
                self.payload.check(Some(&self.lease.payload.file))?;
                let record = Record::decode(bytes)?;
                let hint = record.hint_for(self.selected, self.index)?;
                require(
                    hint.generation == generation
                        && hint.leaf == self.own.leaf
                        && *hint.parent_digest == self.own.parent
                        && *hint.file_digest == self.own.file,
                    "Journal publication binding mismatch",
                )?;
                require(
                    same_scan(
                        &journals(&self.lease, self.selected, self.index)?,
                        &self.records,
                    ),
                    "Journal changed before publication",
                )?;
                let leaf = format!("record-{generation:010}");
                let parent = &self.lease.journals[self.index].file;
                // Exclusive record leaf is the publication. Any later failure is
                // uncertain; torn/unknown records survive for conservative recovery.
                self.uncertain = true;
                let file = open(
                    parent,
                    leaf.as_bytes(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                )
                .map_err(io)?;
                created = true;
                let mut held = Held::new(file, leaf.as_bytes(), false, true)?;
                held.check(Some(parent))?;
                require(
                    held.file.metadata().map_err(io)?.len() == 0,
                    "New journal not empty",
                )?;
                #[cfg(test)]
                super::tests::boundary("record-created")?;
                held.file.write_all(bytes).map_err(io)?;
                #[cfg(test)]
                super::tests::boundary("record-written")?;
                held.file.flush().map_err(io)?;
                held.file.sync_all().map_err(io)?;
                held.check(Some(parent))?;
                parent.sync_all().map_err(io)?;
                #[cfg(test)]
                super::tests::boundary("record-durable")?;
                self.lease.check()?;
                self.records.push(Journal {
                    snapshot: fingerprint(&held.file.metadata().map_err(io)?),
                    held,
                    bytes: bytes.to_vec(),
                    generation,
                });
                self.records.sort_by(|a, b| a.held.leaf.cmp(&b.held.leaf));
                self.uncertain = false;
                self.retain(keep)
            })()
            .map_err(|e| {
                if created || self.uncertain {
                    self.uncertain = true;
                    format!("Journal publication uncertain; retained residue: {e}")
                } else {
                    e
                }
            })
        }
    }
    impl Provider for &mut LinuxProvider<'_> {
        fn revoke_preparation(&mut self) {
            (**self).revoke_preparation();
        }
        fn prepared(&mut self, proof: PreparationComplete<'_>) -> Result<()> {
            (**self).prepared(proof)
        }

        fn ownership(&self) -> Ownership {
            (**self).ownership()
        }
        fn verify(&mut self, file: &File, own: &Ownership) -> Result<()> {
            (**self).verify(file, own)
        }
        fn scan(&mut self, visit: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
            (**self).scan(visit)
        }
        fn retain(&mut self, keep: &[u64]) -> Result<()> {
            (**self).retain(keep)
        }
        fn publish(&mut self, generation: u64, bytes: &[u8], keep: &[u64]) -> Result<()> {
            (**self).publish(generation, bytes, keep)
        }
    }
    fn unlink(parent: &File, held: &Held) -> Result<()> {
        held.check(Some(parent))?;
        let leaf = name(&held.leaf)?;
        // Cooperative boundary only: no atomic identity-conditioned Linux unlink.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), leaf.as_ptr(), 0) } != 0 {
            return Err(io(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    impl<'a> Partial<'a> {
        pub(in crate::setup) fn publish(mut self, cancel: &AtomicBool) -> Result<FinalHandle<'a>> {
            let mut renamed = false;
            (|| {
                stop(cancel)?;
                let own = self.provider.own.clone();
                self.provider.verify(&self.file, &own)?;
                require(
                    self.provider.prepared,
                    "Publication requires successful Session preparation",
                )?;
                verify_bytes(
                    &mut self.file,
                    self.provider.selected,
                    self.provider.index,
                    cancel,
                )?;
                self.provider.verify(&self.file, &own)?;
                self.file.sync_all().map_err(io)?;
                stop(cancel)?;
                let lease = self.provider.lease.clone();
                let parent = &lease.payload.file;
                let source = name(own.leaf.as_bytes())?;
                let final_name = self
                    .provider
                    .selected
                    .bundle()
                    .component(self.provider.index)
                    .unwrap()
                    .filename;
                let target = name(final_name.as_bytes())?;
                // Actual support is required. ENOSYS/EINVAL/EXDEV never fall back.
                let rc = unsafe {
                    libc::renameat2(
                        parent.as_raw_fd(),
                        source.as_ptr(),
                        parent.as_raw_fd(),
                        target.as_ptr(),
                        libc::RENAME_NOREPLACE,
                    )
                };
                if rc != 0 {
                    let e = std::io::Error::last_os_error();
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        return final_handle(
                            lease,
                            self.provider.selected,
                            self.provider.index,
                            cancel,
                        )?
                        .ok_or("Final collision disappeared".into());
                    }
                    return Err(io(e));
                }
                renamed = true;
                #[cfg(test)]
                super::tests::boundary("final-renamed")?;
                self.provider.payload.leaf = final_name.as_bytes().to_vec();
                self.provider.payload.check(Some(parent))?;
                verify_bytes(
                    &mut self.file,
                    self.provider.selected,
                    self.provider.index,
                    cancel,
                )?;
                self.provider.payload.check(Some(parent))?;
                parent.sync_all().map_err(io)?;
                #[cfg(test)]
                super::tests::boundary("final-durable")?;
                // A failure/cancellation after rename leaves independently recoverable
                // final bytes and old journal evidence; never roll back publication.
                stop(cancel)?;
                self.provider.retain(&[])?;
                let readonly = open(parent, final_name.as_bytes(), libc::O_RDONLY).map_err(io)?;
                let held = Held::new(readonly, final_name.as_bytes(), false, true)?;
                held.check(Some(parent))?;
                require(
                    held.id == self.provider.payload.id
                        && fingerprint(&held.file.metadata().map_err(io)?)
                            == fingerprint(&self.file.metadata().map_err(io)?),
                    "Published readonly final changed",
                )?;
                lease.check()?;
                stop(cancel)?;
                Ok(FinalHandle {
                    lease,
                    held,
                    selected: self.provider.selected,
                    index: self.provider.index,
                    residue: false,
                })
            })()
            .map_err(|e| {
                if renamed {
                    format!("Final publication uncertain; retained residue: {e}")
                } else {
                    e
                }
            })
        }
        /// Discard only after a fresh Session::prepare validates all history and
        /// prefix bytes. Failure retains exact/unknown residue and never reports
        /// clean cancellation. Resume-later simply drops the component instead.
        pub(in crate::setup) fn discard(
            mut self,
            operation: u64,
            cancel: &AtomicBool,
        ) -> Result<()> {
            {
                let selected = self.provider.selected;
                let mut session = super::super::Session::new(
                    selected,
                    self.provider.index,
                    operation,
                    &mut self.provider,
                )?;
                drop(session.prepare(selected, &mut self.file, cancel)?);
            }
            stop(cancel)?;
            let own = self.provider.own.clone();
            self.provider.verify(&self.file, &own)?;
            // Delete payload before journal; a crash cannot leave an unowned
            // nonempty payload which looks eligible for fresh acquisition.
            unlink(&self.provider.lease.payload.file, &self.provider.payload)?;
            self.provider.lease.payload.file.sync_all().map_err(io)?;
            self.provider.retain(&[])
        }
    }
    impl FinalHandle<'_> {
        pub(in crate::setup) fn has_retained_residue(&self) -> bool {
            self.residue
        }

        /// Verify once at actual consumption, then retain native guards through
        /// the callback, including its error path. Restore the original cursor.
        #[cfg_attr(
            test,
            expect(
                dead_code,
                reason = "coordinator tests use the deterministic fixture platform"
            )
        )]
        pub(in crate::setup) fn with_verified_reader<T>(
            &mut self,
            cancel: &AtomicBool,
            consume: impl FnOnce(&mut dyn super::super::HeldReadSeek) -> Result<T>,
        ) -> Result<T> {
            let position = self.held.file.stream_position().map_err(io)?;
            let result = (|| {
                self.verify(cancel)?;
                stop(cancel)?;
                self.held.file.rewind().map_err(io)?;
                consume(&mut self.held.file)
            })();
            // Run every postcheck even on callback/verification failure. Do not
            // short circuit away restoration or the final cancellation check.
            let restored = self
                .held
                .file
                .seek(std::io::SeekFrom::Start(position))
                .map_err(io);
            let held = self.held.check(Some(&self.lease.payload.file));
            let root = self.lease.check();
            let stopped = stop(cancel);
            super::super::finish_held_read(result, restored, held, root, stopped)
        }

        pub(in crate::setup) fn verify(&mut self, cancel: &AtomicBool) -> Result<()> {
            self.lease.check()?;
            self.held.check(Some(&self.lease.payload.file))?;
            verify_bytes(&mut self.held.file, self.selected, self.index, cancel)?;
            self.held.check(Some(&self.lease.payload.file))?;
            self.lease.check()
        }
    }
    #[cfg(test)]
    pub(super) fn fixture_acquire(path: &Path, selected: &SelectedManifest) -> Result<CacheRoot> {
        // CI runs inside an overlay-root container while the workspace itself
        // is a qualified host filesystem. Start the native fixture's held
        // ancestry at that explicitly provisioned directory so the tests
        // still exercise every security invariant below the cache boundary.
        // Production acquisition continues to authenticate from `/`.
        raw_components(path.as_os_str().as_bytes())?;
        let root = File::open(path).map_err(io)?;
        CacheRoot::acquire_below(vec![Held::new(root, b"", true, false)?], selected)
    }
}
#[cfg(test)]
mod tests;
