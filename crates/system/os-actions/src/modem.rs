//! Mobile broadband: the modems `ModemManager` drives and the data
//! connection `NetworkManager` keeps for them.
//!
//! `ModemManager` is asked over D-Bus for what only it knows (the SIMs and
//! their locks, signal, operator, registration, network modes, traffic)
//! and for what only it does (PIN, PUK, network mode, SIM slot). The data
//! connection, its APN, roaming and metering are a `NetworkManager` profile,
//! changed through `nmcli` as the rest of the network settings are.
//!
//! PINs and PUKs travel as D-Bus message bodies. They never appear in argv,
//! an environment variable, `NetworkManager` profile storage, a log or Tilo
//! settings, and live in zeroizing buffers on the way.

use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zeroize::Zeroizing;

use crate::network::{MeteredPolicy, split_terse, validate_uuid};
use crate::process::{run, run_capture};

/// `ModemManager`'s bus name.
pub const SERVICE: &str = "org.freedesktop.ModemManager1";
/// Where `ModemManager` lists its modems.
const ROOT: &str = "/org/freedesktop/ModemManager1";
const MODEM_INTERFACE: &str = "org.freedesktop.ModemManager1.Modem";
const MODEM_PREFIX: &str = "/org/freedesktop/ModemManager1/Modem/";
const SIM_PREFIX: &str = "/org/freedesktop/ModemManager1/SIM/";
const BEARER_PREFIX: &str = "/org/freedesktop/ModemManager1/Bearer/";

