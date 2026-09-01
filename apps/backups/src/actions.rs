//! The Backups app's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Backups keeps almost all of its state *outside* the shared document: the
//! catalog of sources, snapshots and destinations is projected live from the
//! storage backend, and running/verifying/restoring a snapshot are external
//! side effects, not CRDT mutations. The one thing that must replicate, undo
//! and replay byte-identically is the **retention policy**, so that is the
//! whole action surface: a single [`BackupsAction::SetRetention`] that sets one
//! policy field to an absolute, pre-resolved count. The exact same
//! [`EncodedAction`] the on-screen +/- affordance dispatches is what Ctrl+K
//! dispatches — one source of truth, one replay path, undo for free.
//!
//! Carrying the resolved target (rather than a delta) makes replay order-free:
//! two replicas at the same heads land on the byte-identical result, and a set
//! that would change nothing is a no-op (see the change-guard in
//! [`BackupsAction::apply`]).

use automerge::{AutoCommit, ROOT};

use qos_actions::{
    Action, ActionContract, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, OperationKind, PermissionRequirement, Resource, ResourceProvider, RetryPolicy,
};
use serde::{Deserialize, Serialize};

use crate::doc::{BackupsDoc, PolicyField};
use crate::model::RetentionPolicy;

/// Registry `type_id` for every backups action.
pub const BACKUPS_ACTION_TYPE: &str = "backups.action";
pub const BACKUPS_OPERATION_TYPE: &str = "backups.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupsOperation {
    Restore { snapshot_id: String },
    Delete { snapshot_id: String },
}

pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<BackupsOperation>, String> {
    if type_id != BACKUPS_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Snapshot effects remain discoverable, but are explicitly unavailable until
/// a storage provider supplies real restore/delete primitives.
pub fn snapshot_resource(snapshot_id: &str) -> Resource {
    let restore = ActionSpec::operation(
        "restore",
        "Restore snapshot",
        OperationKind::Effect,
        BACKUPS_OPERATION_TYPE,
        &BackupsOperation::Restore {
            snapshot_id: snapshot_id.to_string(),
        },
    )
    .expect("BackupsOperation is always serializable")
    .with_contract(ActionContract {
        permission: PermissionRequirement::Write,
        retry: RetryPolicy::AtMostOnce,
        tags: vec!["storage".into(), "restore".into()],
        ..ActionContract::default()
    })
    .unavailable(
        "restore_backend_unavailable",
        "No backup storage backend is configured to restore this snapshot",
    );
    let delete = ActionSpec::operation(
        "delete",
        "Delete snapshot",
        OperationKind::Effect,
        BACKUPS_OPERATION_TYPE,
        &BackupsOperation::Delete {
            snapshot_id: snapshot_id.to_string(),
        },
    )
    .expect("BackupsOperation is always serializable")
    .with_contract(ActionContract {
        permission: PermissionRequirement::Write,
        retry: RetryPolicy::AtMostOnce,
        tags: vec!["storage".into(), "delete".into()],
        ..ActionContract::default()
    })
    .destructive()
    .unavailable(
        "delete_backend_unavailable",
        "No backup storage backend is configured to delete this snapshot",
    );
    Resource::new(
        format!("backups.snapshot:{snapshot_id}"),
        "backups.snapshot",
        format!("Snapshot {snapshot_id}"),
    )
    .with_action(restore)
    .with_action(delete)
}

/// Every collaborative operation the Backups app exports on its stored
/// configuration.
///
/// The retention policy is the whole stored configuration, so there is exactly
/// one variant: set a policy field to an absolute count. The target is resolved
/// at authoring time and carried whole, so the executor's write is a targeted,
/// change-guarded field set (see [`Action::apply`]) that replicates + replays for
/// free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupsAction {
    /// Set a retention-policy field to an absolute `count` of snapshots to keep,
    /// clamped to `[0, 999]`. A set to the value already stored is a no-op.
    SetRetention { field: PolicyField, count: u32 },
}

