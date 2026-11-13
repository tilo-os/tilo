//! The standard library: the frames every tilolang window gets, written in tilolang over the toolkit's
//! components (`std/app.tl`).
//!
//! It is a package like any other, compiled from its source and locked by its `app.ids`; every package
//! compiles with it. Its views are templates, expanded where a package calls them (`std.list(...)`), so
//! what they build is the caller's UI, under the caller's authority.

use std::sync::OnceLock;

use crate::ast::{Item, Module, ViewDecl};
use crate::ids::{IdsLock, SeededEntropy};
use crate::meta::PackageMeta;
use crate::package::Compiled;
use crate::source::PackageSource;

pub const APP: &str = "tilo.std";

pub const NS: &str = "std";

pub const SOURCE: &str = include_str!("../std/app.tl");

pub const IDS: &str = include_str!("../std/app.ids");

/// The library's module.
///
/// # Panics
/// Never for the shipped library, which its tests parse.
#[expect(
    clippy::panic,
    reason = "the library ships with the compiler: a build that cannot parse it has no packages to serve, and its tests prove it parses"
)]
fn module() -> &'static Module {
    static MODULE: OnceLock<Module> = OnceLock::new();
    MODULE.get_or_init(|| {
        crate::package::parse(SOURCE).unwrap_or_else(|diagnostics| {
            let errors: Vec<String> = diagnostics
                .iter()
                .map(|diagnostic| diagnostic.render("std/app.tl"))
                .collect();
            panic!("the standard library parses: {}", errors.join("\n"));
        })
    })
}

/// The library as a package: its compiled module, its metadata and the ids it locks.
struct Library {
    compiled: Compiled,
    templates: Vec<ViewDecl>,
}

impl Library {
    /// Compiles the library from its source, with the ids its lock holds.
    ///
    /// # Panics
    /// Never for the shipped library: its tests prove that it compiles and that the ids in
    /// `std/app.ids` are the ones its declarations take.
    #[expect(
        clippy::panic,
        reason = "the library ships with the compiler: a build that cannot compile it has no packages to serve, and its tests prove it compiles"
    )]
    fn compiled() -> Self {
        let ids = IdsLock::parse(IDS).unwrap_or_else(|error| {
            panic!("the standard library's ids are readable: {error}");
        });
        let compiled = crate::package::compile_package_source_with(
            &PackageSource::single(SOURCE),
            &ids,
            &[],
            &mut SeededEntropy(0),
        )
        .unwrap_or_else(|diagnostics| {
            let errors: Vec<String> = diagnostics
                .iter()
                .map(|diagnostic| diagnostic.render("std/app.tl"))
                .collect();
            panic!("the standard library compiles: {}", errors.join("\n"));
        });
        let templates = module()
            .items
            .iter()
            .filter_map(|item| match item {
                Item::View(view) if crate::checker::is_template(view) => Some(view.clone()),
                _ => None,
            })
            .collect();
        Self {
            compiled,
            templates,
        }
    }
}

/// The compiled library, once per process.
fn library() -> &'static Library {
    static LIBRARY: OnceLock<Library> = OnceLock::new();
    LIBRARY.get_or_init(Library::compiled)
}

#[must_use]
pub fn meta() -> &'static PackageMeta {
    &library().compiled.meta
}

/// The library's compiled module: what a bundle carries so the package id its declarations belong to can
/// run its actions.
#[must_use]
pub fn compiled() -> &'static Compiled {
    &library().compiled
}

/// The library's views, which packages expand where they call them.
#[must_use]
pub fn templates() -> &'static [ViewDecl] {
    &library().templates
}

#[cfg(test)]
mod tests {
    use super::{APP, IDS};

    #[test]
    fn the_library_is_a_package_of_templates() {
        let module = super::module();
        assert_eq!(module.app.id, APP);
        for item in &module.items {
            assert!(
                matches!(item, crate::ast::Item::View(_)),
                "the library declares the window frames and nothing else: {item:?}"
            );
        }
        assert!(!super::templates().is_empty());
        assert_eq!(
            super::templates().len(),
            views(module).len(),
            "every view the library declares is a template its callers expand"
        );
        assert_eq!(
            super::library().compiled.ids.to_text(),
            IDS,
            "write the ids the library takes back to `std/app.ids`"
        );
        // Every bundle carries the library's module: it holds no view code, since its views expand into
        // their callers, so it stays small.
        assert!(
            super::compiled().wasm.len() < 1024,
            "the library's module is {} bytes",
            super::compiled().wasm.len()
        );
        assert!(
            super::library().compiled.meta.app == APP,
            "the library compiles as its own package"
        );
    }

    fn views(module: &crate::ast::Module) -> Vec<&crate::ast::ViewDecl> {
        module
            .items
            .iter()
            .filter_map(|item| match item {
                crate::ast::Item::View(v) => Some(v),
                _ => None,
            })
            .collect()
    }
}
