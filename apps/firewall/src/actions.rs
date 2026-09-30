//! The firewall's exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! Every document-mutating operation the firewall performs on its shared ruleset
//! is a [`FirewallAction`]: a small serializable value that deterministically
//! mutates the CRDT at its stable native address. The exact same [`EncodedAction`] is what the on-screen
//! affordances (the add-rule form, a rule's toggle/trash, the "set active zone"
//! button) dispatch and what the Ctrl+K palette dispatches — one source of
//! truth, one replay path.
//!
//! The resource tree mirrors the data model: the configuration owns its zones,
//! and each zone owns its rules. Every resource exports the [`ActionSpec`]s
//! Ctrl+K offers for it.
//!
//! ## What is *not* an action
//!
//! * **The live apply** — pushing the ruleset to the kernel via
//!   `firewall-cmd`/`nft` (see [`crate::backend`]) is an external side effect
//!   *driven by* the document state, never part of `Action::apply`, which only
//!   mutates the CRDT deterministically. It stays a separate step.
//! * **Presentation** — which zone tab is selected in the UI, the rule filter,
//!   the add-rule form's field focus, scroll — never leaves the machine and
//!   never becomes an action.
//!
//! ## Determinism contract
//!
//! A rule's id is minted at *authoring* time and carried in the [`Rule`] payload;
//! `Action::apply` never touches the RNG or the wall-clock. It reads the
//! current config from the document and transforms it, so two replicas at the
//! same heads land on the byte-identical result. A payload that names a zone or
//! rule that isn't there is a **no-op** rather than an error, so a bad replay
//! can't wedge a peer.

use automerge::AutoCommit;

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, ConfirmationPolicy,
    DiscoveryScope, Doc, EncodedAction, ExecutionLocus, OperationKind, ParamType,
    PermissionRequirement, Resource, ResultSpec, RetryPolicy, RiskLevel,
};

use crate::doc::FirewallDoc;
use crate::model::{
    Action, FirewallConfig, Matcher, Proto, Rule, RuleId, ServiceCatalog, Target, Zone, ZoneKind,
};

/// Registry `type_id` for every firewall action.
pub const FIREWALL_ACTION_TYPE: &str = "firewall.action";
pub const FIREWALL_APPLY_TYPE: &str = "firewall.apply";

/// Every collaborative operation the firewall exports on its shared ruleset.
///
/// Each variant changes only its named field or record. Rule ids are minted at
/// authoring time, and deletion writes a tombstone so stale edits cannot revive
/// a rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirewallAction {
    /// Append a rule to a zone. The rule (including its id) is minted at
    /// authoring time and carried whole; a re-add of the same id is a no-op so
    /// a double-applied envelope can't duplicate it.
    AddRule { zone: ZoneKind, rule: Rule },
    /// Update the rule source only. The app's editable rule field is carried as
    /// a narrow patch, not a stale replacement of matcher/action/enabled.
    EditRule {
        zone: ZoneKind,
        id: RuleId,
        source: String,
    },
    /// Remove a rule from a zone by id.
    DeleteRule { zone: ZoneKind, id: RuleId },
    /// Set, rather than toggle, a rule's enabled state. The target is resolved
    /// by the affordance before dispatch, so remote replay never read-negates.
    SetRuleEnabled {
        zone: ZoneKind,
        id: RuleId,
        on: bool,
    },
    /// Move a rule within its zone by `delta` positions (clamped to the ends).
    /// A move that would leave it in place is a no-op.
    ReorderRule {
        zone: ZoneKind,
        id: RuleId,
        delta: i32,
    },
    /// Set a zone's default policy — the target applied to traffic no rule
    /// matched.
    SetZoneTarget { zone: ZoneKind, target: Target },
    /// Make `zone` the configuration's default (active) zone.
    SetDefaultZone { zone: ZoneKind },
    /// Enable (`on`) or disable a named service in a zone. The target state is
    /// resolved at authoring time and carried, so replay is order-free.
    SetService {
        zone: ZoneKind,
        service: String,
        on: bool,
    },
    /// Set firewalld panic mode (drop all traffic in + out).
    SetPanic { on: bool },
    /// Set the master switch — whether the firewall runs at all.
    SetEnabled { on: bool },
    /// Publish a configuration read from the host, or promote a legacy document
    /// to native maps. Rule edits stay field actions; this is only that import.
    PublishImported { config: FirewallConfig },
}

