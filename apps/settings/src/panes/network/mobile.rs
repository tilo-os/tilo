//! Mobile — the modem's data connection and its SIMs, under Network.
//!
//! It exists only on a device with a modem: `ModemManager` lists one, or
//! `NetworkManager` or the kernel does while `ModemManager` cannot be
//! asked. The first page shows its card next to Wi-Fi; the card opens the
//! modem's page, one of the Network pane's, which shows and changes mobile data, the operator, technology and
//! signal, roaming, the APN, the preferred network types, the data used
//! this period, metering, and the SIMs: their state, the PIN and the PUK
//! after a lockout, the PIN lock, and which SIM is in use where a modem has
//! several.
//!
//! As the rest of Network, every read and change runs off the UI thread
//! through a [`MobileService`] (the device's `ModemManager` and
//! `NetworkManager`, or a scripted modem in a capture or test), and every
//! setting is a [`MobileEffect`]. PINs and PUKs are asked with the
//! window's password prompt ([`codes`]) and handed to the change in zeroed
//! buffers; they are never drawn and never reach a document.

use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use os_actions::{
    MeteredPolicy, MobileProfile, MobileReading, ModemChange, ModemError, ModemFailure, ModemInfo,
    ModemState, NetworkModes, Registration, SimCard, SimKind, SimLock, Technology, Traffic,
    UnlockRetries,
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected};
use ui::storage::{Scope, SettingsSchema};
use ui::tilo_resource::{self, ActionRef};
use ui::widget::{
    Button, ButtonRole, ListRow, Row, SignalLevel, Spacer, TextInput, TextInputKind, Widget,
};
use zeroize::Zeroizing;

use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::scenarios::MobileSample;
use crate::setting::{Follows, Read};
use crate::settings_app::SettingsApp;
use crate::widgets::{
    El, Item, SettingsSection, error_caption, field_row, note, page, row, rows, section,
    section_with, value_row,
};

crate::effects::effect_family!(MobileEffect => answer);

crate::area::pane_area! {
    MobileArea;
    fn poll(&self) {
        MobileArea::poll(self);
    }
    fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        MobileArea::statuses(self)
    }
}

/// The window's [`MobileArea`].
pub fn area(app: &SettingsApp) -> Option<&MobileArea> {
    app.areas().find()
}

/// How often the open section reads the modem again: signal and operator
/// change as the person moves.
const READ_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

/// Whether a connection's data counts against a limit, by the key the
/// metered action takes.
const METERED: &[(&str, &str)] = &[
    ("automatic", "net-mobile-metered-automatic"),
    ("metered", "net-mobile-metered-yes"),
    ("unmetered", "net-mobile-metered-no"),
];

/// The days of the month a data period can start on: every month has them.
const RESET_DAYS: std::ops::RangeInclusive<u32> = 1..=28;

/// Mobile data and the SIMs of this device's modems.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum MobileEffect {
    /// Read the modems, SIMs and the mobile data connection this section shows.
    #[effect(view, returns, kind = "read", label = "Inspect mobile state")]
    InspectMobileState,
    /// Read the modems and the mobile data connection again.
    #[effect(job, idempotent, label = "Refresh mobile data")]
    RefreshMobile,
    /// Use mobile data, or stop.
    #[effect(job, idempotent, label = "Mobile data")]
    SetMobileBroadband { on: bool },
    /// Use mobile data on other operators' networks, or only on the home
    /// network.
    #[effect(job, idempotent, label = "Data roaming")]
    SetDataRoaming { on: bool },
    /// Set the access point mobile data connects through; what is omitted is
    /// what the APN editor holds.
    #[effect(job, label = "Access point (APN)")]
    SetMobileApn {
        #[param("Choose the APN from the operator database")]
        automatic: Option<bool>,
        #[param("The access point name, such as internet.telekom")]
        apn: Option<String>,
    },
    /// Count mobile data against a limit, or let the network say.
    #[effect(job, idempotent, label = "Metered connection")]
    SetMobileMetered {
        #[param("automatic, metered or unmetered", choices = METERED.iter().map(|(key, _)| *key))]
        metered: String,
    },
    /// Choose the network types the modem may use.
    #[effect(job, idempotent, label = "Preferred network type")]
    SetPreferredNetwork {
        #[param("The network types, as listed: 4g-3g-2g, 5g-4g-prefer-5g, 4g …")]
        network: String,
    },
    /// Choose the day of the month the data used starts counting again.
    #[effect(idempotent, label = "Data period starts on")]
    SetDataResetDay {
        #[param("The day of the month, 1 to 28")]
        day: u32,
    },
    /// Start counting the data used again from now.
    #[effect(label = "Reset data used")]
    ResetDataUsage,
    /// Unlock a SIM with its PIN; without it, the PIN is asked for.
    #[effect(job, label = "Unlock SIM")]
    UnlockSim {
        #[param("The SIM, as listed; the locked one when omitted")]
        sim: Option<String>,
        #[param(secret, "The SIM's PIN")]
        pin: Option<String>,
    },
    /// Unlock a SIM blocked after too many wrong PINs with its PUK, choosing
    /// a new PIN; without both, they are asked for.
    #[effect(job, label = "Unblock SIM")]
    UnblockSim {
        #[param("The SIM, as listed; the blocked one when omitted")]
        sim: Option<String>,
        #[param(secret, "The PUK from the SIM card's package")]
        puk: Option<String>,
        #[param(secret, "The new PIN")]
        pin: Option<String>,
    },
    /// Ask for the SIM's PIN when the device starts, or stop asking; without
    /// the PIN, it is asked for.
    #[effect(job, label = "SIM PIN lock")]
    SetSimPinLock {
        #[param("The SIM, as listed; the one in use when omitted")]
        sim: Option<String>,
        on: bool,
        #[param(secret, "The SIM's PIN")]
        pin: Option<String>,
    },
    /// Change the SIM's PIN; without both PINs, they are asked for.
    #[effect(job, label = "Change SIM PIN")]
    ChangeSimPin {
        #[param("The SIM, as listed; the one in use when omitted")]
        sim: Option<String>,
        #[param(secret, "The current PIN")]
        old_pin: Option<String>,
        #[param(secret, "The new PIN")]
        new_pin: Option<String>,
    },
    /// Use this SIM for mobile data, on a modem with several.
    #[effect(job, row, label = "Use SIM")]
    UseSim {
        #[param("The SIM, as listed")]
        sim: String,
    },
    /// Open or close the APN editor.
    #[effect(view, label = "Edit APN…")]
    ShowApnEditor,
    /// Close the APN editor, forgetting what was typed.
    #[effect(view, label = "Cancel")]
    CancelMobileForm,
}

/// Answer `effect`: a change starts on a worker and reports when it lands.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: MobileEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let area = area(app).ok_or_else(crate::area::missing)?;
    if effect == MobileEffect::InspectMobileState {
        return Ok(Effected::value(area.model().borrow().inspection()));
    }
    if effect == MobileEffect::RefreshMobile {
        app.host().radios().look_again();
    }
    let begun = area.model().borrow_mut().begin(effect)?;
    Ok(match begun {
        Begun::Done => Effected::done(),
        Begun::Work(work) => {
            crate::device_read::job(cx, work, |app: &mut SettingsApp, landed, _| {
                area_of(app)?.model().borrow_mut().land(landed)
            })
        }
    })
}

fn area_of(app: &SettingsApp) -> Result<&MobileArea, String> {
    area(app).ok_or_else(crate::area::missing)
}

/// Mobile data, as search shows it: a switch once the modem was read,
/// "no modem" where there is none.
pub(crate) fn mobile_data(app: &SettingsApp, _: &Follows) -> Read {
    let listed = app.host().radios().now().map(|radios| radios.wwan);
    let model = area(app).and_then(|area| area.model.get());
    match model.map(|model| {
        let model = model.borrow();
        (model.loaded, model.present(), model.data_on())
    }) {
        Some((true, false, _)) => Read::Value(Some(t("net-no-modem"))),
        Some((true, true, on)) => Read::Switch(on),
        _ if listed == Some(false) => Read::Value(Some(t("net-no-modem"))),
        _ => Read::Value(None),
    }
}

