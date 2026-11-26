//! The Tilo UI toolkit's base: the widget contract, layout, paint, text, theming,
//! input, accessibility and presence every widget is built on.

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
pub(crate) mod a11y_walk;
#[cfg(all(feature = "native", target_os = "android"))]
mod accesskit_android_adapter;
pub mod accesskit_bridge;
pub mod agent_identity;
pub mod animation;
pub mod arriving;
pub mod capability;
pub mod motion;
/// The actions the host publishes itself, named for apps to call through `ui`.
pub use {tilo_app_id::apps, tilo_app_id::chat};
extern crate self as tilo_ui_base;
/// The app surface an app's changes are decided against, kept with the collab client that names it on
/// every signed op.
#[cfg(feature = "model")]
pub use collab_types::app_surface;
pub mod backdrop;
pub mod caps_lock;
/// Who the keyboard belongs to, which is who paints a caret.
pub mod caret_owner;
pub mod chart_palette;
pub mod chrome;
pub mod clock;
#[cfg(feature = "model")]
pub mod collaboration;
#[cfg(feature = "model")]
pub use runtime_collab::native_media;
#[cfg(feature = "model")]
pub use runtime_collab::shown_actions;
pub mod collection_transition;
pub use {tilo_ui_paint::color, tilo_ui_paint::color_compat};
pub mod carry_out;
pub mod component;
pub mod component_inputs;
pub mod content_layout;
pub mod damage;
pub mod date;
pub mod diag;
pub mod dnd;
pub mod doc;
pub mod doc_source;
#[cfg(any(all(windows, feature = "dx12-shader-cache"), test))]
mod dx12_shader_cache;
pub mod edge_autoscroll;
pub mod evdev;
pub mod fact;
pub mod focus_visible;
pub mod font_catalog;
pub mod format;
pub mod fullscreen;
pub mod gesture;
pub mod gpu;
#[cfg(feature = "native")]
mod host_appearance;
pub mod host_data;
#[cfg(all(windows, feature = "native"))]
pub use native::host_notifications;
pub mod emoji_catalogue;
pub mod held_images;
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
pub mod local_identity;
pub mod local_state;
#[cfg(feature = "native")]
pub mod native;
pub mod nav_stack;
pub mod num;
pub mod offscreen;
pub mod outside_paint;
pub mod overlay;
pub mod paint;
pub mod part_paint;
pub mod pen;
pub mod people_photos;
pub mod permissions;
pub mod person_card;
pub mod platform;
pub mod presence;
pub mod press;
pub mod query_text;
pub use tilo_ui_paint::raster;
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
pub use {query_text::QueryComposition, query_text::QueryEventChange, query_text::QueryImeChange, query_text::QueryInk, query_text::QueryView};
pub use tilo_lock::deferred;
pub mod personal_dictionary;
pub mod text_break;
pub mod text_detect;
pub use {text_break::LineBreakKind, text_break::line_break_opportunities};
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
pub mod draw;
pub(crate) mod draw_cull;
#[cfg(feature = "model")]
pub mod mentions;
pub mod own_paint;
pub mod published;
#[cfg(feature = "model")]
pub mod remote_actions;
pub mod reorder;
pub mod richtext;
/// Rich text as a component shows it: paragraphs with their marks, and how each reference in them reads
/// (`ui.BlockEditor`'s value), the one model the runtime sends and the window draws.
pub use tilo_ui_graph::rich;
pub mod scale;
pub mod scene_parts;
pub mod screen_reader;
pub mod screen_reader_chord;
pub mod screen_reader_touch;
pub mod scroll_copy;
pub mod secret_paint;
pub mod selection;
pub mod shared_subject;
pub mod snap;
pub mod step;
pub mod surface;
pub mod surface_services;
pub mod sysdirs;
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
pub mod uri;
pub mod virtual_viewport;
pub mod workspace_emoji;
#[cfg(feature = "native")]
pub use native::warmup;
pub mod key_input;
pub mod widget;
/// The model services an app's inference and provider administration travel as.
#[cfg(feature = "model")]
#[doc(inline)]
pub use tilo_process_services::models;
#[doc(inline)]
pub use {tilo_process_services::background, tilo_process_services::capture, tilo_process_services::clipboard, tilo_process_services::env, tilo_process_services::jpeg, tilo_process_services::location, tilo_process_services::mpris, tilo_process_services::notify, tilo_process_services::png, tilo_process_services::portal, tilo_process_services::process, tilo_process_services::secrets, tilo_process_services::shell_choosers, tilo_process_services::time, tilo_process_services::wake};
/// The AccessKit types the bridge and [`headless::Driver`] hand back, re-exported so an app crate
/// can name them (`crate::accesskit::Role`) without taking its own dependency on a version that must
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
pub use {vello::kurbo, vello::peniko};
pub use vello_encoding;
pub use winit;
pub use {arriving::Arrival, arriving::ArrivalClock, arriving::ArrivalCompletion, arriving::ArrivalWriter, arriving::Arriving};
pub use caps_lock::CapsLockEstimate;
pub use {collection_transition::RowMotion, collection_transition::RowMotions};
pub use color_compat::ColorCompat;
pub use {content_layout::FocusedContentArea, content_layout::ResponsiveControlBar};
pub use {doc::TextEdit, doc::put_text_diffed, doc::splice_text_diffed};
pub use {i18n::ArgValue, i18n::EmbeddedCatalog, i18n::I18nError, i18n::I18nManager, i18n::PseudoLocale};
pub use {image_store::Fetcher, image_store::HostImageLoader, image_store::ImageStore};
pub use {input::ActionBindings, input::BindingConflict, input::BindingSet, input::ControllerBinding, input::ControllerButton, input::InputBinding, input::KeyLegend, input::KeyboardBinding, input::KeyboardModifier, input::TouchGesture};
pub use {interaction::ControllerFamily, interaction::InputAvailability, interaction::InputDevice, interaction::InputModality, interaction::InteractionEnvironment, interaction::KeyboardFamily, interaction::PointerPrecision, interaction::TouchClaim, interaction::TouchPointerEmulator, interaction::TouchPointerEvent};
pub use {keycaps::KeycapLanguage, keycaps::Keycaps};
pub use {local_state::Intents, local_state::LatestRequest, local_state::LatestTask, local_state::LocalState, local_state::LocalStateWriter, local_state::RequestCompletion, local_state::TaskCompletion};
pub use {nav_stack::EdgeSwipeBack, nav_stack::NavAction, nav_stack::NavStack};
pub use platform::local_surface_id;
pub use {platform::DialogActionOrder, platform::HostShortcut, platform::Platform, platform::PlatformConventions, platform::Shortcut, platform::spell_shortcut, platform::spell_shortcut_keycaps};
pub use {portal::FileFilter, portal::PickRequest, portal::PortalChannel, portal::PortalOutcome};
#[cfg(feature = "model")]
pub use {remote_actions::CancellationCall, remote_actions::CatalogueCall, remote_actions::ChangedCall, remote_actions::Followed, remote_actions::FollowedRead, remote_actions::InvocationCall, remote_actions::RemoteActions, remote_actions::RemoteActionsGuard, remote_actions::ResourceReadCall};
pub use {reorder::DropCandidate, reorder::DropPlacement, reorder::DropTarget, reorder::ReorderController, reorder::ReorderDrop, reorder::ReorderMotion};
pub use {selection::KeySelection, selection::SelectionModel};
pub use text_break::discretionary_hyphen_breaks;
pub use {theme::AppearanceIntent, theme::ContentSurface, theme::SurfaceShape, theme::Theme, theme::WindowAppearance, theme::resolve_theme_layers, theme::workspace_hue};
pub use {tilo_doc::paragraph::ComposedLine, tilo_doc::paragraph::ParagraphBox, tilo_doc::paragraph::ParagraphOptions, tilo_doc::paragraph::compose_paragraph};
/// The one image codec: sniff, decode, encode and transform pixels, with the browser's own codecs on
/// the web.
pub use tilo_image_codec as image_codec;
pub use {virtual_viewport::DragScrollFinish, virtual_viewport::DragScrollUpdate, virtual_viewport::DragScroller, virtual_viewport::KineticSampler, virtual_viewport::ReconcileError, virtual_viewport::ScrollAlignment, virtual_viewport::ScrollGlide, virtual_viewport::ScrollMomentum, virtual_viewport::SparseAnchor, virtual_viewport::SparseVirtualViewport, virtual_viewport::VirtualViewport, virtual_viewport::WheelStep};
// `a11y::Role` / `a11y::Action` are reached through the module path to avoid
// clashing with `permissions::{Role}` / `permissions::Verb`, which are
// already re-exported at the crate root.
pub use a11y::SemanticId;
pub use {a11y::A11ySettings, a11y::AccessBounds, a11y::AccessNode, a11y::AccessState, a11y::AccessibleTextSelection};
pub use {screen_reader::ScreenReader, screen_reader::describe, screen_reader::flatten_reading_order};
pub use tilo_actions;
pub use {draw::DrawCtx, draw::DrawEnv, draw::WidgetOverlay};
pub use widget::ContentKind;
pub use {widget::Element, widget::Widget};
pub use {widget::NativeView, widget::NativeViewAccessibility, widget::NativeViewBackend, widget::NativeViewGeometry, widget::NativeViewHost, widget::NativeViewInput};
pub use {widget::RouteError, widget::RouteHandle, widget::Router};
pub use widget::HistoryEntryId;
// Re-exported so an app reaches the whole history stack through `crate::`
// without knowing the module layout.
#[cfg(feature = "model")]
pub use collab_types::presence::CallPresence;
pub use {tilo_presence::Availability, tilo_presence::IDLE_AFTER, tilo_presence::INFERRED_STATUS_TTL, tilo_presence::LiveStatus, tilo_presence::MAX_STATUS_CHARS, tilo_presence::PresenceVisibility, tilo_presence::StatusOrigin, tilo_presence::StatusSharing};
// Undoing a run of changes that never reached anybody's undo stack — a
// remote author's, an agent's.
pub use {gesture::DragSelect, gesture::Granularity};
pub use {richtext::BaselineShift, richtext::FontFamily, richtext::RichSpan, richtext::RichTextLayout, richtext::TextAlign};
pub use {text_edit::CaretWord, text_edit::CharFilter, text_edit::CharSplice, text_edit::Edit, text_edit::EditCore, text_edit::Motion, text_edit::SUGGESTION_SETTLE, text_edit::TypingRun, text_edit::delete_last_grapheme, text_edit::word_at_caret};
pub use {text_render::FontChoice, text_render::FontVariation, text_render::Fragmentation, text_render::GlyphOutline, text_render::HangingPunctuation, text_render::Hyphenation, text_render::NumericStyle, text_render::OpenTypeFeature, text_render::OpenTypeTag, text_render::OpticalSizing, text_render::ParagraphAlign, text_render::PolygonTextLayout, text_render::TextFlowRegion, text_render::TextFlowRegionError, text_render::TextGround, text_render::TextKind, text_render::TextLayoutConstraints, text_render::TextLayoutEngine, text_render::TextRenderer, text_render::TextScaling, text_render::TextStyle, text_render::TextWeight, text_render::TextWrapMode, text_render::UI_HYPERLEGIBLE_FAMILY, text_render::UI_MONOSPACE_FAMILY, text_render::UI_SANS_FAMILY, text_render::UI_SERIF_FAMILY, text_render::configured_font_system};
pub use {text_selection::CopyWatch, text_selection::Selection, text_selection::TextAffinity, text_selection::TextPosition, text_selection::clipboard_available, text_selection::clipboard_copy, text_selection::clipboard_copy_image, text_selection::clipboard_copy_primary, text_selection::clipboard_copy_rich_accepted, text_selection::clipboard_paste, text_selection::clipboard_paste_image, text_selection::clipboard_paste_primary, text_selection::clipboard_paste_rich, text_selection::extend_span_drag, text_selection::grapheme_boundary_at_or_before, text_selection::line_boundaries, text_selection::next_char_boundary, text_selection::next_word_boundary, text_selection::next_word_boundary_chars, text_selection::prev_char_boundary, text_selection::prev_word_boundary, text_selection::prev_word_boundary_chars, text_selection::reserve_clipboard_copy, text_selection::try_clipboard_copy, text_selection::try_clipboard_copy_secret, text_selection::word_boundaries, text_selection::word_boundaries_chars};
pub use {tokens::ReadingFace, tokens::TextRole, tokens::UiFace};
pub use animation::Lerp;
#[cfg(feature = "model")]
pub use collaboration::PresencePayload;
#[cfg(feature = "model")]
pub use {permissions::Access, permissions::CapDecision, permissions::CapOverride, permissions::PermissionContext, permissions::PermissionScope, permissions::ResourceRef, permissions::Role, permissions::Scope as CapScope};
pub use {permissions::Acting, permissions::ActionKind, permissions::Grantee, permissions::MemberId};
pub use {presence::PathSegment, presence::PresenceCtx, presence::PresenceKey, presence::SelectionRange, presence::human_label};
pub use {wake::WakeBindError, wake::WakeEvent, wake::WakeHandle, wake::WakeScope, wake::WakeScopeGuard};
#[cfg(feature = "model")]
pub use widget::ViewPresence;
#[cfg(feature = "native")]
pub mod input_frame;
#[cfg(feature = "model")]
pub mod semantic_readers;
pub mod synthetic_input;
/// Emission-time local control ownership.
pub mod control_domain;
pub use {control_domain::ControlCoverage, control_domain::ControlDomain, control_domain::ControlOutbox, control_domain::ControlProducerDiscipline};
