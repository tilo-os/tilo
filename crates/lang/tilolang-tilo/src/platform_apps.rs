//! The platform's apps a tilolang view reads but this runtime does not run: the device (its
//! sound, network, Bluetooth, battery, power and displays), Settings (the person's appearance
//! and notification settings), Contacts (the workspace's contact directory) and the image
//! model (the device's vision actions and the library analysis run).
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
pub use tilo_app_id::apps::SETTINGS as SETTINGS_APP;
/// The device's image model app: the picture bounded and sent to the model, as
/// actions, and the library analysis run (`tilo_vision_service`). It is a platform package,
/// not an installed app, so it has no first-party app id.
pub const VISION_APP: &str = "so.tilo.vision";

/// The device's surface as the session publishes it, which the session holds to its services.
const DEVICE_SURFACE: &str = include_str!("../../../session/session/device-surface.json");
/// The app the session's own resources are described as: `use so.tilo.session`.
pub const SESSION_APP: &str = "so.tilo.session";
/// What the session publishes, described as a package (`tilo_session::session_package`, which
/// a test holds to what its api publishes).
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
/// The directory's package, whose root is one record of the workspace ([`CONTACTS_ROOTS`]).
/// Its own id, not the Contacts app's (`so.tilo.contacts`): the app is a tilolang package of
/// that id, and a package a view imports is one id.
pub const CONTACTS_APP: &str = "so.tilo.contacts.directory";
/// The directory's surface, as the crate that keeps it publishes it: written from
/// the record's own declarations (`tilo-contact-directory`'s test), so the type and
/// verbs a view calls are the record's.
const CONTACTS_SURFACE: &str =
    include_str!("../../../integrations/tilo-contact-directory/surface.json");
/// The settings service's own resource a view reads and changes: the agent
/// model choice, over the device's provider section and the person's (its
/// committed surface beside the service, written by its own test, as
/// `device-surface.json` is beside the session). Settings' window does not
/// edit it through this service, so it is published beside Settings' surface
/// rather than in it.
const AGENT_MODELS_SURFACE: &str =
    include_str!("../../../session/settings-service/agent-models-surface.json");
/// The image model service's resources, the surface its own test writes
/// beside it (`surface.json`), as `agent-models-surface.json` is.
const VISION_SURFACE: &str = include_str!("../../../services/vision/surface.json");