/// The mobile data switch where search finds it.
pub(crate) fn mobile_data_switch(cx: &PaneCx, on: bool) -> Item {
    cx.row(t("net-mobile-data"))
        .id("network.mobile")
        .toggle_effect(
            &LocalState::new(on),
            effect(MobileEffect::SET_MOBILE_BROADBAND),
        )
}

/// What the Mobile section keeps for the window: the model, read when
/// first needed.
pub struct MobileArea {
    cx: PaneCx,
    model: OnceCell<Shared<MobileModel>>,
}

impl MobileArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: OnceCell::new(),
        }
    }

    /// The model over this device's modems, or over a scripted one for a
    /// capture.
    pub fn model(&self) -> &Shared<MobileModel> {
        self.model.get_or_init(|| {
            let model = if self.cx.reaches_devices() {
                MobileModel::of_device(self.cx.clone())
            } else {
                let sample = self.cx.scene().and_then(|scene| scene.mobile);
                let mut model =
                    MobileModel::loaded_from(self.cx.clone(), Arc::new(ScriptedModem::new(sample)));
                model.stage(sample);
                model
            };
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// Take in background results, if the model exists.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_background();
        }
    }

    /// Whether the device has a modem: as read, else as the kernel lists
    /// it; `None` while neither has said.
    fn present(&self) -> Option<bool> {
        if let Some(model) = self.model.get()
            && let Ok(model) = model.try_borrow()
            && model.loaded
        {
            return Some(model.present());
        }
        self.cx
            .radios()
            .now()
            .map(|radios| radios.wwan)
            .filter(|wwan| !wwan)
    }

    /// What cannot be done now: every mobile setting without a modem, and
    /// unlocking a SIM that asks for nothing.
    pub(crate) fn statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        let mut unavailable = |id: &'static str, code: &str, reason: &str| {
            statuses.push(EffectStatus::unavailable(
                ActionRef::<SettingsEffect, ()>::new(id, &[]),
                code,
                reason,
            ));
        };
        match self.present() {
            Some(false) => {
                for id in [
                    MobileEffect::SET_MOBILE_BROADBAND.id,
                    MobileEffect::SET_DATA_ROAMING.id,
                    MobileEffect::SET_MOBILE_APN.id,
                    MobileEffect::SET_MOBILE_METERED.id,
                    MobileEffect::SET_PREFERRED_NETWORK.id,
                    MobileEffect::SET_DATA_RESET_DAY.id,
                    MobileEffect::RESET_DATA_USAGE.id,
                    MobileEffect::UNLOCK_SIM.id,
                    MobileEffect::UNBLOCK_SIM.id,
                    MobileEffect::SET_SIM_PIN_LOCK.id,
                    MobileEffect::CHANGE_SIM_PIN.id,
                    MobileEffect::USE_SIM.id,
                    MobileEffect::SHOW_APN_EDITOR.id,
                ] {
                    unavailable(id, "no_modem", &t("net-no-modem"));
                }
            }
            Some(true) => {
                let locks = self.model.get().map_or_else(Vec::new, |model| {
                    model
                        .borrow()
                        .modems
                        .iter()
                        .map(|modem| modem.lock)
                        .collect()
                });
                if !locks.contains(&SimLock::Pin) {
                    unavailable(
                        MobileEffect::UNLOCK_SIM.id,
                        "not_locked",
                        &t("net-mobile-not-locked"),
                    );
                }
                if !locks.contains(&SimLock::Puk) {
                    unavailable(
                        MobileEffect::UNBLOCK_SIM.id,
                        "not_blocked",
                        &t("net-mobile-not-blocked"),
                    );
                }
            }
            None => {}
        }
        statuses
    }
}

/// The data used on this device, counted from `ModemManager`'s traffic
/// totals. Kept on this device only: another device's modem counts its own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_field_names,
    reason = "the app's device-local settings share one namespace, so each field names its area"
)]
pub struct MobileDataUsage {
    /// The day of the month the count starts again; 0 for the 1st.
    pub mobile_reset_day: u32,
    /// The month the period being counted started in, as `year * 12 +
    /// month - 1`; 0 before the first count.
    pub mobile_period: u32,
    /// Bytes used in the period.
    pub mobile_used: u64,
    /// The traffic total last read, to count what came after it.
    pub mobile_counter: Option<u64>,
    /// The boot the total was read in: totals start again at boot.
    pub mobile_boot: Option<String>,
}

impl SettingsSchema for MobileDataUsage {
    const SCOPE: Scope = Scope::DeviceLocal;
}

/// The civil date (year, month, day) of `days` since 1970-01-01.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

impl MobileDataUsage {
    /// The day of the month the period starts on.
    fn reset_day(&self) -> u32 {
        self.mobile_reset_day
            .clamp(*RESET_DAYS.start(), *RESET_DAYS.end())
    }

    /// The period `now` (seconds since the epoch, UTC) falls in, as
    /// `year * 12 + month - 1` of its first day.
    fn period_at(&self, now: i64) -> u32 {
        let (year, month, day) = civil(now.div_euclid(86_400));
        let mut index = year * 12 + i64::from(month) - 1;
        if day < self.reset_day() {
            index -= 1;
        }
        u32::try_from(index).unwrap_or(0)
    }

    /// The first day of the current period, as (month, day).
    fn period_start(&self) -> (u32, u32) {
        (self.mobile_period % 12 + 1, self.reset_day())
    }

    /// Take in a traffic total read at `now`: what was used since the last
    /// total counts toward the period, which starts again on its day. A
    /// total lower than the last, or from another boot, started again from
    /// zero. The first total ever read only starts the count. Whether
    /// anything changed.
    fn account(&mut self, total: Option<u64>, boot: Option<&str>, now: i64) -> bool {
        let before = self.clone();
        let period = self.period_at(now);
        if self.mobile_period != period {
            self.mobile_period = period;
            self.mobile_used = 0;
        }
        if let Some(total) = total {
            let rebooted = matches!((boot, self.mobile_boot.as_deref()), (Some(now), Some(then)) if now != then);
            let used = match self.mobile_counter {
                Some(seen) if !rebooted && total >= seen => total - seen,
                Some(_) => total,
                None => 0,
            };
            self.mobile_used = self.mobile_used.saturating_add(used);
            self.mobile_counter = Some(total);
            if boot.is_some() {
                self.mobile_boot = boot.map(str::to_owned);
            }
        }
        *self != before
    }
}

/// Everything the section shows, read in one pass off the UI thread.
pub(crate) struct MobileSnapshot {
    modems: Result<MobileReading, ModemError>,
    profile: Result<Option<MobileProfile>, String>,
    /// `NetworkManager`'s radios: whether it lists a modem, and its WWAN
    /// switch.
    radios: Result<os_actions::RadioState, String>,
    /// This boot's id, which tells a traffic total that started again.
    boot: Option<String>,
}

/// A change carried out off the UI thread. Codes are zeroed when it is
/// dropped.
#[derive(Debug)]
pub(crate) enum MobileChange {
    Data(bool),
    Roaming {
        uuid: Option<String>,
        on: bool,
    },
    /// `None` for the APN the operator database names.
    Apn {
        uuid: Option<String>,
        apn: Option<String>,
    },
    Metered {
        uuid: Option<String>,
        policy: MeteredPolicy,
    },
    Modem(ModemChange),
}

/// What carries out the section's reads and changes: `ModemManager` and
/// `NetworkManager` on a device, a scripted modem in a capture or test.
pub(crate) trait MobileService: Send + Sync {
    /// Read the modems, the data connection and the radios.
    fn snapshot(&self) -> MobileSnapshot;
    /// Carry out `change`.
    ///
    /// # Errors
    /// A wrong code, or why the device refused or failed it.
    fn change(&self, change: &MobileChange) -> Result<(), ModemError>;
}

/// This device's modems.
struct DeviceModem;

impl DeviceModem {
    /// The data profile to change: the one read, else one made now.
    fn profile(uuid: Option<&str>) -> Result<String, ModemError> {
        if let Some(uuid) = uuid {
            return Ok(uuid.to_owned());
        }
        os_actions::modem::mobile_profile_uuid().map_err(ModemError::Failed)
    }
}

