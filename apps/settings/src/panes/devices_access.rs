//! Devices and access: everything that acts as the person, in one place.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow, ViewCx};
use ui::icons::Icon;
use ui::tilo_actions::remote::mcp_server::{self, Grant, Reach, Server};
use ui::tilo_actions::remote::{Confirmation, InvokeArgument, RemoteActionTarget, session_actions};
use ui::tilo_resource::{self, Effect as _};
use ui::widget::{
    Button, ButtonRole, Column, Container, DisclosureGroup, DisclosureRow, Element, ListRow, Row,
    Spacer, TextInput,
};

use crate::area::AreaContext;
use crate::device_fleet::DeviceView;
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, block, note, page, row, rows, section, untitled,
};
use ui::tilo_actions::remote::identity_admin::KeyKind;

crate::effects::effect_family!(DevicesAccessEffect => answer);

const REVOKE_REASONS: [&str; 4] = ["retired", "lost", "stolen", "compromised"];

const REMOVED: &str = "retired";

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DevicesAccessEffect {
    /// End a device's or an agent's access to everything of the person's, for good.
    #[effect(kind = "outside")]
    #[effect(job, row, destructive)]
    RevokeDevice {
        #[param("The key, in hex")]
        device: String,
        #[param("retired, lost, stolen or compromised", choices = REVOKE_REASONS)]
        reason: String,
    },
    /// End every tab's access of one browser of the person's, for good.
    #[effect(kind = "outside")]
    #[effect(job, row, destructive)]
    RevokeBrowser {
        #[param("The browser's id, as the device list names it")]
        browser: String,
        #[param("retired, lost, stolen or compromised", choices = REVOKE_REASONS)]
        reason: String,
    },
    /// Take an agent's grant back: its sessions end now and it gets in no more.
    #[effect(kind = "outside")]
    #[effect(job, row, destructive)]
    RevokeAgent {
        #[param("The grant's id, as the MCP server lists it")]
        grant: String,
    },
    /// End an agent's open session, keeping its grant so it can come back.
    #[effect(kind = "outside")]
    #[effect(job, row)]
    StopSession {
        #[param("The grant's id, as the MCP server lists it")]
        grant: String,
        #[param("The session's name, as History names it")]
        session: String,
    },
    /// Name a device of the person's: what their device lists read it by from
    /// then on. Two computers set up the same way enroll under one name, so
    /// the person tells them apart here.
    #[effect(kind = "outside", on = Device, on = AgentGrant)]
    #[effect(job, row)]
    RenameDevice {
        #[param("The key, in hex")]
        device: String,
        #[param("The name it should read by, such as “Claude Code on the laptop 2”")]
        name: String,
    },
    // The two below ask the session's shell for a sheet of its own and answer
    // when it has, so they are jobs: a view answers at once. `own_device`
    // keeps who runs them what it was for a view: the person at this device
    // and those acting for them, needing no more of a workspace than reading.
    /// Show how an agent or another computer connects as the person.
    #[effect(kind = "outside")]
    #[effect(job, own_device, idempotent, label = "settings-access-connect")]
    ShowConnect,
    /// Show the link another of your own devices joins with, as a code its camera reads.
    #[effect(kind = "outside")]
    #[effect(job, own_device, idempotent, label = "settings-access-add-device")]
    ShowPairDevice,
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: DevicesAccessEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let area = area(app).ok_or_else(crate::area::missing)?;
    area.model()
        .borrow_mut()
        .act(&area.remote, &effect, Some(cx.reply()))?;
    if effect.takes_back() {
        // Its row says it is being removed from here on: the question the
        // row asked is answered, and the page is built again.
        area.removing.set(None);
        area.shown.set(area.shown.get().wrapping_add(1));
    }
    Ok(Effected::Pending)
}

impl DevicesAccessEffect {
    /// Whether it ends something's access for good.
    const fn takes_back(&self) -> bool {
        matches!(
            self,
            Self::RevokeDevice { .. } | Self::RevokeBrowser { .. } | Self::RevokeAgent { .. }
        )
    }
}

crate::area::pane_area! {
    DevicesAccessArea;
    fn rows_published(&self) -> bool {
        true
    }
    crate::area::serves!(DevicesAccess);
    fn poll(&self) {
        DevicesAccessArea::poll(self);
    }
    fn next_wake(&self) -> Option<ui::time::Instant> {
        let model = self.model.get()?.try_borrow().ok()?;
        let wait = model.wakes_in()?;
        Some(ui::time::now() + std::time::Duration::from_millis(wait))
    }
}

pub fn area(app: &SettingsApp) -> Option<&DevicesAccessArea> {
    app.areas().find()
}

pub struct DevicesAccessArea {
    remote: ui::RemoteActions,
    scene: Option<AccessSample>,
    model: OnceCell<Rc<RefCell<AccessModel>>>,
    shown: LocalState<u64>,
    removing: LocalState<Option<String>>,
    /// The device being renamed, by its key, and what is typed for it.
    naming: LocalState<Option<String>>,
    name_draft: LocalState<String>,
    details: DisclosureGroup,
}

#[derive(Clone, Debug, Default)]
pub struct AccessSample {
    pub devices: Vec<DeviceView>,
    pub api: Server,
    pub now_ms: i64,
}

impl DevicesAccessArea {
    pub fn new(cx: &AreaContext, remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            scene: cx.scene().and_then(|scene| scene.access.clone()),
            model: OnceCell::new(),
            shown: LocalState::new(0),
            removing: LocalState::new(None),
            naming: LocalState::new(None),
            name_draft: LocalState::new(String::new()),
            details: DisclosureGroup::new(),
        }
    }

    pub fn model(&self) -> &Rc<RefCell<AccessModel>> {
        self.model.get_or_init(|| {
            Rc::new(RefCell::new(match &self.scene {
                Some(sample) => AccessModel::shown(sample.clone()),
                None => AccessModel::listed(&self.remote),
            }))
        })
    }

    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get()
            && let Ok(mut model) = model.try_borrow_mut()
            && model.poll(&self.remote)
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        // The name typed for a device now is the row's, since the app reads the
        // name the row carries.
        let draft = self.name_draft.get();
        let model = self.model().borrow();
        let devices = model.entries().into_iter().filter_map(|entry| match entry {
            Entry::Device(device) if !device.current => Some(device_row(device, &draft)),
            Entry::Browser(browser) if browser.revocable() => Some(browser_row(&browser)),
            Entry::Device(_) | Entry::Browser(_) => None,
        });
        let agents = model
            .agents()
            .into_iter()
            .map(|agent| agent_row(&agent, &draft));
        devices.chain(agents).collect()
    }
}

