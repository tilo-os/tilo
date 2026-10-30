//! Desktop search shared by every Tilo desktop: the native shell's palette
//! and the portable desktop surface rank, group and present results with
//! this one implementation.
//!
//! - [`rank`]: per-provider field scores on the shared ranker; the
//!   cross-provider merge and kind grouping are [`ui::search::system`]'s.
//! - [`model`] and [`providers`]: the portable query model and the sources a
//!   desktop can search without a compositor (applications from the manifest
//!   registry, open windows, application actions and content, recent
//!   choices, host commands).
//! - [`palette`]: the toolkit's command palette over those sources.
//! - [`window`]: one window's own search, over what it publishes.
//! - [`lazy`]: search whose code a program may fetch on first use.
//! - [`answers`]: what a query that is not a name answers: a sum, a unit
//!   conversion, an address, the web searches it can be.
//! - `content_index` and `files_index` (feature `indexes`): the session's
//!   on-disk indexes of what apps publish and of the person's files.
//! - [`availability`]: what the manifest registry knows of each app: where
//!   it runs, its category and the words people know it by.
//! - [`usage`] and [`preferences`]: what the person chose before, and the
//!   sources they switched off; the host keeps both.
//! - Features `settings` and `catalogue` (native): Settings' pages and the
//!   apps a person can get, from their own crates' indexes.

pub mod answers;
pub mod availability;
#[cfg(feature = "indexes")]
pub mod content_index;
#[cfg(feature = "indexes")]
pub mod files_index;
mod i18n;
#[cfg(feature = "indexes")]
pub mod indexed;
pub mod lazy;
pub mod model;
pub mod palette;
pub mod preferences;
pub mod providers;
pub mod rank;
pub mod usage;
pub mod window;

pub use lazy::{
    DESKTOP_SEARCH, DesktopSearchApi, LazyDesktopSearch, MakeDesktopSearch, builtin_desktop_search,
};
pub use model::{
    ActionTarget, DESKTOP_APP, Hit, InvokeTarget, OPEN_ACTION, OPEN_ANCHOR, Source, Target, search,
};
pub use palette::{DesktopSearch, SearchData};
pub use preferences::SearchPreferences;
pub use ui::search::system::ResourceKind;
pub use usage::Usage;
