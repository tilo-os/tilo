#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::collapsible_if,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::empty_line_after_doc_comments,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::items_after_test_module,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_assert_eq,
    clippy::manual_find,
    clippy::manual_midpoint,
    clippy::many_single_char_names,
    clippy::map_unwrap_or,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_errors_doc,
    clippy::needless_collect,
    clippy::needless_pass_by_value,
    clippy::needless_range_loop,
    clippy::needless_raw_string_hashes,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_clone,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::ref_as_ptr,
    clippy::result_large_err,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_in_scrutinee,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unchecked_time_subtraction,
    clippy::unicode_not_nfc,
    clippy::uninlined_format_args,
    clippy::unnecessary_literal_bound,
    clippy::unnecessary_wraps,
    clippy::unreadable_literal,
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::useless_conversion,
    reason = "migration stack lint debt is scoped to this target"
)]
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the browser build compiles native-host plumbing it never drives: the App runner, broker transport and offscreen renderer are its only callers"
    )
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "unit tests may drive real threads and wait on the real clock; the rule guards the library"
    )
)]

//! The Tilo UI toolkit: retained-mode, CRDT-native widgets.
//!
//! # Reach for the toolkit's stack through the toolkit
//!
//! An app that uses `ui` cannot avoid touching `vello`, `taffy`, `winit` and
//! `automerge` — it hands scenes to a widget tree, sizes nodes in the same
//! Taffy tree, receives the same winit events, and mutates the same Automerge
//! document. Each is re-exported here; prefer `tilo_ui::vello` over declaring a
//! separate `vello` dependency.
//!
//! This is not a style preference. Cargo will happily resolve two
//! semver-incompatible copies of a crate into one binary, and for `vello` and
//! `winit` that is fatal: `vello::Scene` from one copy is a *different type*
//! from `vello::Scene` in the other, so a widget tree built against the
//! toolkit's copy cannot be handed a scene built against the app's — a type
//! error at best, and a confusing one ("expected `Scene`, found `Scene`").
//! Going through `tilo_ui::` makes the version question unaskable.
//!
//! ```ignore
//! use tilo_ui::vello::kurbo::{Affine, Rect};   // not `use vello::kurbo::...`
//! use tilo_ui::winit::event::WindowEvent;
//! ```
//!
//! Apps that already declare these directly keep working — the workspace
//! pins every member to one version. The re-exports are what lets new code
//! get it right by default.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod a11y;
pub mod a11y_tree;
#[cfg(all(feature = "native", target_os = "android"))]
mod accesskit_android_adapter;
pub mod accesskit_bridge;
/// The document action host behind the framework's dispatcher. Apps submit
/// edits through the framework, never through this host.
#[doc(hidden)]
pub mod action_host;
mod action_locus;
pub mod local_document_action;
pub mod table_model;
pub use action_locus::{ActionUiRoute, PaintedActionMarker};
pub mod action_palette;
pub mod agent_identity;
pub mod animation;
pub mod arriving;
/// What wants a person: the waiting stack, and the ledger an outcome goes to
/// when it does not. Replaces the transient notice as a category.
pub mod attention;
pub mod author;
pub mod capability_door;
pub mod content_zoom;
pub mod control_hints;
/// What is running: one cancellable job with a body, which the whole system
/// can see and which outlives the window that started it.
pub mod jobs;
pub mod motion;
#[cfg(test)]
extern crate self as tilo_ui;
#[cfg(test)]
#[allow(
    dead_code,
    reason = "shared support is compiled by library and integration tests whose consumers intentionally use different subsets"
)]
#[path = "../tests/support/application.rs"]
pub(crate) mod application_test_support;

