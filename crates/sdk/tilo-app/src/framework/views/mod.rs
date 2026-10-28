//! Views: the mechanism behind an app's designed views and its search.
//!
//! A view is a query over the app's resources plus a layout. The app's
//! designed views (a library, a page per album) are crafted and fixed; this
//! module is what they stand on, never something people configure:
//!
//! - **Resource types** ([`KindDecl`]): each type's fields, the role a field
//!   plays where an item is shown ([`FieldRole`]), which fields typed text
//!   finds and which narrow by a chip ([`FilterKind`]), and how its items are
//!   presented in a list of results ([`Form`]). Every place an item appears
//!   renders through the app's presentation of its type, so a search result
//!   is the same song as in the library.
//! - **A view stack per window** ([`ViewStack`]): back and forward always
//!   work, and returning to a view restores where the person was in it
//!   ([`ViewPlace`]).
//! - **Search is a view.** Typing into the window bar's search field turns
//!   the window's view into a query view of the app's resources
//!   ([`QuerySpec`]): results grouped by type, chips under the field from the
//!   filterable fields ([`facet_offers`], [`facet_bar`]), typed tokens
//!   (`artist:ada`) mapped onto the same chips, and the app's actions that
//!   match in a trailing "In this app" group as their real controls
//!   ([`action_hits`], [`actions_group`]). Escape or Back returns to the view
//!   before, exactly.
//! - **The query** ([`QueryEngine`]): local items answered from an index in
//!   the frame of the keystroke; remote sources ([`RemoteSource`]) streamed
//!   into their own sections below, with cancellation, a cache by query,
//!   reuse of an earlier answer while a refined query is on the way, and rows
//!   that never move under the person's pointer or keyboard. An app whose
//!   search already has entries hands those over ([`Item::from_entry`]): one
//!   search model feeds the platform search and the app's query view, and a
//!   match inside an entry's prose is a hit at its passage.
//! - **Keeping** ([`KeptViews`]): any query view can be kept, and then sits
//!   beside the designed views as the person's own.

mod actions;
mod chips;
mod engine;
mod kept;
mod kind;
mod query;
mod stack;

pub use actions::{
    ActionHit, ControlDecl, ControlKind, action_control, action_hits, actions_group,
};
pub use chips::{FacetMenu, FacetOffer, FacetValue, OpenFacet, facet_bar, facet_offers};
pub use engine::{
    Cancel, FRESH, GROUP_LIMIT, LocalSource, QueryEngine, QueryResults, QueryTimings, RemoteError,
    RemoteSection, RemoteSource, ResultGroup, THROTTLE, excerpt,
};
pub use kept::{KeptView, KeptViews};
pub use kind::{FieldDecl, FieldRole, FilterKind, Form, Item, KindDecl, filters};
pub use query::{Chip, QuerySpec};
pub use stack::{ViewEntry, ViewPlace, ViewStack};

#[cfg(test)]
mod tests;
