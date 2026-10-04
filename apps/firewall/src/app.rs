//! Root of the firewall manager.
//!
//! [`FirewallApp`] is a [`NavShell`] window: the zones are the sidebar and the
//! selected zone is the content (see [`crate::view`]). This module owns what
//! the shell cannot: the configuration and where it came from, the zone and
//! rule the person is looking at, the add-rule draft, the delete
//! confirmation, the result toast, and the effects that push the
//! configuration to firewalld.
//!
//! The configuration lives in the shared Automerge document (`doc.rs`): every
//! mutation is a [`FirewallAction`] that addresses a native scalar, text, or
//! stable record, and `sync` adopts remote edits so two instances pointed at
//! one collab socket edit the same ruleset. Which zone is open, the tab and
//! the draft are this window's presentation and never enter the document.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::LocalState;
use ui::a11y::{AccessNode, Politeness, Role};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::toast::{ToastKind, ToastStack};
use ui::widget::{AppModel, ModelPath, NavShell, Widget};

use crate::actions::FirewallAction;
use crate::backend::{Backend, Mode};
use crate::doc::FirewallDoc;
use crate::model::{
    Action, FirewallConfig, Matcher, Proto, Rule, RuleId, Zone, ZoneKind, validate_port,
    validate_source,
};
use crate::view::{self, Draft, Intent, Intents, PageModel, SwitchKey, Switches, Tab};

/// How long a result toast stays before it hides itself.
const TOAST_TTL: Duration = Duration::from_secs(5);

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

#[derive(Clone, Copy)]
enum FirewallEffect {
    Apply,
    SetEnabled(bool),
    SetPanic(bool),
    Retry,
}

struct PendingFirewallEffect {
    invocation: tilo_actions::InvocationId,
    effect: FirewallEffect,
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
    adding: bool,
    unavailable_detail: Option<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags: availability, the shell's build, seeding, and a pending sync"
)]
pub struct FirewallApp {
    shell: NavShell,
    /// The availability the shell's header was built for.
    shell_available: bool,
    intents: Intents,
    switches: Switches,
    enabled_switch: LocalState<bool>,
    panic_switch: LocalState<bool>,

    config: FirewallConfig,
    /// Bumped whenever `config` is replaced.
    rev: u64,
    selected_zone: ZoneKind,
    selected_rule: Option<RuleId>,
    tab: Tab,
    draft: Draft,
    backend: Backend,
    notice: Option<Notice>,
    /// The result toast, and when it was last advanced.
    toasts: ToastStack,
    toasts_ticked: Instant,
    /// The content column is wide enough for the services' descriptions:
    /// at least `wide_from`, the width of a compact window.
    wide: bool,
    wide_from: f64,

    /// Whether the displayed config reflects a real, readable firewall.
    /// `false` means firewalld is absent/stopped and we show an honest
    /// unavailable state instead of any fabricated ruleset.
    available: bool,
    /// The rule the person asked to delete; the dialog confirms it.
    pending_delete: Option<RuleId>,
    /// The add-rule dialog is open.
    adding: bool,

    /// JSON last written to / read from the doc. Guards against re-adopting
    /// our own writes and lets `sync` detect genuine remote edits.
    last_json: Option<String>,
    seeded: bool,

    /// Framework-owned action dispatch: every on-screen affordance routes its
    /// [`FirewallAction`] through here so it brackets exactly **one** undo
    /// boundary, encodes under [`FIREWALL_ACTION_TYPE`](crate::actions::FIREWALL_ACTION_TYPE),
    /// and replays byte-identically on peers — the same path Ctrl+K takes.
    actions: crate::resource::ActionSink,
    pending_effect: Option<PendingFirewallEffect>,
    completed_effect: Option<tilo_actions::InvocationUpdate>,

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

fn shell_for(
    available: bool,
    intents: &Intents,
    enabled: &LocalState<bool>,
    panic: &LocalState<bool>,
) -> NavShell {
    let shell = NavShell::new().with_id("firewall-shell");
    if available {
        shell
            .sidebar_header("Zones", Vec::new())
            .content_header("", view::header_actions(intents, enabled, panic))
    } else {
        shell
    }
}

impl FirewallApp {
    /// The live app: read the **real** current firewall from the host. If
    /// firewalld is absent or stopped, start on an honest unavailable state —
    /// never a fabricated ruleset.
    pub fn new() -> Self {
        match crate::backend::import() {
            Some(config) => Self::from_parts(config, true),
            None => Self::from_parts(FirewallConfig::empty(), false),
        }
    }

