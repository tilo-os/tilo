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
        reason = "the browser build compiles native-host plumbing it never drives: the App runner and its native window are its only callers"
    )
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "unit tests may drive real threads and wait on the real clock; the rule guards the library"
    )
)]

//! The Tilo UI app runner: the [`App`] that drives a widget tree in a window, its headless
//! [`headless::Driver`], the launch entry points, the action palette and frame profiling.
//!
//! The toolkit (`tilo-ui`) is named here whole, as it is named there: a program that depends on
//! `tilo-ui-app` writes `tilo_ui_app::widget::Button` and `tilo_ui_app::App` alike. What this crate
//! names itself takes precedence over the toolkit's.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

extern crate self as tilo_ui_app;
pub use tilo_ui::*;
#[cfg(feature = "model")]
pub use tilo_ui_model::*;

/// The toolkit's widgets, and with the model layer the widgets that host an app.
pub mod widget {
    pub use tilo_ui::widget::*;
    #[cfg(feature = "model")]
    pub use tilo_ui_model::widget::*;
}

/// The toolkit's test fixtures, and with the model layer those of an app's widgets.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    pub use tilo_ui::testing::*;
    #[cfg(feature = "model")]
    pub use tilo_ui_model::testing::*;
}

#[cfg(feature = "model")]
pub mod action_palette;
pub mod app;
mod app_core;
pub mod headless;
pub mod launch;
pub mod profiling;

#[cfg(test)]
#[allow(
    dead_code,
    reason = "shared support is compiled by library and integration tests whose consumers intentionally use different subsets"
)]
#[path = "../tests/support/application.rs"]
pub(crate) mod application_test_support;

#[cfg(feature = "model")]
pub use action_palette::ActionPalette;
pub use app::App;
pub use {
    profiling::FRAME_BUDGET_US, profiling::FrameProfiler, profiling::FrameTiming, profiling::Pass,
    profiling::PerfReport, profiling::PerfScenario, profiling::PerfStep, profiling::StatSummary,
    profiling::run_scenario, profiling::run_scenario_on,
};
