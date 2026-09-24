//! The VPN client's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Every document-mutating operation on the saved fleet is a [`VpnAction`]: a
//! small serializable value that deterministically mutates the shared Automerge
//! document (see [`crate::doc`]). The stored collaborative metadata is only a
//! profile id, display name, protocol, per-connection Tor routing preference,
//! and global "route everything over Tor" toggle. `NetworkManager` UUIDs,
//! endpoint/routing facts and credentials stay in the owner-local store. The exact same
//! [`EncodedAction`] an on-screen affordance dispatches (a completed import, a
//! connection's Tor toggle) is what the Ctrl+K palette dispatches — one source
//! of truth, one replay path, undo for free.
//!
//! The resource tree mirrors the model: the vault owns its connections. The
//! vault exports profile import (per protocol) and the global Tor toggle; each
//! connection exports rename / toggle-Tor / delete.
//!
//! ## What is *not* an action
//!
//! * **The live tunnel** — actually bringing a tunnel up/down (`wg-quick` /
//!   `openvpn`, see [`crate::config`] + [`crate::net`]), the reconciled live
//!   `status`, the byte counters, the last-handshake age. Those are external
//!   side effects and OS-projected live state *driven by* the configuration,
//!   never part of [`Action::apply`], which only mutates the CRDT
//!   deterministically.
//! * **Importing a system config** — the XDG-portal file pick, validation and
//!   privileged `NetworkManager` import are a typed host effect. Only the safe,
//!   secret-free profile summary is added to the shared document afterward.
//! * **Presentation** — which connection is selected, the detail tab, hover,
//!   and modal state remain local to each window.
//!
//! ## Determinism contract
//!
//! A connection's collaborative uuid is minted after validation and
//! `NetworkManager` import, then carried with its secret-free metadata in the
//! [`VpnAction::AddConnection`] payload. It is never the local `NetworkManager`
//! UUID; that association is persisted only on the provisioning device.
//! [`Action::apply`] never touches the RNG or the wall-clock. It reads the
//! current fleet from the document and applies a targeted, change-guarded write,
//! so two replicas at the same heads land on the byte-identical result. A
//! payload that names a connection that isn't there — or a toggle already in the
//! requested state — is a **no-op** rather than an error, so a bad replay can't
//! wedge a peer.

use automerge::AutoCommit;

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionContract, ActionError, ActionRegistry, ActionSpec, ConfirmationPolicy, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, PermissionRequirement, Resource,
    ResourceProvider, ResultSpec, RetryPolicy, RiskLevel,
};

use crate::doc::VpnDoc;
use crate::model::{ConnId, ConnKind, Connection, Vault};

/// Registry `type_id` for every VPN action.
pub const VPN_ACTION_TYPE: &str = "vpn.action";
pub const VPN_IMPORT_TYPE: &str = "vpn.import-profile";
pub const VPN_DELETE_TYPE: &str = "vpn.delete-profile";
pub const VPN_PROVISION_TYPE: &str = "vpn.provision-profile";

/// Every collaborative operation the VPN client exports on its saved fleet.
///
/// Each variant is a deterministic transform of the secret-free stored metadata: the
/// executor loads the current fleet from the document, this transform applies a
/// targeted write, and the diff replicates + replays for free (see
/// [`Action::apply`]). The connection CRUD carries a pre-minted [`Connection`]
/// (id and all) so replay reproduces it exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VpnAction {
    /// Add a saved connection profile. The secret-free identity (including its
    /// collaborative uuid) is
    /// minted at authoring time and carried whole; a re-add of the same id is a
    /// no-op so a double-applied envelope can't duplicate it. Boxed: a full
    /// [`Connection`] (many `String` fields) dwarfs every other variant, which
    /// would otherwise size every `VpnAction` to the largest payload.
    AddConnection { conn: Box<Connection> },
    /// Remove a saved connection by id. Absent id → no-op.
    RemoveConnection { id: ConnId },
    /// Rename a saved connection (metadata only — never touches the live
    /// status / counters). Absent id, empty name, or an unchanged name → no-op.
    RenameConnection { id: ConnId, name: String },
    /// Set a connection's "route this tunnel over Tor" preference. The target
    /// state is resolved at authoring time and carried, so replay is order-free.
    /// Absent id or an already-satisfied value → no-op.
    SetConnTor { id: ConnId, on: bool },
    /// Set the global "route everything over Tor" master preference. An
    /// already-satisfied value → no-op.
    SetTorGlobal { on: bool },
}

