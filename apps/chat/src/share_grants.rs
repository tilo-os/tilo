//! What sharing a window gives: a grant of the room's, on the one resource
//! the window shows.
//!
//! A share's access is no promise of the card's. While the share is live,
//! its room grants the members what the access names on that resource only
//! (`view`: reading it; `edit`: changing it too), as far as the person
//! sharing holds it, and takes it back when the share ends. Chat asks for
//! both through the workspace's session (`tilo_actions::remote::node_target`),
//! and the room decides every write the grant then covers.
//!
//! The room's answer to whether this person may change what someone else
//! shares reaches their desktop, so a window on an item shared with them
//! to edit lets them edit it.
//!
//! A grant adds; it takes nothing away. Someone who may change the resource
//! anyway still may, so a view-only card says so to them instead of
//! promising a view they are not limited to.

use std::collections::HashMap;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    InvokeArgument, NODE_PERMITS_ACTION, SHARE_NODE_ACTION, UNSHARE_NODE_ACTION, node_target,
};
use ui::permissions::{TreePath, policy::Level};
use ui::{Arrival, InvocationCall, RemoteActions};

use crate::app::ChatApp;
use crate::model::Message;
use crate::record::ShareAccess;

/// The grants of this person's shares and what the room lets them do with
/// the resources others share.
#[derive(Default)]
pub(crate) struct ShareGrants {
    door: Option<RemoteActions>,
    /// What each of this person's shares last asked the room: to grant
    /// what an access gives, or (`None`) to take it back, by message.
    asked: HashMap<String, Option<ShareAccess>>,
    /// Asks still on their way, kept until they settle.
    calls: Vec<InvocationCall>,
    /// Whether this person may change what a share shares, by message,
    /// with what the share gave when asked: asked again whenever the share
    /// is switched or ends.
    editable: HashMap<String, (Editable, Option<ShareAccess>)>,
}

enum Editable {
    Asking(InvocationCall),
    Answered(bool),
}

/// The node of the room's tree that `reference`, which `provider`
/// (`record:<app>:<record>`) keeps, stands at: an item of the record
/// (`<kind>/<field>/<item>`), or the whole record. `None` for what names
/// neither.
pub(crate) fn share_node(
    room: &str,
    provider: &str,
    reference: &tilo_reference::ResourceRef,
) -> Option<TreePath> {
    let (app, record) = crate::live_share::app_and_record(provider)?;
    let id = reference.id.as_str();
    let at = |kind: &str| TreePath::room(room).app(app).kind(kind);
    if let Some((collection, item)) = id.rsplit_once('/')
        && let Some((kind, field)) = collection.split_once('/')
        && !field.contains('/')
    {
        // An app's own record of the workspace travels under no id of its
        // own; the item's id is unique, so any record of the kind is it.
        let resource = if record.is_empty() {
            at(kind).any(Level::Resource)
        } else {
            at(kind).resource(record)
        };
        return Some(resource.item(item));
    }
    (!record.is_empty() && id == record).then(|| at(&reference.kind).resource(record))
}

impl ShareGrants {
    /// Ask again whether this person may change what `message` shares: the
    /// share's grant may have reached the room since.
    pub(crate) fn ask_again(&mut self, message: &str) {
        if matches!(self.editable.get(message), Some((Editable::Answered(_), _))) {
            self.editable.remove(message);
        }
    }

    fn door(&mut self) -> &mut RemoteActions {
        self.door.get_or_insert_with(RemoteActions::current)
    }

    fn ask(&mut self, room: &str, node: &TreePath, action: &str, arguments: serde_json::Value) {
        let target = node_target(room, &node.to_string(), action);
        let argument = match arguments {
            serde_json::Value::Object(map) => InvokeArgument::Json(map),
            _ => InvokeArgument::None,
        };
        let call = self.door().invoke(target, argument, None);
        self.calls.push(call);
    }
}

