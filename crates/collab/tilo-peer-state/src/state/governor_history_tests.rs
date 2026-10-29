//! A replica that wrote under its governor's key (a device whose own host
//! governs its room, written by both) numbers its changes as the governor
//! does, and refuses the governor's every write from then on. It takes the
//! governor's history back, and admits the governor's writes again.
use super::*;
use tilo_action_policy::roles::{self, AgentPreset};

/// Whether `peer` lets `app` author its room, for the governor's own device.
fn authors(peer: &StatePeer, app: &str, now: SystemTime) -> bool {
    peer.permissions(peer.authority().device_public_key, now)
        .unwrap()
        .policy
        .app_authors(app, &tilo_action_policy::Context::at_time(now))
}

fn changes(peer: &StatePeer) -> Vec<Vec<u8>> {
    match peer.room_message() {
        NeighbourMsg::SyncResp { changes, .. } => changes,
        _ => unreachable!("a room message is the room's changes"),
    }
}

#[test]
fn a_replica_written_under_the_governors_key_takes_the_governors_history_back() {
    let (mut host, key, _, _, now) = crate::admission_tests::seeded();
    let mut replica = host.clone();
    let governor = key.verifying_key().to_bytes();
    let owner = Subject::Person(hex::encode(host.authority().user_public_key));
    let room = host.room().to_owned();
    assert!(!replica.diverges_from_governor(&changes(&host)));

    // The replica admits an agent under the governor's key; the host,
    // meanwhile, lets an app author the room.
    let agent = AgentPreset::Contributor.grants(&room, "claude", &owner);
    assert!(
        replica
            .governance_mut()
            .put_governed_grants(&agent, &key, now)
            .unwrap()
    );
    let tasks = roles::app_authorship(&room, "so.tilo.tasks", owner.clone());
    assert!(
        host.governance_mut()
            .put_governed_grants(&[tasks], &key, now)
            .unwrap()
    );
    assert!(
        replica
            .receive_from(
                TransportOrigin::RelayDelta(governor),
                host.room_message(),
                now
            )
            .is_err(),
        "the two histories number their changes alike"
    );
    assert!(!authors(&replica, "so.tilo.tasks", now));

    // It takes the governor's history: the app authors the room here too,
    // and the agent the governor never admitted is not the room's.
    assert!(replica.diverges_from_governor(&changes(&host)));
    let beyond = replica
        .adopt_governor_history(&changes(&host), now)
        .unwrap()
        .expect("the replica takes the governor's history");
    assert!(!replica.diverges_from_governor(&changes(&host)));
    assert!(authors(&replica, "so.tilo.tasks", now));
    // What the replica's own history made of the membership comes back,
    // as it was made, for the governor to commit: the agent's grants, and
    // nothing the governor wrote.
    let mut asked: Vec<tilo_action_policy::Grant> = beyond
        .iter()
        .map(|change| match change {
            tilo_action_policy::Change::Grant(grant) => grant.clone(),
            other => panic!("only the agent's grants were made here: {other:?}"),
        })
        .collect();
    let mut made = agent.clone();
    asked.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    made.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    assert_eq!(asked, made);
    assert!(
        replica
            .adopt_governor_history(&changes(&host), now)
            .unwrap()
            .is_none(),
        "a replica on the governor's history keeps its own"
    );
    // The governor commits them again; the replica admits that.
    let before = host.room_heads().unwrap_or_default();
    assert!(
        host.governance_mut()
            .put_governed_grants(&asked, &key, now)
            .unwrap()
    );
    replica
        .receive_from(
            TransportOrigin::RelayDelta(governor),
            NeighbourMsg::SyncResp {
                changes: host.room_changes_since(&before),
                room: room.clone(),
            },
            now,
        )
        .expect("the governor's write is admitted");
    assert!(
        replica
            .permissions(replica.authority().device_public_key, now)
            .unwrap()
            .policy
            .grants
            .iter()
            .any(|grant| grant.subject == Subject::agent("claude")),
        "the invitation is the room's again"
    );

    // The governor's next write is admitted again.
    let notes = roles::app_authorship(&room, "so.tilo.notes", owner);
    let before = host.room_heads().unwrap_or_default();
    assert!(
        host.governance_mut()
            .put_governed_grants(&[notes], &key, now)
            .unwrap()
    );
    replica
        .receive_from(
            TransportOrigin::RelayDelta(governor),
            NeighbourMsg::SyncResp {
                changes: host.room_changes_since(&before),
                room: room.clone(),
            },
            now,
        )
        .expect("the governor's write is admitted");
    assert!(authors(&replica, "so.tilo.notes", now));
}
