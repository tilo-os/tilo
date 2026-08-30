// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

#[path = "accounts_mail_loop.rs"]
mod accounts_mail_loop;
#[path = "category_nav.rs"]
mod category_nav;
#[path = "category_sidebar.rs"]
mod category_sidebar;
#[path = "control_interaction.rs"]
mod control_interaction;
#[path = "no_app_title_header.rs"]
mod no_app_title_header;
#[path = "prefs_persistence.rs"]
mod prefs_persistence;
#[path = "scroll.rs"]
mod scroll;
#[path = "search_focus.rs"]
mod search_focus;
#[path = "shots_smoke.rs"]
mod shots_smoke;
