pub use tilo_fs_client::{
    DirEntry, EntryKind, FileHandle, FsClient, MAX_READ_CHUNK, MAX_WRITE_CHUNK, Metadata, OpenMode,
    WorkspaceFsClient, blobs_capnp, fs_capnp, native_reply, raw, storage, try_connect,
};
#[cfg(unix)]
pub use tilo_fs_client::{
    EntryName, cloud, connect, connect_stream, directory_client, directory_connection,
    selected_client,
};

#[cfg(unix)]
pub mod grants;
#[cfg(unix)]
mod native;
#[cfg(unix)]
pub use grants::{
    DirectoryGrant, DirectoryPage, FileGrant, FsEntry, FsError, FsOwner, FsRight, GrantedEntry,
    PageCursor,
};
#[cfg(unix)]
pub use native::{
    FsAppendHandle, FsDirectoryCursor, FsSession, NativeFileAccess, NativeGrantAuthority,
    NativeHistoryReadObservation, NativeHistoryReadSubject, SelectedReplacementAuthority,
};
#[cfg(unix)]
pub mod blobs;
#[cfg(unix)]
pub mod directory;
#[cfg(unix)]
pub mod directory_backend;
#[cfg(unix)]
pub mod external_history;
#[cfg(unix)]
pub mod operations;
#[cfg(unix)]
pub mod recovery;
#[cfg(unix)]
pub mod selected_file;
#[cfg(unix)]
pub mod workspace_server;
