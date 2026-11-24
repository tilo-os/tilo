//! The platform's apps a tilolang view reads but this runtime does not run: the device (its
//! sound, network, Bluetooth, modem, VPN, tethering, battery, power and displays), Settings
//! (every section of the person's settings and the device's, each set field by field through
//! its setters), Contacts (the workspace's contact directory), the image model
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
/// The app the person's MCP server is published as: `use tilo.mcp`. It is served by the
/// workspace's host, which lets each agent in, names the workspaces it reaches and stops one
/// at the person's word.
pub const MCP_SERVER_APP: &str = "tilo.mcp-server";

/// The app the workspace's activity is published as: `use so.tilo.history`. The app whose window
/// shows it is `so.tilo.activity`, and it reads this package (`history.history`, and the range a
/// view names of it) as any other package it uses.
pub const ACTIVITY_APP: &str = "so.tilo.history";
/// Contacts' app, whose directory of the workspace the session publishes.
/// The directory's package, whose root is one record of the workspace ([`CONTACTS_ROOTS`]).
/// Its own id, not the Contacts app's: the app is a tilolang package of that id, and a
/// package a view imports is one id.
pub const CONTACTS_APP: &str = "so.tilo.contacts.directory";
/// The settings sections' package: the person's and the device's settings as a view reads and
/// sets them, apart from the Settings app that shows them.
pub use tilo_actions::remote::settings_resources::SURFACE_APP as SECTIONS_APP;
/// The app a workspace's own actions are published under: `use tilo.workspace`.
pub const WORKSPACE_APP: &str = tilo_action_policy::agent_answers::WORKSPACE_APP;
/// The device's image model app: the picture bounded and sent to the model, as
/// actions, and the library analysis run (`tilo_vision_service`). It is a platform package,
/// not an installed app, so it has no first-party app id.
pub const VISION_APP: &str = "so.tilo.vision";

/// The app a workspace's invocations are read from: `use tilo.invocations`. A view lists them
/// with a query (`query(invocations.Invocation, status: .waiting, person: me)`), which the host
/// follows from the workspace ([`platform_listings`]).
pub const INVOCATIONS_APP: &str = tilo_actions::remote::workspace_invocations::PROVIDER;

/// The app the styles the device's installed packages offer are read from: `use tilo.styles as
/// styles`, then `styles.installed.offered`. The device publishes them in its person's own
/// workspace, at this app (`tilo_workspace_service::styles::STYLES_APP`).
pub const STYLES_APP: &str = "tilo.styles";
const STYLES_ROOTS: &[(&str, &str, &str)] = &[("installed", "Styles", STYLES_APP)];

/// The app the session's own resources are described as: `use so.tilo.session`.
pub const SESSION_APP: &str = "so.tilo.session";
/// The app a part of the shell is drawn as: `use desktop as desktop` reads the window menu's
/// reading (`desktop:window-menu`) and invokes its verbs.
pub const DESKTOP_APP: &str = "desktop";
/// What the session publishes, described as a package (`tilo_session::session_package`, which
/// a test holds to what its api publishes).
const SESSION_DESCRIPTION: &str = include_str!("../../../../apps/tl/session/surface.toml");