#[zbus::proxy(
    interface = "org.freedesktop.ModemManager1.Modem",
    default_service = "org.freedesktop.ModemManager1"
)]
trait Modem {
    #[zbus(property)]
    fn state(&self) -> zbus::Result<i32>;
    #[zbus(property)]
    fn state_failed_reason(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn manufacturer(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn model(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn unlock_required(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn unlock_retries(&self) -> zbus::Result<HashMap<u32, u32>>;
    #[zbus(property)]
    fn signal_quality(&self) -> zbus::Result<(u32, bool)>;
    #[zbus(property)]
    fn access_technologies(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn supported_modes(&self) -> zbus::Result<Vec<(u32, u32)>>;
    #[zbus(property)]
    fn current_modes(&self) -> zbus::Result<(u32, u32)>;
    #[zbus(property)]
    fn sim(&self) -> zbus::Result<OwnedObjectPath>;
    #[zbus(property)]
    fn sim_slots(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    #[zbus(property)]
    fn primary_sim_slot(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn own_numbers(&self) -> zbus::Result<Vec<String>>;
    #[zbus(property)]
    fn bearers(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    fn set_current_modes(&self, modes: &(u32, u32)) -> zbus::Result<()>;
    fn set_primary_sim_slot(&self, sim_slot: u32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp",
    default_service = "org.freedesktop.ModemManager1"
)]
trait Modem3gpp {
    #[zbus(property)]
    fn operator_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn operator_code(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn registration_state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn enabled_facility_locks(&self) -> zbus::Result<u32>;
}

#[zbus::proxy(
    interface = "org.freedesktop.ModemManager1.Sim",
    default_service = "org.freedesktop.ModemManager1"
)]
trait Sim {
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn sim_identifier(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn operator_identifier(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn operator_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn sim_type(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn esim_status(&self) -> zbus::Result<u32>;
    fn send_pin(&self, pin: &str) -> zbus::Result<()>;
    fn send_puk(&self, puk: &str, pin: &str) -> zbus::Result<()>;
    fn enable_pin(&self, pin: &str, enabled: bool) -> zbus::Result<()>;
    fn change_pin(&self, old_pin: &str, new_pin: &str) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.ModemManager1.Bearer",
    default_service = "org.freedesktop.ModemManager1"
)]
trait Bearer {
    #[zbus(property)]
    fn stats(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

/// What `ModemManager` reports: every modem it drives, in path order.
/// Empty on a machine without a modem.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MobileReading {
    pub modems: Vec<ModemInfo>,
}

/// One modem, as `ModemManager` reports it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModemInfo {
    /// Its `ModemManager` object path.
    pub path: String,
    /// Manufacturer and model, as the modem names itself.
    pub name: String,
    pub state: ModemState,
    /// Why the modem cannot work, when it failed.
    pub failure: Option<ModemFailure>,
    /// What the active SIM asks for before the modem can be used.
    pub lock: SimLock,
    pub retries: UnlockRetries,
    /// Whether the active SIM asks for its PIN at power-on; `None` when the
    /// modem does not say.
    pub pin_enabled: Option<bool>,
    /// Signal quality in percent, while registered.
    pub signal: Option<u8>,
    /// The fastest access technology in use.
    pub technology: Option<Technology>,
    /// The network operator the modem is registered with.
    pub operator: Option<String>,
    pub registration: Registration,
    /// The network types the modem may use now.
    pub modes: Option<NetworkModes>,
    /// Every combination of network types the modem can be told to use.
    pub supported_modes: Vec<NetworkModes>,
    /// The SIMs, one per slot that holds one (or the one SIM of a modem
    /// without slots).
    pub sims: Vec<SimCard>,
    /// The phone numbers of the active SIM.
    pub numbers: Vec<String>,
    /// Bytes moved through the modem's data connections since
    /// `ModemManager` started counting.
    pub traffic: Option<Traffic>,
}

impl ModemInfo {
    /// The SIM in use.
    #[must_use]
    pub fn active_sim(&self) -> Option<&SimCard> {
        self.sims
            .iter()
            .find(|sim| sim.active)
            .or_else(|| self.sims.first())
    }

    /// Whether the modem is ready to carry data: no SIM lock, not failed.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.failure.is_none() && matches!(self.lock, SimLock::None)
    }
}

/// `ModemManager`'s modem states (`MMModemState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModemState {
    Failed,
    Unknown,
    Initializing,
    Locked,
    Disabled,
    Disabling,
    Enabling,
    Enabled,
    Searching,
    Registered,
    Disconnecting,
    Connecting,
    Connected,
}

impl ModemState {
    fn from_mm(value: i32) -> Self {
        match value {
            -1 => Self::Failed,
            1 => Self::Initializing,
            2 => Self::Locked,
            3 => Self::Disabled,
            4 => Self::Disabling,
            5 => Self::Enabling,
            6 => Self::Enabled,
            7 => Self::Searching,
            8 => Self::Registered,
            9 => Self::Disconnecting,
            10 => Self::Connecting,
            11 => Self::Connected,
            _ => Self::Unknown,
        }
    }
}

/// Why a modem failed (`MMModemStateFailedReason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModemFailure {
    SimMissing,
    SimError,
    /// An eSIM with no profile installed.
    EsimWithoutProfiles,
    Other,
}

impl ModemFailure {
    fn from_mm(value: u32) -> Option<Self> {
        match value {
            0 => None,
            2 => Some(Self::SimMissing),
            3 => Some(Self::SimError),
            5 => Some(Self::EsimWithoutProfiles),
            _ => Some(Self::Other),
        }
    }
}

/// What a SIM asks for before the modem can be used (`MMModemLock`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimLock {
    None,
    /// The SIM's PIN.
    Pin,
    /// The PUK and a new PIN, after too many wrong PINs.
    Puk,
    /// A lock this settings page cannot lift (a network or carrier lock).
    Other,
}

impl SimLock {
    const MM_NONE: u32 = 1;
    const MM_SIM_PIN: u32 = 2;
    const MM_SIM_PIN2: u32 = 3;
    const MM_SIM_PUK: u32 = 4;
    const MM_SIM_PUK2: u32 = 5;

    fn from_mm(value: u32) -> Self {
        match value {
            // Unknown says nothing is asked for yet; PIN2 and PUK2 guard
            // only fixed dialling, never data.
            0 | Self::MM_NONE | Self::MM_SIM_PIN2 | Self::MM_SIM_PUK2 => Self::None,
            Self::MM_SIM_PIN => Self::Pin,
            Self::MM_SIM_PUK => Self::Puk,
            _ => Self::Other,
        }
    }
}

/// How many tries are left before the SIM locks harder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnlockRetries {
    pub pin: Option<u32>,
    pub puk: Option<u32>,
}

/// The fastest access technology a modem uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Technology {
    Gsm,
    Gprs,
    Edge,
    Cdma,
    Umts,
    Hspa,
    HspaPlus,
    EvDo,
    Lte,
    Nr5g,
}

impl Technology {
    /// The fastest one in `ModemManager`'s access technology bits.
    fn from_mm(bits: u32) -> Option<Self> {
        const TABLE: &[(u32, Technology)] = &[
            (1 << 15, Technology::Nr5g),
            (1 << 14 | 1 << 16 | 1 << 17, Technology::Lte),
            (1 << 11 | 1 << 12 | 1 << 13, Technology::EvDo),
            (1 << 9, Technology::HspaPlus),
            (1 << 6 | 1 << 7 | 1 << 8, Technology::Hspa),
            (1 << 5, Technology::Umts),
            (1 << 10, Technology::Cdma),
            (1 << 4, Technology::Edge),
            (1 << 3, Technology::Gprs),
            (1 << 1 | 1 << 2, Technology::Gsm),
        ];
        TABLE
            .iter()
            .find(|(mask, _)| bits & mask != 0)
            .map(|(_, technology)| *technology)
    }

    /// The name people see on their phones; the same in every language.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Gsm => "2G",
            Self::Gprs => "GPRS",
            Self::Edge => "EDGE",
            Self::Cdma => "CDMA",
            Self::Umts => "3G",
            Self::Hspa => "HSPA",
            Self::HspaPlus => "HSPA+",
            Self::EvDo => "EV-DO",
            Self::Lte => "LTE",
            Self::Nr5g => "5G",
        }
    }
}

/// Where the modem stands with the mobile network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registration {
    /// Not registered and not looking.
    Idle,
    Home,
    Roaming,
    Searching,
    /// The network refused the SIM.
    Denied,
    /// Only emergency calls.
    EmergencyOnly,
    Unknown,
}

