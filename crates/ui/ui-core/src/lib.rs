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
pub(crate) mod a11y_child_route;
pub mod a11y_tree;
pub(crate) mod a11y_walk;
#[cfg(all(feature = "native", target_os = "android"))]
mod accesskit_android_adapter;
pub mod accesskit_bridge;
mod action_locus;
/// The document action host behind the framework's dispatcher. Apps submit edits through the framework,
/// never through this host.
#[doc(hidden)]
pub mod local_dispatch;
#[cfg(feature = "model")]
pub mod local_document_action;
pub use action_locus::{ActionUiRoute, PaintedActionMarker};
#[cfg(feature = "model")]
pub mod action_palette;
pub mod agent_identity;
pub mod animation;
pub mod arriving;
/// What wants a person: the waiting stack, and the log an outcome goes to when it does not.
pub mod attention;
pub mod author;
pub mod capability;
pub mod content_zoom;
pub mod control_hints;
pub mod control_notice;
/// What is running: one cancellable job with a body, which the whole system can see and which outlives
/// the window that started it.
pub mod jobs;
pub mod motion;
/// The actions the host publishes itself, named for apps to call through `ui`.
pub use tilo_app_id::{apps, chat};
pub mod shell;
extern crate self as tilo_ui;
#[cfg(test)]
#[allow(
    dead_code,
    reason = "shared support is compiled by library and integration tests whose consumers intentionally use different subsets"
)]
#[path = "../tests/support/application.rs"]
pub(crate) mod application_test_support;

