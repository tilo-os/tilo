//! Authorize a FUSE read and serve the bytes from the stage, store, or peer.

use std::{io, sync::Arc};

use collab_types::WorkspaceId;
#[cfg(not(target_os = "android"))]
use fuser::ReplyData;
use iroh_blobs::Hash;

use crate::node::FileType;

#[cfg(not(target_os = "android"))]
use super::to_errno;
use super::{Access, EIO, EISDIR, ENOENT, QuickFS, StagedRead, unavailable};

/// How a FUSE `read` is answered, decided by `QuickFS::plan_read` before any reply is
/// sent.
pub(super) enum ReadPlan {
    Error(i32),
    Buffered(BufferedRead),
    Local(Vec<u8>),
    Remote(Hash, Option<WorkspaceId>),
}

pub(super) enum BufferedRead {
    Staged(io::Result<StagedRead>),
}

impl QuickFS {
    pub(super) fn read_base_blocking(
        &self,
        base: Hash,
        offset: u64,
        len: u64,
        workspace: Option<WorkspaceId>,
    ) -> io::Result<Vec<u8>> {
        let hydrator = &self.hydrator;
        let bytes = hydrator
            .block_on(async {
                match hydrator
                    .read_local_scoped(base, offset, len, workspace)
                    .await
                {
                    Some(bytes) => Ok(bytes),
                    None => hydrator.read_scoped(base, offset, len, workspace).await,
                }
            })
            .map_err(|error| io::Error::other(error.to_string()))?;
        if bytes.len() as u64 != len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "committed content is shorter than the file it backs",
            ));
        }
        Ok(bytes)
    }

    /// Everything a FUSE `read` decides before it replies: authorization, then the write
    /// stage or the committed content. The inode is read under one namespace lock, so its
    /// liveness, path, kind and content hash come back together.
    pub(super) fn plan_read(&self, ino: u64, fh: u64, offset: u64, size: u32) -> ReadPlan {
        let open_handle = self.open_handle(ino, fh);
        let Some(view) = self.inode_view(ino, open_handle.is_some()) else {
            return ReadPlan::Error(ENOENT);
        };
        // Live inodes use their current path. An unlinked/replaced inode has
        // no path, so only the matching kernel handle may continue, with the
        // access path captured when it was opened.
        let access = if view.live {
            self.check_view_access(&view, Access::Read)
        } else if let Some(handle) = &open_handle {
            self.check_path(&handle.opened_path, Access::Read)
        } else {
            Err(ENOENT)
        };
        if let Err(e) = access {
            return ReadPlan::Error(e);
        }
        let workspace = if view.live {
            view.path.as_deref()
        } else {
            open_handle
                .as_ref()
                .map(|handle| handle.opened_path.as_str())
        }
        .and_then(|path| self.hydrator.scoped_workspace(path));
        if let Some(stage) = self.stage(ino) {
            if stage.lock().workspace() != workspace {
                return ReadPlan::Error(ENOENT);
            }
            let read = self.read_stage(&stage, offset, u64::from(size));
            return ReadPlan::Buffered(BufferedRead::Staged(read));
        }
        if view.attr.kind != FileType::RegularFile {
            return ReadPlan::Error(EISDIR);
        }
        let Some(hash) = view.hash else {
            return ReadPlan::Local(Vec::new());
        };
        let size = u64::from(size);
        match self.hydrator.block_on(
            self.hydrator
                .read_local_scoped(hash, offset, size, workspace),
        ) {
            Some(data) => ReadPlan::Local(data),
            None => ReadPlan::Remote(hash, workspace),
        }
    }

    /// Answer a `read` whose bytes must come from a peer. The read blocks its caller as
    /// POSIX expects, but no FUSE worker waits on the network, so a slow peer stalls only
    /// the readers of that file.
    #[cfg(not(target_os = "android"))]
    pub(super) fn reply_remote(
        &self,
        ino: u64,
        hash: Hash,
        workspace: Option<WorkspaceId>,
        range: (u64, u64),
        nonblocking: bool,
        reply: ReplyData,
    ) {
        let (offset, size) = range;
        let hydrator = Arc::clone(&self.hydrator);
        let rt = hydrator.runtime_handle.clone();
        let note = self.unavailable_note(ino, Some(hash));
        if nonblocking {
            // A non-blocking reader gets EAGAIN at once; the range keeps
            // hydrating so a retry finds it local.
            rt.spawn(async move {
                match hydrator.read_scoped(hash, offset, size, workspace).await {
                    Ok(_) => {
                        if let Some(note) = note {
                            note.arrived().await;
                        }
                    }
                    Err(error) => eprintln!("content hydration failed for {hash}: {error:#}"),
                }
            });
            reply.error(to_errno(libc::EAGAIN));
            return;
        }
        // The kernel holds the page locked while the read waits, so the read fails at its
        // bound rather than waiting on a peer that went quiet; the file's record says it
        // is unavailable until bytes arrive, and stored once it is whole.
        let answering = self.activity.begin();
        rt.spawn(async move {
            let _answering = answering;
            let read = async move { hydrator.read_scoped(hash, offset, size, workspace).await };
            let bound = crate::hydration::BLOCKED_READ_BOUND;
            match unavailable::noted_bounded_read(bound, read, note).await {
                Ok(data) => reply.data(&data),
                Err(error) => {
                    eprintln!("content hydration failed for {hash}: {error:#}");
                    reply.error(to_errno(EIO));
                }
            }
        });
    }

    /// Where a blocked read of `ino`'s bytes notes whether they arrived: its availability
    /// record, when this mount keeps them. When it is the file's current content, the
    /// record says the file is stored once every byte of it is local.
    fn unavailable_note(
        &self,
        ino: u64,
        hash: Option<Hash>,
    ) -> Option<unavailable::UnavailableNote> {
        let registry = Arc::clone(self.availability.as_ref()?);
        let (document_id, current) = {
            let ns = self.ns();
            (ns.document_id_for(ino)?, ns.hash_for(ino))
        };
        let content =
            hash.filter(|hash| current == Some(*hash))
                .map(|hash| unavailable::NotedContent {
                    hydrator: Arc::clone(&self.hydrator),
                    hash,
                    stored: Arc::clone(&self.stored_writes),
                });
        Some(unavailable::UnavailableNote {
            registry,
            document_id,
            content,
        })
    }

    /// Answer a FUSE `read` that [`Self::plan_read`] sent to a buffer. Unwritten ranges a
    /// peer must supply are hydrated on the runtime, like an unbuffered read.
    #[cfg(not(target_os = "android"))]
    pub(super) fn reply_buffered(&self, ino: u64, buffered: BufferedRead, reply: ReplyData) {
        match buffered {
            BufferedRead::Staged(Ok(StagedRead::Ready(bytes))) => reply.data(&bytes),
            BufferedRead::Staged(Ok(StagedRead::Missing(missing))) => {
                let hydrator = Arc::clone(&self.hydrator);
                let rt = hydrator.runtime_handle.clone();
                let note = self.unavailable_note(ino, None);
                let answering = self.activity.begin();
                rt.spawn(async move {
                    let _answering = answering;
                    let read = async move { Ok(missing.finish(&hydrator).await?) };
                    let bound = crate::hydration::BLOCKED_READ_BOUND;
                    match unavailable::noted_bounded_read(bound, read, note).await {
                        Ok(bytes) => reply.data(&bytes),
                        Err(error) => {
                            eprintln!("content hydration for a staged read failed: {error:#}");
                            reply.error(to_errno(EIO));
                        }
                    }
                });
            }
            BufferedRead::Staged(Err(error)) => {
                eprintln!("read of the write stage of inode {ino} failed: {error}");
                reply.error(to_errno(EIO));
            }
        }
    }
}