impl MobileService for DeviceModem {
    fn snapshot(&self) -> MobileSnapshot {
        crate::device_read::off_ui_thread();
        MobileSnapshot {
            modems: os_actions::read_mobile(),
            profile: os_actions::read_mobile_profile(),
            radios: os_actions::get_radio_state(),
            boot: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .ok()
                .map(|id| id.trim().to_owned()),
        }
    }

    fn change(&self, change: &MobileChange) -> Result<(), ModemError> {
        crate::device_read::off_ui_thread();
        let nm = |result: Result<(), String>| result.map_err(ModemError::Failed);
        match change {
            MobileChange::Data(on) => nm(os_actions::set_mobile_data(*on)),
            MobileChange::Roaming { uuid, on } => nm(os_actions::set_mobile_roaming(
                &Self::profile(uuid.as_deref())?,
                *on,
            )),
            MobileChange::Apn { uuid, apn } => nm(os_actions::set_mobile_apn(
                &Self::profile(uuid.as_deref())?,
                apn.as_deref(),
            )),
            MobileChange::Metered { uuid, policy } => {
                nm(os_actions::configure_connection_policy_patch(
                    &Self::profile(uuid.as_deref())?,
                    &os_actions::ConnectionPolicyPatch {
                        metered: Some(*policy),
                        ..os_actions::ConnectionPolicyPatch::default()
                    },
                ))
            }
            MobileChange::Modem(change) => os_actions::apply_modem_change(change),
        }
    }
}

/// A scripted modem: the one a capture shows, the one tests drive. It
/// takes `1234` as its PIN and `12345678` as its PUK, and records every
/// change asked of it.
pub(crate) struct ScriptedModem {
    state: Mutex<Scripted>,
}

struct Scripted {
    reading: MobileReading,
    profile: Option<MobileProfile>,
    wwan_enabled: bool,
    changes: Vec<String>,
    refuse: Option<String>,
}

const SAMPLE_MODEM: &str = "/org/freedesktop/ModemManager1/Modem/0";
const SAMPLE_SIM: &str = "/org/freedesktop/ModemManager1/SIM/0";
const SAMPLE_ESIM: &str = "/org/freedesktop/ModemManager1/SIM/1";

/// The modem a capture of `sample` shows; none without one.
fn sample_reading(sample: Option<MobileSample>) -> MobileReading {
    let Some(sample) = sample else {
        return MobileReading::default();
    };
    let lte_first = NetworkModes {
        allowed: 0b1_1110,
        preferred: 1 << 4,
    };
    let sim = |path: &str, slot, active, operator: &str, kind| SimCard {
        path: path.into(),
        slot,
        active,
        operator: Some(operator.into()),
        operator_code: Some("26201".into()),
        iccid: None,
        kind,
    };
    let mut modem = ModemInfo {
        path: SAMPLE_MODEM.into(),
        name: "Quectel EM120R-GL".into(),
        state: ModemState::Connected,
        failure: None,
        lock: SimLock::None,
        retries: UnlockRetries {
            pin: Some(3),
            puk: Some(10),
        },
        pin_enabled: Some(true),
        signal: Some(72),
        technology: Some(Technology::Lte),
        operator: Some("Telekom.de".into()),
        registration: Registration::Home,
        modes: Some(lte_first),
        supported_modes: vec![
            lte_first,
            NetworkModes {
                allowed: 0b0_1110,
                preferred: 1 << 3,
            },
            NetworkModes {
                allowed: 1 << 3,
                preferred: 0,
            },
            NetworkModes {
                allowed: 0b0_0110,
                preferred: 1 << 2,
            },
        ],
        sims: vec![
            sim(SAMPLE_SIM, Some(1), true, "Telekom.de", SimKind::Physical),
            sim(
                SAMPLE_ESIM,
                Some(2),
                false,
                "Vodafone.de",
                SimKind::Esim {
                    profiles: Some(true),
                },
            ),
        ],
        numbers: vec!["+49 170 1234567".into()],
        traffic: Some(Traffic {
            received: 3_100_000_000,
            sent: 420_000_000,
        }),
    };
    match sample {
        MobileSample::Registered => {}
        MobileSample::Roaming => {
            modem.registration = Registration::Roaming;
            modem.operator = Some("Orange F".into());
            modem.technology = Some(Technology::Nr5g);
            modem.signal = Some(41);
        }
        MobileSample::Locked | MobileSample::Puk => {
            modem.state = ModemState::Locked;
            modem.lock = if sample == MobileSample::Puk {
                SimLock::Puk
            } else {
                SimLock::Pin
            };
            modem.retries.pin = Some(if sample == MobileSample::Puk { 0 } else { 2 });
            modem.signal = None;
            modem.technology = None;
            modem.operator = None;
            modem.registration = Registration::Idle;
            modem.traffic = None;
        }
    }
    MobileReading {
        modems: vec![modem],
    }
}

fn sample_profile(sample: Option<MobileSample>) -> Option<MobileProfile> {
    sample.map(|sample| MobileProfile {
        uuid: "6f1c3a52-7d9e-4b8a-a1f0-3c5e2d4b6a71".into(),
        name: os_actions::modem::MOBILE_PROFILE_NAME.into(),
        autoconnect: true,
        active: matches!(sample, MobileSample::Registered | MobileSample::Roaming),
        apn: None,
        automatic_apn: true,
        roaming: sample == MobileSample::Roaming,
        metered: MeteredPolicy::Metered,
    })
}

impl ScriptedModem {
    pub(crate) fn new(sample: Option<MobileSample>) -> Self {
        Self::with(sample_reading(sample), sample_profile(sample))
    }

    pub(crate) fn with(reading: MobileReading, profile: Option<MobileProfile>) -> Self {
        Self {
            state: Mutex::new(Scripted {
                wwan_enabled: !reading.modems.is_empty(),
                reading,
                profile,
                changes: Vec::new(),
                refuse: None,
            }),
        }
    }

    /// What was asked of it, in order; never a code.
    #[cfg(test)]
    pub(crate) fn changes(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.changes.clone())
            .unwrap_or_default()
    }

    /// Refuse every change that is not a code with `reason`.
    #[cfg(test)]
    pub(crate) fn refuse(&self, reason: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.refuse = Some(reason.into());
        }
    }

    fn sim_mut<'a>(reading: &'a mut MobileReading, path: &str) -> Option<&'a mut ModemInfo> {
        reading
            .modems
            .iter_mut()
            .find(|modem| modem.sims.iter().any(|sim| sim.path == path))
    }

    /// A wrong code costs a try; the last wrong PIN asks for the PUK.
    fn wrong(modem: &mut ModemInfo) -> ModemError {
        if modem.lock == SimLock::Puk {
            modem.retries.puk = modem.retries.puk.map(|left| left.saturating_sub(1));
        } else {
            let left = modem.retries.pin.unwrap_or(3).saturating_sub(1);
            modem.retries.pin = Some(left);
            if left == 0 {
                modem.lock = SimLock::Puk;
            }
        }
        ModemError::WrongCode
    }

    fn unlocked(modem: &mut ModemInfo) {
        modem.lock = SimLock::None;
        modem.state = ModemState::Registered;
        modem.retries.pin = Some(3);
        modem.registration = Registration::Home;
        modem.operator = modem.active_sim().and_then(|sim| sim.operator.clone());
        modem.signal = Some(64);
        modem.technology = Some(Technology::Lte);
    }

    fn modem_change(state: &mut Scripted, change: &ModemChange) -> Result<(), ModemError> {
        const PIN: &str = "1234";
        const PUK: &str = "12345678";
        match change {
            ModemChange::SendPin { sim, pin } => {
                let modem = Self::sim_mut(&mut state.reading, sim)
                    .ok_or_else(|| ModemError::Invalid("no such SIM".into()))?;
                if modem.lock != SimLock::Pin || pin.as_str() != PIN {
                    return Err(Self::wrong(modem));
                }
                Self::unlocked(modem);
            }
            ModemChange::SendPuk { sim, puk, .. } => {
                let modem = Self::sim_mut(&mut state.reading, sim)
                    .ok_or_else(|| ModemError::Invalid("no such SIM".into()))?;
                if puk.as_str() != PUK {
                    return Err(Self::wrong(modem));
                }
                Self::unlocked(modem);
            }
            ModemChange::SetPinLock { sim, pin, enabled } => {
                let modem = Self::sim_mut(&mut state.reading, sim)
                    .ok_or_else(|| ModemError::Invalid("no such SIM".into()))?;
                if pin.as_str() != PIN {
                    return Err(Self::wrong(modem));
                }
                modem.pin_enabled = Some(*enabled);
            }
            ModemChange::ChangePin { sim, old_pin, .. } => {
                let modem = Self::sim_mut(&mut state.reading, sim)
                    .ok_or_else(|| ModemError::Invalid("no such SIM".into()))?;
                if old_pin.as_str() != PIN {
                    return Err(Self::wrong(modem));
                }
            }
            ModemChange::SetModes { modem, modes } => {
                if let Some(modem) = state.reading.modems.iter_mut().find(|m| &m.path == modem) {
                    modem.modes = Some(*modes);
                }
            }
            ModemChange::UseSlot { modem, slot } => {
                if let Some(modem) = state.reading.modems.iter_mut().find(|m| &m.path == modem) {
                    for sim in &mut modem.sims {
                        sim.active = sim.slot == Some(*slot);
                    }
                }
            }
        }
        Ok(())
    }
}

