//! The devices of this machine, as the person's privacy stands with them: the camera, the
//! microphone, the screen and the location — whether the machine has each, whether a switch
//! blocks it, what uses it now and who, and every answer the person gave about the devices of
//! theirs.
//!
//! The session is where these are known, so the reading is the session's (`session:privacy`):
//! the switches are the machine's (`os_actions`), the uses are the capture streams `PipeWire`
//! attributes to the program behind them and the captures this desktop's portal brokers, and the
//! answers are the person's own, kept in their workspace and following them to their other
//! devices. A use is named by the program it is, never by a name the program gave itself.
//!
//! A device the platform asks the person about as each use — the location — is one this session
//! does not see in use: its posture says so instead of counting no use.

use std::cell::Cell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, ApiError, ResourceSpec};
use tilo_device_surfaces::privacy::{
    Answer, Capability, InUse, Posture, PrivacyAction, Reading, SavedSecrets, SecretApp, Security,
    Standing, SwitchState,
};
#[cfg(target_os = "linux")]
use tilo_device_surfaces::privacy::{FirewallInForce, Protection};
use tilo_lock::MutexRecover as _;
use tilo_service::{Service, Started, decode, offer};

use crate::i18n::t;

/// The privacy reading in the session's catalogue.
pub(crate) const RESOURCE: &str = "session:privacy";

/// The privacy of this machine's devices, as the session publishes it: its declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "session.privacy", label = "Privacy")]
pub struct PrivacyRoot;

/// How often the machine is asked again what uses its devices: the capture streams and the
/// stems are read off the machine, so the reading follows them at a pace of its own rather than
/// on every pass of the loop.
const REFRESH: Duration = Duration::from_secs(2);

/// How long a reading stays worth refreshing after something last read it: nothing follows the
/// privacy of a session whose Settings is closed, and the machine is not asked for it.
const KEEP_WARM: Duration = Duration::from_secs(30);

/// How often the secret store and the device's own protection are read: listing the store starts
/// a process and reading the disk starts two, so they are asked at a pace of their own rather
/// than on every pass, and again at once when the person changes one.
const DEVICE_FACTS: Duration = Duration::from_secs(30);

/// The phone book of the store's own console: the same command line Settings and scripts run.
#[cfg(target_os = "linux")]
const SECRET_SERVICE: &str = "/usr/libexec/tilo/tilo-secret-service";

/// How long one read of the secret store or the machine may take.
#[cfg(target_os = "linux")]
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// The most one of those commands may answer.
#[cfg(target_os = "linux")]
const COMMAND_OUTPUT_LIMIT: usize = 1024 * 1024;

/// Where the session reads the privacy of this machine's devices: the machine itself in a
/// session, a stand-in in a test.
pub(crate) trait Sources {
    /// What the machine says of its switches, its camera and microphone, and what captures from
    /// them through `PipeWire`.
    fn status(&self) -> os_actions::PrivacyStatus;

    /// The captures this desktop's portal brokers now, each the app it was granted to.
    fn captures(&self) -> Vec<Capture>;

    /// The answers the person kept about the devices of theirs, on any of their devices.
    fn answers(&self) -> Vec<tilo_peer_state::Kept>;

    /// What people call `app`, from the device's own lists.
    fn app_name(&self, app: &str) -> String;

    /// The applications that use this device's saved passwords and keys.
    fn secret_apps(&self) -> SavedSecrets {
        SavedSecrets::default()
    }

    /// What protects this device itself: its disk, its firmware and its firewall.
    fn security(&self) -> Security {
        Security::default()
    }

    /// What the person changed of the two above: they are read again at the next reading.
    fn changed(&self) {}
}

/// One capture the session brokers now, as the reading says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Capture {
    /// The application it was granted to.
    pub app: String,
    /// The device it captures.
    pub capability: Capability,
}