impl tilo_actions::Action for VpnAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Read the current fleet for change-guards, then apply a *targeted*
        // field write (never a whole-vault rewrite — that would churn every
        // connection's fields and blow up tombstones). Each branch that can't
        // find its target, or that would change nothing, leaves the document
        // (its heads) untouched, so a no-op replay can't wedge a peer.
        let vault = VpnDoc::read_vault(doc);
        match self {
            VpnAction::AddConnection { conn } => {
                if vault.get(conn.id).is_none() {
                    VpnDoc::write_connection(doc, conn);
                }
            }
            VpnAction::RemoveConnection { id } => {
                if vault.get(*id).is_some() {
                    VpnDoc::remove_connection(doc, *id);
                }
            }
            VpnAction::RenameConnection { id, name } => {
                let name = name.trim();
                if let Some(c) = vault.get(*id)
                    && !name.is_empty()
                    && c.name != name
                {
                    VpnDoc::set_name(doc, *id, name.to_string());
                }
            }
            VpnAction::SetConnTor { id, on } => {
                if let Some(c) = vault.get(*id)
                    && c.tor != *on
                {
                    VpnDoc::set_conn_tor(doc, *id, *on);
                }
            }
            VpnAction::SetTorGlobal { on } => {
                if vault.tor_global != *on {
                    VpnDoc::set_tor_global(doc, *on);
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // Saved tunnels + routing preferences are personal config; this client
        // applies + replicates and peers replay.
        ExecutionLocus::ClientLocal
    }
}

/// Register the VPN action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<VpnAction>(VPN_ACTION_TYPE);
}

/// Encode any [`VpnAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a VPN action into a
/// replayable envelope.
///
/// # Panics
/// Never in practice: every `VpnAction` field is a `String`, `bool`, `ConnId`
/// (`Uuid`) or a `Connection` of the same, all of which `serde_json` always
/// serializes; this only panics if a future field type introduces a case
/// JSON can't represent (e.g. a non-string map key or a non-finite float).
#[allow(
    clippy::expect_used,
    reason = "VpnAction's fields are all JSON-safe primitives; see this function's # Panics doc"
)]
pub fn encode(action: &VpnAction) -> EncodedAction {
    EncodedAction::encode(VPN_ACTION_TYPE, action).expect("VpnAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

pub fn add_connection_action(conn: Connection) -> VpnAction {
    VpnAction::AddConnection {
        conn: Box::new(conn),
    }
}

pub fn remove_connection_action(id: ConnId) -> VpnAction {
    VpnAction::RemoveConnection { id }
}

pub fn rename_connection_action(id: ConnId, name: String) -> VpnAction {
    VpnAction::RenameConnection { id, name }
}

pub fn set_conn_tor_action(id: ConnId, on: bool) -> VpnAction {
    VpnAction::SetConnTor { id, on }
}

pub fn set_tor_global_action(on: bool) -> VpnAction {
    VpnAction::SetTorGlobal { on }
}

// ===========================================================================
// Resource tree
// ===========================================================================

#[allow(
    clippy::expect_used,
    reason = "VpnAction's fields are all JSON-safe primitives; see encode()'s # Panics doc"
)]
fn immediate_spec(id: &str, label: &str, action: &VpnAction) -> ActionSpec {
    ActionSpec::immediate(id, label, VPN_ACTION_TYPE, action)
        .expect("encode vpn action")
        .with_contract(config_contract())
}

fn config_contract() -> ActionContract {
    ActionContract {
        retry: RetryPolicy::Idempotent,
        reversible: true,
        tags: vec!["configuration".into(), "vpn".into()],
        ..ActionContract::default()
    }
}

/// A one-line label for a connection in the palette.
fn connection_label(c: &Connection) -> String {
    format!("{} — {} · {}", c.name, c.kind.long_name(), c.status.label())
}

