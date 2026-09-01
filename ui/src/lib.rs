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

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod a11y;
pub mod a11y_tree;
pub mod accesskit_bridge;
pub mod action_host;
pub mod action_palette;
pub mod animation;
pub mod app;
pub mod caps_lock;
pub mod chrome;
pub mod clipboard;
pub mod collaboration;
pub mod collection_transition;
pub mod color_compat;
pub mod diag;
/// Zero-copy DMA-BUF → `wgpu` texture import via Vulkan external memory.
/// Shared by the WebView's page buffers and the video decoder's hardware
/// frames.
#[cfg(feature = "dmabuf-import")]
pub mod dmabuf_import;
pub mod dnd;
pub mod doc;
pub mod format;
pub mod fullscreen;
pub mod gesture;
pub mod gpu;
/// Painting decoded video on the GPU: uploading a hardware decoder's planar
/// output and converting YUV to RGB in a shader.
#[cfg(feature = "gpu-video")]
pub mod gpu_video;
pub mod headless;
pub mod history_source;
pub mod i18n;
pub mod icons;
pub mod image_store;
pub mod interaction;
pub mod launch;
pub mod local_state;
pub mod math;
pub mod mpris;
pub mod nav_stack;
pub mod num;
pub mod offscreen;
pub mod overlay;
pub mod paragraph;
pub mod permissions;
pub mod persist;
pub mod platform;
pub mod portal;
pub mod presence;
pub mod profiling;
pub mod query_text;
pub use query_text::{QueryComposition, QueryEventChange, QueryImeChange};
pub mod text_assistance;
pub mod text_break;
pub use text_break::{LineBreakKind, line_break_opportunities};
pub mod reader_hud;
pub mod record;
pub mod render_thread;
pub mod reorder;
pub mod richtext;
pub mod scale;
pub mod screen_reader;
pub mod search;
pub mod selection;
pub mod state;
pub mod status_bubble;
pub mod surface;
pub mod sysdirs;
pub mod text_edit;
pub mod text_render;
pub mod text_selection;
pub mod theme;
pub mod tokens;
pub mod ui_graph;
pub mod undo;
pub mod uri;
pub mod view;
pub mod virtual_viewport;
pub mod warmup;
pub mod webview;
pub mod widget;
pub mod window_icon;

// `kurbo` and `peniko` come through `vello`, where the geometry and paint
// types in every `draw` signature live.
/// The AccessKit types the bridge and [`headless::Driver`] hand back,
/// re-exported so an app crate can name them (`ui::accesskit::Role`) without
/// taking its own dependency on a version that must match this one exactly.
pub use accesskit;
pub use automerge;
pub use taffy;
pub use vello;
pub use vello::{kurbo, peniko};
pub use winit;

