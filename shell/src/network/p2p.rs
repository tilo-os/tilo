use libp2p::{
    gossipsub, mdns, noise, swarm::NetworkBehaviour, swarm::SwarmEvent, tcp, yamux,
};
use libp2p::futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;
use crate::workspace::UserCursor;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub enum NetworkEvent {
    CursorUpdate(UserCursor),
    WindowMoved { zone_id: String, window_id: String, x: f32, y: f32 },
    ZoneCreated { name: String, x: f32, y: f32 },
}

#[derive(NetworkBehaviour)]
pub struct DesktopBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}

pub struct P2PClient {
    pub swarm: libp2p::Swarm<DesktopBehaviour>,
    pub topic: gossipsub::IdentTopic,
}

impl P2PClient {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut swarm = libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_behaviour(|key| {
                let message_id_fn = |message: &gossipsub::Message| {
                    let mut s = DefaultHasher::new();
                    message.data.hash(&mut s);
                    gossipsub::MessageId::from(s.finish().to_string())
                };

                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    .heartbeat_interval(Duration::from_secs(1))
                    .validation_mode(gossipsub::ValidationMode::Strict)
                    .message_id_fn(message_id_fn)
                    .build()?;

                let gossipsub = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossipsub_config,
                )?;

                let mdns = mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;

                Ok(DesktopBehaviour { gossipsub, mdns })
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        let topic = gossipsub::IdentTopic::new("quick-desktop-global");
        swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

        swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

        Ok(Self { swarm, topic })
    }

    pub fn broadcast(&mut self, event: NetworkEvent) {
        if let Ok(json) = serde_json::to_vec(&event) {
            let _ = self.swarm.behaviour_mut().gossipsub.publish(self.topic.clone(), json);
        }
    }

    pub async fn next_event(&mut self) -> Option<NetworkEvent> {
        loop {
            match self.swarm.select_next_some().await {
                SwarmEvent::Behaviour(DesktopBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    message,
                    ..
                })) => {
                    if let Ok(event) = serde_json::from_slice::<NetworkEvent>(&message.data) {
                        return Some(event);
                    }
                }
                SwarmEvent::Behaviour(DesktopBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, _multiaddr) in list {
                        self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use crate::workspace::Point;

    #[test]
    fn test_network_event_serialization() {
        let cursor = UserCursor {
            id: Uuid::new_v4(),
            name: "TestUser".into(),
            position: Point { x: 10.0, y: 20.0 },
            color: [1.0, 0.0, 0.0],
        };
        let event = NetworkEvent::CursorUpdate(cursor.clone());
        
        let json = serde_json::to_string(&event).unwrap();
        let decoded: NetworkEvent = serde_json::from_str(&json).unwrap();
        
        assert_eq!(event, decoded);
    }
}
