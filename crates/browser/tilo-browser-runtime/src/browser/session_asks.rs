//! What the desktop's session asks of the page for its windows: showing
//! what an open named in the window it opened, and asking the room about
//! nodes of its tree (what a share grants, what the person may change).

use super::{Desktop, Mount};

/// What the desktop's session asked of the page and is not done yet.
pub(super) struct Asks {
    /// What the windows it opened are to show, each until it is shown or
    /// gives up waiting for its window.
    reveals: std::cell::RefCell<Vec<(tilo_desktop_surface::session::Reveal, ui::time::Instant)>>,
    /// The peer replicating the room, through whose rooms door the windows'
    /// asks of nodes of the room's tree go.
    peer: Option<u32>,
    /// Nodes the room answered whether this person may change, waiting to
    /// reach the mounts' permissions.
    permitted: std::sync::Arc<std::sync::Mutex<Vec<(String, bool)>>>,
}

impl Asks {
    pub(super) fn new(peer: Option<u32>) -> Self {
        Self {
            reveals: std::cell::RefCell::default(),
            peer,
            permitted: std::sync::Arc::default(),
        }
    }

    /// The peer replicating the room, if the page has one.
    pub(super) fn peer(&self) -> Option<u32> {
        self.peer
    }
}

impl Desktop {
    /// Ask the room what the windows asked of nodes of its tree: whether
    /// they may change one, or to grant (or take back) what a share of it
    /// gives. The room decides each, as it decides every write the grant
    /// then covers.
    pub(super) fn ask_room_of_nodes(&self) {
        let permitted = std::mem::take(
            &mut *self
                .asks
                .permitted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if let Some(peer) = self.asks.peer {
            for (node, may) in permitted {
                crate::replication::permit(peer, &node, may);
            }
        }
        for ask in self.openings.take_node_asks() {
            let Some(peer) = self.asks.peer else {
                ask.answer(Err("This page shares through no room".into()));
                continue;
            };
            let node = ask.node.clone();
            let action = ask.action.clone();
            let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(ask)));
            let answered = std::sync::Arc::clone(&slot);
            let permitted = std::sync::Arc::clone(&self.asks.permitted);
            let woken = std::sync::Arc::clone(&self.shared.woken);
            let asked_node = node.clone();
            let sent = crate::replication::rooms_then(
                peer,
                |room| node_request(room, node, action),
                Box::new(move |reply| {
                    // What the room permits beyond the person's role (an
                    // item shared with them to edit) reaches their windows.
                    if let Ok(tilo_relay_protocol::rooms::RoomsReply::Permits(may)) = &reply {
                        permitted
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push((asked_node, *may));
                        woken.store(true, std::sync::atomic::Ordering::Release);
                    }
                    let ask = answered
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if let Some(ask) = ask {
                        ask.answer(node_answer(reply));
                    }
                }),
            );
            if let Err(error) = sent
                && let Some(ask) = slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
            {
                ask.answer(Err(error));
            }
        }
    }
}

/// The rooms request asking room `room` `action` of `node`.
fn node_request(
    room: &str,
    node: String,
    action: tilo_desktop_surface::session::NodeAction,
) -> tilo_relay_protocol::rooms::RoomsRequest {
    use tilo_desktop_surface::session::NodeAction;
    use tilo_relay_protocol::rooms::RoomsRequest;
    let room = room.to_owned();
    match action {
        NodeAction::Share { share, access } => RoomsRequest::Share {
            room,
            share,
            node,
            access,
        },
        NodeAction::Unshare { share } => RoomsRequest::Unshare { room, share },
        NodeAction::Permits => RoomsRequest::Permits { room, node },
    }
}

/// A node ask's output from the room's reply.
fn node_answer(
    reply: Result<tilo_relay_protocol::rooms::RoomsReply, String>,
) -> Result<serde_json::Value, String> {
    use tilo_relay_protocol::rooms::RoomsReply;
    match reply? {
        RoomsReply::Permits(may) => Ok(serde_json::json!({ "undoable": may })),
        RoomsReply::Shared(count) => Ok(serde_json::json!({ "grants": count })),
        RoomsReply::Refused(reason) => Err(reason),
        other => Err(format!("the room answered something else: {other:?}")),
    }
}

/// How long an opened window has to come up and show what its open named.
const REVEAL_WAIT: ui::time::Duration = ui::time::Duration::from_secs(20);

impl Mount {
    /// Show, in each window the session opened on a record, the resource
    /// its open named, through the resource's own `open` as the person
    /// would: a note in a notebook rather than the notebook. A window still
    /// coming up (its app's code arriving, its record syncing) is tried
    /// again on the next frame.
    pub(super) fn reveal_opened(&mut self) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        let now = ui::time::now();
        let mut reveals = desktop.asks.reveals.borrow_mut();
        reveals.extend(
            desktop
                .openings
                .take_reveals()
                .into_iter()
                .filter(|reveal| {
                    crate::desktop::Shared::narrower(&reveal.app, &reveal.record, &reveal.resource)
                })
                .map(|reveal| (reveal, now.checked_add(REVEAL_WAIT).unwrap_or(now))),
        );
        if reveals.is_empty() {
            return;
        }
        let frame = &mut self.frame;
        let mut settling = false;
        reveals.retain(|(reveal, until)| {
            let Some(tab) = desktop.shared.tab_on(&reveal.app, &reveal.record) else {
                return *until > now;
            };
            let address = ui::bound_dispatch::BoundActionAddress {
                instance_id: tab,
                address: tilo_actions::ActionAddress::new(
                    &reveal.resource,
                    tilo_actions::remote::APPLICATION_OPEN_ACTION,
                ),
            };
            match frame.invoke_bound_action(&address, &serde_json::Map::new()) {
                Ok(_) => false,
                Err(error) => {
                    if *until <= now {
                        web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
                            "an opened window did not show {}: {error}",
                            reveal.resource
                        )));
                    }
                    // Its window is up but has not published the resource
                    // yet: the next frame publishes it.
                    settling |= *until > now;
                    *until > now
                }
            }
        });
        drop(reveals);
        self.pending |= settling;
    }
}
