//! Admission of direct, already committed desktop state from the pinned owner.
//! The private relay authenticates the carrier; StatePeer still verifies every
//! room change, signed record, causal prerequisite, permission and replay floor.
use qos_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use tilo_relay::{
    peer_actions::{ActionMessage, OpenedDesktopTab},
    transport_v2::EnvelopeV2,
};
use web_time::SystemTime;

#[derive(Clone, Debug, PartialEq)]
pub struct CommittedDesktopNotice {
    pub invocation: tilo_actions::InvocationId,
    pub room: uuid::Uuid,
    pub opened: OpenedDesktopTab,
}

pub enum Admission {
    Ignored,
    Ready {
        candidate: StatePeer,
        notice: Option<CommittedDesktopNotice>,
    },
    AwaitingRetainedState(CommittedDesktopNotice),
}

fn contains_opened_route(state: &StatePeer, opened: &OpenedDesktopTab) -> Result<bool, String> {
    let Some(room) = state.admitted_room() else {
        return Ok(false);
    };
    let mut tree = qos_desktop::WorkspaceTree::default();
    qos_desktop::projection::RoomProjection::apply_admitted_room(
        &mut tree,
        qos_desktop::RoomId::parse(state.room()).ok_or("invalid room identity")?,
        room,
    )?;
    Ok(tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .any(|strip| {
            strip.id.0.to_string() == opened.strip
                && strip.window_groups.iter().any(|group| {
                    group.id.to_string() == opened.group
                        && group
                            .tabs
                            .iter()
                            .any(|tab| tab.id.to_string() == opened.tab)
                })
        }))
}

fn notice(
    candidate: &StatePeer,
    action: &tilo_relay::peer_actions::DirectedAction,
) -> Result<Option<CommittedDesktopNotice>, String> {
    let ActionMessage::CommittedDesktop {
        invocation,
        opened: Some(opened),
        ..
    } = &action.message
    else {
        return Ok(None);
    };
    if !contains_opened_route(candidate, opened)? {
        return Err("committed desktop opened route absent from candidate topology".into());
    }
    Ok(Some(CommittedDesktopNotice {
        invocation: *invocation,
        room: action.room,
        opened: opened.clone(),
    }))
}

pub fn admit(
    state: &StatePeer,
    event: &EnvelopeV2,
    recipient: [u8; 32],
    now: SystemTime,
) -> Result<Admission, String> {
    let EnvelopeV2::Delivered {
        action,
        source_user,
    } = event
    else {
        return Ok(Admission::Ignored);
    };
    let ActionMessage::CommittedDesktop { packets, .. } = &action.message else {
        return Ok(Admission::Ignored);
    };
    if !action.validate()
        || action.source != state.authority().device_public_key
        || *source_user != state.authority().user_public_key
        || action.target != recipient
        || action.room.to_string() != state.room()
    {
        return Err("committed desktop carrier identity mismatch".into());
    }
    state.permissions(recipient, now)?;
    state.permissions(action.source, now)?;
    let mut candidate = state.clone();
    for bytes in qos_peer_state::desktop_delivery::decode(packets)? {
        let message: NeighbourMsg = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
        if !matches!(
            message,
            NeighbourMsg::SyncResp { .. } | NeighbourMsg::AppDocument { .. }
        ) {
            return Err("unsupported committed desktop packet".into());
        }
        candidate.receive_from(TransportOrigin::RelayDelta(action.source), message, now)?;
    }
    if let Some(reason) = candidate.incomplete_reason() {
        return Err(reason);
    }
    let complete = crate::desktop::admission(&candidate)?.complete();
    let notice = notice(&candidate, action)?;
    if !complete {
        return notice.map(Admission::AwaitingRetainedState).ok_or_else(|| {
            "committed desktop prerequisites incomplete; awaiting retained state".into()
        });
    }
    Ok(Admission::Ready { candidate, notice })
}