#[derive(tilo_resource::Row)]
#[row(kind = "settings.device", label = "Device", effects = DevicesAccessEffect)]
pub(crate) struct Device;

#[derive(tilo_resource::Row)]
#[row(kind = "settings.agent-grant", label = "Agent", effects = DevicesAccessEffect)]
pub(crate) struct AgentGrant;

enum Entry<'a> {
    Device(&'a DeviceView),
    Browser(BrowserEntry<'a>),
}

impl Entry<'_> {
    fn current(&self) -> bool {
        match self {
            Entry::Device(device) => device.current,
            Entry::Browser(browser) => browser.current(),
        }
    }

    fn last_active(&self) -> Option<i64> {
        match self {
            Entry::Device(device) => device.last_active_ms,
            Entry::Browser(browser) => browser.last_active(),
        }
    }
}

struct BrowserEntry<'a> {
    id: &'a str,
    keys: Vec<&'a DeviceView>,
}

impl BrowserEntry<'_> {
    fn current(&self) -> bool {
        self.keys.iter().any(|key| key.current)
    }

    fn live(&self) -> impl Iterator<Item = &DeviceView> {
        self.keys.iter().copied().filter(|key| !key.revoked)
    }

    fn revoked(&self) -> bool {
        self.live().next().is_none()
    }

    fn revocable(&self) -> bool {
        !self.current() && !self.revoked()
    }

    fn online(&self) -> bool {
        self.live().any(|key| key.online)
    }

    fn last_active(&self) -> Option<i64> {
        self.live().filter_map(|key| key.last_active_ms).max()
    }

    fn kind(&self) -> Option<&str> {
        self.live()
            .last()
            .or_else(|| self.keys.last().copied())
            .map(|key| key.label.as_str())
            .filter(|label| !label.is_empty() && *label != GENERIC_BROWSER)
    }

    fn label(&self) -> String {
        if self.current() {
            return t("settings-access-this-browser");
        }
        self.kind()
            .map_or_else(|| t("settings-access-a-browser"), str::to_owned)
    }
}

const GENERIC_BROWSER: &str = "Browser";

struct AgentEntry<'a> {
    key: Option<&'a DeviceView>,
    grant: Option<&'a Grant>,
}

impl AgentEntry<'_> {
    fn id(&self) -> String {
        match (self.key, self.grant) {
            (Some(key), _) => hex::encode(key.key),
            (None, Some(grant)) => grant.id.clone(),
            (None, None) => String::new(),
        }
    }

    fn label(&self) -> String {
        if let Some(key) = self.key.filter(|key| !key.label.is_empty()) {
            return key.label.clone();
        }
        match self.grant {
            Some(grant) if grant.script_key && !grant.agent.is_empty() => grant.agent.clone(),
            Some(grant) if !grant.device.is_empty() && !grant.agent.is_empty() => t_args(
                "settings-access-agent-on",
                &[
                    ("agent", grant.agent.clone().into()),
                    ("device", grant.device.clone().into()),
                ],
            ),
            Some(grant) => app_of(grant),
            None => t("settings-access-unnamed-agent"),
        }
    }

    fn active_now(&self) -> bool {
        self.key.is_some_and(|key| key.online)
            || self.grant.is_some_and(|grant| !grant.sessions.is_empty())
    }

    fn last_active(&self) -> Option<i64> {
        let key = self.key.and_then(|key| key.last_active_ms);
        let grant = self.grant.and_then(|grant| grant.last_ms);
        key.max(grant)
    }

    fn connected(&self) -> Option<i64> {
        self.key
            .and_then(|key| key.issued_ms)
            .or_else(|| self.grant.map(|grant| grant.granted_ms))
    }

    fn expires(&self) -> Option<i64> {
        self.key
            .and_then(|key| key.expires_ms)
            .or_else(|| self.grant.and_then(|grant| grant.expires_ms))
    }

    fn workspace(&self) -> Option<String> {
        if let Some(key) = self.key {
            return key.workspace.clone();
        }
        match self.grant.map(|grant| &grant.reach) {
            Some(Reach::Workspace { name, .. }) => Some(name.clone()),
            Some(Reach::Account) | None => None,
        }
    }

    fn removal(&self) -> DevicesAccessEffect {
        match self.key {
            Some(key) => DevicesAccessEffect::RevokeDevice {
                device: hex::encode(key.key),
                reason: REMOVED.to_owned(),
            },
            None => DevicesAccessEffect::RevokeAgent { grant: self.id() },
        }
    }
}

struct Pending {
    calls: Vec<ui::InvocationCall>,
    reply: Option<EffectReply>,
    /// What it takes back, as [`AccessModel::leaving`] holds it.
    leaving: Option<u64>,
}

/// How a host answers one of the page's lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Kept {
    /// Not answered yet.
    #[default]
    Unanswered,
    /// The host keeps the read current and sends each change of the list on
    /// it: it is never asked again.
    Current,
    /// The host answered once and cannot say when the list changes.
    Once,
    /// Nothing here answers the list.
    Nowhere,
}

/// One of the page's lists, as its host answers it.
#[derive(Default)]
struct Listed {
    /// The read of it: a host that keeps it current sends each change of the
    /// list on it, one that answers once ends it with its answer.
    read: Option<ui::Followed>,
    seen: u64,
    kept: Kept,
    /// Whether the read asked last is still to be answered.
    awaited: bool,
    answered: bool,
    error: Option<String>,
    /// When it was asked last, ms since the epoch.
    asked_ms: i64,
}

