//! Independent live directory enumeration; no total-row snapshots or child grants.
use super::{
    DirectoryGrant, EntryName, FsError, FsRight, FsSession, MetadataExt, Mutex, describe, io_error,
    lock,
};
use crate::directory_backend::{DirectoryStreamPage, DirectoryStreamRow};
const PAGE_ROWS: usize = 128;
struct State {
    iterator: cap_std::fs::ReadDir,
    sequence: u64,
    done: bool,
    failed: bool,
}
/// Opaque cursor retaining the actual held directory and authenticated session.
pub struct FsDirectoryCursor {
    session: FsSession,
    root: DirectoryGrant,
    state: Mutex<State>,
}
impl FsSession {
    /// Opens an independent cursor from the actual directory descriptor.
    /// # Errors
    /// Refuses non-enumerable or revoked scope and native directory IO.
    pub fn open_directory_cursor(
        &self,
        root: &DirectoryGrant,
    ) -> Result<FsDirectoryCursor, FsError> {
        let table = self.call()?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        let descriptor = Self::directory(&root.lease)?;
        // cap-std entries opens '.' on a new OFD; it does not dup/shared-seek.
        let iterator = descriptor.entries().map_err(io_error)?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        Ok(FsDirectoryCursor {
            session: self.clone(),
            root: root.clone(),
            state: Mutex::new(State {
                iterator,
                sequence: 0,
                done: false,
                failed: false,
            }),
        })
    }
}
impl FsDirectoryCursor {
    /// # Errors
    /// Refuses revoked or closed session/ancestor authority.
    pub fn check(&self) -> Result<(), FsError> {
        self.session.check_directory(&self.root)
    }
    /// At most128 names, each accompanied by the same nofollow stat observation.
    /// Mutation can change enumeration order/content; this is not a snapshot.
    /// # Errors
    /// Refuses stale sequence, enumeration IO, closed session or revoked lineage.
    pub fn page(&self, expected: u64) -> Result<DirectoryStreamPage, FsError> {
        let table = self.session.call()?;
        self.session
            .validate(&table, &self.root.lease, Some(FsRight::Enumerate))?;
        let descriptor = FsSession::directory(&self.root.lease)?;
        let mut state = lock(&self.state)?;
        if state.failed || expected != state.sequence {
            return Err(FsError::StalePage);
        }
        let next_sequence = state
            .sequence
            .checked_add(1)
            .ok_or(FsError::LimitExceeded)?;
        let mut entries = Vec::with_capacity(PAGE_ROWS);
        // Bound examined rows as well as output rows: concurrent deletion must
        // not turn one RPC into an unbounded scan of vanished names.
        for _ in 0..PAGE_ROWS {
            if state.done {
                break;
            }
            self.session
                .validate(&table, &self.root.lease, Some(FsRight::Enumerate))?;
            let row = state.iterator.next();
            self.session
                .validate(&table, &self.root.lease, Some(FsRight::Enumerate))?;
            let Some(row) = row else {
                state.done = true;
                break;
            };
            let row = match row {
                Ok(row) => row,
                Err(error) => {
                    state.failed = true;
                    return Err(io_error(error));
                }
            };
            let name = EntryName::new(row.file_name())?;
            self.session
                .validate(&table, &self.root.lease, Some(FsRight::Enumerate))?;
            let metadata = descriptor.symlink_metadata(name.as_os_str());
            self.session
                .validate(&table, &self.root.lease, Some(FsRight::Enumerate))?;
            let stat = match metadata {
                Ok(metadata) => Some(crate::directory_backend::DirectoryStat {
                    metadata: describe(&metadata),
                    identity: [metadata.dev(), metadata.ino()],
                    file_type: metadata.mode() & super::mode_bits(libc::S_IFMT),
                    execute_bits: (metadata.mode() & 0o111) as u16,
                }),
                // This live enumeration promises no snapshot. A name removed
                // before its checked observation is absent, not inaccessible.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                // Permission and all other native failures remain visible rows.
                Err(_) => None,
            };
            entries.push(DirectoryStreamRow { name, stat });
        }
        self.session
            .validate(&table, &self.root.lease, Some(FsRight::Enumerate))?;
        state.sequence = next_sequence;
        Ok(DirectoryStreamPage {
            entries,
            next_sequence,
            done: state.done,
        })
    }
}
