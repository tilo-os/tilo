//! The `windows-apps` resource: this computer's Windows compatibility layer as one manager reads
//! it — the programs set up in private capsules, the applications the managed Windows appliance
//! publishes, the appliance itself, the setup waiting to be told where to go — with the actions
//! that launch, stop, install, repair and remove them.
//!
//! The layer's own installation (whether Wine and Proton are here at all) is the
//! [`super::windows_compat`] resource. What reads and changes the programs is the session's
//! system service; this crate holds only the contract, so an app that shows or drives them
//! depends on it and not on the session.

use serde::{Deserialize, Serialize};

use super::windows_compat::WindowsCompat;

/// The resource's id.
pub const WINDOWS_APPS: &str = "windows-apps";

/// The id prefix of one program of the reading: a program of its own, whose actions are called
/// on the address its row carries (`windows-program:<id>`).
pub const WINDOWS_PROGRAM: &str = "windows-program";

/// The address of the row of program `id`, when `id` can be one: a program's id goes on from
/// its row's after a `:`, and holds no resource key's separator.
#[must_use]
pub fn program_id(id: &str) -> Option<String> {
    (!id.is_empty() && !id.contains('\u{1f}')).then(|| format!("{WINDOWS_PROGRAM}:{id}"))
}

/// One program of the reading, as a resource of its own: search lists it by its name, and a
/// caller opens or stops a running one on its address.
#[derive(tilo_resource::Row)]
#[row(
    kind = "so.tilo.system.windows-program",
    label = "Windows program",
    effects = WindowsProgramAction
)]
pub struct WindowsProgram;

/// A program's own actions, called on the program's address.
#[derive(Clone, Copy, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.windows-program")]
pub enum WindowsProgramAction {
    /// Open the program.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        row,
        icon = "play",
        keywords = "launch windows app, windows-app starten",
        on = WindowsProgram
    )]
    Launch,
    /// End a running program of a private capsule.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        row,
        destructive,
        icon = "square",
        keywords = "stop windows app, windows-app beenden",
        on = WindowsProgram
    )]
    Stop,
}

/// How a program opens.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Route {
    #[default]
    Wine,
    Proton,
    /// The managed Windows appliance.
    Vm,
}

/// How well this computer's catalogue knows that a program runs.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    /// A recipe verified against the exact installer or program.
    Verified,
    /// A community recipe for the program or its store.
    Community,
    /// A best match on the executable or its publisher.
    Heuristic,
    /// Nothing matched: it opens with the default settings.
    #[default]
    Fallback,
}

/// Where a program's installation stands.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum AppStatus {
    /// Installed and ready to open.
    #[default]
    Ready,
    /// Open now.
    Running,
    /// Its setup stopped before it was ready.
    NeedsAttention,
    Installing,
    Uninstalling,
    Failed,
    Uninstalled,
}

/// What a program may reach, as the compatibility layer's plan for it allows.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Access {
    /// How many places it may read.
    pub files_read_only: usize,
    /// How many places it may change.
    pub files_read_write: usize,
    /// Its network: `none`, `unrestricted`, or the sites allowed in the layer's words.
    pub network: String,
    pub microphone: bool,
    /// Whether its sound plays.
    pub sound: bool,
}

/// Where this computer learned that a program runs.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// A recipe in Tilo's compatibility catalogue.
    #[default]
    Catalogue,
    /// Nothing matched: the layer's default settings.
    Default,
    /// An application the managed Windows appliance published.
    InstalledInWindows,
    /// A setup kept from a file a person chose.
    AddedFromFile,
    /// An application Windows setup found and the person has not added yet.
    FoundBySetup,
}

/// Whether the person may do something with a Windows program, or how one of its ways to open
/// stands: one plain boolean on the wire, and a named type here.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(transparent)]
pub struct Allowed(pub bool);

impl Allowed {
    #[must_use]
    pub const fn yes(self) -> bool {
        self.0
    }
}

impl From<bool> for Allowed {
    fn from(allowed: bool) -> Self {
        Self(allowed)
    }
}

