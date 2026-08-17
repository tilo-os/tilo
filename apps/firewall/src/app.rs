//! Root widget for the firewall manager.
//!
//! The firewall leads with **intent, not internals**. Its primary surface is a
//! plain-language stack of cards — a protection status hero, a "which network
//! am I on?" trust-profile picker, and simple Allow/Block switches for the
//! services a person recognises. The full firewalld machinery (all nine zones,
//! the raw rule list, custom port rules, per-zone default policy) stays
//! reachable behind an **Advanced** disclosure, so nothing is removed — it is
//! layered.
//!
//! ## Composition, not hand-painting
//!
//! Unlike the previous incarnation (one hand-painted widget with manual
//! hit-tests), [`FirewallApp`] is a thin host over a **tree of shared `ui`
//! widgets** — [`Card`](ui::widget::Card), [`Toggle`](ui::widget::Toggle),
//! [`ToggleGroup`](ui::widget::ToggleGroup), [`Button`](ui::widget::Button),
//! [`Dropdown`](ui::widget::Dropdown), [`TextInput`](ui::widget::TextInput),
//! rows and columns. The toolkit gives every control keyboard focus, the shared
//! focus ring, accessibility, hover/press/disabled states and theming for free
//! (HIG §8/§9). The tree is rebuilt from the model whenever the view structure
//! changes (a profile switch, the Advanced disclosure, an applied action), the
//! same pattern the Settings panes use.
//!
//! ## Where a control's press goes
//!
//! Two channels turn a widget interaction into a firewall change:
//!
//! * **Local presentation** (which profile is viewed, whether Advanced is open,
//!   the rules filter) rides a shared [`Rc<Cell>`]/[`Rc<RefCell>`] the widget's
//!   `on_change`/`on_click` closure writes and this host drains after
//!   forwarding the event — it never touches the shared document.
//! * **Real firewall mutations** (enable, panic, allow a service, change a
//!   rule, set a zone's policy, apply) are dispatched as [`FirewallAction`]s
//!   through [`ui::ActionHost`] — the single CRDT-mutation + undo-bracket path
//!   the Ctrl+K palette shares. Switches/dropdowns mirror their value to a doc
//!   key; after a *local* event this host reconciles those keys against the
//!   model and dispatches the matching action, so only the acting peer mutates
//!   and remote peers converge purely through the config CRDT.
//!
//! Pushing the reconciled ruleset to the kernel is a separate side effect
//! (see [`crate::backend`]) driven by the document state — never part of an
//! action.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Element, Widget};

use crate::actions::{
    add_rule_action, delete_rule_action, edit_rule_action, set_default_zone_action,
    set_enabled_action, set_panic_action, set_service_action, set_zone_target_action,
};
use crate::backend::{Backend, Mode};
use crate::doc::FirewallDoc;
use crate::model::{
    Action, FirewallConfig, Matcher, Proto, Rule, RuleId, Target, Zone, ZoneKind,
};

// ---- doc keys the composed controls mirror their state through ----------
pub(crate) const K_ENABLED: &str = "fw.enabled";
pub(crate) const K_PANIC: &str = "fw.panic";
pub(crate) const K_FORM_PORT: &str = "fw.form.port";
pub(crate) const K_FORM_SOURCE: &str = "fw.form.source";
/// The Advanced zone picker mirrors its choice (a zone profile name) here.
pub(crate) const K_VIEWZONE: &str = "fw.viewzone";

pub(crate) fn k_service(z: ZoneKind, id: &str) -> String {
    format!("fw.svc.{}.{}", z.name(), id)
}
pub(crate) fn k_rule(z: ZoneKind, id: RuleId) -> String {
    format!("fw.rule.{}.{}", z.name(), id)
}
pub(crate) fn k_target(z: ZoneKind) -> String {
    format!("fw.target.{}", z.name())
}

/// Which rules the Advanced rule list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Allow,
    Deny,
}

impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Allow => "Allowed",
            Filter::Deny => "Blocked",
        }
    }
    pub const ALL: [Filter; 3] = [Filter::All, Filter::Allow, Filter::Deny];
}

