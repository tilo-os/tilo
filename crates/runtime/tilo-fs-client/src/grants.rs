pub use crate::{EntryName, InvalidEntryName};

/// Authenticated workspace/application identity supplied by the trusted host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsOwner {
    pub workspace: String,
    pub app_id: String,
}

/// Rights recorded at admission. In-place byte writes require explicit `Write`;
/// named mutations are reserved for operation providers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsRight {
    Enumerate,
    Read,
    /// In-place regular-file bytes through an already-writable held descriptor.
    Write,
    Create,
    Rename,
    /// History-preserving atomic replacement of an exactly held regular-file
    /// victim. Admission must derive a current signed edit/Author scope; this
    /// right never admits raw Remove or permanent deletion.
    Replace,
    Copy,
    Move,
    Trash,
    Purge,
}