impl Listed {
    /// Read the list at `target` from now on. The read this replaces ends.
    fn ask(&mut self, remote: &ui::RemoteActions, target: RemoteActionTarget, now: i64) {
        self.read = Some(remote.follow(target, InvokeArgument::None));
        self.seen = 0;
        self.awaited = true;
        self.asked_ms = now;
    }

    /// What the host answered since the last look: the list as it is now, or
    /// why there is none.
    fn take(&mut self) -> Option<Result<Option<serde_json::Value>, String>> {
        use ui::tilo_actions::InvocationUpdate;
        use ui::tilo_actions::remote::ActionCallError;
        let call = self.read.as_ref()?.call();
        let revision = call.revision();
        if revision == self.seen {
            return None;
        }
        self.seen = revision;
        let (kept, answer) = match call.get() {
            ui::Arrival::Ready(InvocationUpdate::Item { value, .. }) => {
                (Kept::Current, Ok(Some(value)))
            }
            ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
                (Kept::Once, Ok(output.value))
            }
            ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                (Kept::Once, Err(error.message))
            }
            ui::Arrival::Ready(InvocationUpdate::Cancelled { .. }) => (
                Kept::Once,
                Err("the host stopped before it answered".to_owned()),
            ),
            ui::Arrival::Failed(
                error @ (ActionCallError::UnknownAction | ActionCallError::Unavailable { .. }),
            ) => (Kept::Nowhere, Err(error.to_string())),
            ui::Arrival::Failed(error) => (Kept::Once, Err(error.to_string())),
            ui::Arrival::Ready(
                InvocationUpdate::Accepted { .. } | InvocationUpdate::Progress { .. },
            )
            | ui::Arrival::Pending { .. } => return None,
        };
        self.kept = kept;
        self.awaited = false;
        self.answered = true;
        self.error = answer.as_ref().err().cloned();
        Some(answer)
    }

    /// How long until a host that answered once is asked again, in ms: `None`
    /// for a list that is not asked again, or whose answer is still awaited.
    fn asked_again_in(&self, now: i64) -> Option<u64> {
        if self.kept != Kept::Once || self.awaited {
            return None;
        }
        u64::try_from(
            ASK_AGAIN_MS
                .saturating_sub(now.saturating_sub(self.asked_ms))
                .max(0),
        )
        .ok()
    }
}

/// Something the person took back here that the lists still name.
struct Leaving {
    id: u64,
    /// What it ends: each device key in hex, or an agent's grant.
    of: Vec<String>,
    /// When the host answered that it took the removal, ms since the epoch.
    taken_ms: Option<i64>,
}

pub struct AccessModel {
    devices: Vec<DeviceView>,
    devices_read: Listed,
    api: Server,
    api_followed: Option<ui::Followed>,
    api_seen: u64,
    pending: Vec<Pending>,
    /// What the person took back here, until the lists show it gone: its row
    /// says it is being removed and offers nothing more.
    leaving: Vec<Leaving>,
    left: u64,
    now_ms: Option<i64>,
}

/// How often a list is asked for again where its host answered it once and
/// cannot say when it changes. A host that keeps the read current, as a
/// page's does for the person's devices, is never asked again.
const ASK_AGAIN_MS: i64 = 4000;

/// How long a row says it is being removed once the host took the removal
/// and its list still names it: after that the row shows what the list says.
const LEAVING_SHOWN_MS: i64 = 10_000;

impl AccessModel {
    fn listed(remote: &ui::RemoteActions) -> Self {
        let mut model = Self::empty();
        model.api_followed =
            Some(remote.follow(mcp_server::target(mcp_server::SERVER), InvokeArgument::None));
        let now = model.now();
        model
            .devices_read
            .ask(remote, crate::device_fleet::listed(), now);
        model
    }

    fn shown(sample: AccessSample) -> Self {
        let mut model = Self::empty();
        model.devices = sample.devices;
        model.devices_read.answered = true;
        model.api = sample.api;
        model.now_ms = Some(sample.now_ms);
        model
    }

    fn empty() -> Self {
        Self {
            devices: Vec::new(),
            devices_read: Listed::default(),
            api: Server::default(),
            api_followed: None,
            api_seen: 0,
            pending: Vec::new(),
            leaving: Vec::new(),
            left: 0,
            now_ms: None,
        }
    }

    fn now(&self) -> i64 {
        self.now_ms
            .unwrap_or_else(|| i64::try_from(ui::time::unix_now_ms()).unwrap_or(i64::MAX))
    }

    /// How long until the open page has something to do that nothing tells
    /// it of, in ms: asking a host that answered a list once again, or
    /// showing a row the host never showed gone as its list has it. `None`
    /// where every list is kept current and nothing is being removed.
    fn wakes_in(&self) -> Option<u64> {
        let now = self.now();
        let shown = self
            .leaving
            .iter()
            .filter_map(|leaving| leaving.taken_ms)
            .filter_map(|taken| {
                u64::try_from(
                    LEAVING_SHOWN_MS
                        .saturating_sub(now.saturating_sub(taken))
                        .max(0),
                )
                .ok()
            });
        [self.devices_read.asked_again_in(now)]
            .into_iter()
            .flatten()
            .chain(shown)
            .min()
    }

    /// Hold `of`, what the person just took back, as leaving.
    fn leave(&mut self, of: Vec<String>) -> u64 {
        self.left = self.left.wrapping_add(1);
        self.leaving.push(Leaving {
            id: self.left,
            of,
            taken_ms: None,
        });
        self.left
    }

    /// Whether the person took `key` back here and the lists still name it.
    fn leaves(&self, key: &str) -> bool {
        self.leaving
            .iter()
            .any(|leaving| leaving.of.iter().any(|of| of == key))
    }

    /// A list was read after the host took a removal: its rows show what the
    /// list says from here on. Whether a row said it was being removed.
    fn listed_anew(&mut self) -> bool {
        let marked = self.leaving.len();
        self.leaving.retain(|leaving| leaving.taken_ms.is_none());
        self.leaving.len() != marked
    }

