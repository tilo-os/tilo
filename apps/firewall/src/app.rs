//! Root of the firewall manager.
//!
//! [`FirewallApp`] is a [`NavShell`] window: the zones are the sidebar and the
//! selected zone is the content (see [`crate::view`]). This module owns what
//! the shell cannot: the configuration and where it came from, the zone and
//! rule the person is looking at, the add-rule draft, the delete
//! confirmation, the last result, and the effects that push the
//! configuration to firewalld.
//!
//! The configuration lives in the shared Automerge document (`doc.rs`): every
//! edit is a [`FirewallAction`] submitted through the framework dispatcher,
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
use ui::theme::Theme;
use ui::widget::data_table::TableSort;
use ui::widget::toast::ToastKind;
use ui::widget::{AppModel, ModelPath, NavShell, Widget};

use ui::framework::{Dispatcher, Setting};

use crate::actions::FirewallAction;
use crate::model::{
    Action, FirewallConfig, Matcher, Proto, Rule, RuleId, Zone, ZoneKind, validate_port,
    validate_source,
};
use crate::resource::{AppliedConfig, ApplyOutcome, Firewall, FirewallEffect};
use crate::view::{self, Draft, Intent, Intents, PageModel, SwitchKey, Switches, Tab};

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
    adding: bool,
    unavailable_detail: Option<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags: availability, the shell's build, the add-rule dialog, and a pending sync"
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
    /// The rule the person asked to delete; the dialog confirms it.
    pending_delete: Option<RuleId>,
    /// The add-rule dialog is open.
    adding: bool,

    /// Where every on-screen affordance submits its [`FirewallAction`]: one
    /// undo step each, replayed byte-identically on peers — the same door
    /// Ctrl+K takes. `None` only for a widget no resource hosts.
    dispatcher: Option<Dispatcher<Firewall>>,
    /// What this device last applied to its firewalld.
    applied: Option<Setting<AppliedConfig>>,
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
            rule_sort: LocalState::new(Some(TableSort::ascending("action"))),
            tab: Tab::Rules,
            draft: Draft::new(),
            notice: None,
            wide: true,
            wide_from: 0.0,
            available,
            pending_delete: None,
            adding: false,
            dispatcher: None,
            applied: None,
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

    /// Submit one [`FirewallAction`] — the single write path shared with the
    /// Ctrl+K palette. The framework applies it after the event and presents
    /// the stored configuration back, which the next frame shows.
    ///
    /// This is *only* the document edit. Pushing the ruleset to the kernel
    /// (`firewall-cmd`/`nft`) follows from the stored configuration on each
    /// device — never part of an action.
    fn dispatch(&self, action: &FirewallAction) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.apply(action);
        }
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

    /// Where the projection reports how the latest apply on this device
    /// ended.
    pub(crate) fn apply_outcome(&self) -> ApplyOutcome {
        Rc::clone(&self.outcome)
    }

    /// Whether there is a configuration to edit and apply.
    pub(crate) fn is_available(&self) -> bool {
        self.available
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
            self.shell.set_content_title("Firewall");
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
            draft: &self.draft,
            adding: self.adding,
            wide: self.wide,
        };
        let page = view::zone_page(&model, &self.intents, &self.switches, &self.scroll);
        self.shell.set_content_title(zone.kind.title());
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
        if self.available {
            let enabled = self.enabled_switch.with(|on| *on);
            if enabled != self.config.enabled {
                self.dispatch(&crate::actions::set_enabled_action(enabled));
            }
            let panic = self.panic_switch.with(|on| *on);
            if panic != self.config.panic_mode {
                self.dispatch(&crate::actions::set_panic_action(panic));
            }
        }
        self.sync_switches();
    }

    fn apply_intent(&mut self, intent: &Intent) {
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
            Intent::AddRule => self.add_rule_from_draft(),
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

    fn add_rule_from_draft(&mut self) {
        if self.config.zone(self.selected_zone).is_none() {
            return;
        }
        let Some(rule) = self.validated_draft() else {
            return;
        };
        let id = rule.id;
        let title = rule.matcher.title();
        self.dispatch(&crate::actions::add_rule_action(self.selected_zone, rule));
        self.selected_rule = Some(id);
        self.tab = Tab::Rules;
        self.adding = false;
        self.draft.port.set(String::new());
        self.draft.source.set(String::new());
        self.report(format!("Added a rule for {title}"), Tone::Success);
    }

    fn delete_rule(&mut self, id: RuleId) {
        self.dispatch(&crate::actions::delete_rule_action(self.selected_zone, id));
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
            self.dispatch(&crate::actions::set_rule_enabled_action(
                self.selected_zone,
                id,
                on,
            ));
        }
    }

    fn set_service(&self, id: &str, on: bool) {
        // Resolved at authoring time so the action is order-free on replay.
        self.dispatch(&crate::actions::set_service_action(
            self.selected_zone,
            id,
            on,
        ));
    }

    fn set_active_zone(&mut self) {
        self.dispatch(&crate::actions::set_default_zone_action(self.selected_zone));
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
        self.drain();
        self.changed = false;
        self.refresh_view();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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