impl Registration {
    fn from_mm(value: u32) -> Self {
        match value {
            0 => Self::Idle,
            1 | 6 | 9 => Self::Home,
            2 => Self::Searching,
            3 => Self::Denied,
            5 | 7 | 10 => Self::Roaming,
            8 => Self::EmergencyOnly,
            _ => Self::Unknown,
        }
    }
}

/// Network types a modem may use, and the one it prefers among them
/// (`MMModemMode` bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NetworkModes {
    pub allowed: u32,
    pub preferred: u32,
}

impl NetworkModes {
    const GENERATIONS: [(u32, u8); 4] = [(1 << 4, 5), (1 << 3, 4), (1 << 2, 3), (1 << 1, 2)];

    /// The generations allowed, fastest first (5, 4, 3, 2).
    #[must_use]
    pub fn generations(self) -> Vec<u8> {
        Self::GENERATIONS
            .iter()
            .filter(|(bit, _)| self.allowed & bit != 0)
            .map(|(_, generation)| *generation)
            .collect()
    }

    /// The preferred generation, when one is.
    #[must_use]
    pub fn preferred_generation(self) -> Option<u8> {
        Self::GENERATIONS
            .iter()
            .find(|(bit, _)| self.preferred & bit != 0)
            .map(|(_, generation)| *generation)
    }

    /// A stable name for an action's parameter: `5g-4g-3g`, with
    /// `-prefer-4g` when one is preferred.
    #[must_use]
    pub fn key(self) -> String {
        let mut key = self
            .generations()
            .iter()
            .map(|generation| format!("{generation}g"))
            .collect::<Vec<_>>()
            .join("-");
        if let Some(preferred) = self.preferred_generation() {
            key = format!("{key}-prefer-{preferred}g");
        }
        key
    }
}

/// One SIM: a card in a slot, or an eSIM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimCard {
    /// Its `ModemManager` object path.
    pub path: String,
    /// The slot it is in, counting from 1, on a modem with several slots.
    pub slot: Option<u32>,
    /// Whether the modem uses it now.
    pub active: bool,
    /// The operator that issued it.
    pub operator: Option<String>,
    /// The operator's MCC and MNC (`26201`), which picks the APN.
    pub operator_code: Option<String>,
    /// The ICCID printed on the card.
    pub iccid: Option<String>,
    pub kind: SimKind,
}

impl SimCard {
    /// What a person calls it: the operator, else the slot.
    #[must_use]
    pub fn label(&self) -> String {
        self.operator.clone().unwrap_or_else(|| match self.slot {
            Some(slot) => format!("SIM {slot}"),
            None => "SIM".into(),
        })
    }
}

/// A physical card or an eSIM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimKind {
    Physical,
    /// An eSIM; `profiles` says whether one is installed.
    Esim {
        profiles: Option<bool>,
    },
    Unknown,
}

/// Bytes received and sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Traffic {
    pub received: u64,
    pub sent: u64,
}

impl Traffic {
    #[must_use]
    pub fn total(self) -> u64 {
        self.received.saturating_add(self.sent)
    }
}

/// Why a modem operation did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModemError {
    /// `ModemManager` is not running or not reachable.
    Unavailable(String),
    /// The PIN or PUK was wrong.
    WrongCode,
    /// The input was refused before anything was asked.
    Invalid(String),
    /// `ModemManager` refused or failed.
    Failed(String),
}

impl std::fmt::Display for ModemError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(formatter, "ModemManager is unavailable: {reason}"),
            Self::WrongCode => formatter.write_str("the code is wrong"),
            Self::Invalid(reason) | Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

