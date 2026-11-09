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
/// The app the session's own resources are described as: `use so.tilo.session`.
pub const SESSION_APP: &str = "so.tilo.session";
/// What the session publishes, described as a package (`tilo_session::session_package`, which
/// a test holds to what its door publishes).
const SESSION_DESCRIPTION: &str = include_str!("../../../../apps/tl/session/surface.toml");

/// The session's package: its resources' types and actions, which a shell's views, an agent's
/// task and the REPL call as any package's (`session.place(resource: …, x: 80, y: 60)`).
pub fn session_package() -> Result<PackageMeta, String> {
    crate::catalogue::described(SESSION_APP, "Session", SESSION_DESCRIPTION)
}

/// The resource each of the session's loose actions is published on, by the action's id.
pub fn session_addresses() -> Result<std::collections::BTreeMap<String, String>, String> {
    crate::catalogue::addresses(SESSION_DESCRIPTION)
}

/// The session's actions as its description declares them, by id: what a call's arguments
/// are sent as ([`tilo_actions::surface::nest_arguments`]).
pub fn session_verbs()
-> Result<std::collections::BTreeMap<String, tilo_actions::surface::VerbSurface>, String> {
    Ok(crate::catalogue::parse_surfaces(SESSION_DESCRIPTION)?
        .into_iter()
        .flat_map(|surface| surface.verbs)
        .map(|verb| (verb.id.clone(), verb))
        .collect())
}

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
/// The surfaces are this build's own, so they are read once: every build of a package reads
/// them, a development loop included.
pub fn platform_packages() -> Result<Vec<PackageMeta>, String> {
    static PACKAGES: std::sync::OnceLock<Result<Vec<PackageMeta>, String>> =
        std::sync::OnceLock::new();
    PACKAGES.get_or_init(read_packages).clone()
}

fn read_packages() -> Result<Vec<PackageMeta>, String> {
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
    // The session's own resources are described here, as the runtime loads them
    // ([`session_package`]): its actions keep the ids its surface names them by, which is what
    // the code calling them compiles to. An app's directory of the same name
    // (`apps/tl/session`) names them by its app (`so.tilo.session#place`), which no call the
    // runtime loads makes; the description here stands first, so a build reads it.
    Ok(vec![device, settings, session_package()?])
}
