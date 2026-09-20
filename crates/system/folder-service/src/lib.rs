//! Framework-owned connected-folder and durable-copy implementation.
//!
//! Native authority and handles remain private. Hosts compose [`startup::dispatch_early`]
//! before UI, runtime, or threads when supporting native self-reexecution.
//!
//! ```
//! use std::path::PathBuf;
//! use tilo_folder_service::{copy::{MigrationJob, MigrationStore}, review::DirectoryIdentity, scan::FolderScan};
//! fn describe(source: PathBuf, destination: PathBuf, journal: PathBuf) -> Result<MigrationJob, String> {
//!     MigrationJob::fresh("review-fixture", source, destination, journal)
//! }
//! let _ = describe as fn(PathBuf, PathBuf, PathBuf) -> Result<MigrationJob, String>;
//! ```
//! ```compile_fail
//! use tilo_folder_service::connection::transaction::native::NativeRestore;
//! ```
//! ```compile_fail
//! use tilo_folder_service::migration::PinnedDirectory;
//! ```
//! ```compile_fail
//! use tilo_folder_service::review::DirectoryIdentity;
//! let _ = DirectoryIdentity { volume: vec![], inode: 0 };
//! ```

mod connection;
mod destination_security;
mod migration;
mod migration_durability;
mod migration_journal;
mod migration_journal_log;
#[cfg(any(target_os = "linux", test))]
mod migration_native;
#[cfg(any(target_os = "linux", test))]
mod migration_native_journal;
mod migration_review;
mod migration_source;
mod migration_store;
mod migration_worker;
mod nofollow;

pub mod connected;
pub mod copy;
pub mod review;
pub mod scan;
pub mod session;
pub mod startup;
#[cfg(feature = "test-support")]
pub mod test_support;
