//! The desktop's model: the workspaces, strips and windows a person arranges, the navigation
//! contract every shell and agent drives them through, and what the desktop says about them.
//! Every shell reads this; none of it draws.

pub mod activity;
pub mod displays;
pub mod history_log;
pub mod i18n;
pub mod names;
pub mod navigation;
pub mod notifications;
pub mod now_playing;
pub mod outcome;
pub mod seed;
pub mod shared_tree;
pub mod tree;
pub mod warmth;
pub mod window;

pub use displays::{display_rect, working_display};
pub use seed::{seed_tree, seed_tree_in};