/// The session's actions as its description declares them, by id: what a call's arguments
/// are sent as ([`tilo_actions::surface::nest_arguments`]).
pub fn session_verbs()
-> Result<std::collections::BTreeMap<String, tilo_actions::surface::VerbSurface>, String> {
    crate::catalogue::loose_verbs(SESSION_DESCRIPTION)
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
    ("screen", "Screen", "session:screen"),
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
    ("folders", "Folders", "session:folders"),
    ("live", "Live", "session:live"),
    ("search", "Search", "session:search"),
    ("launchpad", "Launchpad", "session:launchpad"),
    ("wallpaper", "Wallpaper", "session:wallpaper"),
    ("said", "Said", "session:notice"),
    ("overlays", "Overlays", "desktop:overlays"),
    ("power_menu", "PowerMenu", "desktop:power-menu"),
    ("nearby_send", "NearbySend", "session:nearby-send"),
    ("devices", "Devices", "session:devices"),
    ("server", "Server", "device:server"),
    ("speakers", "Speakers", "session:speakers"),
    ("printers", "Printers", "printers"),
    ("users", "Users", "users"),
    ("drives", "Drives", "drives"),
    ("scanners", "Scanners", "scanners"),
    ("storage", "Storage", "storage"),
    ("disk_unlock", "DiskUnlock", "disk-unlock"),
    ("host_info", "HostInfo", "host-info"),
    ("codec_pack", "CodecPack", "codec-pack"),
    ("windows_compat", "WindowsCompat", "windows-compat"),
    ("firmware", "Firmware", "firmware"),
    ("containers", "Containers", "containers"),
    ("processes", "Processes", "processes"),
    ("disks", "Disks", "disks"),
    ("firewall", "Firewall", "firewall"),
    ("journal", "Journal", "journal"),
    ("modem", "Modem", "modem"),
    ("tethering", "Tethering", "tethering"),
    // What the machine booted, what update waits, and the zone its clock keeps.
    ("update", "SystemUpdate", "system-update"),
    ("time_zone", "TimeZone", "time-zone"),
    ("quick_settings", "QuickSettings", "session:quick-settings"),
    ("clipboard", "Clipboard", "session:clipboard"),
];
/// The activity's root, the one resource every change and every verb of it is on.
const ACTIVITY_ROOTS: &[(&str, &str, &str)] = &[("history", "Activity", "workspace.activity")];

/// The MCP server's root, the one resource every grant and every verb of it is on.
const MCP_SERVER_ROOTS: &[(&str, &str, &str)] = &[("server", "Server", "mcp.server")];
const VISION_ROOTS: &[(&str, &str, &str)] = &[
    ("image_model", "ImageModel", "vision:vision.image-model"),
    ("analysis", "Analysis", "vision:vision.analysis"),
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

/// What the settings provider publishes, as a view reads it: each section's resource with its
/// fields and the verbs it declares, the service's readings (the agent model choice, the
/// providers, the shortcuts table), and where each is published (written by the sections' own
/// test, beside them).
const SECTIONS_SURFACE: &str = include_str!("../../../desktop/settings-sections/published.toml");

/// What stands for the device's key in the address of one of the device's settings
/// (`devices/{device}/settings/power`): a view names the device's settings wherever it runs, and
/// the host reads and sets them on the device it runs on ([`on_device`]).
pub const DEVICE_KEY: &str = "{device}";

/// The resource `published` (as a view names it, [`platform_roots`]) on the device `device` (its
/// key in lowercase hex): where the session publishes it there.
#[must_use]
pub fn on_device(published: &str, device: &str) -> String {
    published.replace(DEVICE_KEY, device)
}

/// Each of Settings' resources a view names, read from the sections' surface: its name in a
/// view (`settings.power`), its kind, and where the session publishes it, a device section's
/// address with [`DEVICE_KEY`] where the device's key goes.
fn settings_roots() -> Result<Vec<(String, String, String)>, String> {
    #[derive(Deserialize)]
    struct Root {
        name: String,
        kind: String,
        published: String,
    }
    #[derive(Deserialize)]
    struct File {
        roots: Vec<Root>,
    }
    let file: File = toml::from_str(SECTIONS_SURFACE).map_err(|error| error.to_string())?;
    Ok(file
        .roots
        .into_iter()
        .map(|root| (root.name, root.kind, root.published))
        .collect())
}

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
    /// The types whose records a view lists with a query, each with the record of the app its
    /// listing is read from (`{"items": [...]}`); their filters are the defaults of their
    /// fields.
    listed: &'static [(&'static str, &'static str)],
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
        files: &[include_str!(
            "../../../desktop/device-surfaces/device-surface.json"
        )],
        described: false,
        listed: &[],
    },
    Published {
        app: SECTIONS_APP,
        title: "Settings",
        kinds: &[],
        // Its roots are the sections' ([`settings_roots`]), and every resource it
        // publishes is one of the sections'.
        roots: &[],
        files: &[SECTIONS_SURFACE],
        described: false,
        listed: &[],
    },
    Published {
        app: ACTIVITY_APP,
        title: "Activity",
        kinds: &[],
        roots: ACTIVITY_ROOTS,
        // What the workspace's activity reads as, beside the session that serves it.
        files: &[include_str!(
            "../../../desktop/session/activity-surface.json"
        )],
        described: false,
        listed: &[],
    },
    Published {
        app: MCP_SERVER_APP,
        title: "MCP server",
        kinds: &[],
        roots: MCP_SERVER_ROOTS,
        // What the server lets in reads as, written beside the host that serves it.
        files: &[include_str!(
            "../../../core/tilo-workspace-service/mcp-server-surface.json"
        )],
        described: false,
        listed: &[],
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
        listed: &[],
    },
    Published {
        app: VISION_APP,
        title: "Vision",
        kinds: &[],
        roots: VISION_ROOTS,
        files: &[include_str!("../../../services/vision/surface.json")],
        described: false,
        listed: &[],
    },
    Published {
        app: WORKSPACE_APP,
        title: "Workspace",
        kinds: &[],
        roots: &[],
        // What a workspace's own actions are, written beside the service that publishes them.
        files: &[include_str!(
            "../../../core/tilo-workspace-service/workspace-surface.toml"
        )],
        described: false,
        listed: &[],
    },
    Published {
        app: SESSION_APP,
        title: "Session",
        kinds: &[],
        roots: &[],
        files: &[include_str!("../../../../apps/tl/session/surface.toml")],
        described: true,
        listed: &[],
    },
    // A part of the shell a view draws: the window menu's reading as the view is compiled
    // against it (`use desktop as desktop`), which the shell's own view reads and no app of the
    // workspace declares.
    Published {
        app: DESKTOP_APP,
        title: "Window menu",
        kinds: &[],
        roots: &[],
        files: &[include_str!(
            "../../../desktop/session/window-menu-surface.toml"
        )],
        described: false,
        listed: &[],
    },
    Published {
        app: STYLES_APP,
        title: "Styles",
        kinds: &[],
        roots: STYLES_ROOTS,
        // What the styles read as, written beside the type the device publishes them as.
        files: &[include_str!(
            "../../../core/tilo-workspace-service/styles-surface.json"
        )],
        described: false,
        listed: &[],
    },
    Published {
        app: INVOCATIONS_APP,
        title: "Invocations",
        kinds: &[],
        roots: &[],
        // What a workspace's invocations read as, written beside the type they are read from.
        files: &[include_str!(
            "../../../core/actions/src/remote/invocations-surface.json"
        )],
        described: false,
        listed: &[("Invocation", "")],
    },
];