    fn act(
        &mut self,
        remote: &ui::RemoteActions,
        effect: &DevicesAccessEffect,
        reply: Option<EffectReply>,
    ) -> Result<(), String> {
        let mut leaving = None;
        let call = match effect {
            DevicesAccessEffect::RevokeDevice { device, reason } => {
                let key = self.device(device)?;
                if !REVOKE_REASONS.contains(&reason.as_str()) {
                    return Err(t("settings-access-reason-unknown"));
                }
                leaving = Some(self.leave(vec![device.clone()]));
                crate::device_fleet::revoke(remote, &key, reason)
            }
            DevicesAccessEffect::RenameDevice { device, name } => {
                let key = self.device(device)?;
                let name = name.trim();
                if name.is_empty() {
                    return Err(t("settings-access-name-required"));
                }
                crate::device_fleet::rename(remote, &key, name)
            }
            DevicesAccessEffect::RevokeBrowser { browser, reason } => {
                let keys = self.browser_keys(browser)?;
                if !REVOKE_REASONS.contains(&reason.as_str()) {
                    return Err(t("settings-access-reason-unknown"));
                }
                let calls = keys
                    .iter()
                    .map(|key| crate::device_fleet::revoke(remote, key, reason))
                    .collect();
                let tabs = self.leave(keys.iter().map(hex::encode).collect());
                self.pending.push(Pending {
                    calls,
                    reply,
                    leaving: Some(tabs),
                });
                return Ok(());
            }
            DevicesAccessEffect::RevokeAgent { grant } => {
                self.grant(grant)?;
                leaving = Some(self.leave(vec![grant.clone()]));
                remote.invoke(
                    mcp_server::target(mcp_server::REVOKE),
                    mcp_server::argument(&mcp_server::GrantRef {
                        grant: grant.clone(),
                    }),
                    Some(Confirmation::Confirmed),
                )
            }
            DevicesAccessEffect::StopSession { grant, session } => {
                self.grant(grant)?;
                remote.invoke(
                    mcp_server::target(mcp_server::STOP),
                    mcp_server::argument(&mcp_server::AgentSessionRef {
                        grant: grant.clone(),
                        session: session.clone(),
                    }),
                    None,
                )
            }
            DevicesAccessEffect::ShowConnect => remote.invoke(
                session_actions::target(session_actions::SHOW_CONNECT),
                InvokeArgument::Json(serde_json::Map::new()),
                None,
            ),
            DevicesAccessEffect::ShowPairDevice => remote.invoke(
                session_actions::target(session_actions::SHOW_PAIR_DEVICE),
                InvokeArgument::Json(serde_json::Map::new()),
                None,
            ),
        };
        self.pending.push(Pending {
            calls: vec![call],
            reply,
            leaving,
        });
        Ok(())
    }

    fn browser_keys(&self, browser: &str) -> Result<Vec<[u8; 32]>, String> {
        let tabs: Vec<&DeviceView> = self
            .devices
            .iter()
            .filter(|listed| {
                listed.kind == KeyKind::Device && listed.browser.as_deref() == Some(browser)
            })
            .collect();
        if tabs.iter().any(|tab| tab.current) {
            return Err(t("settings-access-browser-is-this"));
        }
        let keys: Vec<[u8; 32]> = tabs
            .iter()
            .filter(|tab| !tab.revoked)
            .map(|tab| tab.key)
            .collect();
        if keys.is_empty() {
            return Err(t("settings-access-no-such-device"));
        }
        Ok(keys)
    }

    fn device(&self, device: &str) -> Result<[u8; 32], String> {
        self.devices
            .iter()
            .find(|listed| hex::encode(listed.key) == device && !listed.revoked)
            .map(|listed| listed.key)
            .ok_or_else(|| t("settings-access-no-such-device"))
    }

    fn grant(&self, grant: &str) -> Result<&Grant, String> {
        self.api
            .grants
            .iter()
            .find(|listed| listed.id == grant)
            .ok_or_else(|| t("settings-access-no-such-agent"))
    }

    fn poll(&mut self, remote: &ui::RemoteActions) -> bool {
        // What a host that holds no ask is still running is asked after.
        remote.poll();
        let mut changed = self.poll_api();
        changed |= self.take_lists();
        let now = self.now();
        // A host that answered a list once cannot say when it changes: it is
        // asked again while the page is open. One that keeps the read
        // current says so itself, and is never asked again.
        if self.pending.is_empty() && self.devices_read.asked_again_in(now) == Some(0) {
            self.devices_read
                .ask(remote, crate::device_fleet::listed(), now);
        }
        if self.settle_pending(now) {
            // What the person did here changed a list: a host that answered
            // it once is asked for it again now.
            if self.devices_read.kept == Kept::Once {
                self.devices_read
                    .ask(remote, crate::device_fleet::listed(), now);
            }
            changed = true;
        }
        // A removal the host took and never showed gone: its row shows what
        // the list says again, so the person can see it and try once more.
        let marked = self.leaving.len();
        self.leaving.retain(|leaving| {
            leaving
                .taken_ms
                .is_none_or(|taken| now.saturating_sub(taken) < LEAVING_SHOWN_MS)
        });
        changed | (self.leaving.len() != marked)
    }

    /// Take what the hosts answered of the page's lists since the last look;
    /// whether the page shows something else for it.
    fn take_lists(&mut self) -> bool {
        let mut changed = false;
        let before = (self.devices_read.answered, self.devices_read.error.clone());
        if let Some(answer) = self.devices_read.take() {
            match answer.and_then(crate::device_fleet::devices) {
                Ok(devices) => {
                    changed |= self.devices != devices;
                    self.devices = devices;
                }
                Err(error) => self.devices_read.error = Some(error),
            }
            changed |= before != (true, self.devices_read.error.clone());
            changed |= self.listed_anew();
        }
        changed
    }

