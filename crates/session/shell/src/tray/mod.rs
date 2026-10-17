//! The shell's side of the session's status-notifier tray.
//!
//! The host itself — `org.kde.StatusNotifierWatcher`, each item's
//! properties and menu, and the requests sent back — is [`tilo_tray_host`].
//! The shell supplies only the rasteriser for icons an item names rather
//! than sends ([`crate::app_icons::tray_raster`]), serves the watcher on the
//! session runtime, paints the items in the top bar and publishes them as
//! actions (`crate::shell::search::providers::tray_items`).

use std::sync::{Arc, OnceLock};

use tilo_tray_host::{IconRasteriser, OwnerResolver, Tray};

#[cfg(test)]
mod tests;

/// Named icons through the shell's icon theme lookup.
pub fn icon_rasteriser() -> IconRasteriser {
    Arc::new(|name, theme_path, edge| {
        let raster = crate::app_icons::tray_raster(name, theme_path, edge)?;
        Some((raster.width, raster.height, raster.rgba))
    })
}

/// Which app keeps an item in the tray, proven from the registering
/// process with the checks notifications use: the Flatpak whose sandbox it
/// runs in (its connection is normally the sandbox's xdg-dbus-proxy), else
/// the host app a launch by this shell started. An app that lives on in the
/// tray is reopened through its item ([`crate::shell::host_apps`]).
pub fn owner_resolver() -> OwnerResolver {
    Arc::new(owner_of_process)
}

#[cfg(target_os = "linux")]
fn owner_of_process(pid: u32) -> Option<String> {
    crate::notifications::verified_flatpak_app_id(pid)
        .or_else(|| crate::shell::launcher::verified_package_for_descendant(pid))
}

#[cfg(not(target_os = "linux"))]
fn owner_of_process(_pid: u32) -> Option<String> {
    None
}

/// The session's tray, served by [`tilo_tray_host::watcher::serve`].
pub fn global() -> &'static Arc<Tray> {
    static TRAY: OnceLock<Arc<Tray>> = OnceLock::new();
    TRAY.get_or_init(|| Tray::with_owner_resolver(icon_rasteriser(), owner_resolver()))
}
