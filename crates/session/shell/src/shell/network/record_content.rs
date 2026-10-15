//! The content a record references, on the record's route.
//!
//! A room record's: a device asks the room's topic for a hash its record
//! lacks; a device of the room that keeps it answers that device alone, in
//! bounded pieces, directly when it can reach it and on the room's topic
//! otherwise. Both directions are room traffic: only devices the room admits
//! to read take part, and every message names the device it comes from,
//! bound to its authenticated transport before it is accepted.
//!
//! A person record's: the same exchange on the person's own fabric, between
//! the devices their root certified and nobody else, whichever room the
//! window showing the record is in.

use super::{
    NetCmd, PersonalFabricPayload, ShellNetwork, personal_continuity_topic_id, room_topic_id,
};
use crate::network::actor::RecordContentDelivery;
use crate::shell::state::RoomId;
use neighbour_wire::NeighbourMsg;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tilo_action_broker::record_content::{ContentPeer, ContentPiece};
use tilo_action_broker::route::CrdtRoute;

/// Devices whose transport this device remembers for a direct answer.
const MAX_REMEMBERED_REQUESTERS: usize = 256;
/// Messages waiting for the broker; past it, new ones are dropped and
/// asked again later.
const MAX_QUEUED: usize = 512;

/// A request or answer from another device, for the broker's drainer.
#[derive(Debug, Clone)]
pub enum InboundRecordContent {
    /// `peer` asked the room for `hash`; the room already admitted it to
    /// read.
    Want {
        route: CrdtRoute,
        peer: ContentPeer,
        hash: String,
    },
    /// A piece of content this device asked for.
    Piece {
        route: CrdtRoute,
        piece: ContentPiece,
    },
}

#[derive(Default)]
pub(super) struct State {
    inbound: Arc<Mutex<Vec<InboundRecordContent>>>,
    /// The transport behind each device that asked, per room topic.
    requesters: Mutex<HashMap<(u64, ContentPeer), iroh::EndpointId>>,
}

/// The route a record-content message addresses: the app's records in a
/// room. Content is kept per room and app, not per record.
fn route(app_id: String, cluster_id: String) -> CrdtRoute {
    CrdtRoute::app(app_id, cluster_id)
}

impl ShellNetwork {
    /// Requests and answers from other devices, drained by the broker.
    pub fn inbound_record_content_handle(&self) -> Arc<Mutex<Vec<InboundRecordContent>>> {
        Arc::clone(&self.record_content.inbound)
    }

    /// The room topic of a room record's content, or `None` for a route
    /// that is no shared room.
    fn record_content_topic(&self, route: &CrdtRoute) -> Option<u64> {
        if self.is_local_person_room(&route.cluster_id) {
            return None;
        }
        RoomId::parse(&route.cluster_id)
            .filter(|room| !room.is_nil())
            .map(room_topic_id)
    }

    /// Ask the devices of `route`'s room for `hash`.
    pub fn want_record_content(&self, route: &CrdtRoute, hash: &str) {
        if self.is_local_person_room(&route.cluster_id) {
            self.want_person_content(route, hash);
            return;
        }
        if self.record_content_topic(route).is_none() {
            return;
        }
        let _ = self.publish_message(NeighbourMsg::RecordContentWant {
            requester: self.local_peer_id_hash(),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: hash.to_owned(),
        });
    }

    /// Send `piece` to `peer`, which asked `route`'s room for it.
    pub fn answer_record_content(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece) {
        if self.is_local_person_room(&route.cluster_id) {
            self.answer_person_content(route, peer, piece);
            return;
        }
        let Some(ws) = self.record_content_topic(route) else {
            return;
        };
        let target = self.requester(ws, peer);
        let msg = NeighbourMsg::RecordContentPiece {
            responder: self.local_peer_id_hash(),
            requester: peer,
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            hash: piece.hash,
            total: piece.total,
            offset: piece.offset,
            bytes: piece.bytes,
        };
        if self.join_topic(ws) {
            let _ = self
                .cmd_tx
                .try_send(NetCmd::PublishRecordContent(RecordContentDelivery {
                    ws,
                    target,
                    msg,
                }));
        }
    }

