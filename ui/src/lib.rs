//! The qOS UI toolkit: retained-mode, CRDT-native widgets.
//!
//! # Reach for the toolkit's stack through the toolkit
//!
//! An app that uses `ui` cannot avoid touching `vello`, `taffy`, `winit` and
//! `automerge` — it hands scenes to a widget tree, sizes nodes in the same
//! Taffy tree, receives the same winit events, and mutates the same Automerge
//! document. Each is re-exported here; prefer `ui::vello` over declaring a
//! separate `vello` dependency.
//!
//! This is not a style preference. Cargo will happily resolve two
//! semver-incompatible copies of a crate into one binary, and for `vello` and
//! `winit` that is fatal: `vello::Scene` from one copy is a *different type*
//! from `vello::Scene` in the other, so a widget tree built against the
//! toolkit's copy cannot be handed a scene built against the app's — a type
//! error at best, and a confusing one ("expected `Scene`, found `Scene`").
//! Going through `ui::` makes the version question unaskable.
//!
//! ```ignore
//! use ui::vello::kurbo::{Affine, Rect};   // not `use vello::kurbo::...`
//! use ui::winit::event::WindowEvent;
//! ```
//!
//! Apps that already declare these directly keep working — the workspace
//! pins every member to one version. The re-exports are what lets new code
//! get it right by default.

pub mod a11y;
pub mod a11y_tree;
pub mod accesskit_bridge;
pub mod screen_reader;
pub mod action_host;
pub mod action_palette;
pub mod app;
pub mod color_compat;
pub mod diag;
pub mod doc;
pub mod headless;
pub mod history_source;
pub mod persist;
pub mod record;
pub mod portal;
pub mod uri;
pub mod state;
pub mod view;
pub mod widget;
pub mod text_render;
pub mod richtext;
pub mod chrome;
pub mod clipboard;
pub mod gesture;
pub mod gpu;
pub mod status_bubble;
pub mod text_selection;
pub mod text_edit;
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
pub mod window_icon;
pub mod webview;
pub mod fullscreen;
pub mod scale;
pub mod mpris;
pub mod format;
pub mod nav_stack;
pub mod reader_hud;
pub mod search;
pub mod profiling;
pub mod sysdirs;

// `kurbo` and `peniko` come through `vello`, where the geometry and paint
// types in every `draw` signature live.
pub use automerge;
pub use taffy;
pub use vello;
pub use vello::{kurbo, peniko};
pub use winit;

pub use app::{App, local_surface_id};
pub use reader_hud::{HudAction, HudSwatch, MarkerDrag, ReaderHudStyle, SelectionHud};
pub use color_compat::ColorCompat;
pub use doc::{TextEdit, put_if_changed, put_text_diffed, splice_text_diffed};
pub use record::{RecordCheckout, RecordId, RecordKind, RecordRef, RecordSet, RecordStore};
pub use state::{State, UndoEntry, UndoLog};
pub use view::{
    FocusView, FreezeThaw, FrozenApp, ThawError, View, ViewContribution, ViewRect, FROZEN_VERSION,
};
pub use theme::{AppearanceIntent, Theme};
pub use image_store::{Fetcher, ImageStore};
pub use nav_stack::{EdgeSwipeBack, NavAction, NavStack};
pub use portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
pub use search::{fuzzy_score, FnProvider, SearchGroup, SearchProvider, SearchRegistry, SearchResult};
pub use caps_lock::CapsLockEstimate;
pub use profiling::{
    FrameProfiler, FrameTiming, Pass, PerfReport, PerfScenario, PerfStep, StatSummary,
    run_scenario, run_scenario_on, FRAME_BUDGET_US,
};
pub use selection::SelectionModel;

// `a11y::Role` / `a11y::Action` are reached through the module path to avoid
// clashing with `permissions::{Role}` / `permissions::Verb`, which are
// already re-exported at the crate root.
pub use a11y::{A11ySettings, AccessNode, AccessState};
pub use a11y_tree::{SemanticAction, SemanticActionKind, SemanticId, SemanticNode, SemanticTree, TreeDelta};

