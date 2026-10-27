//! Root of the firewall manager.
//!
//! [`FirewallApp`] is a [`NavShell`] window: the zones are the sidebar and the
//! selected zone is the content (see `crate::view`). This module owns what
//! the shell cannot: the configuration and where it came from, the zone and
//! rule the person is looking at, the add-rule draft, the delete
//! confirmation, the last result, and the effects that push the
//! configuration to firewalld.
//!
//! The configuration lives in the shared Automerge document (`doc.rs`): every
//! edit is a field edit or verb submitted through the framework dispatcher,
//! and the framework presents the stored configuration back (see
//! [`crate::resource`]), so two instances on one record edit the same
//! ruleset. Which zone is open, the tab and the draft are this window's
//! presentation and never enter the document.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::kurbo::Point;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::LocalState;
use ui::a11y::{AccessNode, Politeness, Role};
use ui::capability_door::{Capability, Door};
use ui::theme::Theme;
use ui::widget::data_table::TableSort;
use ui::widget::toast::ToastKind;
use ui::widget::{AppModel, ModelPath, NavShell, Widget};
use ui::window_toolbar::{ToolbarEvent, ToolbarLocation};

use ui::framework::{Dispatcher, Setting};

use crate::blocked::BlockedReport;
use crate::model::{
    Action, FirewallConfig, Matcher, Proto, Rule, RuleId, Zone, ZoneKind, validate_port,
    validate_source,
};
use crate::model::{StoredRule, StoredZonePaths};
use crate::resource::{AppliedConfig, ApplyOutcome, Firewall, FirewallEffect};
use crate::view::{
    self, Draft, Intent, Intents, PageModel, PendingSwitch, SwitchKey, Switches, Tab,
};
use ui::framework::Submit;

/// The job that applies the stored configuration to this device.
const APPLY_JOB: &str = "apply";

/// How a reported result reads: it decides the toast's tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Info,
    Success,
    Error,
}

impl Tone {
    fn kind(self) -> ToastKind {
        match self {
            Tone::Info => ToastKind::Info,
            Tone::Success => ToastKind::Success,
            Tone::Error => ToastKind::Error,
        }
    }
}

/// The last result the app reported: announced to assistive technology and
/// shown once as a toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub text: String,
    pub tone: Tone,
}

/// What the shell currently shows, so it is rebuilt only when it changes.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a snapshot of independent view inputs, compared as a whole"
)]
struct ViewKey {
    rev: u64,
    available: bool,
    zone: ZoneKind,
    rule: Option<RuleId>,
    tab: Tab,
    wide: bool,
    service: bool,
    proto: Proto,
    action: Action,
    port_error: Option<String>,
    source_error: Option<String>,
    pending_delete: Option<RuleId>,
    pending_switch: Option<PendingSwitch>,
    adding: bool,
    applying: bool,
    grantable: bool,
    blocked_rev: u64,
    blocked_loading: bool,
    blocked_error: Option<String>,
    unavailable_detail: Option<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags: availability, the shell's build, the add-rule dialog, and a pending sync"
)]
pub struct FirewallApp {
    shell: NavShell,
    /// What the window's bar names: the open zone, or Firewall.
    title: String,
    intents: Intents,
    switches: Switches,
    enabled_switch: LocalState<bool>,
    panic_switch: LocalState<bool>,

    config: FirewallConfig,
    /// Bumped whenever `config` is replaced.
    rev: u64,
    selected_zone: ZoneKind,
    selected_rule: Option<RuleId>,
    /// The rules table's order, which the table keeps here.
    rule_sort: LocalState<Option<TableSort>>,
    tab: Tab,
    draft: Draft,
    notice: Option<Notice>,
    /// The content column is wide enough for the services' descriptions:
    /// at least `wide_from`, the width of a compact window.
    wide: bool,
    wide_from: f64,