pub use app::{App, local_surface_id};
pub use caps_lock::CapsLockEstimate;
pub use collection_transition::{
    KeyedRowSnapshot, KeyedTransitionSet, RowGeometry, TransitionVisual,
};
pub use color_compat::ColorCompat;
pub use doc::{TextEdit, put_if_changed, put_text_diffed, splice_text_diffed};
pub use i18n::{ArgValue, I18nError, I18nManager};
pub use image_store::{Fetcher, ImageStore};
pub use interaction::{InputModality, InteractionEnvironment, PointerPrecision};
pub use local_state::{
    LatestRequest, LatestTask, LocalState, LocalStateWriter, RequestCompletion, TaskCompletion,
    WakeHandle,
};
pub use nav_stack::{EdgeSwipeBack, NavAction, NavStack};
pub use paragraph::{ComposedLine, ParagraphBox, ParagraphOptions, compose_paragraph};
pub use platform::{DialogActionOrder, Platform, PlatformConventions, Shortcut};
pub use portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
pub use profiling::{
    FRAME_BUDGET_US, FrameProfiler, FrameTiming, Pass, PerfReport, PerfScenario, PerfStep,
    StatSummary, run_scenario, run_scenario_on,
};
pub use reader_hud::{HudAction, HudSwatch, MarkerDrag, ReaderHudStyle, SelectionHud};
pub use record::{RecordCheckout, RecordId, RecordKind, RecordRef, RecordSet, RecordStore};
pub use reorder::{
    DropCandidate, DropPlacement, DropTarget, ReorderController, ReorderDrop, ReorderMotion,
};
pub use search::{
    FnProvider, SearchGroup, SearchProvider, SearchRegistry, SearchResult, fuzzy_score,
};
pub use selection::{KeySelection, SelectionModel};
pub use state::{
    Revision, RevisionHistory, RevisionId, State, UndoEntry, UndoLog, UnknownRevision,
};
pub use text_break::discretionary_hyphen_breaks;
pub use theme::{AppearanceIntent, Theme};
pub use ui_graph::{
    LiveUi, UI_GRAPH_VERSION, UiBuildContext, UiButtonStyle, UiComponentRequest, UiDocument,
    UiDocumentExt, UiGraphEditor, UiGraphError, UiGraphHandle, UiNavigation, UiNode, UiTextRole,
    UiTextSource,
};
pub use undo::{UndoAccess, UndoRequest, UndoStatus};
pub use view::{
    FROZEN_VERSION, FocusView, FreezeThaw, FrozenApp, ThawError, View, ViewContribution, ViewRect,
};
pub use virtual_viewport::{
    DragScrollFinish, DragScrollUpdate, DragScroller, ReconcileError, ScrollAlignment,
    ScrollMomentum, SparseAnchor, SparseVirtualViewport, VirtualViewport,
};

// `a11y::Role` / `a11y::Action` are reached through the module path to avoid
// clashing with `permissions::{Role}` / `permissions::Verb`, which are
// already re-exported at the crate root.
pub use a11y::{A11ySettings, AccessBounds, AccessNode, AccessState, AccessibleTextSelection};
pub use a11y_tree::{
    SemanticAction, SemanticActionKind, SemanticId, SemanticNode, SemanticTree, TreeDelta,
};

// `describe` is the pure phrase-builder; `ScreenReader` the imperative
// narrator the `App` holds when `A11ySettings.screen_reader` is on.
pub use screen_reader::{ScreenReader, describe, flatten_reading_order};

