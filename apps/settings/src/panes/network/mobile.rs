//! Mobile: the modem's data connection and its SIMs, under Network.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use os_actions::{
    MeteredPolicy, MobileProfile, MobileReading, ModemChange, ModemError, ModemFailure, ModemInfo,
    ModemState, NetworkModes, Registration, SimCard, SimKind, SimLock, Technology, Traffic,
    UnlockRetries,
};
use provider_settings_ui::bound::SettingRows as _;
use tracing::warn;
use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, EffectStatus, Effected, ViewCx};
use ui::tilo_resource::{self, ActionRef};
use ui::widget::{Button, ButtonRole, ListRow, Row, SignalLevel, Spacer, TextInput, TextInputKind};
use zeroize::Zeroizing;

use super::Reach;
use crate::area::AreaContext;
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::prefs::{DeviceVpnPrefs, NetworkUsage};
use crate::resource::Settings;
use crate::scenarios::MobileSample;
use crate::setting::{Follows, Read};
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    El, Item, SettingsSection, error_caption, field_row, note, row, rows, section, section_with,
    value_row,
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

pub fn area(app: &SettingsApp) -> Option<&MobileArea> {
    app.areas().find()
}

const METERED: &[(&str, &str)] = &[
    ("automatic", "net-mobile-metered-automatic"),
    ("metered", "net-mobile-metered-yes"),
    ("unmetered", "net-mobile-metered-no"),
];

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum MobileEffect {
    /// Read the modems, SIMs and the mobile data connection this section shows.
    #[effect(view, returns, kind = "read")]
    InspectMobileState,
    /// Read the modems and the mobile data connection again.
    #[effect(job, idempotent)]
    RefreshMobile,
    /// Use mobile data, or stop.
    #[effect(job, idempotent, label = "net-mobile-data")]
    SetMobileBroadband { on: bool },
    /// Use mobile data on other operators' networks, or only on the home
    /// network.
    #[effect(job, idempotent, label = "net-mobile-roaming-setting")]
    SetDataRoaming { on: bool },
    /// Set the access point mobile data connects through; what is omitted is
    /// what the APN editor holds.
    #[effect(job)]
    SetMobileApn {
        #[param("Choose the APN from the operator database")]
        automatic: Option<bool>,
        #[param("The access point name, such as internet.telekom")]
        apn: Option<ui::tilo_resource::NonBlank>,
    },
    /// Count mobile data against a limit, or let the network say.
    #[effect(job, idempotent, label = "net-metered-connection")]
    SetMobileMetered {
        #[param("automatic, metered or unmetered", choices = METERED.iter().map(|(key, _)| *key))]
        metered: String,
    },
    /// Choose the network types the modem may use.
    #[effect(job, idempotent, label = "net-mobile-network-type")]
    SetPreferredNetwork {
        #[param("The network types, as listed: 4g-3g-2g, 5g-4g-prefer-5g, 4g …")]
        network: String,
    },
    /// Unlock a SIM with its PIN; without it, the PIN is asked for.
    #[effect(job, label = "net-unlock-sim")]
    UnlockSim {
        #[param("The SIM, as listed; the locked one when omitted")]
        sim: Option<String>,
        #[param(secret, "The SIM's PIN")]
        pin: Option<String>,
    },
    /// Unlock a SIM blocked after too many wrong PINs with its PUK, choosing
    /// a new PIN; without both, they are asked for.
    #[effect(job, label = "net-mobile-unblock")]
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
    #[effect(job)]
    SetSimPinLock {
        #[param("The SIM, as listed; the one in use when omitted")]
        sim: Option<String>,
        on: bool,
        #[param(secret, "The SIM's PIN")]
        pin: Option<String>,
    },
    /// Change the SIM's PIN; without both PINs, they are asked for.
    #[effect(job)]
    ChangeSimPin {
        #[param("The SIM, as listed; the one in use when omitted")]
        sim: Option<String>,
        #[param(secret, "The current PIN")]
        old_pin: Option<String>,
        #[param(secret, "The new PIN")]
        new_pin: Option<String>,
    },
    /// Use this SIM for mobile data, on a modem with several.
    #[effect(job, row)]
    UseSim {
        #[param("The SIM, as listed")]
        sim: String,
    },
    /// Open or close the APN editor.
    #[effect(view)]
    ShowApnEditor,
    /// Close the APN editor, forgetting what was typed.
    #[effect(view)]
    CancelMobileForm,
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: MobileEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let area = area(app).ok_or_else(crate::area::missing)?;
    if effect == MobileEffect::InspectMobileState {
        return Ok(Effected::value(area.model().borrow().inspection()));
    }
    if effect == MobileEffect::RefreshMobile {
        app.radios().look_again();
    }
    let begun = area.model().borrow_mut().begin(effect)?;
    Ok(match begun {
        Begun::Done => Effected::done(),
        Begun::Work(work) => app.job(cx, work, |app: &SettingsApp, landed, _| {
            area_of(app)?.model().borrow_mut().land(landed)
        }),
    })
}