/// Each published resource a view names, by package: the name a view reads it by
/// (`device.sound`), its type there, and the resource the session publishes it as.
const DEVICE_ROOTS: &[(&str, &str, &str)] = &[
    ("sound", "Sound", "sound"),
    ("network", "Network", "network"),
    ("bluetooth", "Bluetooth", "bluetooth"),
    ("battery", "Battery", "power:battery"),
    ("power", "Power", "power"),
    ("displays", "Displays", "session:displays"),
    ("notifications", "Notifications", "session:notifications"),
    ("osd", "Osd", "session:osd"),
    ("vpn", "Vpn", "vpn"),
    ("night_light", "NightLight", "session:night-light"),
    ("banners", "Banners", "session:banners"),
    ("time", "Time", "session:time"),
    ("apps", "Apps", "session:apps"),
    ("windows", "Windows", "session:windows"),
    ("tray", "Tray", "session:tray"),
    ("agents", "Agents", "session:agents"),
    ("styles", "Styles", "session:styles"),
    ("folders", "Folders", "session:folders"),
    ("overlays", "Overlays", "desktop:overlays"),
    ("nearby_send", "NearbySend", "session:nearby-send"),
    ("devices", "Devices", "session:devices"),
    ("speakers", "Speakers", "session:speakers"),
    ("quick_settings", "QuickSettings", "session:quick-settings"),
];
const VISION_ROOTS: &[(&str, &str, &str)] = &[
    ("image_model", "ImageModel", "vision:vision.image-model"),
    ("analysis", "Analysis", "vision:vision.analysis"),
];
const SETTINGS_ROOTS: &[(&str, &str, &str)] = &[
    ("appearance", "Appearance", "settings:settings.appearance"),
    (
        "notifications",
        "Notifications",
        "settings:settings.notifications",
    ),
    // The displays' settings: whether the screen stays put when the device turns.
    ("display", "Display", "settings:settings.display"),
    // The agent model choice the canvas shows and switches: a reading of
    // both provider sections, its `use-model` answered by the settings
    // service (`agent-models-surface.json`).
    (
        "agent_models",
        "AgentModels",
        "settings:settings.agent-models",
    ),
];
/// The contacts directory of the workspace, which Contacts keeps and Mail,
/// Calendar, Messages and a tilolang view read: one record every member sees,
/// its actions the ones Contacts publishes (`project-contact`,
/// `confirm-endpoint`, `confirm-account`, and the rest), answered by the
/// session's contacts service.
const CONTACTS_ROOTS: &[(&str, &str, &str)] = &[(
    "directory",
    "Identities",
    "contacts:tilo.contacts.identities",
)];

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
        .chain(
            CONTACTS_ROOTS
                .iter()
                .map(|(_, ty, published)| (*published, format!("{CONTACTS_APP}#{ty}"))),
        )
        .chain(
            VISION_ROOTS
                .iter()
                .map(|(_, ty, published)| (*published, format!("{VISION_APP}#{ty}"))),
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

/// The image model service's resources, as its committed surface describes
/// them.
fn vision_surfaces() -> Result<Vec<Surface>, String> {
    #[derive(Deserialize)]
    struct File {
        resources: Vec<Surface>,
    }
    serde_json::from_str::<File>(VISION_SURFACE)
        .map(|file| file.resources)
        .map_err(|error| error.to_string())
}

/// Settings' resources as its package publishes them: those its committed
/// `surface.toml` declares, and the settings service's own
/// ([`AGENT_MODELS_SURFACE`]) a view reads beside them.
fn settings_surfaces() -> Result<Vec<Surface>, String> {
    #[derive(Deserialize)]
    struct File {
        resources: Vec<Surface>,
    }
    let mut surfaces = parse_surfaces(SETTINGS_SURFACE)?;
    let service: File =
        serde_json::from_str(AGENT_MODELS_SURFACE).map_err(|error| error.to_string())?;
    surfaces.extend(service.resources);
    Ok(surfaces)
}

/// The kind of the workspace's contact directory, kept by Contacts and
/// published by the session ([`CONTACTS_ROOTS`]).
const CONTACTS_DIRECTORY_KIND: &str = "tilo.contacts.identities";

/// Contacts' resources as its package publishes them: the directory of the
/// workspace, with its type's fields and its verbs. The book Contacts keeps for
/// its own window is not one: a view of another language names what the session
/// publishes, and the session publishes the directory.
fn contacts_surfaces() -> Result<Vec<Surface>, String> {
    #[derive(Deserialize)]
    struct File {
        resources: Vec<Surface>,
    }
    let file: File = serde_json::from_str(CONTACTS_SURFACE).map_err(|error| error.to_string())?;
    let surfaces: Vec<Surface> = file
        .resources
        .into_iter()
        .filter(|surface| surface.kind == CONTACTS_DIRECTORY_KIND)
        .collect();
    if surfaces.is_empty() {
        return Err(format!("Contacts publishes no `{CONTACTS_DIRECTORY_KIND}`"));
    }
    Ok(surfaces)
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
        package_of(SETTINGS_APP, "Settings", &settings_surfaces()?),
        SETTINGS_ROOTS,
    )?;
    let contacts = named(
        package_of(CONTACTS_APP, "Contacts", &contacts_surfaces()?),
        CONTACTS_ROOTS,
    )?;
    let vision = named(
        package_of(VISION_APP, "Vision", &vision_surfaces()?),
        VISION_ROOTS,
    )?;
    // The session's own resources are described here, as the runtime loads them
    // ([`session_package`]): its actions keep the ids its surface names them by, which is what
    // the code calling them compiles to. An app's directory of the same name
    // (`apps/tl/session`) names them by its app (`so.tilo.session#place`), which no call the
    // runtime loads makes; the description here stands first, so a build reads it.
    Ok(vec![device, settings, contacts, vision, session_package()?])
}
