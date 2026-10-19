//! Native installation actors, validated custody owners and portable contracts.
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
    reason = "the migration stack moved here from apps/imager with its lint debt, which stays scoped to the imager"
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

pub mod account_admission;
pub mod account_checkpoint;
pub mod account_dav_readiness;
pub mod account_sources;
pub mod account_store;
pub mod account_transfer;
pub mod applications_admission;
#[cfg(unix)]
pub mod browser_profiles;
#[cfg(not(unix))]
#[path = "browser_profiles_unsupported.rs"]
pub mod browser_profiles;
pub mod browser_selection;
pub mod cache;
pub mod capture;
pub mod folders;
pub mod home_admission;
pub mod home_transfer;
pub mod icloud_setup;
pub mod imager_model;
#[cfg(target_os = "linux")]
pub mod installed_consumers;
pub mod journey_media;
pub mod journey_promotion;
pub mod journey_transfer;
pub mod known_folders;
pub mod media_capacity;
#[cfg(target_os = "linux")]
pub mod media_layout;
pub mod media_model;
#[cfg(target_os = "linux")]
mod native_root;
pub mod offline_media;
pub mod original_admission;
pub mod pairing;
#[path = "setup/prepared_identity.rs"]
pub mod prepared_identity;
pub mod prepared_media;
pub mod prepared_setup;
pub mod probe;
pub mod profile;
pub mod recovery;
pub mod retained_source;
pub mod staging;
pub mod startup_password;
pub mod windows_accounts;
pub mod windows_inventory;
pub mod windows_media;
pub mod windows_preparation;
pub mod write;

pub mod efi;
pub mod firmware;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod linux_parse;
pub mod model;
pub mod plan;
pub mod platform;
pub mod service_selection;
pub mod session;
pub mod setup;

pub mod destination_security;
pub mod migration;
pub mod migration_directory;
pub mod migration_durability;
pub mod migration_entry;
pub mod migration_journal;
pub mod migration_journal_log;
pub mod migration_native;
pub mod migration_native_journal;
pub mod migration_review;
mod migration_source;
pub mod migration_store;
pub mod migration_worker;
pub mod nofollow;
pub mod portable_transfer;
#[cfg(any(test, all(feature = "test-support", target_os = "linux")))]
pub mod test_support;
pub mod user_transfer;

/// Every start and size is aligned to this many bytes.
pub const ALIGN_BYTES: u64 = 1 << 20;
/// The installer's guided-alongside minimum: 2 GiB ESP + 1 GiB boot + 32 GiB root.
pub const MIN_TARGET_BYTES: u64 = 35 << 30;
/// GPT type of the staging ESP.
pub const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// Firmware boot option description.
pub const BOOT_ENTRY_DESCRIPTION: &str = "Tilo installer";

/// A random RFC 4122 version-4 GUID in canonical lowercase form.
pub fn random_guid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Canonical lowercase GUID text (`8-4-4-4-12` hex digits).
pub fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}

/// Two GUIDs name the same object regardless of case or Windows braces.
pub fn same_guid(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim_matches(|c| c == '{' || c == '}')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}