    /// Hand each thing the person did here its host's answer, once the host
    /// gave it; whether any was answered.
    fn settle_pending(&mut self, now: i64) -> bool {
        let mut settled = false;
        for pending in std::mem::take(&mut self.pending) {
            let answers: Option<Vec<_>> = pending
                .calls
                .iter()
                .map(crate::device_fleet::settled)
                .collect();
            let Some(answers) = answers else {
                self.pending.push(pending);
                continue;
            };
            let failed = answers.iter().position(Result::is_err);
            if let Some(id) = pending.leaving {
                if failed.is_some() {
                    // Refused: it is still the person's, and its row offers
                    // what it did before.
                    self.leaving.retain(|leaving| leaving.id != id);
                } else if let Some(leaving) =
                    self.leaving.iter_mut().find(|leaving| leaving.id == id)
                {
                    leaving.taken_ms = Some(now);
                }
            }
            let answer = match failed {
                Some(at) => answers.into_iter().nth(at),
                None => answers.into_iter().last(),
            };
            if let (Some(reply), Some(answer)) = (pending.reply, answer) {
                reply.finish(answer);
            }
            settled = true;
        }
        settled
    }

    fn poll_api(&mut self) -> bool {
        use ui::tilo_actions::InvocationUpdate;
        let Some(followed) = &self.api_followed else {
            return false;
        };
        let call = followed.call();
        let revision = call.revision();
        if revision == self.api_seen {
            return false;
        }
        self.api_seen = revision;
        let value = match call.get() {
            ui::Arrival::Ready(InvocationUpdate::Item { value, .. }) => value,
            ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
                output.value.unwrap_or(serde_json::Value::Null)
            }
            ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                tracing::debug!(target: "tilo::settings", error = %error.message, "no MCP server answers here");
                return false;
            }
            ui::Arrival::Failed(error) => {
                tracing::debug!(target: "tilo::settings", %error, "no MCP server answers here");
                return false;
            }
            ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => return false,
        };
        match serde_json::from_value::<Server>(value) {
            Ok(api) => {
                self.api = api;
                self.listed_anew();
                true
            }
            Err(error) => {
                tracing::warn!(target: "tilo::settings", %error, "the MCP server answered something unreadable");
                false
            }
        }
    }

    fn entries(&self) -> Vec<Entry<'_>> {
        let mut entries: Vec<Entry<'_>> = Vec::new();
        let mut browsers: Vec<&str> = Vec::new();
        for device in self
            .devices
            .iter()
            .filter(|device| device.kind == KeyKind::Device && device.agent.is_none())
        {
            match device.browser.as_deref() {
                None => {
                    if !device.revoked {
                        entries.push(Entry::Device(device));
                    }
                }
                Some(id) if !browsers.contains(&id) => {
                    browsers.push(id);
                    let keys = self.devices.iter().filter(|key| {
                        key.kind == KeyKind::Device && key.browser.as_deref() == Some(id)
                    });
                    let browser = BrowserEntry {
                        id,
                        keys: keys.collect(),
                    };
                    if !browser.revoked() {
                        entries.push(Entry::Browser(browser));
                    }
                }
                Some(_) => {}
            }
        }
        entries.sort_by_key(|entry| {
            (
                !entry.current(),
                std::cmp::Reverse(entry.last_active().unwrap_or(i64::MIN)),
            )
        });
        entries
    }

    fn agents(&self) -> Vec<AgentEntry<'_>> {
        let grant_of = |key: &DeviceView| {
            self.api
                .grants
                .iter()
                .find(|grant| grant.id == hex::encode(key.key))
        };
        let mut agents: Vec<AgentEntry<'_>> = self
            .devices
            .iter()
            .filter(|key| (key.kind == KeyKind::Agent || key.agent.is_some()) && !key.revoked)
            .map(|key| AgentEntry {
                key: Some(key),
                grant: grant_of(key),
            })
            .collect();
        for grant in &self.api.grants {
            let listed = self
                .devices
                .iter()
                .any(|key| hex::encode(key.key) == grant.id);
            if !listed {
                agents.push(AgentEntry {
                    key: None,
                    grant: Some(grant),
                });
            }
        }
        agents.sort_by_key(|agent| {
            (
                !agent.active_now(),
                std::cmp::Reverse(agent.last_active().unwrap_or(i64::MIN)),
            )
        });
        agents
    }

    fn page(&self, cx: &ViewCx<Settings>, ui_state: &PageState) -> Element {
        let mut sections = vec![Self::intro_section(cx)];
        sections.push(self.agents_section(cx, ui_state));
        sections.push(self.devices_section(cx, ui_state));
        page(sections)
    }

    fn intro_section(cx: &ViewCx<Settings>) -> SettingsSection {
        let add_device = cx.actions().bind_effect_invocation(
            Button::new(
                "settings.access.add-device",
                &t("settings-access-add-device"),
            )
            .role(ButtonRole::Primary)
            .with_icon(Icon::MonitorSmartphone),
            &SettingsEffect::from(DevicesAccessEffect::ShowPairDevice),
        );
        let connect = cx.actions().bind_effect_invocation(
            Button::new("settings.access.connect", &t("settings-access-connect"))
                .role(ButtonRole::Secondary)
                .with_icon(Icon::Plus),
            &SettingsEffect::from(DevicesAccessEffect::ShowConnect),
        );
        untitled(rows(vec![
            note(&t("settings-access-intro")),
            block(Box::new(
                Row::new()
                    .with_rhythm(ui::tokens::Rhythm::Tight)
                    .push(Box::new(add_device))
                    .push(Box::new(connect))
                    .push(Box::new(Spacer::new())),
            )),
        ]))
    }

    fn agents_section(&self, cx: &ViewCx<Settings>, state: &PageState) -> SettingsSection {
        let title = t("settings-access-agents");
        if let Some(note_text) = self.list_note() {
            return section(&title, rows(vec![note(&note_text)]));
        }
        let agents = self.agents();
        if agents.is_empty() {
            return section(&title, rows(vec![note(&t("settings-access-no-agents"))]));
        }
        let now = self.now();
        let items = agents
            .iter()
            .map(|agent| row(agent_item(cx, state, agent, now, self.leaves(&agent.id()))))
            .collect();
        section(&title, rows(items))
    }

    fn devices_section(&self, cx: &ViewCx<Settings>, state: &PageState) -> SettingsSection {
        let title = t("settings-access-browsers-computers");
        if let Some(note_text) = self.list_note() {
            return section(&title, rows(vec![note(&note_text)]));
        }
        let now = self.now();
        let items: Vec<Item> = self
            .entries()
            .iter()
            .map(|entry| {
                row(match entry {
                    Entry::Device(device) => {
                        let leaving = self.leaves(&hex::encode(device.key));
                        device_item(cx, state, device, now, leaving)
                    }
                    Entry::Browser(browser) => {
                        let leaving = browser.live().any(|key| self.leaves(&hex::encode(key.key)));
                        browser_item(cx, state, browser, now, leaving)
                    }
                })
            })
            .collect();
        if items.is_empty() {
            return section(&title, rows(vec![note(&t("settings-access-no-devices"))]));
        }
        section(&title, rows(items))
    }

    fn list_note(&self) -> Option<String> {
        if let Some(error) = &self.devices_read.error {
            tracing::warn!(target: "tilo::settings", %error, "the host did not list the person's devices");
            return Some(t("settings-access-unlisted"));
        }
        (!self.devices_read.answered).then(|| t("settings-access-reading"))
    }
}