/// The Advanced "default policy" dropdown options, human-labelled.
pub(crate) const TARGET_LABELS: [(&str, Target); 4] = [
    ("Block unmatched traffic (recommended)", Target::Default),
    ("Reject with an error", Target::Reject),
    ("Silently drop", Target::Drop),
    ("Allow everything in", Target::Accept),
];

pub(crate) fn target_label(t: Target) -> &'static str {
    TARGET_LABELS
        .iter()
        .find(|(_, tt)| *tt == t)
        .map(|(l, _)| *l)
        .unwrap_or("Block unmatched traffic (recommended)")
}

fn target_from_label(s: &str) -> Option<Target> {
    TARGET_LABELS.iter().find(|(l, _)| *l == s).map(|(_, t)| *t)
}

/// A discrete user intent captured by a `Button`'s `on_click` closure and
/// drained by the host after the event is forwarded. Kept local (never in the
/// document) so only the acting peer reacts.
#[derive(Clone, Debug)]
pub(crate) enum Intent {
    ToggleAdvanced,
    /// Pick a friendly trust profile for this network — makes it the active
    /// (default) zone and views it.
    UseProfile(ZoneKind),
    /// Make the currently-viewed zone the active/default zone.
    MakeActive,
    SetFilter(Filter),
    AddRule,
    DeleteRule(RuleId),
    /// The advanced add-rule form's protocol / action segmented choices.
    SetFormProto(Proto),
    SetFormAction(Action),
    Apply,
}

/// The shared channel the composed widgets push [`Intent`]s onto.
pub(crate) type IntentBus = Rc<RefCell<Vec<Intent>>>;

/// One reconcilable control: after a local event we read its mirrored doc key
/// and, if it disagrees with the model, dispatch the matching action.
enum Binding {
    Enabled,
    Panic,
    /// The Advanced zone picker (presentation only — reconciles to
    /// `selected_zone`, never an action).
    ViewZone,
    Service { zone: ZoneKind, id: String },
    RuleEnabled { zone: ZoneKind, id: RuleId },
    ZoneTarget { zone: ZoneKind },
}

pub struct FirewallApp {
    config: FirewallConfig,
    /// Whether the displayed config reflects a real, readable firewall. `false`
    /// means firewalld is absent/stopped and we show an honest unavailable
    /// state instead of any fabricated ruleset.
    available: bool,

    /// The zone currently *viewed* (its services / rules shown). Presentation
    /// only — never an action, never in the shared document.
    selected_zone: ZoneKind,
    /// Whether the Advanced disclosure is expanded (presentation only).
    advanced_open: bool,
    /// Advanced rule-list filter (presentation only).
    filter: Filter,
    /// The advanced add-rule form's draft protocol + action (presentation).
    form_proto: Proto,
    form_action: Action,

    /// One-line feedback for the last change (e.g. "SSH allowed in Home").
    status: String,

    backend: Backend,

    // ---- presentation signal channels the composed tree writes -----------
    intents: IntentBus,

    // ---- composed widget tree + host bookkeeping -------------------------
    composed: Option<Element>,
    needs_rebuild: bool,
    rect: Rect,
    node: Option<NodeId>,

    /// JSON last written to / read from the doc. Guards against re-adopting our
    /// own writes and lets `sync` detect genuine remote edits.
    last_json: Option<String>,
    seeded: bool,

    /// Framework-owned action dispatch: every on-screen affordance routes its
    /// [`FirewallAction`](crate::actions::FirewallAction) through here so it
    /// brackets exactly one undo boundary, encodes under
    /// [`FIREWALL_ACTION_TYPE`](crate::actions::FIREWALL_ACTION_TYPE), and
    /// replays byte-identically on peers — the same path Ctrl+K takes.
    actions: ui::ActionHost,
}

impl FirewallApp {
    /// The live app: read the real current firewall from the host. If firewalld
    /// is absent or stopped, start on an honest empty/unavailable state — never
    /// a fabricated ruleset.
    pub fn new() -> Self {
        match crate::backend::import() {
            Some(config) => Self::from_parts(config, true),
            None => Self::from_parts(FirewallConfig::empty(), false),
        }
    }

