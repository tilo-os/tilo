//! The standard library: the native frames of a window, written in tilolang over the
//! toolkit's components (`std/std.tl`).
//!
//! Every package compiles with it: its views are templates, expanded where a package calls
//! them (`std.list(...)`), so what they build is the package's own, under its authority. An app
//! that wants something else composes `ui.*` itself, with the same language features the
//! library uses.

use std::sync::OnceLock;

use crate::ast::{Item, ViewDecl};

/// The name the library is called by: `std.list`.
pub const NS: &str = "std";

/// The library's source.
pub const SOURCE: &str = include_str!("../std/std.tl");

/// Where the library's spans start, apart from every package's own.
const BASE: usize = 1 << 30;

/// The library's views.
///
/// # Panics
/// Never for the shipped library, which its tests parse.
#[must_use]
pub fn views() -> &'static [ViewDecl] {
    static VIEWS: OnceLock<Vec<ViewDecl>> = OnceLock::new();
    VIEWS.get_or_init(|| {
        let (parsed, diags) = crate::parser::parse_file_at(SOURCE, BASE, 0);
        debug_assert!(diags.is_empty(), "the standard library parses: {diags:?}");
        parsed
            .items
            .into_iter()
            .filter_map(|item| match item {
                Item::View(v) => Some(v),
                _ => None,
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_library_parses_into_views_only() {
        let (parsed, diags) = crate::parser::parse_file_at(super::SOURCE, 0, 0);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(parsed.header.is_none(), "the library names no app");
        assert!(
            parsed
                .items
                .iter()
                .all(|item| matches!(item, crate::ast::Item::View(_))),
            "the library declares views only"
        );
        assert!(!super::views().is_empty());
    }
}