impl MobileService for ScriptedModem {
    fn snapshot(&self) -> MobileSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        MobileSnapshot {
            modems: Ok(state.reading.clone()),
            profile: Ok(state.profile.clone()),
            radios: Ok(os_actions::RadioState {
                wifi_available: true,
                wifi_enabled: true,
                wwan_available: !state.reading.modems.is_empty(),
                wwan_enabled: state.wwan_enabled,
                ..os_actions::RadioState::default()
            }),
            boot: Some("scripted".into()),
        }
    }

    fn change(&self, change: &MobileChange) -> Result<(), ModemError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.changes.push(match change {
            MobileChange::Modem(change) => format!("{change:?}"),
            other => format!("{other:?}"),
        });
        if let MobileChange::Modem(change) = change {
            return Self::modem_change(&mut state, change);
        }
        if let Some(reason) = state.refuse.clone() {
            return Err(ModemError::Failed(reason));
        }
        let profile = state.profile.get_or_insert_with(|| MobileProfile {
            uuid: "6f1c3a52-7d9e-4b8a-a1f0-3c5e2d4b6a71".into(),
            name: os_actions::modem::MOBILE_PROFILE_NAME.into(),
            autoconnect: true,
            active: false,
            apn: None,
            automatic_apn: true,
            roaming: false,
            metered: MeteredPolicy::Automatic,
        });
        match change {
            MobileChange::Data(on) => {
                profile.autoconnect |= *on;
                profile.active = *on;
                state.wwan_enabled = *on;
            }
            MobileChange::Roaming { on, .. } => profile.roaming = *on,
            MobileChange::Apn { apn, .. } => {
                profile.automatic_apn = apn.is_none();
                profile.apn.clone_from(apn);
            }
            MobileChange::Metered { policy, .. } => profile.metered = *policy,
            MobileChange::Modem(_) => {}
        }
        drop(state);
        Ok(())
    }
}

/// How an effect began: finished at once, or work for a worker.
pub(crate) enum Begun {
    Done,
    Work(Box<dyn FnOnce() -> Landed + Send>),
}

/// Work that finished on a worker, for the model to take in.
pub(crate) enum Landed {
    Read(MobileSnapshot),
    Changed(MobileChange, Result<(), ModemError>),
}

/// The Mobile section's model: the modems and the data connection as last
/// read, the code asked for, and what is running.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: read yet, listed, switched on, editor open, changed"
)]
pub struct MobileModel {
    cx: PaneCx,
    service: Arc<dyn MobileService>,
    /// False until the first snapshot arrives.
    loaded: bool,
    snapshot: Option<crate::channel::Receiver<MobileSnapshot>>,
    modems: Vec<ModemInfo>,
    modems_error: Option<ModemError>,
    profile: Option<MobileProfile>,
    profile_error: Option<String>,
    /// Whether `NetworkManager` lists a modem.
    wwan_listed: bool,
    /// `NetworkManager`'s WWAN switch: whether mobile data may be used.
    wwan_enabled: bool,
    boot: Option<String>,
    usage: MobileDataUsage,
    in_flight: usize,
    notice: Option<String>,
    /// The SIM code the password prompt asks for.
    asking: Option<codes::Asking>,
    /// Changes a prompt's answer started; they land here when done.
    jobs: Vec<crate::channel::Receiver<Landed>>,
    apn_open: bool,
    apn_automatic: LocalState<bool>,
    apn_text: LocalState<String>,
    /// When the modems were last read, to read them again every
    /// [`READ_EVERY`] while the section is shown.
    read_at: Option<ui::time::Instant>,
    changed: bool,
}

impl MobileModel {
    fn of_device(cx: PaneCx) -> Self {
        let mut model = Self::with_service(cx, Arc::new(DeviceModem));
        model.start_snapshot();
        model
    }

    /// A model over `service`, its first snapshot read at once.
    pub(crate) fn loaded_from(cx: PaneCx, service: Arc<dyn MobileService>) -> Self {
        let mut model = Self::with_service(cx, service);
        let snapshot = model.service.snapshot();
        model.adopt(snapshot);
        model
    }

    fn with_service(cx: PaneCx, service: Arc<dyn MobileService>) -> Self {
        let usage = cx.settings::<MobileDataUsage>().get();
        Self {
            cx,
            service,
            loaded: false,
            snapshot: None,
            modems: Vec::new(),
            modems_error: None,
            profile: None,
            profile_error: None,
            wwan_listed: false,
            wwan_enabled: false,
            boot: None,
            usage,
            in_flight: 0,
            notice: None,
            asking: None,
            jobs: Vec::new(),
            apn_open: false,
            apn_automatic: LocalState::new(true),
            apn_text: LocalState::new(String::new()),
            read_at: None,
            changed: false,
        }
    }

    /// Where a capture of `sample` stands: abroad on the modem's page, and
    /// a locked SIM's page with the prompt asking for its code.
    fn stage(&mut self, sample: Option<MobileSample>) {
        let Some(sample) = sample.filter(|sample| *sample != MobileSample::Registered) else {
            return;
        };
        self.cx.pages().open(
            crate::categories::Category::Network,
            super::PAGE_MOBILE,
            t("net-mobile"),
        );
        let modem = self.modem().cloned();
        match sample {
            MobileSample::Locked => self.ask_unlock(None, modem.as_ref()),
            MobileSample::Puk => self.ask_unblock(None, modem.as_ref()),
            MobileSample::Registered | MobileSample::Roaming => {}
        }
    }

    /// Whether this device has a modem: `ModemManager` lists one, or
    /// `NetworkManager` or the kernel does while `ModemManager` cannot be
    /// asked (the section then says why it cannot show the modem).
    fn present(&self) -> bool {
        !self.modems.is_empty()
            || (self.modems_error.is_some()
                && (self.wwan_listed || self.cx.radios().now().is_some_and(|radios| radios.wwan)))
    }

    /// The modem the section shows: the one with a SIM in use, else the
    /// first.
    fn modem(&self) -> Option<&ModemInfo> {
        self.modems
            .iter()
            .find(|modem| modem.failure.is_none())
            .or_else(|| self.modems.first())
    }

    /// Whether mobile data is on: the WWAN switch on, and the profile (if
    /// any) connecting by itself.
    fn data_on(&self) -> bool {
        self.wwan_enabled
            && self
                .profile
                .as_ref()
                .is_none_or(|profile| profile.autoconnect)
    }

    fn start_snapshot(&mut self) {
        let (sender, receiver) = crate::channel::channel();
        let service = Arc::clone(&self.service);
        let wake = ui::WakeHandle::new();
        let started = crate::device_read::thread(
            "settings-mobile-read",
            Box::new(move || {
                if sender.send(service.snapshot()).is_ok() {
                    wake.wake();
                }
            }),
        );
        match started {
            Ok(()) => {
                self.snapshot = Some(receiver);
                self.read_at = Some(ui::time::now());
            }
            Err(error) => {
                self.notice = Some(t_args(
                    "net-mobile-read-err",
                    &[("error", error.to_string().into())],
                ));
            }
        }
        self.changed = true;
    }

