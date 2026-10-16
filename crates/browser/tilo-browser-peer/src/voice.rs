//! This session's part in the calls of the rooms it joined.
//!
//! The host governs each call (see [`tilo_relay_protocol::voice`]); the
//! session keeps at most one participation per room alive for its consumer:
//! it joins, repeats the join with its participation every
//! [`HEARTBEAT`](tilo_relay_protocol::voice::HEARTBEAT) and after every
//! reconnect, joins afresh once when the host forgot the participation
//! (a reconnect after [`PARTICIPATION_TTL`](tilo_relay_protocol::voice::PARTICIPATION_TTL)),
//! and leaves. Each grant the host sends is handed on as a
//! [`VoiceEvent::Granted`]: the token for the voice carrier's admission
//! preface, renewed by the host before it expires, and the call's roster.

use std::collections::HashMap;

use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{CallState, CallSummary, VoiceGrant};

/// What the host said about this device's calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceEvent {
    /// The answer to [`crate::PeerHandle::voice_calls`].
    Calls {
        /// The room's workspace.
        workspace_id: u64,
        /// Its calls, or why it lists none.
        result: Result<Vec<CallSummary>, String>,
    },
    /// The answer to [`crate::PeerHandle::voice_call`] `request`.
    CallResult {
        /// The room's workspace.
        workspace_id: u64,
        /// The request's id.
        request: [u8; 16],
        /// The call now, or why the change was refused.
        result: Result<CallState, String>,
    },
    /// The participation's current grant: the first after a join, then
    /// whenever the roster, the directions or the token change.
    Granted {
        /// The room's workspace.
        workspace_id: u64,
        /// The grant.
        grant: Box<VoiceGrant>,
    },
    /// The participation ended: the call ended, the device may no longer
    /// take part, or the host refused the join.
    Ended {
        /// The room's workspace.
        workspace_id: u64,
        /// The call.
        call: [u8; 32],
        /// Why.
        reason: String,
    },
}

struct Joined {
    call: [u8; 32],
    /// Known once the host granted the join.
    participation: Option<[u8; 32]>,
    /// A fresh join is on its way to the host.
    joining: bool,
}

/// The participations of this session, one per room at most.
#[derive(Default)]
pub(crate) struct Participations {
    joined: HashMap<u64, Joined>,
}

impl Participations {
    /// Join `call` of `workspace_id`, leaving the room's previous call.
    /// Returns what to send, in order.
    pub(crate) fn join(&mut self, workspace_id: u64, call: [u8; 32]) -> Vec<Envelope> {
        if self
            .joined
            .get(&workspace_id)
            .is_some_and(|joined| joined.call == call)
        {
            return Vec::new();
        }
        let mut out: Vec<Envelope> = self.leave(workspace_id).into_iter().collect();
        self.joined.insert(
            workspace_id,
            Joined {
                call,
                participation: None,
                joining: true,
            },
        );
        out.push(Envelope::VoiceJoin {
            workspace_id,
            call,
            participation: None,
        });
        out
    }

    /// Leave the call of `workspace_id`, if any.
    pub(crate) fn leave(&mut self, workspace_id: u64) -> Option<Envelope> {
        let joined = self.joined.remove(&workspace_id)?;
        Some(Envelope::VoiceLeave {
            workspace_id,
            call: joined.call,
            participation: joined.participation?,
        })
    }

    /// The joins a new connection repeats: the heartbeat of every granted
    /// participation, and a fresh join for one the host never answered.
    pub(crate) fn resume(&mut self) -> Vec<Envelope> {
        self.joined
            .iter_mut()
            .map(|(workspace_id, joined)| {
                joined.joining = joined.participation.is_none();
                Envelope::VoiceJoin {
                    workspace_id: *workspace_id,
                    call: joined.call,
                    participation: joined.participation,
                }
            })
            .collect()
    }

    /// Every granted participation's heartbeat.
    pub(crate) fn heartbeats(&self) -> Vec<Envelope> {
        self.joined
            .iter()
            .filter_map(|(workspace_id, joined)| {
                Some(Envelope::VoiceJoin {
                    workspace_id: *workspace_id,
                    call: joined.call,
                    participation: Some(joined.participation?),
                })
            })
            .collect()
    }

