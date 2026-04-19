pub mod app;
pub mod state;
pub mod widget;
pub mod text_render;
pub mod theme;
pub mod i18n;
pub mod collaboration;
pub mod tokens;
pub mod presence;
pub mod permissions;
pub mod dnd;
pub mod animation;
pub mod overlay;

pub use app::App;
pub use state::{State, UndoEntry};
pub use theme::Theme;

pub use widget::{Widget, Element};
pub use widget::{AppCommand, AppKind, AppWidget, IconDescriptor};
pub use widget::command_palette::{CommandEntry, CommandPalette};

// Collaboration contract types (Phase C).
pub use presence::{PresenceCtx, PresenceKey, PathSegment, SelectionRange};
pub use permissions::{
    Op, PermissionContext, PermissionScope, ResourceId, Role, UserId,
};
pub use collaboration::PresencePayload;