impl From<ModemError> for String {
    fn from(error: ModemError) -> Self {
        error.to_string()
    }
}

fn failed(error: &zbus::Error) -> ModemError {
    match error {
        zbus::Error::MethodError(name, _, _) if name.as_str().ends_with(".IncorrectPassword") => {
            ModemError::WrongCode
        }
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner" =>
        {
            ModemError::Unavailable("ModemManager is not running".into())
        }
        zbus::Error::MethodError(_, Some(message), _) => ModemError::Failed(message.clone()),
        other => ModemError::Failed(other.to_string()),
    }
}

/// A change to a modem or its SIM. Codes are zeroed when it is dropped.
pub enum ModemChange {
    /// Unlock the SIM with its PIN.
    SendPin { sim: String, pin: Zeroizing<String> },
    /// Unlock a SIM locked after too many wrong PINs, setting a new PIN.
    SendPuk {
        sim: String,
        puk: Zeroizing<String>,
        new_pin: Zeroizing<String>,
    },
    /// Ask for the PIN at power-on, or stop asking.
    SetPinLock {
        sim: String,
        pin: Zeroizing<String>,
        enabled: bool,
    },
    ChangePin {
        sim: String,
        old_pin: Zeroizing<String>,
        new_pin: Zeroizing<String>,
    },
    /// Use these network types.
    SetModes { modem: String, modes: NetworkModes },
    /// Use the SIM in this slot (counting from 1).
    UseSlot { modem: String, slot: u32 },
}

impl std::fmt::Debug for ModemChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the codes.
        match self {
            Self::SendPin { sim, .. } => write!(formatter, "SendPin({sim})"),
            Self::SendPuk { sim, .. } => write!(formatter, "SendPuk({sim})"),
            Self::SetPinLock { sim, enabled, .. } => {
                write!(formatter, "SetPinLock({sim}, {enabled})")
            }
            Self::ChangePin { sim, .. } => write!(formatter, "ChangePin({sim})"),
            Self::SetModes { modem, modes } => write!(formatter, "SetModes({modem}, {modes:?})"),
            Self::UseSlot { modem, slot } => write!(formatter, "UseSlot({modem}, {slot})"),
        }
    }
}

/// A SIM PIN: 4 to 8 digits.
fn valid_pin(pin: &str) -> Result<(), ModemError> {
    if (4..=8).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ModemError::Invalid("a SIM PIN has 4 to 8 digits".into()))
    }
}

/// A PUK: 8 digits.
fn valid_puk(puk: &str) -> Result<(), ModemError> {
    if puk.len() == 8 && puk.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ModemError::Invalid("a PUK has 8 digits".into()))
    }
}

fn valid_object(path: &str, prefix: &str) -> Result<(), ModemError> {
    path.strip_prefix(prefix)
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .map(|_| ())
        .ok_or_else(|| ModemError::Invalid(format!("not a ModemManager object: {path}")))
}

impl ModemChange {
    /// Refuse malformed codes and objects before `ModemManager` is asked.
    fn validate(&self) -> Result<(), ModemError> {
        match self {
            Self::SendPin { sim, pin } | Self::SetPinLock { sim, pin, .. } => {
                valid_object(sim, SIM_PREFIX)?;
                valid_pin(pin)
            }
            Self::SendPuk { sim, puk, new_pin } => {
                valid_object(sim, SIM_PREFIX)?;
                valid_puk(puk)?;
                valid_pin(new_pin)
            }
            Self::ChangePin {
                sim,
                old_pin,
                new_pin,
            } => {
                valid_object(sim, SIM_PREFIX)?;
                valid_pin(old_pin)?;
                valid_pin(new_pin)
            }
            Self::SetModes { modem, .. } => valid_object(modem, MODEM_PREFIX),
            Self::UseSlot { modem, slot } => {
                valid_object(modem, MODEM_PREFIX)?;
                if *slot == 0 {
                    return Err(ModemError::Invalid("SIM slots count from 1".into()));
                }
                Ok(())
            }
        }
    }
}

/// `ModemManager` over one bus connection: the system bus on a device, a
/// private bus in the tests.
pub struct ModemManager {
    connection: zbus::Connection,
}

/// `a{oa{sa{sv}}}`, as `GetManagedObjects` answers.
type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

impl ModemManager {
    #[must_use]
    pub fn on(connection: zbus::Connection) -> Self {
        Self { connection }
    }

