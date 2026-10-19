//! Small ui-toolkit compositions shared across the store surface.
//!
//! The app's own root widget composes the pages out of `ui::widget`
//! compositions; this module only holds the reusable piece that shows up on
//! more than one screen: the catalogue card and the app icon.

pub mod app;
pub mod card;

pub use card::{
    CardAction, CardDetails, app_icon, category_label, provenance, store_card, visible_version,
};