impl qos_actions::Action for BackupsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            BackupsAction::SetRetention { field, count } => {
                let target = i64::from(*count).clamp(0, 999) as u32;
                // Change-guard: read the current value and skip the write when it
                // already matches, so a redundant / reordered replay leaves the
                // document (its heads) untouched and can't wedge a peer.
                if BackupsDoc::retention(doc, &ROOT, *field) != target {
                    BackupsDoc::set_retention(doc, &ROOT, *field, target);
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // Retention policy is personal config: this client applies + replicates
        // and peers replay.
        ExecutionLocus::ClientLocal
    }
}

/// Register the backups action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<BackupsAction>(BACKUPS_ACTION_TYPE);
}

/// The single point that turns a [`BackupsAction`] into the wire envelope both
/// the palette and the on-screen affordances dispatch.
///
/// # Panics
///
/// Never panics: a `BackupsAction` that fails to serialize (it never should —
/// every field is a plain enum or integer) falls back to an empty-payload
/// envelope rather than crashing the caller.
pub fn encode(action: &BackupsAction) -> EncodedAction {
    EncodedAction::encode(BACKUPS_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: BACKUPS_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Set a retention field to an absolute `count` (clamped to `[0, 999]`).
pub fn set_retention_action(field: PolicyField, count: u32) -> BackupsAction {
    BackupsAction::SetRetention {
        field,
        count: count.min(999),
    }
}

/// The single place both the on-screen +/- affordances and the palette's
/// increment/decrement specs resolve a bump into an absolute target, so a
/// button and its Ctrl+K row build the byte-identical action.
pub fn bump_retention_action(field: PolicyField, cur: u32, delta: i64) -> BackupsAction {
    let count = (i64::from(cur) + delta).clamp(0, 999) as u32;
    BackupsAction::SetRetention { field, count }
}

// ===========================================================================
// Resource tree
// ===========================================================================

fn policy_field(p: &RetentionPolicy, field: PolicyField) -> u32 {
    match field {
        PolicyField::Hourly => p.hourly,
        PolicyField::Daily => p.daily,
        PolicyField::Weekly => p.weekly,
        PolicyField::Monthly => p.monthly,
    }
}

/// Parse a typed retention count, clamped to `[0, 999]`. A non-numeric entry
/// becomes `0` rather than fabricating a surprising value.
fn parse_count(text: &str) -> u32 {
    text.trim().parse::<i64>().unwrap_or(0).clamp(0, 999) as u32
}

fn immediate_spec(id: &str, label: &str, action: &BackupsAction) -> ActionSpec {
    ActionSpec::immediate(id, label, BACKUPS_ACTION_TYPE, action)
        // `ActionSpec`'s fields are private now that it carries typed
        // parameters; build the fallback through the constructor that
        // takes an already-encoded action.
        .unwrap_or_else(|_| {
            ActionSpec::immediate_encoded(id, label.to_string(), action.locus(), encode(action))
        })
        .with_contract(policy_contract())
}

fn policy_contract() -> ActionContract {
    ActionContract {
        retry: RetryPolicy::Idempotent,
        reversible: true,
        tags: vec!["configuration".into(), "retention".into()],
        ..ActionContract::default()
    }
}

/// The resource tree Backups publishes: the retention policy, with a child per
/// class exporting set / increase / decrease, each the same action the
/// on-screen affordance dispatches.
pub fn provide_resources(policy: &RetentionPolicy) -> Vec<Resource> {
    let mut root = Resource::new("backups.policy", "backups.policy", "Retention policy");

    for field in PolicyField::ALL {
        let cur = policy_field(policy, field);
        let mut fr = Resource::new(
            format!("backups.retention:{}", field.key()),
            "backups.retention",
            format!("{}: keep {} ({})", field.label(), cur, field.unit()),
        );

        let set_label = format!("Set {} retention", field.label().to_ascii_lowercase());
        fr = fr.with_action(
            ActionSpec::prompted(
                "set",
                set_label,
                "Count",
                "0–999",
                ExecutionLocus::ClientLocal,
                move |text: &str| encode(&set_retention_action(field, parse_count(text))),
            )
            .with_contract(policy_contract()),
        );

        // Offered only where there is room to move.
        if cur < 999 {
            fr = fr.with_action(immediate_spec(
                "increment",
                "Increase by one",
                &bump_retention_action(field, cur, 1),
            ));
        }
        if cur > 0 {
            fr = fr.with_action(immediate_spec(
                "decrement",
                "Decrease by one",
                &bump_retention_action(field, cur, -1),
            ));
        }

        root = root.with_child(fr);
    }

    vec![root]
}

impl ResourceProvider for crate::app::BackupsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        // Level-triggered: recomputed each frame from the document, the source of
        // truth every action reads and writes.
        provide_resources(&BackupsDoc::policy(doc, &ROOT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BackupsApp;
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
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    /// `sync` seeds the default retention policy, so this is ready for actions
    /// to read.
    fn seeded() -> (BackupsApp, AutoCommit) {
        let mut app = BackupsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    fn field_resource(root: &Resource, field: PolicyField) -> &Resource {
        let id = format!("backups.retention:{}", field.key());
        root.children
            .iter()
            .find(|r| r.id.as_str() == id)
            .unwrap_or_else(|| panic!("a resource for {id}"))
    }

    /// Drives the real resource hook and the real palette, not a mock tree.
    #[test]
    fn ctrl_k_publishes_the_retention_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 2, "policy and live backup-service roots");
        let root = resources
            .iter()
            .find(|resource| resource.kind == "backups.policy")
            .expect("retention-policy root");
        assert_eq!(root.kind, "backups.policy");
        assert_eq!(root.children.len(), 4, "one child per retention class");

        let daily = field_resource(root, PolicyField::Daily);
        assert_eq!(daily.kind, "backups.retention");
        let d_actions: Vec<&str> = daily.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(d_actions.contains(&"set"), "{d_actions:?}");
        assert!(d_actions.contains(&"increment"), "{d_actions:?}");
        assert!(d_actions.contains(&"decrement"), "{d_actions:?}");

        // The flattened tree exports every action across all four classes.
        let flat: Vec<&str> = qos_actions::flatten(&resources)
            .iter()
            .map(|f| f.action.label.as_str())
            .collect();
        assert!(flat.contains(&"Increase by one"), "{flat:?}");
        assert!(flat.iter().any(|l| l.starts_with("Set daily")), "{flat:?}");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels
                .iter()
                .any(|l| l.contains("retention") || l.contains("keep") || l.contains("Set")),
            "{labels:?}"
        );
    }

    #[test]
    fn unavailable_snapshot_effects_are_truthful_and_delete_is_destructive() {
        let resource = snapshot_resource("snap-42");
        let restore = resource
            .actions
            .iter()
            .find(|action| action.id == "restore")
            .unwrap();
        let delete = resource
            .actions
            .iter()
            .find(|action| action.id == "delete")
            .unwrap();
        assert!(!restore.contract.availability.is_available());
        assert!(!delete.contract.availability.is_available());
        assert_eq!(delete.contract.risk, qos_actions::RiskLevel::Destructive);
        assert_eq!(
            delete.contract.confirmation,
            qos_actions::ConfirmationPolicy::Confirm
        );
        let encoded = immediate_encode(delete);
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(BackupsOperation::Delete {
                snapshot_id: "snap-42".into()
            })
        );
    }

    #[test]
    fn bounded_fields_omit_the_impossible_move() {
        let (_app, mut doc) = seeded();
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Weekly, 0);
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Monthly, 999);
        let resources = provide_resources(&BackupsDoc::policy(&doc, &ROOT));
        assert!(
            resources
                .iter()
                .flat_map(|resource| &resource.children)
                .flat_map(|resource| &resource.actions)
                .all(|action| action.contract.reversible
                    && action.contract.retry == RetryPolicy::Idempotent)
        );
        let root = &resources[0];

        let weekly = field_resource(root, PolicyField::Weekly);
        let w_actions: Vec<&str> = weekly.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            !w_actions.contains(&"decrement"),
            "a 0-count field can't decrement: {w_actions:?}"
        );
        assert!(w_actions.contains(&"increment"), "{w_actions:?}");

        let monthly = field_resource(root, PolicyField::Monthly);
        let m_actions: Vec<&str> = monthly.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            !m_actions.contains(&"increment"),
            "a 999-count field can't increment: {m_actions:?}"
        );
        assert!(m_actions.contains(&"decrement"), "{m_actions:?}");
    }

    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut app = BackupsApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);

        let before = BackupsDoc::retention(&state.doc, &ROOT, PolicyField::Daily);
        let target = before + 3;
        let before_heads = state.doc.get_heads();

        let action = set_retention_action(PolicyField::Daily, target);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            BackupsDoc::retention(&state.doc, &ROOT, PolicyField::Daily),
            target,
            "the action set the daily retention"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            BackupsDoc::retention(&state.doc, &ROOT, PolicyField::Daily),
            before,
            "undo reverted the retention change and nothing else"
        );
    }

    /// Same head hashes on replay, not merely the same values.
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

        let was = BackupsDoc::retention(&a, &ROOT, PolicyField::Monthly);
        let action = set_retention_action(PolicyField::Monthly, was + 6);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            BackupsDoc::retention(&a, &ROOT, PolicyField::Monthly),
            was + 6,
            "the set landed"
        );
        assert_eq!(
            BackupsDoc::retention(&a, &ROOT, PolicyField::Monthly),
            BackupsDoc::retention(&b, &ROOT, PolicyField::Monthly),
            "both replicas agree"
        );
    }

    #[test]
    fn redundant_action_is_a_no_op() {
        let (_app, mut doc) = seeded();
        doc.commit();
        let before = doc.get_heads();
        let cur = BackupsDoc::retention(&doc, &ROOT, PolicyField::Hourly);

        let action = set_retention_action(PolicyField::Hourly, cur);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        assert_eq!(doc.get_heads(), before, "a no-change set changed nothing");
    }

    #[test]
    fn palette_specs_match_builders() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let root = &resources[0];
        let daily = field_resource(root, PolicyField::Daily);
        let cur = BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily);

        let inc_spec = daily.actions.iter().find(|a| a.id == "increment").unwrap();
        let from_spec = immediate_encode(inc_spec);
        let from_builder = encode(&bump_retention_action(PolicyField::Daily, cur, 1));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for increment"
        );

        let dec_spec = daily.actions.iter().find(|a| a.id == "decrement").unwrap();
        let from_spec = immediate_encode(dec_spec);
        let from_builder = encode(&bump_retention_action(PolicyField::Daily, cur, -1));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for decrement"
        );

        let set_spec = daily.actions.iter().find(|a| a.id == "set").unwrap();
        let from_spec = prompted_encode(set_spec, "42");
        let from_builder = encode(&set_retention_action(PolicyField::Daily, 42));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "set is one source of truth"
        );
    }

    /// `bump_retention` is what both the mouse hit and the keyboard arrow/`+`/`-`
    /// handlers funnel through.
    #[test]
    fn bump_button_dispatches_the_palette_action() {
        let (mut app, mut doc) = seeded();

        // The palette spec for Daily's increment, captured before we mutate.
        let (cur, spec_encoded) = {
            let resources = Widget::resources(&app, &doc);
            let daily = field_resource(&resources[0], PolicyField::Daily);
            let spec = daily.actions.iter().find(|a| a.id == "increment").unwrap();
            (
                BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily),
                immediate_encode(spec),
            )
        };

        // Drive the real on-screen affordance.
        app.bump_retention(&mut doc, &ROOT, PolicyField::Daily, 1);

        let dispatched = app
            .last_dispatched()
            .expect("the real +/- control dispatched an action");
        assert_eq!(dispatched.type_id, spec_encoded.type_id);
        assert_eq!(
            dispatched.payload, spec_encoded.payload,
            "the +/- control and the palette are one action"
        );
        // And it actually moved the stored policy.
        assert_eq!(
            BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily),
            cur + 1
        );
    }
}