#[derive(Clone)]
struct PageState {
    shown: LocalState<u64>,
    removing: LocalState<Option<String>>,
    naming: LocalState<Option<String>>,
    name_draft: LocalState<String>,
    details: DisclosureGroup,
}

impl PageState {
    fn ask_removal(&self, id: Option<String>) {
        self.removing.set(id);
        self.shown.set(self.shown.get().wrapping_add(1));
    }

    /// Ask what `device` should be called, with `current` in the field; `None`
    /// closes the ask.
    fn ask_rename(&self, device: Option<String>, current: &str) {
        self.name_draft.set(current.to_owned());
        self.naming.set(device);
        self.shown.set(self.shown.get().wrapping_add(1));
    }

    /// Whether the ask is open for the device `key`.
    fn naming(&self, key: &str) -> bool {
        self.naming.get().as_deref() == Some(key)
    }
}

fn app_of(grant: &Grant) -> String {
    if grant.script_key {
        t("settings-access-script-key")
    } else if grant.agent.is_empty() {
        t("settings-access-unnamed-agent")
    } else {
        grant.agent.clone()
    }
}

fn day(ms: i64) -> String {
    ui::format::date_of(&ui::format::UnixSecs(ms / 1000))
}

fn activity(online: bool, last_ms: Option<i64>, now: i64) -> Option<String> {
    if online {
        return Some(t("settings-access-active-now"));
    }
    let last = last_ms?;
    Some(t_args(
        "settings-access-last-active",
        &[(
            "when",
            ui::format::relative(now.saturating_sub(last) / 1000).into(),
        )],
    ))
}

fn agent_may(agent: &AgentEntry<'_>) -> String {
    match agent.workspace() {
        None => t("settings-access-may-all"),
        Some(workspace) => t_args(
            "settings-access-may-one",
            &[("workspace", workspace.into())],
        ),
    }
}

fn agent_lines(agent: &AgentEntry<'_>, now: i64) -> Vec<String> {
    let mut lines = vec![agent_may(agent)];
    lines.extend(activity(agent.active_now(), agent.last_active(), now));
    lines
}

fn browser_lines(browser: &BrowserEntry<'_>, now: i64) -> Vec<String> {
    let tabs = browser.live().count();
    let what = match browser.kind() {
        Some(kind) if browser.current() => t_args(
            "settings-access-browser-with-tabs",
            &[("browser", kind.to_owned().into()), ("count", tabs.into())],
        ),
        _ => t_args("settings-access-tabs", &[("count", tabs.into())]),
    };
    let mut lines = vec![what];
    lines.extend(activity(
        browser.current() || browser.online(),
        browser.last_active(),
        now,
    ));
    lines
}

fn device_lines(device: &DeviceView, now: i64) -> Vec<String> {
    let mut lines = Vec::new();
    if device.current && !device.label.is_empty() {
        lines.push(device.label.clone());
    }
    lines.extend(activity(
        device.current || device.online,
        device.last_active_ms,
        now,
    ));
    lines
}

/// What a row says under its name: that it is being removed, once the person
/// took it back here and until the list shows it gone, else `lines`.
fn lines_unless_leaving(leaving: bool, lines: Vec<String>) -> Vec<String> {
    if leaving {
        vec![t("settings-access-removing")]
    } else {
        lines
    }
}

fn fact(id: &str, label: &str, value: &str) -> Element {
    Box::new(ListRow::new(id, label).value(value).dense())
}

fn details(facts: Vec<Element>, extra: Vec<Element>) -> Box<dyn ui::widget::Widget> {
    let column = facts.into_iter().chain(extra).fold(
        Column::new().with_no_padding().with_intrinsic_height(),
        Column::push,
    );
    Box::new(column)
}

fn key_facts(
    id: &str,
    added: Option<i64>,
    ends: Option<i64>,
    keys: &[&DeviceView],
) -> Vec<Element> {
    let mut facts = Vec::new();
    if let Some(ms) = added {
        facts.push(fact(
            &format!("{id}.added"),
            &t("settings-access-added"),
            &day(ms),
        ));
    }
    if let Some(ms) = ends {
        facts.push(fact(
            &format!("{id}.ends"),
            &t("settings-access-ends"),
            &day(ms),
        ));
    }
    for (n, key) in keys.iter().enumerate() {
        facts.push(fact(
            &format!("{id}.key.{n}"),
            &t("settings-access-key"),
            &key.fingerprint,
        ));
    }
    facts
}