/// The device's surfaces as the file beside the session holds them: what the shell compiles
/// against and its own test holds to what the session publishes.
///
/// # Errors
///
/// A surface the file holds that does not read.
pub fn device_surfaces() -> Result<Vec<tilo_actions::surface::Surface>, String> {
    surfaces_of(include_str!(
        "../../../desktop/device-surfaces/device-surface.json"
    ))
}

/// The platform's resources a view reads, each as a view names it (`power:battery`,
/// `devices/{device}/settings/power`: the host reads one of the device's settings on its own
/// device, [`on_device`]) and as the view's type names it (`so.tilo.device#Battery`): what the
/// host reads for them. Each package's roots are its own table's, and Settings' are its
/// sections' ([`settings_roots`]), so one package's surface never keeps another's roots from a
/// view.
///
/// # Panics
/// Settings' committed surfaces do not read: a fault of the build itself.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "the surfaces are committed with the build; one that does not read is the build's fault"
)]
pub fn platform_roots() -> Vec<(String, String)> {
    static ROOTS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let tables = PUBLISHED.iter().flat_map(|published| {
                published.roots.iter().map(move |(_, ty, published_as)| {
                    (
                        (*published_as).to_owned(),
                        format!("{}#{ty}", published.app),
                    )
                })
            });
            tables
                .chain(settings_types().expect("Settings' committed surfaces read"))
                .collect()
        })
        .clone()
}

/// Settings' roots as [`platform_roots`] lists them: where each section is published, and the
/// type its kind reads as.
fn settings_types() -> Result<Vec<(String, String)>, String> {
    let mut surfaces = Vec::new();
    for published in PUBLISHED
        .iter()
        .filter(|published| published.app == SECTIONS_APP)
    {
        for file in published.files {
            surfaces.extend(surfaces_of(file)?);
        }
    }
    let types = crate::catalogue::kind_names_of(&surfaces);
    settings_roots()?
        .into_iter()
        .map(|(_, kind, published)| {
            let ty = types
                .get(&kind)
                .ok_or_else(|| format!("{SECTIONS_APP} publishes no `{kind}` with fields"))?;
            Ok((published, format!("{SECTIONS_APP}#{ty}")))
        })
        .collect()
}