    /// `ModemManager` on the system bus.
    ///
    /// # Errors
    /// When the system bus cannot be reached.
    pub async fn system() -> Result<Self, ModemError> {
        zbus::Connection::system()
            .await
            .map(Self::on)
            .map_err(|error| ModemError::Unavailable(error.to_string()))
    }

    async fn proxy<'p, P>(&self, path: &str) -> Result<P, ModemError>
    where
        P: From<zbus::Proxy<'p>> + zbus::proxy::Defaults,
    {
        zbus::proxy::Builder::<P>::new(&self.connection)
            .path(path.to_owned())
            .map_err(|error| ModemError::Invalid(error.to_string()))?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(|error| failed(&error))
    }

    /// Every modem `ModemManager` drives, in path order.
    ///
    /// # Errors
    /// When `ModemManager` is not running or does not answer.
    pub async fn read(&self) -> Result<MobileReading, ModemError> {
        let objects: ManagedObjects = zbus::Proxy::new(
            &self.connection,
            SERVICE,
            ROOT,
            "org.freedesktop.DBus.ObjectManager",
        )
        .await
        .map_err(|error| failed(&error))?
        .call("GetManagedObjects", &())
        .await
        .map_err(|error| failed(&error))?;
        let mut paths: Vec<String> = objects
            .iter()
            .filter(|(_, interfaces)| interfaces.contains_key(MODEM_INTERFACE))
            .map(|(path, _)| path.to_string())
            .collect();
        paths.sort_by_key(|path| object_number(path));
        let mut modems = Vec::with_capacity(paths.len());
        for path in paths {
            modems.push(self.modem(&path).await?);
        }
        Ok(MobileReading { modems })
    }