pub mod app;
/// The app surface an app's changes are decided against, kept with the collab client that names it on
/// every signed op.
#[cfg(feature = "model")]
pub use collab_types::app_surface;
pub mod auth_backdrop;
pub mod backdrop;
#[cfg(feature = "model")]
pub mod call_pictures;
pub mod caps_lock;
/// Who the keyboard belongs to, which is who paints a caret.
pub mod caret_owner;
pub mod chart_palette;
pub mod charts;
pub mod chrome;
pub mod clock;
#[cfg(feature = "model")]
pub mod collaboration;
#[cfg(feature = "model")]
pub use runtime_collab::native_media;
#[cfg(feature = "model")]
pub use runtime_collab::shown_actions;
pub mod collection_transition;
pub use tilo_ui_paint::{color, color_compat};
pub mod carry_out;
pub mod component;
pub mod component_catalogue;
pub mod component_inputs;
pub mod component_lifecycle;
pub mod composition;
pub mod content;
pub mod content_layout;
mod cpu_raster;
pub mod damage;
pub mod date;
pub mod device_local_api;
pub mod diag;
/// Zero-copy DMA-BUF → `wgpu` texture import via Vulkan external memory.
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
pub mod files;
pub mod floating_layer;
pub mod floating_surface;
pub mod focus_visible;
pub mod font_catalog;
pub mod format;
pub mod fullscreen;
pub mod gesture;
pub mod gpu;
/// Painting decoded video on the GPU: uploading a hardware decoder's planar output and converting YUV
/// to RGB in a shader.
pub mod gpu_video;
pub mod headless;
pub mod history_mode;
#[cfg(feature = "model")]
pub mod history_source;
#[cfg(feature = "native")]
mod host_appearance;
mod host_chrome;
pub mod host_data;
pub mod hosted_window;
/// What a frame shows an inspector, a screen reader and an automated check: the widget and
/// semantic tree, the named targets a caller addresses, and the accessibility issues it reports.
pub mod inspection;
#[cfg(feature = "model")]
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
pub mod input_generation;
pub mod interaction;
pub mod keycaps;
pub mod keys;
pub mod latin_chord;
pub mod launch;
pub mod lift;
pub mod live_activity;
pub mod live_pictures;
pub mod local_identity;
pub mod local_state;
pub mod math;
#[cfg(feature = "native")]
pub mod native;
pub mod nav_stack;
pub mod num;
pub mod offscreen;
pub mod outside_paint;
pub mod overlay;
pub mod paint;
pub mod part_paint;
pub mod passcode_keyboard;
pub mod pen;
pub mod people_photos;
pub mod permissions;
pub mod person_card;
pub mod pick;
pub mod pick_wire;
pub mod pictures;
pub mod platform;
pub mod pointer_replay;
pub mod presence;
pub mod press;
pub mod profiling;
pub mod query_text;
pub use tilo_ui_paint::raster;
pub mod storage;
/// Where each settings section is kept, and so published: `settings/<section>` for the person's,
/// `devices/<device>/settings/<section>` for the device's.
#[cfg(feature = "model")]
pub use settings_store::placement as settings_placement;
/// The verb a setting is set by, `set-<field>`: each field of a settings section has its own.
#[cfg(feature = "model")]
pub use settings_store::workspace::verb as setting_verb;
pub mod settings_device;
pub mod settings_source;
pub mod workspace_sync;
/// The shell's keyboard shortcut table (actions, their chords and the defaults) as the settings
/// document keeps it: what every shell host resolves a key press through, the native shell and a
/// browser page alike.
#[cfg(feature = "model")]
pub mod shortcut_table {
    pub use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
    pub use settings_store::keyboard_shortcuts::{
        FixedAction, ShortcutHome, fixed_action, fixed_shortcut,
    };
}
/// The widgets and the screen edges things kept at the screen's edges wait at, as the settings
/// document keeps them: what every shell host places them by, the native shell and a browser page
/// alike.
#[cfg(feature = "model")]
pub mod widgets {
    pub use settings_store::widgets::{
        DockEdge, Place, ShowMode, WidgetId, WidgetPrefs, WidgetsPrefs, prefs_key,
    };
}
pub use query_text::{QueryComposition, QueryEventChange, QueryImeChange, QueryInk, QueryView};
pub use tilo_lock::deferred;
pub mod personal_dictionary;
pub mod text_assistance;
pub mod text_break;
pub mod text_detect;
pub use text_break::{LineBreakKind, line_break_opportunities};
pub mod close_admission;
/// The raw operation protocol between a host and the toolkit's widgets: the operation the host admits,
/// how a widget starts it, and the routing to a widget's children. For the toolkit's hosts (the app
/// framework, the shell, the browser runtime, the installer); an app answers operations as a typed
/// effect vocabulary and never names these (`tools/policy/check_app_resource_action_contract.py`).
pub mod host_operation {
    #[cfg(feature = "model")]
    pub use crate::widget::resource_operation::OperationTarget;
    pub use crate::widget::resource_operation::{
        LAY_OUT_FIRST, OperationRequest, ResourceOperation, ResourceOperationStart,
        ResourceOperations, asks_to_lay_out_first, host_lays_out_first, lay_out_first,
        laying_out_first, local_resource_operation_handlers, poll_resource_operation,
        resource_operation_handlers, start_in_children, start_operation,
    };
}
mod app_core;
pub mod draw;
pub(crate) mod draw_cull;
#[cfg(feature = "model")]
pub mod mentions;
pub mod own_paint;
pub mod published;
pub mod reader_hud;
#[cfg(feature = "model")]
pub mod remote_actions;
pub mod render_bump_recovery;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod render_thread;
pub mod reorder;
#[cfg(feature = "model")]
pub mod resource;
pub mod retained_present;
pub mod retained_raster;
pub mod richtext;
/// Rich text as a component shows it: paragraphs with their marks, and how each reference in them reads
/// (`ui.BlockEditor`'s value), the one model the runtime sends and the window draws.
pub use tilo_ui_graph::rich;
mod part_row;
pub mod save_status;
pub mod scale;
pub mod scene_parts;
pub mod screen_reader;
pub mod screen_reader_chord;
pub mod screen_reader_touch;
pub mod scroll_copy;
pub mod search;
pub mod secret_paint;
pub mod selection;
pub mod session_api;
pub mod shared_selection;
pub mod shared_subject;
pub mod shell_cards;
pub mod shell_tabs;
pub mod snap;
pub mod state;
pub mod step;
pub mod strip_layers;
pub mod surface;
/// The actions socket a session hands a surface app is a descriptor of a native process: a browser page
/// is no surface, and no session places its window.
#[cfg(all(feature = "native", feature = "model"))]
pub mod surface_peer;
pub mod surface_services;
pub mod sysdirs;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod text_edit;
pub mod text_match;
pub mod text_metrics;
pub mod text_position;
pub mod text_render;
pub mod text_selection;
pub mod theme;
pub mod theme_bundle;
mod theme_document;
mod theme_layers;
#[cfg(feature = "model")]
mod theme_record;
pub mod tokens;
pub mod type_ahead;
pub mod ui_graph;
pub mod undo;
pub mod uri;
pub mod view;
pub mod virtual_viewport;
pub mod window_link;
pub mod window_name;
pub mod window_parts;
pub mod window_pictures;
pub mod window_share;
pub mod window_targets;
pub mod window_toolbar;
pub mod workspace_emoji;
#[cfg(feature = "native")]
pub use native::warmup;
/// Private completion receipts shared by native and browser text editors.
pub mod text_input {
    pub use tilo_webview::text_input::{Failure, Receipt, Resolver};
}
pub mod key_input;
pub mod terminal;
pub mod webview;
pub mod widget;
pub mod window_icon;

