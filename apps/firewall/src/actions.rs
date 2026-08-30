//! The firewall's exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! Every document-mutating operation the firewall performs on its shared ruleset
//! is a [`FirewallAction`]: a small serializable value that deterministically
//! mutates the CRDT. The whole [`FirewallConfig`] lives as one JSON blob in the
//! shared document (see [`crate::doc`]), so each action loads that config,
//! applies a pure transform, and stores the diff back — replicating and
//! replaying for free. The exact same [`EncodedAction`] is what the on-screen
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
//!   *driven by* the document state, never part of [`Action::apply`], which only
//!   mutates the CRDT deterministically. It stays a separate step.
//! * **Presentation** — which zone tab is selected in the UI, the rule filter,
//!   the add-rule form's field focus, scroll — never leaves the machine and
//!   never becomes an action.
//!
//! ## Determinism contract
//!
//! A rule's id is minted at *authoring* time and carried in the [`Rule`] payload;
//! [`Action::apply`] never touches the RNG or the wall-clock. It reads the
//! current config from the document and transforms it, so two replicas at the
//! same heads land on the byte-identical result. A payload that names a zone or
//! rule that isn't there is a **no-op** rather than an error, so a bad replay
//! can't wedge a peer.

use automerge::AutoCommit;

use qos_actions::{
    ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};
use serde::{Deserialize, Serialize};

use crate::doc::FirewallDoc;
use crate::model::{Action, FirewallConfig, Matcher, Proto, Rule, RuleId, Target, Zone, ZoneKind};

/// Registry `type_id` for every firewall action.
pub const FIREWALL_ACTION_TYPE: &str = "firewall.action";

/// Every collaborative operation the firewall exports on its shared ruleset.
///
/// Each variant is a deterministic transform of the whole [`FirewallConfig`]:
/// the executor loads the current config from the document, this transform is
/// applied, and the diff is written back (see [`Action::apply`]). The rule CRUD
/// carries a pre-minted [`Rule`] (id and all) so replay reproduces it exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirewallAction {
    /// Append a rule to a zone. The rule (including its id) is minted at
    /// authoring time and carried whole; a re-add of the same id is a no-op so
    /// a double-applied envelope can't duplicate it.
    AddRule { zone: ZoneKind, rule: Rule },
    /// Replace a rule in place (matched by id), preserving its position. Used to
    /// edit a rule's source / action / matcher.
    EditRule { zone: ZoneKind, rule: Rule },
    /// Remove a rule from a zone by id.
    DeleteRule { zone: ZoneKind, id: RuleId },
    /// Flip a rule's `enabled` flag. Deterministic against the replay base: the
    /// current value is read from the document and negated.
    ToggleRule { zone: ZoneKind, id: RuleId },
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
}

