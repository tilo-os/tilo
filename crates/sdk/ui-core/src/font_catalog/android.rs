//! The font catalog on Android.
//!
//! `tilo-app build --platform android` packs the catalog into the APK's
//! `assets/tilo/fonts`. Font files inside an APK have no path fontdb could
//! map, so the first start of each catalog version copies them into the app's
//! files directory; loading then verifies that copy like any installed
//! catalog. Android has no
//! fontconfig: the device's own faces, which cover scripts the catalog does
//! not, are read from `/system/fonts`.

use std::ffi::CString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cosmic_text::{FontSystem, fontdb};
use tilo_font_catalog::Scope;

use super::{FontCatalog, FontCatalogError, fallback};

const ASSET_ROOT: &str = "tilo/fonts";
const SYSTEM_FONTS: &str = "/system/fonts";

/// Share verified font data across renderers, as on Windows.
pub(crate) fn android_font_system(locale: String) -> Option<FontSystem> {
    static DATABASE: OnceLock<Option<fontdb::Database>> = OnceLock::new();
    let database = DATABASE.get_or_init(|| {
        let loaded = installed_catalog().and_then(|root| {
            FontCatalog::product(root, locale.clone())
                .load_with_system(|db| db.load_fonts_dir(SYSTEM_FONTS))
        });
        match loaded {
            Ok(font_system) => Some(font_system.into_locale_and_db().1),
            Err(error) => {
                tracing::error!(%error, "Android font catalog unavailable");
                None
            }
        }
    });
    database.as_ref().map(|db| {
        FontSystem::new_with_locale_and_db_and_fallback(locale, db.clone(), fallback::TiloFallback)
    })
}

/// The catalog's directory under `$XDG_DATA_HOME`, copied out of the APK if
/// this version is not there yet.
fn installed_catalog() -> Result<PathBuf, FontCatalogError> {
    let data = tilo_paths::data_dir()
        .ok_or_else(|| FontCatalogError::at("XDG_DATA_HOME", "no app data directory"))?;
    let versions = data.join("tilo/fonts");
    let root = versions.join(tilo_font_catalog::manifest_sha256());
    if root.join("manifest.toml").is_file() {
        return Ok(root);
    }
    let app = crate::launch::android::app()
        .ok_or_else(|| FontCatalogError::at(&root, "no Android activity"))?;
    let staging = versions.join(".staging");
    let _ = std::fs::remove_dir_all(&staging);
    extract(&app.asset_manager(), &staging)?;
    // Only one catalog version is ever loaded; earlier ones are dead weight.
    if let Ok(entries) = std::fs::read_dir(&versions) {
        for entry in entries.flatten() {
            if entry.path() != staging {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    std::fs::rename(&staging, &root).map_err(|e| FontCatalogError::at(&root, e))?;
    Ok(root)
}

/// Copy every product font, then the manifest last: a directory holding a
/// manifest is a complete copy.
fn extract(assets: &ndk::asset::AssetManager, into: &Path) -> Result<(), FontCatalogError> {
    let manifest = tilo_font_catalog::manifest()?;
    for asset in manifest.font.iter().filter(|a| a.scope == Scope::Product) {
        copy_asset(assets, &asset.file, &asset.path_in(into)?)?;
    }
    copy_asset(assets, "manifest.toml", &into.join("manifest.toml"))
}

fn copy_asset(
    assets: &ndk::asset::AssetManager,
    name: &str,
    to: &Path,
) -> Result<(), FontCatalogError> {
    let source = format!("{ASSET_ROOT}/{name}");
    let c_source = CString::new(source.as_str()).map_err(|e| FontCatalogError::at(&source, e))?;
    let mut asset = assets
        .open(&c_source)
        .ok_or_else(|| FontCatalogError::at(&source, "missing from the APK"))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| FontCatalogError::at(parent, e))?;
    }
    let mut file = std::fs::File::create(to).map_err(|e| FontCatalogError::at(to, e))?;
    std::io::copy(&mut asset, &mut file).map_err(|e| FontCatalogError::at(to, e))?;
    file.flush().map_err(|e| FontCatalogError::at(to, e))
}
