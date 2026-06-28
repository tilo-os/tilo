//! One module per settings category.
//!
//! Each pane exposes a `build()` (or category-specific) function
//! returning a `Box<dyn Widget>` containing that pane's content. The
//! wrapper chrome (the pane header) lives in `settings_app::build_pane`,
//! so the modules here focus on the category-specific body.
//!
//! Most panes now own their state via a `Widget`-implementing struct
//! that holds a `os_actions::Dispatcher`. The historical `coming_soon`
//! placeholder is gone — every category has at least a partial
//! interactive surface.

pub mod about;
pub mod audio;
pub mod bluetooth;
pub mod collaboration;
pub mod display;
pub mod network;
pub mod workspaces;