impl qos_actions::Action for FirewallAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // The whole configuration lives as one JSON blob; every action loads it,
        // applies a pure transform, and stores the diff back. Without a stored
        // config there is nothing to mutate — a no-op (so a bad replay can't
        // wedge a peer). When the transform changes nothing (an out-of-range id,
        // an already-satisfied toggle) we skip the store entirely, leaving the
        // document untouched.
        let Some(mut cfg) = FirewallDoc::load(doc) else {
            return Ok(());
        };
        if apply_to_config(self, &mut cfg) {
            FirewallDoc::store(doc, &cfg);
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// The pure core of [`Action::apply`]: transform `cfg` in place and report
/// whether anything actually changed. Kept separate so it is trivially
/// unit-testable without a document, and so `apply` can skip the store on a
/// no-op. Every branch that can't find its target zone/rule returns `false`.
fn apply_to_config(action: &FirewallAction, cfg: &mut FirewallConfig) -> bool {
    match action {
        FirewallAction::AddRule { zone, rule } => {
            let Some(z) = cfg.zone_mut(*zone) else {
                return false;
            };
            if z.rules.iter().any(|r| r.id == rule.id) {
                return false;
            }
            z.rules.push(rule.clone());
            true
        }
        FirewallAction::EditRule { zone, rule } => {
            let Some(z) = cfg.zone_mut(*zone) else {
                return false;
            };
            let Some(slot) = z.rules.iter_mut().find(|r| r.id == rule.id) else {
                return false;
            };
            if *slot == *rule {
                return false;
            }
            *slot = rule.clone();
            true
        }
        FirewallAction::DeleteRule { zone, id } => {
            let Some(z) = cfg.zone_mut(*zone) else {
                return false;
            };
            let before = z.rules.len();
            z.rules.retain(|r| r.id != *id);
            z.rules.len() != before
        }
        FirewallAction::ToggleRule { zone, id } => {
            let Some(z) = cfg.zone_mut(*zone) else {
                return false;
            };
            let Some(r) = z.rules.iter_mut().find(|r| r.id == *id) else {
                return false;
            };
            r.enabled = !r.enabled;
            true
        }
        FirewallAction::ReorderRule { zone, id, delta } => {
            let Some(z) = cfg.zone_mut(*zone) else {
                return false;
            };
            let Some(idx) = z.rules.iter().position(|r| r.id == *id) else {
                return false;
            };
            let max = z.rules.len() as i32 - 1;
            let target = (idx as i32 + *delta).clamp(0, max) as usize;
            if target == idx {
                return false;
            }
            let r = z.rules.remove(idx);
            z.rules.insert(target, r);
            true
        }
        FirewallAction::SetZoneTarget { zone, target } => {
            let Some(z) = cfg.zone_mut(*zone) else {
                return false;
            };
            if z.target == *target {
                return false;
            }
            z.target = *target;
            true
        }
        FirewallAction::SetDefaultZone { zone } => {
            if cfg.zone(*zone).is_none() || cfg.default_zone == *zone {
                return false;
            }
            cfg.default_zone = *zone;
            true
        }
        FirewallAction::SetService { zone, service, on } => {
            let Some(z) = cfg.zone_mut(*zone) else {
                return false;
            };
            let has = z.has_service(service);
            if *on && !has {
                z.services.push(service.clone());
                true
            } else if !*on && has {
                z.services.retain(|s| s != service);
                true
            } else {
                false
            }
        }
        FirewallAction::SetPanic { on } => {
            if cfg.panic_mode == *on {
                return false;
            }
            cfg.panic_mode = *on;
            true
        }
        FirewallAction::SetEnabled { on } => {
            if cfg.enabled == *on {
                return false;
            }
            cfg.enabled = *on;
            true
        }
    }
}

/// Register the firewall's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<FirewallAction>(FIREWALL_ACTION_TYPE);
}

/// Encode any [`FirewallAction`] into the wire form the palette and the
/// on-screen affordances both dispatch. The single point that turns a firewall
/// action into a replayable envelope.
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
    FirewallAction::EditRule { zone, rule }
}

pub fn delete_rule_action(zone: ZoneKind, id: RuleId) -> FirewallAction {
    FirewallAction::DeleteRule { zone, id }
}

