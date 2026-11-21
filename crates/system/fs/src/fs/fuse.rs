//! What the mount answers: the FUSE requests a workspace folder serves, each one admitted for
//! the process that asked, carried out on this mount's tree and store, and replied to.

use super::*;

impl Filesystem for QuickFS {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> Result<(), io::Error> {
        let _ = config.set_max_readahead(1024 * 1024);
        // Every directory operation takes the namespace lock, so the kernel
        // need not serialise lookups/creates per directory for us.
        let _ = config.add_capabilities(InitFlags::FUSE_PARALLEL_DIROPS);
        // `readdirplus` answers a listing and its attributes in one round
        // trip (`ls -l`, file managers); the kernel picks it adaptively.
        let _ = config
            .add_capabilities(InitFlags::FUSE_DO_READDIRPLUS | InitFlags::FUSE_READDIRPLUS_AUTO);
        // Writeback caching stays off: with it the kernel owns `i_size` and mtime and ignores the
        // sizes this daemon reports, so a remote edit that changes a file's length would not
        // become visible without explicit invalidation.
        if let Some(passthrough) = &self.passthrough {
            match (
                config.add_capabilities(InitFlags::FUSE_PASSTHROUGH),
                config.set_max_stack_depth(1),
            ) {
                (Ok(()), Ok(_)) => passthrough.kernel_ready.store(true, Ordering::Release),
                _ => eprintln!("FUSE passthrough is not offered by this kernel; serving reads"),
            }
        }
        Ok(())
    }

    /// Resolve one name in a directory. Namespace lookup and view evaluation run on every call in
    /// the same order, so a nonexistent name and a name hidden by the view cost the same work and
    /// return the same `ENOENT`.
    fn lookup(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let _answering = self.activity.begin();
        note_request("lookup");
        match self.lookup_entry(parent.0, name.as_bytes()) {
            Some((attr, ttl)) => {
                let attr = self.presented_to(req, attr);
                reply.entry_with_ttls(&self.attr_ttl(ttl), &ttl, &attr, Generation(0));
            }
            None => reply.error(to_errno(ENOENT)),
        }
    }

    fn getattr(&self, req: &Request, ino: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
        let _answering = self.activity.begin();
        note_request("getattr");
        match self.attr_for_request(ino.0, fh.map(|fh| fh.0)) {
            Ok(attr) => reply.attr(&self.attr_ttl(TTL), &self.presented_to(req, attr)),
            Err(e) => reply.error(to_errno(e)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let _answering = self.activity.begin();
        note_request("readdir");
        // Entries outside the view simply aren't in the directory; offsets
        // below index what the app can see, so paging never has a gap.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let Some(entries) = self.listing_page(ino.0, offset) else {
            reply.error(to_errno(ENOTDIR));
            return;
        };
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        for (i, (e_ino, kind, name)) in entries.into_iter().enumerate().skip(skip) {
            let next_offset = u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1);
            if reply.add(
                INodeNo(e_ino),
                next_offset,
                kind,
                OsStr::from_bytes(name.as_bytes()),
            ) {
                break;
            }
        }
        reply.ok();
        if let Some(signal) = &self.change_signal {
            signal.note_listed(ino.0);
        }
        self.heuristics.on_readdir(ino.0);
    }

