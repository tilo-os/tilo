//! Rich text, edited in place: the block editor (a document's paragraphs, headings, lists, code,
//! callouts and tables), the rich text line each of its blocks is, the rich input a message is
//! written in, and the menus they open (slash commands, mentions). They are built on the toolkit's
//! widgets and text input, and a window builds them by name like the toolkit's own (`COMPONENTS`).

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
    reason = "the lint debt the editor brought from the toolkit, scoped to this target"
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "unit tests may wait on the real clock; the rule guards the library"
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

extern crate tilo_ui as ui;

pub mod rich_text_line;
pub use rich_text_line::{DeclaredLine, LineSignal, RichTextLine, RichTextLineProps};
pub mod block_editor;
mod block_editor_body;
pub mod reading;
pub use reading::{Reading, ReadingMode, ReadingProps};
pub mod rich_doc;
pub mod slash_menu;
pub use slash_menu::{SlashCommand, SlashHostCommand, SlashMenu};
mod mention_picker;
mod rich_input;
mod table_model;
pub use rich_input::{RichInput, RichInputProps};
mod block_editor_table;
pub use block_editor::references::{BlockEditorProps, OPEN_REFERENCE};
pub use block_editor::{
    Block, BlockContent, BlockEditOp, BlockEditor, BlockKind, CalloutKind, CursorPlacement,
    DocumentPosition, DocumentSelection, EditScope, Pages, Paper, ParagraphStyle, PresenceKeyFn,
    SlashRequest, TransclusionPreview, ZOOM_MAX, ZOOM_MIN, blocks_from_html, blocks_to_html,
};
/// A document's editable table: its cells, its rows and columns, and how it reads to a person.
pub mod table {
    pub use super::block_editor_table::*;
}
pub use table::Table;

include!(concat!(env!("OUT_DIR"), "/component_registry.rs"));

/// The rich-text widgets a window builds by name, beside the toolkit's
/// (`ui::component_catalogue::TOOLKIT`).
pub static COMPONENTS: tilo_ui_base::component::Set = tilo_ui_base::component::Set {
    entries: generated_entries,
    parts: GENERATED_PARTS,
    enums: Vec::new,
};

#[cfg(test)]
mod note_scroll_tests;
