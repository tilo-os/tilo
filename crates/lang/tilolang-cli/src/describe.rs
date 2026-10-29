//! `tilolang describe`: an app's catalogue entry printed as tilolang, native and tilolang apps
//! alike.
//!
//! A native app is read from the `surface.toml` it commits next to its manifest; a tilolang
//! package is compiled and its generated surface read back the same way. Either way what is
//! printed is the catalogue's description ([`tilolang_tilo::catalogue::package_of`]), which is
//! also what a package that `use`s the app is checked against.

use std::path::{Path, PathBuf};

use tilolang::ids::SeededEntropy;
use tilolang::meta::PackageMeta;
use tilolang::source::{compile_dirs, dependency_order_with, search_of};
use tilolang_tilo::catalogue;

/// The catalogue's package of the app in `dir`: a native app's committed surface, or the
/// generated surface of the tilolang package there, compiled with the packages it uses (found in
/// `search`, else among its siblings).
///
/// # Errors
/// The directory is neither, or the package does not compile.
pub fn published(dir: &Path, search: &[PathBuf]) -> Result<PackageMeta, String> {
    if let Some(meta) = catalogue::native_app(dir)? {
        return Ok(meta);
    }
    let natives = catalogue::native_apps(&search_of(dir, search))?;
    let provided: Vec<String> = natives.iter().map(|meta| meta.app.clone()).collect();
    let dirs = dependency_order_with(dir, search, &provided)?;
    let built =
        compile_dirs(&dirs, &natives, &mut SeededEntropy(0)).map_err(|error| error.to_string())?;
    let meta = &built
        .last()
        .ok_or_else(|| format!("{}: no package", dir.display()))?
        .compiled
        .meta;
    let surfaces = tilolang_tilo::surface::package_surfaces(meta);
    Ok(catalogue::package_of(&meta.app, &meta.title, &surfaces))
}

/// What `tilolang describe` prints for the app in `dir`.
///
/// # Errors
/// As [`published`].
pub fn describe(dir: &Path, search: &[PathBuf]) -> Result<String, String> {
    published(dir, search).map(|meta| tilolang::describe::declarations(&meta))
}