impl tilo_actions::Action for FirewallAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Field edits address one scalar, map, or text object. `PublishImported`
        // is the one whole-config write: the host import, or a legacy document
        // promoted to native maps.
        match self {
            FirewallAction::AddRule { zone, rule } => FirewallDoc::add_rule(doc, *zone, rule),
            FirewallAction::EditRule { zone, id, source } => {
                FirewallDoc::set_rule_source(doc, *zone, *id, source);
            }
            FirewallAction::DeleteRule { zone, id } => FirewallDoc::delete_rule(doc, *zone, *id),
            FirewallAction::SetRuleEnabled { zone, id, on } => {
                FirewallDoc::set_rule_enabled(doc, *zone, *id, *on);
            }
            FirewallAction::ReorderRule { zone, id, delta } => {
                FirewallDoc::reorder_rule(doc, *zone, *id, *delta);
            }
            FirewallAction::SetZoneTarget { zone, target } => {
                FirewallDoc::set_zone_target(doc, *zone, *target);
            }
            FirewallAction::SetDefaultZone { zone } => FirewallDoc::set_default_zone(doc, *zone),
            FirewallAction::SetService { zone, service, on } => {
                FirewallDoc::set_service(doc, *zone, service, *on);
            }
            FirewallAction::SetPanic { on } => FirewallDoc::set_panic(doc, *on),
            FirewallAction::SetEnabled { on } => FirewallDoc::set_enabled(doc, *on),
            FirewallAction::PublishImported { config } => {
                let _ = FirewallDoc::store(doc, config);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register the firewall's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<FirewallAction>(FIREWALL_ACTION_TYPE);
}

/// Encode any [`FirewallAction`] into the wire form the palette and the
/// on-screen affordances both dispatch. The single point that turns a firewall
/// action into a replayable envelope.
///
/// # Panics
///
/// Never in practice: `FirewallAction` is a plain enum of ids, enums and
/// strings, which `serde_json` cannot fail to serialize.
#[allow(
    clippy::expect_used,
    reason = "FirewallAction always serializes; there is no meaningful fallback EncodedAction to return instead"
)]
pub fn encode(action: &FirewallAction) -> EncodedAction {
    EncodedAction::encode(FIREWALL_ACTION_TYPE, action)
        .expect("FirewallAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

pub fn add_rule_action(zone: ZoneKind, rule: Rule) -> FirewallAction {
    FirewallAction::AddRule { zone, rule }
}

pub fn edit_rule_action(zone: ZoneKind, rule: Rule) -> FirewallAction {
    FirewallAction::EditRule {
        zone,
        id: rule.id,
        source: rule.source,
    }
}

pub fn delete_rule_action(zone: ZoneKind, id: RuleId) -> FirewallAction {
    FirewallAction::DeleteRule { zone, id }
}

pub fn set_rule_enabled_action(zone: ZoneKind, id: RuleId, on: bool) -> FirewallAction {
    FirewallAction::SetRuleEnabled { zone, id, on }
}

pub fn reorder_rule_action(zone: ZoneKind, id: RuleId, delta: i32) -> FirewallAction {
    FirewallAction::ReorderRule { zone, id, delta }
}

pub fn set_zone_target_action(zone: ZoneKind, target: Target) -> FirewallAction {
    FirewallAction::SetZoneTarget { zone, target }
}

pub fn set_default_zone_action(zone: ZoneKind) -> FirewallAction {
    FirewallAction::SetDefaultZone { zone }
}

pub fn set_service_action(zone: ZoneKind, service: impl Into<String>, on: bool) -> FirewallAction {
    FirewallAction::SetService {
        zone,
        service: service.into(),
        on,
    }
}

pub fn set_panic_action(on: bool) -> FirewallAction {
    FirewallAction::SetPanic { on }
}

pub fn set_enabled_action(on: bool) -> FirewallAction {
    FirewallAction::SetEnabled { on }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// Parse a policy name (as typed into the palette) back to a [`Target`].
/// Anything unrecognised falls back to the firewalld default, so a bad input
/// never fabricates a surprising policy.
fn parse_target(name: &str) -> Target {
    match name.trim().to_ascii_lowercase().as_str() {
        "accept" => Target::Accept,
        "reject" => Target::Reject,
        "drop" => Target::Drop,
        _ => Target::Default,
    }
}

/// Normalise a typed port field: an empty entry becomes `"0"`, matching the
/// add-rule form's own [`FormState::matcher`](crate::app) handling.
fn normalize_port(p: &str) -> String {
    let t = p.trim();
    if t.is_empty() {
        "0".to_string()
    } else {
        t.to_string()
    }
}

/// A one-line label for a zone in the palette.
fn zone_label(z: &Zone, is_default: bool) -> String {
    if is_default {
        format!("{} — {} · active", z.kind.name(), z.target.label())
    } else {
        format!("{} — {}", z.kind.name(), z.target.label())
    }
}

/// A one-line label for a rule, marking a disabled one.
fn rule_label(r: &Rule) -> String {
    let base = format!(
        "{} {} from {}",
        r.action.label(),
        r.matcher.label(),
        r.source_label()
    );
    if r.enabled {
        base
    } else {
        format!("{base} (disabled)")
    }
}

/// Push the stored configuration through `firewall-cmd` / the host apply path.
/// Published from [`crate::resource::Firewall::local_operations`] so hosted
/// surfaces keep the palette row without a second config root.
#[allow(
    clippy::expect_used,
    reason = "the empty string operation payload is always serializable"
)]
pub fn apply_effect_spec() -> ActionSpec {
    ActionSpec::operation(
        "apply-config",
        "Apply firewall configuration",
        OperationKind::Effect,
        FIREWALL_APPLY_TYPE,
        &"",
    )
    .expect("firewall apply payload is serializable")
    .with_contract(ActionContract {
        permission: PermissionRequirement::Admin,
        risk: RiskLevel::Privileged,
        confirmation: ConfirmationPolicy::Confirm,
        retry: RetryPolicy::Idempotent,
        discovery: DiscoveryScope::Launchable,
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Job,
        tags: vec!["security".into(), "firewall".into()],
        ..ActionContract::default()
    })
}

#[allow(
    clippy::expect_used,
    reason = "FirewallAction always serializes; there is no meaningful fallback ActionSpec to return instead"
)]
fn immediate_spec(id: &str, label: &str, action: &FirewallAction) -> ActionSpec {
    ActionSpec::immediate(id, label, FIREWALL_ACTION_TYPE, action)
        .expect("encode firewall action")
        .with_contract(config_contract())
}