pub fn toggle_rule_action(zone: ZoneKind, id: RuleId) -> FirewallAction {
    FirewallAction::ToggleRule { zone, id }
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

fn immediate_spec(id: &str, label: &str, action: &FirewallAction) -> ActionSpec {
    ActionSpec::immediate(id, label, FIREWALL_ACTION_TYPE, action).expect("encode firewall action")
}

/// Build the resource tree the firewall currently has open: the configuration
/// (start/stop + panic), each zone (make-active, set-policy, add-rule), and each
/// rule (toggle, edit, delete, move). Every document-mutating operation is
/// exported as the same action the on-screen affordance dispatches.
pub fn provide_resources(config: &FirewallConfig) -> Vec<Resource> {
    let mut root = Resource::new("firewall.config", "firewall.config", "Firewall");

    // Config-level: master switch + panic mode.
    let enable_label = if config.enabled {
        "Stop firewall"
    } else {
        "Start firewall"
    };
    root = root.with_action(immediate_spec(
        "toggle-enabled",
        enable_label,
        &set_enabled_action(!config.enabled),
    ));
    let panic_label = if config.panic_mode {
        "Exit panic mode"
    } else {
        "Enter panic mode"
    };
    root = root.with_action(immediate_spec(
        "toggle-panic",
        panic_label,
        &set_panic_action(!config.panic_mode),
    ));

    for z in &config.zones {
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
        zr = zr.with_action(ActionSpec::prompted(
            "set-policy",
            "Set default policy",
            "Policy",
            "default, accept, reject, drop",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&set_zone_target_action(kind, parse_target(name))),
        ));
        // Add an allow rule for a port — prompted. The rule's uuid is minted once
        // when the argument is committed, then baked into the envelope, so replay
        // stays deterministic.
        zr = zr.with_action(ActionSpec::prompted(
            "add-allow",
            "Add allow rule",
            "Port",
            "e.g. 8080 or 8000-8080",
            ExecutionLocus::ClientLocal,
            move |port: &str| {
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
        ));

        let n = z.rules.len();
        for (idx, r) in z.rules.iter().enumerate() {
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
                &toggle_rule_action(kind, id),
            ))
            .with_action(immediate_spec(
                "delete",
                "Delete rule",
                &delete_rule_action(kind, id),
            ));

            // Edit — replace the rule's source (its most editable field). Carries
            // the rest of the rule unchanged so the edit is a whole-rule replace.
            let base = r.clone();
            rr = rr.with_action(ActionSpec::prompted(
                "edit",
                "Set source",
                "Source",
                "CIDR, or blank for anywhere",
                ExecutionLocus::ClientLocal,
                move |src: &str| {
                    let mut edited = base.clone();
                    edited.source = src.trim().to_string();
                    encode(&edit_rule_action(kind, edited))
                },
            ));

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

            zr = zr.with_child(rr);
        }
        root = root.with_child(zr);
    }

    vec![root]
}

