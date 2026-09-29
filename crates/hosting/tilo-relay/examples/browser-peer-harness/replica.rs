//! Application state owner used by the native test peer.
use anyhow::{Result, anyhow};
use ed25519_dalek::SigningKey;
use tilo_room::{NeighbourMsg, StatePeer, TransportOrigin};
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::SystemTime;

pub fn seeded(
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
    key: &SigningKey,
) -> Result<StatePeer> {
    let now = SystemTime::now();
    let mut peer = StatePeer::new(
        room,
        authority.clone(),
        roster.to_vec(),
        RevocationList::new(),
        now,
    )
    .map_err(|e| anyhow!(e))?;
    let seed =
        tilo_browser_runtime::fixture_seed(room, authority, roster).map_err(|e| anyhow!(e))?;
    peer.install_seed(seed, key, now).map_err(|e| anyhow!(e))?;
    Ok(peer)
}

pub fn handle(
    peer: &mut StatePeer,
    key: &SigningKey,
    origin: TransportOrigin,
    message: NeighbourMsg,
) -> Result<Vec<NeighbourMsg>> {
    let request = matches!(&message, NeighbourMsg::SyncReq { room, .. } if room == peer.room());
    let app = matches!(&message, NeighbourMsg::AppDocument { .. });
    let outcome = peer
        .receive_from(origin, message, SystemTime::now())
        .map_err(|e| anyhow!(e))?;
    if request || (app && outcome.changed) {
        peer.bootstrap_messages(key, SystemTime::now())
            .map_err(|e| anyhow!(e))
    } else {
        Ok(vec![])
    }
}

pub fn publish(
    session: &tilo_relay::RelayCoeditSession,
    messages: Vec<NeighbourMsg>,
) -> Result<()> {
    use tilo_room::transport::{Retention, retention};
    for message in messages {
        let body = postcard::to_stdvec(&message)?;
        match retention(&message, true) {
            Some((channel, Retention::Checkpoint)) => session.publish_checkpoint(channel, body)?,
            Some((channel, Retention::Delta)) => session.publish_retained_delta(channel, body)?,
            None => session.publish_presence(body)?,
        }
    }
    Ok(())
}

pub fn status(peer: &StatePeer, error: Option<&str>) -> serde_json::Value {
    serde_json::json!({"application_state":if error.is_some(){"incomplete"}else{"admitted"}, "room":peer.room(), "records":peer.rows(), "error":error})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrol;
    use tilo_user_identity::{DeviceCapabilities, UserIdentity};

    #[test]
    fn native_seed_exposes_the_same_two_canonical_records_to_a_fresh_peer() {
        let (root, _) =
            UserIdentity::generate("fixture test", tilo_delivery_protocol::clock::now_ns());
        let authority = enrol(&root, "native", DeviceCapabilities::primary()).unwrap();
        let browser = enrol(&root, "browser", DeviceCapabilities::member()).unwrap();
        let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let roster = vec![authority.cert.clone(), browser.cert];
        let native = seeded(room, &authority.cert, &roster, &authority.key).unwrap();
        assert_eq!(native.rows().len(), 2);
        assert!(native.has_governance());
        assert_eq!(
            native
                .rows()
                .iter()
                .filter(|row| row.record_id.is_empty())
                .count(),
            1
        );
    }
    #[test]
    fn native_loop_admits_browser_writer_change_then_bootstraps_fresh_peer() {
        let (root, _) =
            UserIdentity::generate("fixture test", tilo_delivery_protocol::clock::now_ns());
        let authority = enrol(&root, "native", DeviceCapabilities::primary()).unwrap();
        let browser = enrol(&root, "browser", DeviceCapabilities::member()).unwrap();
        let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let roster = vec![authority.cert.clone(), browser.cert.clone()];
        let mut native = seeded(room, &authority.cert, &roster, &authority.key).unwrap();
        let mut client = StatePeer::new(
            room,
            authority.cert.clone(),
            roster.clone(),
            RevocationList::new(),
            SystemTime::now(),
        )
        .unwrap();
        let request = NeighbourMsg::SyncReq {
            since: vec![],
            room: room.into(),
            invite_id: None,
        };
        let batch = handle(
            &mut native,
            &authority.key,
            TransportOrigin::UntrustedPresence(browser.cert.device_public_key),
            request,
        )
        .unwrap();
        for message in batch {
            client
                .receive_from(
                    TransportOrigin::RelayDelta(authority.cert.device_public_key),
                    message,
                    SystemTime::now(),
                )
                .unwrap();
        }
        let before = native.rows();
        let change = tilo_browser_runtime::fixture_writer_insert(
            &mut client,
            &browser.key,
            "browser edit",
            SystemTime::now(),
        )
        .unwrap();
        let batch = handle(
            &mut native,
            &authority.key,
            TransportOrigin::RelayDelta(browser.cert.device_public_key),
            change.clone(),
        )
        .unwrap();
        assert_ne!(before, native.rows());
        assert_eq!(native.rows(), client.rows());
        let mut fresh = StatePeer::new(
            room,
            authority.cert.clone(),
            roster,
            RevocationList::new(),
            SystemTime::now(),
        )
        .unwrap();
        for message in batch {
            fresh
                .receive_from(
                    TransportOrigin::RelayDelta(authority.cert.device_public_key),
                    message,
                    SystemTime::now(),
                )
                .unwrap();
        }
        assert_eq!(native.rows(), fresh.rows());
        assert!(
            handle(
                &mut native,
                &authority.key,
                TransportOrigin::RelayDelta(browser.cert.device_public_key),
                change
            )
            .unwrap()
            .is_empty()
        );
    }
}