/// The types whose records a view lists with a query (`query(invocations.Invocation)`), each as
/// the view's type names it (`tilo.invocations#Invocation`), with the app and the record its
/// listing is read from: what the host follows for a view that lists them
/// ([`tilolang_runtime::foreign::listing_of`]).
#[must_use]
pub fn platform_listings() -> Vec<(String, &'static str, &'static str)> {
    PUBLISHED
        .iter()
        .flat_map(|published| {
            published.listed.iter().map(move |(ty, record)| {
                (format!("{}#{ty}", published.app), published.app, *record)
            })
        })
        .collect()
}

/// The listing `ty` (`<app>#<Type>`) is read from, when a view lists its records with a query:
/// the app and its record.
#[must_use]
pub fn listing_of(ty: &str) -> Option<(&'static str, &'static str)> {
    platform_listings()
        .into_iter()
        .find(|(listed, _, _)| listed == ty)
        .map(|(_, app, record)| (app, record))
}

/// The packages of the platform's apps, each read from the surface it publishes, its resources
/// named where the session publishes them.
///
/// The surfaces are this build's own, so they are read once: every build of a package reads
/// them, a development loop included.
pub fn platform_packages() -> Result<Vec<PackageMeta>, String> {
    packages().iter().cloned().collect()
}

/// The packages of the platform's apps among `apps`, each read from the surface it publishes:
/// what a package whose `use`s name them compiles against. An app whose surface does not read
/// stops only a package that names it.
///
/// # Errors
/// A surface of one of `apps` does not read.
pub fn platform_packages_of<'a>(
    apps: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<PackageMeta>, String> {
    let named: std::collections::BTreeSet<&str> = apps.into_iter().collect();
    packages()
        .iter()
        .filter(|package| {
            package
                .as_ref()
                .map_or(true, |package| named.contains(package.app.as_str()))
        })
        .cloned()
        .collect()
}

