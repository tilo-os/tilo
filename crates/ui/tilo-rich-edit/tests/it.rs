//! The rich-text widgets driven as an app drives them, through the toolkit's headless host: one
//! test binary, each file a module.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "integration tests may draw a widget directly and wait on the real clock; the rule guards the library"
)]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::format_collect,
    clippy::unnecessary_literal_bound,
    reason = "these tests were the toolkit's own unit tests, under its lint allowances; kept as they were"
)]

mod block_editor_markdown;
mod block_editor_slash_menu_integration;
mod block_editor_wrapping;
mod component;
mod embedded;
mod follow;
mod host;
mod link_popover_renders;
mod notes_controls;
mod semantic_publication;
mod window_damage;
