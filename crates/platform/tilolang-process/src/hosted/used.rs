//! Read imported apps through their own packages.

use std::collections::HashSet;
use tilolang_tilo::resources::ResourceStore;
use tilolang_tilo::used::UsedApp;

use super::{Bundle, Spec};

/// Another app the app uses, read through its own package.
pub(super) struct Used {
    pub(super) app: UsedApp,
    /// Whether the store took in a change its readings have not been read again for.
    pub(super) moved: bool,
}

/// The other apps `bundle`'s app uses ([`Spec::installed`]), each read from `store` through its
/// own package.
pub(super) fn used_apps(spec: &Spec, bundle: &Bundle, store: &ResourceStore) -> Vec<Used> {
    bundle
        .imports
        .iter()
        .filter_map(|import| {
            spec.installed.get(&import.meta.app)?;
            let packages = packages_of(&bundle.imports, &import.meta);
            let app = UsedApp::new(store.clone(), &packages, &spec.person)
                .map_err(|error| {
                    tracing::error!(app = spec.app, used = import.meta.app, %error, "a used app's package did not load");
                })
                .ok()?;
            Some(Used { app, moved: false })
        })
        .collect()
}

/// The packages `meta`'s app is read through, from `imports`: those it uses, in the order they
/// load, then its own.
fn packages_of<'a>(
    imports: &'a [tilolang_tilo::bundle::Import],
    meta: &'a tilolang_tilo::runtime::Meta,
) -> Vec<(&'a [u8], &'a tilolang_tilo::runtime::Meta)> {
    let mut needed: HashSet<&str> = HashSet::new();
    let mut pending = vec![meta];
    while let Some(next) = pending.pop() {
        for used in &next.uses {
            if needed.insert(used.as_str())
                && let Some(import) = imports.iter().find(|import| import.meta.app == *used)
            {
                pending.push(&import.meta);
            }
        }
    }
    imports
        .iter()
        .filter(|import| import.meta.app != meta.app && needed.contains(import.meta.app.as_str()))
        .map(|import| (import.wasm.as_slice(), &import.meta))
        .chain(std::iter::once((
            imports
                .iter()
                .find(|import| import.meta.app == meta.app)
                .map_or(&[][..], |import| import.wasm.as_slice()),
            meta,
        )))
        .collect()
}