/// One Windows program a person can open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct WindowsApp {
    /// The entry's id, as the compatibility layer names it.
    pub id: String,
    pub name: String,
    pub publisher: String,
    /// How it opens now.
    pub route: Route,
    /// How it opens unless someone chooses otherwise.
    pub recommended_route: Route,
    /// The runner's name in the person's words: `Wine 11.0`, `Proton 10.0`, `Windows 11`.
    pub runner: String,
    /// The private capsule it shares with the other programs of its setup, or
    /// `Shared Windows appliance` for a program of the appliance.
    pub capsule: String,
    /// What the setup that installed it is called, as its installer named it.
    #[serde(default)]
    pub setup_name: Option<String>,
    /// Where this computer learned that it runs.
    pub source: Source,
    pub confidence: Confidence,
    /// Why it opens as it does, in the compatibility layer's words.
    pub reason: String,
    pub status: AppStatus,
    pub access: Access,
    /// How its route keeps it apart from the rest of the computer.
    pub fallback_detail: String,
    pub launch_available: Allowed,
    #[serde(default)]
    pub launch_unavailable_reason: Option<String>,
    /// Whether a document this computer chose opens in it.
    pub portal_file_open_available: Allowed,
    #[serde(default)]
    pub portal_file_open_unavailable_reason: Option<String>,
    pub route_override_available: Allowed,
    #[serde(default)]
    pub route_override_unavailable_reason: Option<String>,
    pub can_retry_install: Allowed,
    pub can_escalate_install_to_vm: Allowed,
    pub can_remove_launcher: Allowed,
    pub can_vendor_uninstall: Allowed,
    pub can_delete_data: Allowed,
    pub can_publish_vm_candidate: Allowed,
    /// How its installation went, in the service's words.
    #[serde(default)]
    pub installation: Option<String>,
    pub vm_fallback_available: Allowed,
    #[serde(default)]
    pub vm_fallback_unavailable_reason: Option<String>,
}

/// How the managed Windows appliance stands.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum AppliancePhase {
    NotSetUp,
    Installing,
    FinishingSetup,
    #[default]
    Ready,
    NeedsRecovery,
    Unavailable,
}

/// What Windows says of its licence.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum License {
    #[default]
    Unknown,
    Digital,
    ProductKey,
    Managed,
    Evaluation,
    Unlicensed,
}

/// What the appliance is doing now.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum PowerState {
    #[default]
    Unknown,
    Running,
    Suspended,
    Paused,
    ShutDown,
}

/// A temporary connection the appliance was given, for activation or Windows Update.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct TemporaryConnection {
    /// `activation` or `windows-update`.
    pub purpose: String,
    /// How long it has left, as the reading was made.
    pub expires_in_seconds: u64,
}

/// The managed Windows appliance: one Windows, set up for this computer's apps.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Appliance {
    /// Whether it was set up at all.
    pub configured: bool,
    pub phase: AppliancePhase,
    pub license: License,
    pub power_state: PowerState,
    /// How many apps it publishes to this computer.
    pub published_applications: usize,
    /// Whether its recovery desktop can be opened.
    pub recovery_available: bool,
    #[serde(default)]
    pub temporary_connection: Option<TemporaryConnection>,
    /// What the appliance's interactive Windows session reaches for the programs open in it.
    pub shared_session: SharedSession,
    /// Why it stands as it does, when it says.
    #[serde(default)]
    pub detail: Option<String>,
}

/// What the appliance's one interactive Windows session reaches: the grants its programs use,
/// and who has programs open in it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct SharedSession {
    pub active: bool,
    /// The grants in force: `audio-playback`, `clipboard`, `microphone`, `network-egress`,
    /// `portal-files-read-only`, `portal-files-read-write`.
    pub grants: Vec<String>,
    /// The people whose programs are open in it, by the name each is shown by.
    pub contributors: Vec<String>,
}

/// A program that ended, as its manager hears it once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct EndedProgram {
    /// The entry that ended.
    pub program: String,
    pub name: String,
    /// Whether it ended cleanly.
    pub success: bool,
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// Why it never opened, or what it said as it ended.
    #[serde(default)]
    pub error: Option<String>,
    /// What it printed as it stopped, when it printed anything.
    #[serde(default)]
    pub said: Option<String>,
}

/// A setup that was inspected and waits for the person to choose where it goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct PendingInstall {
    pub display_name: String,
    pub detail: String,
    pub recommended_route: Route,
}

/// This computer's Windows programs as one manager reads them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct WindowsApps {
    /// Whether this computer's compatibility layer answers at all.
    pub serving: bool,
    /// What it is doing, in the person's words.
    pub summary: String,
    pub apps: Vec<WindowsApp>,
    pub appliance: Appliance,
    #[serde(default)]
    pub pending_install: Option<PendingInstall>,
    /// Whether the compatibility layer (Wine, Proton) is installed, and what installing it takes.
    pub support: WindowsCompat,
    /// The last program that ended, said for a while after it did.
    #[serde(default)]
    pub ended: Option<EndedProgram>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last action did not happen, until the next.
    #[serde(default)]
    pub problem: Option<String>,
}

