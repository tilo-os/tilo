//! Shared structured mathematical typesetting.
//!
//! The expression tree, recursive box layout, structural decorations, and
//! hit-testable insertion gaps live here so read-only documents and live math
//! editors use exactly the same geometry. Editing policy intentionally stays
//! with the consuming application: a reader needs immutable MathML rendering,
//! while a calculator additionally owns caret navigation and mutations.

pub mod layout;
pub mod model;
pub mod render;

pub use layout::{BASE_SIZE, Box2d, BoxKind, Gap, Layout, layout, measure_atom, nearest_gap};
pub use model::{
    MathExpr, Node, Path, Row, Slot, child_row, child_row_mut, node_slots, parse, to_linear,
};
pub use render::draw_layout;
