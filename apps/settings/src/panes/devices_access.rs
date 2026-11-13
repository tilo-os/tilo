//! Devices and access: everything that acts as the person, in one place.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow, ViewCx};
use ui::icons::Icon;
use ui::tilo_actions::remote::agent_door::{self, Door, Grant, Reach};
use ui::tilo_actions::remote::{Confirmation, InvokeArgument, device_pairing, session_actions};
use ui::tilo_resource::{self, Effect as _};
use ui::widget::{
    Button, ButtonRole, Column, DisclosureGroup, DisclosureRow, Element, ListRow, Row, Spacer,
};

use crate::area::AreaContext;
use crate::device_fleet::DeviceView;
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, block, note, page, row, rows, section, thing_row, untitled,
};
use ui::tilo_actions::remote::identity_admin::KeyKind;

crate::effects::effect_family!(DevicesAccessEffect => answer);

const REVOKE_REASONS: [&str; 4] = ["retired", "lost", "stolen", "compromised"];

const REMOVED: &str = "retired";

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DevicesAccessEffect {
    /// End a device's or an agent's access to everything of the person's, for good.
    #[effect(job, row, destructive)]
    RevokeDevice {
        #[param("The key, in hex")]
        device: String,
        #[param("retired, lost, stolen or compromised", choices = REVOKE_REASONS)]
        reason: String,
    },
    /// End every tab's access of one browser of the person's, for good.
    #[effect(job, row, destructive)]
    RevokeBrowser {
        #[param("The browser's id, as the device list names it")]
        browser: String,
        #[param("retired, lost, stolen or compromised", choices = REVOKE_REASONS)]
        reason: String,
    },
    /// Take an agent's grant back: its runs end now and it gets in no more.
    #[effect(job, row, destructive)]
    RevokeAgent {
        #[param("The grant's id, as the door lists it")]
        grant: String,
    },
    /// End an agent's open run, keeping its grant so it can come back.
    #[effect(job, row)]
    StopRun {
        #[param("The grant's id, as the door lists it")]
        grant: String,
        #[param("The run's name, as History names it")]
        run: String,
    },
    /// Approve or decline a device asking to be the person.
    #[effect(job, row)]
    AnswerDevice {
        #[param("The ask's id, as the list of asking devices names it")]
        ask: String,
        #[param("The name the device is let in under")]
        label: String,
        approve: bool,
    },
    /// Show how an agent or another computer connects as the person.
    #[effect(view, label = "settings-access-connect")]
    ShowConnect,
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
    Ok(Effected::Pending)
}