    /// Take a call envelope from the host: what to hand the consumer and
    /// what to answer. Envelopes about other things are left alone.
    pub(crate) fn inbound(
        &mut self,
        envelope: &Envelope,
    ) -> Option<(Option<VoiceEvent>, Option<Envelope>)> {
        Some(match envelope {
            Envelope::VoiceCallList {
                workspace_id,
                result,
            } => (
                Some(VoiceEvent::Calls {
                    workspace_id: *workspace_id,
                    result: result.clone(),
                }),
                None,
            ),
            Envelope::VoiceCallResult {
                workspace_id,
                request,
                result,
            } => (
                Some(VoiceEvent::CallResult {
                    workspace_id: *workspace_id,
                    request: *request,
                    result: result.clone(),
                }),
                None,
            ),
            Envelope::VoiceGranted {
                workspace_id,
                grant,
            } => match self.joined.get_mut(workspace_id) {
                Some(joined)
                    if joined.call == grant.call
                        && (joined.joining
                            || joined.participation == Some(grant.participation)) =>
                {
                    joined.participation = Some(grant.participation);
                    joined.joining = false;
                    (
                        Some(VoiceEvent::Granted {
                            workspace_id: *workspace_id,
                            grant: grant.clone(),
                        }),
                        None,
                    )
                }
                // A grant for a participation this session left or replaced.
                _ => (
                    None,
                    Some(Envelope::VoiceLeave {
                        workspace_id: *workspace_id,
                        call: grant.call,
                        participation: grant.participation,
                    }),
                ),
            },
            Envelope::VoiceRefused {
                workspace_id,
                call,
                reason,
            } => match self.joined.get_mut(workspace_id) {
                Some(joined) if joined.call == *call && !joined.joining => {
                    // The host forgot the participation (or ended it): join
                    // afresh once; a refused fresh join ends it.
                    joined.participation = None;
                    joined.joining = true;
                    (
                        None,
                        Some(Envelope::VoiceJoin {
                            workspace_id: *workspace_id,
                            call: *call,
                            participation: None,
                        }),
                    )
                }
                Some(joined) if joined.call == *call => {
                    self.joined.remove(workspace_id);
                    (
                        Some(VoiceEvent::Ended {
                            workspace_id: *workspace_id,
                            call: *call,
                            reason: reason.clone(),
                        }),
                        None,
                    )
                }
                _ => (None, None),
            },
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_relay_protocol::voice::Directions;

    const ROOM: u64 = 7;

    fn grant(call: [u8; 32], participation: [u8; 32]) -> Envelope {
        Envelope::VoiceGranted {
            workspace_id: ROOM,
            grant: Box::new(VoiceGrant {
                call,
                epoch: 1,
                participation,
                token: vec![1; 32],
                endpoint: "https://host/moq/voice".into(),
                fingerprints: Vec::new(),
                directions: Directions {
                    publish: true,
                    subscribe: true,
                },
                publication: None,
                subscriptions: Vec::new(),
                participants: Vec::new(),
                expires_at_ns: 0,
            }),
        }
    }

    fn refused(call: [u8; 32]) -> Envelope {
        Envelope::VoiceRefused {
            workspace_id: ROOM,
            call,
            reason: "the participation ended; join afresh".into(),
        }
    }

    #[test]
    fn a_granted_participation_beats_and_resumes_with_its_id() {
        let mut calls = Participations::default();
        assert!(matches!(
            calls.join(ROOM, [1; 32]).as_slice(),
            [Envelope::VoiceJoin {
                participation: None,
                ..
            }]
        ));
        assert!(calls.heartbeats().is_empty(), "no second fresh join");
        let (event, reply) = calls.inbound(&grant([1; 32], [9; 32])).unwrap();
        assert!(matches!(event, Some(VoiceEvent::Granted { .. })));
        assert!(reply.is_none());
        for beat in [calls.heartbeats(), calls.resume()] {
            assert!(matches!(
                beat.as_slice(),
                [Envelope::VoiceJoin {
                    participation: Some([9, ..]),
                    ..
                }]
            ));
        }
        assert!(matches!(
            calls.leave(ROOM),
            Some(Envelope::VoiceLeave {
                participation: [9, ..],
                ..
            })
        ));
        assert!(calls.heartbeats().is_empty());
    }

    #[test]
    fn a_forgotten_participation_joins_afresh_once_then_ends() {
        let mut calls = Participations::default();
        calls.join(ROOM, [1; 32]);
        calls.inbound(&grant([1; 32], [9; 32]));
        let (event, reply) = calls.inbound(&refused([1; 32])).unwrap();
        assert!(event.is_none());
        assert!(matches!(
            reply,
            Some(Envelope::VoiceJoin {
                participation: None,
                ..
            })
        ));
        // The fresh join is granted a new participation, which replaces it.
        let (event, _) = calls.inbound(&grant([1; 32], [10; 32])).unwrap();
        assert!(
            matches!(event, Some(VoiceEvent::Granted { grant, .. }) if grant.participation == [10; 32])
        );
        // A late grant for the old one is left at the host.
        let (event, reply) = calls.inbound(&grant([1; 32], [9; 32])).unwrap();
        assert!(event.is_none());
        assert!(matches!(
            reply,
            Some(Envelope::VoiceLeave {
                participation: [9, ..],
                ..
            })
        ));
        calls.inbound(&refused([1; 32]));
        let (event, reply) = calls.inbound(&refused([1; 32])).unwrap();
        assert!(matches!(event, Some(VoiceEvent::Ended { .. })));
        assert!(reply.is_none());
        assert!(calls.resume().is_empty());
    }

    #[test]
    fn joining_another_call_leaves_the_first() {
        let mut calls = Participations::default();
        calls.join(ROOM, [1; 32]);
        calls.inbound(&grant([1; 32], [9; 32]));
        assert!(calls.join(ROOM, [1; 32]).is_empty(), "already in it");
        assert!(matches!(
            calls.join(ROOM, [2; 32]).as_slice(),
            [
                Envelope::VoiceLeave { call: [1, ..], .. },
                Envelope::VoiceJoin {
                    call: [2, ..],
                    participation: None,
                    ..
                }
            ]
        ));
    }
}