fn config_contract() -> ActionContract {
    ActionContract {
        permission: PermissionRequirement::Admin,
        risk: RiskLevel::Privileged,
        confirmation: ConfirmationPolicy::Confirm,
        retry: RetryPolicy::Idempotent,
        reversible: true,
        tags: vec!["security".into(), "firewall".into()],
        ..ActionContract::default()
    }
}

/// Build the resource tree the firewall currently has open: the configuration
/// (start/stop + panic), each zone (make-active, set-policy, add-rule), and each
/// rule (toggle, edit, delete, move). Every document-mutating operation is
/// exported as the same action the on-screen affordance dispatches.
#[allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "the only expects encode fixed primitive payloads; ActionSpec cannot fail for them"
)]
pub fn provide_resources(config: &FirewallConfig) -> Vec<Resource> {
    let mut root = Resource::new("firewall.config", "firewall.config", "Firewall");

    // Config-level: master switch + panic mode.
    let enable_label = if config.enabled {
        "Stop firewall"
    } else {
        "Start firewall"
    };
    root = root.with_action(
        ActionSpec::operation(
            "toggle-enabled",
            enable_label,
            OperationKind::Effect,
            "firewall.set-enabled",
            &(!config.enabled),
        )
        .expect("firewall enabled state is serializable")
        .with_contract(ActionContract {
            permission: PermissionRequirement::Admin,
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            retry: RetryPolicy::Idempotent,
            discovery: DiscoveryScope::Launchable,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            tags: vec!["security".into(), "firewall".into()],
            ..ActionContract::default()
        }),
    );
    let panic_label = if config.panic_mode {
        "Exit panic mode"
    } else {
        "Enter panic mode"
    };
    root = root.with_action(
        ActionSpec::operation(
            "toggle-panic",
            panic_label,
            OperationKind::Effect,
            "firewall.set-panic",
            &(!config.panic_mode),
        )
        .expect("firewall panic state is serializable")
        .with_contract(ActionContract {
            confirmation: ConfirmationPolicy::Confirm,
            permission: PermissionRequirement::Admin,
            risk: RiskLevel::Privileged,
            retry: RetryPolicy::Idempotent,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            tags: vec!["security".into(), "firewall".into()],
            ..ActionContract::default()
        }),
    );

    for z in &config.zones {
        root = root.with_child(zone_resource(config, z));
    }

    vec![root]
}