// `describe` is the pure phrase-builder; `ScreenReader` the imperative
// narrator the `App` holds when `A11ySettings.screen_reader` is on.
pub use screen_reader::{ScreenReader, describe, flatten_reading_order};

pub use widget::{Widget, Element};
pub use widget::{AppShell, NarrowScreen, ShellLayout};
pub use widget::{Card, CardGrid, card_column_width, card_columns};
pub use widget::{EditableLabel, EditOutcome};
pub use widget::{RichBlockStyle, RichTextEditor};
pub use widget::{AppCommand, AppKind, AppWidget, IconDescriptor};
pub use widget::command_palette::{
    ArgumentSpec, CommandEntry, CommandOutcome, CommandPalette, PaletteMode, SearchScope,
};
pub use action_host::ActionHost;
pub use action_palette::ActionPalette;
pub use qos_actions;
// Audit-timeline widgets + their shared types. The `collab_types::OpClass` /
// `ArtifactKind` re-exports flow through these widgets, so callers can build
// a `HistoryEntry` without a direct dependency on the collab-types crate.
pub use widget::activity_timeline::{ActivityTimeline, BandUndoRequest};
pub use widget::history::{
    HistoryAuthor, HistoryEntry, HistoryEntryId, HistoryFilter, RedactionDisplay,
    format_relative_time, hue_to_rgb, op_class_glyph, op_class_is_revertible,
};
pub use widget::history_scrubber::HistoryScrubber;
pub use widget::history_group::{
    group_entries, GroupNameRequest, GroupingConfig, HistoryGroup, NamedRange, OpFamily,
    MAX_GROUP_DEPTH,
};
pub use widget::name_field::{NameField, NameFieldOutcome};
// Re-exported so an app reaches the whole history stack through `ui::`
// without knowing the module layout.
pub use history_source::{AuthorDirectory, HistorySource, heads_for_entry, version_index_for_entry};
pub use history_source::groups as history_groups;
// Undoing a run of changes that never reached anybody's undo stack — a
// remote author's, an agent's.
pub use history_source::revert::{
    latest_run_by, plan_revert, revert_run, ChangeRun, KeptSlot, RevertError, RevertOutcome, Slot,
};
pub use history_source::run_undo::{
    refuse_run, undo_run, undo_step, unreachable_steps, RunUndoReport, StepOutcome, StepState,
};
pub use history_source::runs::{
    forget_run, read_runs, record_run, run_starting_at, AgentRun, RunStep,
};
// Channel browser — see `widget/channel_browser.rs` module doc-comment for
// the design intent. Source-agnostic; consumes `Vec<ChannelEntry>` from
// chrome state.
pub use widget::channel_browser::{ChannelBrowser, ChannelEntry};
// Conflict toolkit — see `widget/merge_strategy.rs` module doc-comment for
// the design intent and convention for declaring a merge strategy on a
// CRDT-bound widget.
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
pub use status_bubble::StatusBubble;
pub use text_selection::{
    Selection, clipboard_copy, clipboard_copy_image, clipboard_copy_primary, clipboard_paste,
    clipboard_paste_primary, extend_span_drag, line_boundaries, next_char_boundary,
    next_word_boundary, next_word_boundary_chars, prev_char_boundary, prev_word_boundary,
    prev_word_boundary_chars, word_boundaries, word_boundaries_chars,
};
pub use text_edit::{Edit, EditCore, Motion};

pub use animation::{ease_out, Animated, Curve, Lerp, Spring, Transition};

pub use presence::{PresenceCtx, PresenceKey, PathSegment, SelectionRange};
// The capability model (`collab_types::caps`), reached through the
// toolkit. These are re-exports, not toolkit-local copies — see
// `permissions/mod.rs`.
pub use permissions::{
    Actor, CapDecision, CapOverride, PermissionContext, PermissionScope, ResourceRef, Role,
    Scope as CapScope, Subject, UserId, Verb,
};
pub use collaboration::PresencePayload;
