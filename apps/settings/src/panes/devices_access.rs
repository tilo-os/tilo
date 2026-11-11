//! Devices and access: everything that acts as the person, in one place,
//! the same on the desktop and the web.
//!
//! The person has one root identity. Each of their devices and each agent
//! they let in holds a key the root certified, and taking one back is the
//! root's signed revocation. The pane lists those keys in three lists:
//!
//! - **Devices**: the person's computers, phones and browsers, as the host's
//!   identity actions answer them ([`identity_admin::DEVICES`]), each
//!   revocable as no longer used, lost or stolen
//!   ([`identity_admin::REVOKE`]).
//! - **Agents**: what the agent door let in ([`agent_door::DOOR`], followed
//!   live), grouped by the app that connected (Claude Code, an MCP client,
//!   a script's key), each saying where it runs, what it reaches, until when,
//!   whether it may operate web pages in Browser, when it last acted and the
//!   runs open now; a run can be stopped ([`agent_door::STOP`]) and a grant
//!   revoked ([`agent_door::REVOKE`]). How to connect another agent is the
//!   door's address and the one line each client runs.
//! - **Requests**: a device of the person's asking to be them
//!   ([`device_pairing::ASKING`]), approved or declined here as its
//!   notification does ([`device_pairing::ANSWER`]).
//!
//! Every list is the host's answer: nothing here holds a key or a secret,
//! and a host that answers none of them leaves its list saying so.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow, ViewCx};
use ui::tilo_actions::remote::agent_door::{self, Client, Door, Grant, Reach};
use ui::tilo_actions::remote::{Confirmation, InvokeArgument, device_pairing};
use ui::tilo_resource::{self, Effect as _};
use ui::widget::{Element, RowMenu};

use crate::area::AreaContext;
use crate::device_fleet::DeviceView;
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Item, SettingsSection, note, page, row, rows, section, thing_row, value_row,
};

crate::effects::effect_family!(DevicesAccessEffect => answer);

/// Why a device's access ends, as the root's revocation records it.
const REVOKE_REASONS: [&str; 4] = ["retired", "lost", "stolen", "compromised"];

/// What the person does to the keys acting as them.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DevicesAccessEffect {
    /// End a device's access to everything of the person's, for good.
    #[effect(job, row, destructive, label = "Revoke device")]
    RevokeDevice {
        #[param("The device's key, in hex")]
        device: String,
        #[param("retired, lost, stolen or compromised", choices = REVOKE_REASONS)]
        reason: String,
    },
    /// Take an agent's grant back: its runs end now and it gets in no more.
    #[effect(job, row, destructive, label = "Revoke agent")]
    RevokeAgent {
        #[param("The grant's id, as the door lists it")]
        grant: String,
    },
    /// End an agent's open run, keeping its grant so it can come back.
    #[effect(job, row, label = "Stop run")]
    StopRun {
        #[param("The grant's id, as the door lists it")]
        grant: String,
        #[param("The run's name, as History names it")]
        run: String,
    },
    /// Approve or decline a device asking to be the person.
    #[effect(job, row, label = "Answer device")]
    AnswerDevice {
        #[param("The ask's id, as the list of asking devices names it")]
        ask: String,
        #[param("The name the device is let in under")]
        label: String,
        approve: bool,
    },
}

/// Answer `effect` through the host, the caller hearing how it ended.
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

/// The window's [`DevicesAccessArea`].
pub fn area(app: &SettingsApp) -> Option<&DevicesAccessArea> {
    app.areas().find()
}

/// Devices and access in a window: the model, made when the page is first
/// shown, and what the page is keyed by.
pub struct DevicesAccessArea {
    remote: ui::RemoteActions,
    /// A screenshot's lists in place of the host's.
    scene: Option<AccessSample>,
    model: OnceCell<Rc<RefCell<AccessModel>>>,
    shown: LocalState<u64>,
}

/// The lists a screenshot shows.
#[derive(Clone, Debug, Default)]
pub struct AccessSample {
    pub devices: Vec<DeviceView>,
    pub door: Door,
    pub asking: Vec<Asking>,
}

impl DevicesAccessArea {
    pub fn new(cx: &AreaContext, remote: ui::RemoteActions) -> Self {
        Self {
            remote,
            scene: cx.scene().and_then(|scene| scene.access.clone()),
            model: OnceCell::new(),
            shown: LocalState::new(0),
        }
    }