    async fn modem(&self, path: &str) -> Result<ModemInfo, ModemError> {
        let modem: ModemProxy<'static> = self.proxy(path).await?;
        let gpp: Modem3gppProxy<'static> = self.proxy(path).await?;
        let state = ModemState::from_mm(modem.state().await.map_err(|error| failed(&error))?);
        let retries = modem.unlock_retries().await.unwrap_or_default();
        let retries = UnlockRetries {
            pin: retries.get(&SimLock::MM_SIM_PIN).copied(),
            puk: retries.get(&SimLock::MM_SIM_PUK).copied(),
        };
        let name = [modem.manufacturer().await.ok(), modem.model().await.ok()]
            .into_iter()
            .flatten()
            .map(|part| part.trim().to_owned())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let registered = matches!(
            state,
            ModemState::Registered
                | ModemState::Connected
                | ModemState::Connecting
                | ModemState::Disconnecting
        );
        let signal = modem
            .signal_quality()
            .await
            .ok()
            .filter(|_| registered)
            .map(|(quality, _)| u8::try_from(quality.min(100)).unwrap_or(100));
        let technology = modem
            .access_technologies()
            .await
            .ok()
            .filter(|_| registered)
            .and_then(Technology::from_mm);
        let current = modem
            .current_modes()
            .await
            .ok()
            .map(|(allowed, preferred)| NetworkModes { allowed, preferred });
        let supported_modes = modem
            .supported_modes()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(allowed, preferred)| NetworkModes { allowed, preferred })
            .collect();
        let operator = gpp
            .operator_name()
            .await
            .ok()
            .filter(|_| registered)
            .as_deref()
            .and_then(non_empty);
        let registration = gpp
            .registration_state()
            .await
            .map_or(Registration::Unknown, Registration::from_mm);
        let pin_enabled = gpp
            .enabled_facility_locks()
            .await
            .ok()
            .map(|locks| locks & 1 != 0);
        Ok(ModemInfo {
            path: path.to_owned(),
            name,
            state,
            failure: ModemFailure::from_mm(modem.state_failed_reason().await.unwrap_or(0))
                .filter(|_| state == ModemState::Failed),
            lock: SimLock::from_mm(modem.unlock_required().await.unwrap_or(0)),
            retries,
            pin_enabled,
            signal,
            technology,
            operator,
            registration,
            modes: current,
            supported_modes,
            sims: self.sims(&modem).await,
            numbers: modem.own_numbers().await.unwrap_or_default(),
            traffic: self.traffic(&modem).await,
        })
    }

    /// The SIMs in the modem's slots, or its one SIM.
    async fn sims(&self, modem: &ModemProxy<'_>) -> Vec<SimCard> {
        let slots = modem.sim_slots().await.unwrap_or_default();
        let primary = modem.primary_sim_slot().await.unwrap_or(0);
        let mut sims = Vec::new();
        if slots.is_empty() {
            if let Ok(path) = modem.sim().await
                && path.as_str().starts_with(SIM_PREFIX)
                && let Some(sim) = self.sim(path.as_str(), None, true).await
            {
                sims.push(sim);
            }
            return sims;
        }
        for (index, path) in (1u32..).zip(slots.iter()) {
            if !path.as_str().starts_with(SIM_PREFIX) {
                continue;
            }
            if let Some(sim) = self.sim(path.as_str(), Some(index), index == primary).await {
                sims.push(sim);
            }
        }
        sims
    }

    async fn sim(&self, path: &str, slot: Option<u32>, primary: bool) -> Option<SimCard> {
        let sim: SimProxy<'static> = self.proxy(path).await.ok()?;
        let kind = match sim.sim_type().await {
            Ok(1) => SimKind::Physical,
            Ok(2) => SimKind::Esim {
                profiles: match sim.esim_status().await {
                    Ok(1) => Some(false),
                    Ok(2) => Some(true),
                    _ => None,
                },
            },
            _ => SimKind::Unknown,
        };
        Some(SimCard {
            path: path.to_owned(),
            slot,
            active: sim.active().await.unwrap_or(primary),
            operator: sim
                .operator_name()
                .await
                .ok()
                .as_deref()
                .and_then(non_empty),
            operator_code: sim
                .operator_identifier()
                .await
                .ok()
                .as_deref()
                .and_then(non_empty),
            iccid: sim
                .sim_identifier()
                .await
                .ok()
                .as_deref()
                .and_then(non_empty),
            kind,
        })
    }

    /// Bytes through the modem's data connections, summed over its
    /// bearers: the totals since `ModemManager` started where it keeps
    /// them, else the current connection's.
    async fn traffic(&self, modem: &ModemProxy<'_>) -> Option<Traffic> {
        let bearers = modem.bearers().await.ok()?;
        let mut traffic: Option<Traffic> = None;
        for path in bearers
            .iter()
            .filter(|path| path.as_str().starts_with(BEARER_PREFIX))
        {
            let Ok(bearer) = self.proxy::<BearerProxy<'static>>(path.as_str()).await else {
                continue;
            };
            let Ok(stats) = bearer.stats().await else {
                continue;
            };
            let count = |total: &str, current: &str| {
                u64_of(stats.get(total)).or_else(|| u64_of(stats.get(current)))
            };
            let (Some(received), Some(sent)) = (
                count("total-rx-bytes", "rx-bytes"),
                count("total-tx-bytes", "tx-bytes"),
            ) else {
                continue;
            };
            let sum = traffic.get_or_insert_with(Traffic::default);
            sum.received = sum.received.saturating_add(received);
            sum.sent = sum.sent.saturating_add(sent);
        }
        traffic
    }

    /// Carry out `change`.
    ///
    /// # Errors
    /// A malformed code or object, a wrong code, or `ModemManager`'s refusal.
    pub async fn apply(&self, change: &ModemChange) -> Result<(), ModemError> {
        change.validate()?;
        let result = match change {
            ModemChange::SendPin { sim, pin } => {
                let sim: SimProxy<'static> = self.proxy(sim).await?;
                sim.send_pin(pin).await
            }
            ModemChange::SendPuk { sim, puk, new_pin } => {
                let sim: SimProxy<'static> = self.proxy(sim).await?;
                sim.send_puk(puk, new_pin).await
            }
            ModemChange::SetPinLock { sim, pin, enabled } => {
                let sim: SimProxy<'static> = self.proxy(sim).await?;
                sim.enable_pin(pin, *enabled).await
            }
            ModemChange::ChangePin {
                sim,
                old_pin,
                new_pin,
            } => {
                let sim: SimProxy<'static> = self.proxy(sim).await?;
                sim.change_pin(old_pin, new_pin).await
            }
            ModemChange::SetModes { modem, modes } => {
                let modem: ModemProxy<'static> = self.proxy(modem).await?;
                modem
                    .set_current_modes(&(modes.allowed, modes.preferred))
                    .await
            }
            ModemChange::UseSlot { modem, slot } => {
                let modem: ModemProxy<'static> = self.proxy(modem).await?;
                modem.set_primary_sim_slot(*slot).await
            }
        };
        result.map_err(|error| failed(&error))
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed != "--").then(|| trimmed.to_owned())
}

fn u64_of(value: Option<&OwnedValue>) -> Option<u64> {
    match value.map(|value| &**value) {
        Some(Value::U64(n)) => Some(*n),
        Some(Value::U32(n)) => Some(u64::from(*n)),
        _ => None,
    }
}

