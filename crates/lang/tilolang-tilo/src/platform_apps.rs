//! The platform's apps a tilolang view reads but this runtime does not run: the device (its
//! sound, network, Bluetooth, modem, VPN, tethering, battery, power and displays), Settings
//! (every section of the person's settings and the device's, each set field by field through
//! its setters), Contacts (the workspace's contact directory), the image model
//! (the device's vision actions and the library analysis run), Files, a workspace's connected
//! accounts, its model providers, the person's account with their host, the problems this
//! computer recorded, and its own actions (its
//! members, roles, invitations and permissions).
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
//!
//! Who is here, the windows this device shares and the invitations into a workspace are the
//! shell's to draw as `device.people`, `device.shares` and `device.invite`, beside the sound and
//! the rest.

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

/// The app a workspace's model providers are published as: `use tilo.models`. The host that
/// runs the workspace keeps the providers, their keys in its vault, and the model each kind of
/// work runs on; a window reads them as `models.models` and changes them through its verbs.
pub const MODELS_APP: &str = "tilo.models";

/// The app a workspace's connected accounts are published as: `use tilo.accounts`. The host that
/// runs the workspace keeps the accounts record and the secrets in its vault; a window reads
/// them as `accounts.accounts`, each with the address it signs in at and how its last sync went,
/// and connects, signs in again and removes them through the resource's verbs.
pub const ACCOUNTS_APP: &str = "tilo.accounts";
/// The app the person's account with their host is published as: `use tilo.host-account as
/// billing`. The session publishes it as `billing.account`, with the plan, the payment method on
/// file, what its machines used, the invoices and the page where they are managed. Only
/// Settings reads it.
pub const HOST_ACCOUNT_APP: &str = "tilo.host-account";
/// The app this computer's crash reports are published as: `use tilo.crash-reports as crashes`.
/// The reports are files the host keeps in the person's private crash directory; a window reads
/// them as `crashes.reports`, each with what happened and when, and one report's text as
/// `resource("crash-report:<id>", of: crashes.CrashReportText)`. Nothing changes a report.
pub const CRASH_REPORTS_APP: &str = "tilo.crash-reports";

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
/// actions, everything one picture was found to show as a single record
/// (`shows`), and the library analysis run (`tilo_vision_service`). It is a platform package,
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
/// The app a workspace's environments are published as: `use tilo.environment`. The workspace's
/// machine — the environments a shell, a Browser or a Linux app runs in — is the system's, not
/// an app's: the device that keeps their records answers `create`, `list` and the record's
/// verbs (`tilo_actions::remote::environment_commands::APP_ACTIONS`) at `environment#...`, and a
/// device that takes the workspace's Linux work answers `run` on the same resource.
pub const ENVIRONMENT_APP: &str = tilo_actions::remote::environment_commands::PROVIDER;
/// The app a part of the shell is drawn as: `use desktop as desktop` reads the window menu's
/// reading (`desktop:window-menu`) and invokes its verbs.
pub const DESKTOP_APP: &str = "desktop";
/// What the session publishes, described as a package (`tilo_session::session_package`, which
/// a test holds to what its api publishes).
const SESSION_DESCRIPTION: &str = include_str!("../../../../apps/tl/session/surface.json");

/// The session's actions as its description declares them, by id: what a call's arguments
/// are sent as ([`tilo_actions::surface::nest_arguments`]).
pub fn session_verbs()
-> Result<std::collections::BTreeMap<String, tilo_actions::surface::VerbSurface>, String> {
    crate::catalogue::loose_verbs(SESSION_DESCRIPTION)
}