    /// Whether there is a configuration to show. `false` means no device
    /// could read firewalld into the record yet, and we show an honest
    /// unavailable state instead of any fabricated ruleset.
    available: bool,
    /// Whether this window may write to the host's firewalld. False for a
    /// capture, which stages a sample and must never touch the machine.
    host_writes: bool,
    /// The rule the person asked to delete; the dialog confirms it.
    pending_delete: Option<RuleId>,
    /// The add-rule dialog is open (adding or editing).
    adding: bool,
    /// A global switch asked to change to a state that can cut this session;
    /// the dialog confirms it first.
    pending_switch: Option<PendingSwitch>,
    /// What this app can do on this device, resolved rather than inferred.
    capability: Door,
    /// The last read of dropped traffic, or why it could not be read.
    blocked: Option<BlockedReport>,
    blocked_error: Option<String>,
    blocked_loading: bool,
    blocked_rev: u64,

    /// Where every on-screen affordance submits its field edit or verb: one
    /// undo step each, replayed byte-identically on peers — the same door
    /// Ctrl+K takes. `None` only for a widget no resource hosts.
    dispatcher: Option<Dispatcher<Firewall>>,
    /// What this device last applied to its firewalld.
    applied: Option<Setting<AppliedConfig>>,
    /// Where this window applies the stored configuration to the device.
    jobs: Option<ui::framework::Jobs<Firewall>>,
    /// The configuration being applied now, as its canonical text.
    applying: Option<String>,
    /// A failed apply of the stored configuration on this device, until the
    /// window reports it.
    outcome: ApplyOutcome,

    built: Option<ViewKey>,
    /// The zone list's (config revision, selected zone) when it was built.
    listed: Option<(u64, ZoneKind)>,
    /// The page's scroll offset, kept across rebuilds.
    scroll: Rc<Cell<(f32, f32)>>,
    /// A control changed what the window shows; `update` asks for a sync.
    changed: bool,
}

impl Default for FirewallApp {
    fn default() -> Self {
        Self::new()
    }
}

/// The window: the zones beside the open zone. Its controls are the window
/// bar's slots ([`FirewallApp::run_window_toolbar`]).
fn shell() -> NavShell {
    NavShell::new()
        .content_kind(ui::widget::ContentKind::List)
        .with_id("firewall-shell")
        .with_window_toolbar()
}

impl FirewallApp {
    /// The surface before the resource presents its configuration: an
    /// honest unavailable state, never a fabricated ruleset.
    pub fn new() -> Self {
        Self::from_parts(FirewallConfig::empty(), false)
    }

    /// The bare widget showing the sample ruleset, for tests of its layout
    /// and input.
    #[cfg(test)]
    pub(crate) fn new_demo() -> Self {
        Self::from_parts(crate::sample::sample_config(), true)
    }

    fn from_parts(config: FirewallConfig, available: bool) -> Self {
        let intents = Intents::default();
        let enabled_switch = LocalState::new(config.enabled);
        let panic_switch = LocalState::new(config.panic_mode);
        let capability = capability_for(config.zones.is_empty());
        Self {
            shell: shell(),
            title: String::new(),
            intents,
            switches: Switches::default(),
            enabled_switch,
            panic_switch,
            selected_zone: config.default_zone,
            config,
            rev: 0,
            selected_rule: None,
            rule_sort: LocalState::new(Some(TableSort::ascending("action"))),
            tab: Tab::Rules,
            draft: Draft::new(),
            notice: None,
            wide: true,
            wide_from: 0.0,
            available,
            host_writes: false,
            pending_delete: None,
            pending_switch: None,
            capability,
            blocked: None,
            blocked_error: None,
            blocked_loading: false,
            blocked_rev: 0,
            adding: false,
            dispatcher: None,
            applied: None,
            jobs: None,
            applying: None,
            outcome: ApplyOutcome::default(),
            built: None,
            listed: None,
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            changed: true,
        }
    }