#[cfg(not(target_arch = "wasm32"))]
pub mod app;
pub mod app_surface;
pub mod auth_backdrop;
pub mod backdrop;
pub mod call_pictures;
pub mod caps_lock;
pub mod chart_palette;
pub mod charts;
pub mod chrome;
pub mod clock;
pub mod collaboration;
pub use runtime_collab::native_media;
pub mod collection_transition;
pub use tilo_ui_paint::{color, color_compat};
pub mod component_lifecycle;
pub mod content;
pub mod content_layout;
mod damage;
pub mod date;
pub mod diag;
/// Zero-copy DMA-BUF → `wgpu` texture import via Vulkan external memory.
/// Shared by the WebView's page buffers and the video decoder's hardware
/// frames.
#[cfg(all(feature = "dmabuf-import", target_os = "linux"))]
pub mod dmabuf_import;
pub mod dnd;
pub mod doc;
pub mod doc_source;
#[cfg(any(all(windows, feature = "dx12-shader-cache"), test))]
mod dx12_shader_cache;
pub mod edge_autoscroll;
pub mod element_pictures;
pub mod evdev;
pub mod fact;
pub mod floating_surface;
pub mod focus_visible;
pub mod font_catalog;
pub mod format;
pub mod fullscreen;
pub mod gesture;
pub mod gpu;
/// Painting decoded video on the GPU: uploading a hardware decoder's planar
/// output and converting YUV to RGB in a shader. Plain wgpu, so every target
/// (WebGPU included) has it; `gpu-video` adds the zero-copy dmabuf import,
/// which `media::availability(Feature::ZeroCopyVideo)` reports.
pub mod gpu_video;
pub mod graph_editor;
pub mod headless;
pub mod history_mode;
pub mod history_source;
#[cfg(feature = "native")]
mod host_appearance;
#[cfg(not(target_arch = "wasm32"))]
mod host_chrome;
pub mod host_data;
pub mod hosted_window;
pub mod owner_wake;
/// The app's own vello pipelines, kept across launches in its cache directory.
#[cfg(feature = "native")]
pub(crate) mod pipeline_cache;
/// The session's read-only vello pipeline cache every app starts from.
pub mod pipeline_seed;
#[cfg(all(windows, feature = "native"))]
pub use native::host_notifications;
pub mod emoji_catalogue;
pub mod held_images;
#[cfg(not(all(windows, feature = "native")))]
#[path = "host_notifications_unsupported.rs"]
pub mod host_notifications;
pub mod i18n;
pub mod icons;
pub mod image_decoders;
pub mod image_store;
pub mod ime;
pub mod input;
pub mod interaction;
pub mod keycaps;
pub mod latin_chord;
pub mod launch;
pub mod live_collaboration;
pub mod local_identity;
pub mod local_state;
pub mod math;
#[cfg(feature = "native")]
pub mod native;
pub mod nav_stack;
pub mod num;
pub mod offscreen;
pub mod overlay;
pub mod paint;
pub mod paragraph;
pub mod passcode_keyboard;
pub mod pen;
pub mod people_photos;
pub mod permissions;
pub mod persist;
pub mod person_card;
pub mod pick;
pub mod pick_wire;
pub mod platform;
pub mod pointer_replay;
pub mod portable_storage;
pub mod presence;
pub mod priority_contacts;
pub mod profiling;
pub mod query_text;
pub use tilo_ui_paint::raster;
pub mod realtime_continuity;
pub mod room_sync;
pub mod standalone_history_pane;
pub mod storage;
/// The desktop's keyboard shortcut table (actions, their chords and the
/// defaults) as the settings document keeps it: what every desktop host
/// resolves a key press through, the native shell and a browser page alike.
pub mod shortcut_table {
    pub use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
}
/// The desk widgets and the screen edges things kept at the screen's edges
/// wait at, as the settings document keeps them: what every desktop host
/// places them by, the native shell and a browser page alike.
pub mod desk_widgets {
    pub use settings_store::desk_widgets::{DockEdge, WidgetId};
}
#[cfg(any(test, feature = "ux-bench"))]
pub mod ux_bench;
pub use query_text::{QueryComposition, QueryEventChange, QueryImeChange, QueryInk, QueryView};
pub mod deferred;
pub mod person_chat;
pub mod personal_dictionary;
pub mod private_draft;
pub mod text_assistance;
pub mod text_break;
pub mod text_detect;
pub use text_break::{LineBreakKind, line_break_opportunities};
pub mod close_admission;
/// The raw operation protocol between a host and the toolkit's widgets: the
/// operation the host admits, how a widget starts it, and the routing to a
/// widget's children. For the toolkit's hosts (the app framework, the shell,
/// the browser runtime); a session app before any compositor exists hosts its
/// typed effects through `widget::resource_effects` and never names these
/// (`tools/policy/check_app_resource_action_contract.py`).
pub mod host_operation {
    pub use crate::widget::resource_operation::{
        OperationRequest, OperationTarget, ResourceOperation, ResourceOperationStart,
        ResourceOperations, local_resource_operation_handlers, poll_resource_operation,
        resource_operation_handlers, start_in_children, start_operation,
    };
}
#[cfg_attr(
    not(any(feature = "native", test)),
    expect(dead_code, reason = "only a native host's frame culls its draw")
)]
pub(crate) mod draw_cull;
pub mod host_palette;
pub mod published;
pub mod reader_hud;
pub mod record;
pub mod remote_actions;
pub mod render_bump_recovery;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod render_thread;
pub mod reorder;
pub mod resources;
pub mod retained_present;
pub mod retained_raster;
pub mod richtext;
pub mod save_status;
pub mod scale;
pub mod scene_parts;
pub mod screen_reader;
pub mod scroll_copy;
pub mod search;
pub mod secret_paint;
pub mod selection;
pub mod session_door;
pub mod shared_selection;
pub mod shell_tabs;
mod slot_row;
mod standalone_history;
mod standalone_history_host;
pub mod state;
pub mod surface;
pub mod sysdirs;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod text_edit;
pub mod text_metrics;
pub mod text_position;
pub mod text_render;
pub mod text_selection;
pub mod theme;
mod theme_document;
mod theme_layers;
pub mod tokens;
pub mod type_ahead;
pub mod ui_graph;
pub mod undo;
pub mod uri;
pub mod view;
pub mod virtual_viewport;
pub mod window_door;
pub mod window_pictures;
pub mod window_share;
pub mod window_slots;
pub mod window_toolbar;
pub mod workspace_emoji;
pub mod workspace_identity;
#[cfg(feature = "native")]
pub use native::warmup;
/// Private completion receipts shared by native and browser text editors.
/// These types grant no access to an embedded browser or its engine.
pub mod text_input {
    pub use crate::webview::text_input::{Failure, Receipt, Resolver};
}
pub mod webview;
pub mod widget;
pub mod window_icon;