    /// The hermetic `--shots` / demo build: seeded with a realistic sample
    /// ruleset so the headless screenshot is fully populated offline. This is
    /// the ONLY place the sample config is used.
    pub fn new_demo() -> Self {
        Self::from_parts(crate::sample::sample_config(), true)
    }

    fn from_parts(config: FirewallConfig, available: bool) -> Self {
        let selected_zone = config.default_zone;
        Self {
            config,
            available,
            selected_zone,
            // `--shots` hook: force the Advanced disclosure open so its screen
            // can be captured headlessly.
            advanced_open: std::env::var("QOS_FIREWALL_ADVANCED").as_deref() == Ok("1"),
            filter: Filter::All,
            form_proto: Proto::Tcp,
            form_action: Action::Allow,
            status: String::new(),
            backend: Backend::detect(),
            intents: Rc::new(RefCell::new(Vec::new())),
            composed: None,
            needs_rebuild: true,
            rect: Rect::ZERO,
            node: None,
            last_json: None,
            seeded: false,
            actions: ui::ActionHost::new(),
        }
    }

    // ---- read-side accessors used by the view builders -------------------
    pub(crate) fn cfg(&self) -> &FirewallConfig {
        &self.config
    }
    pub(crate) fn is_available(&self) -> bool {
        self.available
    }
    pub(crate) fn active_zone(&self) -> ZoneKind {
        self.config.default_zone
    }
    pub(crate) fn viewed_zone(&self) -> ZoneKind {
        self.selected_zone
    }
    pub(crate) fn is_advanced_open(&self) -> bool {
        self.advanced_open
    }
    pub(crate) fn current_filter(&self) -> Filter {
        self.filter
    }
    pub(crate) fn form_proto(&self) -> Proto {
        self.form_proto
    }
    pub(crate) fn form_action(&self) -> Action {
        self.form_action
    }
    pub(crate) fn status_text(&self) -> &str {
        &self.status
    }
    pub(crate) fn backend_mode(&self) -> Mode {
        self.backend.mode()
    }
    pub(crate) fn intents(&self) -> IntentBus {
        self.intents.clone()
    }

    pub(crate) fn zone_ref(&self, kind: ZoneKind) -> Option<&Zone> {
        self.config.zone(kind)
    }

    /// Rule indices in `zone` passing the active filter.
    pub(crate) fn filtered_rule_ids(&self, kind: ZoneKind) -> Vec<RuleId> {
        let Some(z) = self.config.zone(kind) else {
            return Vec::new();
        };
        z.rules
            .iter()
            .filter(|r| match self.filter {
                Filter::All => true,
                Filter::Allow => r.action == Action::Allow,
                Filter::Deny => r.action == Action::Deny,
            })
            .map(|r| r.id)
            .collect()
    }

    /// The "N allowed in" count on the active zone — enabled allow rules plus
    /// enabled named services — for the at-a-glance status line.
    pub(crate) fn allowed_in_count(&self, kind: ZoneKind) -> usize {
        let Some(z) = self.config.zone(kind) else {
            return 0;
        };
        let allow_rules = z.rules.iter().filter(|r| r.enabled && r.action == Action::Allow).count();
        allow_rules + z.services.len()
    }

    /// The configuration Ctrl+K builds its resource tree from, or `None` when
    /// the firewall is unavailable / has no zones.
    pub(crate) fn firewall_resource_ctx(&self) -> Option<FirewallConfig> {
        if !self.available || self.config.zones.is_empty() {
            None
        } else {
            Some(self.config.clone())
        }
    }

    // ---- action dispatch -------------------------------------------------