/// Build the resource tree the VPN client currently has open: the vault (add a
/// connection per protocol + the global Tor toggle) and each saved connection
/// (rename, toggle Tor, delete). Every document-mutating operation is exported
/// as the same action the on-screen affordance dispatches.
///
/// The vault root is always published — an open VPN app can always "add a
/// connection", even with an empty fleet — while the per-connection children
/// track the saved list.
#[allow(
    clippy::too_many_lines,
    reason = "one resource-tree builder keeps profile actions and their contracts together"
)]
pub fn provide_resources(vault: &Vault) -> Vec<Resource> {
    let mut root = Resource::new("vpn.vault", "vpn.vault", "VPN");

    // Global Tor preference (stored config).
    let tor_label = if vault.tor_global {
        "Stop routing all traffic over Tor"
    } else {
        "Route all traffic over Tor"
    };
    root = root.with_action(immediate_spec(
        "toggle-tor-global",
        tor_label,
        &set_tor_global_action(!vault.tor_global),
    ));

    // A name is not a VPN profile. Import is a typed privileged host effect;
    // the collaborative add action is emitted only after validation succeeds.
    for kind in [ConnKind::WireGuard, ConnKind::OpenVpn] {
        let (id, label) = match kind {
            ConnKind::WireGuard => ("import-wireguard", "Import WireGuard profile"),
            ConnKind::OpenVpn => ("import-openvpn", "Import OpenVPN profile"),
        };
        let Ok(mut spec) = ActionSpec::operation(
            id,
            label,
            OperationKind::Effect,
            VPN_IMPORT_TYPE,
            &kind.as_str(),
        ) else {
            continue;
        };
        spec.contract = ActionContract {
            permission: PermissionRequirement::Admin,
            risk: RiskLevel::Privileged,
            confirmation: ConfirmationPolicy::Confirm,
            retry: RetryPolicy::AtMostOnce,
            execution: ExecutionMode::Background { cancellable: true },
            result: ResultSpec::Job,
            tags: vec!["vpn".into(), "import".into(), kind.as_str().into()],
            ..ActionContract::default()
        };
        root = root.with_action(spec);
    }

    for c in &vault.connections {
        let id = c.id;
        let mut cr = Resource::new(
            format!("vpn.connection:{id}"),
            "vpn.connection",
            connection_label(c),
        );

        cr.children.push(Resource::new(
            format!("vpn.connection:{id}:protocol"),
            "vpn.protocol",
            c.kind.long_name(),
        ));
        if let Ok(mut provision) = ActionSpec::operation(
            "provision-local",
            "Set up on this device",
            OperationKind::Effect,
            VPN_PROVISION_TYPE,
            &(id.to_string(), c.kind.as_str()),
        ) {
            provision.contract = ActionContract {
                permission: PermissionRequirement::Admin,
                risk: RiskLevel::Privileged,
                confirmation: ConfirmationPolicy::Confirm,
                retry: RetryPolicy::AtMostOnce,
                execution: ExecutionMode::Background { cancellable: true },
                result: ResultSpec::Job,
                tags: vec!["vpn".into(), "provisioning".into()],
                ..ActionContract::default()
            };
            cr = cr.with_action(provision);
        }
        // Never make endpoint, routing, address, certificate/key or other
        // tunnel material searchable. A peer can navigate the stable profile
        // id, then provision its exact local credentials through the portal.
        cr.children.push(Resource::new(
            format!("vpn.connection:{id}:provisioning"),
            "vpn.local-provisioning",
            "Provisioning is local to this device",
        ));

        // Toggle this tunnel's Tor routing preference.
        let tor_label = if c.tor {
            "Stop routing over Tor"
        } else {
            "Route over Tor"
        };
        cr = cr.with_action(immediate_spec(
            "toggle-tor",
            tor_label,
            &set_conn_tor_action(id, !c.tor),
        ));

        // Removal must delete the real NetworkManager profile before the
        // shared secret-free summary disappears.
        if let Ok(mut delete) = ActionSpec::operation(
            "delete",
            "Delete connection",
            OperationKind::Effect,
            VPN_DELETE_TYPE,
            &(id, c.name.clone()),
        ) {
            delete.contract = ActionContract {
                permission: PermissionRequirement::Admin,
                risk: RiskLevel::Destructive,
                confirmation: ConfirmationPolicy::Confirm,
                retry: RetryPolicy::AtMostOnce,
                execution: ExecutionMode::Background { cancellable: false },
                result: ResultSpec::Job,
                tags: vec!["configuration".into(), "vpn".into(), "delete".into()],
                ..ActionContract::default()
            };
            cr = cr.with_action(delete);
        }

        root = root.with_child(cr);
    }

    vec![root]
}