/// The platform's packages the package written as `source` names with `use`: what it compiles
/// against, a built-in package or a test's alike.
///
/// # Errors
/// A named surface does not read.
pub fn named_platform_packages(source: &str) -> Result<Vec<PackageMeta>, String> {
    let named: Vec<String> = tilolang::parser::parse_module(source)
        .0
        .map(|module| {
            module
                .items
                .into_iter()
                .filter_map(|item| match item {
                    tilolang::ast::Item::Use(used) => Some(used.app),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    platform_packages_of(named.iter().map(String::as_str))
}

/// Each platform app's package, or why its surface does not read, read once for the build.
fn packages() -> &'static [Result<PackageMeta, String>] {
    static PACKAGES: std::sync::OnceLock<Vec<Result<PackageMeta, String>>> =
        std::sync::OnceLock::new();
    PACKAGES.get_or_init(|| {
        let mut packages: Vec<Result<PackageMeta, String>> =
            PUBLISHED.iter().map(package).collect();
        // What the workspace's action registry offers, read and called by code: the one package
        // that is a module of the language as much as an app (`use tilo.actions as actions`).
        packages.push(Ok(tilolang::actions_meta()));
        packages
    })
}

/// The package of the platform's app `app` alone ([`platform_packages`]): what a view that uses
/// only that app compiles against.
///
/// # Errors
/// The platform publishes no app `app`, or its surface does not read.
pub fn platform_package(app: &str) -> Result<PackageMeta, String> {
    if app == tilolang::ACTIONS_APP {
        return Ok(tilolang::actions_meta());
    }
    PUBLISHED
        .iter()
        .find(|published| published.app == app)
        .ok_or_else(|| format!("the platform publishes no app {app}"))
        .and_then(package)
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
    // ([`session_package`]): its actions keep the ids its surface names them by (`Shell/place`),
    // which is what the code calling them compiles to. An app's directory of the same name
    // (`apps/tl/session`) names them by its app too (`so.tilo.session#Shell/place`), which no
    // call the runtime loads makes; the description here stands first, so a build reads it.
    let mut meta = if published.described {
        let file = published.files.first().copied().unwrap_or_default();
        crate::catalogue::described(published.app, published.title, file)?
    } else {
        package_of(published.app, published.title, &surfaces)
    };
    meta = crate::catalogue::one_action_per_id(meta)?;
    if !published.listed.is_empty() {
        tilolang::search::describe_defaults(&mut meta);
    }
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
    if published.app == SECTIONS_APP {
        name_settings(&mut meta, &surfaces)?;
    }
    Ok(meta)
}

/// Settings' roots, one per section and reading ([`settings_roots`]), each the type its kind
/// reads as. Every field of a section is set by `set-<field>`, so a setting's verb can share its
/// id with one of another package's (Appearance's `set-mode` and the displays' `set-mode`): its
/// actions are named by their app, as a native app's are in a catalogue, so both packages load
/// side by side. The settings take the verb back ([`crate::catalogue::published_verb`]).
fn name_settings(meta: &mut PackageMeta, surfaces: &[Surface]) -> Result<(), String> {
    let types = crate::catalogue::kind_names_of(surfaces);
    for (name, kind, published) in settings_roots()? {
        let ty = types
            .get(&kind)
            .and_then(|ty| meta.types.iter().find(|declared| declared.name == *ty))
            .map(|declared| declared.id.clone())
            .ok_or_else(|| format!("{} publishes no `{kind}` with fields", meta.app))?;
        meta.named_roots.push(NamedRootMeta {
            name,
            ty,
            published: Some(published),
        });
    }
    for action in &mut meta.actions {
        action.id = crate::catalogue::action_id(&meta.app, &action.id);
    }
    Ok(())
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

/// One published resource, followed: its reading each time the session publishes one.
pub trait Reading: Send {
    /// The newest reading since the last call, `None` while nothing new came.
    fn latest(&self) -> Option<Result<String, String>>;
}

/// Where the platform's published resources are reached: the session's services, which a
/// process reaches over its connection.
pub trait PublishedSource: Send + Sync {
    /// Follow the reading of the published resource `resource`, waking `wake`'s owner at each
    /// new one.
    ///
    /// # Errors
    /// No session's services are reached, or the watch was refused.
    fn follow(
        &self,
        resource: &str,
        wake: tilo_rt::wake::WakeHandle,
    ) -> Result<Box<dyn Reading>, String>;

    /// Ask the published resource `resource` to run `action` with `arguments`, as the session
    /// serves it, and hand back what it said: a refusal is its words.
    ///
    /// # Errors
    /// No session's services are reached, or the change was refused or cancelled.
    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String>;
}

/// The session's services: the platform roots read where the session publishes them.
pub struct Session;

impl PublishedSource for Session {
    fn follow(
        &self,
        resource: &str,
        wake: tilo_rt::wake::WakeHandle,
    ) -> Result<Box<dyn Reading>, String> {
        Ok(Box::new(Followed(collaboration::SessionResource::follow(
            resource, wake,
        )?)))
    }

    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        collaboration::SessionResource::invoke_without_watching(resource, action, arguments)
    }
}

/// A resource of the session's followed: each reading the session publishes.
struct Followed(collaboration::SessionResource);

impl Reading for Followed {
    fn latest(&self) -> Option<Result<String, String>> {
        self.0.latest()
    }
}

static SOURCE: std::sync::OnceLock<Box<dyn PublishedSource>> = std::sync::OnceLock::new();

/// Read the platform's published resources through `source` from now on. Called once, by the
/// process's start; a process with none reads no platform root.
pub fn install(source: Box<dyn PublishedSource>) {
    let _ = SOURCE.set(source);
}

fn published() -> Option<&'static dyn PublishedSource> {
    SOURCE.get().map(Box::as_ref)
}

/// Load the platform packages `uses` names into `host`, as descriptions before the bundle that
/// uses them links: what its views read as `device.sound` or `history.history`. The apps loaded
/// are returned, so the caller follows the roots each publishes. A package that does not load is
/// said and left out: the views that read it draw their waiting state.
pub fn load(host: &crate::host::Host, uses: &[String]) -> Vec<String> {
    let Ok(packages) = platform_packages() else {
        tracing::warn!("the platform's packages do not read");
        return Vec::new();
    };
    let mut loaded = Vec::new();
    for meta in packages {
        if !uses.contains(&meta.app) {
            continue;
        }
        let app = meta.app.clone();
        if let Err(error) = host.runtime().load_foreign(meta) {
            tracing::warn!(%app, %error, "a platform package did not load");
            continue;
        }
        loaded.push(app);
    }
    loaded
}

/// The platform's roots one window reads: the packages its bundle `use`s, and each root followed
/// from where the session publishes it.
#[derive(Default)]
pub struct Roots {
    /// The apps among them whose package this window loaded.
    apps: std::collections::BTreeSet<String>,
    /// The roots followed, each with the resource of the app it is.
    followed: Vec<(crate::runtime::ResourceRef, Box<dyn Reading>)>,
    /// The roots a package declares, by id: followed for as long as the window runs. The others
    /// are read on demand — a reading keyed by a range — and only while a view shows one.
    declared: std::collections::BTreeSet<String>,
    /// Wakes the window when a followed root publishes a reading.
    wake: Option<tilo_rt::wake::WakeHandle>,
    /// Whether the packages and their roots were loaded from the bundle.
    loaded: bool,
}

impl Roots {
    /// The platform packages `uses` names, loaded into `host`, and each of their roots followed.
    /// What the session does not answer is said once and left reading as it would start.
    pub fn load(
        &mut self,
        host: &crate::host::Host,
        uses: &[String],
        wake: &tilo_rt::wake::WakeHandle,
    ) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.wake = Some(wake.clone());
        let apps = load(host, uses);
        if published().is_none() {
            self.apps.extend(apps);
            return;
        }
        let Ok(packages) = platform_packages() else {
            return;
        };
        for meta in packages {
            if !apps.contains(&meta.app) {
                continue;
            }
            for root in &meta.named_roots {
                let Some(published) = root.published.as_deref() else {
                    continue;
                };
                // The type a view reads the root as: its app and its name, as the platform's
                // roots name it (`so.tilo.device#Sound`).
                let Some(name) = meta
                    .types
                    .iter()
                    .find(|declared| declared.id == root.ty)
                    .map(|declared| declared.name.clone())
                else {
                    continue;
                };
                let Some(target) = crate::runtime::ResourceRef::parse_id(
                    published,
                    format!("{}#{name}", meta.app),
                ) else {
                    continue;
                };
                self.declared.insert(target.id_string());
                self.follow(&target, wake.clone());
                Self::seed(host, &target);
            }
        }
        self.apps.extend(apps);
    }

    /// Follow `target`, the resource the session publishes under its id.
    fn follow(&mut self, target: &crate::runtime::ResourceRef, wake: tilo_rt::wake::WakeHandle) {
        let Some(source) = published() else {
            return;
        };
        match source.follow(&target.id_string(), wake) {
            Ok(reading) => self.followed.push((target.clone(), reading)),
            Err(why) => tracing::debug!(
                resource = %target.id_string(),
                %why,
                "a platform root is not read here"
            ),
        }
    }

    /// Start `target` reading what it would before its app publishes anything, so a view that
    /// reads a field of it runs as the app opens, rather than trapping on a resource with no
    /// reading at all. The session's first reading replaces it.
    fn seed(host: &crate::host::Host, target: &crate::runtime::ResourceRef) {
        let _ = host.runtime().set_reading(target, "{}");
    }

    /// Whether `app` is a platform app this window reads: what its views change is its
    /// resources' actions, asked of the session's services.
    #[must_use]
    pub fn reads(&self, app: &str) -> bool {
        self.apps.contains(app)
    }

    /// Ask the published resource `resource` to run `action` with `arguments`, as the session
    /// serves it, and hand back what it said.
    ///
    /// # Errors
    /// No session's services are reached, or the change was refused or cancelled.
    pub fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        published()
            .ok_or_else(|| "nothing reads the platform's resources here".to_owned())?
            .invoke(resource, action, arguments)
    }

    /// Hand the runtime every new reading of the roots followed, and follow the ones its views
    /// read before they were. Whether a reading moved, which runs those views again, and only
    /// them: a reading is parsed once as it arrives, never per frame.
    pub fn read(
        &mut self,
        host: &crate::host::Host,
        reads: impl Fn(&crate::runtime::ResourceRef) -> bool,
    ) -> bool {
        let runtime = host.runtime();
        let mut moved = false;
        for (target, reading) in &self.followed {
            while let Some(result) = reading.latest() {
                match result {
                    Ok(reading) => {
                        moved |= runtime.set_reading(target, &reading).unwrap_or_else(|error| {
                            tracing::warn!(%error, resource = %target.id_string(), "a platform reading was refused");
                            false
                        });
                    }
                    Err(why) => {
                        tracing::debug!(resource = %target.id_string(), %why, "a platform reading failed");
                    }
                }
            }
        }
        // A view read a root before it was followed (a resource this bundle's platform package
        // publishes under no name it declares): it is followed now, and reads as it would start
        // until its reading comes.
        let apps = &self.apps;
        let wanted: Vec<(String, String)> = runtime.take_wanted_of(|app| apps.contains(app));
        let Some(wake) = self.wake.clone() else {
            return moved;
        };
        for (id, ty) in wanted {
            let Some((app, _name)) = ty.split_once('#') else {
                continue;
            };
            if !self.apps.contains(app)
                || self
                    .followed
                    .iter()
                    .any(|(target, _)| target.id_string() == id)
            {
                continue;
            }
            let Some(target) = crate::runtime::ResourceRef::parse_id(&id, ty.clone()) else {
                continue;
            };
            self.follow(&target, wake.clone());
            Self::seed(host, &target);
        }
        // A reading followed because a view read it goes once no view of this window reads it:
        // a detail beside another change, a range another window shows. The session stops
        // publishing it here, and the runtime keeps what was read of it no longer.
        let declared = self.declared.clone();
        self.followed
            .retain(|(target, _)| declared.contains(&target.id_string()) || reads(target));
        moved
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;

    /// Every root a published app names is a type its committed surface publishes. A resource
    /// added to the table without its surface fails here, naming the kind, before every package
    /// that reads the app does.
    #[test]
    fn every_published_root_is_a_type_its_surface_publishes() {
        for published in PUBLISHED {
            if published.roots.is_empty() {
                continue;
            }
            let file = published.files.first().copied().unwrap_or_default();
            let mut surfaces = Vec::new();
            for file in published.files {
                surfaces.extend(surfaces_of(file).unwrap_or_else(|why| {
                    panic!("{}'s surface does not read: {why}", published.app)
                }));
            }
            let meta = if published.described {
                crate::catalogue::described(published.app, published.title, file).unwrap_or_else(
                    |why| panic!("{}'s description does not read: {why}", published.app),
                )
            } else {
                package_of(published.app, published.title, &surfaces)
            };
            for (root, ty, _) in published.roots {
                assert!(
                    meta.types.iter().any(|declared| declared.name == *ty),
                    "{} publishes no `{ty}`, the type its root `{root}` names: add that kind to {file}",
                    published.app
                );
            }
        }
    }

    /// Code reads the catalogue and calls an action by address: the two verbs and the record
    /// they answer compile against the packages the platform publishes.
    #[test]
    fn the_registry_packages_are_what_code_names() {
        let source = r#"app test.registry "Registry"

use tilo.actions as actions
use tilo.invocations as invocations

type Counter {
    value: Int merge counter
}

root Counter

default view counter for Counter

view counter(c: Counter) {
    text("{c.value}")
}

action look() -> Int {
    let found = actions.catalogue(query: .none, app: .none)
    found.len()
}

action poke(address: Text, arguments: json.Value) -> Text {
    let made = actions.call(address: address, arguments: arguments)
    made.status.label()
}
"#;
        let used = platform_packages_of(["tilo.actions", "tilo.invocations"].into_iter())
            .expect("the platform's registry packages read");
        let compiled = tilolang::compile_package(source, &tilolang::IdsLock::new(), &used)
            .unwrap_or_else(|diagnostics| {
                panic!("the registry packages compile: {diagnostics:#?}")
            });
        assert!(
            compiled
                .meta
                .uses
                .contains(&tilolang::ACTIONS_APP.to_owned())
        );
    }
}