    /// Queue a record-content message the room admitted from `transport`.
    pub(super) fn dispatch_record_content(
        &self,
        ws: u64,
        transport: iroh::EndpointId,
        msg: NeighbourMsg,
    ) {
        let entry = match msg {
            NeighbourMsg::RecordContentWant {
                requester,
                app_id,
                cluster_id,
                hash,
            } => {
                if requester == self.local_peer_hash || self.is_local_person_room(&cluster_id) {
                    return;
                }
                self.remember_requester(ws, requester, transport);
                InboundRecordContent::Want {
                    route: route(app_id, cluster_id),
                    peer: requester,
                    hash,
                }
            }
            NeighbourMsg::RecordContentPiece {
                requester,
                app_id,
                cluster_id,
                hash,
                total,
                offset,
                bytes,
                ..
            } => {
                if requester != self.local_peer_hash || self.is_local_person_room(&cluster_id) {
                    return;
                }
                InboundRecordContent::Piece {
                    route: route(app_id, cluster_id),
                    piece: ContentPiece {
                        hash,
                        total,
                        offset,
                        bytes,
                    },
                }
            }
            _ => return,
        };
        self.queue_record_content(entry);
    }

    fn queue_record_content(&self, entry: InboundRecordContent) {
        if let Ok(mut inbound) = self.record_content.inbound.lock()
            && inbound.len() < MAX_QUEUED
        {
            inbound.push(entry);
        }
        self.drain_wake.poke();
    }

    fn remember_requester(&self, ws: u64, peer: ContentPeer, transport: iroh::EndpointId) {
        if let Ok(mut requesters) = self.record_content.requesters.lock() {
            if requesters.len() >= MAX_REMEMBERED_REQUESTERS
                && !requesters.contains_key(&(ws, peer))
            {
                requesters.clear();
            }
            requesters.insert((ws, peer), transport);
        }
    }

    fn requester(&self, ws: u64, peer: ContentPeer) -> Option<iroh::EndpointId> {
        self.record_content
            .requesters
            .lock()
            .ok()
            .and_then(|requesters| requesters.get(&(ws, peer)).copied())
    }

    /// The person's fabric, when this device has a person and may use it.
    fn person_fabric(&self) -> Option<(u64, [u8; 32])> {
        let user = self.personal_continuity_user?;
        let ws = personal_continuity_topic_id(&user);
        (self.personal_fabric_allowed() && self.join_topic(ws)).then_some((ws, user))
    }

    /// Ask the person's other devices for content of one of their records.
    fn want_person_content(&self, route: &CrdtRoute, hash: &str) {
        let Some((ws, user_public_key)) = self.person_fabric() else {
            return;
        };
        let payload = PersonalFabricPayload::RecordContentWant {
            app_id: route.app_id.clone(),
            hash: hash.to_owned(),
        };
        for target in self.personal_route_targets(ws, &user_public_key) {
            let _ = self.cmd_tx.try_send(NetCmd::PublishPersonalFabricTarget {
                ws,
                user_public_key,
                target,
                payload: payload.clone(),
            });
        }
    }

    /// Send a piece of a person record's content to the device of the
    /// person that asked for it.
    fn answer_person_content(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece) {
        let Some((ws, user_public_key)) = self.person_fabric() else {
            return;
        };
        let Some(target) = self.requester(ws, peer) else {
            return;
        };
        let _ = self.cmd_tx.try_send(NetCmd::PublishPersonalFabricTarget {
            ws,
            user_public_key,
            target,
            payload: PersonalFabricPayload::RecordContentPiece {
                app_id: route.app_id.clone(),
                hash: piece.hash,
                total: piece.total,
                offset: piece.offset,
                bytes: piece.bytes,
            },
        });
    }