crate::area::pane_area! {
    DevicesAccessArea;
    fn poll(&self) {
        DevicesAccessArea::poll(self);
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
    details: DisclosureGroup,
}

#[derive(Clone, Debug, Default)]
pub struct AccessSample {
    pub devices: Vec<DeviceView>,
    pub door: Door,
    pub asking: Vec<Asking>,
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
            details: DisclosureGroup::new(),
        }
    }

    pub fn model(&self) -> &Rc<RefCell<AccessModel>> {
        self.model.get_or_init(|| {
            Rc::new(RefCell::new(match &self.scene {
                Some(sample) => AccessModel::shown(sample.clone()),
                None => AccessModel::asking(&self.remote),
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
        let model = self.model().borrow();
        let devices = model.entries().into_iter().filter_map(|entry| match entry {
            Entry::Device(device) if !device.current => Some(device_row(device)),
            Entry::Browser(browser) if browser.revocable() => Some(browser_row(&browser)),
            Entry::Device(_) | Entry::Browser(_) => None,
        });
        let agents = model.agents().into_iter().map(|agent| agent_row(&agent));
        let asking = model.asking.iter().map(asking_row);
        devices.chain(agents).chain(asking).collect()
    }
}

pub(crate) struct Device;

impl tilo_resource::words::RowType for Device {
    const KIND: &'static str = "settings.device";
    const LABEL: &'static str = "Device";
}

pub(crate) struct AgentGrant;

impl tilo_resource::words::RowType for AgentGrant {
    const KIND: &'static str = "settings.agent-grant";
    const LABEL: &'static str = "Agent";
}

pub(crate) struct AskingDevice;

impl tilo_resource::words::RowType for AskingDevice {
    const KIND: &'static str = "settings.device-asking";
    const LABEL: &'static str = "Device asking to join";
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Asking {
    pub ask: String,
    pub label: String,
    pub code: String,
    pub via: String,
}

#[derive(serde::Deserialize)]
struct AskingAnswer {
    asking: Vec<Asking>,
}

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
            Some(grant) if !grant.runs_on.is_empty() && !grant.agent.is_empty() => t_args(
                "settings-access-agent-on",
                &[
                    ("agent", grant.agent.clone().into()),
                    ("place", grant.runs_on.clone().into()),
                ],
            ),
            Some(grant) => app_of(grant),
            None => t("settings-access-unnamed-agent"),
        }
    }

    fn active_now(&self) -> bool {
        self.key.is_some_and(|key| key.online)
            || self.grant.is_some_and(|grant| !grant.runs.is_empty())
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
}

#[derive(Default)]
struct Asked {
    call: Option<ui::InvocationCall>,
    error: Option<String>,
    answered: bool,
}

pub struct AccessModel {
    devices: Vec<DeviceView>,
    devices_asked: Asked,
    door: Door,
    door_followed: Option<ui::Followed>,
    door_seen: u64,
    asking: Vec<Asking>,
    asking_asked: Asked,
    pending: Vec<Pending>,
    now_ms: Option<i64>,
}

impl AccessModel {
    fn asking(remote: &ui::RemoteActions) -> Self {
        let mut model = Self::empty();
        model.door_followed =
            Some(remote.follow(agent_door::target(agent_door::DOOR), InvokeArgument::None));
        model.ask_lists(remote);
        model
    }

    fn shown(sample: AccessSample) -> Self {
        let mut model = Self::empty();
        model.devices = sample.devices;
        model.devices_asked.answered = true;
        model.door = sample.door;
        model.asking = sample.asking;
        model.asking_asked.answered = true;
        model.now_ms = Some(sample.now_ms);
        model
    }

    fn empty() -> Self {
        Self {
            devices: Vec::new(),
            devices_asked: Asked::default(),
            door: Door::default(),
            door_followed: None,
            door_seen: 0,
            asking: Vec::new(),
            asking_asked: Asked::default(),
            pending: Vec::new(),
            now_ms: None,
        }
    }

    fn now(&self) -> i64 {
        self.now_ms
            .unwrap_or_else(|| i64::try_from(ui::time::unix_now_ms()).unwrap_or(i64::MAX))
    }

    fn ask_lists(&mut self, remote: &ui::RemoteActions) {
        self.devices_asked.call = Some(crate::device_fleet::list(remote));
        self.asking_asked.call = Some(remote.invoke(
            device_pairing::target(device_pairing::ASKING),
            InvokeArgument::None,
            None,
        ));
    }

    fn act(
        &mut self,
        remote: &ui::RemoteActions,
        effect: &DevicesAccessEffect,
        reply: Option<EffectReply>,
    ) -> Result<(), String> {
        let call = match effect {
            DevicesAccessEffect::RevokeDevice { device, reason } => {
                let key = self.device(device)?;
                if !REVOKE_REASONS.contains(&reason.as_str()) {
                    return Err(t("settings-access-reason-unknown"));
                }
                crate::device_fleet::revoke(remote, &key, reason)
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
                self.pending.push(Pending { calls, reply });
                return Ok(());
            }
            DevicesAccessEffect::RevokeAgent { grant } => {
                self.grant(grant)?;
                remote.invoke(
                    agent_door::target(agent_door::REVOKE),
                    agent_door::argument(&agent_door::GrantRef {
                        grant: grant.clone(),
                    }),
                    Some(Confirmation::Confirmed),
                )
            }
            DevicesAccessEffect::StopRun { grant, run } => {
                self.grant(grant)?;
                remote.invoke(
                    agent_door::target(agent_door::STOP),
                    agent_door::argument(&agent_door::RunRef {
                        grant: grant.clone(),
                        run: run.clone(),
                    }),
                    None,
                )
            }
            DevicesAccessEffect::AnswerDevice {
                ask,
                label,
                approve,
            } => {
                let mut arguments = serde_json::Map::new();
                arguments.insert("ask".into(), ask.clone().into());
                arguments.insert("label".into(), label.clone().into());
                arguments.insert("approve".into(), (*approve).into());
                arguments.insert("confirm".into(), true.into());
                remote.invoke(
                    device_pairing::target(device_pairing::ANSWER),
                    InvokeArgument::Json(arguments),
                    Some(Confirmation::Confirmed),
                )
            }
            DevicesAccessEffect::ShowConnect => remote.invoke(
                session_actions::target(session_actions::SHOW_CONNECT),
                InvokeArgument::Json(serde_json::Map::new()),
                None,
            ),
        };
        self.pending.push(Pending {
            calls: vec![call],
            reply,
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
        self.door
            .grants
            .iter()
            .find(|listed| listed.id == grant)
            .ok_or_else(|| t("settings-access-no-such-agent"))
    }

    fn poll(&mut self, remote: &ui::RemoteActions) -> bool {
        if self.devices_asked.call.is_some()
            || self.asking_asked.call.is_some()
            || !self.pending.is_empty()
        {
            remote.poll();
        }
        let mut changed = self.poll_door();
        if let Some(answer) = settle(&mut self.devices_asked) {
            match answer.and_then(crate::device_fleet::devices) {
                Ok(devices) => self.devices = devices,
                Err(error) => self.devices_asked.error = Some(error),
            }
            changed = true;
        }
        if let Some(answer) = settle(&mut self.asking_asked) {
            match answer.and_then(|value| {
                serde_json::from_value::<AskingAnswer>(value.unwrap_or_default())
                    .map_err(|_| t("settings-access-unreadable"))
            }) {
                Ok(answered) => self.asking = answered.asking,
                Err(_) => self.asking.clear(),
            }
            changed = true;
        }
        let mut settled = false;
        for pending in std::mem::take(&mut self.pending) {
            let answers: Option<Vec<_>> = pending
                .calls
                .iter()
                .map(crate::device_fleet::settled)
                .collect();
            match answers {
                Some(answers) => {
                    let failed = answers.iter().position(Result::is_err);
                    let answer = match failed {
                        Some(at) => answers.into_iter().nth(at),
                        None => answers.into_iter().last(),
                    };
                    if let (Some(reply), Some(answer)) = (pending.reply, answer) {
                        reply.finish(answer);
                    }
                    settled = true;
                }
                None => self.pending.push(pending),
            }
        }
        if settled {
            self.ask_lists(remote);
            changed = true;
        }
        changed
    }

    fn poll_door(&mut self) -> bool {
        use ui::tilo_actions::InvocationUpdate;
        let Some(followed) = &self.door_followed else {
            return false;
        };
        let call = followed.call();
        let revision = call.revision();
        if revision == self.door_seen {
            return false;
        }
        self.door_seen = revision;
        let value = match call.get() {
            ui::Arrival::Ready(InvocationUpdate::Item { value, .. }) => value,
            ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
                output.value.unwrap_or(serde_json::Value::Null)
            }
            ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                tracing::debug!(target: "tilo::settings", error = %error.message, "no agent door answers here");
                return false;
            }
            ui::Arrival::Failed(error) => {
                tracing::debug!(target: "tilo::settings", %error, "no agent door answers here");
                return false;
            }
            ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => return false,
        };
        match serde_json::from_value::<Door>(value) {
            Ok(door) => {
                self.door = door;
                true
            }
            Err(error) => {
                tracing::warn!(target: "tilo::settings", %error, "the agent door answered something unreadable");
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
            self.door
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
        for grant in &self.door.grants {
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
        if !self.asking.is_empty() {
            sections.push(self.requests_section(cx));
        }
        sections.push(self.agents_section(cx, ui_state));
        sections.push(self.devices_section(cx, ui_state));
        page(sections)
    }

    fn intro_section(cx: &ViewCx<Settings>) -> SettingsSection {
        let connect = cx.actions().bind_effect_invocation(
            Button::new("settings.access.connect", &t("settings-access-connect"))
                .role(ButtonRole::Primary)
                .with_icon(Icon::Plus),
            &SettingsEffect::from(DevicesAccessEffect::ShowConnect),
        );
        untitled(rows(vec![
            note(&t("settings-access-intro")),
            block(Box::new(
                Row::new()
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
            .map(|agent| row(agent_item(cx, state, agent, now)))
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
                    Entry::Device(device) => device_item(cx, state, device, now),
                    Entry::Browser(browser) => browser_item(cx, state, browser, now),
                })
            })
            .collect();
        if items.is_empty() {
            return section(&title, rows(vec![note(&t("settings-access-no-devices"))]));
        }
        section(&title, rows(items))
    }

    fn list_note(&self) -> Option<String> {
        if let Some(error) = &self.devices_asked.error {
            tracing::warn!(target: "tilo::settings", %error, "the host did not list the person's devices");
            return Some(t("settings-access-unlisted"));
        }
        (!self.devices_asked.answered).then(|| t("settings-access-reading"))
    }

    fn requests_section(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let items = self
            .asking
            .iter()
            .map(|asking| asking_item(cx, asking))
            .collect();
        section(&t("settings-access-requests"), rows(items))
    }
}

#[derive(Clone)]
struct PageState {
    shown: LocalState<u64>,
    removing: LocalState<Option<String>>,
    details: DisclosureGroup,
}

impl PageState {
    fn ask_removal(&self, id: Option<String>) {
        self.removing.set(id);
        self.shown.set(self.shown.get().wrapping_add(1));
    }
}

fn settle(asked: &mut Asked) -> Option<Result<Option<serde_json::Value>, String>> {
    let answer = crate::device_fleet::settled(asked.call.as_ref()?)?;
    asked.call = None;
    asked.answered = true;
    asked.error = None;
    Some(answer.map(|output| output.value))
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
    } else if device.can_host {
        lines.push(t("settings-access-hosts"));
    }
    lines.extend(activity(
        device.current || device.online,
        device.last_active_ms,
        now,
    ));
    lines
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
        live: &ui::tilo_actions::ResourceSpec,
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
                .on_view(move || ask.ask_removal(Some(asked.clone()))),
            );
        }
        let cancel = state.clone();
        Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(
                    Button::new(
                        &format!("settings.access.{id}.cancel"),
                        &t("settings-access-cancel"),
                    )
                    .on_view(move || cancel.ask_removal(None)),
                ))
                .push(Box::new(self.confirm)),
        )
    }
}

fn device_row(device: &DeviceView) -> LiveRow {
    let key = hex::encode(device.key);
    let mut row = LiveRow::typed::<Device>(format!("settings.device:{key}"), device.label.clone());
    for reason in REVOKE_REASONS {
        row = row.operation(
            SettingsEffect::from(DevicesAccessEffect::RevokeDevice {
                device: key.clone(),
                reason: reason.to_owned(),
            })
            .bound(),
        );
    }
    row
}

fn device_item(cx: &ViewCx<Settings>, state: &PageState, device: &DeviceView, now: i64) -> Element {
    let key = hex::encode(device.key);
    let id = format!("device.{key}");
    let title = if device.current {
        t("settings-access-this-computer")
    } else {
        device.label.clone()
    };
    let removal = (!device.current).then(|| {
        Removal::new(
            cx,
            &id,
            &device_row(device).into_resource(),
            DevicesAccessEffect::RevokeDevice {
                device: key.clone(),
                reason: REMOVED.to_owned(),
            },
            t("settings-access-remove-device-consequence"),
        )
    });
    let mut extra = Vec::new();
    if device.current && device.can_host {
        extra.push(caption(&t("settings-access-hosts")));
    }
    if device.current {
        extra.push(caption(&t("settings-access-remove-from-another")));
    }
    entry_row(
        state,
        &id,
        &title,
        crate::device_fleet::device_icon(&device.label),
        device_lines(device, now),
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
    let mut row = LiveRow::typed::<Device>(
        format!("settings.device-browser:{}", browser.id),
        browser.label(),
    );
    for reason in REVOKE_REASONS {
        row = row.operation(
            SettingsEffect::from(DevicesAccessEffect::RevokeBrowser {
                browser: browser.id.to_owned(),
                reason: reason.to_owned(),
            })
            .bound(),
        );
    }
    row
}

fn browser_item(
    cx: &ViewCx<Settings>,
    state: &PageState,
    browser: &BrowserEntry<'_>,
    now: i64,
) -> Element {
    let id = format!("browser.{}", browser.id);
    let removal = browser.revocable().then(|| {
        Removal::new(
            cx,
            &id,
            &browser_row(browser).into_resource(),
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
        browser_lines(browser, now),
        removal,
        details(key_facts(&id, added, None, &live), extra),
    )
}

fn agent_row(agent: &AgentEntry<'_>) -> LiveRow {
    let id = agent.id();
    let mut row = LiveRow::typed::<AgentGrant>(format!("settings.agent-grant:{id}"), agent.label())
        .operation(SettingsEffect::from(agent.removal()).bound());
    for run in agent.grant.iter().flat_map(|grant| &grant.runs) {
        row = row.operation(
            SettingsEffect::from(DevicesAccessEffect::StopRun {
                grant: id.clone(),
                run: run.name.clone(),
            })
            .bound(),
        );
    }
    row
}

fn agent_item(
    cx: &ViewCx<Settings>,
    state: &PageState,
    agent: &AgentEntry<'_>,
    now: i64,
) -> Element {
    let key = agent.id();
    let id = format!("agent.{key}");
    let live = agent_row(agent).into_resource();
    let dispatcher = cx.dispatcher();
    let removal = Removal::new(
        cx,
        &id,
        &live,
        agent.removal(),
        t("settings-access-remove-agent-consequence"),
    );
    let mut extra: Vec<Element> = Vec::new();
    if let Some(grant) = agent.grant {
        extra.push(caption(&t(if grant.browser_pages {
            "settings-access-browser-pages-on"
        } else {
            "settings-access-browser-pages-off"
        })));
        for run in &grant.runs {
            let stop = crate::on_row::bind_invocation(
                &dispatcher,
                &live,
                Button::new(
                    &format!("settings.access.{id}.stop.{}", run.name),
                    &t_args(
                        "settings-access-stop-run",
                        &[("run", run.name.clone().into())],
                    ),
                ),
                DevicesAccessEffect::StopRun {
                    grant: key.clone(),
                    run: run.name.clone(),
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
    entry_row(
        state,
        &id,
        &agent.label(),
        Icon::Bot,
        agent_lines(agent, now),
        Some(removal),
        details(
            key_facts(&id, agent.connected(), agent.expires(), &keys),
            extra,
        ),
    )
}

fn asking_row(asking: &Asking) -> LiveRow {
    let mut row = LiveRow::typed::<AskingDevice>(
        format!("settings.device-asking:{}", asking.ask),
        asking.label.clone(),
    );
    for approve in [true, false] {
        row = row.operation(
            SettingsEffect::from(DevicesAccessEffect::AnswerDevice {
                ask: asking.ask.clone(),
                label: asking.label.clone(),
                approve,
            })
            .bound(),
        );
    }
    row
}

fn asking_lines(asking: &Asking) -> Vec<String> {
    vec![
        t_args(
            "settings-access-asking-code",
            &[("code", asking.code.clone().into())],
        ),
        t_args(
            "settings-access-asking-via",
            &[("device", asking.via.clone().into())],
        ),
    ]
}

fn asking_item(cx: &ViewCx<Settings>, asking: &Asking) -> Item {
    let live = asking_row(asking).into_resource();
    let dispatcher = cx.dispatcher();
    let answer = |approve: bool| DevicesAccessEffect::AnswerDevice {
        ask: asking.ask.clone(),
        label: asking.label.clone(),
        approve,
    };
    let decline = crate::on_row::bind_invocation(
        &dispatcher,
        &live,
        Button::new(
            &format!("settings.access.asking.{}.decline", asking.ask),
            &t("settings-access-decline"),
        ),
        answer(false),
    );
    let approve = crate::on_row::bind_invocation(
        &dispatcher,
        &live,
        Button::new(
            &format!("settings.access.asking.{}.approve", asking.ask),
            &t("settings-access-approve"),
        )
        .role(ButtonRole::Primary),
        answer(true),
    );
    row(Box::new(
        thing_row(
            &format!("settings.access.asking.{}", asking.ask),
            &asking.label,
            &asking_lines(asking),
            Some(Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(decline))
                    .push(Box::new(approve)),
            )),
        )
        .icon(Icon::MonitorSmartphone),
    ))
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    model.borrow_mut().poll(&held.remote);
    let state = PageState {
        shown: held.shown.clone(),
        removing: held.removing.clone(),
        details: held.details.clone(),
    };
    Some(cx.switch(&held.shown, move |cx, _| model.borrow().page(cx, &state)))
}

#[cfg(test)]
mod tests;
