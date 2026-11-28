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
    reason = "lint debt the typesetter carried in the toolkit, scoped to this target"
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

//! Shared structured mathematical typesetting. The expression tree, recursive box layout, structural
//! decorations, and hit-testable insertion gaps live here so read-only documents and live math editors
//! use exactly the same geometry.

pub mod edit;
pub mod layout;
pub mod model;
pub mod render;
pub mod tex;
pub mod typst;

#[cfg(test)]
mod integral_tests;

pub use layout::{
    BASE_SIZE, Box2d, BoxKind, Gap, Layout, layout, layout_wrapped, measure_atom, nearest_gap,
};
pub use model::{
    MathExpr, Node, Path, Row, Slot, child_row, child_row_mut, node_slots, parse, to_linear,
};
pub use render::draw_layout;
pub use tex::{ParseDiagnostic, ParseOutput, parse_tex, serialize_tex};
pub use typst::{parse_typst, serialize_typst};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormulaSyntax {
    Tex,
    Typst,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FormulaMode {
    #[default]
    Source,
    Visual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FormulaDescriptor {
    pub syntax: FormulaSyntax,
    pub mode: FormulaMode,
}

impl FormulaSyntax {
    pub fn parse(self, source: &str) -> ParseOutput {
        match self {
            Self::Tex => parse_tex(source),
            Self::Typst => parse_typst(source),
        }
    }

    pub fn serialize(self, expr: &MathExpr) -> String {
        match self {
            Self::Tex => serialize_tex(expr),
            Self::Typst => serialize_typst(expr),
        }
    }

    pub fn from_fence_language(language: Option<&str>) -> Option<Self> {
        FormulaDescriptor::from_fence_language(language).map(|descriptor| descriptor.syntax)
    }
}

impl FormulaDescriptor {
    pub fn from_fence_language(language: Option<&str>) -> Option<Self> {
        let mut parts = language?.split_whitespace();
        let syntax = match parts.next()?.to_ascii_lowercase().as_str() {
            "tex" | "latex" | "math" => Some(FormulaSyntax::Tex),
            "typst" | "typst-math" => Some(FormulaSyntax::Typst),
            _ => None,
        }?;
        let mode = if parts.any(|part| part.eq_ignore_ascii_case("visual")) {
            FormulaMode::Visual
        } else {
            FormulaMode::Source
        };
        Some(Self { syntax, mode })
    }

    pub fn fence_language(self) -> String {
        let syntax = match self.syntax {
            FormulaSyntax::Tex => "tex",
            FormulaSyntax::Typst => "typst",
        };
        match self.mode {
            FormulaMode::Source => syntax.to_string(),
            FormulaMode::Visual => format!("{syntax} visual"),
        }
    }
}
