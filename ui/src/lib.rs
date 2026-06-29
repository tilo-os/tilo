pub mod app;
pub mod color_compat;
pub mod state;
pub mod widget;
pub mod text_render;
pub mod text_selection;
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
// Phase 1 (Audit timeline) widgets + their shared types. The
// `qos_collab_types::OpClass` / `ArtifactKind` re-exports flow through
// these widgets, so callers can build a `HistoryEntry` without taking
// a direct dependency on the collab-types crate.
pub use widget::activity_timeline::ActivityTimeline;
pub use widget::history::{
    HistoryAuthor, HistoryEntry, HistoryEntryId, HistoryFilter,
    format_relative_time, hue_to_rgb, op_class_glyph, op_class_is_revertible,
};
pub use widget::history_scrubber::HistoryScrubber;
// Phase 5 (Channel browser) — see `widget/channel_browser.rs` module
// doc-comment for the design intent. Source-agnostic; consumes
// `Vec<ChannelEntry>` from chrome state.
pub use widget::channel_browser::{ChannelBrowser, ChannelEntry};
// Phase 4 (Conflict toolkit) — see `widget/merge_strategy.rs` module
// doc-comment for the design intent and convention for declaring a
// merge strategy on a CRDT-bound widget.
pub use widget::merge_strategy::{
    Alternative, ConcurrentOp, ConflictKind, ConflictResolver, MergeStrategy,
    surface_banner,
};
pub use widget::conflict_banner::ConflictBanner;
pub use widget::conflict_picker::{ConflictPicker, PickerOutcome};
pub use text_render::{TextRenderer, TextWeight};
pub use text_selection::{
    Selection, clipboard_copy, clipboard_paste, line_boundaries,
    next_char_boundary, next_word_boundary, prev_char_boundary,
    prev_word_boundary, word_boundaries,
};

// Unified animation primitives (foundation #17).
pub use animation::{Animated, Curve, Lerp};

// Collaboration contract types (Phase C).
pub use presence::{PresenceCtx, PresenceKey, PathSegment, SelectionRange};
pub use permissions::{
    Op, PermissionContext, PermissionScope, ResourceId, Role, UserId,
};
pub use collaboration::PresencePayload;