fn entry_row(
    state: &PageState,
    id: &str,
    title: &str,
    icon: Icon,
    lines: Vec<String>,
    removal: Option<Removal>,
    body: Box<dyn ui::widget::Widget>,
) -> Element {
    let mut lines = lines.into_iter();
    let mut row = DisclosureRow::keyed(title, body, &state.details, id)
        .with_id(format!("settings.access.{id}"))
        .icon(icon);
    let asking = removal.is_some() && state.removing.get().as_deref() == Some(id);
    if asking {
        if let Some(removal) = &removal {
            row = row.secondary(removal.consequence.clone());
        }
    } else {
        if let Some(line) = lines.next() {
            row = row.secondary(line);
        }
        if let Some(line) = lines.next() {
            row = row.tertiary(line);
        }
    }
    if let Some(removal) = removal {
        row = row.trailing(removal.control(state, id, asking));
    }
    Box::new(row)
}

struct Removal {
    confirm: Button,
    consequence: String,
}

impl Removal {
    fn new(
        cx: &ViewCx<Settings>,
        id: &str,
        live: &(impl ui::tilo_actions::Exports + ?Sized),
        effect: DevicesAccessEffect,
        consequence: String,
    ) -> Self {
        let confirm = crate::on_row::bind_invocation(
            &cx.dispatcher(),
            live,
            Button::new(
                &format!("settings.access.{id}.remove"),
                &t("settings-access-remove"),
            )
            .role(ButtonRole::Destructive),
            effect,
        );
        Self {
            confirm,
            consequence,
        }
    }

    fn control(self, state: &PageState, id: &str, asking: bool) -> Element {
        if !asking {
            let ask = state.clone();
            let asked = id.to_owned();
            return Box::new(
                Button::new(
                    &format!("settings.access.{id}.ask-remove"),
                    &t("settings-access-remove"),
                )
                .on_open(move || ask.ask_removal(Some(asked.clone()))),
            );
        }
        let cancel = state.clone();
        // The pair takes the width it needs and no more. A line of controls
        // fills its parent otherwise, and a row whose trailing control fills
        // it leaves nothing for its name and what removing does.
        Box::new(
            Container::new().hug().with_child(Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(
                        Button::new(
                            &format!("settings.access.{id}.cancel"),
                            &t("settings-access-cancel"),
                        )
                        .on_open(move || cancel.ask_removal(None)),
                    ))
                    .push(Box::new(self.confirm)),
            )),
        )
    }
}

/// `draft` is what is typed for the device being renamed now: the row it is
/// published on carries it, since the name the app reads is the row's.
fn device_row(device: &DeviceView, draft: &str) -> LiveRow {
    let key = hex::encode(device.key);
    let bindings = ui::tilo_resource::RowBindings::new()
        // Bound whole: a row whose action asks for text makes pressing the row
        // collect its label as that text, which renamed a device to the name it
        // already had. The name is the row's, published as the person types.
        .bind(
            DevicesAccessEffect::RENAME_DEVICE,
            [
                ("device", key.clone().into()),
                ("name", draft.to_owned().into()),
            ],
        )
        .spec(revocation(DevicesAccessEffect::RevokeDevice {
            device: key,
            reason: REMOVED.to_owned(),
        }));
    LiveRow::typed_with::<Device>(
        format!("settings.device:{}", hex::encode(device.key)),
        device.label.clone(),
        &bindings,
    )
}

/// Taking a device or a browser back, as its row offers it: once, bound to
/// what it ends, asking why. The reasons are the parameter's choices, so the
/// page's own button, the search, an agent and the screen reader name one of
/// them to the same action. An address carries one action: the host refuses
/// a surface that offers an address twice, and withdraws every action of the
/// app with it.
///
/// What it says of itself is what the person reads when they are asked
/// before it runs: what removing does, said to them, as the row says it.
fn revocation(effect: DevicesAccessEffect) -> ui::tilo_actions::ActionSpec {
    SettingsEffect::from(effect)
        .bound_asking(&["reason"])
        .with_description(t("settings-access-remove-device-consequence"))
}

/// Naming `device`: a button that opens a field holding the name it reads by,
/// and Save hands the new one to the host that keeps the person's resource.
fn rename_control(
    cx: &ViewCx<Settings>,
    state: &PageState,
    id: &str,
    device: &DeviceView,
    live: &(impl ui::tilo_actions::Exports + ?Sized),
) -> Element {
    let key = hex::encode(device.key);
    if !state.naming(&key) {
        let ask = state.clone();
        let asked = key;
        let current = device.label.clone();
        return Box::new(
            Button::new(
                &format!("settings.access.{id}.rename"),
                &t("settings-access-rename"),
            )
            .on_open(move || ask.ask_rename(Some(asked.clone()), &current)),
        );
    }
    let input = TextInput::from_state(&state.name_draft)
        .with_id(&format!("settings.access.{id}.name"))
        .with_accessible_name(t("settings-access-name"));
    let draft = state.name_draft.get();
    let save = crate::on_row::bind_invocation(
        &cx.dispatcher(),
        live,
        Button::new(
            &format!("settings.access.{id}.save-name"),
            &t("settings-access-save-name"),
        ),
        DevicesAccessEffect::RenameDevice {
            device: key,
            name: draft,
        },
    );
    let ask = state.clone();
    let cancel = Button::new(
        &format!("settings.access.{id}.cancel-name"),
        &t("settings-access-cancel"),
    )
    .on_open(move || ask.ask_rename(None, ""));
    Box::new(
        Row::new()
            .no_wrap()
            .push(Box::new(input))
            .push(Box::new(save))
            .push(Box::new(cancel)),
    )
}

fn device_item(
    cx: &ViewCx<Settings>,
    state: &PageState,
    device: &DeviceView,
    now: i64,
    leaving: bool,
) -> Element {
    let key = hex::encode(device.key);
    let id = format!("device.{key}");
    let title = if device.current {
        t("settings-access-this-computer")
    } else {
        device.label.clone()
    };
    let live = device_row(device, &state.name_draft.get());
    let removal = (!device.current && !leaving).then(|| {
        Removal::new(
            cx,
            &id,
            &live,
            DevicesAccessEffect::RevokeDevice {
                device: key.clone(),
                reason: REMOVED.to_owned(),
            },
            t("settings-access-remove-device-consequence"),
        )
    });
    let mut extra = Vec::new();
    if device.current {
        extra.push(caption(&t("settings-access-remove-from-another")));
    }
    extra.push(rename_control(cx, state, &id, device, &live));
    entry_row(
        state,
        &id,
        &title,
        crate::device_fleet::device_icon(&device.label),
        lines_unless_leaving(leaving, device_lines(device, now)),
        removal,
        details(
            key_facts(&id, device.issued_ms, device.expires_ms, &[device]),
            extra,
        ),
    )
}