    /// The model, asking the host for its lists from when it is made.
    pub fn model(&self) -> &Rc<RefCell<AccessModel>> {
        self.model.get_or_init(|| {
            Rc::new(RefCell::new(match &self.scene {
                Some(sample) => AccessModel::shown(sample.clone()),
                None => AccessModel::asking(&self.remote),
            }))
        })
    }

    /// Whether the page was shown, so its rows are read.
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// Take in the host's latest answers, if the model exists, and move what
    /// the page is keyed by when they changed.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get()
            && let Ok(mut model) = model.try_borrow_mut()
            && model.poll(&self.remote)
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    /// Each device, agent and asking device as a row of the published
    /// surface, offering what can be done to it now.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let devices = model
            .devices
            .iter()
            .filter(|device| !device.current && !device.revoked)
            .map(device_row);
        let grants = model.door.grants.iter().map(grant_row);
        let asking = model.asking.iter().map(asking_row);
        devices.chain(grants).chain(asking).collect()
    }
}

/// A device of the person's, as the type its rows are items of.
pub(crate) struct Device;

impl tilo_resource::words::RowType for Device {
    const KIND: &'static str = "settings.device";
    const LABEL: &'static str = "Device";
}

/// An agent's grant the door let in, as the type its rows are items of.
pub(crate) struct AgentGrant;

impl tilo_resource::words::RowType for AgentGrant {
    const KIND: &'static str = "settings.agent-grant";
    const LABEL: &'static str = "Agent";
}

/// A device asking to be the person, as the type its rows are items of.
pub(crate) struct AskingDevice;

impl tilo_resource::words::RowType for AskingDevice {
    const KIND: &'static str = "settings.device-asking";
    const LABEL: &'static str = "Device asking to join";
}

/// A device asking to be the person, as [`device_pairing::ASKING`] answers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Asking {
    /// What its answer names it by.
    pub ask: String,
    pub label: String,
    /// The code both screens show, so the person can tell it is theirs.
    pub code: String,
    /// The device of theirs it asked through.
    pub via: String,
}

#[derive(serde::Deserialize)]
struct AskingAnswer {
    asking: Vec<Asking>,
}

/// An action the person took, until the host settles it.
struct Pending {
    call: ui::InvocationCall,
    reply: Option<EffectReply>,
}

/// One list's host call and what it answered.
#[derive(Default)]
struct Asked {
    call: Option<ui::InvocationCall>,
    error: Option<String>,
    answered: bool,
}

