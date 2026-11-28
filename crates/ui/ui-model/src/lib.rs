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
    clippy::implicit_hasher,
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

//! The Tilo UI model layer: what a window shows, bound to the workspace's resources.
//!
//! Applications and their components ([`application`]), the resources a component keeps
//! ([`resource`], [`state`]) and the actions bound to them ([`bound_dispatch`]), search
//! ([`search`]), history and undo ([`history_mode`], [`history_source`], [`undo`]), the view a window
//! restores ([`view`]), the accessibility tree read from all of it ([`a11y_tree`]), and the widgets
//! that host an app ([`widget`]). It sits on the toolkit (`tilo-ui`), which knows none of it.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

extern crate self as tilo_ui_model;

#[cfg(test)]
#[allow(
    dead_code,
    reason = "shared support is compiled by library and integration tests whose consumers intentionally use different subsets"
)]
#[path = "../../ui-app/tests/support/application.rs"]
pub(crate) mod application_test_support;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod widget;

include!(concat!(env!("OUT_DIR"), "/component_registry.rs"));

/// The model layer's own described components, which a window registers beside the toolkit's.
pub static COMPONENTS: tilo_ui_base::component::Set = tilo_ui_base::component::Set {
    entries: generated_entries,
    parts: GENERATED_PARTS,
    enums: Vec::new,
};

/// The app's own client for the session's action API, and what it answers.
pub mod remote_actions;
pub use {
    remote_actions::CancellationCall, remote_actions::CatalogueCall, remote_actions::ChangedCall,
    remote_actions::Followed, remote_actions::FollowedRead, remote_actions::InvocationCall,
    remote_actions::InvocationProgress, remote_actions::RemoteActions,
    remote_actions::RemoteActionsGuard, remote_actions::ResourceReadCall,
};
pub mod a11y_tree;
pub mod action_locus;
/// The document action host behind the framework's dispatcher. Apps submit edits through the framework,
/// never through this host.
#[doc(hidden)]
pub mod local_dispatch;
pub mod local_document_action;
pub use action_locus::ActionUiRoute;
/// The client a window asks the device's own read over: the action line this process's launcher
/// handed it ([`install_window_line`]), as the app `app`, else the client this thread has (a
/// provider entered for it, or the session's collaboration).
#[must_use]
pub fn remote_of_the_window(app: &str) -> remote_actions::RemoteActions {
    use remote_actions::RemoteActions;
    if let Some((line, workspace)) = WINDOW_LINE.get() {
        return RemoteActions::with_provider(
            std::sync::Arc::clone(line),
            tilo_actions::remote::Caller::local_app(app).in_workspace(workspace),
        );
    }
    RemoteActions::current()
}
/// The action line an app's launcher handed this process, and the workspace it was started in.
type WindowLine = (
    std::sync::Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    String,
);
static WINDOW_LINE: std::sync::OnceLock<WindowLine> = std::sync::OnceLock::new();
/// Install the action line the app's launcher handed this process, in `workspace`: the runner
/// that starts the app does, before the first frame, so the toolkit links nothing of the daemon's
/// transport. The first install stands, since a process has one connection to its device; a host
/// process (the shell, a test) installs none.
pub fn install_window_line(
    line: std::sync::Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    workspace: String,
) {
    let _ = WINDOW_LINE.set((line, workspace));
}
pub mod close_admission;
pub mod component_lifecycle;
pub mod device_local_api;
pub mod harness_device;
pub mod history_mode;
pub mod history_source;
pub mod host_chrome;
pub mod launch_error;
pub mod owner_wake;
pub mod resource;
pub mod save_status;
pub mod search;
pub mod session_api;
pub mod state;
pub mod undo;
pub mod view;
pub mod window_share_stage;
// Paragraph composition is the document engine's, so the reader, the word processor and PDF
// export lay text out by one policy.
pub use state::State;
pub use {
    a11y_tree::SemanticAction, a11y_tree::SemanticActionKind, a11y_tree::SemanticNode,
    a11y_tree::SemanticTree, a11y_tree::TreeDelta,
};
pub use {
    resource::DocumentCheckout, resource::DocumentId, resource::DocumentRef,
    resource::ResourceRefusal, resource::ResourceSet, resource::ResourceStore,
    resource::ResourceType,
};
pub use {
    search::ArgumentKind, search::ArgumentSpec, search::CommandEntry, search::CommandOutcome,
    search::RowFace,
};
pub use {
    search::FnProvider, search::SearchGroup, search::SearchProvider, search::SearchRegistry,
    search::SearchResult,
};
pub use {state::DocumentFrontier, state::UndoEntry, state::UndoLog};
pub use {undo::UndoAccess, undo::UndoRequest, undo::UndoStatus};
pub use {
    view::FROZEN_VERSION, view::FocusView, view::FreezeThaw, view::FrozenApp, view::ThawError,
    view::View, view::ViewContribution, view::ViewRect,
};
pub use {widget::AppCommand, widget::AppWidget, widget::IconDescriptor};
// Audit-timeline widgets + their shared types. The `ArtifactKind`
// re-export flows through these widgets, so callers can build
// a `HistoryEntry` without a direct dependency on the collab-types crate.
pub use history_source::groups as history_groups;
pub use widget::history::author_face;
pub use {
    history_mode::HistoryController, history_mode::HistoryError, history_mode::HistoryResource,
    history_mode::HistorySnapshot, history_mode::PreviewSide, history_mode::RestoreReview,
};
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
pub use {
    history_source::invocations::AgentSession, history_source::invocations::InvocationStep,
    history_source::invocations::forget_invocation,
    history_source::invocations::invocation_starting_at,
    history_source::invocations::read_invocations, history_source::invocations::record_invocation,
    history_source::invocations::record_invocation_with_details,
};
pub use {
    history_source::revert::ChangeRange, history_source::revert::KeptSlot,
    history_source::revert::RevertError, history_source::revert::RevertOutcome,
    history_source::revert::Slot, history_source::revert::latest_invocation_by,
    history_source::revert::plan_revert, history_source::revert::revert_invocation,
};
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
pub use {widget::activity_timeline::ActivityTimeline, widget::activity_timeline::BandUndoRequest};
pub use {
    widget::history::AuthorRelation, widget::history::EntryKind,
    widget::history::HistoryAttribution, widget::history::HistoryAuthor,
    widget::history::HistoryClassification, widget::history::HistoryEntry,
    widget::history::HistoryFilter, widget::history::RedactionDisplay,
    widget::history::format_relative_time, widget::history::hue_to_rgb,
};
pub use {
    widget::history_group::FOLD_ACTION_FACET, widget::history_group::FOLD_ITEM_FACET,
    widget::history_group::GroupNameRequest, widget::history_group::GroupingConfig,
    widget::history_group::HistoryGroup, widget::history_group::MAX_GROUP_DEPTH,
    widget::history_group::NamedRange, widget::history_group::OpFamily,
    widget::history_group::group_entries,
};
pub mod application;
pub mod application_view;
pub mod bound_dispatch;
pub mod invocation_scope;
pub mod semantic_document_read;
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

// Tests of the toolkit's control ownership and manual widgets that drive the model above them.
#[cfg(test)]
#[path = "control_domain/lifecycle_tests.rs"]
mod control_domain_lifecycle_tests;
#[cfg(test)]
#[path = "control_domain/tests.rs"]
mod control_domain_tests;
#[cfg(test)]
#[path = "widget/manual/ownership_tests.rs"]
mod widget_manual_ownership_tests;
