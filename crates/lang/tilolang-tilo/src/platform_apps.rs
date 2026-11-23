//! The platform's apps a tilolang view reads but this runtime does not run: the device (its
//! sound, network, Bluetooth, modem, VPN, tethering, battery, power and displays), Settings
//! (the person's appearance
//! and notification settings), Contacts (the workspace's contact directory), the image model
//! (the device's vision actions and the library analysis run), Files, and a workspace's own
//! actions (its members, roles, invitations and permissions).
//!
//! Each is a package read from the surface it publishes ([`crate::catalogue::package_of`]):
//! its types are what its resources read as and its actions are their published verbs. Its
//! named roots are the resources the session publishes (`use so.tilo.device as device` and then
//! `device.sound` reads the session's `sound`). The host hands their readings in
//! ([`crate::instance::Instance::set_reading`]) and answers their actions by asking the session,
//! so a view of them never holds a copy.
//!
//! One table ([`PUBLISHED`]) maps each app to the committed surface files it is read from, so a
//! new one is a row, and `tilolang check` and a task's host read the same packages.

use serde::Deserialize;
use tilo_actions::surface::Surface;
use tilolang::PackageMeta;
use tilolang::meta::NamedRootMeta;

use crate::catalogue::{package_of, parse_surfaces};

/// The app the device's resources are published as: `use so.tilo.device`.
pub const DEVICE_APP: &str = "so.tilo.device";
/// Contacts' app, whose directory of the workspace the session publishes.
/// The directory's package, whose root is one record of the workspace ([`CONTACTS_ROOTS`]).
/// Its own id, not the Contacts app's: the app is a tilolang package of that id, and a
/// package a view imports is one id.
pub const CONTACTS_APP: &str = "so.tilo.contacts.directory";
/// Files' app, whose folders and trees the session publishes.
/// Settings' app, whose sections the session keeps and publishes.
pub use tilo_app_id::apps::SETTINGS as SETTINGS_APP;
/// The app a workspace's own actions are published under: `use tilo.workspace`.
pub const WORKSPACE_APP: &str = tilo_action_policy::agent_answers::WORKSPACE_APP;
/// The device's image model app: the picture bounded and sent to the model, as
/// actions, and the library analysis run (`tilo_vision_service`). It is a platform package,
/// not an installed app, so it has no first-party app id.
pub const VISION_APP: &str = "so.tilo.vision";

/// The app the session's own resources are described as: `use so.tilo.session`.
pub const SESSION_APP: &str = "so.tilo.session";
/// What the session publishes, described as a package (`tilo_session::session_package`, which
/// a test holds to what its api publishes).
const SESSION_DESCRIPTION: &str = include_str!("../../../../apps/tl/session/surface.toml");

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

/// The session's package: its resources' types and actions, which a shell's views, an agent's
/// task and the REPL call as any package's (`session.place(resource: …, x: 80, y: 60)`).
pub fn session_package() -> Result<PackageMeta, String> {
    crate::catalogue::described(SESSION_APP, "Session", SESSION_DESCRIPTION)
}

/// The resource each of the session's loose actions is published on, by the action's id.
pub fn session_addresses() -> Result<std::collections::BTreeMap<String, String>, String> {
    crate::catalogue::addresses(SESSION_DESCRIPTION)
}

/// The resource kinds a program reads of Contacts: the workspace's directory, which Mail,
/// Calendar, Messages and a tilolang view read. The book Contacts keeps for its own window is
/// not one of them.
const CONTACTS_DIRECTORY_KIND: &str = "tilo.contacts.identities";

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
    ("printers", "Printers", "printers"),
    ("modem", "Modem", "modem"),
    ("tethering", "Tethering", "tethering"),
    // What the machine booted, what update waits, and the zone its clock keeps.
    ("update", "SystemUpdate", "system-update"),
    ("time_zone", "TimeZone", "time-zone"),
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
    // Every other section the person's settings keep: each published as its record, with the
    // fields a view shows and the verbs that change it as its own actions.
    (
        "accessibility",
        "Accessibility",
        "settings:settings.accessibility",
    ),
    (
        "accessibility_device",
        "AccessibilityDevice",
        "settings:settings.accessibility-device",
    ),
    ("input", "Input", "settings:settings.input"),
    (
        "input_device",
        "InputDevice",
        "settings:settings.input-device",
    ),
    (
        "text_assistance",
        "TextAssistance",
        "settings:settings.text-assistance",
    ),
    ("locale", "Locale", "settings:settings.locale"),
    ("shortcuts", "Shortcuts", "settings:settings.shortcuts"),
    ("continuity", "Continuity", "settings:settings.continuity"),
    ("updates", "Updates", "settings:settings.updates"),
    ("clock", "Clock", "settings:settings.clock"),
    (
        "default_apps",
        "DefaultApps",
        "settings:settings.default-apps",
    ),
    ("identity", "Identity", "settings:settings.identity"),
    ("night_light", "NightLight", "settings:settings.night-light"),
    ("audio", "Audio", "settings:settings.audio"),
    (
        "network_usage",
        "NetworkUsage",
        "settings:settings.network-usage",
    ),
    ("device_vpn", "DeviceVpn", "settings:settings.device-vpn"),
    (
        "audio_device",
        "AudioDevice",
        "settings:settings.audio-device",
    ),
    (
        "desktop_controls",
        "DesktopControls",
        "settings:settings.desktop-controls",
    ),
    (
        "desktop_controls_device",
        "DesktopControlsDevice",
        "settings:settings.desktop-controls-device",
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

/// One app the platform publishes to programs: what people call it, the resource kinds of its
/// surface a program reads (all of them when empty), the roots a program names of it
/// (`(name, type, published resource)`), and the committed surface files it is read from.
struct Published {
    app: &'static str,
    title: &'static str,
    kinds: &'static [&'static str],
    roots: &'static [(&'static str, &'static str, &'static str)],
    /// The app's own committed surface, and beside it the surface of a service that publishes
    /// some of its resources (the settings service's agent model choice, the image model
    /// service's run): each read the same way, whichever wrote it.
    files: &'static [&'static str],
    /// Its actions keep the ids its file declares (a tilolang package's description), rather
    /// than being named by its app.
    described: bool,
}

