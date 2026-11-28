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
    reason = "lint debt the rendering pipeline carried in the toolkit, scoped to this target"
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "unit tests may drive real threads and wait on the real clock; a strict float comparison is the assertion, not a hazard"
    )
)]

//! The toolkit's rendering pipeline: how a drawn scene becomes pixels on a surface.
//!
//! A host hands a frame here and this crate decides what to draw and what to keep:
//!
//! - [`render_thread`] runs the native worker that encodes, acquires, blits, submits and presents a
//!   frame off the event-loop thread, and [`retry`](render_thread::retry) admits the next one.
//! - [`retained_raster`] rasterizes a frame into one target, its damage and scroll copies applied,
//!   so a host keeps the pixels it last presented. [`cpu_raster`] does the same where no GPU can run
//!   vello's compute pipeline, and [`strip_layers`] composes a frame from layers so a panned strip
//!   is a copy.
//! - [`gpu_video`] uploads a decoder's planar frame and converts YUV to RGB on the GPU;
//!   [`dmabuf_import`] takes a picture another process already has in GPU memory.
//! - [`pipeline_cache`] keeps an app's vello pipelines across launches over the seed the session
//!   compiles ([`pipeline_seed`]), and [`render_bump_recovery`] repaints a target whole once after
//!   its bump buffer was lost.
//! - [`native_scenario`] is the opt-in native input and timing log the pipeline writes its marks to.
//!
//! It sits on [`tilo_ui_base`] and the host's services, below the toolkit: an app's window, the
//! workspace's render service and the browser shell all draw a frame through it, so they produce
//! the same pixels from the same scene.

pub mod cpu_raster;
/// Zero-copy DMA-BUF -> `wgpu` texture import via Vulkan external memory.
#[cfg(all(feature = "dmabuf-import", target_os = "linux"))]
pub mod dmabuf_import;
/// Painting decoded video on the GPU: uploading a hardware decoder's planar output and converting
/// YUV to RGB in a shader.
pub mod gpu_video;
pub mod native_scenario;
/// The app's own vello pipelines, kept across launches in its cache directory.
#[cfg(feature = "native")]
pub mod pipeline_cache;
/// The session's read-only vello pipeline cache every app starts from.
pub mod pipeline_seed;
pub mod render_bump_recovery;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod render_thread;
pub mod retained_raster;
pub mod strip_layers;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod presentation_feedback;

// The host services the pipeline reads, under the toolkit's own names: the clock, the process
// environment, background work and the paths a cache lives in. The first two are read only where a
// native window's own process drives the pipeline, so a host that draws frames without one carries
// neither.
#[cfg(feature = "native")]
pub(crate) use tilo_process_services::background;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub(crate) use tilo_process_services::env;
pub(crate) use tilo_process_services::{process, time};