    /// Queue a person-record content message another device of the person
    /// sent on the fabric, which already authenticated it as theirs.
    pub(super) fn dispatch_person_content(
        &self,
        ws: u64,
        transport: iroh::EndpointId,
        payload: PersonalFabricPayload,
    ) {
        let Some(room) = self.local_person_room() else {
            return;
        };
        let peer = crate::network::behaviour::peer_liveness_handle(&transport);
        let entry = match payload {
            PersonalFabricPayload::RecordContentWant { app_id, hash } => {
                self.remember_requester(ws, peer, transport);
                InboundRecordContent::Want {
                    route: route(app_id, room.to_string()),
                    peer,
                    hash,
                }
            }
            PersonalFabricPayload::RecordContentPiece {
                app_id,
                hash,
                total,
                offset,
                bytes,
            } => InboundRecordContent::Piece {
                route: route(app_id, room.to_string()),
                piece: ContentPiece {
                    hash,
                    total,
                    offset,
                    bytes,
                },
            },
            _ => return,
        };
        self.queue_record_content(entry);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;
    use std::collections::BTreeMap;
    use tilo_action_broker::record_content::{
        ContentPeers, ContentScope, ContentShelf, PIECE_BYTES, RouteContent,
    };
    use tilo_content::hash_bytes;
    use tokio::sync::mpsc;

    #[derive(Default)]
    struct Shelf(Mutex<BTreeMap<(ContentScope, String), Vec<u8>>>);

    impl ContentShelf for Shelf {
        fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
            self.0
                .lock()
                .ok()?
                .get(&(scope.clone(), hash.to_owned()))
                .cloned()
        }

        fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
            self.0
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .insert((scope.clone(), hash.to_owned()), bytes.to_vec());
            Ok(())
        }
    }

    struct Peers(Arc<ShellNetwork>);

    impl ContentPeers for Peers {
        fn want(&self, route: &CrdtRoute, hash: &str) {
            self.0.want_record_content(route, hash);
        }

        fn answer(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece) {
            self.0.answer_record_content(route, peer, piece);
        }
    }

    /// A record-content service over `net`, keeping its content in memory.
    pub(in crate::shell::network) fn content_over(net: &Arc<ShellNetwork>) -> Arc<RouteContent> {
        RouteContent::new(Arc::new(Shelf::default()), Arc::new(Peers(Arc::clone(net))))
    }

    struct Device {
        net: Arc<ShellNetwork>,
        commands: mpsc::Receiver<NetCmd>,
        content: Arc<RouteContent>,
        transport: iroh::EndpointId,
    }

    fn device(room: RoomId, seed: u8) -> Device {
        let mut tree = WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Slides");
        assert!(tree.set_room(ws, room));
        let transport = iroh::SecretKey::from_bytes(&[seed; 32]).public();
        let (net, commands) = super::super::shell_network_for_test(
            tree.shared(),
            crate::network::behaviour::peer_liveness_handle(&transport),
        );
        let net = Arc::new(net);
        let content = content_over(&net);
        Device {
            net,
            commands,
            content,
            transport,
        }
    }

    /// Everything `from` put on the wire, as `to` receives it.
    fn deliver(from: &mut Device, to: &Device) -> usize {
        let mut delivered = 0_usize;
        while let Ok(command) = from.commands.try_recv() {
            let (ws, msg) = match command {
                NetCmd::Publish { ws, msg } => (ws, msg),
                NetCmd::PublishRecordContent(delivery) => {
                    assert_eq!(
                        delivery.target,
                        Some(to.transport),
                        "an answer goes to the device that asked, directly"
                    );
                    (delivery.ws, delivery.msg)
                }
                _ => continue,
            };
            to.net
                .dispatch_neighbour_msg(ws, from.transport, None, false, msg);
            delivered = delivered.saturating_add(1);
        }
        let inbound = to.net.inbound_record_content_handle();
        assert!(crate::shell::record_content::drain(&inbound, &to.content));
        delivered
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each device stays up for the whole exchange"
    )]
    fn a_record_s_content_crosses_the_room_to_the_device_that_asked() {
        let room = RoomId::parse("6b3f9a10-0000-4000-8000-00000000000c").expect("room");
        let mut laptop = device(room, 3);
        let mut phone = device(room, 4);
        let route = CrdtRoute::record("so.tilo.slides", room.to_string(), "deck");
        let picture: Vec<u8> = (0..PIECE_BYTES * 2 + 17)
            .map(|index| u8::try_from(index % 251).expect("byte"))
            .collect();
        let hash = hash_bytes(&picture);
        laptop
            .content
            .offer(&route, &hash, &picture)
            .expect("the laptop keeps its picture");
        let mut arrivals = phone.content.subscribe(&route);

        phone.content.want(&route, &hash);
        assert_eq!(deliver(&mut phone, &laptop), 1, "one request");
        assert_eq!(deliver(&mut laptop, &phone), 3, "three pieces");
        let arrival = arrivals.try_recv().expect("the picture arrived");
        assert_eq!(arrival.hash, hash);
        assert_eq!(arrival.bytes, picture);

        // Pieces meant for another device are not gathered here.
        let mut tablet = device(room, 5);
        let mut tablet_arrivals = tablet.content.subscribe(&route);
        tablet.content.want(&route, &hash);
        let _ = tablet.commands.try_recv();
        phone.content.want(&route, &hash);
        assert_eq!(deliver(&mut phone, &laptop), 0, "the phone keeps it now");
        laptop
            .content
            .peer_wants(&route, phone.net.local_peer_id_hash(), true, &hash);
        let mut pieces = Vec::new();
        while let Ok(NetCmd::PublishRecordContent(delivery)) = laptop.commands.try_recv() {
            pieces.push(delivery.msg);
        }
        assert_eq!(pieces.len(), 3);
        for msg in pieces {
            tablet.net.dispatch_neighbour_msg(
                room_topic_id(room),
                laptop.transport,
                None,
                false,
                msg,
            );
        }
        let inbound = tablet.net.inbound_record_content_handle();
        assert!(
            inbound.lock().expect("queue").is_empty(),
            "nothing to gather"
        );
        assert!(tablet_arrivals.try_recv().is_err());
    }
}