    /// Presentation-only screenshot states, staged once the scenario's
    /// configuration is shown: empty, invalid, confirmation and changed
    /// states for visual review.
    pub(crate) fn pose_fixture(&mut self, name: &str) {
        match name {
            "empty" => self.selected_zone = ZoneKind::Drop,
            "add-rule" => self.adding = true,
            "error" => {
                self.adding = true;
                self.draft.service = false;
                self.draft.port.set("70000".into());
                self.draft.source.set("192.168.1".into());
                let _ = self.validated_draft();
            }
            "confirmation" => {
                self.pending_delete = self
                    .selected_zone_ref()
                    .and_then(|zone| zone.rules.get(4))
                    .map(|rule| rule.id);
            }
            "active-change" => {
                self.selected_zone = ZoneKind::Home;
                self.report("Home is now the active zone", Tone::Success);
            }
            "service-error" => {
                self.report(
                    "Permission to read firewalld was denied. Ask an administrator for access, then try again.",
                    Tone::Error,
                );
                // The empty state carries the reason; no toast repeats it.
                self.shell.dismiss_toast();
            }
            "services" => self.tab = Tab::Services,
            "blocked" => {
                self.tab = Tab::Blocked;
                self.blocked = Some(crate::blocked::sample());
                self.blocked_rev += 1;
            }
            "switch-confirmation" => self.pending_switch = Some(PendingSwitch::PanicOn),
            "stop-confirmation" => self.pending_switch = Some(PendingSwitch::Stop),
            "selected" => {
                self.selected_rule = self
                    .selected_zone_ref()
                    .and_then(|zone| zone.rules.get(1))
                    .map(|rule| rule.id);
            }
            "drawer" => self.shell.open_sidebar(),
            _ => {}
        }
        self.sync_switches();
        self.changed = true;
    }

    fn selected_zone_ref(&self) -> Option<&Zone> {
        self.config.zone(self.selected_zone)
    }

    fn set_config(&mut self, config: FirewallConfig) {
        if config != self.config {
            self.config = config;
            self.rev += 1;
            self.changed = true;
        }
        if self.config.zone(self.selected_zone).is_none() {
            self.selected_zone = self.config.default_zone;
        }
        self.sync_switches();
    }

    /// Point the header switches at the configuration, so the only
    /// difference between them is one the person makes.
    fn sync_switches(&self) {
        if self.enabled_switch.with(|on| *on) != self.config.enabled {
            self.enabled_switch.set(self.config.enabled);
        }
        if self.panic_switch.with(|on| *on) != self.config.panic_mode {
            self.panic_switch.set(self.config.panic_mode);
        }
    }

    /// Record a result: announced to assistive technology and shown once.
    fn report(&mut self, text: impl Into<String>, tone: Tone) {
        let notice = Notice {
            text: text.into(),
            tone,
        };
        self.shell.toast(notice.text.clone(), notice.tone.kind());
        self.notice = Some(notice);
        self.changed = true;
    }