/// What the page knows: the host's last answers.
pub struct AccessModel {
    devices: Vec<DeviceView>,
    devices_asked: Asked,
    door: Door,
    /// The door's live read, while it is followed.
    door_followed: Option<ui::Followed>,
    door_seen: u64,
    door_error: Option<String>,
    door_answered: bool,
    asking: Vec<Asking>,
    asking_asked: Asked,
    pending: Vec<Pending>,
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
        model.door_answered = true;
        model.asking = sample.asking;
        model.asking_asked.answered = true;
        model
    }

    fn empty() -> Self {
        Self {
            devices: Vec::new(),
            devices_asked: Asked::default(),
            door: Door::default(),
            door_followed: None,
            door_seen: 0,
            door_error: None,
            door_answered: false,
            asking: Vec::new(),
            asking_asked: Asked::default(),
            pending: Vec::new(),
        }
    }

    /// Ask the host again for the devices and the devices asking in.
    fn ask_lists(&mut self, remote: &ui::RemoteActions) {
        self.devices_asked.call = Some(crate::device_fleet::list(remote));
        self.asking_asked.call = Some(remote.invoke(
            device_pairing::target(device_pairing::ASKING),
            InvokeArgument::None,
            None,
        ));
    }

    /// Ask the host to do what `effect` says; `reply` hears how it ended,
    /// and the lists are read again once it has.
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
        };
        self.pending.push(Pending { call, reply });
        Ok(())
    }

    /// The key of the device `device` names, among those listed.
    fn device(&self, device: &str) -> Result<[u8; 32], String> {
        self.devices
            .iter()
            .find(|listed| hex::encode(listed.key) == device && !listed.revoked)
            .map(|listed| listed.key)
            .ok_or_else(|| t("settings-access-no-such-device"))
    }

    /// The grant `grant` names, among those the door let in.
    fn grant(&self, grant: &str) -> Result<&Grant, String> {
        self.door
            .grants
            .iter()
            .find(|listed| listed.id == grant)
            .ok_or_else(|| t("settings-access-no-such-agent"))
    }

    /// Take in what the host answered; whether what the page shows changed.
    fn poll(&mut self, remote: &ui::RemoteActions) -> bool {
        // Calls asked of the host move only as the host is polled.
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
                // A host without pairing has no devices asking: the list is empty, not broken.
                Err(_) => self.asking.clear(),
            }
            changed = true;
        }
        let mut settled = false;
        for pending in std::mem::take(&mut self.pending) {
            match crate::device_fleet::settled(&pending.call) {
                Some(answer) => {
                    if let Some(reply) = pending.reply {
                        reply.finish(answer);
                    }
                    settled = true;
                }
                None => self.pending.push(pending),
            }
        }
        if settled {
            // What the person did changed the lists: ask for them again.
            self.ask_lists(remote);
            changed = true;
        }
        changed
    }

    /// Take in the door's latest read; whether it changed.
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
                self.door_error = Some(error.message);
                self.door_answered = true;
                return true;
            }
            ui::Arrival::Failed(error) => {
                self.door_error = Some(error.to_string());
                self.door_answered = true;
                return true;
            }
            ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => return false,
        };
        match serde_json::from_value::<Door>(value) {
            Ok(door) => {
                self.door = door;
                self.door_error = None;
            }
            Err(_) => self.door_error = Some(t("settings-access-unreadable")),
        }
        self.door_answered = true;
        true
    }

    fn page(&self, cx: &ViewCx<Settings>) -> Element {
        // What waits for the person comes first while anything does.
        let mut sections = Vec::new();
        if !self.asking.is_empty() {
            sections.push(self.requests_section(cx));
        }
        sections.push(self.devices_section(cx));
        sections.push(self.agents_section(cx));
        sections.push(self.connect_section());
        if self.asking.is_empty() {
            sections.push(self.requests_section(cx));
        }
        page(sections)
    }

    /// The person's devices: this one first, then the others, then the ones
    /// whose access ended.
    fn devices_section(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let title = t("settings-access-devices");
        if let Some(error) = &self.devices_asked.error {
            return section(&title, rows(vec![note(error)]));
        }
        if !self.devices_asked.answered {
            return section(&title, rows(vec![note(&t("settings-access-reading"))]));
        }
        let mut ordered: Vec<&DeviceView> = self.devices.iter().filter(|d| d.current).collect();
        ordered.extend(self.devices.iter().filter(|d| !d.current && !d.revoked));
        ordered.extend(self.devices.iter().filter(|d| !d.current && d.revoked));
        if ordered.is_empty() {
            return section(&title, rows(vec![note(&t("settings-access-no-devices"))]));
        }
        let items = ordered
            .into_iter()
            .map(|device| device_item(cx, device))
            .collect();
        section(&title, rows(items))
    }

    /// The agents the door let in, grouped by the app that connected.
    fn agents_section(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let title = t("settings-access-agents");
        if !self.door_answered {
            return section(&title, rows(vec![note(&t("settings-access-reading"))]));
        }
        if let Some(error) = &self.door_error {
            return section(&title, rows(vec![note(error)]));
        }
        if self.door.grants.is_empty() {
            return section(&title, rows(vec![note(&t("settings-access-no-agents"))]));
        }
        // Each app's grants together, in the order the door lists the app first.
        let mut apps: Vec<String> = Vec::new();
        for grant in &self.door.grants {
            let app = app_of(grant);
            if !apps.contains(&app) {
                apps.push(app);
            }
        }
        let items = apps
            .iter()
            .flat_map(|app| {
                self.door
                    .grants
                    .iter()
                    .filter(move |grant| &app_of(grant) == app)
            })
            .map(|grant| grant_item(cx, grant))
            .collect();
        section(&title, rows(items))
    }

    /// How another agent connects: the door's address and each client's line.
    fn connect_section(&self) -> SettingsSection {
        let title = t("settings-access-connect");
        if self.door.endpoint.is_empty() {
            return section(&title, rows(vec![note(&t("settings-access-no-door"))]));
        }
        let endpoint = &self.door.endpoint;
        let mut items = vec![value_row(&t("settings-access-mcp-address"), endpoint)];
        for client in Client::ALL {
            let label = t(match client {
                Client::ClaudeCode => "settings-access-client-claude-code",
                Client::Codex => "settings-access-client-codex",
                Client::Other => "settings-access-client-other",
            });
            items.push(value_row(&label, &client.command(endpoint)));
        }
        items.push(note(&t("settings-access-connect-note")));
        section(&title, rows(items))
    }

    /// Devices asking to be the person.
    fn requests_section(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let title = t("settings-access-requests");
        if !self.asking_asked.answered {
            return section(&title, rows(vec![note(&t("settings-access-reading"))]));
        }
        if self.asking.is_empty() {
            return section(&title, rows(vec![note(&t("settings-access-no-requests"))]));
        }
        let items = self
            .asking
            .iter()
            .map(|asking| asking_item(cx, asking))
            .collect();
        section(&title, rows(items))
    }
}

