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

/// The app's own client for the session's action API, and what it answers.
#[cfg(feature = "model")]
pub mod remote_actions;
#[cfg(feature = "model")]
pub use {
    remote_actions::CancellationCall, remote_actions::CatalogueCall, remote_actions::ChangedCall,
    remote_actions::Followed, remote_actions::FollowedRead, remote_actions::InvocationCall,
    remote_actions::InvocationProgress, remote_actions::RemoteActions,
    remote_actions::RemoteActionsGuard, remote_actions::ResourceReadCall,
};
pub mod settings_device;
pub mod surface_services;

pub mod a11y_tree;
mod action_locus;
/// The document action host behind the framework's dispatcher. Apps submit edits through the framework,
/// never through this host.
#[doc(hidden)]
pub mod local_dispatch;
#[cfg(feature = "model")]
pub mod local_document_action;
pub use {action_locus::ActionUiRoute, action_locus::PaintedActionMarker};
#[cfg(feature = "model")]
pub mod action_palette;
pub mod author;
pub mod content_zoom;
pub mod control_hints;
pub mod control_notice;
/// What is running: one cancellable job with a body, which the whole system can see and which outlives
/// the window that started it.
pub mod jobs;
/// The person's presence said and heard on the connection an app's launcher handed it, which
/// the runner installs before the app starts.
#[cfg(not(target_arch = "wasm32"))]
pub mod presence_line;
/// The actions the host publishes itself, named for apps to call through `ui`.
pub use {tilo_app_id::apps, tilo_app_id::chat};
pub mod shell;
extern crate self as tilo_ui;
/// The base every widget is built on (the widget contract, layout, text, theming, input and
/// accessibility), named here as before it was a crate of its own: a program that depends on
/// `tilo-ui` alone writes `tilo_ui::Widget`, `tilo_ui::LocalState` and `tilo_ui::TextRole`. What
/// this crate names itself takes precedence over the base's.
pub use tilo_ui_base::*;
#[cfg(test)]
#[allow(
    dead_code,
    reason = "shared support is compiled by library and integration tests whose consumers intentionally use different subsets"
)]
#[path = "../../ui-app/tests/support/application.rs"]
pub(crate) mod application_test_support;

