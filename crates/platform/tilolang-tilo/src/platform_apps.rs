//! The platform's apps a tilolang view reads but this runtime does not run: the device (its
//! sound, network, Bluetooth, battery, power and displays) and Settings (the person's
//! appearance and notification settings).
//!
//! Each is a package read from the surface it publishes ([`crate::catalogue::package_of`]):
//! its types are what its resources read as and its actions are their published verbs. Its
//! named roots are the resources the session publishes (`use so.tilo.device as device` and then
//! `device.sound` reads the session's `sound`). The host hands their readings in
//! ([`crate::instance::Instance::set_reading`]) and answers their actions by asking the session,
//! so a view of them never holds a copy.

use serde::Deserialize;
use tilo_actions::surface::Surface;
use tilolang::PackageMeta;
use tilolang::meta::NamedRootMeta;

use crate::catalogue::{package_of, parse_surfaces};

/// The app the device's resources are published as: `use so.tilo.device`.
pub const DEVICE_APP: &str = "so.tilo.device";
/// Settings' app, whose sections the session keeps and publishes.
pub const SETTINGS_APP: &str = "so.tilo.settings";

/// The device's surface as the session publishes it, which the session holds to its services.
const DEVICE_SURFACE: &str = include_str!("../../../session/session/device-surface.json");
/// Settings' committed surface, the one its package ships.
const SETTINGS_SURFACE: &str = include_str!("../../../../apps/settings/surface.toml");

/// Each published resource a view names, by package: the name a view reads it by
/// (`device.sound`), its type there, and the resource the session publishes it as.
const DEVICE_ROOTS: &[(&str, &str, &str)] = &[
    ("sound", "Sound", "sound"),
    ("network", "Network", "network"),
    ("bluetooth", "Bluetooth", "bluetooth"),
    ("battery", "Battery", "power:battery"),
    ("power", "Power", "power"),
    ("displays", "Displays", "session:displays"),
    ("desk", "Desk", "session:desk"),
    ("arrangement", "Arrangement", "shell:arrangement"),
];
const SETTINGS_ROOTS: &[(&str, &str, &str)] = &[
    ("appearance", "Appearance", "settings:settings.appearance"),
    (
        "notifications",
        "Notifications",
        "settings:settings.notifications",
    ),
];

/// The platform's resources a view reads, each as the session publishes it (`power:battery`)
/// and as the view's type names it (`so.tilo.device#Battery`): what the host reads for them.
#[must_use]
pub fn platform_roots() -> Vec<(&'static str, String)> {
    DEVICE_ROOTS
        .iter()
        .map(|(_, ty, published)| (*published, format!("{DEVICE_APP}#{ty}")))
        .chain(
            SETTINGS_ROOTS
                .iter()
                .map(|(_, ty, published)| (*published, format!("{SETTINGS_APP}#{ty}"))),
        )
        .collect()
}

/// The device's resources the session publishes, as their surfaces.
///
/// # Errors
/// The committed file is not a surface file.
pub fn device_surfaces() -> Result<Vec<Surface>, String> {
    #[derive(Deserialize)]
    struct File {
        resources: Vec<Surface>,
    }
    serde_json::from_str::<File>(DEVICE_SURFACE)
        .map(|file| file.resources)
        .map_err(|error| error.to_string())
}

/// The packages of the device and Settings, each read from the surface it publishes, its
/// resources named where the session publishes them.
///
/// # Errors
/// A surface does not read, or does not have a type a view names.
pub fn platform_packages() -> Result<Vec<PackageMeta>, String> {
    let named = |mut meta: PackageMeta, roots: &[(&str, &str, &str)]| {
        for (name, ty, published) in roots {
            let Some(id) = meta
                .types
                .iter()
                .find(|declared| declared.name == *ty)
                .map(|declared| declared.id.clone())
            else {
                return Err(format!("{} publishes no `{ty}`", meta.app));
            };
            meta.named_roots.push(NamedRootMeta {
                name: (*name).to_owned(),
                ty: id,
                published: Some((*published).to_owned()),
            });
        }
        Ok(meta)
    };
    let device = named(
        package_of(DEVICE_APP, "Device", &device_surfaces()?),
        DEVICE_ROOTS,
    )?;
    let settings = named(
        package_of(SETTINGS_APP, "Settings", &parse_surfaces(SETTINGS_SURFACE)?),
        SETTINGS_ROOTS,
    )?;
    Ok(vec![device, settings])
}