// Host services — the clock, background work, clipboard, file choosers, URI
// handlers, notifications, media sessions, location and screen capture — are
// their own crate below the toolkit. They keep their toolkit paths
// (`tilo_ui::portal`, `tilo_ui::host`, `tilo_ui::time`, …).
#[doc(inline)]
pub use tilo_host::{
    background, capture, clipboard, env, host, jpeg, location, models, mpris, notify, png, portal,
    secrets, shell_choosers, time, wake,
};

// `kurbo` and `peniko` come through `vello`, where the geometry and paint
// types in every `draw` signature live.
/// The AccessKit types the bridge and [`headless::FrameHarness`] hand back,
/// re-exported so an app crate can name them (`tilo_ui::accesskit::Role`) without
/// taking its own dependency on a version that must match this one exactly.
pub use accesskit;
pub use automerge;
pub use taffy;
/// Locks that hand out a poisoned lock's guard (`ui::lock::MutexRecover`).
pub use tilo_lock as lock;
/// The async runtime model: the app runtime off the UI thread, the CPU pool,
/// the file-I/O pool and `block_on` for synchronous entry points.
pub use tilo_rt as rt;
pub use vello;
pub use vello::{kurbo, peniko};
pub use winit;

#[cfg(not(target_arch = "wasm32"))]
pub use app::{App, local_surface_id};
pub use arriving::{Arrival, ArrivalClock, ArrivalCompletion, ArrivalWriter, Arriving};
pub use caps_lock::CapsLockEstimate;
pub use collection_transition::{
    KeyedRowSnapshot, KeyedTransitionSet, RowGeometry, RowMotion, RowMotions, TransitionVisual,
};
pub use color_compat::ColorCompat;
pub use content_layout::{FocusedContentLane, ResponsiveControlBar};
pub use doc::{TextEdit, put_text_diffed, splice_text_diffed};
pub use i18n::{ArgValue, EmbeddedCatalog, I18nError, I18nManager, PseudoLocale};
pub use image_store::{Fetcher, HostImageLoader, ImageStore};
pub use input::{
    ActionBindings, BindingConflict, BindingSet, ControllerBinding, ControllerButton, InputBinding,
    KeyLegend, KeyboardBinding, KeyboardModifier, TouchGesture,
};
pub use interaction::{
    ControllerFamily, InputAvailability, InputDevice, InputModality, InteractionEnvironment,
    KeyboardFamily, PointerPrecision, TouchClaim, TouchPointerEmulator, TouchPointerEvent,
};
pub use keycaps::{KeycapLanguage, Keycaps};
pub use local_state::{
    Intents, LatestRequest, LatestTask, LocalState, LocalStateWriter, RequestCompletion,
    TaskCompletion,
};
pub use nav_stack::{EdgeSwipeBack, NavAction, NavStack};
pub use paragraph::{ComposedLine, ParagraphBox, ParagraphOptions, compose_paragraph};
pub use platform::{DialogActionOrder, Platform, PlatformConventions, Shortcut};
pub use portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
pub use profiling::{
    FRAME_BUDGET_US, FrameProfiler, FrameTiming, Pass, PerfReport, PerfScenario, PerfStep,
    StatSummary, run_scenario, run_scenario_on,
};
pub use reader_hud::{
    HudAction, HudControls, HudPlacement, HudSide, HudSwatch, MarkerDrag, SelectionHud,
};
pub use record::{
    RecordCheckout, RecordId, RecordKind, RecordRef, RecordRefusal, RecordSet, RecordStore,
};
pub use remote_actions::{
    CancellationCall, CatalogueCall, Followed, InvocationCall, RemoteActions, RemoteActionsGuard,
};
pub use reorder::{
    DropCandidate, DropPlacement, DropTarget, ReorderController, ReorderDrop, ReorderMotion,
};
pub use search::{
    FnProvider, ListIndex, SearchGroup, SearchProvider, SearchRegistry, SearchResult,
};
pub use selection::{KeySelection, SelectionModel};
pub use state::{
    DocumentDiagnostics, DocumentFrontier, FrontierDiagnostics, Revision, RevisionHistory,
    RevisionId, State, UndoEntry, UndoLog, UnknownRevision,
};
pub use text_break::discretionary_hyphen_breaks;
pub use text_break::{Hyphenate, MakeHyphenation, builtin_hyphenation, hyphenation_generation};
#[cfg(feature = "lazy-hyphenation")]
pub use text_break::{defer_hyphenation, hyphenation_load_failed, install_hyphenation};
pub use theme::{
    AppearanceIntent, ContentSurface, SurfaceShape, Theme, WindowAppearance, resolve_theme_layers,
    workspace_hue,
};
pub use ui_graph::{
    LIVE_GRAPH_DIR_ENV, LIVE_GRAPH_OPERATION, LiveUi, UI_GRAPH_VERSION, UiBuildContext,
    UiButtonRole, UiComponentRequest, UiDocument, UiDocumentExt, UiGraphEdit, UiGraphEditor,
    UiGraphError, UiGraphHandle, UiNavigation, UiNode, UiTextRole, UiTextSource, UiValues,
    parse_ui_graph_edits,
};
pub use undo::{UndoAccess, UndoRequest, UndoStatus};
pub use view::{
    FROZEN_VERSION, FocusView, FreezeThaw, FrozenApp, ThawError, View, ViewContribution, ViewRect,
};
pub use virtual_viewport::{
    DragScrollFinish, DragScrollUpdate, DragScroller, KineticSampler, ReconcileError,
    ScrollAlignment, ScrollGlide, ScrollMomentum, SparseAnchor, SparseVirtualViewport,
    VirtualViewport, WheelStep,
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

pub use action_palette::ActionPalette;
pub use tilo_actions;
/// The record and effect vocabulary a published action belongs to.
pub use tilo_resource;

/// The word-boundary corrections every text field applies, for an input
/// method that commits whole words itself (the on-screen keyboard).
pub mod autocorrect {
    pub use spellcheck::contraction;
}
pub use widget::ModelView;
pub use widget::command_palette::{
    ArgumentSpec, CommandEntry, CommandOutcome, CommandPalette, PaletteMode, RowFace, SearchScope,
};
pub use widget::{AppCommand, AppKind, AppWidget, IconDescriptor};
pub use widget::{Card, CardGrid, Section, card_column_width, card_columns};
pub use widget::{ContentKind, NavItem, NavList, NavMode, NavPane, NavSection, NavShell};
pub use widget::{EditOutcome, EditableLabel};
pub use widget::{Element, Widget};
pub use widget::{
    NativeView, NativeViewAccessibility, NativeViewBackend, NativeViewGeometry, NativeViewHost,
    NativeViewInput,
};

pub use widget::{RouteError, RouteHandle, Router};
// Audit-timeline widgets + their shared types. The `ArtifactKind`
// re-export flows through these widgets, so callers can build
// a `HistoryEntry` without a direct dependency on the collab-types crate.
pub use history_mode::{
    HistoryController, HistoryError, HistoryResource, HistorySnapshot, PreviewSide, RestoreReview,
};
pub use widget::activity_timeline::{ActivityTimeline, BandUndoRequest};
pub use widget::history::{
    AuthorRelation, EntryKind, HistoryAttribution, HistoryAuthor, HistoryClassification,
    HistoryEntry, HistoryEntryId, HistoryFilter, RedactionDisplay, format_relative_time,
    hue_to_rgb,
};
pub use widget::history_group::{
    GroupNameRequest, GroupingConfig, HistoryGroup, MAX_GROUP_DEPTH, NamedRange, OpFamily,
    group_entries,
};
pub use widget::name_field::{NameField, NameFieldOutcome};
// Re-exported so an app reaches the whole history stack through `tilo_ui::`
// without knowing the module layout.
pub use history_source::groups as history_groups;
pub use history_source::{
    ActivityBrief, ActivityBriefAction, ActivityDigest, ActivityNarrator, AuthorDirectory,
    CompareError, GroundedNarrative, HistoryComparison, HistoryFacetIndex, HistoryResourceTarget,
    HistorySeenFrontier, HistorySelectionDetail, HistorySource, NarrativeRequest, NarrativeSource,
    compare_entries, deterministic_narrative, heads_for_entry, human_label,
    participant_display_label, validate_narrative, version_index_for_entry,
};
pub use live_collaboration::{
    Availability, CallPresence, IDLE_AFTER, INFERRED_STATUS_TTL, LiveStatus, MAX_STATUS_CHARS,
    PresenceVisibility, StatusOrigin, StatusSharing,
};
// Undoing a run of changes that never reached anybody's undo stack — a
// remote author's, an agent's.
pub use gesture::{DragSelect, Granularity};
pub use history_source::revert::{
    ChangeRun, KeptSlot, RevertError, RevertOutcome, Slot, latest_run_by, plan_revert, revert_run,
};
pub use history_source::run_undo::{
    RunUndoReport, StepOutcome, StepState, refuse_run, undo_run, undo_step, unreachable_steps,
};
pub use history_source::runs::{
    AgentRun, RunStep, forget_run, read_runs, record_run, record_run_with_details, run_starting_at,
};
pub use richtext::{BaselineShift, FontFamily, RichSpan, RichTextLayout, TextAlign};
pub use save_status::{
    SAVE_GIVE_UP, SAVE_RETRY_ACTION, SAVE_STALL, SaveAction, SaveNotice, SaveState,
    SaveStatusSurface,
};
pub use text_edit::{
    CaretWord, CharFilter, CharSplice, Edit, EditCore, Motion, SUGGESTION_SETTLE, TypingRun,
    delete_last_grapheme, word_at_caret,
};
pub use text_render::{
    FontChoice, FontVariation, Fragmentation, GlyphOutline, HangingPunctuation, Hyphenation,
    NumericStyle, OpenTypeFeature, OpenTypeTag, OpticalSizing, ParagraphAlign, PolygonTextLayout,
    TextFlowRegion, TextFlowRegionError, TextGround, TextKind, TextLayoutConstraints,
    TextLayoutEngine, TextRenderer, TextScaling, TextStyle, TextWeight, TextWrapMode,
    UI_HYPERLEGIBLE_FAMILY, UI_MONOSPACE_FAMILY, UI_SANS_FAMILY, UI_SERIF_FAMILY,
    configured_font_system,
};
pub use text_selection::{
    CopyWatch, Selection, TextAffinity, TextPosition, clipboard_copy, clipboard_copy_image,
    clipboard_copy_primary, clipboard_copy_rich_accepted, clipboard_paste, clipboard_paste_image,
    clipboard_paste_primary, clipboard_paste_rich, extend_span_drag,
    grapheme_boundary_at_or_before, line_boundaries, next_char_boundary, next_word_boundary,
    next_word_boundary_chars, prev_char_boundary, prev_word_boundary, prev_word_boundary_chars,
    try_clipboard_copy, try_clipboard_copy_secret, word_boundaries, word_boundaries_chars,
};
pub use tokens::{ReadingFace, TextRole, UiFace};

pub use animation::Lerp;

pub use presence::{PathSegment, PresenceCtx, PresenceKey, SelectionRange};
pub use wake::{WakeBindError, WakeEvent, WakeHandle, WakeScope, WakeScopeGuard};
// The capability model (`collab_types::caps`), reached through the
// toolkit. These are re-exports, not toolkit-local copies — see
// `permissions/mod.rs`.
pub use collaboration::PresencePayload;
pub use permissions::{
    Access, ActionKind, Actor, CapDecision, CapOverride, PermissionContext, PermissionScope,
    ResourceRef, Role, Scope as CapScope, Subject, UserId,
};
pub use widget::ParticipantPresence;

pub mod application;
pub mod application_view;
#[cfg(not(target_arch = "wasm32"))]
pub mod host_worker;
pub mod invocation_scope;
#[cfg(not(target_arch = "wasm32"))]
pub mod native_scenario;
#[cfg(feature = "native")]
mod physical_input_trace;
#[cfg(all(feature = "native", feature = "test-support"))]
#[doc(hidden)]
pub use physical_input_trace::Trace as NativePhysicalInputTrace;
pub mod semantic_document_read;
pub mod synthetic_input;

pub mod bound_dispatch;
pub(crate) mod retained_widgets;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod presentation_feedback;

/// Emission-time local control custody.
pub mod control_custody;
pub use control_custody::{
    ControlCoverage, ControlDomain, ControlOutbox, ControlProducerDiscipline,
};
mod focus_navigation;

pub use widget::{KeyboardMap, KeyboardMapInput, KeyboardMapKey, KeyboardMapSelection};