/// Build one zone's resource: make-active/set-policy/add-rule actions plus a
/// child per rule. Split out of [`provide_resources`] purely to keep that
/// function short — same tree, same actions.
fn zone_resource(config: &FirewallConfig, z: &Zone) -> Resource {
    let kind = z.kind;
    let is_default = config.default_zone == kind;
    let mut zr = Resource::new(
        format!("firewall.zone:{}", kind.name()),
        "firewall.zone",
        zone_label(z, is_default),
    );

    // Make this the active (default) zone — only when it isn't already.
    if !is_default {
        zr = zr.with_action(immediate_spec(
            "set-default",
            "Make active zone",
            &set_default_zone_action(kind),
        ));
    }
    // Set the zone's default policy (target) — prompted by name.
    zr = zr.with_action(
        ActionSpec::typed(
            "set-policy",
            "Set default policy",
            "default, accept, reject, drop",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("policy", ParamType::Text, "Default zone policy")
                    .with_choices(["default", "accept", "reject", "drop"]),
            ],
            move |args| {
                encode(&set_zone_target_action(
                    kind,
                    parse_target(args.text("policy").unwrap_or("default")),
                ))
            },
        )
        .with_contract(config_contract()),
    );
    // Add an allow rule for a port — prompted. The rule's uuid is minted once
    // when the argument is committed, then baked into the envelope, so replay
    // stays deterministic.
    zr = zr.with_action(
        ActionSpec::typed(
            "add-allow",
            "Add allow rule",
            "e.g. 8080 or 8000-8080",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("port", ParamType::Text, "TCP port or inclusive port range")
                    .with_schema(serde_json::json!({ "pattern": "^[0-9]{1,5}(-[0-9]{1,5})?$" })),
            ],
            move |args| {
                let port = args.text("port").unwrap_or_default();
                encode(&add_rule_action(
                    kind,
                    Rule::new(
                        Matcher::Port {
                            port: normalize_port(port),
                            proto: Proto::Tcp,
                        },
                        Action::Allow,
                        "",
                    ),
                ))
            },
        )
        .with_contract(config_contract()),
    );

    for service in ServiceCatalog::ALL {
        let on = !z.has_service(service.id);
        zr = zr.with_action(immediate_spec(
            &format!("service-{}", service.id),
            &format!("{} {}", if on { "Enable" } else { "Disable" }, service.name),
            &set_service_action(kind, service.id, on),
        ));
    }

    let n = z.rules.len();
    for (idx, r) in z.rules.iter().enumerate() {
        zr = zr.with_child(rule_resource(kind, idx, n, r));
    }
    // Services are first-class resources too, rather than controls hidden on
    // their parent zone. This gives search, history and collaboration a stable
    // address for each membership change.
    for service in ServiceCatalog::ALL {
        let on = z.has_service(service.id);
        zr = zr.with_child(
            Resource::new(
                format!("firewall.service:{}:{}", kind.name(), service.id),
                "firewall.service",
                format!(
                    "{} — {}",
                    service.name,
                    if on { "enabled" } else { "disabled" }
                ),
            )
            .with_action(immediate_spec(
                "set-enabled",
                if on {
                    "Disable service"
                } else {
                    "Enable service"
                },
                &set_service_action(kind, service.id, !on),
            )),
        );
    }
    zr
}