pub mod app;
/// The app surface an app's changes are decided against, kept with the collab client that names it on
/// every signed op.
#[cfg(feature = "model")]
pub use collab_types::app_surface;
pub mod auth_backdrop;
#[cfg(feature = "model")]
pub mod call_pictures;
pub mod charts;
#[cfg(feature = "model")]
pub use runtime_collab::shown_actions;
#[cfg(feature = "model")]
pub use tilo_actions::remote::native_media;
pub use {tilo_ui_paint::color, tilo_ui_paint::color_compat};
#[cfg(feature = "model")]
pub mod activity_wire;
pub mod component_catalogue;
pub mod component_lifecycle;
pub mod composition;
pub mod content;
mod cpu_raster;
pub mod device_local_api;
/// Zero-copy DMA-BUF → `wgpu` texture import via Vulkan external memory.
#[cfg(all(feature = "dmabuf-import", target_os = "linux"))]
pub mod dmabuf_import;
pub mod edge_slots;
pub mod element_pictures;
pub mod files;
pub mod floating_layer;
pub mod floating_surface;
#[cfg(any(test, feature = "test-support"))]
pub mod frame_probe;
/// Painting decoded video on the GPU: uploading a hardware decoder's planar output and converting YUV
/// to RGB in a shader.
pub mod gpu_video;
pub mod headless;
pub mod history_mode;
#[cfg(feature = "model")]
pub mod history_source;
mod host_chrome;
#[cfg(not(all(windows, feature = "native")))]
#[path = "host_notifications_unsupported.rs"]
pub mod host_notifications;
#[cfg(all(windows, feature = "native"))]
pub use tilo_process_services::notify::windows_toasts as host_notifications;
pub mod hosted_window;
/// What a frame shows an inspector, a screen reader and an automated check: the widget and
/// semantic tree, the named targets a caller addresses, and the accessibility issues it reports.
pub mod inspection;
pub mod launch;
pub mod launch_error;
pub mod lift;
pub mod live_activity;
pub mod live_pictures;
pub mod math;
#[cfg(feature = "model")]
pub mod owner_wake;
pub mod passcode_keyboard;
pub mod pick;
pub mod pick_wire;
pub mod pictures;
/// The app's own vello pipelines, kept across launches in its cache directory.
#[cfg(feature = "native")]
pub(crate) mod pipeline_cache;
/// The session's read-only vello pipeline cache every app starts from.
pub mod pipeline_seed;
pub mod pointer_replay;
pub mod profiling;
pub mod shot_frame;
pub use tilo_ui_paint::raster;
pub mod storage;
/// Where each settings section is kept, and so published: `settings/<section>` for the person's,
/// `devices/<device>/settings/<section>` for the device's.
#[cfg(feature = "model")]
pub use settings_store::placement as settings_placement;
/// The verb a setting is set by, `set-<field>`: each field of a settings section has its own.
#[cfg(feature = "model")]
pub use settings_store::workspace::verb as setting_verb;
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
pub use tilo_lock::deferred;
mod app_core;
pub mod close_admission;
pub mod reader_hud;
pub mod render_bump_recovery;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod render_thread;
#[cfg(feature = "model")]
pub mod resource;
pub mod retained_present;
pub mod retained_raster;
pub mod text_assistance;
/// Rich text as a component shows it: paragraphs with their marks, and how each reference in them reads
/// (`ui.BlockEditor`'s value), the one model the runtime sends and the window draws.
pub use tilo_ui_graph::rich;
mod part_row;
pub mod save_status;
pub mod search;
pub mod session_api;
pub mod shared_selection;
pub mod shell_cards;
pub mod shell_tabs;
pub mod state;
pub mod strip_layers;
/// The actions socket a session hands a surface app is a descriptor of a native process: a browser page
/// is no surface, and no session places its window.
#[cfg(all(feature = "native", feature = "model"))]
pub mod surface_peer;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod ui_graph;
pub mod undo;
pub mod view;
pub mod window_link;
pub mod window_name;
pub mod window_parts;
pub mod window_pictures;
pub mod window_share;
pub mod window_targets;
pub mod window_toolbar;
/// Private completion receipts shared by native and browser text editors.
pub mod text_input {
    pub use tilo_webview::text_input::{Failure, Receipt, Resolver};
}
pub mod terminal;
pub mod webview;
pub mod widget;
pub mod window_icon;

