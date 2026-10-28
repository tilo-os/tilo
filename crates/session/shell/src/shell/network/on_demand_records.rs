//! On-demand records a local app reads (`neighbour_wire::record_sync`): an
//! on-demand record, or a part of a chain behind its head. A room's hosts
//! replay a joining device only the records every device follows, and send
//! one of these once the device asks.
//!
//! The first read asks the room's hosts, once; the record's history then
//! arrives as the room's other documents do. Until it is here, a read says
//! whether it is on its way or not: not when no host of the room is
//! connected, or a host said it retains nothing of it. Either changing is
//! announced ([`tilo_identity_host::policy_change::announce`]), so a reader
//! waiting on the read is woken instead of asking again.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{NetCmd, ShellNetwork, room_topic_id};
use crate::shell::state::RoomId;

/// Where an on-demand record a local app reads stands, while it is not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OnDemand {
    /// Asked for; its history is on its way.
    Arriving,
    /// No host of the room is connected, or one said it retains nothing of
    /// it.
    Absent,
}

#[derive(Default)]
pub(super) struct State {
    /// The records asked for, by room topic and channel, with whether a
    /// host said it retains none of it.
    asked: Mutex<HashMap<(u64, [u8; 32]), bool>>,
}

impl ShellNetwork {
    /// Ask `room`'s hosts for `app`'s on-demand `record` the first time a
    /// local app reads it, and say where it stands. Later reads ask nobody.
    pub(crate) fn read_on_demand(&self, room: RoomId, app: &str, record: &str) -> OnDemand {
        let ws = room_topic_id(room);
        let channel = neighbour_wire::app_document_channel(app, record);
        let empty = {
            let Ok(mut asked) = self.on_demand_records.asked.lock() else {
                return OnDemand::Arriving;
            };
            if let Some(empty) = asked.get(&(ws, channel)) {
                *empty
            } else {
                asked.insert((ws, channel), false);
                // A host session that starts later is asked too.
                let _ = self.cmd_tx.try_send(NetCmd::OpenRecord { ws, channel });
                false
            }
        };
        let hosted = self
            .workspace_control_relays
            .lock()
            .is_ok_and(|hosts| hosts.iter().any(|(topic, _)| *topic == ws));
        if empty || !hosted {
            OnDemand::Absent
        } else {
            OnDemand::Arriving
        }
    }

    /// A host of topic `ws` sent everything it retains of the on-demand
    /// record on `channel`, `frames` of it. With none, the record is absent
    /// at the room's hosts; any frames are its history, which arrives as
    /// the room's documents do.
    pub(super) fn on_demand_record_opened(&self, ws: u64, channel: [u8; 32], frames: u32) {
        if frames > 0 {
            return;
        }
        let changed = self.on_demand_records.asked.lock().is_ok_and(|mut asked| {
            asked
                .get_mut(&(ws, channel))
                .is_some_and(|empty| !std::mem::replace(empty, true))
        });
        if changed {
            tilo_identity_host::policy_change::announce();
        }
    }

    /// A host of topic `ws` connected or left: whether an on-demand record
    /// asked for there can arrive changed.
    pub(super) fn on_demand_hosts_changed(&self, ws: u64) {
        let asked_here = self
            .on_demand_records
            .asked
            .lock()
            .is_ok_and(|asked| asked.keys().any(|(topic, _)| *topic == ws));
        if asked_here {
            tilo_identity_host::policy_change::announce();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NetCmd, NetEvent, room_topic_id, shell_network_for_test};
    use super::OnDemand;
    use crate::shell::state::{RoomId, WorkspaceTree};

    /// The first read asks the room's hosts once; later reads ask nobody,
    /// and say where the record stands: absent with no host connected,
    /// arriving once one is, and absent again once a host said it holds
    /// none of it. Each change is announced, so a waiting reader re-reads.
    #[test]
    fn an_on_demand_record_is_asked_for_once_and_says_where_it_stands() {
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (network, mut commands) = shell_network_for_test(tree, 1);
        let room = RoomId::new();
        let ws = room_topic_id(room);
        let record = neighbour_wire::record_part("derived:messages@room", 0);
        let channel = neighbour_wire::app_document_channel("so.tilo.chat", &record);
        let asked = |commands: &mut tokio::sync::mpsc::Receiver<NetCmd>| {
            std::iter::from_fn(|| commands.try_recv().ok())
                .filter(|command| {
                    matches!(command, NetCmd::OpenRecord { ws: at, channel: asked }
                        if *at == ws && *asked == channel)
                })
                .count()
        };

        assert_eq!(
            network.read_on_demand(room, "so.tilo.chat", &record),
            OnDemand::Absent,
            "no host is connected to send it"
        );
        assert_eq!(asked(&mut commands), 1);

        let epoch = tilo_identity_host::policy_change::epoch();
        let host = iroh::SecretKey::generate().public();
        network.dispatch_net_event(NetEvent::HostReady { ws, peer: host });
        assert!(
            tilo_identity_host::policy_change::epoch() > epoch,
            "announced"
        );
        assert_eq!(
            network.read_on_demand(room, "so.tilo.chat", &record),
            OnDemand::Arriving
        );
        assert_eq!(asked(&mut commands), 0, "asked once");

        network.dispatch_net_event(NetEvent::RecordOpened {
            ws,
            channel,
            frames: 3,
        });
        assert_eq!(
            network.read_on_demand(room, "so.tilo.chat", &record),
            OnDemand::Arriving,
            "what the host sent arrives as the room's documents do"
        );
        let epoch = tilo_identity_host::policy_change::epoch();
        network.dispatch_net_event(NetEvent::RecordOpened {
            ws,
            channel,
            frames: 0,
        });
        assert!(
            tilo_identity_host::policy_change::epoch() > epoch,
            "announced"
        );
        assert_eq!(
            network.read_on_demand(room, "so.tilo.chat", &record),
            OnDemand::Absent,
            "the host holds none of it"
        );
        assert_eq!(asked(&mut commands), 0);
    }
}