    /// The hermetic `--shots` / demo build: seeded with a realistic sample
    /// ruleset so the headless screenshot is fully populated offline. This
    /// is the ONLY place the sample config is used — the live app never
    /// fabricates a firewall.
    pub fn new_demo() -> Self {
        Self::from_parts(crate::sample::sample_config(), true)
    }

    /// A window that never reads the host: it shows whatever configuration
    /// the shared document holds (a capture's example ruleset), and the
    /// unavailable state until it holds one.
    pub fn offline() -> Self {
        Self::from_parts(FirewallConfig::empty(), false)
    }

    fn from_parts(config: FirewallConfig, available: bool) -> Self {
        let intents = Intents::default();
        let enabled_switch = LocalState::new(config.enabled);
        let panic_switch = LocalState::new(config.panic_mode);
        Self {
            shell: shell_for(available, &intents, &enabled_switch, &panic_switch),
            shell_available: available,
            intents,
            switches: Switches::default(),
            enabled_switch,
            panic_switch,
            selected_zone: config.default_zone,
            config,
            rev: 0,
            selected_rule: None,
            tab: Tab::Rules,
            draft: Draft::new(),
            backend: Backend::detect(),
            notice: None,
            toasts: ToastStack::new().with_max(1).with_default_ttl(TOAST_TTL),
            toasts_ticked: ui::time::now(),
            wide: true,
            wide_from: 0.0,
            available,
            pending_delete: None,
            adding: false,
            last_json: None,
            seeded: false,
            actions: crate::resource::ActionSink::default(),
            pending_effect: None,
            completed_effect: None,
            built: None,
            listed: None,
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            changed: true,
        }
    }

    /// Deterministic, presentation-only screenshot states. These never run in
    /// the live constructor and never touch the host firewall; they let visual
    /// review cover empty, invalid, confirmation, and changed states.
    pub fn pose_fixture(&mut self, name: &str) {
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
                self.set_config(FirewallConfig {
                    default_zone: ZoneKind::Home,
                    ..self.config.clone()
                });
                self.report("Home is now the active zone", Tone::Success);
            }
            "service-error" => {
                self.set_config(FirewallConfig::empty());
                self.available = false;
                self.report(
                    "Permission to read firewalld was denied. Ask an administrator for access, then try again.",
                    Tone::Error,
                );
                // The empty state carries the reason; no toast repeats it.
                self.toasts.dismiss(0);
            }
            "services" => self.tab = Tab::Services,
            "selected" => {
                self.selected_rule = self
                    .selected_zone_ref()
                    .and_then(|zone| zone.rules.get(1))
                    .map(|rule| rule.id);
            }
            "panic" => self.set_config(FirewallConfig {
                panic_mode: true,
                ..self.config.clone()
            }),
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
        self.toasts.push(notice.text.clone(), notice.tone.kind());
        self.notice = Some(notice);
        self.changed = true;
    }

    /// Dispatch one [`FirewallAction`] — the single CRDT-mutation path shared
    /// with the Ctrl+K palette. The action is applied through the Tilo action
    /// executor (a pinned actor + committed change, so it replays byte-identically
    /// on peers), exactly as the palette does. The host records **one** undo
    /// boundary bracketing it ([`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action)).
    /// The action mutates its addressed shared document field; we then adopt
    /// the result back into the in-memory view so the next frame paints it, and
    /// remember the JSON so `sync` doesn't re-adopt our own write as a remote
    /// edit.
    ///
    /// This is *only* the CRDT mutation. Pushing the ruleset to the kernel
    /// (`firewall-cmd`/`nft`, see [`Self::apply`]) is a separate side effect
    /// driven by the resulting document state — never part of an action.
    fn perform_action(&mut self, state: &mut AutoCommit, action: &FirewallAction) {
        self.actions.dispatch_with_context(
            state,
            crate::actions::FIREWALL_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.firewall", "Firewall", action),
        );
        if let Some(cfg) = FirewallDoc::load(state) {
            self.last_json = Some(cfg.to_json());
            self.set_config(cfg);
        }
    }