/// The model services an app's inference and provider administration travel as.
#[cfg(feature = "model")]
#[doc(inline)]
pub use tilo_process_services::models;
#[doc(inline)]
pub use {
    tilo_process_services::background, tilo_process_services::capture,
    tilo_process_services::clipboard, tilo_process_services::env, tilo_process_services::location,
    tilo_process_services::mpris, tilo_process_services::notify, tilo_process_services::portal,
    tilo_process_services::process, tilo_process_services::secrets,
    tilo_process_services::shell_choosers, tilo_process_services::time,
    tilo_process_services::wake,
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
pub use vello_encoding;
pub use winit;
pub use {vello::kurbo, vello::peniko};

pub use app::App;
pub use color_compat::ColorCompat;
// Paragraph composition is the document engine's, so the reader, the word processor and PDF
// export lay text out by one policy.
pub use state::State;
/// The one image codec: sniff, decode, encode and transform pixels, with the browser's own codecs on
/// the web.
pub use tilo_image_codec as image_codec;
pub use {pictures::BLOB_SCHEME, pictures::Pictures, pictures::Shown as ShownPicture};
pub use {pictures::lend_content, pictures::set_remote_fetcher};
pub use {portal::FileFilter, portal::PickRequest, portal::PortalChannel, portal::PortalOutcome};
pub use {
    profiling::FRAME_BUDGET_US, profiling::FrameProfiler, profiling::FrameTiming, profiling::Pass,
    profiling::PerfReport, profiling::PerfScenario, profiling::PerfStep, profiling::StatSummary,
    profiling::run_scenario, profiling::run_scenario_on,
};
pub use {
    reader_hud::HudAction, reader_hud::HudControls, reader_hud::HudPlacement, reader_hud::HudSide,
    reader_hud::HudSwatch, reader_hud::MarkerDrag, reader_hud::SelectionHud,
};
#[cfg(feature = "model")]
pub use {
    resource::DocumentCheckout, resource::DocumentId, resource::DocumentRef,
    resource::ResourceRefusal, resource::ResourceSet, resource::ResourceStore,
    resource::ResourceType,
};
pub use {
    search::FnProvider, search::SearchGroup, search::SearchProvider, search::SearchRegistry,
    search::SearchResult,
};
#[cfg(feature = "model")]
pub use {state::DocumentFrontier, state::UndoEntry, state::UndoLog};
pub use {
    tilo_doc::paragraph::ComposedLine, tilo_doc::paragraph::ParagraphBox,
    tilo_doc::paragraph::ParagraphOptions, tilo_doc::paragraph::compose_paragraph,
};
pub use {
    ui_graph::GraphPropKind, ui_graph::LIVE_GRAPH_DIR_ENV, ui_graph::LIVE_GRAPH_OPERATION,
    ui_graph::LiveUi, ui_graph::UI_GRAPH_VERSION, ui_graph::UiBuildContext, ui_graph::UiButtonRole,
    ui_graph::UiComponentRequest, ui_graph::UiDocument, ui_graph::UiDocumentExt,
    ui_graph::UiGraphEdit, ui_graph::UiGraphEditor, ui_graph::UiGraphError,
    ui_graph::UiGraphHandle, ui_graph::UiNavigation, ui_graph::UiNode, ui_graph::UiTextRole,
    ui_graph::UiTextSource, ui_graph::UiValues, ui_graph::parse_ui_graph_edits,
};
pub use {undo::UndoAccess, undo::UndoRequest, undo::UndoStatus};
pub use {
    view::FROZEN_VERSION, view::FocusView, view::FreezeThaw, view::FrozenApp, view::ThawError,
    view::View, view::ViewContribution, view::ViewRect,
};

pub use {
    a11y_tree::SemanticAction, a11y_tree::SemanticActionKind, a11y_tree::SemanticNode,
    a11y_tree::SemanticTree, a11y_tree::TreeDelta,
};

#[cfg(feature = "model")]
pub use action_palette::ActionPalette;
pub use tilo_actions;

/// The word-boundary corrections every text field applies, for an input method that commits whole words
/// itself (the on-screen keyboard).
pub mod autocorrect {
    pub use spellcheck::contraction;
}
pub use widget::ModelView;
#[cfg(feature = "model")]
pub use {
    search::ArgumentKind, search::ArgumentSpec, search::CommandEntry, search::CommandOutcome,
    search::RowFace,
};
pub use {widget::AppCommand, widget::AppKind, widget::AppWidget, widget::IconDescriptor};
pub use {
    widget::Card, widget::CardGrid, widget::Section, widget::card_column_width,
    widget::card_columns,
};
pub use {widget::EditOutcome, widget::EditableLabel};
pub use {
    widget::NavItem, widget::NavList, widget::NavMode, widget::NavPane, widget::NavSection,
    widget::NavShell,
};

// Audit-timeline widgets + their shared types. The `ArtifactKind`
// re-export flows through these widgets, so callers can build
// a `HistoryEntry` without a direct dependency on the collab-types crate.
#[cfg(feature = "model")]
pub use {
    history_mode::HistoryController, history_mode::HistoryError, history_mode::HistoryResource,
    history_mode::HistorySnapshot, history_mode::PreviewSide, history_mode::RestoreReview,
};
#[cfg(feature = "model")]
pub use {widget::activity_timeline::ActivityTimeline, widget::activity_timeline::BandUndoRequest};
#[cfg(feature = "model")]
pub use {
    widget::history::AuthorRelation, widget::history::EntryKind,
    widget::history::HistoryAttribution, widget::history::HistoryAuthor,
    widget::history::HistoryClassification, widget::history::HistoryEntry,
    widget::history::HistoryFilter, widget::history::RedactionDisplay,
    widget::history::format_relative_time, widget::history::hue_to_rgb,
};
#[cfg(feature = "model")]
pub use {
    widget::history_group::FOLD_ACTION_FACET, widget::history_group::FOLD_ITEM_FACET,
    widget::history_group::GroupNameRequest, widget::history_group::GroupingConfig,
    widget::history_group::HistoryGroup, widget::history_group::MAX_GROUP_DEPTH,
    widget::history_group::NamedRange, widget::history_group::OpFamily,
    widget::history_group::group_entries,
};
pub use {widget::name_field::NameField, widget::name_field::NameFieldOutcome};
// Re-exported so an app reaches the whole history stack through `tilo_ui::`
// without knowing the module layout.
#[cfg(feature = "model")]
pub use collab_types::presence::CallPresence;
#[cfg(feature = "model")]
pub use history_source::groups as history_groups;
#[cfg(feature = "model")]
pub use widget::history::author_face;
#[cfg(feature = "model")]
pub use {
    history_source::ActivityBrief, history_source::ActivityBriefAction,
    history_source::ActivityDigest, history_source::ActivityNarrator,
    history_source::AuthorDirectory, history_source::CompareError,
    history_source::GroundedNarrative, history_source::HistoryComparison,
    history_source::HistoryFacetIndex, history_source::HistoryResourceTarget,
    history_source::HistorySelectionDetail, history_source::HistorySource,
    history_source::NarrativeRequest, history_source::NarrativeSource,
    history_source::compare_entries, history_source::deterministic_narrative,
    history_source::heads_for_entry, history_source::validate_narrative,
    history_source::version_index_for_entry,
};
#[cfg(feature = "model")]
pub use {
    history_source::invocations::AgentSession, history_source::invocations::InvocationStep,
    history_source::invocations::forget_invocation,
    history_source::invocations::invocation_starting_at,
    history_source::invocations::read_invocations, history_source::invocations::record_invocation,
    history_source::invocations::record_invocation_with_details,
};
#[cfg(feature = "model")]
pub use {
    history_source::revert::ChangeRange, history_source::revert::KeptSlot,
    history_source::revert::RevertError, history_source::revert::RevertOutcome,
    history_source::revert::Slot, history_source::revert::latest_invocation_by,
    history_source::revert::plan_revert, history_source::revert::revert_invocation,
};
#[cfg(feature = "model")]
pub use {
    history_source::sentence::AuthorParts, history_source::sentence::attribution_hover,
    history_source::sentence::author_line, history_source::sentence::author_parts,
    history_source::sentence::author_spoken, history_source::sentence::learn_item_names,
    history_source::sentence::wire_author_line, history_source::sentence::writer_badge,
};
pub use {
    save_status::SAVE_GIVE_UP, save_status::SAVE_RETRY_ACTION, save_status::SAVE_STALL,
    save_status::SaveAction, save_status::SaveNotice, save_status::SaveState,
    save_status::SaveStatusSurface,
};
pub use {
    tilo_presence::Availability, tilo_presence::IDLE_AFTER, tilo_presence::INFERRED_STATUS_TTL,
    tilo_presence::LiveStatus, tilo_presence::MAX_STATUS_CHARS, tilo_presence::PresenceVisibility,
    tilo_presence::StatusOrigin, tilo_presence::StatusSharing,
};

pub use {
    wake::WakeBindError, wake::WakeEvent, wake::WakeHandle, wake::WakeScope, wake::WakeScopeGuard,
};

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
#[cfg(feature = "model")]
pub mod semantic_document_read;
pub mod semantic_outline;

pub mod bound_dispatch;
pub(crate) mod retained_widgets;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod presentation_feedback;

mod focus_navigation;

pub use {
    widget::KeyboardMap, widget::KeyboardMapInput, widget::KeyboardMapKey,
    widget::KeyboardMapSelection,
};

/// Components as a test builds them: a node with its props, parts and views, without a document.
#[cfg(any(test, feature = "test-support"))]
pub mod component_testing;

/// The settings model and store an app's settings tests declare their schema and write through.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use settings_model;
#[cfg(all(feature = "model", feature = "test-support"))]
#[doc(hidden)]
pub use settings_store;

/// The bytes a frozen app is carried in, for the tests above the toolkit that read them.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use postcard;

/// The graph the UI editor reads and writes, for the tests above the toolkit that build it.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use tilo_ui_graph;

/// The calendar types a date picker names its day in, for the tests above the toolkit.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use jiff;

/// The Objective-C bindings the macOS chrome is built on, for the tests above the toolkit.
#[cfg(all(target_os = "macos", feature = "native", feature = "test-support"))]
#[doc(hidden)]
pub use objc2;
#[cfg(all(target_os = "macos", feature = "native", feature = "test-support"))]
#[doc(hidden)]
pub use objc2_app_kit;
#[cfg(all(target_os = "macos", feature = "native", feature = "test-support"))]
#[doc(hidden)]
pub use objc2_foundation;

/// The host chrome's window bars, menus and jump tasks, for the tests above the toolkit that
/// drive them through an app.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod host_chrome_testing {
    pub use crate::host_chrome::header_bar::{HeaderBar, HeaderRequest, resize_edge};
    pub use crate::host_chrome::menus::{Entry, MenuKind, entries};
    pub use crate::host_chrome::model::{CommandGroup, HostCommand};
    pub use crate::host_chrome::windows_items::{decode, encode, jump_tasks, menu_text, quote};
}

