use super::*;
use crate::staging_manifest::StageManifest;
use std::io::{Read, Write};
struct Name {
    bytes: [u8; 107],
    len: usize,
}
impl Name {
    fn new(device: &[u8; 32], delivery: &[u8; 16], suffix: &[u8]) -> Self {
        let mut name = Self {
            bytes: [0; 107],
            len: 97 + suffix.len(),
        };
        let hex = b"0123456789abcdef";
        for (i, byte) in device.iter().enumerate() {
            name.bytes[2 * i] = hex[(byte >> 4) as usize];
            name.bytes[2 * i + 1] = hex[(byte & 15) as usize];
        }
        name.bytes[64] = b'-';
        for (i, byte) in delivery.iter().enumerate() {
            name.bytes[65 + 2 * i] = hex[(byte >> 4) as usize];
            name.bytes[66 + 2 * i] = hex[(byte & 15) as usize];
        }
        name.bytes[97..name.len].copy_from_slice(suffix);
        name
    }
    fn cstr(&self) -> &CStr {
        CStr::from_bytes_with_nul(&self.bytes[..=self.len])
            .expect("bounded canonical stage filename")
    }
}
fn parse(name: &CStr) -> Result<([u8; 32], [u8; 16], u8), HostWorkFailure> {
    let bytes = name.to_bytes();
    let (stem, kind) = if let Some(s) = bytes.strip_suffix(b".meta.tmp") {
        (s, 4)
    } else if let Some(s) = bytes.strip_suffix(b".meta") {
        (s, 2)
    } else if let Some(s) = bytes.strip_suffix(b".data") {
        (s, 1)
    } else {
        return Err(HostWorkFailure::Unavailable);
    };
    if stem.len() != 97 || stem[64] != b'-' {
        return Err(HostWorkFailure::Unavailable);
    }
    fn digit(b: u8) -> Result<u8, HostWorkFailure> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(HostWorkFailure::Unavailable),
        }
    }
    let mut device = [0; 32];
    let mut delivery = [0; 16];
    for (i, b) in device.iter_mut().enumerate() {
        *b = digit(stem[2 * i])? * 16 + digit(stem[2 * i + 1])?;
    }
    for (i, b) in delivery.iter_mut().enumerate() {
        *b = digit(stem[65 + 2 * i])? * 16 + digit(stem[66 + 2 * i])?;
    }
    if device == [0; 32] || delivery == [0; 16] {
        return Err(HostWorkFailure::Unavailable);
    }
    Ok((device, delivery, kind))
}
pub(in crate::store::host_work) fn optional(
    parent: &File,
    name: &CStr,
    flags: i32,
) -> Result<Option<File>, HostWorkFailure> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(HostWorkFailure::Unavailable)
        };
    }
    let file = unsafe { File::from_raw_fd(fd) };
    private(&file, false)?;
    if file.metadata().map_err(io)?.nlink() != 1 {
        return Err(HostWorkFailure::Unavailable);
    }
    Ok(Some(file))
}
pub(in crate::store::host_work) fn unlink(
    parent: &File,
    name: &CStr,
) -> Result<(), HostWorkFailure> {
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0
        || std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
    {
        Ok(())
    } else {
        Err(HostWorkFailure::Unavailable)
    }
}
fn read_manifest(
    directory: &File,
    device: &[u8; 32],
    delivery: &[u8; 16],
) -> Result<Option<StageManifest>, HostWorkFailure> {
    let name = Name::new(device, delivery, b".meta");
    let Some(mut file) = optional(directory, name.cstr(), libc::O_RDONLY)? else {
        return Ok(None);
    };
    if file.metadata().map_err(io)?.len() != 148 {
        return Err(HostWorkFailure::Unavailable);
    }
    let mut bytes = [0; 148];
    file.read_exact(&mut bytes).map_err(io)?;
    StageManifest::decode(&bytes)
        .map(Some)
        .map_err(|_| HostWorkFailure::Unavailable)
}
#[derive(Clone, Copy)]
struct Files {
    device: [u8; 32],
    delivery: [u8; 16],
    kinds: u8,
}
pub(super) fn recover(
    directory: &File,
    workspace: u64,
) -> Result<[Option<StageRecord>; STAGE_SLOTS], HostWorkFailure> {
    let mut files: [Option<Files>; STAGE_SLOTS] = [None; STAGE_SLOTS];
    let mut entries = 0;
    visit(directory, |name| {
        entries += 1;
        if entries > 96 {
            return Err(HostWorkFailure::Capacity);
        }
        let (device, delivery, kind) = parse(name)?;
        let file =
            optional(directory, name, libc::O_RDONLY)?.ok_or(HostWorkFailure::Unavailable)?;
        let maximum = if kind == 1 { 64 * 1024 * 1024 } else { 148 };
        if file.metadata().map_err(io)?.len() > maximum {
            return Err(HostWorkFailure::Unavailable);
        }
        let index = files
            .iter()
            .position(|f| f.is_some_and(|f| f.device == device && f.delivery == delivery))
            .or_else(|| files.iter().position(Option::is_none))
            .ok_or(HostWorkFailure::Capacity)?;
        let file = files[index].get_or_insert(Files {
            device,
            delivery,
            kinds: 0,
        });
        if file.kinds & kind != 0 {
            return Err(HostWorkFailure::Unavailable);
        }
        file.kinds |= kind;
        Ok(())
    })?;
    let mut recovered = [None; STAGE_SLOTS];
    let mut total = 0u64;
    // Validate every recognized manifest before removing any inert orphan.
    for (index, files) in files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| f.map(|f| (i, f)))
    {
        if files.kinds & 2 == 0 {
            continue;
        }
        if files.kinds & 1 == 0 {
            return Err(HostWorkFailure::Unavailable);
        }
        let manifest = read_manifest(directory, &files.device, &files.delivery)?
            .ok_or(HostWorkFailure::Unavailable)?;
        let identity = manifest.identity;
        if identity.workspace != workspace
            || identity.device != files.device
            || identity.delivery != files.delivery
        {
            return Err(HostWorkFailure::Conflict);
        }
        total = total
            .checked_add(identity.total)
            .ok_or(HostWorkFailure::Capacity)?;
        if total > WORKSPACE_STAGE_BYTES {
            return Err(HostWorkFailure::Capacity);
        }
        if recovered
            .iter()
            .flatten()
            .filter(|r: &&StageRecord| r.identity.device == identity.device)
            .count()
            >= 2
        {
            return Err(HostWorkFailure::Capacity);
        }
        let name = Name::new(&identity.device, &identity.delivery, b".data");
        let mut data =
            optional(directory, name.cstr(), libc::O_RDWR)?.ok_or(HostWorkFailure::Unavailable)?;
        let length = data.metadata().map_err(io)?.len();
        if length < manifest.prefix || length > identity.total {
            return Err(HostWorkFailure::Unavailable);
        }
        let mut remaining = manifest.prefix;
        let mut hash = blake3::Hasher::new();
        let mut buffer = [0u8; 64 * 1024];
        while remaining > 0 {
            let count = (remaining as usize).min(buffer.len());
            data.read_exact(&mut buffer[..count]).map_err(io)?;
            hash.update(&buffer[..count]);
            remaining -= count as u64;
        }
        if hash.finalize().as_bytes() != &manifest.prefix_digest {
            return Err(HostWorkFailure::Unavailable);
        }
        if length > manifest.prefix {
            data.set_len(manifest.prefix).map_err(io)?;
            data.sync_all().map_err(io)?;
        }
        recovered[index] = Some(StageRecord {
            identity,
            leases: 0,
        });
    }
    for files in files.into_iter().flatten() {
        if files.kinds & 2 == 0 && files.kinds & 1 != 0 {
            unlink(
                directory,
                Name::new(&files.device, &files.delivery, b".data").cstr(),
            )?;
        }
        if files.kinds & 4 != 0 {
            unlink(
                directory,
                Name::new(&files.device, &files.delivery, b".meta.tmp").cstr(),
            )?;
        }
    }
    directory_barrier(directory)?;
    Ok(recovered)
}
impl StagingReservation {
    pub(crate) fn validate_completed_file(
        &self,
        store: &SessionStore,
        workspace: u64,
        file: &File,
        now_ns: u64,
    ) -> Result<(), HostWorkFailure> {
        self.sole()?;
        if store.is_poisoned() {
            return Err(HostWorkFailure::Unavailable);
        }
        let root = store.directory.metadata().map_err(io)?;
        if self.directory.key
            != (DirectoryKey {
                device: root.dev(),
                inode: root.ino(),
                workspace,
            })
            || self.identity.workspace != workspace
        {
            return Err(HostWorkFailure::Conflict);
        }
        private(file, false)?;
        let metadata = file.metadata().map_err(io)?;
        let name = Name::new(&self.identity.device, &self.identity.delivery, b".data");
        let current = optional(&self.directory.directory, name.cstr(), libc::O_RDONLY)?
            .ok_or(HostWorkFailure::Unavailable)?;
        let current = current.metadata().map_err(io)?;
        if metadata.dev() != current.dev()
            || metadata.ino() != current.ino()
            || metadata.nlink() != 1
            || metadata.len() != self.identity.total
            || current.len() != self.identity.total
        {
            return Err(HostWorkFailure::Conflict);
        }
        let manifest = self.read_manifest()?.ok_or(HostWorkFailure::Unavailable)?;
        if manifest.prefix != self.identity.total
            || manifest.prefix_digest != self.identity.digest
            || manifest.expires_at_ns <= now_ns
        {
            return Err(HostWorkFailure::Conflict);
        }
        Ok(())
    }
    /// Re-establish directory durability before an existing prefix is reported.
    pub fn recover_barrier(&self) -> Result<(), HostWorkFailure> {
        self.sole()?;
        directory_barrier(&self.directory.directory)
    }
    fn sole(&self) -> Result<(), HostWorkFailure> {
        let state = self
            .directory
            .inventory
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        let record = state
            .slots
            .iter()
            .find(|s| s.key == Some(self.directory.key))
            .and_then(|s| {
                s.stages
                    .iter()
                    .flatten()
                    .find(|r| r.identity == self.identity)
            })
            .ok_or(HostWorkFailure::Unavailable)?;
        if record.leases != 1 {
            return Err(HostWorkFailure::Capacity);
        }
        Ok(())
    }
    pub(crate) fn same_decode_inventory(&self, lease: &LargeDecodeLease) -> bool {
        Arc::ptr_eq(
            self.directory
                .inventory
                .0
                .as_ref()
                .expect("live staging inventory"),
            lease.inventory.0.as_ref().expect("live decode inventory"),
        )
    }
    pub fn same_pool(&self, pool: &HostBytes) -> bool {
        self.directory.inventory.same_pool(pool)
    }
    pub fn preflight(&self) -> Result<(), HostWorkFailure> {
        let bytes = self
            .directory
            .inventory
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?
            .disk;
        disk_preflight(&self.directory.directory, bytes)
    }
    pub(crate) fn identity(&self) -> &StageIdentity {
        &self.identity
    }
    pub fn open_data(&self, create: bool) -> Result<File, HostWorkFailure> {
        self.sole()?;
        self.preflight()?;
        let name = Name::new(&self.identity.device, &self.identity.delivery, b".data");
        let flags = libc::O_RDWR
            | if create {
                libc::O_CREAT | libc::O_EXCL
            } else {
                0
            };
        let file = optional(&self.directory.directory, name.cstr(), flags)?
            .ok_or(HostWorkFailure::Unavailable)?;
        if file.metadata().map_err(io)?.len() > self.identity.total {
            return Err(HostWorkFailure::Unavailable);
        }
        Ok(file)
    }
    pub(crate) fn read_manifest(&self) -> Result<Option<StageManifest>, HostWorkFailure> {
        let value = read_manifest(
            &self.directory.directory,
            &self.identity.device,
            &self.identity.delivery,
        )?;
        if value.is_some_and(|m| m.identity != self.identity) {
            return Err(HostWorkFailure::Conflict);
        }
        Ok(value)
    }
    pub(crate) fn write_manifest(&self, manifest: &StageManifest) -> Result<(), HostWorkFailure> {
        self.sole()?;
        self.preflight()?;
        if manifest.identity != self.identity {
            return Err(HostWorkFailure::Conflict);
        }
        let bytes = manifest.encode().map_err(|_| HostWorkFailure::Conflict)?;
        let tmp = Name::new(&self.identity.device, &self.identity.delivery, b".meta.tmp");
        let target = Name::new(&self.identity.device, &self.identity.delivery, b".meta");
        let mut file = optional(
            &self.directory.directory,
            tmp.cstr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
        )?
        .ok_or(HostWorkFailure::Unavailable)?;
        file.write_all(&bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
        drop(file);
        let fd = self.directory.directory.as_raw_fd();
        if unsafe { libc::renameat(fd, tmp.cstr().as_ptr(), fd, target.cstr().as_ptr()) } != 0 {
            return Err(HostWorkFailure::Unavailable);
        }
        directory_barrier(&self.directory.directory)
    }
    pub fn remove(self) -> Result<(), HostWorkFailure> {
        let mut state = self
            .directory
            .inventory
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        let index = state
            .slots
            .iter()
            .position(|s| s.key == Some(self.directory.key))
            .ok_or(HostWorkFailure::Unavailable)?;
        let position = state.slots[index]
            .stages
            .iter()
            .position(|r| r.is_some_and(|r| r.identity == self.identity))
            .ok_or(HostWorkFailure::Unavailable)?;
        if state.slots[index].stages[position].as_ref().unwrap().leases != 1 {
            return Err(HostWorkFailure::Capacity);
        }
        for suffix in [b".data".as_slice(), b".meta", b".meta.tmp"] {
            unlink(
                &self.directory.directory,
                Name::new(&self.identity.device, &self.identity.delivery, suffix).cstr(),
            )?;
        }
        directory_barrier(&self.directory.directory)?;
        state.slots[index].stages[position] = None;
        state.disk -= self.identity.total;
        drop(state);
        Ok(())
    }
}

pub(in crate::store::host_work) fn directory_barrier(
    directory: &File,
) -> Result<(), HostWorkFailure> {
    #[cfg(test)]
    barrier_fault::check(directory)?;
    directory.sync_all().map_err(io)
}
#[cfg(test)]
pub(in crate::store::host_work) mod barrier_fault {
    use super::*;
    use std::cell::RefCell;
    thread_local! { static DIRECTORY: RefCell<Option<(u64,u64)>> = const { RefCell::new(None) }; }
    struct Reset(Option<(u64, u64)>);
    impl Drop for Reset {
        fn drop(&mut self) {
            DIRECTORY.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    pub(in crate::store::host_work) fn with<T>(
        directory: &File,
        callback: impl FnOnce() -> T,
    ) -> T {
        let metadata = directory.metadata().expect("actual staging directory");
        let guard =
            Reset(DIRECTORY.with(|slot| slot.replace(Some((metadata.dev(), metadata.ino())))));
        let result = callback();
        drop(guard);
        result
    }
    pub(super) fn check(directory: &File) -> Result<(), HostWorkFailure> {
        let metadata = directory.metadata().map_err(io)?;
        if DIRECTORY.with(|slot| *slot.borrow() == Some((metadata.dev(), metadata.ino()))) {
            Err(HostWorkFailure::Unavailable)
        } else {
            Ok(())
        }
    }
}
