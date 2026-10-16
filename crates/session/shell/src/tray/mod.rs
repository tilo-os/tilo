//! The shell's side of the session's status-notifier tray.
//!
//! The host itself — `org.kde.StatusNotifierWatcher`, each item's
//! properties and menu, and the requests sent back — is [`tilo_tray_host`].
//! The shell supplies only the rasteriser for icons an item names rather
//! than sends ([`crate::app_icons::tray_raster`]), serves the watcher on the
//! session runtime, paints the items in the top bar and publishes them as
//! actions (`crate::shell::search::providers::tray_items`).

use std::sync::{Arc, OnceLock};

use tilo_tray_host::{IconRasteriser, Tray};

#[cfg(test)]
mod tests;

/// Named icons through the shell's icon theme lookup.
pub fn icon_rasteriser() -> IconRasteriser {
    Arc::new(|name, theme_path, edge| {
        let raster = crate::app_icons::tray_raster(name, theme_path, edge)?;
        Some((raster.width, raster.height, raster.rgba))
    })
}

/// The session's tray, served by [`tilo_tray_host::watcher::serve`].
pub fn global() -> &'static Arc<Tray> {
    static TRAY: OnceLock<Arc<Tray>> = OnceLock::new();
    TRAY.get_or_init(|| Tray::new(icon_rasteriser()))
}
