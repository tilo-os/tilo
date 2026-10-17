//! Raw-name Home presentation over existing directory/file/job capabilities.
//! No source credentials or descriptors reach this target adapter. Native jobs
//! provide durable NOREPLACE outcomes; replacing rename/history is unsupported.
use crate::{
    EntryKind, EntryName, Metadata, OpenMode,
    directory_backend::{DirectoryStat, JobUpdate},
    directory_connection::{DirectoryConnection, ROOT_DIRECTORY},
    operations::ItemState,
};
use fuser::{
    FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, InitFlags,
    IoctlFlags, KernelConfig, LockOwner, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyIoctl, ReplyOpen, ReplyStatfs, ReplyWrite,
    ReplyXattr, Request, WriteFlags,
};
use std::os::unix::ffi::OsStringExt;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant, UNIX_EPOCH},
};
const ROOT: u64 = 1;
const NODES: usize = 4096;
// Native pins consume real source-worker descriptors. The canonical managed
// worker has RLIMIT_NOFILE=1024: retain at most 256 nodes, plus at most 64 byte
// handles and 64 cursors, leaving half that table for provider/runtime/transient
// descriptors. This is admission, never authority or permission to drop a pin.
const RESIDENT_NODES: usize = 256;
const TRIM_AT: usize = 64;
const TRIM_TO: usize = 32;
#[path = "directory_fuse/invalidation.rs"]
mod invalidation;
pub use invalidation::{InvalidationControl, InvalidationWorker};
const HANDLES: usize = 64;
const TTL: Duration = Duration::ZERO;
#[derive(Clone)]
struct Node {
    parent: u64,
    name: Option<EntryName>,
    identity: [u64; 2],
    kind: EntryKind,
    file_type: u32,
    pin: Option<u64>,
    directory: Option<u64>,
    lookups: u64,
    published: bool,
    invalidating: bool,
}
struct OpenFile {
    node: u64,
    remote: u64,
    append: bool,
}
struct Rows {
    node: u64,
    cursor: u64,
    sequence: u64,
    base: u64,
    entries: Vec<Result<(u64, FileType, OsString), i32>>,
    high_water: u64,
    done: bool,
}
struct Table {
    next: u64,
    nodes: HashMap<u64, Node>,
    files: HashMap<u64, OpenFile>,
    rows: HashMap<u64, Rows>,
    notification_error: Option<i32>,
}
pub struct DirectoryFilesystem {
    source: DirectoryConnection,
    uid: u32,
    gid: u32,
    table: Arc<Mutex<Table>>,
    invalidations: InvalidationControl,
}
fn error(code: i32) -> io::Error {
    io::Error::from_raw_os_error(code)
}
fn reply_errno(failure: io::Error) -> fuser::Errno {
    if failure.raw_os_error().is_some() {
        return failure.into();
    }
    if crate::native_reply::is_native_refusal(&failure) {
        return fuser::Errno::from_i32(libc::EACCES);
    }
    if crate::native_reply::is_operation_refusal(&failure) {
        return fuser::Errno::from_i32(libc::ENFILE);
    }
    // Transport/authority errors remain I/O failure, never successful access.
    fuser::Errno::EIO
}
/// The FUSE type of a native `S_IFMT` value. `DirectoryClient` already
/// refuses any other value; one that still arrives is an I/O failure, never
/// a guessed type.
fn kind(raw: u32) -> io::Result<FileType> {
    Ok(match raw {
        libc::S_IFREG => FileType::RegularFile,
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFIFO => FileType::NamedPipe,
        libc::S_IFSOCK => FileType::Socket,
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFCHR => FileType::CharDevice,
        _ => return Err(error(libc::EIO)),
    })
}
fn name(name: &OsStr) -> io::Result<EntryName> {
    EntryName::new(name.to_os_string()).map_err(|_| error(libc::EINVAL))
}
fn mode(flags: i32, created: bool) -> io::Result<OpenMode> {
    match flags & (libc::O_ACCMODE | libc::O_APPEND | libc::O_TRUNC) {
        libc::O_RDONLY => Ok(OpenMode::Read),
        libc::O_RDWR => Ok(OpenMode::ReadWriteExisting),
        v if v == (libc::O_WRONLY | libc::O_TRUNC) => Ok(OpenMode::Write),
        v if v == (libc::O_WRONLY | libc::O_APPEND) => Ok(OpenMode::Append),
        libc::O_WRONLY => Ok(OpenMode::WriteExisting),
        v if created && v == (libc::O_RDWR | libc::O_TRUNC) => Ok(OpenMode::ReadWriteExisting),
        _ => Err(error(libc::EOPNOTSUPP)),
    }
}
impl DirectoryFilesystem {
    /// Uses only a mode-selected, independently admitted directory channel.
    /// # Errors
    /// Refuses invalid target credentials or missing/invalid directory custody.
    pub fn new(source: DirectoryConnection, uid: u32, gid: u32) -> io::Result<Self> {
        if uid == 0 {
            return Err(error(libc::EINVAL));
        }
        let root = source.metadata(ROOT_DIRECTORY)?;
        if root.metadata.kind != EntryKind::Directory {
            return Err(error(libc::ENOTDIR));
        }
        let table = Arc::new(Mutex::new(Table {
            next: ROOT,
            nodes: HashMap::from([(
                ROOT,
                Node {
                    parent: ROOT,
                    name: None,
                    identity: root.identity,
                    kind: EntryKind::Directory,
                    file_type: root.file_type,
                    pin: None,
                    directory: Some(ROOT_DIRECTORY),
                    lookups: 1,
                    published: true,
                    invalidating: false,
                },
            )]),
            files: HashMap::new(),
            rows: HashMap::new(),
            notification_error: None,
        }));
        Ok(Self {
            source,
            uid,
            gid,
            invalidations: InvalidationControl::new(&table),
            table,
        })
    }
    /// Attach only the notifier from the same canonical FUSE Session. The
    /// returned worker has no source authority, descriptors or alternate mount.
    #[must_use]
    pub fn invalidation_control(&self) -> InvalidationControl {
        self.invalidations.clone()
    }
    fn published(&self, ino: u64) {
        if let Ok(mut table) = self.lock()
            && let Some(node) = table.nodes.get_mut(&ino)
        {
            node.published = true;
            // A fresh positive reply can recreate a dentry after invalidation.
            node.invalidating = false;
        }
        self.invalidations.wake();
    }
    fn lock(&self) -> io::Result<std::sync::MutexGuard<'_, Table>> {
        self.table.lock().map_err(|_| error(libc::EIO))
    }
    fn next(table: &mut Table) -> io::Result<u64> {
        table.next = table
            .next
            .checked_add(1)
            .ok_or_else(|| error(libc::ENFILE))?;
        Ok(table.next)
    }
    fn directory(&self, table: &mut Table, ino: u64) -> io::Result<u64> {
        let node = table
            .nodes
            .get(&ino)
            .cloned()
            .ok_or_else(|| error(libc::ESTALE))?;
        if node.kind != EntryKind::Directory {
            return Err(error(libc::ENOTDIR));
        }
        if let Some(directory) = node.directory {
            self.source.metadata(directory)?;
            return Ok(directory);
        }
        let parent = self.directory(table, node.parent)?;
        let directory = self.source.directory(
            parent,
            node.name.as_ref().ok_or_else(|| error(libc::ESTALE))?,
        )?;
        let actual = self.source.metadata(directory)?;
        if actual.identity != node.identity {
            self.source.release_directory(directory)?;
            return Err(error(libc::ESTALE));
        }
        table
            .nodes
            .get_mut(&ino)
            .ok_or_else(|| error(libc::ESTALE))?
            .directory = Some(directory);
        Ok(directory)
    }
    fn observe(
        &self,
        table: &mut Table,
        parent: u64,
        name: &EntryName,
    ) -> io::Result<(u64, DirectoryStat)> {
        let directory = self.directory(table, parent)?;
        let mut stat = self
            .source
            .stat(directory, name)?
            .ok_or_else(|| error(libc::ENOENT))?;
        let existing = table
            .nodes
            .iter()
            .find(|(_, node)| {
                node.parent == parent
                    && node.name.as_ref() == Some(name)
                    && node.identity == stat.identity
            })
            .map(|(ino, _)| *ino);
        let ino = if let Some(ino) = existing {
            ino
        } else {
            if let Some(errno) = table.notification_error {
                return Err(error(errno));
            }
            if table.nodes.len() >= RESIDENT_NODES || table.nodes.len() >= NODES {
                // Never wait for kernel FORGET inside a related callback.
                // Existing owners remain held; a later retry can be admitted
                // after the independent notifier and actual FORGET progress.
                return Err(error(libc::ENFILE));
            }
            let ino = Self::next(table)?;
            let (held_directory, pin) = match stat.metadata.kind {
                EntryKind::Directory => {
                    let held = self.source.directory(directory, name)?;
                    let actual = match self.source.metadata(held) {
                        Ok(actual) => actual,
                        Err(error) => {
                            let _ = self.source.release_directory(held);
                            return Err(error);
                        }
                    };
                    if actual.identity != stat.identity {
                        self.source.release_directory(held)?;
                        return Err(error(libc::ESTALE));
                    }
                    stat = actual;
                    (Some(held), None)
                }
                EntryKind::File => {
                    let held = self.source.pin_metadata(directory, name, stat.identity)?;
                    let actual = match self.source.metadata_pin(held) {
                        Ok(actual) => actual,
                        Err(error) => {
                            let _ = self.source.release_pin(held);
                            return Err(error);
                        }
                    };
                    if actual.identity != stat.identity {
                        self.source.release_pin(held)?;
                        return Err(error(libc::ESTALE));
                    }
                    stat = actual;
                    (None, Some(held))
                }
                _ => (None, None),
            };
            table.nodes.insert(
                ino,
                Node {
                    parent,
                    name: Some(name.clone()),
                    identity: stat.identity,
                    kind: stat.metadata.kind,
                    file_type: stat.file_type,
                    pin,
                    directory: held_directory,
                    lookups: 0,
                    published: false,
                    invalidating: false,
                },
            );
            ino
        };
        Ok((ino, stat))
    }
    fn attr(
        &self,
        ino: u64,
        metadata: &Metadata,
        file_type: u32,
        execute_bits: u16,
    ) -> io::Result<FileAttr> {
        let time = metadata
            .modified_unix_seconds
            .and_then(|v| u64::try_from(v).ok())
            .and_then(|v| UNIX_EPOCH.checked_add(Duration::from_secs(v)))
            .unwrap_or(UNIX_EPOCH);
        let directory = metadata.kind == EntryKind::Directory;
        Ok(FileAttr {
            ino: INodeNo(ino),
            size: metadata.size,
            blocks: metadata.size.div_ceil(512),
            atime: time,
            mtime: time,
            ctime: time,
            crtime: time,
            kind: kind(file_type)?,
            perm: if directory {
                0o700
            } else if metadata.kind == EntryKind::Other {
                0
            } else {
                0o600 | (execute_bits & 0o111)
            },
            nlink: if directory { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        })
    }
    fn attributes(
        &self,
        table: &mut Table,
        ino: u64,
        handle: Option<FileHandle>,
    ) -> io::Result<FileAttr> {
        let node = table
            .nodes
            .get(&ino)
            .cloned()
            .ok_or_else(|| error(libc::ESTALE))?;
        let (metadata, execute_bits) = if let Some(handle) = handle {
            let file = table
                .files
                .get(&handle.0)
                .filter(|file| file.node == ino)
                .ok_or_else(|| error(libc::EBADF))?;
            // The actual open inode remains pinned. Read original mode from
            // that SAME metadata-only held capability, never a replacement path.
            let stat = self
                .source
                .metadata_pin(node.pin.ok_or_else(|| error(libc::ESTALE))?)?;
            if stat.identity != node.identity {
                return Err(error(libc::ESTALE));
            }
            (self.source.metadata_handle(file.remote)?, stat.execute_bits)
        } else if let Some(pin) = node.pin {
            let stat = self.source.metadata_pin(pin)?;
            if stat.identity != node.identity {
                return Err(error(libc::ESTALE));
            }
            (stat.metadata, stat.execute_bits)
        } else if node.kind == EntryKind::Directory {
            let directory = self.directory(table, ino)?;
            let stat = self.source.metadata(directory)?;
            if stat.identity != node.identity {
                return Err(error(libc::ESTALE));
            }
            (stat.metadata, stat.execute_bits)
        } else {
            let parent = self.directory(table, node.parent)?;
            let stat = self
                .source
                .stat(
                    parent,
                    node.name.as_ref().ok_or_else(|| error(libc::ESTALE))?,
                )?
                .ok_or_else(|| error(libc::ENOENT))?;
            if stat.identity != node.identity {
                return Err(error(libc::ESTALE));
            }
            (stat.metadata, stat.execute_bits)
        };
        self.attr(ino, &metadata, node.file_type, execute_bits)
    }
    fn open_file(&self, table: &mut Table, ino: u64, mode: OpenMode) -> io::Result<u64> {
        if table.files.len() >= HANDLES {
            return Err(error(libc::EMFILE));
        }
        let node = table
            .nodes
            .get(&ino)
            .cloned()
            .ok_or_else(|| error(libc::ESTALE))?;
        if node.kind != EntryKind::File {
            return Err(error(libc::EOPNOTSUPP));
        }
        let parent = self.directory(table, node.parent)?;
        let remote = self.source.open_expected(
            parent,
            node.name.as_ref().ok_or_else(|| error(libc::ESTALE))?,
            mode,
            node.identity,
        )?;
        let id = Self::next(table)?;
        table.files.insert(
            id,
            OpenFile {
                node: ino,
                remote,
                append: mode == OpenMode::Append,
            },
        );
        Ok(id)
    }
    fn sweep(&self, table: &mut Table) -> io::Result<()> {
        loop {
            let removable = table
                .nodes
                .iter()
                .find(|(id, node)| {
                    **id != ROOT
                        && node.lookups == 0
                        && !table.nodes.values().any(|child| child.parent == **id)
                        && !table.files.values().any(|file| file.node == **id)
                        && !table.rows.values().any(|rows| rows.node == **id)
                })
                .map(|(id, _)| *id);
            let Some(id) = removable else {
                break;
            };
            if let Some(node) = table.nodes.remove(&id) {
                if let Some(pin) = node.pin {
                    self.source.release_pin(pin)?;
                }
                if let Some(directory) = node.directory {
                    self.source.release_directory(directory)?;
                }
            }
        }
        Ok(())
    }
    fn job(&self, id: u64) -> io::Result<[u64; 2]> {
        self.job_effect(id, false)?.ok_or_else(|| error(libc::EIO))
    }
    fn job_effect(&self, id: u64, require_capture: bool) -> io::Result<Option<[u64; 2]>> {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or_else(|| error(libc::EOVERFLOW))?;
        let mut failed = false;
        let mut destination = None;
        let mut captured = false;
        let result = (|| {
            loop {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "native mutation outcome unknown",
                    ));
                }
                for update in self.source.poll(id)?.updates {
                    match update {
                        JobUpdate::Item {
                            state,
                            destination: actual,
                            recoveries,
                            ..
                        } => {
                            failed |= state != ItemState::Completed;
                            destination = actual;
                            captured |= !recoveries.is_empty();
                        }
                        JobUpdate::Finished {
                            completed,
                            failed: count,
                            cancelled,
                        } => {
                            return if !failed
                                && completed == 1
                                && count == 0
                                && cancelled == 0
                                && (!require_capture || captured)
                            {
                                Ok(destination)
                            } else {
                                Err(error(libc::EIO))
                            };
                        }
                        JobUpdate::Progress { .. } => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })();
        // Lost/unknown effects are never retried, nor represented as completion.
        let released = self.source.release_job(id);
        match (result, released) {
            (Ok(identity), Ok(())) => Ok(identity),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }
}
impl Filesystem for DirectoryFilesystem {
    fn getxattr(&self, _: &Request, ino: INodeNo, attribute: &OsStr, size: u32, reply: ReplyXattr) {
        let result = (|| {
            let query = attribute.to_str().ok_or_else(|| error(libc::ENODATA))?;
            if query != "user.tilo.history"
                && query != "user.tilo.record-id"
                && fs_protocol::files::FileHistoryReadQueryV1::parse_xattr(query).is_err()
            {
                return Err(error(libc::ENODATA));
            }
            let mut table = self.lock()?;
            let node = table
                .nodes
                .get(&ino.0)
                .cloned()
                .ok_or_else(|| error(libc::ESTALE))?;
            if node.kind != EntryKind::File {
                return Err(error(libc::ENODATA));
            }
            // The request stays bound to this actual FUSE inode. In particular,
            // an open old FD cannot acquire the history of a replacement name.
            if let Some(pin) = node.pin
                && self.source.metadata_pin(pin)?.identity != node.identity
            {
                return Err(error(libc::ESTALE));
            }
            let parent = self.directory(&mut table, node.parent)?;
            // The history query is a round trip to the source: never under the table lock.
            drop(table);
            let name = node.name.as_ref().ok_or_else(|| error(libc::ESTALE))?;
            let bytes = self
                .source
                .history_query(parent, name, node.identity, query.as_bytes())?;
            if bytes.len() > fs_protocol::files::MAX_HISTORY_READ_BYTES {
                return Err(error(libc::E2BIG));
            }
            Ok(bytes)
        })();
        match result {
            Ok(bytes) if size == 0 => match u32::try_from(bytes.len()) {
                Ok(length) => reply.size(length),
                Err(_) => reply.error(fuser::Errno::E2BIG),
            },
            Ok(bytes) if bytes.len() <= size as usize => reply.data(&bytes),
            Ok(_) => reply.error(fuser::Errno::ERANGE),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn statfs(&self, _: &Request, _: INodeNo, reply: ReplyStatfs) {
        // The held Home root supplies filesystem-wide capacity. File inode
        // handles are not converted into directory grants or byte permissions.
        match self.source.statfs(ROOT_DIRECTORY) {
            Ok(stats) => reply.statfs(
                stats.blocks,
                stats.blocks_free,
                stats.blocks_available,
                stats.files,
                stats.files_free,
                stats.block_size,
                stats.name_max,
                stats.fragment_size,
            ),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn init(&mut self, _: &Request, config: &mut KernelConfig) -> io::Result<()> {
        config
            .add_capabilities(InitFlags::FUSE_ATOMIC_O_TRUNC)
            .map_err(|_| error(libc::EOPNOTSUPP))?;
        Ok(())
    }
    fn lookup(&self, _: &Request, parent: INodeNo, component: &OsStr, reply: ReplyEntry) {
        let result = (|| {
            let mut table = self.lock()?;
            let (ino, stat) = self.observe(&mut table, parent.0, &name(component)?)?;
            let node = table
                .nodes
                .get_mut(&ino)
                .ok_or_else(|| error(libc::ESTALE))?;
            node.published = false;
            node.lookups = node
                .lookups
                .checked_add(1)
                .ok_or_else(|| error(libc::ENFILE))?;
            drop(table);
            Ok::<_, io::Error>((
                ino,
                self.attr(ino, &stat.metadata, stat.file_type, stat.execute_bits)?,
            ))
        })();
        match result {
            Ok((ino, attr)) => {
                reply.entry(&TTL, &attr, Generation(0));
                self.published(ino);
            }
            Err(error) => {
                reply.error(reply_errno(error));
                self.invalidations.wake();
            }
        }
    }
    fn forget(&self, _: &Request, ino: INodeNo, count: u64) {
        if let Ok(mut table) = self.lock() {
            if let Some(node) = table.nodes.get_mut(&ino.0) {
                node.lookups = node.lookups.saturating_sub(count);
            }
            let _ = self.sweep(&mut table);
        }
        self.invalidations.wake();
    }
    fn getattr(&self, _: &Request, ino: INodeNo, handle: Option<FileHandle>, reply: ReplyAttr) {
        let result = self
            .lock()
            .and_then(|mut table| self.attributes(&mut table, ino.0, handle));
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn setattr(
        &self,
        _: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        ctime: Option<std::time::SystemTime>,
        handle: Option<FileHandle>,
        created_at: Option<std::time::SystemTime>,
        chgtime: Option<std::time::SystemTime>,
        bkuptime: Option<std::time::SystemTime>,
        flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let result = (|| {
            if uid.is_some()
                || gid.is_some()
                || size.is_some()
                || atime.is_some()
                || mtime.is_some()
                || ctime.is_some()
                || created_at.is_some()
                || chgtime.is_some()
                || bkuptime.is_some()
                || flags.is_some()
            {
                return Err(error(libc::EOPNOTSUPP));
            }
            let mode = mode.ok_or_else(|| error(libc::EOPNOTSUPP))?;
            if mode & !0o7777 != 0 {
                return Err(error(libc::EINVAL));
            }
            let mut table = self.lock()?;
            let node = table
                .nodes
                .get(&ino.0)
                .cloned()
                .ok_or_else(|| error(libc::ESTALE))?;
            if node.kind != EntryKind::File {
                return Err(error(libc::EPERM));
            }
            if let Some(handle) = handle
                && table
                    .files
                    .get(&handle.0)
                    .ok_or_else(|| error(libc::EBADF))?
                    .node
                    != ino.0
            {
                return Err(error(libc::EBADF));
            }
            let directory = self.directory(&mut table, node.parent)?;
            self.source.set_created_permissions(
                directory,
                node.name.as_ref().ok_or_else(|| error(libc::ESTALE))?,
                node.identity,
                mode,
            )?;
            // Native setter checks real original-UID fchmod/readback. Presentation
            // retains the target policy baseline; mode is no authority upgrade.
            let result = self.attributes(&mut table, ino.0, handle);
            drop(table);
            result
        })();
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn ioctl(
        &self,
        _: &Request,
        ino: INodeNo,
        handle: FileHandle,
        flags: IoctlFlags,
        command: u32,
        input: &[u8],
        output: u32,
        reply: ReplyIoctl,
    ) {
        use tilo_files::native::atomic::save_marker;
        let result = (|| {
            if !flags.is_empty() {
                return Err(error(libc::EINVAL));
            }
            let mut table = self.lock()?;
            let file = table
                .files
                .get(&handle.0)
                .filter(|file| file.node == ino.0)
                .ok_or_else(|| error(libc::EBADF))?;
            self.source.metadata_handle(file.remote)?;
            let node = table
                .nodes
                .get(&ino.0)
                .cloned()
                .filter(|node| node.kind == EntryKind::File)
                .ok_or_else(|| error(libc::ESTALE))?;
            let result = match command {
                save_marker::PROBE if input.is_empty() && output == 4 => {
                    Ok(Some(save_marker::MAGIC))
                }
                save_marker::ARM if output == 0 => {
                    let raw = save_marker::decode(input)?;
                    let target = EntryName::new(std::ffi::OsString::from_vec(raw.to_vec()))
                        .map_err(|_| error(libc::EINVAL))?;
                    let parent = self.directory(&mut table, node.parent)?;
                    self.source.atomic_save(
                        parent,
                        node.name.as_ref().ok_or_else(|| error(libc::ESTALE))?,
                        node.identity,
                        &target,
                    )?;
                    Ok(None)
                }
                save_marker::PROBE | save_marker::ARM => Err(error(libc::EINVAL)),
                _ => Err(error(libc::ENOTTY)),
            };
            drop(table);
            result
        })();
        match result {
            Ok(Some(magic)) => reply.ioctl(0, &magic),
            Ok(None) => reply.ioctl(0, &[]),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn readlink(&self, _: &Request, ino: INodeNo, reply: ReplyData) {
        let result = (|| {
            let mut table = self.lock()?;
            let node = table
                .nodes
                .get(&ino.0)
                .cloned()
                .ok_or_else(|| error(libc::ESTALE))?;
            if node.kind != EntryKind::Symlink {
                return Err(error(libc::EINVAL));
            }
            let parent = self.directory(&mut table, node.parent)?;
            let result = self.source.readlink(
                parent,
                node.name.as_ref().ok_or_else(|| error(libc::ESTALE))?,
                node.identity,
            );
            drop(table);
            result
        })();
        match result {
            Ok(bytes) => reply.data(&bytes),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn open(&self, _: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let result = mode(flags.0, false).and_then(|mode| {
            self.lock()
                .and_then(|mut table| self.open_file(&mut table, ino.0, mode))
        });
        match result {
            Ok(id) => reply.opened(FileHandle(id), FopenFlags::FOPEN_DIRECT_IO),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn read(
        &self,
        _: &Request,
        ino: INodeNo,
        handle: FileHandle,
        offset: u64,
        size: u32,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let result = self.lock().and_then(|table| {
            let file = table
                .files
                .get(&handle.0)
                .filter(|file| file.node == ino.0)
                .ok_or_else(|| error(libc::EBADF))?;
            self.source.read(file.remote, offset, size)
        });
        match result {
            Ok(bytes) => reply.data(&bytes),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn write(
        &self,
        _: &Request,
        ino: INodeNo,
        handle: FileHandle,
        offset: u64,
        bytes: &[u8],
        _: WriteFlags,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let result = self.lock().and_then(|table| {
            let file = table
                .files
                .get(&handle.0)
                .filter(|file| file.node == ino.0)
                .ok_or_else(|| error(libc::EBADF))?;
            if file.append {
                self.source.append(file.remote, bytes)
            } else {
                self.source.write(file.remote, offset, bytes)
            }
        });
        match result {
            Ok(count) => reply.written(count),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn flush(
        &self,
        _: &Request,
        ino: INodeNo,
        handle: FileHandle,
        _: LockOwner,
        reply: ReplyEmpty,
    ) {
        let result = self.lock().and_then(|table| {
            let file = table
                .files
                .get(&handle.0)
                .filter(|file| file.node == ino.0)
                .ok_or_else(|| error(libc::EBADF))?;
            self.source.flush(file.remote)
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn fsync(&self, req: &Request, ino: INodeNo, handle: FileHandle, _: bool, reply: ReplyEmpty) {
        self.flush(req, ino, handle, LockOwner(0), reply);
    }
    fn release(
        &self,
        _: &Request,
        ino: INodeNo,
        handle: FileHandle,
        _: OpenFlags,
        _: Option<LockOwner>,
        _: bool,
        reply: ReplyEmpty,
    ) {
        let result = self.lock().and_then(|mut table| {
            let file = table
                .files
                .remove(&handle.0)
                .filter(|file| file.node == ino.0)
                .ok_or_else(|| error(libc::EBADF))?;
            self.source.release(file.remote)?;
            self.sweep(&mut table)
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_errno(error)),
        }
        self.invalidations.wake();
    }
    fn opendir(&self, _: &Request, ino: INodeNo, _: OpenFlags, reply: ReplyOpen) {
        let result = (|| {
            let mut table = self.lock()?;
            if table.rows.len() >= HANDLES {
                return Err(error(libc::EMFILE));
            }
            let directory = self.directory(&mut table, ino.0)?;
            let parent = table
                .nodes
                .get(&ino.0)
                .ok_or_else(|| error(libc::ESTALE))?
                .parent;
            let rows = vec![
                Ok((ino.0, FileType::Directory, OsString::from("."))),
                Ok((parent, FileType::Directory, OsString::from(".."))),
            ];
            let cursor = self.source.open_cursor(directory)?;
            let id = Self::next(&mut table)?;
            table.rows.insert(
                id,
                Rows {
                    node: ino.0,
                    cursor,
                    sequence: 0,
                    base: 0,
                    high_water: 0,
                    entries: rows,
                    done: false,
                },
            );
            drop(table);
            Ok(id)
        })();
        match result {
            Ok(id) => reply.opened(FileHandle(id), FopenFlags::empty()),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn fsyncdir(&self, _: &Request, ino: INodeNo, handle: FileHandle, _: bool, reply: ReplyEmpty) {
        let result = (|| {
            let mut table = self.lock()?;
            table
                .rows
                .get(&handle.0)
                .filter(|rows| rows.node == ino.0)
                .ok_or_else(|| error(libc::EBADF))?;
            let directory = self.directory(&mut table, ino.0)?;
            let result = self.source.sync_directory(directory);
            drop(table);
            result
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn readdir(
        &self,
        _: &Request,
        ino: INodeNo,
        handle: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let result = (|| {
            let mut table = self.lock()?;
            let directory = self.directory(&mut table, ino.0)?;
            let parent = table
                .nodes
                .get(&ino.0)
                .ok_or_else(|| error(libc::ESTALE))?
                .parent;
            let rows = table
                .rows
                .get_mut(&handle.0)
                .filter(|rows| rows.node == ino.0)
                .ok_or_else(|| error(libc::EBADF))?;
            if offset > rows.high_water {
                return Err(error(libc::EINVAL));
            }
            if offset < rows.base {
                // Rewind/replay uses a new independent OFD, one bounded page at a
                // time. Under concurrent mutation these are live positions, not
                // snapshot/CAS bookmarks. No caller cookie mints path authority.
                self.source.release_cursor(rows.cursor)?;
                rows.cursor = self.source.open_cursor(directory)?;
                rows.sequence = 0;
                rows.base = 0;
                rows.done = false;
                rows.entries = vec![
                    Ok((ino.0, FileType::Directory, OsString::from("."))),
                    Ok((parent, FileType::Directory, OsString::from(".."))),
                ];
            }
            let replay_deadline = Instant::now()
                .checked_add(Duration::from_secs(5))
                .ok_or_else(|| error(libc::EOVERFLOW))?;
            loop {
                let end = rows
                    .base
                    .checked_add(rows.entries.len() as u64)
                    .ok_or_else(|| error(libc::EOVERFLOW))?;
                if offset < end || rows.done {
                    if offset > end {
                        return Err(error(libc::ESTALE));
                    }
                    break;
                }
                if Instant::now() >= replay_deadline {
                    return Err(error(libc::ETIMEDOUT));
                }
                let page = self.source.cursor_page(rows.cursor, rows.sequence)?;
                rows.sequence = page.next_sequence;
                rows.done = page.done;
                rows.base = end;
                rows.entries = page
                    .entries
                    .into_iter()
                    .map(|row| match row.stat {
                        Some(stat) => kind(stat.file_type)
                            .map(|kind| (0, kind, row.name.as_os_str().to_os_string()))
                            .map_err(|_| libc::EIO),
                        None => Err(libc::EACCES),
                    })
                    .collect();
            }
            let index = offset
                .checked_sub(rows.base)
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| error(libc::EINVAL))?;
            for (index, row) in rows.entries.iter().enumerate().skip(index) {
                let (entry_ino, kind, name) = row.as_ref().map_err(|code| error(*code))?;
                let cookie = rows
                    .base
                    .checked_add(index as u64)
                    .and_then(|v| v.checked_add(1))
                    .ok_or_else(|| error(libc::EOVERFLOW))?;
                if reply.add(INodeNo(*entry_ino), cookie, *kind, name) {
                    break;
                }
                rows.high_water = rows.high_water.max(cookie);
            }
            drop(table);
            Ok::<_, io::Error>(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn releasedir(
        &self,
        _: &Request,
        ino: INodeNo,
        handle: FileHandle,
        _: OpenFlags,
        reply: ReplyEmpty,
    ) {
        let result = self.lock().and_then(|mut table| {
            let rows = table
                .rows
                .remove(&handle.0)
                .filter(|rows| rows.node == ino.0)
                .ok_or_else(|| error(libc::EBADF))?;
            self.source.release_cursor(rows.cursor)?;
            self.sweep(&mut table)
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_errno(error)),
        }
        self.invalidations.wake();
    }
    fn create(
        &self,
        _: &Request,
        parent: INodeNo,
        component: &OsStr,
        _: u32,
        _: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| {
            let mode = mode(flags, true)?;
            let mut table = self.lock()?;
            let name = name(component)?;
            let directory = self.directory(&mut table, parent.0)?;
            if self.source.stat(directory, &name)?.is_some() {
                return Err(error(libc::EEXIST));
            }
            let job = self.source.create_file(directory, &name)?;
            let identity = self.job(job)?;
            let (ino, stat) = self.observe(&mut table, parent.0, &name)?;
            if stat.identity != identity {
                return Err(error(libc::ESTALE));
            }
            let handle = self.open_file(&mut table, ino, mode)?;
            let node = table
                .nodes
                .get_mut(&ino)
                .ok_or_else(|| error(libc::ESTALE))?;
            node.lookups = node
                .lookups
                .checked_add(1)
                .ok_or_else(|| error(libc::EOVERFLOW))?;
            drop(table);
            Ok((
                self.attr(ino, &stat.metadata, stat.file_type, stat.execute_bits)?,
                handle,
            ))
        })();
        match result {
            Ok((attr, id)) => {
                let ino = attr.ino.0;
                reply.created(
                    &TTL,
                    &attr,
                    Generation(0),
                    FileHandle(id),
                    FopenFlags::FOPEN_DIRECT_IO,
                );
                self.published(ino);
            }
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn unlink(&self, _: &Request, parent: INodeNo, component: &OsStr, reply: ReplyEmpty) {
        let result = (|| {
            let mut table = self.lock()?;
            let name = name(component)?;
            let directory = self.directory(&mut table, parent.0)?;
            let stat = self
                .source
                .stat(directory, &name)?
                .ok_or_else(|| error(libc::ENOENT))?;
            if stat.metadata.kind != EntryKind::File {
                return Err(error(libc::EPERM));
            }
            let job = self
                .source
                .retire_created(directory, &name, stat.identity)?;
            // Source policy admits only its real own-created temporary. The
            // native completed effect retains it in recovery, never raw Purge.
            let effect = self.job_effect(job, true)?;
            drop(table);
            if effect.is_some() {
                return Err(error(libc::EIO));
            }
            // Completion is the durable native captured-inode receipt. The
            // filtered scope may now release that name, so a later lookup is
            // outside the scope and cannot be used as cleanup evidence.
            Ok(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn mkdir(
        &self,
        _: &Request,
        parent: INodeNo,
        component: &OsStr,
        _: u32,
        _: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let mut table = self.lock()?;
            let name = name(component)?;
            let directory = self.directory(&mut table, parent.0)?;
            if self.source.stat(directory, &name)?.is_some() {
                return Err(error(libc::EEXIST));
            }
            let job = self.source.create_directory(directory, &name)?;
            let identity = self.job(job)?;
            let (ino, stat) = self.observe(&mut table, parent.0, &name)?;
            if stat.identity != identity {
                return Err(error(libc::ESTALE));
            }
            let node = table
                .nodes
                .get_mut(&ino)
                .ok_or_else(|| error(libc::ESTALE))?;
            node.lookups = node
                .lookups
                .checked_add(1)
                .ok_or_else(|| error(libc::EOVERFLOW))?;
            drop(table);
            self.attr(ino, &stat.metadata, stat.file_type, stat.execute_bits)
        })();
        match result {
            Ok(attr) => {
                let ino = attr.ino.0;
                reply.entry(&TTL, &attr, Generation(0));
                self.published(ino);
            }
            Err(error) => reply.error(reply_errno(error)),
        }
    }
    fn rename(
        &self,
        _: &Request,
        parent: INodeNo,
        component: &OsStr,
        newparent: INodeNo,
        newcomponent: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let result = (|| {
            if parent != newparent || (flags.bits() != 0 && flags.bits() != libc::RENAME_NOREPLACE)
            {
                return Err(error(libc::EOPNOTSUPP));
            }
            let mut table = self.lock()?;
            let old = name(component)?;
            let new = name(newcomponent)?;
            let directory = self.directory(&mut table, parent.0)?;
            let job = if flags.bits() == libc::RENAME_NOREPLACE {
                if self.source.stat(directory, &new)?.is_some() {
                    return Err(error(libc::EEXIST));
                }
                self.source.rename_no_replace(directory, &old, &new)?
            } else {
                // No local rename/exchange fallback. The backend is closed unless
                // it owns the canonical signed history coordinator and actual job.
                self.source.rename_replace(directory, &old, &new)?
            };
            self.job(job)?;
            for node in table.nodes.values_mut() {
                if node.parent == parent.0 && node.name.as_ref() == Some(&old) {
                    node.name = Some(new.clone());
                }
            }
            drop(table);
            Ok::<_, io::Error>(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(reply_errno(error)),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "owned real native directory/RPC assertions"
)]
mod tests {
    use super::*;
    use crate::{
        FsOwner, FsRight, NativeGrantAuthority, directory::DirectoryWorkspaceBackend,
        workspace_server::serve_workspace_connected,
    };
    use std::{
        fs::File,
        os::unix::{
            fs::MetadataExt,
            net::{UnixListener, UnixStream},
        },
        sync::Arc,
    };

    #[test]
    fn plain_write_only_open_preserves_existing_mode_without_read_or_truncate() {
        assert_eq!(
            mode(libc::O_WRONLY, false).unwrap(),
            OpenMode::WriteExisting
        );
        assert_eq!(
            mode(libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC, false).unwrap(),
            OpenMode::WriteExisting
        );
        assert_eq!(mode(libc::O_WRONLY, true).unwrap(), OpenMode::WriteExisting);
        assert_eq!(
            mode(libc::O_WRONLY | libc::O_TRUNC, false).unwrap(),
            OpenMode::Write
        );
        assert_eq!(
            mode(libc::O_WRONLY | libc::O_APPEND, false).unwrap(),
            OpenMode::Append
        );
    }

    fn assert_native_symlink_identity(disk: &std::path::Path, source: &DirectoryConnection) {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        // Exercise actual connected RPC, native backend, and bounded bridge.
        let raw = std::ffi::OsString::from_vec(b"nested/missing-\xff".to_vec());
        std::os::unix::fs::symlink(&raw, disk.join("raw-link")).unwrap();
        let link_name = EntryName::new("raw-link".into()).unwrap();
        let link = source.stat(ROOT_DIRECTORY, &link_name).unwrap().unwrap();
        assert_eq!(
            source
                .readlink(ROOT_DIRECTORY, &link_name, link.identity)
                .unwrap(),
            raw.as_bytes()
        );
        std::os::unix::fs::symlink("other", disk.join("replacement-link")).unwrap();
        std::fs::rename(disk.join("replacement-link"), disk.join("raw-link")).unwrap();
        assert!(
            source
                .readlink(ROOT_DIRECTORY, &link_name, link.identity)
                .is_err()
        );
    }

    fn assert_native_mode_observations(
        disk: &std::path::Path,
        fs: &DirectoryFilesystem,
        table: &mut Table,
    ) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(disk.join("file"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let executable = fs
            .source
            .stat(ROOT_DIRECTORY, &EntryName::new("file".into()).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(executable.execute_bits, 0o111);
        let (executable_ino, actual) = fs
            .observe(table, ROOT, &EntryName::new("file".into()).unwrap())
            .unwrap();
        assert_eq!(
            fs.attr(
                executable_ino,
                &actual.metadata,
                actual.file_type,
                actual.execute_bits
            )
            .unwrap()
            .perm,
            0o711
        );
        let handle = fs.open_file(table, executable_ino, OpenMode::Read).unwrap();
        std::fs::set_permissions(disk.join("file"), std::fs::Permissions::from_mode(0o640))
            .unwrap();
        assert_eq!(
            fs.attributes(table, executable_ino, None).unwrap().perm,
            0o600
        );
        assert_eq!(
            fs.attributes(table, executable_ino, Some(FileHandle(handle)))
                .unwrap()
                .perm,
            0o600
        );
        // Metadata remains observational; virtual owner and write/read policy stay target-local.
        assert_eq!(
            fs.attributes(table, executable_ino, None).unwrap().uid,
            1000
        );
        let held = table.files.remove(&handle).unwrap();
        fs.source.release(held.remote).unwrap();
    }

    fn assert_native_pin_capacity(
        disk: &std::path::Path,
        fs: &DirectoryFilesystem,
        table: &mut Table,
        old: u64,
    ) {
        // Actual native metadata pins consume descriptors even without byte
        // opens. Capacity refusal must leave the old unlinked inode pinned.
        while table.nodes.len() < RESIDENT_NODES {
            let name = format!("capacity-{}", table.nodes.len());
            std::fs::write(disk.join(&name), b"capacity").unwrap();
            fs.observe(table, ROOT, &EntryName::new(name.into()).unwrap())
                .unwrap();
        }
        std::fs::write(disk.join("capacity-refused"), b"not admitted").unwrap();
        assert_eq!(
            fs.observe(
                table,
                ROOT,
                &EntryName::new("capacity-refused".into()).unwrap()
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::ENFILE)
        );
        assert_eq!(table.nodes.len(), RESIDENT_NODES);
        assert_eq!(fs.attributes(table, old, None).unwrap().size, 12);
        assert!(table.nodes[&old].pin.is_some());
    }

    fn assert_native_special_types(fs: &DirectoryFilesystem, table: &mut Table) {
        for (name, expected) in [("socket", FileType::Socket), ("fifo", FileType::NamedPipe)] {
            let (ino, stat) = fs
                .observe(table, ROOT, &EntryName::new(name.into()).unwrap())
                .unwrap();
            assert_eq!(
                fs.attr(ino, &stat.metadata, stat.file_type, stat.execute_bits)
                    .unwrap()
                    .kind,
                expected
            );
            assert_eq!(fs.attributes(table, ino, None).unwrap().kind, expected);
            assert_eq!(
                fs.open_file(table, ino, OpenMode::Read)
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EOPNOTSUPP)
            );
        }
    }

    #[test]
    fn native_lookup_pins_inode_and_preserves_socket_fifo_types() {
        let disk = tempfile::tempdir().unwrap();
        let path = disk.path().join("file");
        std::fs::write(&path, b"old contents").unwrap();
        let socket = UnixListener::bind(disk.path().join("socket")).unwrap();
        let fifo = std::ffi::CString::new(disk.path().join("fifo").as_os_str().as_encoded_bytes())
            .unwrap();
        // Create an actual special inode; byte opens below must still refuse.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let owner = FsOwner {
            workspace: "native-home-lookup".into(),
            app_id: "so.tilo.file-explorer".into(),
        };
        let authority = NativeGrantAuthority::new();
        let root = authority
            .admit_directory(
                File::open(disk.path()).unwrap(),
                owner.clone(),
                vec![FsRight::Enumerate, FsRight::Read, FsRight::Write],
            )
            .unwrap();
        let session = authority
            .bind(
                owner,
                uuid::Uuid::new_v4(),
                std::slice::from_ref(&root),
                &[],
            )
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (server_stream, stream) = UnixStream::pair().unwrap();
        let server = serve_workspace_connected(
            server_stream,
            Arc::new(DirectoryWorkspaceBackend::new(session, root.clone()).unwrap()),
            runtime.handle().clone(),
        )
        .unwrap();
        let source = DirectoryConnection::connected(stream).unwrap();
        assert_native_symlink_identity(disk.path(), &source);
        let fs = DirectoryFilesystem::new(source.clone(), 1000, 1000).unwrap();
        let mut table = fs.lock().unwrap();
        assert_native_mode_observations(disk.path(), &fs, &mut table);
        assert_native_special_types(&fs, &mut table);
        let (old, observed) = fs
            .observe(&mut table, ROOT, &EntryName::new("file".into()).unwrap())
            .unwrap();
        assert!(table.nodes[&old].pin.is_some());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"new").unwrap();
        let actual = std::fs::metadata(&path).unwrap();
        assert_ne!(
            observed.identity,
            [actual.dev(), actual.ino()],
            "held metadata FD prevents physical inode reuse"
        );
        assert_eq!(fs.attributes(&mut table, old, None).unwrap().size, 12);
        let (new, replaced) = fs
            .observe(&mut table, ROOT, &EntryName::new("file".into()).unwrap())
            .unwrap();
        assert_ne!(old, new);
        assert_eq!(replaced.metadata.size, 3);
        assert_ne!(observed.identity, replaced.identity);
        assert_native_pin_capacity(disk.path(), &fs, &mut table, old);
        authority.revoke_directory(&root).unwrap();
        assert!(
            source
                .stat(ROOT_DIRECTORY, &EntryName::new("file".into()).unwrap())
                .is_err(),
            "revocation denies further mode observations"
        );
        assert!(
            fs.attributes(&mut table, old, None).is_err(),
            "held mode observations cannot outlive actual native authority"
        );
        drop(table);
        drop(fs);
        drop(socket);
        // The deliberately revoked capability fault remains the worker's
        // terminal result. Retirement joins that worker and must preserve the
        // denial, rather than report a clean authority checkpoint.
        assert_eq!(
            source.retire().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        server.retire().unwrap();
        drop(runtime);
    }
}
