pub mod a11y;
pub mod accesskit_bridge;
pub mod screen_reader;
pub mod app;
pub mod color_compat;
pub mod doc;
pub mod state;
pub mod widget;
pub mod text_render;
pub mod richtext;
pub mod chrome;
pub mod gesture;
pub mod text_selection;
pub mod theme;
pub mod caps_lock;
pub mod i18n;
pub mod collaboration;
pub mod tokens;
pub mod presence;
pub mod permissions;
pub mod dnd;
pub mod animation;
pub mod surface;
pub mod overlay;
pub mod icons;
pub mod selection;
pub mod offscreen;
pub mod image_store;
pub mod render_thread;
pub mod warmup;
pub mod webview;
pub mod fullscreen;
pub mod mpris;
pub mod nav_stack;

pub use app::App;
pub use color_compat::ColorCompat;
pub use doc::{TextEdit, put_if_changed, put_text_diffed, splice_text_diffed};
pub use state::{State, UndoEntry};
pub use theme::Theme;
pub use image_store::{Fetcher, ImageStore};
pub use nav_stack::{EdgeSwipeBack, NavAction, NavStack};
pub use caps_lock::CapsLockEstimate;
pub use selection::SelectionModel;

// Accessibility semantic model (W0). `a11y::Role` / `a11y::Action` are reached
// through the module path to avoid clashing with `permissions::{Role}` /
// `permissions::Op`, which are already re-exported at the crate root.
pub use a11y::{A11ySettings, AccessNode, AccessState};

// Built-in screen reader (W6): the first-party narrator over the a11y tree +
// qos-tts. `describe` is the pure phrase-builder; `ScreenReader` the imperative
// narrator the `App` holds when `A11ySettings.screen_reader` is on.
pub use screen_reader::{ScreenReader, describe, flatten_reading_order};

pub use widget::{Widget, Element};
pub use widget::{EditableLabel, EditOutcome};
pub use widget::{AppCommand, AppKind, AppWidget, IconDescriptor};
pub use widget::command_palette::{
    ArgumentSpec, CommandEntry, CommandOutcome, CommandPalette, PaletteMode,
};
// Phase 1 (Audit timeline) widgets + their shared types. The
// `collab_types::OpClass` / `ArtifactKind` re-exports flow through
// these widgets, so callers can build a `HistoryEntry` without taking
// a direct dependency on the collab-types crate.
pub use widget::activity_timeline::ActivityTimeline;
pub use widget::history::{
    HistoryAuthor, HistoryEntry, HistoryEntryId, HistoryFilter, RedactionDisplay,
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
pub use text_render::{FontChoice, TextRenderer, TextWeight};
pub use richtext::{BaselineShift, FontFamily, RichSpan, RichTextLayout, TextAlign};
pub use chrome::{LabelCue, ToolbarItem};
pub use gesture::{DragSelect, Granularity};
pub use text_selection::{
    Selection, clipboard_copy, clipboard_copy_image, clipboard_copy_primary, clipboard_paste,
    clipboard_paste_primary, extend_span_drag, line_boundaries, next_char_boundary,
    next_word_boundary, next_word_boundary_chars, prev_char_boundary, prev_word_boundary,
    prev_word_boundary_chars, word_boundaries, word_boundaries_chars,
};

// Shared motion system — one curve set, one wall-clock driver, one
// interruptible open/close transition, one interactive spring.
pub use animation::{Animated, Curve, Lerp, Spring, Transition};

// Collaboration contract types (Phase C).
pub use presence::{PresenceCtx, PresenceKey, PathSegment, SelectionRange};
pub use permissions::{
    Op, PermissionContext, PermissionScope, ResourceId, Role, UserId,
};
pub use collaboration::PresencePayload;
