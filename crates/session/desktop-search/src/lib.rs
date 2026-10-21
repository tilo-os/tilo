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
//! - [`availability`]: what the manifest registry knows of each app: where
//!   it runs, its category and the words people know it by.

pub mod availability;
pub mod model;
pub mod palette;
pub mod providers;
pub mod rank;

pub use model::{ActionTarget, Hit, Source, Target, search};
pub use palette::{DesktopSearch, SearchData};
pub use ui::search::system::ResourceKind;
