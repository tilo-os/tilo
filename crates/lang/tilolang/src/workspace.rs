//! The workspace's own state as a resource every package reads (`std/workspace/app.tl`): the leases
//! whose claim stands now, and in time everything else under `workspace.`.
//!
//! It is a package like the standard library, compiled from its source and locked by its
//! `app.ids`, and every package compiles with it without a `use`: `workspace.leases` is a
//! field of its one root, read like a field of any record. No code of it runs and nothing writes
//! it. The host answers each read from the workspace document as it stands then
//! (`Runtime::set_workspace_reading`), because the device that holds a lease can change at any
//! moment and a stale answer would let the wrong device act.

use std::sync::OnceLock;

use crate::ids::{IdsLock, SeededEntropy};
use crate::meta::PackageMeta;
use crate::source::PackageSource;

pub const APP: &str = "tilo.workspace";

/// The root type's name: `workspace.<field>` reads a field of its one resource.
pub const ROOT_TYPE: &str = "Workspace";

/// The document the host publishes the root as: what a read of `workspace.<field>` names.
pub const DOCUMENT: &str = "workspace";

pub const SOURCE: &str = include_str!("../std/workspace/app.tl");

pub const IDS: &str = include_str!("../std/workspace/app.ids");

/// Compiles the package from its source, with the ids its lock holds, its root published as
/// [`DOCUMENT`].
///
/// # Panics
/// Never for the shipped package: its tests prove that it compiles and that the ids in
/// `std/workspace/app.ids` are the ones its declarations take.
#[expect(
    clippy::panic,
    reason = "the package ships with the compiler: a build that cannot compile it has no packages to serve, and its tests prove it compiles"
)]
fn compiled() -> (PackageMeta, IdsLock) {
    let ids = IdsLock::parse(IDS).unwrap_or_else(|error| {
        panic!("the workspace package's ids are readable: {error}");
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
            .map(|diagnostic| diagnostic.render("std/workspace/app.tl"))
            .collect();
        panic!("the workspace package compiles: {}", errors.join("\n"));
    });
    let mut meta = compiled.meta;
    let root = meta
        .types
        .iter()
        .find(|ty| ty.name == ROOT_TYPE)
        .map_or_else(
            || panic!("the workspace package declares `{ROOT_TYPE}`"),
            |ty| ty.id.clone(),
        );
    // Its one resource is the root the host publishes: nothing opens it in a window, so the
    // source declares no `root`, which would want a default view.
    meta.named_roots.push(crate::meta::NamedRootMeta {
        name: DOCUMENT.to_owned(),
        ty: root,
        published: Some(DOCUMENT.to_owned()),
    });
    (meta, compiled.ids)
}

fn package() -> &'static (PackageMeta, IdsLock) {
    static PACKAGE: OnceLock<(PackageMeta, IdsLock)> = OnceLock::new();
    PACKAGE.get_or_init(compiled)
}

/// The package's description: its types and its one root, published as [`DOCUMENT`]. A runtime
/// registers it as an app it runs no code of, whose readings the host answers.
#[must_use]
pub fn meta() -> &'static PackageMeta {
    &package().0
}

#[cfg(test)]
mod tests {
    use super::{APP, DOCUMENT, IDS, ROOT_TYPE};

    #[test]
    fn the_package_declares_the_workspace_root_and_its_leases() {
        let meta = super::meta();
        assert_eq!(meta.app, APP);
        assert_eq!(
            super::package().1.to_text(),
            IDS,
            "write the ids the package takes back to `std/workspace/app.ids`"
        );
        let root = meta
            .types
            .iter()
            .find(|ty| ty.name == ROOT_TYPE)
            .expect("the root type is declared");
        let [named] = meta.named_roots.as_slice() else {
            panic!("one named root: {:?}", meta.named_roots);
        };
        assert_eq!(named.ty, root.id);
        assert_eq!(named.published.as_deref(), Some(DOCUMENT));
        assert!(
            meta.actions.is_empty(),
            "nothing writes the workspace's own state through the package"
        );
    }
}