    fn adopt(&mut self, snapshot: MobileSnapshot) {
        self.changed = true;
        if self.loaded && self.in_flight > 0 {
            // Read before a running change landed; the read after it tells.
            return;
        }
        match snapshot.modems {
            Ok(reading) => {
                self.modems = reading.modems;
                self.modems_error = None;
            }
            Err(error) => {
                warn!(target: "tilo::settings", %error, "mobile broadband read failed");
                self.modems.clear();
                self.modems_error = Some(error);
            }
        }
        match snapshot.profile {
            Ok(profile) => {
                self.profile = profile;
                self.profile_error = None;
            }
            Err(error) => self.profile_error = Some(error),
        }
        if let Ok(radios) = snapshot.radios {
            self.wwan_listed = radios.wwan_available;
            self.wwan_enabled = radios.wwan_enabled;
        }
        self.boot = snapshot.boot;
        self.count_usage();
        self.loaded = true;
        self.follow_lock();
    }

    /// Count the traffic just read toward the period, and keep the count.
    fn count_usage(&mut self) {
        let total = self
            .modems
            .iter()
            .filter_map(|modem| modem.traffic)
            .map(Traffic::total)
            .reduce(u64::saturating_add);
        let now = tilo_clock::unix_now_i64();
        let mut usage = self.usage.clone();
        if usage.account(total, self.boot.as_deref(), now) {
            self.usage = usage.clone();
            if let Err(error) = self.cx.update::<MobileDataUsage>(|stored| *stored = usage) {
                warn!(target: "tilo::settings", %error, "the mobile data count could not be kept");
            }
        }
    }

    /// Start `change` on a worker.
    fn change(&mut self, change: MobileChange) -> Begun {
        self.in_flight += 1;
        let service = Arc::clone(&self.service);
        Begun::Work(Box::new(move || {
            let result = service.change(&change);
            Landed::Changed(change, result)
        }))
    }

    /// The SIM `wanted` names (its path or label), else the one `fallback`
    /// picks among the modems.
    fn sim(
        &self,
        wanted: Option<&str>,
        fallback: impl Fn(&ModemInfo) -> bool,
    ) -> Result<(String, String), String> {
        if let Some(wanted) = wanted {
            return self
                .modems
                .iter()
                .find_map(|modem| {
                    modem
                        .sims
                        .iter()
                        .find(|sim| sim.path == wanted || sim.label() == wanted)
                        .map(|sim| (modem.path.clone(), sim.path.clone()))
                })
                .ok_or_else(|| t_args("net-mobile-no-such-sim", &[("sim", wanted.into())]));
        }
        self.modems
            .iter()
            .filter(|modem| fallback(modem))
            .find_map(|modem| {
                modem
                    .active_sim()
                    .map(|sim| (modem.path.clone(), sim.path.clone()))
            })
            .ok_or_else(|| t("net-mobile-no-sim"))
    }

    fn require_modem(&self) -> Result<&ModemInfo, String> {
        self.modem().ok_or_else(|| t("net-no-modem"))
    }

    /// The network types `chosen` names (their key or their words) among
    /// what the modem supports.
    fn modes_named(modem: &ModemInfo, chosen: &str) -> Option<NetworkModes> {
        modem
            .supported_modes
            .iter()
            .copied()
            .find(|modes| modes.key() == chosen || modes_shown(*modes) == chosen)
    }

