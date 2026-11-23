//! Handle-opening checks shared with the source readers; they live in
//! `migration-sources`.

pub use tilo_migrate::pinned::nofollow::{
    WindowsCopyBlock, metadata_nofollow, open_directory_nofollow, reject_windows_placeholder,
    same_directory_identity, windows_copy_block,
};
