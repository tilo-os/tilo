//! The qOS UI toolkit: retained-mode, CRDT-native widgets.
//!
//! # Reach for the toolkit's stack through the toolkit
//!
//! An app that uses `ui` cannot avoid touching `vello`, `taffy`, `winit` and
//! `automerge` — it hands scenes to a widget tree, sizes nodes in the same
//! Taffy tree, receives the same winit events, and mutates the same Automerge
//! document. Each of those is re-exported here, and an app should prefer
//! `ui::vello` over declaring its own `vello` dependency.
//!
//! This is not a style preference. Cargo will happily resolve two
//! semver-incompatible copies of a crate into one binary, and for `vello` and
//! `winit` that is fatal rather than merely wasteful: `vello::Scene` from one
//! copy is a *different type* from `vello::Scene` in the other, so a widget
//! tree built against the toolkit's copy cannot be handed a scene built
//! against the app's. The failure is a type error at best and a confusing one
//! at that ("expected `Scene`, found `Scene`"). Going through `ui::` makes the
//! version question unaskable — there is exactly one copy, and it is the one
//! the widget tree was compiled against.
//!
//! ```ignore
//! use ui::vello::kurbo::{Affine, Rect};   // not `use vello::kurbo::...`
//! use ui::winit::event::WindowEvent;
//! ```
//!
//! Apps that already declare these directly keep working — the workspace
//! pins every member to one version, so they resolve to the same crate. The
//! re-exports are what lets new code get it right by default, and what lets an
//! app drop the direct dependency when it is next touched.

pub mod a11y;
pub mod accesskit_bridge;
pub mod screen_reader;
pub mod app;
pub mod color_compat;
pub mod doc;
pub mod headless;
pub mod history_source;
pub mod persist;
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

// The toolkit's own stack, re-exported so an app never has to name a version.
// See the crate-level note above for why this matters for `vello` and `winit`
// in particular. `kurbo` and `peniko` come through `vello`, which is where the
// geometry and paint types in every `draw` signature actually live.
pub use automerge;
pub use taffy;
pub use vello;
pub use vello::{kurbo, peniko};
pub use winit;

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
// `permissions::Verb`, which are already re-exported at the crate root.
pub use a11y::{A11ySettings, AccessNode, AccessState};

// Built-in screen reader (W6): the first-party narrator over the a11y tree +
// qos-tts. `describe` is the pure phrase-builder; `ScreenReader` the imperative
// narrator the `App` holds when `A11ySettings.screen_reader` is on.
pub use screen_reader::{ScreenReader, describe, flatten_reading_order};

pub use widget::{Widget, Element};
pub use widget::{AppShell, NarrowScreen, ShellLayout};
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
// The producer that turns a live CRDT document into the rows those two
// widgets render, plus the actor-id → person directory that gives each row
// an author. Re-exported here so an app reaches the whole history stack
// through `ui::` without knowing the module layout.
pub use history_source::{AuthorDirectory, HistorySource, heads_for_entry, version_index_for_entry};
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
pub use animation::{ease_out, Animated, Curve, Lerp, Spring, Transition};

// Collaboration contract types (Phase C).
pub use presence::{PresenceCtx, PresenceKey, PathSegment, SelectionRange};
// The capability model (`collab_types::caps`), reached through the
// toolkit. These are re-exports, not toolkit-local copies — see
// `permissions/mod.rs`.
pub use permissions::{
    Actor, CapDecision, CapOverride, PermissionContext, PermissionScope, ResourceRef, Role,
    Scope as CapScope, Subject, UserId, Verb,
};
pub use collaboration::PresencePayload;
