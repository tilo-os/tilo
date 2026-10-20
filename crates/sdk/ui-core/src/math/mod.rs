//! Shared structured mathematical typesetting.
//!
//! The expression tree, recursive box layout, structural decorations, and
//! hit-testable insertion gaps live here so read-only documents and live math
//! editors use exactly the same geometry. Editing policy intentionally stays
//! with the consuming application: a reader needs immutable `MathML` rendering,
//! while a calculator additionally owns caret navigation and mutations.

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