/// Build one rule's resource: toggle/delete/edit actions plus move-up/-down
/// where there is room. Split out of [`zone_resource`] for the same reason.
fn rule_resource(kind: ZoneKind, idx: usize, n: usize, r: &Rule) -> Resource {
    let id = r.id;
    let toggle_label = if r.enabled {
        "Disable rule"
    } else {
        "Enable rule"
    };
    let mut rr = Resource::new(
        format!("firewall.rule:{id}"),
        "firewall.rule",
        rule_label(r),
    )
    .with_action(immediate_spec(
        "toggle",
        toggle_label,
        &set_rule_enabled_action(kind, id, !r.enabled),
    ))
    .with_action(
        immediate_spec("delete", "Delete rule", &delete_rule_action(kind, id)).with_contract(
            ActionContract {
                confirmation: ConfirmationPolicy::Confirm,
                risk: RiskLevel::Destructive,
                ..config_contract()
            },
        ),
    );

    // Edit — replace the rule's source (its most editable field). Carries
    // the rest of the rule unchanged so the edit is a whole-rule replace.
    let base = r.clone();
    rr = rr.with_action(
        ActionSpec::typed(
            "edit",
            "Set source",
            "CIDR, or blank for anywhere",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::optional(
                "source",
                ParamType::Text,
                "CIDR/address; omitted means anywhere",
            )],
            move |args| {
                let mut edited = base.clone();
                edited.source = args.text("source").unwrap_or_default().trim().to_string();
                encode(&edit_rule_action(kind, edited))
            },
        )
        .with_contract(config_contract()),
    );

    // Reorder — offered only where there is room to move.
    if idx > 0 {
        rr = rr.with_action(immediate_spec(
            "move-up",
            "Move up",
            &reorder_rule_action(kind, id, -1),
        ));
    }
    if idx + 1 < n {
        rr = rr.with_action(immediate_spec(
            "move-down",
            "Move down",
            &reorder_rule_action(kind, id, 1),
        ));
    }

    rr
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::FirewallApp {
    pub fn publish_resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        // Level-triggered: recomputed each frame from the app's current config,
        // which `sync` and every action keep reconciled with the shared
        // document. Empty when the firewall is unavailable / has no zones —
        // nothing to act on.
        match self.firewall_resource_ctx() {
            Some(config) => provide_resources(&config),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::FirewallApp;
    use automerge::ROOT;
    use tilo_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    /// The demo (sample-seeded) firewall synced into a fresh document, plus the
    /// app that owns it. The document is the source of truth every action reads.
    fn seeded() -> (FirewallApp, AutoCommit) {
        let mut app = FirewallApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// The palette publishes the firewall tree (or only recovery when unavailable), ignores out-of-range targets and creates rules through the add-allow action.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn firewall_action_surface_publishes_and_dispatches() {
        // --- phase: ctrl_k_publishes_the_firewall_tree ---
        // Ctrl+K on the firewall yields the tree: the configuration (start/stop +
        // panic), a `firewall.zone` child per zone exporting make-active/set-policy/
        // add-rule, and a `firewall.rule` child per rule exporting toggle/edit/
        // delete. Drives the real resource hook.
        {
            let (app, doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
            assert_eq!(resources.len(), 1, "one configuration root");
            let cfg = &resources[0];
            assert_eq!(cfg.kind, "firewall.config");
            let cfg_actions: Vec<&str> = cfg.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(cfg_actions.contains(&"toggle-enabled"), "{cfg_actions:?}");
            assert!(cfg_actions.contains(&"toggle-panic"), "{cfg_actions:?}");
            let enabled = cfg
                .actions
                .iter()
                .find(|action| action.id == "toggle-enabled")
                .expect("start/stop operation");
            assert_eq!(enabled.kind, OperationKind::Effect);
            assert_eq!(enabled.contract.permission, PermissionRequirement::Admin);
            assert_eq!(enabled.contract.confirmation, ConfirmationPolicy::Confirm);
            assert_eq!(enabled.contract.discovery, DiscoveryScope::Launchable);
            assert_eq!(cfg.children.len(), 9, "all nine stock zones are published");

            // The public zone carries rules in the sample; find it and check both
            // the zone-level and rule-level action sets.
            let public = cfg
                .children
                .iter()
                .find(|r| r.id.as_str() == "firewall.zone:public")
                .expect("a public zone resource");
            assert_eq!(public.kind, "firewall.zone");
            let z_actions: Vec<&str> = public.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(z_actions.contains(&"set-policy"), "{z_actions:?}");
            assert!(z_actions.contains(&"add-allow"), "{z_actions:?}");
            assert!(z_actions.contains(&"service-ssh"), "{z_actions:?}");
            assert!(
                public
                    .actions
                    .iter()
                    .find(|action| action.id == "set-policy")
                    .is_some_and(|action| !action.params().is_empty())
            );
            // public is the active zone in the sample, so make-active is suppressed.
            assert!(
                !z_actions.contains(&"set-default"),
                "active zone omits make-active"
            );
            assert!(!public.children.is_empty(), "public has rules");

            let rule = &public.children[0];
            assert_eq!(rule.kind, "firewall.rule");
            let r_actions: Vec<&str> = rule.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(r_actions.contains(&"toggle"), "{r_actions:?}");
            assert!(r_actions.contains(&"edit"), "{r_actions:?}");
            assert!(r_actions.contains(&"delete"), "{r_actions:?}");
            assert_eq!(
                rule.actions
                    .iter()
                    .find(|action| action.id == "delete")
                    .expect("delete action")
                    .contract
                    .risk,
                RiskLevel::Destructive
            );
            // First rule can't move up; it can move down.
            assert!(!r_actions.contains(&"move-up"), "{r_actions:?}");
            assert!(r_actions.contains(&"move-down"), "{r_actions:?}");

            let ssh = public
                .children
                .iter()
                .find(|resource| resource.id.as_str() == "firewall.service:public:ssh")
                .expect("a service resource with a stable address");
            assert_eq!(ssh.kind, "firewall.service");
            assert!(ssh.actions.iter().any(|action| action.id == "set-enabled"));

            // A non-active zone offers make-active.
            let home = cfg
                .children
                .iter()
                .find(|r| r.id.as_str() == "firewall.zone:home")
                .expect("a home zone resource");
            let home_actions: Vec<&str> = home.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(home_actions.contains(&"set-default"), "{home_actions:?}");

            // The flattened tree exports every action, including the deeper rule
            // operations that sort after the zones.
            let flat: Vec<&str> = tilo_actions::flatten(&resources)
                .iter()
                .map(|f| f.action.label.as_str())
                .collect();
            assert!(flat.contains(&"Delete rule"), "{flat:?}");
            assert!(flat.contains(&"Make active zone"), "{flat:?}");
            assert!(flat.contains(&"Set default policy"), "{flat:?}");
        }

        // --- phase: unavailable_firewall_publishes_only_recovery ---
        // The unavailable firewall publishes only its read-only recovery action;
        // it never exposes rule mutations against an unknown host state.
        {
            let app = FirewallApp::new(); // no firewalld on the test host → unavailable
            let doc = AutoCommit::new();
            let resources = ui::resources::resources(&app, &doc);
            assert_eq!(resources.len(), 1);
            assert_eq!(resources[0].kind, "firewall.recovery");
            assert_eq!(resources[0].actions.len(), 1);
            assert_eq!(resources[0].actions[0].id, "retry");
        }

        // --- phase: out_of_range_action_is_a_no_op ---
        // A malformed / out-of-range payload is a no-op, not an error: deleting a
        // rule id that isn't there leaves the document (its heads) untouched.
        {
            let (_app, mut doc) = seeded();
            let zone = FirewallDoc::load(&doc).unwrap().default_zone;
            let before = doc.get_heads();
            let action = delete_rule_action(zone, RuleId::from_u128(0xDEAD_BEEF));
            let encoded = encode(&action);
            let decoded = registry().decode(&encoded).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
            assert_eq!(
                doc.get_heads(),
                before,
                "a delete of an absent rule changed nothing"
            );
        }

        // --- phase: add_allow_action_creates_a_rule ---
        // Dispatching the zone-level "Add allow rule" prompted action creates a
        // rule in the document with the committed port.
        {
            let (app, mut doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
            let public = resources[0]
                .children
                .iter()
                .find(|r| r.id.as_str() == "firewall.zone:public")
                .unwrap();
            let spec = public.actions.iter().find(|a| a.id == "add-allow").unwrap();
            let before = FirewallDoc::load(&doc)
                .unwrap()
                .zone(ZoneKind::Public)
                .unwrap()
                .rules
                .len();

            let encoded = prompted_encode(spec, "8443");
            let decoded = registry().decode(&encoded).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

            let after = FirewallDoc::load(&doc).unwrap();
            let rules = &after.zone(ZoneKind::Public).unwrap().rules;
            assert_eq!(rules.len(), before + 1, "the action added a rule");
            assert!(
                rules.iter().any(|r| r.matcher
                    == Matcher::Port {
                        port: "8443".into(),
                        proto: Proto::Tcp
                    }
                    && r.action == Action::Allow),
                "the created rule carries the committed port"
            );
        }
    }

    /// The configuration a peer holding `doc` publishes: the provider reads
    /// the shared document, the source of truth every action reads.
    fn config(doc: &AutoCommit) -> FirewallConfig {
        FirewallDoc::load(doc).expect("the seeded firewall configuration")
    }

    /// The first rule of the active zone.
    fn first_rule(doc: &AutoCommit) -> (ZoneKind, Rule) {
        let cfg = config(doc);
        let zone = cfg.default_zone;
        let rule = cfg.zone(zone).expect("active zone").rules[0].clone();
        (zone, rule)
    }

    /// The stable rule id a rule resource's address ends in.
    fn rule_id(res: &Resource) -> RuleId {
        res.id
            .as_str()
            .trim_start_matches("firewall.rule:")
            .parse()
            .expect("rule resource id")
    }

    ui::action_contract_tests! {
        seed: seeded().1,
        registry: registry(),
        resources: |doc| {
            let config = config(doc);
            vec![ui::tilo_resource::publish::<crate::resource::Firewall>(
                "firewall.config",
                "Firewall",
                &config,
            )]
        },
        palette: ["Delete rule", "Make active zone", "Set default policy"],
        replay: [
            toggle_rule => |doc| {
                let (zone, rule) = first_rule(doc);
                encode(&set_rule_enabled_action(zone, rule.id, !rule.enabled))
            },
            add_rule => |doc| encode(&add_rule_action(
                config(doc).default_zone,
                Rule::new(
                    Matcher::Port {
                        port: "9443".into(),
                        proto: Proto::Tcp,
                    },
                    Action::Allow,
                    "10.0.0.0/8",
                ),
            )),
        ],
        undo: [
            delete_rule => |doc| {
                let (zone, rule) = first_rule(doc);
                encode(&delete_rule_action(zone, rule.id))
            },
        ],
        encodes: [
            delete_rule => ("firewall.rule", "delete", |_, res| {
                encode(&delete_rule_action(ZoneKind::Public, rule_id(res)))
            }),
            edit_source => ("firewall.rule", "edit", arg: "172.16.0.0/12", |doc, res| {
                let mut edited = config(doc)
                    .zone(ZoneKind::Public)
                    .expect("public zone")
                    .rules
                    .iter()
                    .find(|r| r.id == rule_id(res))
                    .expect("the published rule")
                    .clone();
                edited.source = "172.16.0.0/12".to_string();
                encode(&edit_rule_action(ZoneKind::Public, edited))
            }),
        ],
    }
}
