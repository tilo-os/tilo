// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "category_nav.rs"]
mod category_nav;
#[path = "category_sidebar.rs"]
mod category_sidebar;
#[path = "control_interaction.rs"]
mod control_interaction;
#[path = "no_app_title_header.rs"]
mod no_app_title_header;
#[path = "scroll.rs"]
mod scroll;
#[path = "search_focus.rs"]
mod search_focus;
#[path = "shots_smoke.rs"]
mod shots_smoke;

#[path = "composition_balance.rs"]
mod composition_balance;

#[path = "alignment_review.rs"]
mod alignment_review;
