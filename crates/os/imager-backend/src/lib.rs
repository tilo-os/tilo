//! Native installation actors and portable contracts.
//! Frontends retain presentation state and issue requests to these actors.

#![allow(
    clippy::cast_lossless,
    clippy::cloned_ref_to_slice_refs,
    clippy::collapsible_if,
    clippy::doc_markdown,
    clippy::double_ended_iterator_last,
    clippy::drop_non_drop,
    clippy::expect_used,
    clippy::format_push_string,
    clippy::ignored_unit_patterns,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_let_else,
    clippy::many_single_char_names,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_const_for_thread_local,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::needless_continue,
    clippy::needless_lifetimes,
    clippy::needless_option_as_deref,
    clippy::needless_pass_by_ref_mut,
    clippy::needless_pass_by_value,
    clippy::nonminimal_bool,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_closure_for_method_calls,
    clippy::result_unit_err,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::struct_field_names,
    clippy::suspicious_operation_groupings,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unnecessary_mut_passed,
    clippy::unnecessary_semicolon,
    clippy::unnecessary_wraps,
    clippy::unreachable,
    clippy::unreadable_literal,
    clippy::unused_io_amount,
    clippy::unused_self,
    clippy::unwrap_used,
    clippy::used_underscore_binding,
    clippy::wildcard_imports,
    reason = "the Disk Imager's lint debt, which stays scoped to the imager"
)]
// Only the Disk Imager app links this crate, so its arithmetic runs in the
// app's own process, as it did before the code moved out of apps/imager.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic of the Disk Imager; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod cache;
pub mod imager_model;
pub mod media_model;
pub mod probe;
pub mod profile;
pub mod record;
pub mod staging;
pub mod startup_password;
pub mod write;

pub mod firmware;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod linux_parse;
pub mod model;
pub mod plan;
pub mod platform;
pub mod setup;

#[cfg(all(feature = "destination", target_os = "linux"))]
pub mod destination;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// Every start and size is aligned to this many bytes.
pub const ALIGN_BYTES: u64 = 1 << 20;
/// The smallest root installed beside another system.
pub const MIN_ALONGSIDE_BYTES: u64 = staging_handoff_model::sizes::MIN_ROOT_BYTES;
/// Tilo keeps several signed UKIs for rollback and repair, which a 100--512 MiB ESP cannot hold.
pub const MIN_ALONGSIDE_ESP_BYTES: u64 = staging_handoff_model::sizes::ESP_BYTES;
/// One boot partition, large enough for the kernel and initramfs of several entries.
pub const BOOT_BYTES: u64 = staging_handoff_model::sizes::BOOT_BYTES;
/// The least room Tilo takes on a disk, alongside or erased: ESP, boot and the smallest root.
pub const MIN_TARGET_BYTES: u64 = staging_handoff_model::sizes::MIN_TILO_BYTES;
/// GPT type of the staging ESP.
pub const ESP_TYPE: &str = staging_handoff_model::gpt::EFI_SYSTEM;
/// Firmware boot option description.
pub const BOOT_ENTRY_DESCRIPTION: &str = "Tilo installer";

/// A random RFC 4122 version-4 GUID in canonical lowercase form.
pub fn random_guid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// An operation, attempt or request id, as [`random_guid`] writes it.
pub use staging_handoff_model::is_operation_id;

/// Canonical lowercase GUID text (`8-4-4-4-12` hex digits).
pub fn is_guid(value: &str) -> bool {
    staging_handoff_model::is_guid(value) && !value.bytes().any(|byte| byte.is_ascii_uppercase())
}

/// Two GUIDs name the same object regardless of case or Windows braces.
pub fn same_guid(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim_matches(|c| c == '{' || c == '}')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}