/// The `windows-apps` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.windows-apps")]
pub enum WindowsAppsAction {
    /// Install an `.exe` or `.msi` in a private capsule, or offer to set it up in Windows.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "plus",
        keywords = "install windows app, windows programm installieren"
    )]
    Install {
        #[param("Absolute path of the installer; omit to choose one")]
        path: Option<String>,
    },
    /// Import a Windows ISO and set up the managed appliance from it.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "disc",
        keywords = "import windows iso, windows iso importieren"
    )]
    ImportIso {
        #[param("Absolute path of the ISO; omit to choose one")]
        path: Option<String>,
    },
    /// Open the appliance's recovery desktop.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "wrench",
        keywords = "windows recovery, windows wiederherstellung"
    )]
    Recovery,
    /// Start Windows.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "play", keywords = "start windows, windows starten")]
    Start,
    /// Restart Windows.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "restart windows, windows neu starten"
    )]
    Restart,
    /// Suspend Windows.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "pause",
        keywords = "suspend windows, windows anhalten"
    )]
    Suspend,
    /// Resume Windows.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "play",
        keywords = "resume windows, windows fortsetzen"
    )]
    Resume,
    /// Shut Windows down.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "power",
        keywords = "shut down windows, windows herunterfahren"
    )]
    Shutdown,
    /// Connect the appliance long enough to activate Windows.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "network",
        keywords = "activate windows, windows aktivieren"
    )]
    TemporaryNetwork,
    /// Connect the appliance long enough for Windows Update.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "download", keywords = "windows update")]
    UpdateNetwork,
    /// End the appliance's temporary connection.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "x",
        keywords = "disconnect windows, verbindung trennen"
    )]
    RevokeNetwork,
    /// Set the waiting setup up in a private capsule.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "shield",
        keywords = "private capsule, private kapsel"
    )]
    InstallPrivateCapsule,
    /// Set the waiting setup up in the Windows appliance.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "monitor",
        keywords = "install in windows, in windows installieren"
    )]
    InstallWindowsAppliance,
    /// Open a program.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "play",
        keywords = "launch windows app, windows-app starten"
    )]
    Launch {
        #[param("The application's id")]
        program: String,
    },
    /// End a running program of a private capsule.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "square",
        keywords = "stop windows app, windows-app beenden"
    )]
    Stop {
        #[param("The application's id")]
        program: String,
    },
    /// Open a private-capsule program in the Windows appliance instead.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "monitor",
        keywords = "open in windows vm, in windows öffnen"
    )]
    LaunchVmFallback {
        #[param("The application's id")]
        program: String,
    },
    /// Open one document this computer chose in an appliance application.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "file",
        keywords = "open file in windows, datei in windows öffnen"
    )]
    OpenFile {
        #[param("The application's id")]
        program: String,
        #[param("Absolute path of the file; omit to choose one")]
        path: Option<String>,
    },
    /// Add an application the appliance published to this computer's apps.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "plus", keywords = "publish windows app")]
    Publish {
        #[param("The application's id")]
        program: String,
    },
    /// Run the setup whose private capsule this is again.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "retry setup, installation wiederholen"
    )]
    RetryInstall {
        #[param("The private capsule the setup installed into")]
        capsule: String,
    },
    /// Continue a setup that did not finish in the Windows appliance.
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "monitor", keywords = "continue setup in windows")]
    EscalateInstall {
        #[param("The private capsule the setup installed into")]
        capsule: String,
    },
    /// Remove a program's launcher; its files stay.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "remove launcher, verknüpfung entfernen"
    )]
    RemoveLauncher {
        #[param("The application's id")]
        program: String,
    },
    /// Run the program's own uninstaller.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "uninstall windows app, deinstallieren"
    )]
    Uninstall {
        #[param("The application's id")]
        program: String,
    },
    /// Delete everything the setup installed into its private capsule.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "delete windows app data, daten löschen"
    )]
    DeleteData {
        #[param("The private capsule the setup installed into")]
        capsule: String,
    },
    /// Choose how an application opens from now on, or let the catalogue recommend it.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        idempotent,
        icon = "route",
        keywords = "open with wine, proton, windows vm"
    )]
    SetRoute {
        #[param("The application's id")]
        program: String,
        #[param("How it opens; omit for the catalogue's recommendation")]
        route: Option<Route>,
    },
}
