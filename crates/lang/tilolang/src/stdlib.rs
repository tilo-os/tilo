//! The standard library: the frames every tilolang window gets and the functions over what an app
//! carries, written in tilolang over the toolkit's components (`std/app.tl`).
//!
//! It is a package like any other, compiled from its source and locked by its `app.ids`; every package
//! compiles with it. Its views are templates, expanded where a package calls them (`std.list(...)`), so
//! what they build is the caller's UI, under the caller's authority. Its `fn`s are code: a package
//! calls one where it runs (`std.reactions_said(keys)`), and the runtime loads the library beside
//! the package that called it. A document's map of its own headings is one of its templates
//! (`std.headings`), as are the frames a window of a tree or a list gets.
//!
//! Like any package, it is read from its source: a tool run in a checkout reads the checkout's
//! (`load_from` what `in_tree` finds), so an edit of the library takes effect at once. The copy
//! built in is the packaged release's, for a tool run anywhere else.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::ast::{Item, ViewDecl};
use crate::ids::{IdsLock, SeededEntropy};
use crate::meta::PackageMeta;
use crate::package::Compiled;
use crate::source::PackageSource;

pub const APP: &str = "tilo.std";

pub const NS: &str = "std";

/// The packaged release's copy of the library's source.
pub const SOURCE: &str = include_str!("../std/app.tl");

/// The packaged release's copy of the ids the library locks.
pub const IDS: &str = include_str!("../std/app.ids");

/// Where a checkout keeps the library's source, from its root.
pub const IN_TREE: &str = "crates/lang/tilolang/std";

/// The library's source this process compiles: its text, the ids it locks and the file it reads as.
struct Source {
    text: String,
    ids: String,
    path: String,
}

static IN_USE: OnceLock<Source> = OnceLock::new();

fn source() -> &'static Source {
    IN_USE.get_or_init(|| Source {
        text: SOURCE.to_owned(),
        ids: IDS.to_owned(),
        path: "std/app.tl".to_owned(),
    })
}

/// The library the checkout at or above `dir` holds, if `dir` is in one.
#[must_use]
pub fn in_tree(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .map(|root| root.join(IN_TREE))
        .find(|lib| lib.join("app.tl").is_file())
}

/// Reads the library from its package directory `dir` for the rest of the process, in place of
/// the packaged copy.
///
/// # Errors
/// Its source or ids cannot be read, or the process already compiled another library.
pub fn load_from(dir: &Path) -> Result<(), String> {
    let read = |name: &str| {
        let path = dir.join(name);
        std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
    };
    let text = read("app.tl")?;
    let ids = read("app.ids")?;
    let path = dir.join("app.tl").display().to_string();
    let wanted = text.clone();
    if IN_USE.set(Source { text, ids, path }).is_err() && source().text != wanted {
        return Err(format!(
            "{}: the standard library was already read from {}",
            dir.display(),
            source().path
        ));
    }
    if LIBRARY.get().is_none() {
        let library = Library::compiled(source())
            .map_err(|error| format!("the standard library does not compile:\n{error}"))?;
        let _ = LIBRARY.set(library);
    }
    Ok(())
}

/// The library as a package: its module, compiled, with the ids it locks.
struct Library {
    compiled: Compiled,
    templates: Vec<ViewDecl>,
}

impl Library {
    /// Compiles the library from `source`, with the ids its lock holds.
    ///
    /// # Errors
    /// Its ids, or its source, as the file it reads as says where.
    fn compiled(source: &Source) -> Result<Self, String> {
        let render = |diagnostics: Vec<crate::diag::Diagnostic>| {
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.render(&source.path))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let module = crate::package::parse(&source.text).map_err(render)?;
        let ids = IdsLock::parse(&source.ids).map_err(|e| format!("{}: {e}", source.path))?;
        let compiled = crate::package::compile_package_source_with(
            &PackageSource::single(&source.text),
            &ids,
            &[],
            &mut SeededEntropy(0),
        )
        .map_err(render)?;
        let templates = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::View(view) if crate::checker::is_template(view) => Some(view.clone()),
                _ => None,
            })
            .collect();
        Ok(Self {
            compiled,
            templates,
        })
    }
}

static LIBRARY: OnceLock<Library> = OnceLock::new();

/// The compiled library, once per process.
///
/// # Panics
/// Never for the packaged copy, which its tests compile; a library read from a checkout is
/// compiled when it is read (`load_from`).
#[expect(
    clippy::panic,
    reason = "the packaged library ships with the compiler: a build that cannot compile it has no packages to serve, and its tests prove it compiles"
)]
fn library() -> &'static Library {
    LIBRARY.get_or_init(|| {
        Library::compiled(source())
            .unwrap_or_else(|error| panic!("the standard library compiles: {error}"))
    })
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
    fn the_library_is_a_package_of_templates_and_functions() {
        let module = &crate::package::parse(super::SOURCE).unwrap();
        assert_eq!(module.app.id, APP);
        for item in &module.items {
            assert!(
                matches!(item, crate::ast::Item::View(_) | crate::ast::Item::Fn(_)),
                "the library declares the window frames and the functions over them, and nothing \
                 else: {item:?}"
            );
        }
        assert!(!super::templates().is_empty());
        // One view of the library is no template: `call_member` draws a person's call
        // membership, which the library's own `call` view calls with a plain value.
        let templates: Vec<&crate::ast::ViewDecl> = views(module)
            .into_iter()
            .filter(|view| crate::checker::is_template(view))
            .collect();
        assert_eq!(
            super::templates().len(),
            templates.len(),
            "every template view the library declares is expanded into its callers"
        );
        assert_eq!(
            super::library().compiled.ids.to_text(),
            IDS,
            "write the ids the library takes back to `std/app.ids`"
        );
        // A view of the library expands into its callers, so its text travels in the metadata; a
        // function of it is code its callers call (`Imp::CallFn`), and its module carries it.
        let meta = &super::library().compiled.meta;
        assert_eq!(
            meta.fns.len(),
            functions(module).len(),
            "every function the library declares runs here"
        );
        let lines = crate::meta::lines_from_wasm(&super::compiled().wasm);
        for f in &meta.fns {
            assert!(
                lines
                    .iter()
                    .any(|line| line.export == format!("n_{}", f.id)),
                "the library's module carries `{}`: {lines:?}",
                f.name
            );
        }
        // A view that is no template is drawn by the library itself, so its module carries it
        // beside its functions: `call_member` is the one view the library draws, called by its
        // `call` view.
        let own_views = views(module).len() - templates.len();
        assert_eq!(
            lines.len(),
            meta.fns.len() + own_views,
            "the library's module carries its functions and the one view it draws: {lines:?}"
        );
        assert!(meta.app == APP, "the library compiles as its own package");
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

    fn functions(module: &crate::ast::Module) -> Vec<&crate::ast::FnDecl> {
        module
            .items
            .iter()
            .filter_map(|item| match item {
                crate::ast::Item::Fn(f) => Some(f),
                _ => None,
            })
            .collect()
    }
}