fn area_of(app: &SettingsApp) -> Result<&MobileArea, String> {
    area(app).ok_or_else(crate::area::missing)
}

pub(crate) fn mobile_data(app: &SettingsApp, _: &Follows) -> Read {
    let listed = app.radios().now().map(|radios| radios.wwan);
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

pub(crate) fn mobile_data_switch(cx: &ViewCx<Settings>, on: bool) -> Item {
    cx.setting(t("net-mobile-data"))
        .id("network.mobile")
        .toggle_effect(
            &LocalState::new(on),
            &cx.actions(),
            effect(MobileEffect::SET_MOBILE_BROADBAND),
        )
}

pub struct MobileArea {
    reach: Reach,
    sample: Option<MobileSample>,
    model: OnceCell<Rc<RefCell<MobileModel>>>,
}

impl MobileArea {
    pub fn new(cx: &AreaContext) -> Self {
        Self {
            reach: Reach::of(cx),
            sample: cx.scene().and_then(|scene| scene.mobile),
            model: OnceCell::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn over(cx: &AreaContext, service: Arc<dyn MobileService>) -> Self {
        let area = Self::new(cx);
        let _ = area
            .model
            .set(Rc::new(RefCell::new(MobileModel::loaded_from(
                Reach::of(cx),
                service,
            ))));
        area
    }

    pub fn model(&self) -> &Rc<RefCell<MobileModel>> {
        self.model.get_or_init(|| {
            let model = if self.reach.devices {
                MobileModel::of_device(self.reach.clone())
            } else {
                let sample = self.sample;
                let mut model = MobileModel::loaded_from(
                    self.reach.clone(),
                    Arc::new(ScriptedModem::new(sample)),
                );
                model.stage(sample);
                model
            };
            Rc::new(RefCell::new(model))
        })
    }

    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_background();
        }
    }

    fn present(&self) -> Option<bool> {
        if let Some(model) = self.model.get()
            && let Ok(model) = model.try_borrow()
            && model.loaded
        {
            return Some(model.present());
        }
        self.reach
            .radios
            .now()
            .map(|radios| radios.wwan)
            .filter(|wwan| !wwan)
    }

    pub(crate) fn statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        let mut unavailable = |id: &'static str, code: &str, reason: &str| {
            statuses.push(EffectStatus::unavailable(
                ActionRef::<SettingsEffect, ()>::new(
                    id,
                    &[],
                    <SettingsEffect as ui::tilo_resource::Effect>::kind(),
                ),
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

pub(crate) struct MobileSnapshot {
    modems: Result<MobileReading, ModemError>,
    profile: Result<Option<MobileProfile>, String>,
    radios: Result<os_actions::RadioState, String>,
}

#[derive(Debug)]
pub(crate) enum MobileChange {
    Data(bool),
    Roaming {
        uuid: Option<String>,
        on: bool,
    },
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

pub(crate) trait MobileService: Send + Sync {
    fn snapshot(&self) -> MobileSnapshot;
    fn change(&self, change: &MobileChange) -> Result<(), ModemError>;
    fn follow(&self, _changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
        None
    }
}

struct DeviceModem;

impl DeviceModem {
    fn profile(uuid: Option<&str>) -> Result<String, ModemError> {
        if let Some(uuid) = uuid {
            return Ok(uuid.to_owned());
        }
        os_actions::modem::mobile_profile_uuid().map_err(ModemError::Failed)
    }
}

impl MobileService for DeviceModem {
    fn follow(&self, changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
        let changed: Arc<dyn Fn() + Send + Sync> = Arc::from(changed);
        let modems = Arc::clone(&changed);
        let modems = os_actions::bus_follow::follow_system_services(
            &["org.freedesktop.ModemManager1"],
            move || modems(),
        );
        let network = os_actions::network::follow_network_changes(Box::new(move || changed()));
        match (modems, network) {
            (Ok(modems), Ok(network)) => Some(Box::new((modems, network))),
            (Err(error), _) | (_, Err(error)) => {
                warn!(target: "tilo::settings", %error, "the modems' changes are not followed");
                None
            }
        }
    }

    fn snapshot(&self) -> MobileSnapshot {
        crate::device_read::off_ui_thread();
        MobileSnapshot {
            modems: os_actions::read_mobile(),
            profile: os_actions::read_mobile_profile(),
            radios: os_actions::get_radio_state(),
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

    #[cfg(test)]
    pub(crate) fn changes(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.changes.clone())
            .unwrap_or_default()
    }

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

pub(crate) enum Begun {
    Done,
    Work(Box<dyn FnOnce() -> Landed + Send>),
}

pub(crate) enum Landed {
    Read(MobileSnapshot),
    Changed(MobileChange, Result<(), ModemError>),
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts: read yet, listed, switched on, editor open, changed"
)]
pub struct MobileModel {
    reach: Reach,
    service: Arc<dyn MobileService>,
    loaded: bool,
    snapshot: Option<crate::channel::Receiver<MobileSnapshot>>,
    modems: Vec<ModemInfo>,
    modems_error: Option<ModemError>,
    profile: Option<MobileProfile>,
    profile_error: Option<String>,
    wwan_listed: bool,
    wwan_enabled: bool,
    in_flight: usize,
    notice: Option<String>,
    asking: Option<codes::Asking>,
    jobs: Vec<crate::channel::Receiver<Landed>>,
    apn_open: bool,
    apn_automatic: LocalState<bool>,
    apn_text: LocalState<String>,
    following: std::rc::Weak<crate::watch::Counted>,
    changes_seen: u64,
    changed: bool,
}

impl MobileModel {
    fn of_device(reach: Reach) -> Self {
        let mut model = Self::with_service(reach, Arc::new(DeviceModem));
        model.start_snapshot();
        model
    }

    pub(crate) fn loaded_from(reach: Reach, service: Arc<dyn MobileService>) -> Self {
        let mut model = Self::with_service(reach, service);
        let snapshot = model.service.snapshot();
        model.adopt(snapshot);
        model
    }

    fn with_service(reach: Reach, service: Arc<dyn MobileService>) -> Self {
        Self {
            reach,
            service,
            loaded: false,
            snapshot: None,
            modems: Vec::new(),
            modems_error: None,
            profile: None,
            profile_error: None,
            wwan_listed: false,
            wwan_enabled: false,
            in_flight: 0,
            notice: None,
            asking: None,
            jobs: Vec::new(),
            apn_open: false,
            apn_automatic: LocalState::new(true),
            apn_text: LocalState::new(String::new()),
            following: std::rc::Weak::new(),
            changes_seen: 0,
            changed: false,
        }
    }

    fn stage(&mut self, sample: Option<MobileSample>) {
        if sample == Some(MobileSample::Roaming) {
            super::usage::sample(&self.reach.storage);
        }
        let Some(sample) = sample.filter(|sample| *sample != MobileSample::Registered) else {
            return;
        };
        self.reach.pages.open(
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

    fn present(&self) -> bool {
        !self.modems.is_empty()
            || (self.modems_error.is_some()
                && (self.wwan_listed || self.reach.radios.now().is_some_and(|radios| radios.wwan)))
    }

    fn modem(&self) -> Option<&ModemInfo> {
        self.modems
            .iter()
            .find(|modem| modem.failure.is_none())
            .or_else(|| self.modems.first())
    }

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
            Ok(()) => self.snapshot = Some(receiver),
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
        self.loaded = true;
        self.follow_lock();
    }

    fn change(&mut self, change: MobileChange) -> Begun {
        self.in_flight += 1;
        let service = Arc::clone(&self.service);
        Begun::Work(Box::new(move || {
            let result = service.change(&change);
            Landed::Changed(change, result)
        }))
    }

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

    fn modes_named(modem: &ModemInfo, chosen: &str) -> Option<NetworkModes> {
        modem
            .supported_modes
            .iter()
            .copied()
            .find(|modes| modes.key() == chosen || modes_shown(*modes) == chosen)
    }

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
                    let apn = match apn {
                        Some(apn) => apn,
                        None => ui::tilo_resource::NonBlank::parse(&self.apn_text.get())
                            .ok_or_else(|| t("net-mobile-apn-empty"))?,
                    };
                    Some(apn.into_string())
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

    fn read_reported_changes(&mut self) {
        let Some(changes) = self
            .following
            .upgrade()
            .and_then(|following| following.changes())
        else {
            return;
        };
        if changes == self.changes_seen
            || !self.loaded
            || self.snapshot.is_some()
            || self.in_flight > 0
        {
            return;
        }
        self.changes_seen = changes;
        self.start_snapshot();
    }

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
        let usage = self.reach.storage.settings::<NetworkUsage>().get();
        serde_json::json!({
            "source": if self.reach.devices { "device" } else { "preview" },
            "loaded": self.loaded, "present": self.loaded.then(|| self.present()),
            "data": self.loaded.then(|| self.data_on()),
            "roaming": self.profile.as_ref().map(|profile| profile.roaming),
            "apn": self.profile.as_ref().map(|profile| profile.apn.clone().unwrap_or_else(|| "automatic".into())),
            "metered": self.profile.as_ref().map(|profile| metered_key(profile.metered)),
            "data_used_bytes": usage.mobile_used(ui::clock::local_now().date_naive()),
            "data_limit_bytes": usage.limit(),
            "data_period_starts_on_day": usage.reset_day.get(),
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

const fn signal_level(percent: u8) -> SignalLevel {
    SignalLevel::from_bars(match percent {
        0 => 0,
        1..=25 => 1,
        26..=50 => 2,
        51..=75 => 3,
        _ => 4,
    })
}

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

pub(crate) struct Page<'a> {
    model: &'a MobileModel,
    cx: &'a ViewCx<DeviceVpnPrefs>,
    actions: &'a Actions<Settings>,
}

impl std::ops::Deref for Page<'_> {
    type Target = MobileModel;

    fn deref(&self) -> &MobileModel {
        self.model
    }
}

impl<'a> Page<'a> {
    pub(crate) fn new(
        model: &'a MobileModel,
        cx: &'a ViewCx<DeviceVpnPrefs>,
        actions: &'a Actions<Settings>,
    ) -> Self {
        Self { model, cx, actions }
    }

    fn button(&self, id: &str, label: &str, effect: MobileEffect) -> Button {
        self.actions
            .bind_effect_invocation(Button::new(id, label), &SettingsEffect::from(effect))
    }

    fn buttons(buttons: Vec<Button>) -> Item {
        row(Box::new(
            buttons
                .into_iter()
                .fold(Row::new(), |row, button| row.push(Box::new(button)))
                .push(Box::new(Spacer::new())),
        ))
    }

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

    fn data_rows(&self, modem: &ModemInfo) -> Vec<Item> {
        let mut items = Vec::new();
        let profile = self.profile.as_ref();
        items.push(
            self.cx
                .setting(t("net-mobile-roaming-setting"))
                .id("network.mobile.roaming")
                .described(t("net-mobile-roaming-caption"))
                .toggle_effect(
                    &LocalState::new(profile.is_some_and(|profile| profile.roaming)),
                    self.actions,
                    effect(MobileEffect::SET_DATA_ROAMING),
                ),
        );
        let metered = profile.map_or(MeteredPolicy::Automatic, |profile| profile.metered);
        items.push(
            self.cx
                .setting(t("net-mobile-metered"))
                .id("network.mobile.metered")
                .described(t("net-mobile-metered-caption"))
                .choice_effect(
                    METERED.iter().map(|(_, words)| t(words)),
                    &LocalState::new(metered_shown(metered)),
                    self.actions,
                    effect(MobileEffect::SET_MOBILE_METERED),
                ),
        );
        if modem.supported_modes.len() > 1 {
            let current = modem.modes.map(modes_shown).unwrap_or_default();
            items.push(
                self.cx
                    .setting(t("net-mobile-network-type"))
                    .id("network.mobile.network-type")
                    .choice_effect(
                        modem.supported_modes.iter().copied().map(modes_shown),
                        &LocalState::new(current),
                        self.actions,
                        effect(MobileEffect::SET_PREFERRED_NETWORK),
                    ),
            );
        }
        items.extend(self.apn_rows());
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
                    .setting(t("net-mobile-apn-auto"))
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

    fn pin_rows(&self, modem: &ModemInfo) -> Vec<Item> {
        let mut items = Vec::new();
        if let Some(enabled) = modem.pin_enabled {
            let shown = match self.asked() {
                Some(codes::CodeAsk::PinLock { on, .. }) => *on,
                _ => enabled,
            };
            items.push(
                self.cx
                    .setting(t("net-mobile-pin-lock"))
                    .id("network.mobile.pin-lock")
                    .described(t("net-mobile-pin-lock-caption"))
                    .toggle_effect(
                        &LocalState::new(shown),
                        self.actions,
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

    fn data_switch(&self, modem: &ModemInfo) -> El {
        Box::new(
            self.actions.bind_effect(
                ui::widget::Toggle::from_state(&LocalState::new(self.data_on()))
                    .with_id("network.mobile")
                    .with_enabled(modem.usable()),
                effect(MobileEffect::SET_MOBILE_BROADBAND),
            ),
        )
    }

    pub(crate) fn card(&self) -> Vec<SettingsSection> {
        if !self.loaded || !self.present() {
            return Vec::new();
        }
        let Some(modem) = self.modem() else {
            return self.page_sections();
        };
        let mut status = self.status_row(modem).disclosure();
        status = match self
            .actions
            .effect(&SettingsEffect::from(super::NetworkEffect::ShowMobile))
        {
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

    pub(crate) fn page_sections(&self) -> Vec<SettingsSection> {
        if !self.loaded || !self.present() {
            return Vec::new();
        }
        let Some(modem) = self.modem() else {
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
        if modem.usable() {
            sections.push(section(
                &t("net-mobile-usage-title"),
                rows(super::usage::mobile_rows(self.cx)),
            ));
        }
        let sims = self.sim_rows(modem);
        if !sims.is_empty() {
            sections.push(section(&t("net-mobile-sim-title"), rows(sims)));
        }
        sections
    }
}

impl MobileModel {
    pub(super) fn poll(&mut self) -> bool {
        self.poll_background();
        self.poll_codes();
        self.read_reported_changes();
        std::mem::take(&mut self.changed)
    }

    pub(super) fn follow(&mut self) -> Rc<crate::watch::Counted> {
        if let Some(held) = self.following.upgrade() {
            return held;
        }
        self.changes_seen = if self.loaded { u64::MAX } else { 0 };
        let service = Arc::clone(&self.service);
        crate::watch::shared(&mut self.following, move || {
            crate::watch::Counted::start(|changed| service.follow(changed))
        })
    }
}

mod codes;
#[cfg(test)]
mod tests;
