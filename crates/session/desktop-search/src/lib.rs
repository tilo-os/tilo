//! Desktop search shared by every Tilo desktop: the native shell's palette
//! and the portable desktop surface rank, group and present results with
//! this one implementation.
//!
//! - [`kind`]: the result kinds groups and chips are named by.
//! - [`rank`]: the shared ranking — per-provider field scores on the shared
//!   ranker, the cross-provider merge, and the kind grouping.
//! - [`model`] and [`providers`]: the portable query model and the sources a
//!   desktop can search without a compositor (applications from the manifest
//!   registry, open windows, application actions and content, recent
//!   choices, host commands).
//! - [`palette`]: the toolkit's command palette over those sources.
//! - [`availability`]: the manifest registry's apps a desktop cannot run,
//!   with the reason shown for each.

pub mod availability;
pub mod kind;
pub mod model;
pub mod palette;
pub mod providers;
pub mod rank;

pub use kind::ResourceKind;
pub use model::{ActionTarget, Hit, Source, Target, search};
pub use palette::{DesktopSearch, SearchData};