/// The number at the end of an object path, so `Modem/10` sorts after
/// `Modem/9`.
fn object_number(path: &str) -> (u64, String) {
    let number = path
        .rsplit('/')
        .next()
        .and_then(|id| id.parse().ok())
        .unwrap_or(u64::MAX);
    (number, path.to_owned())
}

/// Run `future` to its end on the app runtime, from a worker thread.
fn block<T>(
    future: impl std::future::Future<Output = Result<T, ModemError>>,
) -> Result<T, ModemError> {
    tilo_rt::try_app()
        .and_then(|runtime| runtime.block_on(future))
        .map_err(|error| ModemError::Unavailable(error.to_string()))?
}

/// The modems on this device, from `ModemManager` on the system bus.
///
/// # Errors
/// When `ModemManager` is not running or does not answer.
pub fn read_mobile() -> Result<MobileReading, ModemError> {
    block(async { ModemManager::system().await?.read().await })
}

/// Carry out `change` through `ModemManager` on the system bus.
///
/// # Errors
/// A malformed code or object, a wrong code, or `ModemManager`'s refusal.
pub fn apply_modem_change(change: &ModemChange) -> Result<(), ModemError> {
    change.validate()?;
    block(async { ModemManager::system().await?.apply(change).await })
}

/// The `NetworkManager` profile mobile data connects with.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate NetworkManager property of the profile"
)]
pub struct MobileProfile {
    pub uuid: String,
    pub name: String,
    /// Whether it connects by itself whenever the modem can.
    pub autoconnect: bool,
    /// Whether it is connected now.
    pub active: bool,
    /// The APN typed in; `None` while it is chosen automatically.
    pub apn: Option<String>,
    /// Whether `NetworkManager` picks the APN from the provider database.
    pub automatic_apn: bool,
    /// Whether it may connect on another operator's network.
    pub roaming: bool,
    pub metered: MeteredPolicy,
}

/// What a new mobile data profile is called.
pub const MOBILE_PROFILE_NAME: &str = "Mobile data";

/// The profile mobile data uses: the one connected, else the one that
/// connects by itself, else the one used last. `None` when there is none.
///
/// # Errors
/// When `nmcli` cannot list or read the profiles.
pub fn read_mobile_profile() -> Result<Option<MobileProfile>, String> {
    let list = run_capture(
        "nmcli",
        &[
            "-t",
            "-f",
            "UUID,TYPE,AUTOCONNECT,ACTIVE,TIMESTAMP",
            "connection",
            "show",
        ],
    )?;
    let Some(chosen) = choose_mobile_profile(&list) else {
        return Ok(None);
    };
    let details = run_capture(
        "nmcli",
        &[
            "-t",
            "--escape",
            "yes",
            "-f",
            "connection.id,connection.autoconnect,connection.metered,gsm.apn,gsm.auto-config,gsm.home-only",
            "connection",
            "show",
            "uuid",
            &chosen.uuid,
        ],
    )?;
    Ok(Some(parse_mobile_profile(chosen, &details)))
}

/// One row of `nmcli connection show`, before its details are read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedProfile {
    pub uuid: String,
    pub autoconnect: bool,
    pub active: bool,
    pub timestamp: u64,
}

/// The mobile data profile among `nmcli -t -f
/// UUID,TYPE,AUTOCONNECT,ACTIVE,TIMESTAMP connection show`.
#[must_use]
pub fn choose_mobile_profile(list: &str) -> Option<ListedProfile> {
    list.lines()
        .filter_map(|line| {
            let fields = split_terse(line);
            let [uuid, kind, autoconnect, active, timestamp] = fields.as_slice() else {
                return None;
            };
            (kind == "gsm" && validate_uuid(uuid).is_ok()).then(|| ListedProfile {
                uuid: uuid.clone(),
                autoconnect: autoconnect == "yes",
                active: active == "yes",
                timestamp: timestamp.parse().unwrap_or(0),
            })
        })
        .max_by_key(|profile| (profile.active, profile.autoconnect, profile.timestamp))
}

