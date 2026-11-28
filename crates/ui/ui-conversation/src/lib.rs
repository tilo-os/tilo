//! `conversation`: what a conversation is made of, once for every app that has one.

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
    reason = "lint debt these widgets carried in the toolkit, scoped to this target"
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard, and tests may wait on the real clock"
    )
)]

pub mod attachment;
pub mod composer;
pub mod delivery;
pub mod draft;
pub mod earlier;
pub mod list;
mod list_component;
pub use list_component::{
    ConversationEntryProps, ConversationListProps, DescribedConversationList,
};

pub use attachment::{
    AttachmentAction, AttachmentFace, AttachmentKind, AttachmentSummary, SavedCopy, UploadProgress,
    full_size_rect,
};
pub use composer::{ComposerIntent, ComposerRule};
pub use delivery::{DeliveryFace, DeliveryState, DeliveryStep};
pub use draft::{DRAFT_PREVIEW_CHARS, DraftBook, line as draft_line, preview as draft_preview};
pub use earlier::EarlierUnavailableFace;
pub use list::{CaptionShows, captions};
pub use list::{
    ConversationLine, ConversationList, ConversationRole, ListCache, ListFace, ListWork,
    bubble_colors, run_breaks,
};

/// Resolve one of the conversation's labels in the shell's language.
///
/// The words live in the toolkit's catalogue with every other widget's, one
/// namespace per module (`conversation-`), so a language that ships the toolkit
/// ships these too and an app can override any key in its own catalogue.
pub(crate) fn t(key: &str) -> String {
    tilo_ui_base::i18n::widget_format(key, &[])
}

#[cfg(test)]
mod tests;

include!(concat!(env!("OUT_DIR"), "/component_registry.rs"));

/// The conversation's described widgets, which a window builds by name beside the toolkit's
/// (`tilo_ui::component_catalogue::TOOLKIT`).
pub static COMPONENTS: tilo_ui_base::component::Set = tilo_ui_base::component::Set {
    entries: generated_entries,
    parts: GENERATED_PARTS,
    enums: Vec::new,
};
