//! The VPN client's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Every document-mutating operation on the saved fleet is a [`VpnAction`]: a
//! small serializable value that deterministically mutates the shared Automerge
//! document (see [`crate::doc`]). The stored **configuration** is the document —
//! the saved connection profiles, their per-connection Tor routing preference,
//! and the global "route everything over Tor" toggle. The exact same
//! [`EncodedAction`] an on-screen affordance dispatches (the add form, a
//! connection's Tor toggle) is what the Ctrl+K palette dispatches — one source
//! of truth, one replay path, undo for free.
//!
//! The resource tree mirrors the model: the vault owns its connections. The
//! vault exports "add connection" (per protocol) and the global Tor toggle; each
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
//! * **Importing a system config** — the XDG-portal file pick, reading and
//!   parsing the `.conf` / `.ovpn`, and persisting it to the data dir are
//!   external steps ([`crate::portal`], [`crate::config`]). Only the resulting
//!   parsed [`Connection`] is added to the document, and that add is the same
//!   [`VpnAction::AddConnection`] the add form dispatches.
//! * **Presentation** — which connection is selected, the detail tab, hover,
//!   the confirm/add modals. Selection is stored in the document as a shared
//!   cursor but is view state, never an action.
//!
//! ## Determinism contract
//!
//! A connection's uuid is minted at *authoring* time (in [`crate::model::stub_connection`]
//! / [`crate::model::Connection::new`], called by the affordance or the palette
//! closure) and carried whole in the [`VpnAction::AddConnection`] payload;
//! [`Action::apply`] never touches the RNG or the wall-clock. It reads the
//! current fleet from the document and applies a targeted, change-guarded write,
//! so two replicas at the same heads land on the byte-identical result. A
//! payload that names a connection that isn't there — or a toggle already in the
//! requested state — is a **no-op** rather than an error, so a bad replay can't
//! wedge a peer.

use automerge::AutoCommit;

use qos_actions::{
    ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};
use serde::{Deserialize, Serialize};

use crate::doc::VpnDoc;
use crate::model::{ConnId, ConnKind, Connection, Vault, stub_connection};

/// Registry `type_id` for every VPN action.
pub const VPN_ACTION_TYPE: &str = "vpn.action";

/// Every collaborative operation the VPN client exports on its saved fleet.
///
/// Each variant is a deterministic transform of the stored configuration: the
/// executor loads the current fleet from the document, this transform applies a
/// targeted write, and the diff replicates + replays for free (see
/// [`Action::apply`]). The connection CRUD carries a pre-minted [`Connection`]
/// (id and all) so replay reproduces it exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VpnAction {
    /// Add a saved connection profile. The connection (including its uuid) is
    /// minted at authoring time and carried whole; a re-add of the same id is a
    /// no-op so a double-applied envelope can't duplicate it.
    AddConnection { conn: Connection },
    /// Remove a saved connection by id. Absent id → no-op.
    RemoveConnection { id: ConnId },
    /// Rename a saved connection (config field only — never touches the live
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

impl qos_actions::Action for VpnAction {
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
pub fn encode(action: &VpnAction) -> EncodedAction {
    EncodedAction::encode(VPN_ACTION_TYPE, action).expect("VpnAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

pub fn add_connection_action(conn: Connection) -> VpnAction {
    VpnAction::AddConnection { conn }
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

fn immediate_spec(id: &str, label: &str, action: &VpnAction) -> ActionSpec {
    ActionSpec::immediate(id, label, VPN_ACTION_TYPE, action).expect("encode vpn action")
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

    // Add a saved connection — one prompted action per protocol. The uuid is
    // minted once, in the closure, when the name argument is committed, then
    // baked into the envelope so replay stays deterministic.
    for kind in [ConnKind::WireGuard, ConnKind::OpenVpn] {
        let (id, label) = match kind {
            ConnKind::WireGuard => ("add-wireguard", "Add WireGuard connection"),
            ConnKind::OpenVpn => ("add-openvpn", "Add OpenVPN connection"),
        };
        root = root.with_action(ActionSpec::prompted(
            id,
            label,
            "Name",
            "e.g. Home",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&add_connection_action(stub_connection(name, kind))),
        ));
    }

    for c in &vault.connections {
        let id = c.id;
        let mut cr = Resource::new(
            format!("vpn.connection:{id}"),
            "vpn.connection",
            connection_label(c),
        );

        // Rename — prompted for the new name.
        cr = cr.with_action(ActionSpec::prompted(
            "rename",
            "Rename connection",
            "Name",
            "new name",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&rename_connection_action(id, name.trim().to_string())),
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

        // Delete the saved connection.
        cr = cr.with_action(immediate_spec(
            "delete",
            "Delete connection",
            &remove_connection_action(id),
        ));

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
    use automerge::ROOT;
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;
    use uuid::Uuid;

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
        assert!(root_actions.contains(&"add-wireguard"), "{root_actions:?}");
        assert!(root_actions.contains(&"add-openvpn"), "{root_actions:?}");
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
        assert!(h_actions.contains(&"rename"), "{h_actions:?}");
        assert!(h_actions.contains(&"toggle-tor"), "{h_actions:?}");
        assert!(h_actions.contains(&"delete"), "{h_actions:?}");

        // The flattened tree exports every action, including the per-connection
        // operations that sort after the vault-level ones.
        let flat: Vec<&str> = qos_actions::flatten(&resources)
            .iter()
            .map(|f| f.action.label.as_str())
            .collect();
        assert!(flat.contains(&"Delete connection"), "{flat:?}");
        assert!(flat.contains(&"Add WireGuard connection"), "{flat:?}");

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
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
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

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

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

        let conn = stub_connection("Travel", ConnKind::WireGuard);
        let cid = conn.id;
        let action = add_connection_action(conn);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

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
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        assert_eq!(
            doc.get_heads(),
            before,
            "a delete of an absent connection changed nothing"
        );
    }

    /// Single source of truth (spec == builder): the palette's per-connection
    /// delete / rename specs and the vault's global-Tor spec encode
    /// byte-identically to the builders the on-screen affordances call.
    #[test]
    fn palette_specs_match_builders() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let root = &resources[0];
        let id = home_id(root);
        let home = root
            .children
            .iter()
            .find(|r| r.label.starts_with("Home"))
            .unwrap();

        let delete_spec = home.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        let from_builder = encode(&remove_connection_action(id));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        let rename_spec = home.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rename_spec, "Cabin");
        let from_builder = encode(&rename_connection_action(id, "Cabin".to_string()));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );

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
