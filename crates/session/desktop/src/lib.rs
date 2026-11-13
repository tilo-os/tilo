//! The platform-neutral desktop, as widgets over the shared desktop model.
//! Every model type (workspaces, strips, groups, tabs, the workspace schema and desktop commands)
//! comes from [`tilo_desktop_model`]; this crate holds none. Hosts supply platform reads through
//! snapshot fields and traits (chrome, activity, tile resolution) and apply the intents it emits.
#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        reason = "test assertions; layout tests pin exact computed positions"
    )
)]

pub mod about;
pub mod authority;
pub mod backdrop;
pub mod collaborators;
pub mod control_hints;
pub mod desk_edges;
pub mod desk_verbs;
pub mod desk_widgets;
pub mod desktop_host;
pub mod display_identify;
pub mod emoji;
pub mod feedback;
pub mod feedback_picker;
pub mod follow_banner;
pub mod fullscreen_hint;
pub mod here;
pub mod launchpad;
pub mod osd;
pub mod overlays;
pub mod overview;
pub mod overview_panel;
pub mod overview_picture;
pub mod overview_tilo;
pub mod profile_card;
pub mod records;
pub mod resolver;
pub mod search_overlay;
pub mod shortcuts;
pub mod strip_indicators;
pub mod strip_navigator;
pub mod strip_switcher;
pub mod super_peek;
pub mod text_entry;
pub mod topbar;
pub mod window_flight;
pub mod window_switcher;
pub mod workspace_menu;
mod workspace_names;
pub mod workspace_settings;

// The workspace history pane, the notification stack and centre, the drill-in list and the
// desktop's catalogue are portable: they live with the desktop surface, which the browser builds
// too.
pub use tilo_desktop_surface::{activity, drill_in, history_log, i18n, notifications};
// The host contract — the bar's snapshots, what a host answers the desk's rules, the asks the desk
// leaves it — is portable: it lives with the desktop surface, which a page builds too.
pub use tilo_desktop_surface::host::chrome;
pub use tilo_desktop_surface::host::contract as host_contract;

#[cfg(test)]
mod i18n_copy_lint_tests;