/// Whether `call` settled, and with which `undoable` answer.
fn permits(call: &InvocationCall) -> Option<bool> {
    if pending(call) {
        return None;
    }
    Some(call.with(|arrival| {
        match arrival {
            Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => output
                .value
                .as_ref()
                .and_then(|value| value.get("undoable"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            // A room that cannot say promises nothing.
            _ => false,
        }
    }))
}

/// Whether `call` is still on its way.
fn pending(call: &InvocationCall) -> bool {
    call.with(|arrival| {
        matches!(
            arrival,
            Arrival::Pending { .. }
                | Arrival::Ready(
                    InvocationUpdate::Accepted { .. }
                        | InvocationUpdate::Progress { .. }
                        | InvocationUpdate::Item { .. }
                )
        )
    })
}

impl ChatApp {
    /// Ask the room to grant what each of this person's live shares gives
    /// and take back what each ended one gave, once per change (a share
    /// switched in place included); and ask, of each share of someone
    /// else's and again whenever it is switched or ends, whether this
    /// person may change what it shares (on a view-only share: whether
    /// they may anyway).
    pub(crate) fn follow_share_grants(&mut self) {
        let Ok(room) = ui::framework::reference::room() else {
            return;
        };
        let shares: Vec<(String, bool, bool, ShareAccess, Option<TreePath>)> = self
            .search_messages
            .iter()
            .filter_map(|m: &Message| {
                let share = m.share.as_ref()?;
                let node = share_node(&room, m.source_provider.as_deref()?, m.reference.as_ref()?);
                Some((
                    m.id.clone(),
                    self.is_me(&m.author),
                    !share.ended,
                    share.access,
                    node,
                ))
            })
            .collect();
        let mut changed = false;
        let grants = &mut self.live_shares.grants;
        for (message, mine, live, access, node) in shares {
            let Some(node) = node else {
                continue;
            };
            // What the share gives now: its access while live, nothing once ended.
            let now = live.then_some(access);
            if mine {
                if grants.asked.get(&message) == Some(&now) {
                    continue;
                }
                grants.asked.insert(message.clone(), now);
                match now {
                    // Asked again under the same share id when switched: the
                    // room replaces what the share's grants give.
                    Some(access) => grants.ask(
                        &room,
                        &node,
                        SHARE_NODE_ACTION,
                        serde_json::json!({ "share": message, "access": access.as_str() }),
                    ),
                    None => grants.ask(
                        &room,
                        &node,
                        UNSHARE_NODE_ACTION,
                        serde_json::json!({ "share": message }),
                    ),
                }
            } else {
                // Asked of every live share of someone else's: the answer
                // also lets this person's window change an item shared to
                // edit. Asked again when the share is switched or ends, so
                // their window offers exactly what the room keeps.
                let ask = match grants.editable.get(&message) {
                    None => live,
                    Some((Editable::Asking(call), asked)) => {
                        if let Some(may) = permits(call) {
                            let asked = *asked;
                            grants
                                .editable
                                .insert(message.clone(), (Editable::Answered(may), asked));
                            changed = true;
                        }
                        false
                    }
                    Some((Editable::Answered(_), asked)) => *asked != now,
                };
                if ask {
                    let target = node_target(&room, &node.to_string(), NODE_PERMITS_ACTION);
                    let call = grants.door().invoke(target, InvokeArgument::None, None);
                    grants
                        .editable
                        .insert(message, (Editable::Asking(call), now));
                }
            }
        }
        grants.calls.retain(pending);
        if changed {
            self.request_relayout();
        }
    }

    /// Whether this person may change what `message` shares whatever the
    /// share gives, as the room answered.
    pub(crate) fn can_already_edit(&self, message: &str) -> bool {
        matches!(
            self.live_shares.grants.editable.get(message),
            Some((Editable::Answered(true), _))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::share_node;

    fn reference(kind: &str, id: &str) -> tilo_reference::ResourceRef {
        tilo_reference::ResourceRef::whole(kind, id, "Plan")
    }

    #[test]
    fn a_share_stands_at_the_item_or_the_record_it_shares() {
        let room = "00000000-0000-0000-0000-000000000001";
        let note = reference("notes.notebook.pages", "notes.notebook/pages/p1");
        assert_eq!(
            share_node(room, "record:so.tilo.notes:", &note).map(|node| node.to_string()),
            Some(format!(
                "room:{room}/app:so.tilo.notes/kind:notes.notebook/resource:*/item:p1"
            )),
            "a note of the room's notebook"
        );
        assert_eq!(
            share_node(room, "record:so.tilo.notes:r1", &note).map(|node| node.to_string()),
            Some(format!(
                "room:{room}/app:so.tilo.notes/kind:notes.notebook/resource:r1/item:p1"
            ))
        );
        let deck = reference("slides.deck", "d1");
        assert_eq!(
            share_node(room, "record:so.tilo.slides:d1", &deck).map(|node| node.to_string()),
            Some(format!(
                "room:{room}/app:so.tilo.slides/kind:slides.deck/resource:d1"
            )),
            "a whole record"
        );
        assert_eq!(
            share_node(
                room,
                "record:so.tilo.notes:",
                &reference("notes.notebook", "notes.notebook")
            ),
            None,
            "the room's whole notebook stands at no node a share may name"
        );
    }
}