fn caption(text: &str) -> Element {
    provider_settings_ui::widgets::caption(text)
}

fn browser_row(browser: &BrowserEntry<'_>) -> LiveRow {
    let bindings = ui::tilo_resource::RowBindings::new().spec(revocation(
        DevicesAccessEffect::RevokeBrowser {
            browser: browser.id.to_owned(),
            reason: REMOVED.to_owned(),
        },
    ));
    LiveRow::typed_with::<Device>(
        format!("settings.device-browser:{}", browser.id),
        browser.label(),
        &bindings,
    )
}

fn browser_item(
    cx: &ViewCx<Settings>,
    state: &PageState,
    browser: &BrowserEntry<'_>,
    now: i64,
    leaving: bool,
) -> Element {
    let id = format!("browser.{}", browser.id);
    let removal = (browser.revocable() && !leaving).then(|| {
        Removal::new(
            cx,
            &id,
            &browser_row(browser),
            DevicesAccessEffect::RevokeBrowser {
                browser: browser.id.to_owned(),
                reason: REMOVED.to_owned(),
            },
            t("settings-access-remove-device-consequence"),
        )
    });
    let live: Vec<&DeviceView> = browser.live().collect();
    let added = live.iter().filter_map(|key| key.issued_ms).min();
    let mut extra = Vec::new();
    if browser.current() {
        extra.push(caption(&t("settings-access-remove-from-another")));
    }
    entry_row(
        state,
        &id,
        &browser.label(),
        Icon::Globe,
        lines_unless_leaving(leaving, browser_lines(browser, now)),
        removal,
        details(key_facts(&id, added, None, &live), extra),
    )
}

fn agent_row(agent: &AgentEntry<'_>, draft: &str) -> LiveRow {
    let id = agent.id();
    let mut bindings = ui::tilo_resource::RowBindings::new().spec(
        SettingsEffect::from(agent.removal())
            .bound()
            // What the person reads when they are asked before it runs.
            .with_description(t("settings-access-remove-agent-consequence")),
    );
    // A computer an agent works on is a device of the person's too: they name
    // it here, where two of them enrolled under one name.
    if let Some(key) = agent.key {
        bindings = bindings.bind(
            DevicesAccessEffect::RENAME_DEVICE,
            [
                ("device", hex::encode(key.key).into()),
                ("name", draft.to_owned().into()),
            ],
        );
    }
    if let Some(grant) = agent.grant.filter(|grant| !grant.sessions.is_empty()) {
        bindings = bindings.spec(stop_session(&id, &grant.sessions));
    }
    LiveRow::typed_with::<AgentGrant>(
        format!("settings.agent-grant:{id}"),
        agent.label(),
        &bindings,
    )
}

/// Ending one of an agent's open sessions, as its row offers it: once, bound
/// to the grant, asking which. The sessions open now are the parameter's
/// choices, by the names History gives them.
fn stop_session(grant: &str, open: &[mcp_server::AgentSession]) -> ui::tilo_actions::ActionSpec {
    let effect = SettingsEffect::from(DevicesAccessEffect::StopSession {
        grant: grant.to_owned(),
        session: String::new(),
    });
    let mut params = effect.bound_asking(&["session"]).params().to_vec();
    for param in &mut params {
        param.choices = open
            .iter()
            .map(|session| session.name.clone().into())
            .collect();
    }
    effect.bound_asking_params(params)
}

fn agent_item(
    cx: &ViewCx<Settings>,
    state: &PageState,
    agent: &AgentEntry<'_>,
    now: i64,
    leaving: bool,
) -> Element {
    let key = agent.id();
    let id = format!("agent.{key}");
    let live = agent_row(agent, &state.name_draft.get());
    let dispatcher = cx.dispatcher();
    let removal = (!leaving).then(|| {
        Removal::new(
            cx,
            &id,
            &live,
            agent.removal(),
            t("settings-access-remove-agent-consequence"),
        )
    });
    let mut extra: Vec<Element> = Vec::new();
    if let Some(grant) = agent.grant {
        extra.push(caption(&t(if grant.browser_pages {
            "settings-access-browser-pages-on"
        } else {
            "settings-access-browser-pages-off"
        })));
        for session in &grant.sessions {
            let stop = crate::on_row::bind_invocation(
                &dispatcher,
                &live,
                Button::new(
                    &format!("settings.access.{id}.stop.{}", session.name),
                    &t_args(
                        "settings-access-stop-session",
                        &[("session", session.name.clone().into())],
                    ),
                ),
                DevicesAccessEffect::StopSession {
                    grant: key.clone(),
                    session: session.name.clone(),
                },
            );
            extra.push(Box::new(
                Row::new()
                    .push(Box::new(stop))
                    .push(Box::new(Spacer::new())),
            ));
        }
    }
    let keys: Vec<&DeviceView> = agent.key.into_iter().collect();
    if let Some(device) = agent.key {
        extra.push(rename_control(cx, state, &id, device, &live));
    }
    entry_row(
        state,
        &id,
        &agent.label(),
        Icon::Bot,
        lines_unless_leaving(leaving, agent_lines(agent, now)),
        removal,
        details(
            key_facts(&id, agent.connected(), agent.expires(), &keys),
            extra,
        ),
    )
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    model.borrow_mut().poll(&held.remote);
    let state = PageState {
        shown: held.shown.clone(),
        removing: held.removing.clone(),
        naming: held.naming.clone(),
        name_draft: held.name_draft.clone(),
        details: held.details.clone(),
    };
    Some(cx.switch(&held.shown, move |cx, _| model.borrow().page(cx, &state)))
}

#[cfg(test)]
mod tests;