/// The apps the platform publishes to programs, one row each: where an app keeps the surface
/// it publishes, which a program naming it compiles against and the host reads its readings
/// by. `tilolang check` reads these files; a task's host loads the same packages.
const PUBLISHED: &[Published] = &[
    Published {
        app: DEVICE_APP,
        title: "Device",
        kinds: &[],
        roots: DEVICE_ROOTS,
        files: &[include_str!("../../../session/session/device-surface.json")],
        described: false,
    },
    Published {
        app: SETTINGS_APP,
        title: "Settings",
        kinds: &[],
        roots: SETTINGS_ROOTS,
        files: &[
            include_str!("../../../../apps/settings/surface.toml"),
            include_str!("../../../session/settings-service/agent-models-surface.json"),
        ],
        described: false,
    },
    Published {
        app: CONTACTS_APP,
        title: "Contacts",
        kinds: &[CONTACTS_DIRECTORY_KIND],
        roots: CONTACTS_ROOTS,
        files: &[include_str!(
            "../../../integrations/tilo-contact-directory/surface.json"
        )],
        described: false,
    },
    Published {
        app: VISION_APP,
        title: "Vision",
        kinds: &[],
        roots: VISION_ROOTS,
        files: &[include_str!("../../../services/vision/surface.json")],
        described: false,
    },
    Published {
        app: WORKSPACE_APP,
        title: "Workspace",
        kinds: &[],
        roots: &[],
        // What a workspace's own actions are, written beside the service that publishes them.
        files: &[include_str!(
            "../../../server/tilo-workspace-service/workspace-surface.toml"
        )],
        described: false,
    },
    Published {
        app: SESSION_APP,
        title: "Session",
        kinds: &[],
        roots: &[],
        files: &[include_str!("../../../../apps/tl/session/surface.toml")],
        described: true,
    },
];

/// The device's surfaces as the file beside the session holds them: what the shell compiles
/// against and its own test holds to what the session publishes.
///
/// # Errors
///
/// A surface the file holds that does not read.
pub fn device_surfaces() -> Result<Vec<tilo_actions::surface::Surface>, String> {
    surfaces_of(include_str!("../../../session/session/device-surface.json"))
}

/// The platform's resources a view reads, each as the session publishes it (`power:battery`)
/// and as the view's type names it (`so.tilo.device#Battery`): what the host reads for them.
#[must_use]
pub fn platform_roots() -> Vec<(&'static str, String)> {
    PUBLISHED
        .iter()
        .flat_map(|published| {
            published.roots.iter().map(move |(_, ty, published_as)| {
                (*published_as, format!("{}#{ty}", published.app))
            })
        })
        .collect()
}

/// The packages of the platform's apps, each read from the surface it publishes, its resources
/// named where the session publishes them.
///
/// The surfaces are this build's own, so they are read once: every build of a package reads
/// them, a development loop included.
pub fn platform_packages() -> Result<Vec<PackageMeta>, String> {
    static PACKAGES: std::sync::OnceLock<Result<Vec<PackageMeta>, String>> =
        std::sync::OnceLock::new();
    PACKAGES.get_or_init(read_packages).clone()
}

fn read_packages() -> Result<Vec<PackageMeta>, String> {
    PUBLISHED.iter().map(package).collect()
}

/// The package [`Published`] describes: its surfaces, the resources a program reads of them,
/// and the roots a program names.
fn package(published: &Published) -> Result<PackageMeta, String> {
    let mut surfaces = Vec::new();
    for file in published.files {
        surfaces.extend(surfaces_of(file)?);
    }
    if !published.kinds.is_empty() {
        surfaces.retain(|surface| published.kinds.contains(&surface.kind.as_str()));
        let missing: Vec<&str> = published
            .kinds
            .iter()
            .copied()
            .filter(|kind| !surfaces.iter().any(|surface| surface.kind == *kind))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "{} publishes no `{}`",
                published.app,
                missing.join("`, `")
            ));
        }
    }
    // The session's own resources are described here, as the runtime loads them
    // ([`session_package`]): its actions keep the ids its surface names them by, which is what
    // the code calling them compiles to. An app's directory of the same name
    // (`apps/tl/session`) names them by its app (`so.tilo.session#place`), which no call the
    // runtime loads makes; the description here stands first, so a build reads it.
    let mut meta = if published.described {
        let file = published.files.first().copied().unwrap_or_default();
        crate::catalogue::described(published.app, published.title, file)?
    } else {
        package_of(published.app, published.title, &surfaces)
    };
    for (name, ty, published_as) in published.roots {
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
            published: Some((*published_as).to_owned()),
        });
    }
    Ok(meta)
}

/// The resources a surface file publishes, however it was written: a service's JSON, or an
/// app's `surface.toml`.
fn surfaces_of(file: &str) -> Result<Vec<Surface>, String> {
    #[derive(Deserialize)]
    struct Json {
        resources: Vec<Surface>,
    }
    if file.trim_start().starts_with('{') {
        let json: Json = serde_json::from_str(file).map_err(|error| error.to_string())?;
        Ok(json.resources)
    } else {
        parse_surfaces(file)
    }
}