/// This machine, as the person's session knows it.
struct ThisMachine {
    captures: Option<tilo_portal::captures::Captures>,
    consents: Option<tilo_portal::consent::Consents>,
    /// What the secret store held when it was last asked, and when.
    secrets: Mutex<Option<(Instant, SavedSecrets)>>,
    /// What protected this device when it was last asked, and when.
    security: Mutex<Option<(Instant, Security)>>,
}

impl Sources for ThisMachine {
    fn status(&self) -> os_actions::PrivacyStatus {
        os_actions::get_privacy_status()
    }

    fn captures(&self) -> Vec<Capture> {
        self.captures
            .as_ref()
            .map(|captures| {
                captures
                    .broker()
                    .lock_recover()
                    .live()
                    .into_iter()
                    .map(|live| Capture {
                        app: live.app_id,
                        capability: capability_of_capture(live.kind),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn answers(&self) -> Vec<tilo_peer_state::Kept> {
        self.consents
            .as_ref()
            .map(tilo_portal::consent::Consents::standing)
            .unwrap_or_default()
    }

    fn app_name(&self, app: &str) -> String {
        crate::app_names::app_name(app)
    }

    fn secret_apps(&self) -> SavedSecrets {
        let mut held = self.secrets.lock_recover();
        if let Some((at, secrets)) = held.as_ref()
            && at.elapsed() < DEVICE_FACTS
        {
            return secrets.clone();
        }
        let secrets = read_secret_apps();
        *held = Some((Instant::now(), secrets.clone()));
        secrets
    }

    fn security(&self) -> Security {
        let mut held = self.security.lock_recover();
        if let Some((at, security)) = held.as_ref()
            && at.elapsed() < DEVICE_FACTS
        {
            return security.clone();
        }
        let security = read_security();
        *held = Some((Instant::now(), security.clone()));
        security
    }

    fn changed(&self) {
        *self.secrets.lock_recover() = None;
    }
}

/// The privacy of this machine's devices, published as `session:privacy`.
pub(crate) struct Privacy {
    sources: Box<dyn Sources>,
    /// The reading last published, and when it was read: the machine is asked again only after
    /// [`REFRESH`].
    last: Mutex<Option<(Instant, Reading)>>,
    /// When something last read the resource: nothing follows it while nothing reads it.
    asked: Cell<Option<Instant>>,
}

impl Privacy {
    /// The privacy of `portal`'s captures and answers, and of the machine's own switches: the
    /// session's reading.
    pub(crate) fn new(
        captures: Option<tilo_portal::captures::Captures>,
        consents: Option<tilo_portal::consent::Consents>,
    ) -> Self {
        Self::over(Box::new(ThisMachine {
            captures,
            consents,
            secrets: Mutex::default(),
            security: Mutex::default(),
        }))
    }

    /// The privacy as `sources` reads it.
    pub(crate) fn over(sources: Box<dyn Sources>) -> Self {
        Self {
            sources,
            last: Mutex::new(None),
            asked: Cell::new(None),
        }
    }

    /// The reading as it stands now, off `sources`.
    fn read_now(&self) -> Reading {
        reading_of(self.sources.as_ref())
    }

    /// The reading last read, read now when it is older than [`REFRESH`].
    fn current(&self) -> Reading {
        let now = Instant::now();
        let mut last = self.last.lock_recover();
        if let Some((at, reading)) = last.as_ref()
            && now.duration_since(*at) < REFRESH
        {
            return reading.clone();
        }
        let reading = self.read_now();
        *last = Some((now, reading.clone()));
        reading
    }
}

impl Service for Privacy {
    fn poll(&mut self) -> bool {
        let now = Instant::now();
        // Nothing has read the privacy lately: the machine is not asked for what uses it.
        let Some(asked) = self.asked.get() else {
            return false;
        };
        if now.duration_since(asked) > KEEP_WARM {
            return false;
        }
        // A reading younger than the refresh stands: asking the machine takes the session's turn
        // for as long as `pw-dump` runs, and the loop polls on every pass, a key's included.
        if self
            .last
            .lock_recover()
            .as_ref()
            .is_some_and(|(at, _)| now.duration_since(*at) < REFRESH)
        {
            return false;
        }
        let reading = self.read_now();
        let mut last = self.last.lock_recover();
        let changed = last.as_ref().is_none_or(|(_, was)| &reading != was);
        *last = Some((now, reading));
        changed
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<PrivacyAction>(
            tilo_resource::LiveRow::typed::<PrivacyRoot>(RESOURCE, t("session-privacy"))
                .into_resource(),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if resource != RESOURCE {
            return None;
        }
        self.asked.set(Some(Instant::now()));
        serde_json::to_value(self.current()).ok()
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        match decode::<PrivacyAction>(&self.resources(), resource, action, arguments)? {
            PrivacyAction::SetSecretsAccess { app, allowed } => {
                let verb = if allowed { "allow" } else { "revoke" };
                secret_command(&["apps", verb, &app]).map_err(ApiError::Failed)?;
            }
            PrivacyAction::ForgetSecrets { app } => {
                secret_command(&["apps", "forget", &app]).map_err(ApiError::Failed)?;
            }
        }
        // What the person changed is read again at once, rather than after the pace of the
        // reading or of the store: the next read lists what they left.
        self.sources.changed();
        *self.last.lock_recover() = None;
        Ok(Started::Done(ActionOutput::default()))
    }
}

/// The answers the person gave, as the reading lists them.
fn answers_of(sources: &dyn Sources) -> Vec<Answer> {
    let mut answers: Vec<Answer> = sources
        .answers()
        .into_iter()
        .filter_map(|kept| {
            let capability = capability_of_device(kept.answer.cap)?;
            let (standing, until_unix_ms) = standing_of(kept.answer.lifetime);
            Some(Answer {
                // The answer as the resource it is: the consent this session's portal publishes
                // for it, whose `forget` takes it back from any of the person's devices.
                address: tilo_resource::Handle::new(tilo_portal::consent_resource(&kept.answer)),
                app: kept.answer.app.clone(),
                name: sources.app_name(&kept.answer.app),
                capability,
                allowed: kept.answer.answer == tilo_action_policy::Answer::Allow,
                standing,
                until_unix_ms,
                device: kept.device_label,
                this_device: kept.this_device,
            })
        })
        .collect();
    answers.sort_by(|a, b| {
        (
            a.device.as_str(),
            a.name.as_str(),
            capability_name(a.capability),
        )
            .cmp(&(
                b.device.as_str(),
                b.name.as_str(),
                capability_name(b.capability),
            ))
    });
    answers
}

/// The reading `sources` gives now: the four devices, and the answers the person gave.
fn reading_of(sources: &dyn Sources) -> Reading {
    let status = sources.status();
    let captures = sources.captures();
    let uses = |cap: Capability| -> Vec<InUse> {
        let mut found: Vec<InUse> = Vec::new();
        let mut add = |app: &str| {
            if app.is_empty() || found.iter().any(|use_| use_.app == app) {
                return;
            }
            found.push(InUse {
                app: app.to_owned(),
                name: sources.app_name(app),
            });
        };
        for capture in &captures {
            if capture.capability == cap {
                add(&capture.app);
            }
        }
        if let Some(kind) = media_kind(cap) {
            for use_ in &status.active_media {
                if use_.kind == kind {
                    // The stream names its program: its app when the session knows one, else the
                    // program it is.
                    add(if use_.app_id.trim().is_empty() {
                        &use_.label
                    } else {
                        &use_.app_id
                    });
                }
            }
        }
        found
    };
    let posture =
        |cap: Capability, present: bool, switch: SwitchState, (muted, mute_known)| Posture {
            capability: cap,
            present,
            switch,
            muted,
            mute_known,
            watched: cap != Capability::Location,
            in_use: uses(cap),
        };
    let microphone_present = status.microphone_muted.is_some()
        || status.microphone_kill != os_actions::KillState::Unavailable;
    let mute = status.microphone_muted;
    let capabilities = vec![
        posture(
            Capability::Camera,
            status.camera_present || status.camera_kill != os_actions::KillState::Unavailable,
            switch_of(status.camera_kill),
            (false, false),
        ),
        posture(
            Capability::Microphone,
            microphone_present,
            switch_of(status.microphone_kill),
            (mute == Some(true), mute.is_some()),
        ),
        // A desktop captures what it shows: the screen is here whenever the session is.
        posture(
            Capability::Screen,
            true,
            SwitchState::Unavailable,
            (false, false),
        ),
        // The platform asks the person about the location each time it is wanted, so this session
        // sees no use of it: it says the machine has it where the build reaches one, and that it
        // cannot say who asks.
        posture(
            Capability::Location,
            !matches!(
                tilo_location::availability(tilo_location::Feature::Locate),
                tilo_location::Availability::Unsupported(_)
            ),
            SwitchState::Unavailable,
            (false, false),
        ),
    ];
    let answers = answers_of(sources);
    Reading {
        capabilities,
        answers,
        secrets: Some(sources.secret_apps()),
        security: Some(sources.security()),
    }
}

/// The applications that use this device's saved passwords and keys, as the store lists them.
#[cfg(target_os = "linux")]
fn read_secret_apps() -> SavedSecrets {
    #[derive(serde::Deserialize)]
    struct Stored {
        id: String,
        #[serde(default)]
        name: String,
        access: String,
        #[serde(default)]
        uses: u64,
        #[serde(default)]
        items: u32,
    }
    let listed = bounded_command::run(
        SECRET_SERVICE,
        &["apps", "list", "--json"],
        None,
        COMMAND_OUTPUT_LIMIT,
        COMMAND_TIMEOUT,
    )
    .map_err(|error| format!("{SECRET_SERVICE}: {error}"))
    .and_then(|output| {
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            let said = output.stderr_diagnostic();
            Err(if said.is_empty() {
                format!("{SECRET_SERVICE}: exited with {}", output.status)
            } else {
                format!("{SECRET_SERVICE}: {said}")
            })
        }
    })
    .and_then(|json| serde_json::from_str::<Vec<Stored>>(&json).map_err(|error| error.to_string()));
    match listed {
        Ok(apps) => SavedSecrets {
            apps: apps
                .into_iter()
                .map(|app| SecretApp {
                    allowed: app.access == "allowed",
                    name: if app.name.is_empty() {
                        crate::app_names::app_name(&app.id)
                    } else {
                        app.name
                    },
                    app: app.id,
                    uses: app.uses,
                    items: app.items,
                })
                .collect(),
            read_error: None,
        },
        Err(why) => SavedSecrets {
            apps: Vec::new(),
            read_error: Some(why),
        },
    }
}

/// A device that keeps no store of saved passwords and keys of its own.
#[cfg(not(target_os = "linux"))]
fn read_secret_apps() -> SavedSecrets {
    SavedSecrets {
        apps: Vec::new(),
        read_error: Some("This device keeps no saved passwords and keys.".to_owned()),
    }
}

/// What protects this device itself: its disk, its firmware and its firewall.
fn read_security() -> Security {
    #[cfg(target_os = "linux")]
    {
        let (firewall, firewall_error) = firewall_in_force();
        Security {
            disk_encryption: disk_encryption(),
            secure_boot: secure_boot(),
            firewall,
            firewall_error,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Security::default()
    }
}

/// Whether the disk this system runs from is encrypted: whether its device is a `crypt` one, or
/// one built on a `crypt` device below it.
#[cfg(target_os = "linux")]
fn disk_encryption() -> Protection {
    let Ok(source) = query("findmnt", &["-no", "SOURCE", "/"]) else {
        return Protection::Unknown;
    };
    let Ok(chain) = query("lsblk", &["-s", "-n", "-o", "TYPE", &source]) else {
        return Protection::Unknown;
    };
    if chain.lines().any(|line| line.trim() == "crypt") {
        Protection::On
    } else {
        Protection::Off
    }
}

/// Whether the firmware verifies what it boots: the `SecureBoot` variable the firmware keeps, and
/// the byte inside it. A machine with no EFI variables has no Secure Boot; one whose variables
/// cannot be read says it cannot tell.
#[cfg(target_os = "linux")]
fn secure_boot() -> Protection {
    let Ok(entries) = std::fs::read_dir("/sys/firmware/efi/efivars") else {
        return Protection::Unknown;
    };
    let Some(path) = entries.flatten().map(|entry| entry.path()).find(|path| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("SecureBoot-"))
    }) else {
        return Protection::Off;
    };
    match std::fs::read(path)
        .ok()
        .and_then(|value| value.get(4).copied())
    {
        Some(0) => Protection::Off,
        Some(_) => Protection::On,
        None => Protection::Unknown,
    }
}

/// The firewall in force: `firewalld` when it serves, else the kernel's own ruleset, which is off
/// only when it was read and holds nothing.
#[cfg(target_os = "linux")]
fn firewall_in_force() -> (FirewallInForce, Option<String>) {
    let firewalld = tilo_system_service::firewall::reading();
    if firewalld.serving {
        return (FirewallInForce::Firewalld, None);
    }
    match query("nft", &["list", "ruleset"]) {
        Ok(ruleset) if !ruleset.trim().is_empty() => (FirewallInForce::Nftables, None),
        Ok(_) => (FirewallInForce::Off, None),
        Err(nft_error) => (
            FirewallInForce::Unknown,
            Some(match firewalld.read_error {
                Some(firewalld_error) => format!("{firewalld_error}; {nft_error}"),
                None => nft_error,
            }),
        ),
    }
}

/// One read-only `program` query, its trimmed standard output.
#[cfg(target_os = "linux")]
fn query(program: &str, args: &[&str]) -> Result<String, String> {
    let output = bounded_command::run(program, args, None, COMMAND_OUTPUT_LIMIT, COMMAND_TIMEOUT)
        .map_err(|error| format!("{program}: {error}"))?;
    if !output.status.success() {
        let said = output.stderr_diagnostic();
        return Err(if said.is_empty() {
            format!("{program}: exited with {}", output.status)
        } else {
            format!("{program}: {said}")
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// One change the secret store's own console makes: letting an application use it again,
/// stopping it, or forgetting it.
#[cfg(target_os = "linux")]
fn secret_command(args: &[&str]) -> Result<(), String> {
    let output = bounded_command::run(
        SECRET_SERVICE,
        args,
        None,
        COMMAND_OUTPUT_LIMIT,
        COMMAND_TIMEOUT,
    )
    .map_err(|error| format!("{SECRET_SERVICE}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = output.stderr_diagnostic();
    Err(if said.is_empty() {
        format!("{SECRET_SERVICE}: exited with {}", output.status)
    } else {
        format!("{SECRET_SERVICE}: {said}")
    })
}

/// The machine's switch for one of its devices, as the reading says it.
fn switch_of(state: os_actions::KillState) -> SwitchState {
    match state {
        os_actions::KillState::Unavailable => SwitchState::Unavailable,
        os_actions::KillState::Allowed => SwitchState::Allowed,
        os_actions::KillState::SoftwareBlocked => SwitchState::SoftwareBlocked,
        os_actions::KillState::HardwareBlocked => SwitchState::HardwareBlocked,
    }
}

/// The capability a portal capture is of.
fn capability_of_capture(kind: tilo_portal::capture_broker::CaptureKind) -> Capability {
    use tilo_portal::capture_broker::CaptureKind;
    match kind {
        CaptureKind::Camera => Capability::Camera,
        CaptureKind::Microphone => Capability::Microphone,
        CaptureKind::Screen => Capability::Screen,
        CaptureKind::SystemAudio => Capability::SystemAudio,
    }
}

/// The `PipeWire` capture kind a capability's use is listed under, when the sound server sees
/// those uses.
fn media_kind(cap: Capability) -> Option<os_actions::MediaKind> {
    match cap {
        Capability::Camera => Some(os_actions::MediaKind::Camera),
        Capability::Microphone => Some(os_actions::MediaKind::Microphone),
        Capability::Screen
        | Capability::Location
        | Capability::Input
        | Capability::SystemAudio
        | Capability::Api => None,
    }
}

/// The capability one of the person's answered devices is, when the pane lists answers about it.
fn capability_of_device(cap: tilo_permissions::caps::DeviceCap) -> Option<Capability> {
    use tilo_permissions::caps::DeviceCap;
    match cap {
        DeviceCap::Camera => Some(Capability::Camera),
        DeviceCap::Microphone => Some(Capability::Microphone),
        DeviceCap::Screen => Some(Capability::Screen),
        DeviceCap::Input => Some(Capability::Input),
        DeviceCap::SystemAudio => Some(Capability::SystemAudio),
        DeviceCap::Api => Some(Capability::Api),
        _ => None,
    }
}

/// How long an answer stands, and the moment it ends at when it is timed.
fn standing_of(lifetime: tilo_action_types::Lifetime) -> (Standing, i64) {
    use tilo_action_types::Lifetime;
    match lifetime {
        Lifetime::Once => (Standing::Once, 0),
        Lifetime::Session => (Standing::Session, 0),
        Lifetime::Until(at) => (Standing::Until, i64::try_from(at).unwrap_or(i64::MAX)),
        Lifetime::UntilRevoked => (Standing::UntilRevoked, 0),
    }
}

/// The capability's own name, for a stable order the pane reads.
const fn capability_name(cap: Capability) -> &'static str {
    match cap {
        Capability::Camera => "camera",
        Capability::Microphone => "microphone",
        Capability::Screen => "screen",
        Capability::Location => "location",
        Capability::Input => "input",
        Capability::SystemAudio => "system-audio",
        Capability::Api => "api",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine with the person's answers, and what a stand-in says uses them.
    struct Stand {
        status: os_actions::PrivacyStatus,
        captures: Vec<Capture>,
        answers: Vec<tilo_peer_state::Kept>,
        secrets: SavedSecrets,
        security: Security,
    }

    impl Sources for Stand {
        fn status(&self) -> os_actions::PrivacyStatus {
            self.status.clone()
        }

        fn captures(&self) -> Vec<Capture> {
            self.captures.clone()
        }

        fn answers(&self) -> Vec<tilo_peer_state::Kept> {
            self.answers.clone()
        }

        fn app_name(&self, app: &str) -> String {
            app.rsplit('.').next().unwrap_or(app).to_owned()
        }

        fn secret_apps(&self) -> SavedSecrets {
            self.secrets.clone()
        }

        fn security(&self) -> Security {
            self.security.clone()
        }
    }

    fn answer(
        app: &str,
        cap: tilo_permissions::caps::DeviceCap,
        allowed: bool,
    ) -> tilo_peer_state::Kept {
        tilo_peer_state::Kept {
            answer: tilo_peer_state::DeviceAnswer {
                device: "ab".into(),
                app: app.into(),
                cap,
                answer: if allowed {
                    tilo_action_policy::Answer::Allow
                } else {
                    tilo_action_policy::Answer::Deny
                },
                lifetime: tilo_action_types::Lifetime::UntilRevoked,
            },
            device_label: "Laptop".into(),
            this_device: true,
        }
    }

    fn machine() -> Stand {
        Stand {
            status: os_actions::PrivacyStatus {
                microphone_muted: Some(true),
                microphone_kill: os_actions::KillState::Allowed,
                camera_present: true,
                camera_kill: os_actions::KillState::SoftwareBlocked,
                active_media: vec![os_actions::ActiveMediaUse {
                    client_id: 7,
                    pid: 90,
                    app_id: "so.tilo.recorder".into(),
                    label: "recorder".into(),
                    binary: "/usr/bin/recorder".into(),
                    kind: os_actions::MediaKind::Camera,
                }],
                ..os_actions::PrivacyStatus::default()
            },
            captures: vec![Capture {
                app: "so.tilo.meet".into(),
                capability: Capability::Screen,
            }],
            answers: vec![
                answer(
                    "so.tilo.recorder",
                    tilo_permissions::caps::DeviceCap::Camera,
                    true,
                ),
                answer(
                    "so.tilo.meet",
                    tilo_permissions::caps::DeviceCap::Microphone,
                    false,
                ),
            ],
            secrets: SavedSecrets {
                apps: vec![SecretApp {
                    app: "sandbox:so.tilo.mail".into(),
                    name: "mail".into(),
                    allowed: true,
                    uses: 7,
                    items: 2,
                }],
                read_error: None,
            },
            security: Security {
                disk_encryption: tilo_device_surfaces::privacy::Protection::On,
                secure_boot: tilo_device_surfaces::privacy::Protection::Off,
                firewall: tilo_device_surfaces::privacy::FirewallInForce::Firewalld,
                firewall_error: None,
            },
        }
    }

    /// The reading names each device, whether the machine has it, what blocks it and what uses
    /// it now, and the location says the session cannot see its use.
    #[test]
    fn the_reading_says_what_uses_each_device() {
        let reading = reading_of(&machine());
        let posture = |cap: Capability| {
            reading
                .capabilities
                .iter()
                .find(|posture| posture.capability == cap)
                .unwrap_or_else(|| panic!("{cap:?} is one of the four"))
        };
        assert_eq!(
            reading
                .capabilities
                .iter()
                .map(|posture| posture.capability)
                .collect::<Vec<_>>(),
            vec![
                Capability::Camera,
                Capability::Microphone,
                Capability::Screen,
                Capability::Location
            ]
        );
        let camera = posture(Capability::Camera);
        assert!(camera.present);
        assert_eq!(camera.switch, SwitchState::SoftwareBlocked);
        assert_eq!(
            camera.in_use,
            vec![InUse {
                app: "so.tilo.recorder".into(),
                name: "recorder".into()
            }],
            "the capture stream names the program behind it"
        );
        let microphone = posture(Capability::Microphone);
        assert!(microphone.mute_known && microphone.muted);
        assert_eq!(microphone.switch, SwitchState::Allowed);
        let screen = posture(Capability::Screen);
        assert_eq!(
            screen.in_use,
            vec![InUse {
                app: "so.tilo.meet".into(),
                name: "meet".into()
            }]
        );
        let location = posture(Capability::Location);
        assert!(!location.watched, "the platform asks, so no use is seen");
        assert!(location.in_use.is_empty());
    }

    /// The answers are the person's, each with the device it stands on and how long it stands.
    #[test]
    fn the_answers_name_their_app_and_their_device() {
        let reading = reading_of(&machine());
        assert_eq!(reading.answers.len(), 2);
        let camera = &reading.answers[0];
        assert_eq!(
            (camera.app.as_str(), camera.name.as_str()),
            ("so.tilo.meet", "meet")
        );
        assert!(!camera.allowed, "the microphone was refused there");
        assert_eq!(camera.capability, Capability::Microphone);
        assert_eq!(camera.standing, Standing::UntilRevoked);
        let recorder = &reading.answers[1];
        assert_eq!(recorder.capability, Capability::Camera);
        assert!(recorder.allowed);
        assert!(recorder.this_device);
        assert_eq!(recorder.device, "Laptop");
    }

    /// Each answer is the consent resource the portal publishes for it: the address its own
    /// `forget` is called on is the one the answer carries.
    #[test]
    fn each_answer_carries_the_consent_its_forget_is_called_on() {
        let reading = reading_of(&machine());
        let placed: Vec<&str> = reading
            .answers
            .iter()
            .map(|answer| answer.address.address())
            .collect();
        assert_eq!(
            placed,
            vec![
                "portal:consent:ab\u{1f}so.tilo.meet\u{1f}microphone",
                "portal:consent:ab\u{1f}so.tilo.recorder\u{1f}camera",
            ]
        );
    }

    /// The applications that use the saved passwords and keys, and the device's own protection,
    /// are published with the reading: a pane draws them from what the session read.
    #[test]
    fn the_reading_carries_the_saved_secrets_and_the_protection() {
        let reading = reading_of(&machine());
        let secrets = reading.secrets.expect("the store was read");
        assert_eq!(secrets.apps.len(), 1);
        assert_eq!(secrets.apps[0].app, "sandbox:so.tilo.mail");
        assert!(secrets.apps[0].allowed);
        assert_eq!((secrets.apps[0].uses, secrets.apps[0].items), (7, 2));
        let security = reading.security.expect("the device was read");
        assert_eq!(
            security.disk_encryption,
            tilo_device_surfaces::privacy::Protection::On
        );
        assert_eq!(
            security.secure_boot,
            tilo_device_surfaces::privacy::Protection::Off
        );
        assert_eq!(
            security.firewall,
            tilo_device_surfaces::privacy::FirewallInForce::Firewalld
        );
    }

    /// The session's service publishes the reading on its resource and on no other, and reads it
    /// before anything has: the first read is the machine's, not an empty one.
    #[test]
    fn the_service_publishes_the_reading_where_a_view_reads_it() {
        let privacy = Privacy::over(Box::new(machine()));
        let published = privacy.resources();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].id.as_str(), RESOURCE);
        let read = privacy.read(RESOURCE).expect("the reading");
        let reading: Reading =
            serde_json::from_value(read).expect("the reading is the surface's type");
        assert_eq!(reading.capabilities.len(), 4);
        assert_eq!(reading.answers.len(), 2);
        assert!(privacy.read("session:displays").is_none());
    }

    /// The machine is asked again only after its reading has aged: a pass of the loop does not
    /// run `pw-dump` per frame.
    #[test]
    fn the_machine_is_not_asked_again_within_the_refresh() {
        struct Counted(std::sync::Arc<std::sync::atomic::AtomicUsize>, Stand);
        impl Sources for Counted {
            fn status(&self) -> os_actions::PrivacyStatus {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.1.status()
            }
            fn captures(&self) -> Vec<Capture> {
                self.1.captures()
            }
            fn answers(&self) -> Vec<tilo_peer_state::Kept> {
                self.1.answers()
            }
            fn app_name(&self, app: &str) -> String {
                self.1.app_name(app)
            }
            fn secret_apps(&self) -> SavedSecrets {
                self.1.secret_apps()
            }
            fn security(&self) -> Security {
                self.1.security()
            }
        }
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut privacy =
            Privacy::over(Box::new(Counted(std::sync::Arc::clone(&asked), machine())));
        let _ = privacy.read(RESOURCE);
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!privacy.poll(), "nothing moved in the reading");
        let _ = privacy.read(RESOURCE);
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a read within the refresh answers from the reading read before"
        );
    }
}

/// A device with no store of saved passwords and keys to change.
#[cfg(not(target_os = "linux"))]
fn secret_command(_arguments: &[&str]) -> Result<(), String> {
    Err("This device keeps no saved passwords and keys.".to_owned())
}
