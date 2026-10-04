//! The VPN client's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Every document-mutating operation on the saved fleet is a [`VpnAction`]: a
//! small serializable value that deterministically mutates the shared Automerge
//! document (see [`crate::doc`]). The stored collaborative metadata is only a
//! profile id, display name, protocol and per-connection Tor routing
//! preference. `NetworkManager` UUIDs,
//! endpoint/routing facts and credentials stay in the owner-local store. The exact same
//! action an on-screen affordance dispatches (a completed import, a
//! connection's Tor toggle) is what the Ctrl+K palette dispatches — one source
//! of truth, one replay path, undo for free.
//!
//! The resource tree mirrors the model: the vault owns its connections, each
//! exporting its Tor preference; renaming is a declaration on the vault. What
//! a profile does on this device (connect, disconnect, set up, delete) is a
//! [`crate::effects::VpnEffect`] the app publishes on the same child.
//!
//! ## What is *not* an action
//!
//! * **The live tunnel** — actually bringing a tunnel up/down (`wg-quick` /
//!   `openvpn`, see [`crate::config`] + [`crate::net`]), the reconciled live
//!   `status`, the byte counters, the last-handshake age. Those are external
//!   side effects and OS-projected live state *driven by* the configuration,
//!   never part of `Action::apply`, which only mutates the CRDT
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
//! `Action::apply` never touches the RNG or the wall-clock. It reads the
//! current fleet from the document and applies a targeted, change-guarded write,
//! so two replicas at the same heads land on the byte-identical result. A
//! payload that names a connection that isn't there — or a toggle already in the
//! requested state — is a **no-op** rather than an error, so a bad replay can't
//! wedge a peer.

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionContract, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    Resource, RetryPolicy,
};

use crate::doc::VpnDoc;
use crate::model::{ConnId, Connection, Vault};

/// Registry `type_id` for every VPN action.
pub const VPN_ACTION_TYPE: &str = "vpn.action";

/// Every collaborative operation the VPN client exports on its saved fleet.
///
/// Each variant is a deterministic transform of the secret-free stored metadata: the
/// executor loads the current fleet from the document, this transform applies a
/// targeted write, and the diff replicates + replays for free (see
/// `Action::apply`). The connection CRUD carries a pre-minted [`Connection`]
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
    // The tunnel's state is this device's, not the shared profile's.
    format!("{} — {}", c.name, c.kind.long_name())
}

/// Saved profiles as children of the published vault.
pub(crate) fn connection_resources(vault: &Vault) -> Vec<Resource> {
    vault
        .connections
        .iter()
        .map(|c| {
            let id = c.id;
            let mut cr = Resource::new(
                crate::effects::connection_resource_id(id),
                "vpn.connection",
                connection_label(c),
            );
            cr.children.push(Resource::new(
                format!("vpn.connection:{id}:protocol"),
                "vpn.protocol",
                c.kind.long_name(),
            ));
            // Never make endpoint, routing, address, certificate/key or other
            // tunnel material searchable. A peer can navigate the stable
            // profile id, then set it up with its exact local credentials.
            cr.children.push(Resource::new(
                format!("vpn.connection:{id}:provisioning"),
                "vpn.local-provisioning",
                "Provisioning is local to this device",
            ));
            let tor_label = if c.tor {
                "Stop routing over Tor"
            } else {
                "Route over Tor"
            };
            cr.with_action(immediate_spec(
                "toggle-tor",
                tor_label,
                &set_conn_tor_action(id, !c.tor),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnKind, draft_connection, sample_vault};
    use tilo_actions::LocalTransport;
    use ui::automerge::AutoCommit;
    use uuid::Uuid;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// A document holding the sample fleet.
    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        VpnDoc::write_vault(&mut doc, &sample_vault());
        doc.commit();
        doc
    }

    fn published(doc: &AutoCommit) -> Vec<Resource> {
        vec![ui::tilo_resource::publish::<crate::resource::Vpn>(
            "vpn.vault",
            "VPN",
            &VpnDoc::read_vault(doc),
        )]
    }

    #[test]
    fn an_action_naming_an_absent_connection_changes_nothing() {
        let mut doc = seeded();
        let before = doc.get_heads();
        let encoded = encode(&remove_connection_action(Uuid::from_u128(0xDEAD_BEEF)));
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        assert_eq!(doc.get_heads(), before);
    }

    fn first_conn(doc: &AutoCommit) -> Connection {
        VpnDoc::read_vault(doc)
            .connections
            .into_iter()
            .next()
            .expect("a seeded connection")
    }

    fn conn_id(res: &Resource) -> ConnId {
        res.id
            .as_str()
            .trim_start_matches("vpn.connection:")
            .parse()
            .expect("a uuid connection id")
    }

    ui::action_contract_tests! {
        seed: seeded(),
        registry: registry(),
        resources: |doc| published(doc),
        palette: [
            "Rename connection",
            "Route over Tor",
        ],
        replay: [
            toggle_conn_tor => |doc| {
                let conn = first_conn(doc);
                encode(&set_conn_tor_action(conn.id, !conn.tor))
            },
            add_connection => |_| encode(&add_connection_action(draft_connection(
                "Travel",
                ConnKind::WireGuard,
            ))),
            rename_connection => |doc| encode(&rename_connection_action(
                first_conn(doc).id,
                "Renamed".into(),
            )),
        ],
        undo: [
            remove_connection => |doc| encode(&remove_connection_action(first_conn(doc).id)),
        ],
        encodes: [
            toggle_conn_tor => ("vpn.connection", "toggle-tor", |doc, res| {
                let id = conn_id(res);
                let on = VpnDoc::read_vault(doc).get(id).expect("published connection").tor;
                encode(&set_conn_tor_action(id, !on))
            }),
        ],
    }
}