impl ResourceProvider for crate::app::FirewallApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
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
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            _ => panic!("expected a prompted action: {}", spec.id),
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

    /// Ctrl+K on the firewall yields the tree: the configuration (start/stop +
    /// panic), a `firewall.zone` child per zone exporting make-active/set-policy/
    /// add-rule, and a `firewall.rule` child per rule exporting toggle/edit/
    /// delete. Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_firewall_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one configuration root");
        let cfg = &resources[0];
        assert_eq!(cfg.kind, "firewall.config");
        let cfg_actions: Vec<&str> = cfg.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(cfg_actions.contains(&"toggle-enabled"), "{cfg_actions:?}");
        assert!(cfg_actions.contains(&"toggle-panic"), "{cfg_actions:?}");
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
        // First rule can't move up; it can move down.
        assert!(!r_actions.contains(&"move-up"), "{r_actions:?}");
        assert!(r_actions.contains(&"move-down"), "{r_actions:?}");

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
        let flat: Vec<&str> = qos_actions::flatten(&resources)
            .iter()
            .map(|f| f.action.label.as_str())
            .collect();
        assert!(flat.contains(&"Delete rule"), "{flat:?}");
        assert!(flat.contains(&"Make active zone"), "{flat:?}");
        assert!(flat.contains(&"Set default policy"), "{flat:?}");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("firewall")
                || l.contains("Firewall")
                || l.contains("Stop")
                || l.contains("Start")),
            "{labels:?}"
        );
    }

    /// The unavailable firewall publishes nothing to Ctrl+K — there is no real
    /// ruleset to act on.
    #[test]
    fn unavailable_firewall_publishes_no_resources() {
        let app = FirewallApp::new(); // no firewalld on the test host → unavailable
        let doc = AutoCommit::new();
        assert!(Widget::resources(&app, &doc).is_empty());
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        // A real document with the demo firewall in it.
        let mut app = FirewallApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);

        let zone = FirewallDoc::load(&state.doc).unwrap().default_zone;
        let before_cfg = FirewallDoc::load(&state.doc).unwrap();
        let victim = before_cfg.zone(zone).unwrap().rules[0].id;
        let before_heads = state.doc.get_heads();
        let before_len = before_cfg.zone(zone).unwrap().rules.len();

        let action = delete_rule_action(zone, victim);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        let after = FirewallDoc::load(&state.doc).unwrap();
        assert_eq!(
            after.zone(zone).unwrap().rules.len(),
            before_len - 1,
            "the action deleted a rule"
        );
        assert!(
            !after
                .zone(zone)
                .unwrap()
                .rules
                .iter()
                .any(|r| r.id == victim),
            "the right rule is gone"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        let restored = FirewallDoc::load(&state.doc).unwrap();
        assert_eq!(
            restored.zone(zone).unwrap().rules.len(),
            before_len,
            "undo reverted the deletion and nothing else"
        );
        assert!(
            restored
                .zone(zone)
                .unwrap()
                .rules
                .iter()
                .any(|r| r.id == victim),
            "the rule is back after undo"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        // Toggle a rule: deterministic against the shared base (each replica
        // reads the stored value and negates it).
        let cfg = FirewallDoc::load(&a).unwrap();
        let zone = cfg.default_zone;
        let rule_id = cfg.zone(zone).unwrap().rules[0].id;
        let was = cfg.zone(zone).unwrap().rules[0].enabled;
        let action = toggle_rule_action(zone, rule_id);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        let after_a = FirewallDoc::load(&a).unwrap();
        let after_b = FirewallDoc::load(&b).unwrap();
        let enabled_a = after_a
            .zone(zone)
            .unwrap()
            .rules
            .iter()
            .find(|r| r.id == rule_id)
            .unwrap()
            .enabled;
        let enabled_b = after_b
            .zone(zone)
            .unwrap()
            .rules
            .iter()
            .find(|r| r.id == rule_id)
            .unwrap()
            .enabled;
        assert_eq!(enabled_a, !was, "the toggle landed");
        assert_eq!(enabled_a, enabled_b, "both replicas agree");
    }

    /// Replaying an `AddRule` (a create) also converges byte-identically — the
    /// rule's id is carried in the payload, so both replicas write the same row.
    #[test]
    fn add_rule_replays_byte_identical() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let zone = FirewallDoc::load(&a).unwrap().default_zone;
        let rule = Rule::new(
            Matcher::Port {
                port: "9443".into(),
                proto: Proto::Tcp,
            },
            Action::Allow,
            "10.0.0.0/8",
        );
        let rid = rule.id;
        let action = add_rule_action(zone, rule);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "create replayed identically");
        assert!(
            FirewallDoc::load(&b)
                .unwrap()
                .zone(zone)
                .unwrap()
                .rules
                .iter()
                .any(|r| r.id == rid),
            "the added rule is present on the peer"
        );
    }

    /// A malformed / out-of-range payload is a no-op, not an error: deleting a
    /// rule id that isn't there leaves the document (its heads) untouched.
    #[test]
    fn out_of_range_action_is_a_no_op() {
        let (_app, mut doc) = seeded();
        let zone = FirewallDoc::load(&doc).unwrap().default_zone;
        let before = doc.get_heads();
        let action = delete_rule_action(zone, RuleId::from_u128(0xDEAD_BEEF));
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        assert_eq!(
            doc.get_heads(),
            before,
            "a delete of an absent rule changed nothing"
        );
    }

    /// Single source of truth: the palette's per-rule "Delete rule" spec and the
    /// builder the trash button calls encode byte-identically, and the same holds
    /// for the prompted "Set source" edit.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let cfg = &resources[0];
        let public = cfg
            .children
            .iter()
            .find(|r| r.id.as_str() == "firewall.zone:public")
            .unwrap();
        let rule = &public.children[0];
        let id: RuleId = rule
            .id
            .as_str()
            .trim_start_matches("firewall.rule:")
            .parse()
            .unwrap();

        let delete_spec = rule.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        let from_builder = encode(&delete_rule_action(ZoneKind::Public, id));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        // The edit spec matches the edit builder for the same new source.
        let mut edited = FirewallDoc::load(&doc)
            .unwrap()
            .zone(ZoneKind::Public)
            .unwrap()
            .rules
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .clone();
        let edit_spec = rule.actions.iter().find(|a| a.id == "edit").unwrap();
        let from_spec = prompted_encode(edit_spec, "172.16.0.0/12");
        edited.source = "172.16.0.0/12".to_string();
        let from_builder = encode(&edit_rule_action(ZoneKind::Public, edited));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "edit is one source of truth"
        );
    }

    /// Dispatching the zone-level "Add allow rule" prompted action creates a
    /// rule in the document with the committed port.
    #[test]
    fn add_allow_action_creates_a_rule() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
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
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

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