    /// Dispatch one [`FirewallAction`] through the shared executor (a pinned
    /// actor + committed change, so it replays byte-identically on peers),
    /// exactly as the Ctrl+K palette does, then adopt the mutated config back
    /// into the in-memory view.
    fn perform_action(&mut self, state: &mut AutoCommit, action: crate::actions::FirewallAction) {
        self.actions
            .dispatch(state, crate::actions::FIREWALL_ACTION_TYPE, &action);
        if let Some(json) = FirewallDoc::load_json(state) {
            if let Some(cfg) = FirewallConfig::from_json(&json) {
                self.config = cfg;
            }
            self.last_json = Some(json);
        }
        self.needs_rebuild = true;
    }

    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    // ---- doc key helpers -------------------------------------------------
    fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> Option<bool> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Boolean(b) => Some(*b),
                _ => None,
            },
            _ => None,
        }
    }

    fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(t) => Some(t.to_string()),
                _ => None,
            },
            Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
            _ => None,
        }
    }

    /// Set `key` to `desired` only when it currently differs — so seeding the
    /// control mirrors each frame is churn-free at steady state (HIG §6).
    fn seed_bool(state: &mut AutoCommit, root: &ObjId, key: &str, desired: bool) {
        if Self::read_bool(state, root, key) != Some(desired) {
            let _ = state.put(root, key, ScalarValue::Boolean(desired));
        }
    }
    fn seed_str(state: &mut AutoCommit, root: &ObjId, key: &str, desired: &str) {
        if Self::read_str(state, root, key).as_deref() != Some(desired) {
            let _ = state.put(root, key, desired);
        }
    }

    /// Every reconcilable control currently rendered. Used to seed the mirror
    /// keys before the child widgets read them, and to reconcile them back to
    /// the model after a local event. Derived from the same view state the tree
    /// is built from, so the two stay in lock-step.
    fn bindings(&self) -> Vec<Binding> {
        let mut out = Vec::new();
        if !self.available {
            return out;
        }
        out.push(Binding::Enabled);
        out.push(Binding::Panic);

        // Friendly service switches operate on the active network profile.
        let active = self.config.default_zone;
        for id in crate::model::COMMON_SERVICE_IDS {
            if self.config.zone(active).is_some() {
                out.push(Binding::Service { zone: active, id: (*id).to_string() });
            }
        }

        if self.advanced_open {
            let z = self.selected_zone;
            if self.config.zone(z).is_some() {
                out.push(Binding::ViewZone);
                out.push(Binding::ZoneTarget { zone: z });
                // Full service catalogue for the viewed zone.
                for def in crate::model::ServiceCatalog::ALL {
                    out.push(Binding::Service { zone: z, id: def.id.to_string() });
                }
                // Enable switch per rule in the viewed zone (filtered view).
                for id in self.filtered_rule_ids(z) {
                    out.push(Binding::RuleEnabled { zone: z, id });
                }
            }
        }
        out
    }

    /// Seed the mirror doc keys from the model before the child widgets read
    /// them (so a switch shows the real state on its very first frame — this is
    /// what makes a single-frame `--shots` capture correct).
    fn seed_control_keys(&self, state: &mut AutoCommit, root: &ObjId) {
        if !self.available {
            return;
        }
        Self::seed_bool(state, root, K_ENABLED, self.config.enabled);
        Self::seed_bool(state, root, K_PANIC, self.config.panic_mode);
        for b in self.bindings() {
            match b {
                Binding::Enabled | Binding::Panic => {}
                Binding::ViewZone => {
                    Self::seed_str(state, root, K_VIEWZONE, self.selected_zone.profile_name());
                }
                Binding::Service { zone, id } => {
                    let on = self.config.zone(zone).map(|z| z.has_service(&id)).unwrap_or(false);
                    Self::seed_bool(state, root, &k_service(zone, &id), on);
                }
                Binding::RuleEnabled { zone, id } => {
                    let on = self
                        .config
                        .zone(zone)
                        .and_then(|z| z.rules.iter().find(|r| r.id == id))
                        .map(|r| r.enabled)
                        .unwrap_or(false);
                    Self::seed_bool(state, root, &k_rule(zone, id), on);
                }
                Binding::ZoneTarget { zone } => {
                    let t = self.config.zone(zone).map(|z| z.target).unwrap_or(Target::Default);
                    Self::seed_str(state, root, &k_target(zone), target_label(t));
                }
            }
        }
    }

    /// After a *local* event, compare each control's mirror key against the
    /// model and dispatch the matching action for any that diverged. Runs only
    /// where the input physically happened, so a remote peer never double-fires.
    fn reconcile_controls(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.available {
            return;
        }
        // Master enable / panic.
        if let Some(v) = Self::read_bool(state, root, K_ENABLED) {
            if v != self.config.enabled {
                self.perform_action(state, set_enabled_action(v));
                self.status = if v { "Firewall turned on".into() } else { "Firewall turned off".into() };
            }
        }
        if let Some(v) = Self::read_bool(state, root, K_PANIC) {
            if v != self.config.panic_mode {
                self.perform_action(state, set_panic_action(v));
                self.status = if v {
                    "Lockdown on — all traffic blocked".into()
                } else {
                    "Lockdown off".into()
                };
            }
        }

        for b in self.bindings() {
            match b {
                Binding::Enabled | Binding::Panic => {}
                Binding::ViewZone => {
                    if let Some(s) = Self::read_str(state, root, K_VIEWZONE) {
                        if let Some(z) = crate::model::zone_from_profile_name(&s) {
                            if z != self.selected_zone && self.config.zone(z).is_some() {
                                self.selected_zone = z;
                                self.needs_rebuild = true;
                            }
                        }
                    }
                }
                Binding::Service { zone, id } => {
                    let key = k_service(zone, &id);
                    let Some(v) = Self::read_bool(state, root, &key) else { continue };
                    let cur = self.config.zone(zone).map(|z| z.has_service(&id)).unwrap_or(false);
                    if v != cur {
                        self.perform_action(state, set_service_action(zone, id.clone(), v));
                        let name = crate::model::ServiceCatalog::name_of(&id);
                        self.status = format!(
                            "{name} {} in {}",
                            if v { "allowed" } else { "blocked" },
                            zone.profile_name()
                        );
                    }
                }
                Binding::RuleEnabled { zone, id } => {
                    let key = k_rule(zone, id);
                    let Some(v) = Self::read_bool(state, root, &key) else { continue };
                    // Clone the target rule (ending the borrow of `self.config`)
                    // *before* dispatching. EditRule carries the whole rule with
                    // the desired absolute `enabled`, so replay is idempotent.
                    let edited = self
                        .config
                        .zone(zone)
                        .and_then(|z| z.rules.iter().find(|r| r.id == id))
                        .filter(|r| r.enabled != v)
                        .map(|r| {
                            let mut e = r.clone();
                            e.enabled = v;
                            e
                        });
                    if let Some(edited) = edited {
                        self.perform_action(state, edit_rule_action(zone, edited));
                        self.status = if v { "Rule enabled".into() } else { "Rule disabled".into() };
                    }
                }
                Binding::ZoneTarget { zone } => {
                    let key = k_target(zone);
                    let Some(s) = Self::read_str(state, root, &key) else { continue };
                    let Some(t) = target_from_label(&s) else { continue };
                    let cur = self.config.zone(zone).map(|z| z.target).unwrap_or(Target::Default);
                    if t != cur {
                        self.perform_action(state, set_zone_target_action(zone, t));
                        self.status = format!("Default policy for {} updated", zone.profile_name());
                    }
                }
            }
        }
    }

    /// Drain the local intent bus and apply each one (presentation change or a
    /// dispatched action).
    fn drain_intents(&mut self, state: &mut AutoCommit) {
        let intents: Vec<Intent> = self.intents.borrow_mut().drain(..).collect();
        for intent in intents {
            match intent {
                Intent::ToggleAdvanced => {
                    self.advanced_open = !self.advanced_open;
                    self.needs_rebuild = true;
                }
                Intent::UseProfile(zone) => {
                    self.selected_zone = zone;
                    if self.config.zone(zone).is_some() && self.config.default_zone != zone {
                        self.perform_action(state, set_default_zone_action(zone));
                    }
                    self.status = format!("On the {} profile", zone.profile_name());
                    self.needs_rebuild = true;
                }
                Intent::MakeActive => {
                    let zone = self.selected_zone;
                    if self.config.zone(zone).is_some() && self.config.default_zone != zone {
                        self.perform_action(state, set_default_zone_action(zone));
                        self.status = format!("{} is now the active zone", zone.profile_name());
                    }
                }
                Intent::SetFilter(f) => {
                    self.filter = f;
                    self.needs_rebuild = true;
                }
                Intent::SetFormProto(p) => {
                    self.form_proto = p;
                    self.needs_rebuild = true;
                }
                Intent::SetFormAction(a) => {
                    self.form_action = a;
                    self.needs_rebuild = true;
                }
                Intent::AddRule => self.add_rule_from_form(state),
                Intent::DeleteRule(id) => {
                    self.perform_action(state, delete_rule_action(self.selected_zone, id));
                    self.status = "Rule deleted".into();
                }
                Intent::Apply => self.apply(),
            }
        }
    }

    fn add_rule_from_form(&mut self, state: &mut AutoCommit) {
        let zone = self.selected_zone;
        if self.config.zone(zone).is_none() {
            return;
        }
        let root = &automerge::ROOT;
        let port = Self::read_str(state, root, K_FORM_PORT).unwrap_or_default();
        let port = port.trim().to_string();
        if port.is_empty() {
            self.status = "Enter a port to add a rule".into();
            self.needs_rebuild = true;
            return;
        }
        let source = Self::read_str(state, root, K_FORM_SOURCE).unwrap_or_default();
        let matcher = Matcher::Port { port: port.clone(), proto: self.form_proto };
        let rule = Rule::new(matcher, self.form_action, source.trim().to_string());
        self.perform_action(state, add_rule_action(zone, rule));
        // Clear the draft fields so the next add starts fresh.
        let _ = state.put(root, K_FORM_PORT, "");
        let _ = state.put(root, K_FORM_SOURCE, "");
        self.status = format!(
            "{} {}/{} in {}",
            if self.form_action == Action::Allow { "Allowed" } else { "Blocked" },
            port,
            self.form_proto.label(),
            zone.profile_name()
        );
    }

    fn apply(&mut self) {
        match self.backend.apply(&self.config) {
            Ok(()) => {
                let n = self.backend.last_plan().len();
                self.status = match self.backend.mode() {
                    Mode::Live => format!("Applied — {n} firewall changes are now live"),
                    Mode::DryRun => {
                        format!("Ready to apply {n} changes (arm with QOS_FIREWALL_APPLY=1)")
                    }
                };
            }
            Err(e) => self.status = format!("Couldn't apply: {e}"),
        }
        self.needs_rebuild = true;
    }
}