/// The session's package: its resources' types and actions, which a shell's views, an agent's
/// task and the REPL call as any package's (`session.place(resource: …, x: 80, y: 60)`).
///
/// It is the platform's copy of the app ([`platform_package`]), the one [`platform_packages`]
/// hands a window's host as well: one app is one package, so a host that loads the description a
/// shell or a keyboard was compiled against and then describes the platform's apps again holds
/// the session once, rather than two descriptions declaring its types and actions twice.
pub fn session_package() -> Result<PackageMeta, String> {
    platform_package(SESSION_APP)
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
    ("battery_history", "BatteryHistory", "power:history"),
    ("power", "Power", "power"),
    ("displays", "Displays", "session:displays"),
    ("screen", "Screen", "session:screen"),
    ("input_devices", "InputDevices", "session:input-devices"),
    ("keyboard", "Keyboard", "session:keyboard"),
    ("privacy", "Privacy", "session:privacy"),
    ("notifications", "Notifications", "session:notifications"),
    ("osd", "Osd", "session:osd"),
    ("vpn", "Vpn", "vpn"),
    ("night_light", "NightLight", "session:night-light"),
    ("banners", "Banners", "session:banners"),
    ("time", "Time", "session:time"),
    ("input", "Input", "session:input"),
    // The room the shell's bar and dock leave on the display the view draws, measured from what
    // its style laid out, so a widget stands clear of both at any size they are.
    ("room", "Room", "session:room"),
    ("clock", "Clock", "session:clock"),
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
    ("people", "People", "session:people"),
    ("shares", "Shares", "session:window-share"),
    ("invite", "Invite", "session:invite"),
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
    ("windows_apps", "WindowsApps", "windows-apps"),
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

/// Where the workspace's members are read: everyone in it now, which every host reads through
/// its store, in the workspace it is in ([`crate::host::Host::follow_members`]).
pub const WORKSPACE_MEMBERS: &str = "workspace/members";
/// Where the workspace's content is read: its owners' limits on what members post there, and
/// the content a limit refused, which every host serves its runtime from what it decides by
/// ([`crate::content::Host::give_content`]).
pub const WORKSPACE_CONTENT: &str = "workspace/content";
/// Where the workspace's grants are read: what its apps, people and agents were given beyond
/// their own documents, which the session publishes for the workspaces it shows.
pub const WORKSPACE_ACCESS: &str = "workspace/access";
/// The workspace's roots a view names (`use tilo.workspace as workspace`): its members, its
/// content, the record of its agents and its grants, which the session publishes.
const WORKSPACE_ROOTS: &[(&str, &str, &str)] = &[
    ("members", "Members", WORKSPACE_MEMBERS),
    ("content", "Content", WORKSPACE_CONTENT),
    ("agents", "WorkspaceAgent", "workspace:tilo.workspace.agent"),
    ("access", "Access", WORKSPACE_ACCESS),
];

/// The resource the workspace's members are read as, of the type its package reads it as.
#[must_use]
pub fn workspace_members() -> crate::runtime::ResourceRef {
    crate::runtime::ResourceRef {
        doc: WORKSPACE_MEMBERS.to_owned(),
        item: None,
        ty: format!("{WORKSPACE_APP}#Members"),
    }
}

/// The resource the workspace's grants are read as, of the type its package reads it as.
#[must_use]
pub fn workspace_access() -> crate::runtime::ResourceRef {
    crate::runtime::ResourceRef {
        doc: WORKSPACE_ACCESS.to_owned(),
        item: None,
        ty: format!("{WORKSPACE_APP}#Access"),
    }
}

/// The resource the workspace's content is read as, of the type its package reads it as.
#[must_use]
pub fn workspace_content() -> crate::runtime::ResourceRef {
    crate::runtime::ResourceRef {
        doc: WORKSPACE_CONTENT.to_owned(),
        item: None,
        ty: format!("{WORKSPACE_APP}#Content"),
    }
}

/// The MCP server's root, the one resource every grant and every verb of it is on.
const MCP_SERVER_ROOTS: &[(&str, &str, &str)] = &[("server", "Server", "mcp.server")];

/// The model providers' root, the one resource every provider and every verb of them is on.
const MODELS_ROOTS: &[(&str, &str, &str)] = &[("models", "Models", "models")];
/// The accounts' root, the one resource every account and every verb of them is on.
const ACCOUNTS_ROOTS: &[(&str, &str, &str)] = &[("accounts", "Accounts", "accounts")];
/// The host account's root, the one resource its plan, card, usage and invoices are
/// read from.
const HOST_ACCOUNT_ROOTS: &[(&str, &str, &str)] = &[("account", "HostAccount", "host-account")];
/// The reports' root, every report this computer recorded.
const CRASH_REPORT_ROOTS: &[(&str, &str, &str)] = &[("reports", "CrashReports", "crash-reports")];
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
const SECTIONS_SURFACE: &str = include_str!("../../../desktop/settings-sections/published.json");

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

/// Where Settings publishes the person's region and formats (the `locale` section), which a
/// device that never chose them still has to read before a session publishes one.
pub const SETTINGS_LOCALE: &str = "settings/locale";

/// What a device that has chosen no region or formats yet reads at [`SETTINGS_LOCALE`]: every
/// field the section's type defaults them to (`tilo_locale::Locale::default()`), so a view that
/// formats a number, a day or a unit lays out rather than trapping on a root no session has
/// published. A literal, because the section's type lives in `tilo-locale`, which pulls the app
/// toolkit into a wasm build of a page; the host's test holds it to the type.
pub const SETTINGS_LOCALE_DEFAULTS: &str = concat!(
    r#"{"currency":"USD","date_format":"Language","time_format":"Language","#,
    r#""measurement":"Metric","number_style":"DecimalPoint","first_day_of_week":"Monday"}"#,
);

/// What the platform root `published` reads before any session publishes it: the new-install
/// defaults of a root whose type has them ([`SETTINGS_LOCALE_DEFAULTS`]), and nothing for every
/// other root, so a view over one lays out as it does on a computer without that resource.
#[must_use]
pub fn platform_root_defaults(published: &str) -> &'static str {
    if published == SETTINGS_LOCALE {
        SETTINGS_LOCALE_DEFAULTS
    } else {
        "{}"
    }
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
    let file: File = serde_json::from_str(SECTIONS_SURFACE)
        .map_err(|error| format!("the settings sections' surface: {error}"))?;
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
        app: MODELS_APP,
        title: "Model providers",
        kinds: &[],
        roots: MODELS_ROOTS,
        // What a workspace's providers read as, written beside the service that keeps them.
        files: &[include_str!(
            "../../../core/tilo-workspace-service/models-surface.json"
        )],
        described: false,
        listed: &[],
    },
    Published {
        app: ACCOUNTS_APP,
        title: "Connected accounts",
        kinds: &[],
        roots: ACCOUNTS_ROOTS,
        // What a workspace's accounts read as, written beside the service that keeps them.
        files: &[include_str!(
            "../../../core/tilo-workspace-service/accounts-surface.json"
        )],
        described: false,
        listed: &[],
    },
    Published {
        app: HOST_ACCOUNT_APP,
        title: "Hosting account",
        kinds: &[],
        roots: HOST_ACCOUNT_ROOTS,
        // What the person's account with their host reads as, written beside the session
        // that answers it.
        files: &[include_str!(
            "../../../desktop/session/host-account-surface.json"
        )],
        described: false,
        listed: &[],
    },
    Published {
        app: CRASH_REPORTS_APP,
        title: "Crash reports",
        kinds: &[],
        roots: CRASH_REPORT_ROOTS,
        // What this computer's reports read as, written beside the service that keeps them.
        files: &[include_str!(
            "../../../services/crash-reports/crash-reports-surface.json"
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
            "../../../core/tilo-contact-directory/surface.json"
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
        roots: WORKSPACE_ROOTS,
        // What a workspace's own actions are, written beside the service that publishes them.
        files: &[include_str!(
            "../../../core/tilo-workspace-service/workspace-surface.json"
        )],
        described: false,
        listed: &[],
    },
    Published {
        app: SESSION_APP,
        title: "Session",
        kinds: &[],
        // The on-screen keyboard as it stands over the focused field: its app reads it as
        // `session.keyboard` (whether it shows, where, and what the field asks of typing) and
        // types into the field through its actions.
        roots: &[("keyboard", "Keyboard", "keyboard")],
        files: &[include_str!("../../../../apps/tl/session/surface.json")],
        described: true,
        listed: &[],
    },
    Published {
        app: ENVIRONMENT_APP,
        title: "Environments",
        kinds: &[],
        roots: &[],
        files: &[include_str!("../../../vm/environment/surface.json")],
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
            "../../../desktop/session/window-menu-surface.json"
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

/// Whether `app` is one of the platform's packages, read without copying them.
///
/// # Errors
/// A platform surface does not read.
pub fn is_platform_app(app: &str) -> Result<bool, String> {
    packages().iter().try_fold(false, |held, package| {
        Ok(held || package.as_ref().map_err(Clone::clone)?.app == app)
    })
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
    let parsed = tilo_failure::ignored!(
        "a source that does not parse names no package here; its compile, which runs next, says why",
        tilolang::parser::parse_module(source).0
    );
    let named: Vec<String> = parsed
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

/// [`named_platform_packages`] of a package of several files: the platform's packages any of
/// its files names with `use`, each once.
///
/// # Errors
/// A named surface does not read.
pub fn named_platform_packages_of(
    source: &tilolang::PackageSource,
) -> Result<Vec<PackageMeta>, String> {
    let mut named: Vec<String> = Vec::new();
    for file in &source.files {
        for app in tilolang::parser::parse_module(&file.text)
            .0
            .into_iter()
            .flat_map(|module| module.items)
            .filter_map(|item| match item {
                tilolang::ast::Item::Use(used) => Some(used.app),
                _ => None,
            })
        {
            if !named.contains(&app) {
                named.push(app);
            }
        }
    }
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
            device: false,
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
            device: false,
        });
    }
    for action in &mut meta.actions {
        action.id = crate::catalogue::action_id(&meta.app, &action.id);
    }
    Ok(())
}

/// The resources a published description declares, as the compiled form every receiver reads.
fn surfaces_of(file: &str) -> Result<Vec<Surface>, String> {
    parse_surfaces(file)
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
    /// The app whose window this is, when it declares preferences: the only app's preferences it
    /// reads and changes.
    own_app: Option<String>,
    /// Where this app's preferences are published (`settings/apps/<app>`): the section of the
    /// settings service it reads and sets.
    prefs: Option<String>,
}

/// Where an app's preferences are published by the settings service: `settings/apps/<app>`.
#[must_use]
pub fn prefs_address(app: &str) -> String {
    format!("settings/apps/{app}")
}

/// The system's settings section that keeps `app`'s settings at [`prefs_address`], when one
/// does: `settings.osk-sources` for the on-screen keyboard. Its resource is that record, so the
/// app declares no preferences of its own there.
#[must_use]
pub fn section_keeping(app: &str) -> Option<String> {
    let at = prefs_address(app);
    settings_roots()
        .ok()?
        .into_iter()
        .find_map(|(_, kind, published)| (published == at).then_some(kind))
}

/// Why the resource `id` is another app's preferences, which this app neither reads nor changes:
/// its own live at `settings/apps/{app}`. `None` for its own, and for anything else.
#[must_use]
pub fn refuse_foreign_prefs(id: &str, app: &str) -> Option<String> {
    let other = id.split('#').next().unwrap_or(id);
    let theirs = other.strip_prefix("settings/apps/")?;
    (theirs != app).then(|| {
        format!(
            "`{id}` is the preferences of {theirs}: an app reads and changes only its own, at {}",
            prefs_address(app)
        )
    })
}

/// The settings-service call a control's edit of the preference `field` to `value` is: the
/// field's setter (`set-<field>`), with the whole new value. One app's preferences are set field
/// by field, so a bound control's change is the same change Settings makes.
#[must_use]
pub fn prefs_setter(
    field: &str,
    value: &crate::runtime::Value,
) -> (String, serde_json::Map<String, serde_json::Value>) {
    let verb = format!("set-{}", field.replace('_', "-"));
    let value = crate::runtime::value::to_argument(value).unwrap_or_default();
    (
        verb,
        serde_json::Map::from_iter([("value".to_owned(), value)]),
    )
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

    /// Follow the app's own preferences, which the settings service keeps
    /// (`settings/apps/<app>`): each reads as its declared defaults until the service answers,
    /// and every view that reads one runs again when it does. An app that declares none reads
    /// nothing here.
    pub fn load_prefs(
        &mut self,
        host: &crate::host::Host,
        meta: &tilolang::meta::PackageMeta,
        wake: &tilo_rt::wake::WakeHandle,
    ) {
        if meta.prefs.is_empty() {
            return;
        }
        let address = prefs_address(&meta.app);
        self.own_app = Some(meta.app.clone());
        self.prefs = Some(address.clone());
        let runtime = host.runtime();
        for ty in &meta.prefs {
            let target = crate::runtime::ResourceRef::root(address.clone(), ty.clone());
            // What the app declared is what every device shows before a person sets it: the
            // settings service's reading replaces it as it arrives.
            let defaults = meta
                .types
                .iter()
                .find(|declared| declared.id == *ty)
                .map(|declared| {
                    let mut held = serde_json::Map::new();
                    for field in &declared.fields {
                        if let Some(default) = &field.default {
                            held.insert(field.name.clone(), default.clone());
                        }
                    }
                    serde_json::Value::Object(held)
                });
            if let Some(defaults) = defaults {
                let _ = runtime.set_reading(&target, &defaults.to_string());
            }
            self.declared.insert(target.id_string());
            self.follow(&target, wake.clone());
        }
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

    /// Start `target` reading what it would before its app publishes anything
    /// ([`platform_root_defaults`]), so a view that reads a field of it runs as the app opens,
    /// rather than trapping on a resource with no reading at all. The session's first reading
    /// replaces it. A root the host reads in its own workspace (its members) reads as it is
    /// there.
    fn seed(host: &crate::host::Host, target: &crate::runtime::ResourceRef) {
        if !host.reads_in_workspace(target) {
            let _ = host
                .runtime()
                .set_reading(target, platform_root_defaults(&target.id_string()));
        }
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
        if let Some(app) = self.own_app.as_deref()
            && let Some(why) = refuse_foreign_prefs(resource, app)
        {
            return Err(why);
        }
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
        // An app reads only its own preferences: a view that names another app's is told so,
        // rather than left waiting for a reading no service will hand it.
        if let Some(app) = self.own_app.clone() {
            let refused: Vec<(String, String)> =
                runtime.take_wanted_if(|id, _| refuse_foreign_prefs(id, &app).is_some());
            for (id, ty) in refused {
                let Some(target) = crate::runtime::ResourceRef::parse_id(&id, ty) else {
                    continue;
                };
                let Some(why) = refuse_foreign_prefs(&id, &app) else {
                    continue;
                };
                let unreadable =
                    serde_json::json!({ tilolang_runtime::store::UNREADABLE: why }).to_string();
                moved |= runtime
                    .set_reading(&target, &unreadable)
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, resource = %id, "a refused preference read was not said");
                        false
                    });
            }
        }
        for (target, reading) in &self.followed {
            while let Some(result) = reading.latest() {
                match result {
                    Ok(_) if host.reads_in_workspace(target) => {}
                    Ok(reading) => {
                        moved |= runtime.set_reading(target, &reading).unwrap_or_else(|error| {
                            tracing::warn!(%error, resource = %target.id_string(), "a platform reading was refused");
                            false
                        });
                    }
                    Err(why) => {
                        // The device or the service did not answer: the resource could not be
                        // read. A view reads that as it is (`unreadable(r)`) and says so, rather
                        // than reading every field as the empty value its type starts with, which
                        // reads as a device with no battery and no sound. A reading that answers
                        // replaces this one.
                        let unreadable = serde_json::Value::Object(serde_json::Map::from_iter([(
                            tilolang_runtime::store::UNREADABLE.to_owned(),
                            serde_json::Value::String(why.clone()),
                        )]))
                        .to_string();
                        moved |= runtime.set_reading(target, &unreadable).unwrap_or_else(|error| {
                            tracing::warn!(%error, resource = %target.id_string(), "a platform reading was refused");
                            false
                        });
                        tracing::debug!(resource = %target.id_string(), %why, "a platform reading could not be read");
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

    /// A page with no session behind it seeds the platform's roots from
    /// [`platform_root_defaults`]: nothing for every root that declares no defaults, and the
    /// region and formats section's own new-install defaults, which this holds to the type they
    /// stand for, so the literal a wasm build carries never drifts from the section.
    #[test]
    fn a_new_install_reads_the_settings_defaults_and_nothing_else() {
        let locale: tilo_locale::Locale = serde_json::from_str(SETTINGS_LOCALE_DEFAULTS)
            .expect("the defaults read as the section's type");
        assert_eq!(
            serde_json::to_value(locale).expect("the section writes as JSON"),
            serde_json::to_value(tilo_locale::Locale::default()).expect("its defaults write"),
            "the literal is the section's default, field for field"
        );
        assert_eq!(
            platform_root_defaults(SETTINGS_LOCALE),
            SETTINGS_LOCALE_DEFAULTS
        );
        for (published, _) in platform_roots() {
            if published == SETTINGS_LOCALE {
                continue;
            }
            assert_eq!(
                platform_root_defaults(&published),
                "{}",
                "`{published}` declares no defaults: a device without it reads nothing"
            );
        }
    }

    /// The environment package's surface names exactly the verbs the device answers for a record
    /// (`environment_commands::APP_ACTIONS`, which `ush`'s supervisor applies), beside the two the
    /// workspace's directory answers: `create` and `list`. A verb added to one side alone fails
    /// here, so the package a view compiles against and the device that answers it never
    /// disagree about what a caller may ask.
    #[test]
    fn the_environment_surface_names_the_verbs_the_device_answers() {
        #[derive(Deserialize)]
        struct File {
            resources: Vec<Resource>,
        }
        #[derive(Deserialize)]
        struct Resource {
            verbs: Vec<Verb>,
        }
        #[derive(Deserialize)]
        struct Verb {
            id: String,
        }
        let file: File = serde_json::from_str(include_str!("../../../vm/environment/surface.json"))
            .expect("the environment surface reads");
        assert_eq!(file.resources.len(), 1, "one resource is the environments");
        let mut published: Vec<&str> = file.resources[0]
            .verbs
            .iter()
            .map(|verb| verb.id.as_str())
            .collect();
        published.sort_unstable();
        let mut asked: Vec<&str> = tilo_actions::remote::environment_commands::APP_ACTIONS.to_vec();
        asked.extend(["create", "list"]);
        asked.sort_unstable();
        assert_eq!(published, asked);
    }

    /// Every app the platform publishes reads as the package a program compiles against, its
    /// roots the resources the app names: an app whose committed surface stops carrying a root's
    /// kind fails here, with the error every host that reads the platform is given, before a
    /// daemon's first workspace does.
    #[test]
    fn every_published_root_is_a_type_its_surface_publishes() {
        for published in PUBLISHED {
            if let Err(why) = package(published) {
                panic!("{}: {why}", published.app);
            }
        }
    }

    /// Every type a published surface declares is the type the package declares under that
    /// name: a type another reading's type or kind already takes is renamed after its kind
    /// (`<Kind><Name>`), and `device.<Name>` would silently read another record. A new clash
    /// fails here, naming it.
    #[test]
    fn every_device_type_keeps_the_name_its_surface_gives() {
        // The clashes the device already has: the system monitor's own battery and network
        // readings, beside the device's battery and connectivity readings.
        const RENAMED: &[(&str, &str)] = &[("processes", "Battery"), ("processes", "Network")];
        let published = PUBLISHED
            .iter()
            .find(|published| published.app == DEVICE_APP)
            .expect("the device is one of the platform's apps");
        let mut surfaces = Vec::new();
        for file in published.files {
            surfaces.extend(
                surfaces_of(file)
                    .unwrap_or_else(|why| panic!("the device's surface does not read: {why}")),
            );
        }
        let meta = package_of(published.app, published.title, &surfaces);
        // The names a type's fields or cases carry, in the order and spelling the package
        // declares them: nested fields are the type's own and no field of the record.
        let names = |names: &mut dyn Iterator<Item = &str>| -> Vec<String> {
            // A row's `$address` is where its actions are called, not a field code reads;
            // a nested field belongs to the type it names, not to the record.
            let mut names: Vec<String> = names
                .map(|name| name.replace('-', "_"))
                .filter(|name| !name.contains('.') && !name.starts_with('$'))
                .collect();
            names.sort();
            names
        };
        for surface in &surfaces {
            for ty in &surface.types {
                if RENAMED.contains(&(surface.kind.as_str(), ty.name.as_str())) {
                    continue;
                }
                // A resource kind's own type may repeat a named type's name with the same
                // fields (`saved-network` and the network reading's `SavedNetwork`); any type of
                // the name with the fields the surface declares is the one code reads.
                let kept = if ty.is_enum() {
                    let want = names(&mut ty.cases.iter().map(|case| case.name.as_str()));
                    meta.enums.iter().any(|declared| {
                        declared.name == ty.name
                            && names(&mut declared.variants.iter().map(|case| case.name.as_str()))
                                == want
                    })
                } else {
                    let want = names(&mut ty.fields.iter().map(|field| field.name.as_str()));
                    meta.types.iter().any(|declared| {
                        declared.name == ty.name
                            && names(&mut declared.fields.iter().map(|field| field.name.as_str()))
                                == want
                    })
                };
                assert!(
                    kept,
                    "the `{}` reading's `{}` is not the type the device declares as `{}`: another \
                     reading's type of that name takes it, and the catalogue renames one of them \
                     after its kind, so `device.{}` stops reading this one. Give one of them \
                     another name",
                    surface.kind, ty.name, ty.name, ty.name
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
        let used = platform_packages_of(["tilo.actions", "tilo.invocations"])
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
