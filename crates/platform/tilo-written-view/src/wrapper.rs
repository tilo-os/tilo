//! The package every written view is shown through.
//!
//! An instance shows a view of a resource. A written view is neither a package nor a resource:
//! it is a definition and its writer. So the definition is handed to the instance as the reading
//! of one resource ([`SHOWN`], described by [`ANCHOR`]), and the instance runs one small package
//! whose only view embeds what that reading holds, with the writer as the author of the embedded
//! code. Everything a written view is (checked against the packages loaded, bounded by its
//! writer, drawn again when what it reads moves) is then the runtime's own `embed`.

use std::rc::Rc;

use tilolang_tilo::catalogue;
use tilolang_tilo::tilolang::{Compiled, IdsLock, PackageMeta, compile_package};

/// The resource a view's definition is read from, as its reading is published.
pub(crate) const SHOWN: &str = "written.view";
/// The app that resource is described as.
const ANCHOR_APP: &str = "so.tilo.written";

/// What a written view is, as a surface: its definition, and who wrote it.
const ANCHOR: &str = r#"
[[resources]]
kind = "written.view"
label = "Written view"
version = 1
fields = [
  { path = "definition", type = "text" },
  { path = "author", type = "actor" },
]
"#;

/// The package an instance runs to show one.
const SOURCE: &str = "app so.tilo.written.shown \"Written view\"\n\
\n\
use so.tilo.written as written\n\
\n\
view shown(v: written.View) {\n    embed(v.definition, author: v.author, invocation: .none)\n}\n";

pub(crate) struct Wrapper {
    /// The description of the reading a definition arrives as.
    pub(crate) anchor: PackageMeta,
    /// The type of that reading's resource.
    pub(crate) ty: String,
    pub(crate) compiled: Compiled,
    /// The id of the view that embeds the definition.
    pub(crate) view: String,
}

thread_local! {
    /// Compiled once: every view on this thread is shown through the same package.
    static WRAPPER: Result<Rc<Wrapper>, String> = build().map(Rc::new);
}

/// The package, or why this build of it does not compile (a fault of this crate, not of a view).
pub(crate) fn get() -> Result<Rc<Wrapper>, String> {
    WRAPPER.with(Clone::clone)
}

fn build() -> Result<Wrapper, String> {
    let surfaces = catalogue::parse_surfaces(ANCHOR)?;
    let anchor = catalogue::package_of(ANCHOR_APP, "Written view", &surfaces);
    let ty = anchor
        .types
        .first()
        .map(|declared| declared.id.clone())
        .ok_or("the written view's description declares no type")?;
    let compiled = compile_package(SOURCE, &IdsLock::new(), std::slice::from_ref(&anchor))
        .map_err(|diagnostics| {
            diagnostics
                .iter()
                .map(|d| format!("{}:{}: {} {}", d.line, d.col, d.code, d.message))
                .collect::<Vec<_>>()
                .join("\n")
        })?;
    let view = compiled
        .meta
        .views
        .iter()
        .find(|declared| declared.name == "shown")
        .map(|declared| declared.id.clone())
        .ok_or("the written view's package declares no view")?;
    Ok(Wrapper {
        anchor,
        ty,
        compiled,
        view,
    })
}