/// The macOS title bar chrome, for the tests above the toolkit that drive it through an app.
#[cfg(all(
    target_os = "macos",
    feature = "native",
    any(test, feature = "test-support")
))]
#[doc(hidden)]
pub mod host_chrome_macos_testing {
    pub use crate::host_chrome::macos::{ChromeRequest, MacChrome};
}

// Tests of the base that drive the widgets above it.
#[cfg(test)]
#[path = "accesskit_bridge/tests.rs"]
mod accesskit_bridge_tests;
#[cfg(test)]
#[path = "component/controls_tests.rs"]
mod component_controls_tests;
#[cfg(test)]
#[path = "component/coverage_tests.rs"]
mod component_coverage_tests;
#[cfg(test)]
#[path = "component/feedback_tests.rs"]
mod component_feedback_tests;
#[cfg(test)]
#[path = "component/tests.rs"]
mod component_tests;
#[cfg(test)]
#[path = "control_domain/lifecycle_tests.rs"]
mod control_domain_lifecycle_tests;
#[cfg(test)]
#[path = "control_domain/tests.rs"]
mod control_domain_tests;
#[cfg(test)]
#[path = "i18n/tests.rs"]
mod i18n_tests;
#[cfg(test)]
#[path = "ime/tests.rs"]
mod ime_tests;
#[cfg(test)]
#[path = "keycaps/tests.rs"]
mod keycaps_tests;
#[cfg(test)]
#[path = "part_paint/tests.rs"]
mod part_paint_tests;
#[cfg(test)]
#[cfg(not(feature = "lazy-painter"))]
#[path = "part_paint/wasm_tests.rs"]
mod part_paint_wasm_tests;
#[cfg(test)]
#[path = "presence/draft/tests.rs"]
mod presence_draft_tests;
#[cfg(test)]
#[cfg(feature = "model")]
#[path = "presence/key/view_key_tests.rs"]
mod presence_key_view_key_tests;
/// A session must not paint its own member, and must go on
/// painting everybody else. Both halves are asserted here: the second
/// is the one that keeps a fix in this area from hiding the workspace.
#[cfg(test)]
#[cfg(feature = "model")]
#[path = "presence/own_identity_tests.rs"]
mod presence_own_identity_tests;
#[cfg(test)]
#[path = "richtext/retained_layout_tests.rs"]
mod richtext_retained_layout_tests;
#[cfg(test)]
#[path = "scene_parts/widget_keys/tests.rs"]
mod scene_parts_widget_keys_tests;
#[cfg(test)]
#[path = "secret_paint/tests.rs"]
mod secret_paint_tests;
#[cfg(test)]
#[path = "surface/tests.rs"]
mod surface_tests;
#[cfg(test)]
#[path = "theme_bundle/tests.rs"]
mod theme_bundle_tests;
#[cfg(test)]
#[path = "widget/binding/tests.rs"]
mod widget_binding_tests;
#[cfg(test)]
#[path = "widget/keyboard_entry_tests.rs"]
mod widget_keyboard_entry_tests;
#[cfg(test)]
#[path = "widget/scrollable_placement_tests.rs"]
mod widget_layout_scrollable_placement_tests;
#[cfg(test)]
#[path = "widget/layout/tests.rs"]
mod widget_layout_tests;
#[cfg(test)]
#[path = "widget/manual/ownership_tests.rs"]
mod widget_manual_ownership_tests;
#[cfg(test)]
#[path = "widget/native_view/tests.rs"]
mod widget_native_view_tests;
#[cfg(test)]
#[path = "widget/resource_operation/tests.rs"]
mod widget_resource_operation_tests;
#[cfg(test)]
#[path = "widget/spacer/tests.rs"]
mod widget_spacer_tests;
