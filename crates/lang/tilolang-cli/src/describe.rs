//! `tilolang describe`: an app's catalogue entry printed as tilolang, native and tilolang apps
//! alike.
//!
//! A native app is read from the `surface.toml` it commits next to its manifest; a tilolang
//! package is printed from its own metadata, as it was compiled. Either way what is printed is
//! what a package that `use`s the app compiles against.

use std::path::{Path, PathBuf};

use tilolang::ids::SeededEntropy;
use tilolang::meta::PackageMeta;

use tilolang_tilo::catalogue;

/// The catalogue's package of the app in `dir`: a native app's committed surface, or the
/// metadata of the tilolang package there, compiled with the packages it uses (found in
/// `search`, else among its siblings).
pub fn published(dir: &Path, search: &[PathBuf]) -> Result<PackageMeta, String> {
    if let Some(meta) = catalogue::native_app(dir)? {
        return Ok(meta);
    }
    let (built, _) =
        crate::compile(dir, search, &mut SeededEntropy(0)).map_err(|error| error.to_string())?;
    let meta = &built
        .last()
        .ok_or_else(|| format!("{}: no package", dir.display()))?
        .compiled
        .meta;
    // The package's own declarations, as a package that `use`s it compiles against them: its
    // generated surface names each action by the id a change of it names, which is no word an
    // author reads here.
    Ok(meta.clone())
}

/// What `tilolang describe` prints for the app in `dir`.
///
/// # Errors
/// As [`published`].
pub fn describe(dir: &Path, search: &[PathBuf]) -> Result<String, String> {
    published(dir, search).map(|meta| tilolang::describe::declarations(&meta))
}