pub use action_host::ActionHost;
pub use action_palette::ActionPalette;
pub use qos_actions;
pub use widget::ModelView;
pub use widget::command_palette::{
    ArgumentSpec, CommandEntry, CommandOutcome, CommandPalette, PaletteMode, SearchScope,
};
pub use widget::{AppCommand, AppKind, AppWidget, IconDescriptor};
pub use widget::{AppShell, NarrowScreen, ShellLayout};
pub use widget::{Card, CardGrid, card_column_width, card_columns};
pub use widget::{EditOutcome, EditableLabel};
pub use widget::{Element, Widget};
pub use widget::{
    NativeView, NativeViewAccessibility, NativeViewBackend, NativeViewGeometry, NativeViewHost,
    NativeViewInput,
};
pub use widget::{RichBlockStyle, RichTextEditor};
pub use widget::{RouteError, RouteHandle, Router};
// Audit-timeline widgets + their shared types. The `collab_types::OpClass` /
// `ArtifactKind` re-exports flow through these widgets, so callers can build
// a `HistoryEntry` without a direct dependency on the collab-types crate.
pub use widget::activity_timeline::{ActivityTimeline, BandUndoRequest};
pub use widget::history::{
    HistoryAuthor, HistoryClassification, HistoryEntry, HistoryEntryId, HistoryFilter,
    RedactionDisplay, format_relative_time, hue_to_rgb, op_class_glyph, op_class_is_revertible,
};
pub use widget::history_group::{
    GroupNameRequest, GroupingConfig, HistoryGroup, MAX_GROUP_DEPTH, NamedRange, OpFamily,
    group_entries,
};
pub use widget::history_scrubber::HistoryScrubber;
pub use widget::name_field::{NameField, NameFieldOutcome};
// Re-exported so an app reaches the whole history stack through `ui::`
// without knowing the module layout.
pub use history_source::groups as history_groups;
pub use history_source::{
    ActivityBrief, ActivityDigest, ActivityNarrator, AuthorDirectory, CompareError,
    GroundedNarrative, HistoryComparison, HistoryFacetIndex, HistoryResourceTarget,
    HistorySeenFrontier, HistorySelectionDetail, HistorySource, NarrativeRequest, compare_entries,
    deterministic_narrative, heads_for_entry, validate_narrative, version_index_for_entry,
};
// Undoing a run of changes that never reached anybody's undo stack — a
// remote author's, an agent's.
pub use history_source::revert::{
    ChangeRun, KeptSlot, RevertError, RevertOutcome, Slot, latest_run_by, plan_revert, revert_run,
};
pub use history_source::run_undo::{
    RunUndoReport, StepOutcome, StepState, refuse_run, undo_run, undo_step, unreachable_steps,
};
pub use history_source::runs::{
    AgentRun, RunStep, forget_run, read_runs, record_run, run_starting_at,
};
// Channel browser — see `widget/channel_browser.rs` module doc-comment for
// the design intent. Source-agnostic; consumes `Vec<ChannelEntry>` from
// chrome state.
pub use widget::channel_browser::{ChannelBrowser, ChannelEntry};
// Conflict toolkit — see `widget/merge_strategy.rs` module doc-comment for
// the design intent and convention for declaring a merge strategy on a
// CRDT-bound widget.
pub use chrome::{LabelCue, ToolbarItem};
pub use gesture::{DragSelect, Granularity};
pub use richtext::{BaselineShift, FontFamily, RichSpan, RichTextLayout, TextAlign};
pub use status_bubble::StatusBubble;
pub use text_edit::{Edit, EditCore, Motion};
pub use text_render::{
    FontChoice, FontVariation, Fragmentation, HangingPunctuation, Hyphenation, NumericStyle,
    OpenTypeFeature, OpenTypeTag, OpticalSizing, ParagraphAlign, PolygonTextLayout, TextFlowRegion,
    TextFlowRegionError, TextLayoutConstraints, TextLayoutEngine, TextRenderer, TextStyle,
    TextWeight, TextWrapMode, UI_MONOSPACE_FAMILY, UI_SANS_FAMILY, UI_SERIF_FAMILY,
    configured_font_system,
};
pub use text_selection::{
    Selection, TextAffinity, TextPosition, clipboard_copy, clipboard_copy_image,
    clipboard_copy_primary, clipboard_paste, clipboard_paste_primary, extend_span_drag,
    grapheme_boundary_at_or_before, line_boundaries, next_char_boundary, next_word_boundary,
    next_word_boundary_chars, prev_char_boundary, prev_word_boundary, prev_word_boundary_chars,
    word_boundaries, word_boundaries_chars,
};
pub use widget::conflict_banner::ConflictBanner;
pub use widget::conflict_picker::{ConflictPicker, PickerOutcome};
pub use widget::merge_strategy::{
    Alternative, ConcurrentOp, ConflictKind, ConflictResolver, MergeStrategy, surface_banner,
};

pub use animation::{Animated, Curve, Lerp, Spring, Transition, ease_out};

pub use presence::{PathSegment, PresenceCtx, PresenceKey, SelectionRange};
// The capability model (`collab_types::caps`), reached through the
// toolkit. These are re-exports, not toolkit-local copies — see
// `permissions/mod.rs`.
pub use collaboration::PresencePayload;
pub use permissions::{
    Actor, CapDecision, CapOverride, PermissionContext, PermissionScope, ResourceRef, Role,
    Scope as CapScope, Subject, UserId, Verb,
};
