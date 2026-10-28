//! The one catalogue of apps a person can get, shared by the Store and
//! desktop search.
//!
//! Two sources make it up:
//!
//! - **Tilo packages** from the configured registries ([`tilo`]): the
//!   `app list --json` rows, joined with the signed manifest metadata of the
//!   Tilo fleet ([`fleet`]) so an app that is not on this device is still
//!   known by its tag line, other names, keywords, the documents it creates
//!   and the actions it offers. A Tilo app has no install step: opening one
//!   downloads it if it is absent.
//! - **Flathub** from Flatpak's cached `AppStream` metadata ([`appstream`]):
//!   the file Flatpak keeps under each installation, with the icons it
//!   caches beside it. A Flathub app is installed explicitly.
//!
//! On top sits the [`BlockList`]: apps blocked by the person, a workspace
//! or the device's administrator appear in neither search nor the Store.
//!
//! # Reading it
//!
//! ```no_run
//! use app_catalogue::{BlockList, Catalogue, Installed, appstream, tilo};
//!
//! // On a worker thread: every read here touches the disk.
//! let tilo_rows: Vec<registry_types::catalogue::CatalogueApp> = Vec::new();
//! let catalogue = Catalogue::from_sources(
//!     tilo_rows.iter().map(tilo::entry).collect(),
//!     appstream::load_cached("en"),
//! );
//! let blocks = BlockList::load();
//! let installed = Installed::from_ids(["so.tilo.files"]);
//! for entry in catalogue.offered(&installed, &blocks) {
//!     println!("{} ({:?})", entry.name, entry.source);
//! }
//! ```
//!
//! # Refreshing it
//!
//! Nothing here goes to the network. The registries' listing is refreshed
//! by `app list` (which the shell runs in the background and caches), and
//! Flathub's `AppStream` by Flatpak itself, which the hourly
//! `tilo-app-update` run asks for. A reader compares
//! [`appstream::cache_stamp`] to learn whether Flatpak refreshed since.
//!
//! # Stability
//!
//! The Store and the shell build on [`CatalogueEntry`], [`Catalogue`],
//! [`Installed`], [`BlockList`] and the loaders in [`appstream`] and
//! [`tilo`]. Fields are only ever added to these types, never renamed or
//! removed, so both sides can move independently.

pub mod appstream;
/// The block list, which apps reach through the registry vocabulary.
pub use registry_types::block;
pub mod catalogue;
pub mod entry;
pub mod flathub;
pub mod fleet;
pub mod tilo;

pub use block::{Block, BlockList, Blocker, UnblockError};
pub use catalogue::{Catalogue, Installed};
pub use entry::{CatalogueAction, CatalogueEntry, Installation, ResourceType, Source};
