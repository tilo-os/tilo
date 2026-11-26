//! Tilo's fonts for the apps the command runs, captures and builds.
//!
//! An app sets its text in Tilo's font catalog, which a Tilo computer installs and a Tilo
//! checkout prepares when it builds. On any other machine the command prepares it once, into the
//! font artifact cache where every Tilo program looks: the pinned fonts are fetched from Tilo's
//! package store and checked against the hashes this build pins. A built app carries the catalog
//! beside its program (`fonts/`), so it sets text in Tilo's faces wherever it runs.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context as _, Result, anyhow};
use tilo_font_catalog::prepare::{self, Selection};

/// The catalog the apps this command starts set text in: the one this machine has, or the one
/// prepared for it.
pub fn ensure() -> Result<PathBuf> {
    static READY: OnceLock<PathBuf> = OnceLock::new();
    if let Some(root) = READY.get() {
        return Ok(root.clone());
    }
    let cache = artifact_cache();
    let root = if let Some(root) = tilo_ui_base::font_catalog::catalog_root() {
        root
    } else {
        let cache = cache.as_deref().ok_or_else(|| {
            anyhow!(
                "Tilo's fonts have no place on this computer: set HOME, XDG_CACHE_HOME or \
                 TILO_FONT_ASSET_CACHE"
            )
        })?;
        prepare::prepared(cache, Selection::Product, prepare::offline(), |fetching| {
            eprintln!(
                "Fetching Tilo's fonts ({}) once…",
                tilo_ui_base::format::size(fetching.bytes)
            );
        })
        .map_err(|error| anyhow!("{}: {}", error.path.display(), error.problem))
        .context("preparing Tilo's fonts")?
    };
    // A catalog in the cache is found through the cache's place, which an app given a cache
    // directory of its own would not see: name it to every app this command starts.
    if let Some(cache) = cache.filter(|cache| root.starts_with(cache.join("catalogs"))) {
        tilo_app_builder::keep_font_artifact_cache(cache);
    }
    Ok(READY.get_or_init(|| root).clone())
}

/// Bundle the product catalog beside each of `programs` (`fonts/` in its directory), from the
/// catalog [`ensure`] found. A program built in a Tilo checkout finds the catalog the build
/// prepared beside it (`font-catalogs/` in its directory), and one bundled there would hide that
/// one from every program built there, so such a build bundles nothing.
pub fn bundle_beside<'a>(programs: impl IntoIterator<Item = &'a Path>) -> Result<()> {
    let root = ensure()?;
    for program in programs {
        let Some(directory) = program.parent() else {
            continue;
        };
        if directory.join("font-catalogs").is_dir() {
            continue;
        }
        let fonts = directory.join("fonts");
        prepare::bundle(&root, &fonts, Selection::Product)
            .map_err(|error| anyhow!("{}: {}", error.path.display(), error.problem))
            .with_context(|| format!("putting Tilo's fonts in {}", fonts.display()))?;
    }
    Ok(())
}

/// The font artifact cache every Tilo program on this machine looks in: `TILO_FONT_ASSET_CACHE`,
/// else `tilo/test-env/font-artifacts` in the user's cache directory.
fn artifact_cache() -> Option<PathBuf> {
    let paths = &tilo_process::environment::environment().paths;
    paths.font_artifact_cache.clone().or_else(|| {
        paths
            .cache_dir()
            .map(|cache| cache.join("tilo/test-env/font-artifacts"))
    })
}