    /// `readdir` plus each entry's attributes, exactly as `lookup` would
    /// report them, for the same filtered view.
    fn readdirplus(
        &self,
        req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectoryPlus,
    ) {
        let _answering = self.activity.begin();
        note_request("readdirplus");
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let Some(entries) = self.listing_page(ino.0, offset) else {
            reply.error(to_errno(ENOTDIR));
            return;
        };
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        // Each page's attributes are read in one namespace pass over the entries it answers,
        // so a folder of a thousand answers pays a handful of locks rather than a thousand.
        let mut remaining = entries.into_iter().enumerate().skip(skip);
        'pages: loop {
            let page: Vec<(usize, (u64, FileType, fs_protocol::FileNameV1))> =
                remaining.by_ref().take(ATTRIBUTES_PER_PASS).collect();
            if page.is_empty() {
                break;
            }
            let attrs: Vec<Option<FileAttr>> = {
                let ns = self.ns();
                page.iter()
                    .map(|(_, (e_ino, _, _))| ns.getattr(*e_ino))
                    .collect()
            };
            for ((i, (e_ino, _, name)), attr) in page.into_iter().zip(attrs) {
                let Some(attr) = attr else {
                    continue;
                };
                let attr = self.live_attr(attr, self.staged_len(e_ino));
                let attr = self.presented_to(req, attr);
                let ttl = self.attr_ttl(TTL);
                let next_offset = u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1);
                if reply.add(
                    INodeNo(e_ino),
                    next_offset,
                    OsStr::from_bytes(name.as_bytes()),
                    &ttl,
                    &attr,
                    Generation(0),
                ) {
                    break 'pages;
                }
            }
        }
        reply.ok();
        if let Some(signal) = &self.change_signal {
            signal.note_listed(ino.0);
        }
        self.heuristics.on_readdir(ino.0);
    }

    fn open(&self, req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let _answering = self.activity.begin();
        note_request("open");
        let _writer = if matches!(flags.acc_mode(), OpenAccMode::O_RDONLY) {
            None
        } else {
            match self.admit_writer(
                req.pid(),
                Some(ino.0),
                fs_protocol::file_action::verbs::WRITE,
            ) {
                Ok(writer) => Some(writer),
                Err(errno) => {
                    reply.error(to_errno(errno));
                    return;
                }
            }
        };
        let opened_path = {
            let ns = self.ns();
            ns.getattr(ino.0).and_then(|_| ns.path_for(ino.0))
        };
        let Some(opened_path) = opened_path else {
            reply.error(to_errno(ENOENT));
            return;
        };
        // Requires the identity to hold the access the open mode implies.
        // Outside the view ⇒ `ENOENT`; in the view but not writable ⇒
        // `EROFS`, so opening for writing falls back to read-only.
        for access in required_accesses(flags.acc_mode()) {
            if let Err(e) = self.check_path(&opened_path, *access) {
                reply.error(to_errno(e));
                return;
            }
        }
        let handle = self.next_file_handle.fetch_add(1, Ordering::Relaxed);
        self.open_file_handles.lock_recover().insert(
            handle,
            OpenFileHandleState {
                ino: ino.0,
                opened_path,
                writable: matches!(
                    flags.acc_mode(),
                    OpenAccMode::O_WRONLY | OpenAccMode::O_RDWR
                ),
                append: flags.0 & libc::O_APPEND != 0,
            },
        );
        let cache = self.open_cache_flags(ino.0);
        if flags.acc_mode() == OpenAccMode::O_RDONLY
            && let Some(backing) = self.passthrough_backing(ino.0)
        {
            match reply.open_backing(&backing) {
                Ok(id) => {
                    reply.opened_passthrough(FileHandle(handle), cache, &id);
                    if let Some(passthrough) = &self.passthrough {
                        passthrough.hold(handle, id, backing);
                    }
                    self.predict_after_open(ino.0);
                    return;
                }
                Err(error) => {
                    if let Some(passthrough) = &self.passthrough {
                        passthrough.disable(&error);
                    }
                }
            }
        }
        reply.opened(FileHandle(handle), cache);
        self.predict_after_open(ino.0);
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let _answering = self.activity.begin();
        note_request("read");
        let _timing = OpTimer::new("read");
        match self.plan_read(ino.0, fh.0, offset, size) {
            ReadPlan::Error(e) => reply.error(to_errno(e)),
            ReadPlan::Buffered(buffered) => self.reply_buffered(ino.0, buffered, reply),
            ReadPlan::Local(data) => reply.data(&data),
            ReadPlan::Remote(hash, workspace) => self.reply_remote(
                ino.0,
                hash,
                workspace,
                (offset, u64::from(size)),
                flags.0 & libc::O_NONBLOCK != 0,
                reply,
            ),
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the FUSE create callback keeps authorization, creation, and returned attributes on one inode snapshot"
    )]
    fn create(
        &self,
        req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let _answering = self.activity.begin();
        note_request("create");
        let _timing = OpTimer::new("create");
        let writer =
            match self.admit_writer(req.pid(), None, fs_protocol::file_action::verbs::NEW_FILE) {
                Ok(writer) => writer,
                Err(errno) => {
                    reply.error(to_errno(errno));
                    return;
                }
            };
        let name_bytes = name.as_bytes();
        if let Err(e) = self.check_new_name(parent.0, name_bytes) {
            reply.error(to_errno(e));
            return;
        }
        let ino = match self.create_for_write(parent.0, name_bytes, mode & !umask) {
            Ok(created) => created,
            Err(error) => {
                reply.error(to_errno(error));
                return;
            }
        };
        let attr = self.ns().getattr(ino);
        let Some(attr) = attr else {
            reply.error(to_errno(EIO));
            return;
        };
        self.publish();
        drop(writer);
        let handle = self.next_file_handle.fetch_add(1, Ordering::Relaxed);
        let opened_path = self.ns().path_for(attr.ino.0);
        if let Some(opened_path) = opened_path {
            self.open_file_handles.lock_recover().insert(
                handle,
                OpenFileHandleState {
                    ino: attr.ino.0,
                    opened_path,
                    writable: true,
                    append: flags & libc::O_APPEND != 0,
                },
            );
        }
        reply.created(
            &self.attr_ttl(TTL),
            &self.presented_to(req, attr),
            Generation(0),
            FileHandle(handle),
            self.open_cache_flags(ino),
        );
    }

    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let _answering = self.activity.begin();
        note_request("write");
        let _timing = OpTimer::new("write");
        if let Err(e) = self.check_open_handle_access(ino.0, fh, Access::Write) {
            reply.error(to_errno(e));
            return;
        }
        match self.write_handle(ino.0, fh.0, offset, data) {
            Ok(written) => reply.written(written),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    fn flush(
        &self,
        req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        let _answering = self.activity.begin();
        note_request("flush");
        let _timing = OpTimer::new("flush");
        let _writer = self.note_writer(req.pid());
        match self.flush_inode(ino.0) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    fn fsync(
        &self,
        req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let _answering = self.activity.begin();
        let _writer = self.note_writer(req.pid());
        match self.fsync_inode(ino.0) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    /// `fsync` of a directory: the names in it survive a crash. Left unimplemented, the kernel
    /// takes the `ENOSYS` as success for every later directory sync while a rename or create
    /// stays in memory until the persister's next write.
    fn fsyncdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let _answering = self.activity.begin();
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        match self.sync_namespace() {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let _answering = self.activity.begin();
        note_request("release");
        let _timing = OpTimer::new("release");
        if let Some(passthrough) = &self.passthrough {
            passthrough.release(fh.0);
        }
        // The kernel ignores release errors (close already returned flush's
        // result); the error is still reported for tracing.
        match self.release_handle(ino.0, fh.0) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    fn destroy(&mut self) {
        self.persist_all_buffers();
        // What the window had not synced yet is on disk once the mount is gone.
        let _ = self.sync_namespace();
    }

    fn setattr(
        &self,
        req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let _answering = self.activity.begin();
        let _writer = if size.is_some() || mode.is_some() {
            match self.admit_writer(
                req.pid(),
                Some(ino.0),
                fs_protocol::file_action::verbs::WRITE,
            ) {
                Ok(writer) => Some(writer),
                Err(errno) => {
                    reply.error(to_errno(errno));
                    return;
                }
            }
        } else {
            None
        };
        // Truncation is a write; everything else here is ignored anyway, but a `setattr` on
        // something outside the view must not describe it, so the visibility check runs regardless.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if (size.is_some() || mode.is_some())
            && let Err(e) = self.check_access(ino.0, Access::Write)
        {
            reply.error(to_errno(e));
            return;
        }
        // A mode is the record's, as on any Linux file system: a checkout's scripts stay
        // executable. Setuid never replicates, and owner and times are ignored.
        if let Some(mode) = mode
            && let Err(errno) = self.set_mode(ino.0, mode)
        {
            reply.error(to_errno(errno));
            return;
        }
        // Truncation never reads the bytes it drops: a stage only changes its length, and an
        // unopened file commits the kept prefix.
        if let Some(new_size) = size
            && let Err(errno) = self.truncate_inode(ino.0, new_size)
        {
            reply.error(to_errno(errno));
            return;
        }

        match self.presented_attr(ino.0) {
            Some((attr, ttl)) => reply.attr(&self.attr_ttl(ttl), &self.presented_to(req, attr)),
            None => reply.error(to_errno(ENOENT)),
        }
    }

    fn mkdir(
        &self,
        req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let _answering = self.activity.begin();
        note_request("mkdir");
        let _timing = OpTimer::new("mkdir");
        let _writer =
            match self.admit_writer(req.pid(), None, fs_protocol::file_action::verbs::NEW_FOLDER) {
                Ok(writer) => writer,
                Err(errno) => {
                    reply.error(to_errno(errno));
                    return;
                }
            };
        let name = name.as_bytes();
        if let Err(e) = self.check_new_name(parent.0, name) {
            reply.error(to_errno(e));
            return;
        }
        let ino = match self.create_empty_dir(parent.0, name) {
            Ok(created) => created,
            Err(error) => {
                reply.error(to_errno(error));
                return;
            }
        };
        if let Err(error) = self.set_mode(ino, mode & !umask) {
            reply.error(to_errno(error));
            return;
        }
        let attr = self.ns().getattr(ino);
        match attr {
            Some(attr) => {
                self.publish();
                reply.entry_with_ttls(
                    &self.attr_ttl(TTL),
                    &TTL,
                    &self.presented_to(req, attr),
                    Generation(0),
                );
            }
            None => reply.error(to_errno(EIO)),
        }
    }

    fn unlink(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let _answering = self.activity.begin();
        let _writer = match self.admit_named(
            req.pid(),
            parent.0,
            name.as_bytes(),
            fs_protocol::file_action::verbs::TRASH,
        ) {
            Ok(writer) => writer,
            Err(errno) => {
                reply.error(to_errno(errno));
                return;
            }
        };
        self.remove_entry(parent, name, false, reply);
    }

    fn rmdir(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let _answering = self.activity.begin();
        let _writer = match self.admit_named(
            req.pid(),
            parent.0,
            name.as_bytes(),
            fs_protocol::file_action::verbs::TRASH,
        ) {
            Ok(writer) => writer,
            Err(errno) => {
                reply.error(to_errno(errno));
                return;
            }
        };
        self.remove_entry(parent, name, true, reply);
    }

    fn rename(
        &self,
        req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let _answering = self.activity.begin();
        let _writer = match self.admit_named(
            req.pid(),
            parent.0,
            name.as_bytes(),
            fs_protocol::file_action::verbs::MOVE,
        ) {
            Ok(writer) => writer,
            Err(errno) => {
                reply.error(to_errno(errno));
                return;
            }
        };
        let name = name.as_bytes();
        let newname = newname.as_bytes();
        #[cfg(target_os = "linux")]
        let unsupported_flags = flags.difference(RenameFlags::RENAME_NOREPLACE);
        #[cfg(not(target_os = "linux"))]
        let unsupported_flags = flags;
        if !unsupported_flags.is_empty() {
            reply.error(to_errno(libc::ENOTSUP));
            return;
        }
        #[cfg(target_os = "linux")]
        if flags.contains(RenameFlags::RENAME_NOREPLACE)
            && self.ns().lookup(newparent.0, newname).is_some()
        {
            reply.error(to_errno(EEXIST));
            return;
        }
        // Both ends: the source a writable name in the view, the destination a writable new name
        // in a directory in the view.
        if let Err(e) = self
            .check_existing_name(parent.0, name)
            .and_then(|()| self.check_new_name(newparent.0, newname))
        {
            reply.error(to_errno(e));
            return;
        }
        match self.rename_named(parent.0, name, newparent.0, newname) {
            Ok(()) => {
                self.publish();
                reply.ok();
            }
            Err(error) => reply.error(to_errno(error)),
        }
    }

    fn setxattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        name: &OsStr,
        value: &[u8],
        _flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        let _answering = self.activity.begin();
        if position != 0 {
            reply.error(to_errno(EINVAL));
            return;
        }
        // The daemon's own change signal: it changes nothing, and is only
        // ever raised by the daemon, which alone knows the token.
        if name.to_str() == Some(tilo_files::native::CHANGE_SIGNAL_XATTR) {
            match &self.change_signal {
                Some(signal) if signal.accepts(value) => reply.ok(),
                _ => reply.error(to_errno(libc::EPERM)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::AVAILABILITY_CONTROL_XATTR) {
            let result = match value {
                b"retry" => self.retry_transfer(ino.0),
                b"evict" => self.free_local_space(ino.0, EvictionMode::Safe),
                b"evict-destructive" => {
                    self.free_local_space(ino.0, EvictionMode::DestructiveOverride)
                }
                _ => {
                    reply.error(to_errno(EINVAL));
                    return;
                }
            };
            match result {
                Ok(()) => reply.ok(),
                Err(errno) => reply.error(to_errno(errno)),
            }
            return;
        }
        if name.to_str() != Some(fs_protocol::PIN_XATTR) {
            reply.error(to_errno(libc::ENOTSUP));
            return;
        }
        let policy = match value {
            b"pinned" => AvailabilityPolicy::Pinned,
            b"automatic" => AvailabilityPolicy::Automatic,
            _ => {
                reply.error(to_errno(EINVAL));
                return;
            }
        };
        match self.set_pin_policy(ino.0, policy) {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(to_errno(errno)),
        }
    }

    /// Serve the `user.tilo.sync` extended attribute: the entry's hydration state
    /// (`local`/`projected`/`downloading`). Every other attribute name reports absent (`ENODATA`).
    #[allow(
        clippy::too_many_lines,
        reason = "one dispatch point owns the bounded collaborative extended-attribute protocol"
    )]
    fn getxattr(&self, _req: &Request, ino: INodeNo, name: &OsStr, size: u32, reply: ReplyXattr) {
        let _answering = self.activity.begin();
        // Outside the view the inode doesn't exist — checked before the
        // attribute name, so the sync state never reports on a hidden file.
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        if name.to_str() == Some(tilo_files::native::CHANGE_SIGNAL_XATTR) {
            match self
                .change_signal
                .as_ref()
                .and_then(|signal| signal.answer())
            {
                Some(live) => reply_xattr_bytes(reply, live, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::THUMB_XATTR) {
            match self.thumbnail_for(ino.0) {
                // The kernel caps an xattr value at 64 KiB: report an oversize preview as absent so
                // the client falls back.
                Some(png) if png.len() <= crate::thumbs::MAX_XATTR_THUMB => {
                    reply_xattr_bytes(reply, &png, size);
                }
                _ => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::CONTENT_HASH_XATTR) {
            let hash = self.ns().hash_for(ino.0);
            match hash {
                Some(hash) => reply_xattr_bytes(reply, hash.as_bytes(), size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(collab_types::document::DOCUMENT_ID_XATTR) {
            let document_id = self.ns().document_id_for(ino.0);
            match document_id {
                Some(document_id) => reply_xattr_bytes(reply, document_id.0.as_bytes(), size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::METADATA_XATTR) {
            match self.metadata_projection_for(ino.0) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::projection::DOCUMENT_INDEX_V2_XATTR) {
            if ino.0 != crate::namespace::ROOT_INO {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            // Raw-offset v2 is safe only where all records are observable.
            // ACL-constrained mounts use v3's private opaque continuations.
            if self.enforcement.is_some() {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.document_index_manifest_v2() {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::projection::DOCUMENT_INDEX_V3_XATTR) {
            if ino.0 != crate::namespace::ROOT_INO || self.enforcement.is_none() {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.document_index_manifest_v3() {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if let Some((revision, cursor)) = name
            .to_str()
            .and_then(|name| name.strip_prefix("user.tilo.record-index.v3."))
            .and_then(|page| page.split_once('.'))
        {
            if ino.0 != crate::namespace::ROOT_INO || self.enforcement.is_none() {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.document_index_page_v3(revision, cursor) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if let Some(document_id) = name
            .to_str()
            .and_then(|name| {
                name.strip_prefix(fs_protocol::projection::DOCUMENT_INDEX_BY_ID_XATTR_PREFIX)
            })
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .map(fs_protocol::FileDocumentId)
        {
            if ino.0 != crate::namespace::ROOT_INO {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.document_index_by_id_v2(document_id) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if let Some((revision, offset)) = name
            .to_str()
            .and_then(|name| name.strip_prefix("user.tilo.record-index.v2."))
            .and_then(|page| page.rsplit_once('.'))
            .and_then(|(revision, offset)| {
                offset
                    .parse::<usize>()
                    .ok()
                    .map(|offset| (revision, offset))
            })
        {
            if ino.0 != crate::namespace::ROOT_INO || self.enforcement.is_some() {
                reply.error(to_errno(libc::ENODATA));
                return;
            }
            match self.document_index_page_v2(revision, offset) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::AVAILABILITY_XATTR) {
            match self.availability_for(ino.0) {
                Some(bytes) => reply_xattr_bytes(reply, &bytes, size),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() == Some(fs_protocol::PIN_XATTR) {
            let Some(registry) = &self.availability else {
                reply.error(to_errno(libc::ENODATA));
                return;
            };
            // The policy `user.tilo.availability` reports: the file's own, or a folder's above it. The
            // registry's row for the file alone would say nothing about a file created in a pinned
            // folder until a restart registered it.
            let ancestry = document_ancestry(&mut self.ns(), ino.0);
            let policy =
                ancestry.map(|ancestry| inherited_policy(&registry.lock_recover(), &ancestry));
            match policy {
                Some(policy) => reply_xattr_bytes(
                    reply,
                    match policy {
                        AvailabilityPolicy::Pinned => b"pinned",
                        AvailabilityPolicy::Automatic => b"automatic",
                    },
                    size,
                ),
                None => reply.error(to_errno(libc::ENODATA)),
            }
            return;
        }
        if name.to_str() != Some(fs_protocol::SYNC_XATTR) {
            reply.error(to_errno(libc::ENODATA));
            return;
        }
        let Some(state) = self.sync_state_for(ino.0) else {
            reply.error(to_errno(ENOENT));
            return;
        };
        reply_xattr_bytes(reply, state.as_str().as_bytes(), size);
    }

    /// List the extended attributes on `ino`: just `user.tilo.sync` (NUL terminated), so
    /// `getfattr` and `listxattr` discover it.
    fn listxattr(&self, _req: &Request, ino: INodeNo, size: u32, reply: ReplyXattr) {
        let _answering = self.activity.begin();
        if !self.ino_in_view(ino.0) {
            reply.error(to_errno(ENOENT));
            return;
        }
        let (exists, name) = {
            let ns = self.ns();
            let exists = ns.getattr(ino.0).is_some();
            let name = ns
                .path_for(ino.0)
                .and_then(|p| p.rsplit('/').next().map(std::string::ToString::to_string));
            drop(ns);
            (exists, name)
        };
        if !exists {
            reply.error(to_errno(ENOENT));
            return;
        }
        let mut buf = Vec::with_capacity(fs_protocol::SYNC_XATTR.len().saturating_add(1));
        buf.extend_from_slice(fs_protocol::SYNC_XATTR.as_bytes());
        buf.push(0);
        // Cheap extension check, no generation: a `getxattr` of it may still be `ENODATA` while the
        // content is not local yet.
        let previewable = self.thumbs.is_some()
            && name.as_deref().is_some_and(|n| {
                thumbnail::category_for_path(std::path::Path::new(n)).has_preview()
            });
        if previewable {
            buf.extend_from_slice(fs_protocol::THUMB_XATTR.as_bytes());
            buf.push(0);
        }
        if self.ns().hash_for(ino.0).is_some() {
            buf.extend_from_slice(fs_protocol::CONTENT_HASH_XATTR.as_bytes());
            buf.push(0);
        }
        if self.ns().document_id_for(ino.0).is_some() {
            buf.extend_from_slice(collab_types::document::DOCUMENT_ID_XATTR.as_bytes());
            buf.push(0);
        }
        if self.metadata_projection_for(ino.0).is_some() {
            buf.extend_from_slice(fs_protocol::METADATA_XATTR.as_bytes());
            buf.push(0);
        }
        if ino.0 == crate::namespace::ROOT_INO {
            buf.extend_from_slice(fs_protocol::projection::DOCUMENT_INDEX_V2_XATTR.as_bytes());
            buf.push(0);
        }
        if self.availability.is_some() {
            buf.extend_from_slice(fs_protocol::AVAILABILITY_XATTR.as_bytes());
            buf.push(0);
            buf.extend_from_slice(fs_protocol::PIN_XATTR.as_bytes());
            buf.push(0);
            buf.extend_from_slice(fs_protocol::AVAILABILITY_CONTROL_XATTR.as_bytes());
            buf.push(0);
        }

        if size == 0 {
            reply.size(u32::try_from(buf.len()).unwrap_or(u32::MAX));
        } else if (size as usize) < buf.len() {
            reply.error(to_errno(libc::ERANGE));
        } else {
            reply.data(&buf);
        }
    }
}