/// What a finished list call answered, taking it out of `asked`; `None`
/// while it runs or when none was asked.
fn settle(asked: &mut Asked) -> Option<Result<Option<serde_json::Value>, String>> {
    let answer = crate::device_fleet::settled(asked.call.as_ref()?)?;
    asked.call = None;
    asked.answered = true;
    asked.error = None;
    Some(answer.map(|output| output.value))
}

/// The app an agent came in through, as the list groups it.
fn app_of(grant: &Grant) -> String {
    if grant.script_key {
        t("settings-access-script-keys")
    } else if grant.agent.is_empty() {
        t("settings-access-unnamed-agent")
    } else {
        grant.agent.clone()
    }
}

/// `ms` since the epoch as the person reads a date.
fn day(ms: i64) -> String {
    ui::format::date_of(&ui::format::UnixSecs(ms / 1000))
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

fn device_item(cx: &ViewCx<Settings>, device: &DeviceView) -> Item {
    let key = hex::encode(device.key);
    let mut details = Vec::new();
    // A key says it may host the person's workspaces; nothing says what
    // else a device is, so its name speaks for it.
    if device.can_host {
        details.push(t("settings-access-kind-server"));
    }
    if device.current {
        details.push(t("settings-access-this-device"));
    } else if device.revoked {
        details.push(t("settings-access-revoked"));
    }
    details.push(t_args(
        "settings-access-fingerprint",
        &[("fingerprint", device.fingerprint.clone().into())],
    ));
    let menu: Option<Element> = (!device.current && !device.revoked).then(|| {
        let live = device_row(device).into_resource();
        let dispatcher = cx.dispatcher();
        let mut menu = RowMenu::new(
            format!("settings.access.device.more.{key}"),
            &t_args(
                "settings-row-more",
                &[("name", device.label.clone().into())],
            ),
        );
        for reason in REVOKE_REASONS {
            menu = menu.destructive(
                t(match reason {
                    "lost" => "settings-access-revoke-lost",
                    "stolen" => "settings-access-revoke-stolen",
                    "compromised" => "settings-access-revoke-compromised",
                    _ => "settings-access-revoke-retired",
                }),
                crate::on_row::invocation(
                    &dispatcher,
                    &live,
                    DevicesAccessEffect::RevokeDevice {
                        device: key.clone(),
                        reason: reason.to_owned(),
                    },
                ),
            );
        }
        Box::new(menu) as Element
    });
    row(Box::new(thing_row(
        &format!("settings.access.device.{key}"),
        &device.label,
        &details,
        menu,
    )))
}

fn grant_row(grant: &Grant) -> LiveRow {
    let mut row =
        LiveRow::typed::<AgentGrant>(format!("settings.agent-grant:{}", grant.id), app_of(grant))
            .operation(
                SettingsEffect::from(DevicesAccessEffect::RevokeAgent {
                    grant: grant.id.clone(),
                })
                .bound(),
            );
    for run in &grant.runs {
        row = row.operation(
            SettingsEffect::from(DevicesAccessEffect::StopRun {
                grant: grant.id.clone(),
                run: run.name.clone(),
            })
            .bound(),
        );
    }
    row
}

/// What a grant's row says under its app, a line each: where it runs, what
/// it reaches and until when, whether it may operate web pages in Browser,
/// and when it last acted or which runs are open now.
fn grant_lines(grant: &Grant) -> Vec<String> {
    let mut lines = Vec::new();
    if grant.script_key {
        lines.push(if grant.runs_on.is_empty() {
            t("settings-access-script-key")
        } else {
            t_args(
                "settings-access-script-key-on",
                &[("place", grant.runs_on.clone().into())],
            )
        });
    } else if !grant.runs_on.is_empty() {
        lines.push(t_args(
            "settings-access-runs-on",
            &[("place", grant.runs_on.clone().into())],
        ));
    }
    let until = match grant.expires_ms {
        Some(ms) => t_args("settings-access-until", &[("date", day(ms).into())]),
        None => t("settings-access-until-revoked"),
    };
    lines.push(t_args(
        "settings-access-reach-until",
        &[("reach", reach_line(grant).into()), ("until", until.into())],
    ));
    lines.push(t(if grant.browser_pages {
        "settings-access-browser-pages-on"
    } else {
        "settings-access-browser-pages-off"
    }));
    let runs: Vec<&str> = grant.runs.iter().map(|run| run.name.as_str()).collect();
    lines.push(if runs.is_empty() {
        match grant.last_ms {
            Some(ms) => t_args("settings-access-last-active", &[("date", day(ms).into())]),
            None => t("settings-access-never-active"),
        }
    } else {
        t_args(
            "settings-access-run-open",
            &[("run", runs.join(", ").into())],
        )
    });
    lines
}

/// What a grant reaches, said in a line.
fn reach_line(grant: &Grant) -> String {
    match &grant.reach {
        Reach::Account => t("settings-access-reaches-all"),
        Reach::Workspace { name, .. } => t_args(
            "settings-access-reaches-one",
            &[("workspace", name.clone().into())],
        ),
    }
}

fn grant_item(cx: &ViewCx<Settings>, grant: &Grant) -> Item {
    // A script's key is named by the person; an agent by its app.
    let title = if grant.script_key && !grant.agent.is_empty() {
        grant.agent.clone()
    } else {
        app_of(grant)
    };
    let details = grant_lines(grant);
    let live = grant_row(grant).into_resource();
    let dispatcher = cx.dispatcher();
    let mut menu = RowMenu::new(
        format!("settings.access.agent.more.{}", grant.id),
        &t_args("settings-row-more", &[("name", title.clone().into())]),
    );
    for run in &grant.runs {
        menu = menu.action(
            t_args(
                "settings-access-stop-run",
                &[("run", run.name.clone().into())],
            ),
            crate::on_row::invocation(
                &dispatcher,
                &live,
                DevicesAccessEffect::StopRun {
                    grant: grant.id.clone(),
                    run: run.name.clone(),
                },
            ),
        );
    }
    menu = menu.destructive(
        t("settings-access-revoke-agent"),
        crate::on_row::invocation(
            &dispatcher,
            &live,
            DevicesAccessEffect::RevokeAgent {
                grant: grant.id.clone(),
            },
        ),
    );
    row(Box::new(thing_row(
        &format!("settings.access.agent.{}", grant.id),
        &title,
        &details,
        Some(Box::new(menu) as Element),
    )))
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

fn asking_item(cx: &ViewCx<Settings>, asking: &Asking) -> Item {
    let live = asking_row(asking).into_resource();
    let dispatcher = cx.dispatcher();
    let menu = RowMenu::new(
        format!("settings.access.asking.more.{}", asking.ask),
        &t_args(
            "settings-row-more",
            &[("name", asking.label.clone().into())],
        ),
    )
    .action(
        t("settings-access-approve"),
        crate::on_row::invocation(
            &dispatcher,
            &live,
            DevicesAccessEffect::AnswerDevice {
                ask: asking.ask.clone(),
                label: asking.label.clone(),
                approve: true,
            },
        ),
    )
    .destructive(
        t("settings-access-decline"),
        crate::on_row::invocation(
            &dispatcher,
            &live,
            DevicesAccessEffect::AnswerDevice {
                ask: asking.ask.clone(),
                label: asking.label.clone(),
                approve: false,
            },
        ),
    );
    row(Box::new(thing_row(
        &format!("settings.access.asking.{}", asking.ask),
        &asking.label,
        &[
            t_args(
                "settings-access-asking-via",
                &[("device", asking.via.clone().into())],
            ),
            t_args(
                "settings-access-asking-code",
                &[("code", asking.code.clone().into())],
            ),
        ],
        Some(Box::new(menu) as Element),
    )))
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    model.borrow_mut().poll(&held.remote);
    Some(cx.switch(&held.shown, move |cx, _| model.borrow().page(cx)))
}

#[cfg(test)]
mod tests;