    /// Begin `effect`.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per effect; the longer ones call out"
    )]
    pub(crate) fn begin(&mut self, effect: MobileEffect) -> Result<Begun, String> {
        self.changed = true;
        self.notice = None;
        let uuid = self.profile.as_ref().map(|profile| profile.uuid.clone());
        Ok(match effect {
            MobileEffect::InspectMobileState => {
                return Err("Mobile inspection is a read-only resource operation.".into());
            }
            MobileEffect::RefreshMobile => {
                let service = Arc::clone(&self.service);
                Begun::Work(Box::new(move || Landed::Read(service.snapshot())))
            }
            MobileEffect::SetMobileBroadband { on } => {
                self.require_modem()?;
                self.wwan_enabled = on;
                if let Some(profile) = &mut self.profile {
                    profile.autoconnect |= on;
                }
                self.change(MobileChange::Data(on))
            }
            MobileEffect::SetDataRoaming { on } => {
                self.require_modem()?;
                if let Some(profile) = &mut self.profile {
                    profile.roaming = on;
                }
                self.change(MobileChange::Roaming { uuid, on })
            }
            MobileEffect::SetMobileApn { automatic, apn } => {
                self.require_modem()?;
                let automatic = automatic.unwrap_or_else(|| self.apn_automatic.get());
                let apn = if automatic {
                    None
                } else {
                    let apn = apn.unwrap_or_else(|| self.apn_text.get());
                    let apn = apn.trim().to_owned();
                    if apn.is_empty() {
                        return Err(t("net-mobile-apn-empty"));
                    }
                    Some(apn)
                };
                self.apn_open = false;
                self.change(MobileChange::Apn { uuid, apn })
            }
            MobileEffect::SetMobileMetered { metered } => {
                self.require_modem()?;
                let policy = metered_named(&metered).ok_or_else(|| {
                    t_args("net-mobile-unknown-choice", &[("choice", metered.into())])
                })?;
                if let Some(profile) = &mut self.profile {
                    profile.metered = policy;
                }
                self.change(MobileChange::Metered { uuid, policy })
            }
            MobileEffect::SetPreferredNetwork { network } => {
                let modem = self.require_modem()?;
                let chosen = Self::modes_named(modem, &network).ok_or_else(|| {
                    t_args("net-mobile-unknown-choice", &[("choice", network.into())])
                })?;
                let path = modem.path.clone();
                self.change(MobileChange::Modem(ModemChange::SetModes {
                    modem: path,
                    modes: chosen,
                }))
            }
            MobileEffect::SetDataResetDay { day } => {
                if !RESET_DAYS.contains(&day) {
                    return Err(t("net-mobile-reset-day-range"));
                }
                let now = tilo_clock::unix_now_i64();
                let mut usage = self.usage.clone();
                usage.mobile_reset_day = day;
                // The count so far stays; the period it belongs to follows
                // the new day.
                usage.mobile_period = usage.period_at(now);
                self.store_usage(usage)?;
                Begun::Done
            }
            MobileEffect::ResetDataUsage => {
                let mut usage = self.usage.clone();
                usage.mobile_used = 0;
                self.store_usage(usage)?;
                Begun::Done
            }
            MobileEffect::UnlockSim { sim, pin } => {
                let Some(pin) = pin.map(Zeroizing::new) else {
                    self.require_modem()?;
                    self.ask_unlock(sim, self.modem().cloned().as_ref());
                    return Ok(Begun::Done);
                };
                let (_, sim) = self.sim(sim.as_deref(), |modem| modem.lock == SimLock::Pin)?;
                self.change(MobileChange::Modem(ModemChange::SendPin { sim, pin }))
            }
            MobileEffect::UnblockSim { sim, puk, pin } => {
                let (Some(puk), Some(new_pin)) = (puk.map(Zeroizing::new), pin.map(Zeroizing::new))
                else {
                    self.require_modem()?;
                    self.ask_unblock(sim, self.modem().cloned().as_ref());
                    return Ok(Begun::Done);
                };
                let (_, sim) = self.sim(sim.as_deref(), |modem| modem.lock == SimLock::Puk)?;
                self.change(MobileChange::Modem(ModemChange::SendPuk {
                    sim,
                    puk,
                    new_pin,
                }))
            }
            MobileEffect::SetSimPinLock { sim, on, pin } => {
                let Some(pin) = pin.map(Zeroizing::new) else {
                    self.require_modem()?;
                    self.ask_pin_lock(sim, on, self.modem().cloned().as_ref());
                    return Ok(Begun::Done);
                };
                let (_, sim) = self.sim(sim.as_deref(), ModemInfo::usable)?;
                self.change(MobileChange::Modem(ModemChange::SetPinLock {
                    sim,
                    pin,
                    enabled: on,
                }))
            }
            MobileEffect::ChangeSimPin {
                sim,
                old_pin,
                new_pin,
            } => {
                let (Some(old_pin), Some(new_pin)) =
                    (old_pin.map(Zeroizing::new), new_pin.map(Zeroizing::new))
                else {
                    self.require_modem()?;
                    self.ask_change_pin(sim);
                    return Ok(Begun::Done);
                };
                let (_, sim) = self.sim(sim.as_deref(), ModemInfo::usable)?;
                self.change(MobileChange::Modem(ModemChange::ChangePin {
                    sim,
                    old_pin,
                    new_pin,
                }))
            }
            MobileEffect::UseSim { sim } => {
                let (modem, path) = self.sim(Some(&sim), |_| true)?;
                let slot = self
                    .modems
                    .iter()
                    .flat_map(|modem| &modem.sims)
                    .find(|card| card.path == path)
                    .and_then(|card| card.slot)
                    .ok_or_else(|| t("net-mobile-single-slot"))?;
                self.change(MobileChange::Modem(ModemChange::UseSlot { modem, slot }))
            }
            MobileEffect::ShowApnEditor => {
                self.apn_open = !self.apn_open;
                if self.apn_open {
                    let profile = self.profile.as_ref();
                    self.apn_automatic
                        .set(profile.is_none_or(|profile| profile.apn.is_none()));
                    self.apn_text.set(
                        profile
                            .and_then(|profile| profile.apn.clone())
                            .unwrap_or_default(),
                    );
                }
                Begun::Done
            }
            MobileEffect::CancelMobileForm => {
                self.apn_open = false;
                Begun::Done
            }
        })
    }

    fn store_usage(&mut self, usage: MobileDataUsage) -> Result<(), String> {
        self.cx
            .update::<MobileDataUsage>(|stored| *stored = usage.clone())?;
        self.usage = usage;
        Ok(())
    }

    /// Take in work that landed; what the effect answers.
    pub(crate) fn land(&mut self, landed: Landed) -> EffectAnswer {
        self.changed = true;
        match landed {
            Landed::Read(snapshot) => {
                self.adopt(snapshot);
                Ok(Effected::done())
            }
            Landed::Changed(change, result) => {
                self.in_flight = self.in_flight.saturating_sub(1);
                let reported = self.report(&change, result);
                self.settle_codes(&reported);
                if self.in_flight == 0 {
                    self.start_snapshot();
                }
                reported.map(|()| Effected::done())
            }
        }
    }

    /// Say how `change` went. A wrong code is said in the prompt that asked
    /// for it; the read that follows every change puts back what the device
    /// holds.
    fn report(
        &mut self,
        change: &MobileChange,
        result: Result<(), ModemError>,
    ) -> Result<(), String> {
        match result {
            Ok(()) => {
                self.notice = match change {
                    MobileChange::Modem(
                        ModemChange::SendPin { .. } | ModemChange::SendPuk { .. },
                    ) => Some(t("net-mobile-unlocked")),
                    MobileChange::Modem(ModemChange::ChangePin { .. }) => {
                        Some(t("net-mobile-pin-changed"))
                    }
                    _ => None,
                };
                Ok(())
            }
            Err(ModemError::WrongCode) => {
                warn!(target: "tilo::settings", op = ?change, "a SIM code was refused");
                Err(t(match change {
                    MobileChange::Modem(ModemChange::SendPuk { .. }) => "net-mobile-wrong-puk",
                    _ => "net-mobile-wrong-pin",
                }))
            }
            Err(error) => {
                warn!(target: "tilo::settings", op = ?change, %error, "a mobile broadband change failed");
                let said = t_args(
                    "net-mobile-change-err",
                    &[("error", error.to_string().into())],
                );
                self.notice = Some(said.clone());
                Err(said)
            }
        }
    }

    /// Run `work`, which a prompt's answer started, on a worker; it lands
    /// in [`Self::poll_background`].
    fn start_job(&mut self, work: Box<dyn FnOnce() -> Landed + Send>) {
        let (sender, receiver) = crate::channel::channel();
        let wake = ui::WakeHandle::new();
        let started = crate::device_read::thread(
            "settings-mobile-code",
            Box::new(move || {
                if sender.send(work()).is_ok() {
                    wake.wake();
                }
            }),
        );
        match started {
            Ok(()) => self.jobs.push(receiver),
            Err(error) => {
                self.in_flight = self.in_flight.saturating_sub(1);
                let said = t_args(
                    "net-mobile-change-err",
                    &[("error", error.to_string().into())],
                );
                self.settle_codes(&Err(said));
            }
        }
        self.changed = true;
    }

    /// Take in what arrived: a read, and changes a prompt started.
    fn poll_background(&mut self) {
        let mut landed = Vec::new();
        self.jobs.retain(|job| match job.try_recv() {
            Ok(work) => {
                landed.push(work);
                false
            }
            Err(crate::channel::TryRecvError::Empty) => true,
            Err(crate::channel::TryRecvError::Disconnected) => false,
        });
        for work in landed {
            // What went wrong is said in the prompt and the section.
            let _ = self.land(work);
        }
        if let Some(receiver) = &self.snapshot {
            match receiver.try_recv() {
                Ok(snapshot) => {
                    self.snapshot = None;
                    self.adopt(snapshot);
                }
                Err(crate::channel::TryRecvError::Disconnected) => {
                    self.snapshot = None;
                    self.changed = true;
                }
                Err(crate::channel::TryRecvError::Empty) => {}
            }
        }
    }

    /// Read again every [`READ_EVERY`] while a modem is shown and nothing
    /// runs.
    fn read_when_due(&mut self, now: ui::time::Instant) {
        let due = self.loaded
            && self.present()
            && self.cx.reaches_devices()
            && self.snapshot.is_none()
            && self.in_flight == 0
            && self
                .read_at
                .is_none_or(|at| now.saturating_duration_since(at) >= READ_EVERY);
        if due {
            self.start_snapshot();
        }
    }

    /// The state the section shows, for agents and scripts: never a code.
    fn inspection(&self) -> serde_json::Value {
        let modems: Vec<serde_json::Value> = self
            .modems
            .iter()
            .map(|modem| {
                serde_json::json!({
                    "modem": modem.path, "name": modem.name,
                    "state": format!("{:?}", modem.state).to_lowercase(),
                    "failure": modem.failure.map(|failure| format!("{failure:?}").to_lowercase()),
                    "sim_lock": format!("{:?}", modem.lock).to_lowercase(),
                    "pin_tries_left": modem.retries.pin, "puk_tries_left": modem.retries.puk,
                    "pin_lock": modem.pin_enabled,
                    "operator": modem.operator, "technology": modem.technology.map(Technology::name),
                    "signal_percent": modem.signal,
                    "registration": format!("{:?}", modem.registration).to_lowercase(),
                    "network": modem.modes.map(NetworkModes::key),
                    "networks": modem.supported_modes.iter().map(|modes| modes.key()).collect::<Vec<_>>(),
                    "sims": modem.sims.iter().map(|sim| serde_json::json!({
                        "sim": sim.path, "label": sim.label(), "slot": sim.slot, "in_use": sim.active,
                        "esim": matches!(sim.kind, SimKind::Esim { .. }),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({
            "source": if self.cx.reaches_devices() { "device" } else { "preview" },
            "loaded": self.loaded, "present": self.loaded.then(|| self.present()),
            "data": self.loaded.then(|| self.data_on()),
            "roaming": self.profile.as_ref().map(|profile| profile.roaming),
            "apn": self.profile.as_ref().map(|profile| profile.apn.clone().unwrap_or_else(|| "automatic".into())),
            "metered": self.profile.as_ref().map(|profile| metered_key(profile.metered)),
            "data_used_bytes": self.usage.mobile_used,
            "data_period_starts_on_day": self.usage.reset_day(),
            "modems": modems,
            "errors": { "modems": self.modems_error.as_ref().map(ToString::to_string), "profile": self.profile_error },
            "changes_pending": self.in_flight,
        })
    }
}

fn metered_key(policy: MeteredPolicy) -> &'static str {
    match policy {
        MeteredPolicy::Automatic => "automatic",
        MeteredPolicy::Metered => "metered",
        MeteredPolicy::Unmetered => "unmetered",
    }
}

/// The policy `chosen` names: its key or its words.
fn metered_named(chosen: &str) -> Option<MeteredPolicy> {
    let key = METERED
        .iter()
        .find(|(key, words)| *key == chosen || t(words) == chosen)
        .map(|(key, _)| *key)?;
    Some(match key {
        "metered" => MeteredPolicy::Metered,
        "unmetered" => MeteredPolicy::Unmetered,
        _ => MeteredPolicy::Automatic,
    })
}

fn metered_shown(policy: MeteredPolicy) -> String {
    let key = metered_key(policy);
    METERED
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, words)| t(words))
        .unwrap_or_default()
}

/// Network types in words: "2G to 5G, 5G preferred".
fn modes_shown(modes: NetworkModes) -> String {
    let generations: Vec<String> = modes
        .generations()
        .iter()
        .map(|generation| format!("{generation}G"))
        .collect();
    let allowed = match generations.as_slice() {
        [] => t("net-mobile-network-any"),
        [only] => t_args(
            "net-mobile-network-only",
            &[("network", only.as_str().into())],
        ),
        [fastest, .., slowest]
            if modes.generations().windows(2).all(
                |pair| matches!(pair, [faster, slower] if *faster == slower.saturating_add(1)),
            ) =>
        {
            t_args(
                "net-mobile-network-range",
                &[
                    ("fastest", fastest.as_str().into()),
                    ("slowest", slowest.as_str().into()),
                ],
            )
        }
        many => many.join(", "),
    };
    match modes.preferred_generation() {
        Some(preferred) if modes.generations().len() > 1 => t_args(
            "net-mobile-network-preferred",
            &[
                ("networks", allowed.into()),
                ("preferred", format!("{preferred}G").into()),
            ],
        ),
        _ => allowed,
    }
}

/// The shared signal meter's level for `percent`, at the same steps as
/// Wi-Fi's.
const fn signal_level(percent: u8) -> SignalLevel {
    SignalLevel::from_bars(match percent {
        0 => 0,
        1..=25 => 1,
        26..=50 => 2,
        51..=75 => 3,
        _ => 4,
    })
}

/// What the modem's situation is called, as the status row's title.
fn situation(modem: &ModemInfo, data_on: bool) -> String {
    if let Some(failure) = modem.failure {
        return t(match failure {
            ModemFailure::SimMissing => "net-mobile-no-sim-card",
            ModemFailure::SimError => "net-mobile-sim-error",
            ModemFailure::EsimWithoutProfiles => "net-mobile-esim-empty",
            ModemFailure::Other => "net-mobile-modem-failed",
        });
    }
    match modem.lock {
        SimLock::Pin => return t("net-mobile-sim-locked"),
        SimLock::Puk => return t("net-mobile-sim-blocked"),
        SimLock::Other => return t("net-mobile-carrier-locked"),
        SimLock::None => {}
    }
    match modem.registration {
        Registration::Home | Registration::Roaming => modem
            .operator
            .clone()
            .unwrap_or_else(|| t("net-mobile-registered")),
        Registration::Searching => t("net-mobile-searching"),
        Registration::Denied => t("net-mobile-denied"),
        Registration::EmergencyOnly => t("net-mobile-emergency-only"),
        Registration::Idle | Registration::Unknown if !data_on => t("net-mobile-data-off"),
        Registration::Idle | Registration::Unknown => t("net-mobile-not-registered"),
    }
}

impl MobileModel {
    fn button(&self, id: &str, label: &str, effect: MobileEffect) -> Button {
        self.cx.bind_invocation(Button::new(id, label), effect)
    }

    fn buttons(buttons: Vec<Button>) -> Item {
        row(Box::new(
            buttons
                .into_iter()
                .fold(Row::new(), |row, button| row.push(Box::new(button)))
                .push(Box::new(Spacer::new())),
        ))
    }

    /// What a locked SIM needs: its PIN, or after too many wrong PINs its
    /// PUK and a new PIN, which the password prompt asks for.
    fn unlock_rows(&self, modem: &ModemInfo) -> Vec<Item> {
        let tries = |left: Option<u32>| {
            left.map(|left| t_args("net-mobile-tries-left", &[("count", left.into())]))
                .unwrap_or_default()
        };
        let (explain, left, id, label, effect) = match modem.lock {
            SimLock::Pin => (
                "net-mobile-pin-explain",
                modem.retries.pin,
                "settings.network.mobile.unlock",
                "net-unlock-sim-ask",
                MobileEffect::UnlockSim {
                    sim: None,
                    pin: None,
                },
            ),
            SimLock::Puk => (
                "net-mobile-puk-explain",
                modem.retries.puk,
                "settings.network.mobile.unblock",
                "net-mobile-unblock-ask",
                MobileEffect::UnblockSim {
                    sim: None,
                    puk: None,
                    pin: None,
                },
            ),
            SimLock::Other => return vec![note(&t("net-mobile-carrier-locked-explain"))],
            SimLock::None => return Vec::new(),
        };
        vec![
            note(format!("{} {}", t(explain), tries(left)).trim()),
            Self::buttons(vec![
                self.button(id, &t(label), effect)
                    .role(ButtonRole::Primary)
                    .with_enabled(self.in_flight == 0),
            ]),
        ]
    }

    /// The status line: operator or situation, led by the signal meter,
    /// with technology and roaming.
    fn status_row(&self, modem: &ModemInfo) -> ListRow {
        let mut details: Vec<String> = Vec::new();
        if modem.usable() {
            if let Some(technology) = modem.technology {
                details.push(technology.name().to_owned());
            }
            if modem.registration == Registration::Roaming {
                details.push(t("net-mobile-roaming"));
            }
            if modem.state == ModemState::Connected && self.data_on() {
                details.push(t("net-mobile-connected"));
            }
        }
        let mut status = ListRow::new(
            "settings.network.mobile.status",
            situation(modem, self.data_on()),
        );
        if !details.is_empty() {
            status = status.secondary(details.join(", "));
        }
        if let Some(signal) = modem.signal.filter(|_| modem.usable()) {
            status = status.signal(signal_level(signal));
        }
        status
    }

    /// Roaming, metering, network types, APN and the data used.
    fn data_rows(&self, modem: &ModemInfo) -> Vec<Item> {
        let mut items = Vec::new();
        let profile = self.profile.as_ref();
        items.push(
            self.cx
                .row(t("net-mobile-roaming-setting"))
                .id("network.mobile.roaming")
                .described(t("net-mobile-roaming-caption"))
                .toggle_effect(
                    &LocalState::new(profile.is_some_and(|profile| profile.roaming)),
                    effect(MobileEffect::SET_DATA_ROAMING),
                ),
        );
        let metered = profile.map_or(MeteredPolicy::Automatic, |profile| profile.metered);
        items.push(
            self.cx
                .row(t("net-mobile-metered"))
                .id("network.mobile.metered")
                .described(t("net-mobile-metered-caption"))
                .choice_effect(
                    METERED.iter().map(|(_, words)| t(words)),
                    &LocalState::new(metered_shown(metered)),
                    effect(MobileEffect::SET_MOBILE_METERED),
                ),
        );
        if modem.supported_modes.len() > 1 {
            let current = modem.modes.map(modes_shown).unwrap_or_default();
            items.push(
                self.cx
                    .row(t("net-mobile-network-type"))
                    .id("network.mobile.network-type")
                    .choice_effect(
                        modem.supported_modes.iter().copied().map(modes_shown),
                        &LocalState::new(current),
                        effect(MobileEffect::SET_PREFERRED_NETWORK),
                    ),
            );
        }
        items.extend(self.apn_rows());
        items.extend(self.usage_rows());
        items
    }

    fn apn_rows(&self) -> Vec<Item> {
        let shown = self
            .profile
            .as_ref()
            .and_then(|profile| profile.apn.clone())
            .unwrap_or_else(|| t("net-mobile-apn-automatic"));
        let mut items = vec![row(Box::new(
            ListRow::new("settings.network.mobile.apn", t("net-apn"))
                .secondary(shown)
                .trailing(Box::new(self.button(
                    "settings.network.mobile.apn.edit",
                    &t("net-mobile-apn-edit"),
                    MobileEffect::ShowApnEditor,
                ))),
        ))];
        if self.apn_open {
            items.push(
                self.cx
                    .row(t("net-mobile-apn-auto"))
                    .id("network.mobile.apn.automatic")
                    .described(t("net-mobile-apn-auto-caption"))
                    .control(Box::new(
                        ui::widget::Toggle::from_state(&self.apn_automatic)
                            .with_id("network.mobile.apn.automatic.toggle"),
                    )),
            );
            if !self.apn_automatic.get() {
                items.push(field_row(
                    &t("net-apn"),
                    Box::new(
                        TextInput::from_state(&self.apn_text)
                            .with_id("network.mobile.apn.name")
                            .with_kind(TextInputKind::Code)
                            .with_placeholder(t("net-mobile-apn-placeholder")),
                    ),
                ));
            }
            items.push(Self::buttons(vec![
                self.button(
                    "settings.network.mobile.apn.save",
                    &t("net-mobile-save"),
                    MobileEffect::SetMobileApn {
                        automatic: None,
                        apn: None,
                    },
                )
                .role(ButtonRole::Primary),
                self.button(
                    "settings.network.mobile.apn.cancel",
                    &t("net-mobile-cancel"),
                    MobileEffect::CancelMobileForm,
                ),
            ]));
        }
        items
    }

    fn usage_rows(&self) -> Vec<Item> {
        let (month, day) = self.usage.period_start();
        let used = t_args(
            "net-mobile-used",
            &[
                ("used", ui::format::size(self.usage.mobile_used).into()),
                ("month", ui::format::month_short(month).into()),
                ("day", day.into()),
            ],
        );
        vec![
            row(Box::new(
                ListRow::new("settings.network.mobile.used", t("net-mobile-data-used"))
                    .secondary(used)
                    .trailing(Box::new(self.button(
                        "settings.network.mobile.used.reset",
                        &t("net-mobile-used-reset"),
                        MobileEffect::ResetDataUsage,
                    ))),
            )),
            self.cx
                .row(t("net-mobile-reset-day"))
                .id("network.mobile.reset-day")
                .choice_effect(
                    RESET_DAYS.map(|day| day.to_string()),
                    &LocalState::new(self.usage.reset_day().to_string()),
                    effect(MobileEffect::SET_DATA_RESET_DAY)
                        .map(|day: String| day.parse::<u32>().unwrap_or(1)),
                ),
        ]
    }

    /// The SIMs, the number, the PIN lock and changing the PIN.
    fn sim_rows(&self, modem: &ModemInfo) -> Vec<Item> {
        let mut items = Vec::new();
        let kind = |sim: &SimCard| match sim.kind {
            SimKind::Esim {
                profiles: Some(false),
            } => t("net-mobile-esim-no-profile"),
            SimKind::Esim { .. } => t("net-mobile-esim"),
            SimKind::Physical | SimKind::Unknown => t("net-mobile-sim-card"),
        };
        if modem.sims.len() > 1 {
            for sim in &modem.sims {
                let mut details = vec![kind(sim)];
                if let Some(slot) = sim.slot {
                    details.push(t_args("net-mobile-slot", &[("slot", slot.into())]));
                }
                let mut entry = ListRow::new(
                    format!("settings.network.mobile.sim:{}", sim.path),
                    sim.label(),
                )
                .secondary(details.join(", "));
                if sim.active {
                    entry = entry.value(t("net-mobile-in-use"));
                } else {
                    entry = entry.trailing(Box::new(
                        self.button(
                            &format!("settings.network.mobile.use:{}", sim.path),
                            &t("net-mobile-use-sim"),
                            MobileEffect::UseSim {
                                sim: sim.path.clone(),
                            },
                        )
                        .with_enabled(self.in_flight == 0),
                    ));
                }
                items.push(row(Box::new(entry)));
            }
        } else if let Some(sim) = modem.active_sim() {
            items.push(value_row(
                &t("net-sim"),
                &format!("{}, {}", sim.label(), kind(sim)),
            ));
        }
        if let Some(number) = modem.numbers.first() {
            items.push(value_row(&t("net-mobile-number"), number));
        }
        if modem.usable() {
            items.extend(self.pin_rows(modem));
        }
        items
    }

    /// The PIN lock and changing the PIN; the password prompt asks for the
    /// codes either needs.
    fn pin_rows(&self, modem: &ModemInfo) -> Vec<Item> {
        let mut items = Vec::new();
        if let Some(enabled) = modem.pin_enabled {
            let shown = match self.asked() {
                Some(codes::CodeAsk::PinLock { on, .. }) => *on,
                _ => enabled,
            };
            items.push(
                self.cx
                    .row(t("net-mobile-pin-lock"))
                    .id("network.mobile.pin-lock")
                    .described(t("net-mobile-pin-lock-caption"))
                    .toggle_effect(
                        &LocalState::new(shown),
                        effect(MobileEffect::SET_SIM_PIN_LOCK)
                            .map(|on: bool| (None::<String>, on, None::<String>)),
                    ),
            );
        }
        if modem.pin_enabled == Some(true) {
            items.push(Self::buttons(vec![
                self.button(
                    "settings.network.mobile.change-pin",
                    &t("net-mobile-change-pin-open"),
                    MobileEffect::ChangeSimPin {
                        sim: None,
                        old_pin: None,
                        new_pin: None,
                    },
                )
                .with_enabled(self.in_flight == 0),
            ]));
        }
        items
    }

    /// The mobile data switch, which a modem that cannot connect holds off.
    fn data_switch(&self, modem: &ModemInfo) -> El {
        Box::new(
            self.cx.bind(
                ui::widget::Toggle::from_state(&LocalState::new(self.data_on()))
                    .with_id("network.mobile")
                    .with_enabled(modem.usable()),
                effect(MobileEffect::SET_MOBILE_BROADBAND),
            ),
        )
    }

    /// The first page's Mobile card, next to Wi-Fi: the data switch, how
    /// the modem stands (its row opens the modem's page), and what a
    /// locked SIM needs. Nothing without a modem.
    pub(crate) fn card(&self) -> Vec<SettingsSection> {
        if !self.loaded || !self.present() {
            return Vec::new();
        }
        let Some(modem) = self.modem() else {
            return self.page_sections();
        };
        let mut status = self.status_row(modem).disclosure();
        status = match self.cx.invocation_binding(super::NetworkEffect::ShowMobile) {
            Some(binding) => status.on_action(binding),
            None => status.with_enabled(false),
        };
        let mut items = vec![row(Box::new(status))];
        items.extend(self.unlock_rows(modem));
        if let Some(notice) = &self.notice {
            items.push(note(notice));
        }
        vec![section_with(
            &t("net-mobile"),
            self.data_switch(modem),
            rows(items),
        )]
    }

    /// The modem's page: Mobile data with its switch and settings, then
    /// the SIM.
    pub(crate) fn page_sections(&self) -> Vec<SettingsSection> {
        if !self.loaded || !self.present() {
            return Vec::new();
        }
        let Some(modem) = self.modem() else {
            // A modem listed that ModemManager cannot describe.
            let reason = self
                .modems_error
                .as_ref()
                .map_or_else(String::new, ToString::to_string);
            return vec![section(
                &t("net-mobile"),
                rows(vec![row(error_caption(&t_args(
                    "net-mobile-read-err",
                    &[("error", reason.into())],
                )))]),
            )];
        };
        let mut data = vec![row(Box::new(self.status_row(modem)))];
        data.extend(self.unlock_rows(modem));
        if let Some(notice) = &self.notice {
            data.push(note(notice));
        }
        if let Some(error) = &self.profile_error {
            data.push(row(error_caption(&t_args(
                "net-mobile-profile-err",
                &[("error", error.as_str().into())],
            ))));
        }
        if modem.usable() {
            data.extend(self.data_rows(modem));
        }
        let mut sections = vec![section_with(
            &t("net-mobile"),
            self.data_switch(modem),
            rows(data),
        )];
        let sims = self.sim_rows(modem);
        if !sims.is_empty() {
            sections.push(section(&t("net-mobile-sim-title"), rows(sims)));
        }
        sections
    }
}

impl PaneModel for MobileModel {
    fn compose(&self) -> Box<dyn Widget> {
        page(self.page_sections())
    }

    fn poll(&mut self) -> bool {
        self.poll_background();
        self.poll_codes();
        self.read_when_due(ui::time::now());
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "MobilePane"
    }

    fn next_wake(&self) -> Option<ui::time::Instant> {
        (self.loaded && self.present() && self.cx.reaches_devices())
            .then(|| self.read_at?.checked_add(READ_EVERY))
            .flatten()
    }
}

mod codes;
#[cfg(test)]
mod tests;
