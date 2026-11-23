//! The desktop's model: the workspaces, strips and windows a person arranges, the navigation
//! contract every shell and agent drives them through, and what the desktop says about them.
//! Every shell reads this; none of it draws.

pub mod about;
pub mod activity;
pub mod authority;
pub mod displays;
pub mod emoji;
pub mod here;
pub mod history_log;
pub mod host;
pub mod i18n;
pub mod launchpad;
pub mod names;
pub mod navigation;
pub mod notifications;
pub mod now_playing;
pub mod osd;
pub mod outcome;
pub mod overlays;
pub mod people;
pub mod profile;
pub mod seed;
pub mod session;
pub mod shared_tree;
pub mod shell_verbs;
pub mod shortcuts;
pub mod state;
pub mod super_peek;
pub mod verbs;
pub mod wake;
pub mod warmth;
pub mod window;
pub mod workspace_names;
pub mod workspace_settings;

pub use displays::{display_rect, working_display};
pub use seed::{seed_tree, seed_tree_in};