/// `listed`'s details, from `nmcli -t --escape yes -f … connection show`.
#[must_use]
pub fn parse_mobile_profile(listed: ListedProfile, details: &str) -> MobileProfile {
    let mut values = HashMap::new();
    for line in details.lines() {
        let fields = split_terse(line);
        if let Some((key, rest)) = fields.split_first() {
            values.insert(key.clone(), rest.join(":"));
        }
    }
    let value = |key: &str| values.get(key).map_or("", |v| v.trim());
    let automatic_apn = value("gsm.auto-config") == "yes";
    MobileProfile {
        name: Some(value("connection.id"))
            .filter(|name| !name.is_empty())
            .unwrap_or(MOBILE_PROFILE_NAME)
            .to_owned(),
        autoconnect: values
            .get("connection.autoconnect")
            .map_or(listed.autoconnect, |v| v.trim() == "yes"),
        active: listed.active,
        apn: Some(value("gsm.apn"))
            .filter(|apn| !apn.is_empty() && *apn != "--" && !automatic_apn)
            .map(str::to_owned),
        automatic_apn,
        roaming: value("gsm.home-only") != "yes",
        metered: match value("connection.metered") {
            "yes" => MeteredPolicy::Metered,
            "no" => MeteredPolicy::Unmetered,
            _ => MeteredPolicy::Automatic,
        },
        uuid: listed.uuid,
    }
}

/// An APN: what operators publish, never a control character.
fn valid_apn(apn: &str) -> Result<(), String> {
    if apn.is_empty()
        || apn.len() > 100
        || !apn
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err("an APN is letters, digits, dots, dashes and underscores".into());
    }
    Ok(())
}

/// Make the mobile data profile: the APN chosen from the provider database,
/// connecting by itself. Its PIN is never stored: the SIM is unlocked
/// through `ModemManager`.
fn create_mobile_profile() -> Result<String, String> {
    run(
        "nmcli",
        &[
            "connection",
            "add",
            "type",
            "gsm",
            "ifname",
            "*",
            "con-name",
            MOBILE_PROFILE_NAME,
            "gsm.auto-config",
            "yes",
            "gsm.pin-flags",
            "2",
            "gsm.password-flags",
            "2",
            "connection.autoconnect",
            "yes",
        ],
    )?;
    read_mobile_profile()?
        .map(|profile| profile.uuid)
        .ok_or_else(|| "NetworkManager did not keep the mobile data profile".into())
}

/// The mobile data profile's uuid, making the profile when there is none,
/// so its roaming, APN and metering can be set before data is first on.
///
/// # Errors
/// What `nmcli` refused.
pub fn mobile_profile_uuid() -> Result<String, String> {
    match read_mobile_profile()? {
        Some(profile) => Ok(profile.uuid),
        None => create_mobile_profile(),
    }
}

/// Turn mobile data on or off: the modem's radio, and the profile that
/// connects through it. Turning it on makes the profile when there is none
/// and connects it.
///
/// # Errors
/// What `nmcli` refused.
pub fn set_mobile_data(on: bool) -> Result<(), String> {
    run("nmcli", &["radio", "wwan", if on { "on" } else { "off" }])?;
    if !on {
        return Ok(());
    }
    let profile = read_mobile_profile()?;
    let uuid = match &profile {
        Some(profile) => profile.uuid.clone(),
        None => create_mobile_profile()?,
    };
    if profile.as_ref().is_some_and(|profile| !profile.autoconnect) {
        run(
            "nmcli",
            &[
                "connection",
                "modify",
                "uuid",
                &uuid,
                "connection.autoconnect",
                "yes",
            ],
        )?;
    }
    if profile.as_ref().is_some_and(|profile| profile.active) {
        return Ok(());
    }
    run("nmcli", &["connection", "up", "uuid", &uuid])
}

/// Let the mobile data profile connect on another operator's network, or
/// keep it to the home network.
///
/// # Errors
/// A malformed profile id, or what `nmcli` refused.
pub fn set_mobile_roaming(uuid: &str, allowed: bool) -> Result<(), String> {
    validate_uuid(uuid)?;
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            "uuid",
            uuid,
            "gsm.home-only",
            if allowed { "no" } else { "yes" },
        ],
    )
}

/// Use `apn`, or (`None`) the one the provider database names for the SIM.
///
/// # Errors
/// A malformed profile id or APN, or what `nmcli` refused.
pub fn set_mobile_apn(uuid: &str, apn: Option<&str>) -> Result<(), String> {
    validate_uuid(uuid)?;
    match apn {
        Some(apn) => {
            valid_apn(apn)?;
            run(
                "nmcli",
                &[
                    "connection",
                    "modify",
                    "uuid",
                    uuid,
                    "gsm.auto-config",
                    "no",
                    "gsm.apn",
                    apn,
                ],
            )
        }
        None => run(
            "nmcli",
            &[
                "connection",
                "modify",
                "uuid",
                uuid,
                "gsm.apn",
                "",
                "gsm.auto-config",
                "yes",
            ],
        ),
    }
}

#[cfg(test)]
#[path = "modem_tests.rs"]
mod tests;