impl Default for FirewallApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for FirewallApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            // Adopt an existing shared config, or publish the real one we read
            // from the host. Only REAL data is ever published; an unavailable
            // firewall has no zones, so nothing fabricated is written.
            if let Some(json) = FirewallDoc::load_json(state) {
                if let Some(cfg) = FirewallConfig::from_json(&json) {
                    self.config = cfg;
                    self.available = true;
                    if self.config.zone(self.selected_zone).is_none() {
                        self.selected_zone = self.config.default_zone;
                    }
                    self.last_json = Some(json);
                }
            } else if !self.config.zones.is_empty() {
                let json = FirewallDoc::store(state, &self.config);
                self.last_json = Some(json);
            }
            self.seeded = true;
            self.needs_rebuild = true;
        } else if let Some(json) = FirewallDoc::load_json(state) {
            // Adopt genuine remote edits (collab).
            if self.last_json.as_deref() != Some(json.as_str()) {
                if let Some(cfg) = FirewallConfig::from_json(&json) {
                    self.config = cfg;
                    if self.config.zone(self.selected_zone).is_none() {
                        self.selected_zone = self.config.default_zone;
                    }
                    self.last_json = Some(json);
                    self.needs_rebuild = true;
                }
            }
        }

        // Keep the control mirror keys in step with the model *before* the
        // child widgets read them.
        self.seed_control_keys(state, path);

        // (Re)build the composed tree when the view structure changed, and sync
        // it immediately so children reflect the current state on this frame —
        // essential for the single-frame `--shots` path.
        if self.needs_rebuild || self.composed.is_none() {
            let mut tree = self.build_composition();
            tree.sync(state, path);
            self.composed = Some(tree);
            // `needs_rebuild` stays set so `take_structure_dirty` tells the host
            // to re-register the fresh subtree in `build_layout`.
        } else if let Some(c) = &mut self.composed {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.composed.is_none() {
            self.composed = Some(self.build_composition());
        }
        let child = self
            .composed
            .as_mut()
            .unwrap()
            .build_layout(taffy, theme, node_map);
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&cn) = children.first() {
                    c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, t, tr, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw_with_presence(scene, t, tr, theme, presence, permissions);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Forward the event to the composed tree first (a control may write its
        // mirror key or push an intent), then reconcile / drain the results.
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&cn) = children.first() {
                    c.handle_event(
                        event, scale_factor, modifiers, state, path, handled, focus_target, cn,
                        taffy,
                    );
                }
            }
        }
        // A pointer move never changes a control's state; skip the doc reads.
        if !matches!(event, WindowEvent::CursorMoved { .. }) {
            self.drain_intents(state);
            self.reconcile_controls(state, path);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.as_ref().map(|c| c.as_ref() as &dyn Widget).into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str {
        "FirewallApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.needs_rebuild);
        let child = self
            .composed
            .as_mut()
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false);
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.composed.as_ref().is_some_and(|c| c.needs_animation_frame())
    }

    /// Publish the open configuration + its zones/rules so the host's Ctrl+K
    /// palette scopes to them.
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Bubble the undo bracket up from whichever control produced it: our own
    /// [`ActionHost`] (switches/dropdowns/buttons reconciled here) first, then
    /// any deeper widget that dispatched.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        if let Some(heads) = self.actions.take_committed() {
            return Some(heads);
        }
        fn walk(w: &mut dyn Widget) -> Option<Vec<automerge::ChangeHash>> {
            if let Some(h) = w.take_committed_action() {
                return Some(h);
            }
            for child in w.children_mut() {
                if let Some(h) = walk(child) {
                    return Some(h);
                }
            }
            None
        }
        for child in self.children_mut() {
            if let Some(h) = walk(child) {
                return Some(h);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;

    /// The demo (sample-seeded) firewall synced into a fresh document, plus the
    /// app that owns it — the real first-frame state the live app reaches.
    fn seeded() -> (FirewallApp, AutoCommit) {
        let mut app = FirewallApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// Write a boolean key the way a `Toggle` would when the user flips it.
    fn flip(doc: &mut AutoCommit, key: &str, v: bool) {
        doc.put(&ROOT, key, ScalarValue::Boolean(v)).unwrap();
    }

    #[test]
    fn master_switch_reconciles_to_set_enabled() {
        let (mut app, mut doc) = seeded();
        assert!(app.cfg().enabled);
        flip(&mut doc, K_ENABLED, false); // user turns protection off
        app.reconcile_controls(&mut doc, &ROOT);
        assert!(!app.cfg().enabled, "the switch drove the model off");
        assert!(app.last_dispatched().is_some(), "it dispatched a real action");
        assert!(!FirewallDoc::load(&doc).unwrap().enabled, "and the shared document agrees");
    }

    #[test]
    fn service_switch_reconciles_to_set_service() {
        let (mut app, mut doc) = seeded();
        let z = app.active_zone(); // public in the sample
        assert!(app.zone_ref(z).unwrap().has_service("ssh"), "ssh is on in the sample");
        flip(&mut doc, &k_service(z, "ssh"), false); // block SSH
        app.reconcile_controls(&mut doc, &ROOT);
        assert!(!app.zone_ref(z).unwrap().has_service("ssh"));
        assert!(!FirewallDoc::load(&doc).unwrap().zone(z).unwrap().has_service("ssh"));
    }

    #[test]
    fn rule_enable_switch_reconciles_to_edit_rule() {
        let (mut app, mut doc) = seeded();
        app.advanced_open = true;
        app.needs_rebuild = true;
        app.sync(&mut doc, &ROOT); // rebuild so the rule controls + keys exist
        let z = app.viewed_zone();
        let id = app.zone_ref(z).unwrap().rules[0].id;
        assert!(app.zone_ref(z).unwrap().rules[0].enabled);
        flip(&mut doc, &k_rule(z, id), false); // disable the rule
        app.reconcile_controls(&mut doc, &ROOT);
        let r = app.zone_ref(z).unwrap().rules.iter().find(|r| r.id == id).unwrap();
        assert!(!r.enabled, "the rule's enable switch drove EditRule");
    }

    #[test]
    fn choosing_a_profile_makes_it_the_active_zone() {
        let (mut app, mut doc) = seeded();
        assert_ne!(app.active_zone(), ZoneKind::Home);
        app.intents().borrow_mut().push(Intent::UseProfile(ZoneKind::Home));
        app.drain_intents(&mut doc);
        assert_eq!(app.active_zone(), ZoneKind::Home);
        assert_eq!(
            FirewallDoc::load(&doc).unwrap().default_zone,
            ZoneKind::Home,
            "the profile switch dispatched SetDefaultZone into the document"
        );
    }

    #[test]
    fn advanced_discloses_raw_rules_and_policy() {
        let (mut app, _doc) = seeded();
        // The friendly default surfaces no raw rule / policy controls.
        assert!(!app.bindings().iter().any(|b| matches!(b, Binding::RuleEnabled { .. })));
        assert!(!app.bindings().iter().any(|b| matches!(b, Binding::ZoneTarget { .. })));
        // Opening Advanced discloses them without removing anything.
        app.advanced_open = true;
        let bs = app.bindings();
        assert!(bs.iter().any(|b| matches!(b, Binding::RuleEnabled { .. })), "raw rules disclosed");
        assert!(bs.iter().any(|b| matches!(b, Binding::ZoneTarget { .. })), "default policy disclosed");
    }

    #[test]
    fn delete_button_intent_removes_the_rule() {
        let (mut app, mut doc) = seeded();
        let z = app.viewed_zone();
        let id = app.zone_ref(z).unwrap().rules[0].id;
        let before = app.zone_ref(z).unwrap().rules.len();
        app.intents().borrow_mut().push(Intent::DeleteRule(id));
        app.drain_intents(&mut doc);
        assert_eq!(app.zone_ref(z).unwrap().rules.len(), before - 1);
        assert!(!app.zone_ref(z).unwrap().rules.iter().any(|r| r.id == id));
    }

    #[test]
    fn add_rule_form_appends_a_port_rule() {
        let (mut app, mut doc) = seeded();
        let z = app.viewed_zone();
        let before = app.zone_ref(z).unwrap().rules.len();
        doc.put(&ROOT, K_FORM_PORT, "9443").unwrap(); // as the port field mirrors
        app.form_action = Action::Allow;
        app.form_proto = Proto::Tcp;
        app.intents().borrow_mut().push(Intent::AddRule);
        app.drain_intents(&mut doc);
        assert_eq!(app.zone_ref(z).unwrap().rules.len(), before + 1);
        let last = app.zone_ref(z).unwrap().rules.last().unwrap();
        assert_eq!(last.matcher, Matcher::Port { port: "9443".into(), proto: Proto::Tcp });
    }

    #[test]
    fn composed_tree_exposes_keyboard_focusable_controls() {
        // Every control is a shared widget, so the toolkit's Tab/arrow/Enter
        // model reaches it — provided it joins the focus order (`is_focusable`).
        let (app, _doc) = seeded();
        fn count(w: &dyn Widget) -> usize {
            let mut n = usize::from(w.is_focusable());
            for c in w.children() {
                n += count(c);
            }
            n
        }
        let total: usize = app.children().iter().map(|c| count(*c)).sum();
        assert!(total >= 5, "the friendly view exposes focusable controls, got {total}");
    }

    #[test]
    fn unavailable_firewall_has_no_interactive_controls() {
        let mut app = FirewallApp::from_parts(FirewallConfig::empty(), false);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        assert!(!app.is_available());
        assert!(app.bindings().is_empty(), "nothing to reconcile when firewalld is unavailable");
        assert!(
            FirewallDoc::load(&doc).is_none(),
            "the unavailable state seeds no fabricated ruleset into the document"
        );
    }
}
