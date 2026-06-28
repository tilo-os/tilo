pub mod app;
pub mod color_compat;
pub mod state;
pub mod widget;
pub mod text_render;
pub mod richtext;
pub mod chrome;
pub mod theme;
pub mod i18n;
pub mod collaboration;
pub mod tokens;
pub mod presence;
pub mod permissions;
pub mod dnd;
pub mod animation;
pub mod overlay;
pub mod icons;
pub mod offscreen;
pub mod render_thread;

pub use app::App;
pub use color_compat::ColorCompat;
pub use state::{State, UndoEntry};
pub use theme::Theme;

pub use widget::{Widget, Element};
pub use widget::{AppCommand, AppKind, AppWidget, IconDescriptor};
pub use widget::command_palette::{CommandEntry, CommandPalette};
pub use text_render::{TextRenderer, TextWeight};
pub use richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
pub use chrome::{LabelCue, ToolbarItem};

// Unified animation primitives (foundation #17).
pub use animation::{Animated, Curve, Lerp};

// Collaboration contract types (Phase C).
pub use presence::{PresenceCtx, PresenceKey, PathSegment, SelectionRange};
pub use permissions::{
    Op, PermissionContext, PermissionScope, ResourceId, Role, UserId,
};
pub use collaboration::PresencePayload;