    /// Show a shared configuration the framework resource already holds.
    /// An empty projection does not wipe a config just read from the host.
    pub(crate) fn adopt_resource_state(&mut self, state: &FirewallConfig) {
        if state.zones.is_empty() || state.validate().is_err() {
            return;
        }
        self.set_config(state.clone());
        if !self.available {
            self.available = true;
            self.changed = true;
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`](ui::framework::ResourceSurface).
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Firewall>,
    ) {
        self.actions.attach(dispatcher);
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// The last action dispatched, for the `affordance == ActionSpec` covenant
    /// test.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The configuration Ctrl+K builds its resource tree from, or `None` when the
    /// firewall is unavailable / has no zones (nothing to act on). Exposed to
    /// [`crate::actions`], which can't read the app's private fields directly.
    pub(crate) fn firewall_resource_ctx(&self) -> Option<FirewallConfig> {
        if !self.available || self.config.zones.is_empty() {
            None
        } else {
            Some(self.config.clone())
        }
    }

    // ---- the shell's content ------------------------------------------

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
            adding: self.adding,
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
        if self.shell_available != self.available {
            self.shell = shell_for(
                self.available,
                &self.intents,
                &self.enabled_switch,
                &self.panic_switch,
            );
            self.shell_available = self.available;
            self.built = None;
            self.listed = None;
        }
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
            self.shell.set_content_title("");
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
            pending_delete,
            draft: &self.draft,
            adding: self.adding,
            wide: self.wide,
        };
        let page = view::zone_page(&model, &self.intents, &self.switches, &self.scroll);
        self.shell.set_content_title(zone.kind.title());
        self.shell.set_content(page);
    }

    /// Apply what the window's controls asked for.
    fn drain(&mut self, state: &mut AutoCommit) {
        let intents = self.intents.take();
        let flipped = self.switches.flipped();
        if !intents.is_empty() || !flipped.is_empty() {
            self.changed = true;
        }
        for intent in intents {
            self.apply_intent(&intent, state);
        }
        for (key, on) in flipped {
            match key {
                SwitchKey::Rule(id) => self.set_rule_enabled(id, on, state),
                SwitchKey::Service(id) => self.set_service(id, on, state),
            }
        }
        if self.available {
            let enabled = self.enabled_switch.with(|on| *on);
            if enabled != self.config.enabled {
                let _ = self.set_enabled_and_apply(state, enabled);
            }
            let panic = self.panic_switch.with(|on| *on);
            if panic != self.config.panic_mode {
                let _ = self.set_panic_and_apply(state, panic);
            }
        }
        // A refused change leaves the switches showing the configuration,
        // so it is asked for once, not on every frame.
        self.sync_switches();
    }

    fn apply_intent(&mut self, intent: &Intent, state: &mut AutoCommit) {
        match *intent {
            Intent::SelectZone(zone) => self.select_zone(zone),
            Intent::SelectRule(id) => self.selected_rule = Some(id),
            Intent::SetTab(tab) => self.tab = tab,
            Intent::SetMatchService(service) => {
                self.draft.service = service;
                self.draft.port_error = None;
            }
            Intent::SetProto(proto) => self.draft.proto = proto,
            Intent::SetAction(action) => self.draft.action = action,
            Intent::OpenAddRule => {
                self.adding = true;
                self.draft.port_error = None;
                self.draft.source_error = None;
            }
            Intent::CancelAddRule => self.adding = false,
            Intent::AddRule => self.add_rule_from_draft(state),
            Intent::AskDelete(id) => self.pending_delete = Some(id),
            Intent::ConfirmDelete => {
                if let Some(id) = self.pending_delete {
                    self.delete_rule(id, state);
                }
            }
            Intent::CancelDelete => self.pending_delete = None,
            Intent::MakeActive => self.set_active_zone(state),
            Intent::Apply => {
                let _ = self.apply();
            }
            Intent::Retry => self.retry_connect(state),
        }
    }

    // ---- mutations ------------------------------------------------------
    fn select_zone(&mut self, kind: ZoneKind) {
        if self.selected_zone != kind {
            self.selected_zone = kind;
            self.selected_rule = None;
            self.pending_delete = None;
            self.adding = false;
            self.scroll.set((0.0, 0.0));
        }
    }

    /// Re-query the host after the user has started firewalld or fixed an
    /// authorization problem. This is intentionally read-only until a valid
    /// live configuration is found; recovery never seeds demo data.
    fn retry_connect(&mut self, state: &mut AutoCommit) {
        self.recover_from_probe(crate::backend::import(), state);
    }

    /// Apply the result of the read-only firewalld probe. Every retry route
    /// shares this fail-closed behaviour; a failed probe never mutates rules.
    fn recover_from_probe(&mut self, probe: Option<FirewallConfig>, state: &mut AutoCommit) {
        match probe {
            Some(config) if config.validate().is_ok() => {
                self.selected_zone = config.default_zone;
                self.available = true;
                self.pending_delete = None;
                let json = FirewallDoc::reconcile_probe(state, &config);
                self.last_json = Some(json);
                self.set_config(config);
                self.report("Connected to firewalld", Tone::Success);
            }
            Some(_) => self.report(
                "Firewalld returned rules this app cannot show safely. Nothing was changed.",
                Tone::Error,
            ),
            None => self.report(
                "Firewalld is still not reachable. Start the service, then try again.",
                Tone::Error,
            ),
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

    fn add_rule_from_draft(&mut self, state: &mut AutoCommit) {
        if self.config.zone(self.selected_zone).is_none() {
            return;
        }
        let Some(rule) = self.validated_draft() else {
            return;
        };
        let id = rule.id;
        let title = rule.matcher.title();
        self.perform_action(
            state,
            &crate::actions::add_rule_action(self.selected_zone, rule),
        );
        self.selected_rule = Some(id);
        self.tab = Tab::Rules;
        self.adding = false;
        self.draft.port.set(String::new());
        self.draft.source.set(String::new());
        self.report(format!("Added a rule for {title}"), Tone::Success);
    }

    fn delete_rule(&mut self, id: RuleId, state: &mut AutoCommit) {
        self.perform_action(
            state,
            &crate::actions::delete_rule_action(self.selected_zone, id),
        );
        if self.selected_rule == Some(id) {
            self.selected_rule = None;
        }
        self.pending_delete = None;
        self.report("Rule deleted", Tone::Info);
    }

    fn set_rule_enabled(&mut self, id: RuleId, on: bool, state: &mut AutoCommit) {
        let exists = self
            .selected_zone_ref()
            .is_some_and(|zone| zone.rules.iter().any(|rule| rule.id == id));
        if exists {
            self.perform_action(
                state,
                &crate::actions::set_rule_enabled_action(self.selected_zone, id, on),
            );
        }
    }

    fn set_service(&mut self, id: &str, on: bool, state: &mut AutoCommit) {
        // Resolved at authoring time so the action is order-free on replay.
        self.perform_action(
            state,
            &crate::actions::set_service_action(self.selected_zone, id, on),
        );
    }

    fn set_active_zone(&mut self, state: &mut AutoCommit) {
        self.perform_action(
            state,
            &crate::actions::set_default_zone_action(self.selected_zone),
        );
        self.report(
            format!("{} is now the active zone", self.selected_zone.title()),
            Tone::Success,
        );
    }

    fn set_panic_and_apply(&mut self, state: &mut AutoCommit, on: bool) -> Result<(), String> {
        self.perform_action(state, &crate::actions::set_panic_action(on));
        self.apply()
    }

    fn set_enabled_and_apply(&mut self, state: &mut AutoCommit, on: bool) -> Result<(), String> {
        self.perform_action(state, &crate::actions::set_enabled_action(on));
        self.apply()
    }

    fn apply(&mut self) -> Result<(), String> {
        match self.backend.apply(&self.config) {
            Ok(()) => {
                let n = self.backend.last_plan().len();
                let changes = if n == 1 { "change" } else { "changes" };
                match self.backend.mode() {
                    Mode::Live => self.report(format!("Applied {n} {changes}"), Tone::Success),
                    Mode::DryRun => self.report(
                        format!("{n} {changes} checked. This device does not allow applying them."),
                        Tone::Info,
                    ),
                }
                Ok(())
            }
            Err(e) => {
                self.report(
                    format!("Could not apply the configuration: {e}"),
                    Tone::Error,
                );
                Err(e)
            }
        }
    }

    /// Fold the shared document into the app: seed it once, run a requested
    /// effect, and adopt remote edits.
    fn reconcile(&mut self, state: &mut AutoCommit) {
        if !self.seeded {
            self.seed(state);
            return;
        }
        if let Some(effect) = self.pending_effect.take() {
            let result = match effect.effect {
                FirewallEffect::Apply => self.apply(),
                FirewallEffect::SetEnabled(on) => self.set_enabled_and_apply(state, on),
                FirewallEffect::SetPanic(on) => self.set_panic_and_apply(state, on),
                FirewallEffect::Retry => {
                    self.retry_connect(state);
                    if self.available {
                        Ok(())
                    } else {
                        Err(self
                            .notice
                            .as_ref()
                            .map(|n| n.text.clone())
                            .unwrap_or_default())
                    }
                }
            };
            self.completed_effect = Some(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation: effect.invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation: effect.invocation,
                    error: tilo_actions::ActionFailure {
                        code: "firewall.apply_failed".into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                },
            });
        }
        // Adopt genuine remote edits (collab).
        if let Some(cfg) = FirewallDoc::load(state) {
            let json = cfg.to_json();
            if self.last_json.as_deref() == Some(json.as_str()) {
                return;
            }
            if cfg.validate().is_ok() {
                self.last_json = Some(json);
                self.set_config(cfg);
            } else {
                self.report(
                    "A shared change to the configuration was invalid and was ignored",
                    Tone::Error,
                );
            }
        }
    }

    /// Adopt an existing shared config, or publish the one we read from the
    /// host. We only ever publish REAL data: a config we imported from
    /// firewalld or (in the demo build) the sample. An unavailable firewall
    /// has no zones, so nothing fabricated is ever written into the shared
    /// document.
    fn seed(&mut self, state: &mut AutoCommit) {
        self.seeded = true;
        if let Some(cfg) = FirewallDoc::load(state) {
            if cfg.validate().is_err() {
                self.report(
                    "The shared configuration was invalid and was not loaded",
                    Tone::Error,
                );
                return;
            }
            self.available = true;
            self.set_config(cfg);
            // A legacy JSON document is accepted once, then promoted to
            // native maps before any visible control can dispatch.
            if FirewallDoc::has_native(state) {
                self.last_json = Some(self.config.to_json());
            } else {
                self.perform_action(
                    state,
                    &FirewallAction::PublishImported {
                        config: self.config.clone(),
                    },
                );
            }
        } else if !self.config.zones.is_empty() {
            self.perform_action(
                state,
                &FirewallAction::PublishImported {
                    config: self.config.clone(),
                },
            );
        }
    }

    /// Advance the result toast's clock and place it in the window's
    /// bottom trailing corner; the stack expires, paints and dismisses it.
    fn place_toast(&mut self, window: Rect, theme: &Theme) {
        let now = ui::time::now();
        self.toasts
            .tick(now.saturating_duration_since(self.toasts_ticked));
        self.toasts_ticked = now;
        self.toasts
            .place_bottom_right(window, f64::from(theme.tokens.spacing.s6));
    }

    /// When the showing toast expires, so the window wakes to hide it. A
    /// toast under the pointer waits for it to leave.
    fn toast_deadline(&self) -> Option<Instant> {
        if self.toasts.is_hovered() {
            return None;
        }
        self.toasts
            .iter()
            .map(|toast| toast.ttl_remaining)
            .min()
            .map(|left| self.toasts_ticked + left)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect {
            return Ok(None);
        }
        if self.pending_effect.is_some() {
            return Err("another firewall operation is still running".into());
        }
        let effect = match type_id {
            "firewall.retry" => FirewallEffect::Retry,
            _ if !self.available => return Err("Firewall service is unavailable".into()),
            "firewall.apply" => FirewallEffect::Apply,
            "firewall.set-enabled" => FirewallEffect::SetEnabled(
                serde_json::from_slice(payload).map_err(|error| error.to_string())?,
            ),
            "firewall.set-panic" => FirewallEffect::SetPanic(
                serde_json::from_slice(payload).map_err(|error| error.to_string())?,
            ),
            _ => return Ok(None),
        };
        self.pending_effect = Some(PendingFirewallEffect { invocation, effect });
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
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

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::undo::CommittedActions>() {
            request.provide::<dyn ui::undo::CommittedActions>(self);
            return;
        }
        self.shell.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::undo::CommittedActions>() {
            request.provide::<dyn ui::undo::CommittedActions>(self);
            return;
        }
        self.shell.provide_mut(request);
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "firewall.apply",
            "firewall.set-enabled",
            "firewall.set-panic",
            "firewall.retry",
        ]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(&mut self.shell, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(&mut self.shell, op, doc, path)? {
            return Ok(Some(started));
        }
        self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
        )
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if cancellation_requested
            && self
                .pending_effect
                .as_ref()
                .is_some_and(|pending| pending.invocation == invocation)
        {
            self.pending_effect = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("cancelled before firewall changes were applied".into()),
            });
        }
        if self
            .completed_effect
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.completed_effect.take();
        }
        None
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.reconcile(doc);
            self.drain(doc);
        }
        self.changed = false;
        self.refresh_view();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.drain(doc);
        }
        let shell = self.shell.update(state, path);
        shell || self.changed || self.pending_effect.is_some()
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
        // The result toast: hovering holds it, a click dismisses it.
        self.toasts.handle_event(event, ctx);
        if *ctx.handled {
            return;
        }
        self.shell.handle_event(event, ctx);
        self.drain(ctx.state);
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

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_overlay(scene, transform, text, theme, presence);
        let window = presence
            .viewport
            .get()
            .unwrap_or_else(|| self.shell.get_rect());
        self.place_toast(window, theme);
        self.toasts.draw(scene, transform, text, theme, presence);
    }

    fn needs_animation_frame(&self) -> bool {
        // A width that crossed the services' breakpoint rebuilds the page.
        let relayout = self.built.as_ref().is_some_and(|key| key.wide != self.wide);
        relayout || self.shell.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let shell = self.shell.next_wake_deadline();
        match (shell, self.toast_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
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

impl ui::resources::PublishesResources for FirewallApp {
    /// Publish the open configuration so the host's Ctrl+K palette scopes to
    /// it. When the framework hosts the surface, that door already owns the
    /// config — only service-recovery stays here.
    #[allow(
        clippy::expect_used,
        reason = "the empty string operation payload is always serializable"
    )]
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if !self.available {
            return vec![
                tilo_actions::Resource::new(
                    "firewall.recovery",
                    "firewall.recovery",
                    "Firewall service recovery",
                )
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "retry",
                        "Retry firewalld connection",
                        tilo_actions::OperationKind::Effect,
                        "firewall.retry",
                        &"",
                    )
                    .expect("firewall retry payload is serializable")
                    .with_contract(tilo_actions::ActionContract {
                        retry: tilo_actions::RetryPolicy::Idempotent,
                        discovery: tilo_actions::DiscoveryScope::Launchable,
                        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                        result: tilo_actions::ResultSpec::Job,
                        tags: vec!["security".into(), "firewall".into(), "recovery".into()],
                        ..tilo_actions::ActionContract::default()
                    }),
                ),
            ];
        }
        // The framework root publishes the config document (including apply).
        // When hosted and available, return nothing — recovery above stays as
        // the only live extra (it is not in Resource::children).
        if self.actions.is_hosted() {
            return Vec::new();
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.actions.is_hosted() && id == "firewall.config" {
            return None;
        }
        find_resource_tree(&self.publish_resources(doc), id)
    }
}

fn find_resource_tree(
    resources: &[tilo_actions::Resource],
    id: &str,
) -> Option<tilo_actions::Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_resource_tree(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

impl ui::undo::CommittedActions for FirewallApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its firewall action, so it records one undo boundary for it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

#[cfg(test)]
mod tests;
