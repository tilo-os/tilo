// One test binary instead of one per file; #[path] keeps the files in place.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "typing_desktop/mod.rs"]
mod typing_desktop;

#[path = "a11y.rs"]
mod a11y;
#[path = "ask_over_search.rs"]
mod ask_over_search;
#[path = "card_composites.rs"]
mod card_composites;
#[path = "catalogue.rs"]
mod catalogue;
#[path = "closed_tab.rs"]
mod closed_tab;
#[path = "control_hints.rs"]
mod control_hints;
#[path = "emoji.rs"]
mod emoji;
#[path = "page_top_bar.rs"]
mod page_top_bar;
#[path = "palette_above_windows.rs"]
mod palette_above_windows;
#[path = "popover_frame.rs"]
mod popover_frame;
#[path = "press_outside.rs"]
mod press_outside;
#[path = "retained_popover.rs"]
mod retained_popover;
#[path = "search_overlay.rs"]
mod search_overlay;
#[path = "shortcuts.rs"]
mod shortcuts;
#[path = "top_bar_reveal.rs"]
mod top_bar_reveal;
#[path = "typing_sync.rs"]
mod typing_sync;
#[path = "welcome_card.rs"]
mod welcome_card;
#[path = "workspace_activity.rs"]
mod workspace_activity;