/// The model services an app's inference and provider administration travel as.
#[cfg(feature = "model")]
#[doc(inline)]
pub use tilo_process_services::models;
#[doc(inline)]
pub use tilo_process_services::{
    background, capture, clipboard, env, jpeg, location, mpris, notify, png, portal, process,
    secrets, shell_choosers, time, wake,
};

/// The AccessKit types the bridge and [`headless::Driver`] hand back, re-exported so an app crate
/// can name them (`tilo_ui::accesskit::Role`) without taking its own dependency on a version that must
/// match this one exactly.
pub use accesskit;
pub use automerge;
pub use taffy;
/// Locks that hand out a poisoned lock's guard (`ui::lock::MutexRecover`).
pub use tilo_lock as lock;
/// The async runtime model: the app runtime off the UI thread, the CPU pool, the file-I/O pool and
/// `block_on` for synchronous entry points.
pub use tilo_rt as rt;
pub use vello;
pub use vello::{kurbo, peniko};
pub use vello_encoding;
pub use winit;

pub use app::App;
pub use arriving::{Arrival, ArrivalClock, ArrivalCompletion, ArrivalWriter, Arriving};
pub use caps_lock::CapsLockEstimate;
pub use collection_transition::{RowMotion, RowMotions};
pub use color_compat::ColorCompat;
pub use content_layout::{FocusedContentArea, ResponsiveControlBar};
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
pub use platform::local_surface_id;
// Paragraph composition is the document engine's, so the reader, the word processor and PDF
// export lay text out by one policy.
pub use pictures::{BLOB_SCHEME, Pictures, Shown as ShownPicture};
pub use pictures::{lend_content, set_remote_fetcher};
pub use platform::{
    DialogActionOrder, HostShortcut, Platform, PlatformConventions, Shortcut, spell_shortcut,
    spell_shortcut_keycaps,
};
pub use portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
pub use profiling::{
    FRAME_BUDGET_US, FrameProfiler, FrameTiming, Pass, PerfReport, PerfScenario, PerfStep,
    StatSummary, run_scenario, run_scenario_on,
};
pub use reader_hud::{
    HudAction, HudControls, HudPlacement, HudSide, HudSwatch, MarkerDrag, SelectionHud,
};
#[cfg(feature = "model")]
pub use remote_actions::{
    CancellationCall, CatalogueCall, ChangedCall, Followed, FollowedRead, InvocationCall,
    RemoteActions, RemoteActionsGuard, ResourceReadCall,
};
pub use reorder::{
    DropCandidate, DropPlacement, DropTarget, ReorderController, ReorderDrop, ReorderMotion,
};
#[cfg(feature = "model")]
pub use resource::{
    DocumentCheckout, DocumentId, DocumentRef, ResourceRefusal, ResourceSet, ResourceStore,
    ResourceType,
};
pub use search::{FnProvider, SearchGroup, SearchProvider, SearchRegistry, SearchResult};
pub use selection::{KeySelection, SelectionModel};
pub use state::State;
#[cfg(feature = "model")]
pub use state::{DocumentFrontier, UndoEntry, UndoLog};
pub use text_break::discretionary_hyphen_breaks;
pub use theme::{
    AppearanceIntent, ContentSurface, SurfaceShape, Theme, WindowAppearance, resolve_theme_layers,
    workspace_hue,
};
pub use tilo_doc::paragraph::{ComposedLine, ParagraphBox, ParagraphOptions, compose_paragraph};
/// The one image codec: sniff, decode, encode and transform pixels, with the browser's own codecs on
/// the web.
pub use tilo_image_codec as image_codec;
pub use ui_graph::{
    GraphPropKind, LIVE_GRAPH_DIR_ENV, LIVE_GRAPH_OPERATION, LiveUi, UI_GRAPH_VERSION,
    UiBuildContext, UiButtonRole, UiComponentRequest, UiDocument, UiDocumentExt, UiGraphEdit,
    UiGraphEditor, UiGraphError, UiGraphHandle, UiNavigation, UiNode, UiTextRole, UiTextSource,
    UiValues, parse_ui_graph_edits,
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
pub use a11y::SemanticId;
pub use a11y::{A11ySettings, AccessBounds, AccessNode, AccessState, AccessibleTextSelection};
pub use a11y_tree::{SemanticAction, SemanticActionKind, SemanticNode, SemanticTree, TreeDelta};

pub use screen_reader::{ScreenReader, describe, flatten_reading_order};

#[cfg(feature = "model")]
pub use action_palette::ActionPalette;
pub use tilo_actions;

/// The word-boundary corrections every text field applies, for an input method that commits whole words
/// itself (the on-screen keyboard).
pub mod autocorrect {
    pub use spellcheck::contraction;
}
#[cfg(feature = "model")]
pub use action_palette::{ArgumentKind, ArgumentSpec, CommandEntry, CommandOutcome, RowFace};
pub use draw::{DrawCtx, DrawEnv, WidgetOverlay};
pub use widget::ModelView;
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
#[cfg(feature = "model")]
pub use history_mode::{
    HistoryController, HistoryError, HistoryResource, HistorySnapshot, PreviewSide, RestoreReview,
};
pub use widget::HistoryEntryId;
#[cfg(feature = "model")]
pub use widget::activity_timeline::{ActivityTimeline, BandUndoRequest};
#[cfg(feature = "model")]
pub use widget::history::{
    AuthorRelation, EntryKind, HistoryAttribution, HistoryAuthor, HistoryClassification,
    HistoryEntry, HistoryFilter, RedactionDisplay, format_relative_time, hue_to_rgb,
};
#[cfg(feature = "model")]
pub use widget::history_group::{
    FOLD_ACTION_FACET, FOLD_ITEM_FACET, GroupNameRequest, GroupingConfig, HistoryGroup,
    MAX_GROUP_DEPTH, NamedRange, OpFamily, group_entries,
};
pub use widget::name_field::{NameField, NameFieldOutcome};
// Re-exported so an app reaches the whole history stack through `tilo_ui::`
// without knowing the module layout.
#[cfg(feature = "model")]
pub use collab_types::presence::CallPresence;
#[cfg(feature = "model")]
pub use history_source::groups as history_groups;
#[cfg(feature = "model")]
pub use history_source::sentence::{
    AuthorParts, attribution_hover, author_line, author_parts, author_spoken, learn_item_names,
    wire_author_line, writer_badge,
};
#[cfg(feature = "model")]
pub use history_source::{
    ActivityBrief, ActivityBriefAction, ActivityDigest, ActivityNarrator, AuthorDirectory,
    CompareError, GroundedNarrative, HistoryComparison, HistoryFacetIndex, HistoryResourceTarget,
    HistorySelectionDetail, HistorySource, NarrativeRequest, NarrativeSource, compare_entries,
    deterministic_narrative, heads_for_entry, validate_narrative, version_index_for_entry,
};
pub use tilo_action_policy::presence::{
    Availability, IDLE_AFTER, INFERRED_STATUS_TTL, LiveStatus, MAX_STATUS_CHARS,
    PresenceVisibility, StatusOrigin, StatusSharing,
};
#[cfg(feature = "model")]
pub use widget::history::author_face;
// Undoing a run of changes that never reached anybody's undo stack — a
// remote author's, an agent's.
pub use gesture::{DragSelect, Granularity};
#[cfg(feature = "model")]
pub use history_source::invocations::{
    AgentSession, InvocationStep, forget_invocation, invocation_starting_at, read_invocations,
    record_invocation, record_invocation_with_details,
};
#[cfg(feature = "model")]
pub use history_source::revert::{
    ChangeRange, KeptSlot, RevertError, RevertOutcome, Slot, latest_invocation_by, plan_revert,
    revert_invocation,
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
    CopyWatch, Selection, TextAffinity, TextPosition, clipboard_available, clipboard_copy,
    clipboard_copy_image, clipboard_copy_primary, clipboard_copy_rich_accepted, clipboard_paste,
    clipboard_paste_image, clipboard_paste_primary, clipboard_paste_rich, extend_span_drag,
    grapheme_boundary_at_or_before, line_boundaries, next_char_boundary, next_word_boundary,
    next_word_boundary_chars, prev_char_boundary, prev_word_boundary, prev_word_boundary_chars,
    reserve_clipboard_copy, try_clipboard_copy, try_clipboard_copy_secret, word_boundaries,
    word_boundaries_chars,
};
pub use tokens::{ReadingFace, TextRole, UiFace};

pub use animation::Lerp;

#[cfg(feature = "model")]
pub use collaboration::PresencePayload;
#[cfg(feature = "model")]
pub use permissions::{
    Access, CapDecision, CapOverride, PermissionContext, PermissionScope, ResourceRef, Role,
    Scope as CapScope,
};
pub use permissions::{Acting, ActionKind, Grantee, MemberId};
pub use presence::{PathSegment, PresenceCtx, PresenceKey, SelectionRange, human_label};
pub use wake::{WakeBindError, WakeEvent, WakeHandle, WakeScope, WakeScopeGuard};
#[cfg(feature = "model")]
pub use widget::ViewPresence;

#[cfg(feature = "model")]
pub mod application;
#[cfg(feature = "model")]
pub mod application_view;
#[cfg(feature = "model")]
pub mod invocation_scope;
pub mod native_scenario;
#[cfg(feature = "native")]
mod physical_input_trace;
#[cfg(all(feature = "native", feature = "test-support"))]
#[doc(hidden)]
pub use physical_input_trace::Trace as NativePhysicalInputTrace;
#[cfg(feature = "native")]
pub mod input_frame;
#[cfg(feature = "model")]
pub mod semantic_document_read;
pub mod semantic_outline;
#[cfg(feature = "model")]
pub mod semantic_readers;
pub mod synthetic_input;

pub mod bound_dispatch;
pub(crate) mod retained_widgets;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod presentation_feedback;

/// Emission-time local control ownership.
pub mod control_domain;
pub use control_domain::{
    ControlCoverage, ControlDomain, ControlOutbox, ControlProducerDiscipline,
};
mod focus_navigation;

pub use widget::{KeyboardMap, KeyboardMapInput, KeyboardMapKey, KeyboardMapSelection};