impl ResourceProvider for crate::app::VpnApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        // Level-triggered: recomputed each frame from the document, the source
        // of truth every action reads and writes.
        provide_resources(&VpnDoc::read_vault(doc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::VpnApp;
    use crate::model::draft_connection;
    use automerge::ROOT;
    use tilo_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;
    use uuid::Uuid;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    #[test]
    fn published_delete_declares_destructive_confirmation() {
        let vault = crate::model::sample_vault();
        let resources = provide_resources(&vault);
        let delete = resources[0].children[0]
            .actions
            .iter()
            .find(|action| action.id == "delete")
            .expect("saved connection exports delete");
        assert_eq!(delete.contract.risk, RiskLevel::Destructive);
        assert_eq!(delete.contract.confirmation, ConfirmationPolicy::Confirm);
    }

    #[test]
    fn setup_resource_carries_exact_profile_id_and_protocol() {
        let vault = crate::model::sample_vault();
        let profile = &vault.connections[0];
        let resources = provide_resources(&vault);
        let resource = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.0 == format!("vpn.connection:{}", profile.id))
            .expect("profile resource");
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == "provision-local")
            .expect("exact setup action");
        let encoded = immediate_encode(action);
        assert_eq!(encoded.type_id, VPN_PROVISION_TYPE);
        let (id, kind): (String, String) = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(id, profile.id.to_string());
        assert_eq!(kind, profile.kind.as_str());
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                panic!("expected an immediate action: {}", spec.id)
            }
        }
    }

    /// The demo (sample-seeded) VPN app synced into a fresh document, plus the
    /// app that owns it. `sync` seeds the sample fleet into the document (which
    /// is empty here) and reads it back, so the document is the source of truth
    /// every action reads.
    fn seeded() -> (VpnApp, AutoCommit) {
        let mut app = VpnApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    fn home_id(root: &Resource) -> ConnId {
        root.children
            .iter()
            .find(|r| r.label.starts_with("Home"))
            .expect("a Home connection resource")
            .id
            .as_str()
            .trim_start_matches("vpn.connection:")
            .parse()
            .expect("a uuid connection id")
    }

    /// Ctrl+K on the VPN client yields the tree: the vault (add-per-protocol +
    /// global Tor) and a `vpn.connection` child per saved connection exporting
    /// rename / toggle-Tor / delete. Drives the real resource hook and the real
    /// palette.
    #[test]
    fn ctrl_k_publishes_the_vpn_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one vault root");
        let root = &resources[0];
        assert_eq!(root.kind, "vpn.vault");

        let root_actions: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            root_actions.contains(&"toggle-tor-global"),
            "{root_actions:?}"
        );
        assert!(
            root_actions.contains(&"import-wireguard"),
            "{root_actions:?}"
        );
        assert!(root_actions.contains(&"import-openvpn"), "{root_actions:?}");
        assert!(
            !root.children.is_empty(),
            "the sample connections are published"
        );

        let home = root
            .children
            .iter()
            .find(|r| r.label.starts_with("Home"))
            .expect("a Home connection resource");
        assert_eq!(home.kind, "vpn.connection");
        let h_actions: Vec<&str> = home.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(h_actions.contains(&"toggle-tor"), "{h_actions:?}");
        assert!(h_actions.contains(&"delete"), "{h_actions:?}");

        // The flattened tree exports every action, including the per-connection
        // operations that sort after the vault-level ones.
        let flat: Vec<&str> = tilo_actions::flatten(&resources)
            .iter()
            .map(|f| f.action.label.as_str())
            .collect();
        assert!(flat.contains(&"Delete connection"), "{flat:?}");
        assert!(flat.contains(&"Import WireGuard profile"), "{flat:?}");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels
                .iter()
                .any(|l| l.contains("VPN") || l.contains("connection") || l.contains("Tor")),
            "{labels:?}"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let mut app = VpnApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);

        let before = VpnDoc::read_vault(&state.doc);
        let victim = before.connections[0].id;
        let before_heads = state.doc.get_heads();
        let before_len = before.connections.len();

        let action = remove_connection_action(victim);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        let after = VpnDoc::read_vault(&state.doc);
        assert_eq!(
            after.connections.len(),
            before_len - 1,
            "the action deleted a connection"
        );
        assert!(
            !after.connections.iter().any(|c| c.id == victim),
            "the right connection is gone"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        let restored = VpnDoc::read_vault(&state.doc);
        assert_eq!(
            restored.connections.len(),
            before_len,
            "undo reverted the deletion and nothing else"
        );
        assert!(
            restored.connections.iter().any(|c| c.id == victim),
            "the connection is back after undo"
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

        // Toggle a connection's Tor preference: deterministic against the shared
        // base (both replicas write the carried target state).
        let vault = VpnDoc::read_vault(&a);
        let cid = vault.connections[0].id;
        let was = vault.connections[0].tor;
        let action = set_conn_tor_action(cid, !was);
        let encoded = encode(&action);

        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        let after_a = VpnDoc::read_vault(&a);
        let after_b = VpnDoc::read_vault(&b);
        let tor_a = after_a.get(cid).unwrap().tor;
        let tor_b = after_b.get(cid).unwrap().tor;
        assert_eq!(tor_a, !was, "the toggle landed");
        assert_eq!(tor_a, tor_b, "both replicas agree");
    }

    /// Replaying an `AddConnection` (a create) also converges byte-identically —
    /// the connection's uuid is carried in the payload, so both replicas write
    /// the same profile.
    #[test]
    fn add_connection_replays_byte_identical() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let conn = draft_connection("Travel", ConnKind::WireGuard);
        let cid = conn.id;
        let action = add_connection_action(conn);
        let encoded = encode(&action);

        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "create replayed identically");
        assert!(
            VpnDoc::read_vault(&b).get(cid).is_some(),
            "the added connection is present on the peer"
        );
    }

    /// A malformed / out-of-range payload is a no-op, not an error: deleting a
    /// connection id that isn't there leaves the document (its heads) untouched.
    #[test]
    fn out_of_range_action_is_a_no_op() {
        let (_app, mut doc) = seeded();
        let before = doc.get_heads();
        let action = remove_connection_action(Uuid::from_u128(0xDEAD_BEEF));
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        assert_eq!(
            doc.get_heads(),
            before,
            "a delete of an absent connection changed nothing"
        );
    }

    /// Profile removal is a privileged OS operation.
    #[test]
    fn palette_specs_match_builders() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let root = &resources[0];
        let home = root
            .children
            .iter()
            .find(|r| r.label.starts_with("Home"))
            .unwrap();

        let delete_spec = home.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        assert_eq!(from_spec.type_id, VPN_DELETE_TYPE);

        let vault = VpnDoc::read_vault(&doc);
        let global = root
            .actions
            .iter()
            .find(|a| a.id == "toggle-tor-global")
            .unwrap();
        let from_spec = immediate_encode(global);
        let from_builder = encode(&set_tor_global_action(!vault.tor_global));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "global Tor is one source of truth"
        );
    }

    /// Single source of truth on the *real* affordance path: the on-screen
    /// per-connection Tor toggle (`toggle_conn_tor`) dispatches the byte-identical
    /// action the palette's `toggle-tor` spec exports.
    #[test]
    fn tor_toggle_button_dispatches_the_palette_action() {
        let (mut app, mut doc) = seeded();

        // Read the palette spec for Home's toggle-Tor, capturing the owned
        // encoded action before we mutate the app.
        let (id, spec_encoded) = {
            let resources = Widget::resources(&app, &doc);
            let root = &resources[0];
            let id = home_id(root);
            let home = root
                .children
                .iter()
                .find(|r| r.label.starts_with("Home"))
                .unwrap();
            let spec = home.actions.iter().find(|a| a.id == "toggle-tor").unwrap();
            (id, immediate_encode(spec))
        };

        // Drive the real on-screen affordance.
        app.toggle_conn_tor(&mut doc, id);

        let dispatched = app
            .last_dispatched()
            .expect("the real toggle dispatched an action");
        assert_eq!(dispatched.type_id, spec_encoded.type_id);
        assert_eq!(
            dispatched.payload, spec_encoded.payload,
            "the button and the palette are one action"
        );
    }
}