    /// Submit one field edit or verb of the stored configuration. The
    /// framework applies it after the event and presents the stored
    /// configuration back, which the next frame shows.
    ///
    /// This is *only* the document edit. Pushing the ruleset to the kernel
    /// (`firewall-cmd`/`nft`) follows from the stored configuration on each
    /// device — never part of an action.
    fn dispatch(&self, submit: impl Submit<Firewall>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(submit);
        }
    }

    /// The paths of the selected zone's stored fields.
    fn zone_paths(&self) -> StoredZonePaths<Firewall> {
        Firewall::paths().zones.key(self.selected_zone.name())
    }

    /// Invoke one of the firewall's effects through the same door as the
    /// palette.
    fn invoke(&self, effect: &FirewallEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    /// Show the stored configuration. An invalid one is not shown, so no
    /// control acts on it.
    pub(crate) fn show_config(&mut self, state: &FirewallConfig) {
        if state.zones.is_empty() {
            self.set_config(state.clone());
            self.capability = capability_for(false);
            if self.available {
                self.available = false;
                self.changed = true;
            }
            return;
        }
        if state.validate().is_err() {
            self.report(
                "A shared change to the configuration was invalid and was ignored",
                Tone::Error,
            );
            return;
        }
        self.set_config(state.clone());
        self.capability = capability_for(true);
        if !self.available {
            self.available = true;
            self.changed = true;
        }
        if let Some(rule) = self.pending_delete
            && !self
                .config
                .zones
                .iter()
                .any(|zone| zone.rules.iter().any(|r| r.id == rule))
        {
            self.pending_delete = None;
            self.changed = true;
        }
    }

    /// Take part in the resource: edits go through `dispatcher`, and
    /// `applied` remembers what this device's firewalld holds.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: Dispatcher<Firewall>,
        applied: Setting<AppliedConfig>,
    ) {
        self.dispatcher = Some(dispatcher);
        self.applied = Some(applied);
    }

    /// Apply the stored configuration to this device with `jobs`, after
    /// every change of it.
    pub(crate) fn follow_jobs(&mut self, jobs: ui::framework::Jobs<Firewall>) {
        self.jobs = Some(jobs);
    }

    /// A live window may change this device; a capture may not.
    pub(crate) fn set_host_writes(&mut self, allowed: bool) {
        self.host_writes = allowed;
    }

    pub(crate) fn host_writes(&self) -> bool {
        self.host_writes
    }

    /// Apply `config` to this device's firewalld unless it holds it already
    /// or is being given it: a job of the window, the latest configuration
    /// replacing one still being applied. A failure is reported and tried
    /// again after the next change.
    pub(crate) fn apply_to_device(&mut self, config: &FirewallConfig) {
        let (Some(jobs), Some(applied)) = (&self.jobs, &self.applied) else {
            return;
        };
        if !self.capability.is_granted() {
            return;
        }
        let Some((canonical, config)) =
            crate::resource::pending_apply(config, &applied.get()).pop()
        else {
            return;
        };
        let door = self.capability.clone();
        if self.applying.as_deref() == Some(canonical.as_str()) && jobs.is_running(APPLY_JOB) {
            return;
        }
        self.applying = Some(canonical);
        let app_writes = self.host_writes;
        jobs.start::<Self>(APPLY_JOB, move |cx| {
            Ok(cx.thread(
                move |_| {
                    let result = crate::resource::apply_on_device(&config, &door, app_writes);
                    (config, result)
                },
                |app: &mut Self,
                 (config, result): (FirewallConfig, crate::resource::Applied),
                 _| {
                    app.applying = None;
                    if result.is_err() {
                        let status = crate::resource::applied_status(&result);
                        let message = status.text.clone();
                        *app.outcome.borrow_mut() = Some(status);
                        app.changed = true;
                        return Err(message);
                    }
                    if let Some(applied) = &app.applied {
                        crate::resource::remember_applied(applied, &config);
                    }
                    Ok(ui::framework::Effected::done())
                },
            ))
        });
    }

    /// Whether there is a configuration to edit and apply.
    pub(crate) fn is_available(&self) -> bool {
        self.available
    }

    /// The capability this window's privileged verbs run under.
    pub(crate) fn capability(&self) -> &Door {
        &self.capability
    }

    pub(crate) fn mark_blocked_loading(&mut self) {
        self.blocked_loading = true;
        self.blocked_error = None;
        self.changed = true;
    }

    pub(crate) fn finish_blocked(&mut self, read: Result<BlockedReport, String>) {
        self.blocked_loading = false;
        match read {
            Ok(report) => {
                self.blocked = Some(report);
                self.blocked_error = None;
                self.blocked_rev += 1;
            }
            Err(error) => {
                self.blocked = None;
                self.blocked_error = Some(error);
                self.blocked_rev += 1;
            }
        }
        self.changed = true;
    }

    fn view_key(&self) -> ViewKey {
        ViewKey {
            rev: self.rev,
            available: self.available,
            zone: self.selected_zone,
            rule: self.selected_rule,
            tab: self.tab,
            wide: self.wide,
            service: self.draft.service,
            proto: self.draft.proto,
            action: self.draft.action,
            port_error: self.draft.port_error.clone(),
            source_error: self.draft.source_error.clone(),
            pending_delete: self.pending_delete,
            pending_switch: self.pending_switch,
            adding: self.adding,
            applying: self.applying.is_some(),
            grantable: self.capability.is_grantable(),
            blocked_rev: self.blocked_rev,
            blocked_loading: self.blocked_loading,
            blocked_error: self.blocked_error.clone(),
            unavailable_detail: self.unavailable_detail().map(str::to_owned),
        }
    }

    /// Why firewalld could not be read, when a retry said so.
    fn unavailable_detail(&self) -> Option<&str> {
        if self.available {
            return None;
        }
        self.notice
            .as_ref()
            .filter(|notice| notice.tone == Tone::Error)
            .map(|notice| notice.text.as_str())
    }

    /// Rebuild the sidebar and the content when what they show changed.
    fn refresh_view(&mut self) {
        let key = self.view_key();
        if self.built.as_ref() == Some(&key) {
            return;
        }
        self.built = Some(key);
        self.switches.clear();
        let Some(zone) = self
            .config
            .zone(self.selected_zone)
            .filter(|_| self.available)
        else {
            self.shell.set_sidebar(None);
            self.listed = None;
            "Firewall".clone_into(&mut self.title);
            self.shell
                .set_content(view::unavailable(self.unavailable_detail(), &self.intents));
            return;
        };
        let listed = (self.rev, self.selected_zone);
        if self.listed != Some(listed) {
            self.listed = Some(listed);
            self.shell.set_sidebar(Some(Box::new(view::zone_list(
                &self.config,
                self.selected_zone,
                &self.intents,
            ))));
        }
        let pending_delete = self
            .pending_delete
            .and_then(|id| zone.rules.iter().find(|rule| rule.id == id));
        let model = PageModel {
            config: &self.config,
            zone,
            tab: self.tab,
            selected_rule: self.selected_rule,
            rule_sort: &self.rule_sort,
            pending_delete,
            pending_switch: self.pending_switch,
            draft: &self.draft,
            adding: self.adding,
            applying: self.applying.is_some(),
            blocked: self.blocked.as_ref(),
            blocked_error: self.blocked_error.as_deref(),
            blocked_loading: self.blocked_loading,
            wide: self.wide,
        };
        let page = view::zone_page(&model, &self.intents, &self.switches, &self.scroll);
        zone.kind.title().clone_into(&mut self.title);
        self.shell.set_content(page);
    }

    /// Apply what the window's controls asked for.
    fn drain(&mut self) {
        let intents = self.intents.take();
        let flipped = self.switches.flipped();
        if !intents.is_empty() || !flipped.is_empty() {
            self.changed = true;
        }
        for intent in intents {
            self.apply_intent(&intent);
        }
        for (key, on) in flipped {
            match key {
                SwitchKey::Rule(id) => self.set_rule_enabled(id, on),
                SwitchKey::Service(id) => self.set_service(id, on),
            }
        }
        // A header switch the person turned asks for the edit once; the
        // switch follows the stored configuration again when it is
        // presented back, so a refused edit is not asked for every frame.
        // The two that can cut this session ask before they act.
        self.sync_switches();
    }

    fn apply_intent(&mut self, intent: &Intent) {
        match *intent {
            Intent::SelectZone(zone) => self.select_zone(zone),
            Intent::SelectRule(id) => self.selected_rule = Some(id),
            Intent::SetTab(tab) => {
                self.tab = tab;
                self.open_blocked_if_needed();
            }
            Intent::SetMatchService(service) => {
                self.draft.service = service;
                self.draft.port_error = None;
            }
            Intent::SetProto(proto) => self.draft.proto = proto,
            Intent::SetAction(action) => self.draft.action = action,
            Intent::OpenAddRule => {
                self.adding = true;
                self.draft.editing = None;
                self.draft.port_error = None;
                self.draft.source_error = None;
                self.draft.port.set(String::new());
                self.draft.source.set(String::new());
            }
            Intent::EditRule(id) => self.open_edit_rule(id),
            Intent::CancelAddRule => self.adding = false,
            Intent::AddRule => self.save_rule_from_draft(),
            Intent::ConfirmSwitch => self.confirm_switch(),
            Intent::CancelSwitch => {
                self.pending_switch = None;
                self.sync_switches();
            }
            Intent::RefreshBlocked => self.read_blocked(),
            Intent::AskDelete(id) => self.pending_delete = Some(id),
            Intent::ConfirmDelete => {
                if let Some(id) = self.pending_delete {
                    self.delete_rule(id);
                }
            }
            Intent::CancelDelete => self.pending_delete = None,
            Intent::MakeActive => self.set_active_zone(),
            Intent::Apply => self.invoke(&FirewallEffect::ApplyConfig),
            Intent::Retry => self.invoke(&FirewallEffect::Retry),
        }
    }

    fn select_zone(&mut self, kind: ZoneKind) {
        if self.selected_zone != kind {
            self.selected_zone = kind;
            self.selected_rule = None;
            self.pending_delete = None;
            self.adding = false;
            self.scroll.set((0.0, 0.0));
        }
    }

    /// Show how reading firewalld ended, answering with the configuration to
    /// import. Every retry route shares this fail-closed behaviour; a failed
    /// read never changes the configuration.
    pub(crate) fn finish_retry(
        &mut self,
        read: Result<FirewallConfig, String>,
    ) -> Result<FirewallConfig, String> {
        match read {
            Ok(config) => {
                self.selected_zone = config.default_zone;
                self.pending_delete = None;
                // What this device's firewalld reported it already holds.
                if let Some(applied) = &self.applied
                    && self.config.zones.is_empty()
                {
                    crate::resource::remember_applied(applied, &config.canonical());
                }
                self.report("Connected to firewalld", Tone::Success);
                Ok(config)
            }
            Err(message) => {
                self.report(message.clone(), Tone::Error);
                Err(message)
            }
        }
    }

    /// Check the draft. On success, the rule to add; otherwise each field
    /// carries what is wrong with it.
    fn validated_draft(&mut self) -> Option<Rule> {
        let draft = &mut self.draft;
        let matcher = if draft.service {
            draft.port_error = None;
            Matcher::Service(draft.service_id())
        } else {
            let port = draft.port();
            draft.port_error = validate_port(&port).err();
            Matcher::Port {
                port,
                proto: draft.proto,
            }
        };
        let source = draft.source();
        draft.source_error = validate_source(&source).err();
        if draft.port_error.is_some() || draft.source_error.is_some() {
            return None;
        }
        Some(Rule::new(matcher, draft.action, source))
    }

    /// Open the add-rule dialog on an existing rule. Editing keeps the rule's
    /// identity, so its position and enabled state survive a typo fix.
    fn open_edit_rule(&mut self, id: RuleId) {
        let Some(rule) = self
            .selected_zone_ref()
            .and_then(|zone| zone.rules.iter().find(|rule| rule.id == id))
            .cloned()
        else {
            return;
        };
        self.draft.editing = Some(id);
        self.draft.service = matches!(rule.matcher, Matcher::Service(_));
        match rule.matcher {
            Matcher::Service(service) => self.draft.service_id.set(service),
            Matcher::Port { port, proto } => {
                self.draft.port.set(port);
                self.draft.proto = proto;
            }
        }
        self.draft.action = rule.action;
        self.draft.source.set(rule.source);
        self.draft.port_error = None;
        self.draft.source_error = None;
        self.adding = true;
    }

    fn save_rule_from_draft(&mut self) {
        if self.config.zone(self.selected_zone).is_none() {
            return;
        }
        let Some(rule) = self.validated_draft() else {
            return;
        };
        if let Some(id) = self.draft.editing {
            let title = rule.matcher.title();
            let paths = self.zone_paths().rules.item(&Firewall::rule(id));
            self.dispatch(paths.matcher.set(rule.matcher));
            self.dispatch(paths.action.set(rule.action));
            self.dispatch(paths.source.set(rule.source));
            self.selected_rule = Some(id);
            self.report(format!("Updated the rule for {title}"), Tone::Success);
        } else {
            let id = rule.id;
            let title = rule.matcher.title();
            self.dispatch(self.zone_paths().rules.insert_as(
                &Firewall::rule(id),
                usize::MAX,
                &StoredRule {
                    matcher: rule.matcher,
                    action: rule.action,
                    source: rule.source,
                    enabled: rule.enabled,
                },
            ));
            self.selected_rule = Some(id);
            self.report(format!("Added a rule for {title}"), Tone::Success);
        }
        self.tab = Tab::Rules;
        self.adding = false;
        self.draft.editing = None;
        self.draft.port.set(String::new());
        self.draft.source.set(String::new());
    }

    fn confirm_switch(&mut self) {
        let Some(which) = self.pending_switch.take() else {
            return;
        };
        match which {
            PendingSwitch::Stop => self.dispatch(Firewall::STOP.invoke()),
            PendingSwitch::PanicOn => self.dispatch(Firewall::ENTER_PANIC_MODE.invoke()),
        }
        self.changed = true;
    }

    /// Read dropped traffic for the open window. A failure is shown as the
    /// reason it happened, never as a quiet firewall.
    fn read_blocked(&mut self) {
        if self.blocked_loading {
            return;
        }
        self.blocked_loading = true;
        self.blocked_error = None;
        self.changed = true;
        self.invoke(&FirewallEffect::ReadBlocked);
    }

    fn open_blocked_if_needed(&mut self) {
        if self.tab == Tab::Blocked && self.blocked.is_none() && self.blocked_error.is_none() {
            self.read_blocked();
        }
    }

    fn delete_rule(&mut self, id: RuleId) {
        self.dispatch(Firewall::DELETE_RULE.with((self.selected_zone, Firewall::rule(id))));
        if self.selected_rule == Some(id) {
            self.selected_rule = None;
        }
        self.pending_delete = None;
        self.report("Rule deleted", Tone::Info);
    }

    fn set_rule_enabled(&self, id: RuleId, on: bool) {
        let exists = self
            .selected_zone_ref()
            .is_some_and(|zone| zone.rules.iter().any(|rule| rule.id == id));
        if exists {
            self.dispatch(
                self.zone_paths()
                    .rules
                    .item(&Firewall::rule(id))
                    .enabled
                    .set(on),
            );
        }
    }

    fn set_service(&self, id: &str, on: bool) {
        let services = self.zone_paths().services;
        let id = id.to_owned();
        self.dispatch(if on {
            services.insert(&id)
        } else {
            services.remove(&id)
        });
    }

    fn set_active_zone(&mut self) {
        self.dispatch(Firewall::paths().default_zone.set(self.selected_zone));
        self.report(
            format!("{} is now the active zone", self.selected_zone.title()),
            Tone::Success,
        );
    }

    /// Show how an apply of `config` ended, and remember a successful one
    /// as what this device's firewalld holds.
    pub(crate) fn finish_apply(
        &mut self,
        config: &FirewallConfig,
        result: &crate::resource::Applied,
    ) {
        let notice = crate::resource::applied_status(result);
        self.report(notice.text, notice.tone);
        if result.is_ok()
            && let Some(applied) = &self.applied
        {
            crate::resource::remember_applied(applied, config);
        }
    }

    /// Run what the person did in the window's bar: the shell's own slots,
    /// the firewall's switches, Apply, Add rule and the views.
    fn take_window_toolbar_events(&mut self) {
        for event in ui::window_toolbar::take_events() {
            if self.shell.window_toolbar_event(&event) {
                self.changed = true;
                continue;
            }
            let ToolbarEvent::Action { id } = event else {
                continue;
            };
            match id.as_str() {
                view::FIREWALL_ON => {
                    if self.capability.is_granted() {
                        self.dispatch(if self.config.enabled {
                            Firewall::STOP.invoke()
                        } else {
                            Firewall::START.invoke()
                        });
                    }
                }
                view::PANIC_MODE => {
                    if self.capability.is_granted() {
                        self.dispatch(if self.config.panic_mode {
                            Firewall::EXIT_PANIC_MODE.invoke()
                        } else {
                            Firewall::ENTER_PANIC_MODE.invoke()
                        });
                    }
                }
                view::APPLY => self.intents.push(Intent::Apply),
                view::ADD_RULE => self.intents.push(Intent::OpenAddRule),
                id => {
                    if let Some(tab) = Tab::ALL.into_iter().find(|tab| tab.view_id() == id) {
                        self.intents.push(Intent::SetTab(tab));
                    }
                }
            }
            self.changed = true;
        }
    }

    /// Declare the window bar's slots: the shell's own, the open zone as
    /// the location, the firewall's switches and Apply, Add rule on the
    /// rules, and the zone's two views.
    fn declare_window_toolbar(&self) {
        let mut toolbar = self.shell.window_toolbar();
        toolbar.location = Some(ToolbarLocation {
            label: self.title.clone(),
            ..ToolbarLocation::default()
        });
        if self.available {
            let enabled = self.enabled_switch.with(|on| *on);
            let panic = self.panic_switch.with(|on| *on);
            // Every privileged control comes through the door: an
            // unavailable capability draws no switch, Apply or panic item.
            if let Some(actions) = self
                .capability
                .control(|| view::window_actions(enabled, panic, self.tab, self.applying.is_some()))
            {
                NavShell::push_window_actions(&mut toolbar, actions);
                toolbar.views = Some(view::window_views(self.tab));
            }
        }
        ui::window_toolbar::declare(Some(toolbar));
    }

    /// Report a failed apply of the projection on this device. It runs
    /// after every change, so a success stays quiet and the person keeps
    /// the confirmation of what they did.
    fn take_outcome(&mut self) {
        let failure = self.outcome.borrow_mut().take();
        if let Some(notice) = failure {
            self.report(notice.text, notice.tone);
        }
    }
}

/// What this app may do on this device. The packaged app is granted the
/// firewall capability; a device with no readable firewalld gets the
/// unavailable face, with the sentence that says what would change it. The
/// backend never infers this from `$PATH` or an environment variable.
fn capability_for(readable: bool) -> Door {
    if readable {
        Door::granted(Capability::FIREWALL)
    } else {
        Door::new(
            Capability::FIREWALL,
            ui::capability_door::Resolution::unavailable(
                "The firewall service isn\u{2019}t running, so no rule can be applied.",
            )
            .with_remedy("Start firewalld, then try again."),
        )
    }
}

impl ui::widget::Decorator for FirewallApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.take_outcome();
        self.take_window_toolbar_events();
        self.drain();
        self.changed = false;
        self.refresh_view();
        self.declare_window_toolbar();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.take_window_toolbar_events();
        self.drain();
        let shell = self.shell.update(state, path);
        shell || self.changed || self.outcome.borrow().is_some()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.wide_from = f64::from(theme.tokens.metrics.compact_breakpoint);
        self.shell.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.shell.apply_layout(taffy, node, origin);
        let wide = self.shell.geometry().content_column.width() >= self.wide_from;
        if wide != self.wide {
            self.wide = wide;
            self.changed = true;
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.shell.handle_event(event, ctx);
        self.drain();
        if !*ctx.handled
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Delete),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            && let Some(id) = self.selected_rule
        {
            self.pending_delete = Some(id);
            self.changed = true;
            *ctx.handled = true;
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // A width that crossed the services' breakpoint rebuilds the page.
        let relayout = self.built.as_ref().is_some_and(|key| key.wide != self.wide);
        relayout || self.shell.needs_animation_frame()
    }

    fn name(&self) -> &'static str {
        "FirewallApp"
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Window)
            .with_name("Firewall")
            .with_bounds(self.shell.get_rect());
        if let Some(notice) = &self.notice {
            root.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(notice.text.clone())
                    .with_hashed_key("firewall.status")
                    .with_live(Politeness::Polite),
            );
        }
        Some(root)
    }
}

#[cfg(test)]
mod tests;
