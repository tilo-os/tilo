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
use tilo_relay_protocol::voice::{CallState, CallSummary, Showing, VoiceGrant};

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
        grant: Box<VoiceGrant>,
    },
    /// The participation ended: the call ended, the device may no longer
    /// take part, or the host refused the join.
    Ended {
        /// The room's workspace.
        workspace_id: u64,
        call: [u8; 32],
        /// Why.
        reason: String,
    },
}

struct Joined {
    call: [u8; 32],
    /// Known once the host granted the join.
    participation: Option<[u8; 32]>,
    joining: bool,
    /// Joins repeated with the granted participation that the host has
    /// not answered yet. The host answers in order, so a refusal while any
    /// are out is the answer to one of them, never to a fresh join: a
    /// page that stood still sends several at once, and only the first
    /// refusal may start the fresh join.
    beats: usize,
    /// What this participation shows besides its voice: said to the host
    /// when it changes, with every heartbeat and after a fresh grant.
    showing: Showing,
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
                beats: 0,
                showing: Showing::default(),
            },
        );
        out.push(Envelope::VoiceJoin {
            workspace_id,
            call,
            participation: None,
        });
        out
    }

    /// Show `showing` besides the voice in the call of `workspace_id` from
    /// now on: what to send, once the participation is granted.
    pub(crate) fn show(&mut self, workspace_id: u64, showing: Showing) -> Option<Envelope> {
        let joined = self.joined.get_mut(&workspace_id)?;
        joined.showing = showing;
        Some(Envelope::VoiceShow {
            workspace_id,
            call: joined.call,
            participation: joined.participation?,
            showing,
        })
    }

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
                // The last connection's unanswered joins are never answered.
                joined.beats = usize::from(!joined.joining);
                Envelope::VoiceJoin {
                    workspace_id: *workspace_id,
                    call: joined.call,
                    participation: joined.participation,
                }
            })
            .collect()
    }

    /// Every granted participation's heartbeat, with what it shows when it
    /// shows anything.
    pub(crate) fn heartbeats(&mut self) -> Vec<Envelope> {
        self.joined
            .iter_mut()
            .filter_map(|(workspace_id, joined)| {
                let participation = joined.participation?;
                joined.beats = joined.beats.saturating_add(1);
                let mut out = vec![Envelope::VoiceJoin {
                    workspace_id: *workspace_id,
                    call: joined.call,
                    participation: Some(participation),
                }];
                if joined.showing != Showing::default() {
                    out.push(Envelope::VoiceShow {
                        workspace_id: *workspace_id,
                        call: joined.call,
                        participation,
                        showing: joined.showing,
                    });
                }
                Some(out)
            })
            .flatten()
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
                    // A fresh participation shows nothing at the host until
                    // told.
                    let fresh = joined.joining && joined.showing != Showing::default();
                    if !joined.joining {
                        joined.beats = joined.beats.saturating_sub(1);
                    }
                    joined.participation = Some(grant.participation);
                    joined.joining = false;
                    (
                        Some(VoiceEvent::Granted {
                            workspace_id: *workspace_id,
                            grant: grant.clone(),
                        }),
                        fresh.then_some(Envelope::VoiceShow {
                            workspace_id: *workspace_id,
                            call: grant.call,
                            participation: grant.participation,
                            showing: joined.showing,
                        }),
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
                // The answer to a join repeated with a participation the
                // host forgot, after the first: the fresh join is out.
                Some(joined) if joined.call == *call && joined.joining && joined.beats > 0 => {
                    joined.beats = joined.beats.saturating_sub(1);
                    (None, None)
                }
                Some(joined) if joined.call == *call && !joined.joining => {
                    joined.beats = joined.beats.saturating_sub(1);
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
        let (event, _) = calls.inbound(&grant([1; 32], [10; 32])).unwrap();
        assert!(
            matches!(event, Some(VoiceEvent::Granted { grant, .. }) if grant.participation == [10; 32])
        );
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
    fn a_page_that_stood_still_joins_afresh_once_for_all_its_refused_beats() {
        let mut calls = Participations::default();
        calls.join(ROOM, [1; 32]);
        calls.inbound(&grant([1; 32], [9; 32]));
        // Three beats out at once; the host forgot the participation.
        for _ in 0..3 {
            assert_eq!(calls.heartbeats().len(), 1);
        }
        let (event, reply) = calls.inbound(&refused([1; 32])).unwrap();
        assert!(event.is_none());
        assert!(matches!(
            reply,
            Some(Envelope::VoiceJoin {
                participation: None,
                ..
            })
        ));
        for _ in 0..2 {
            assert_eq!(
                calls.inbound(&refused([1; 32])),
                Some((None, None)),
                "the answer to an earlier beat, not to the fresh join"
            );
        }
        let (event, _) = calls.inbound(&grant([1; 32], [10; 32])).unwrap();
        assert!(
            matches!(event, Some(VoiceEvent::Granted { grant, .. }) if grant.participation == [10; 32]),
            "the call goes on with a fresh participation"
        );
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
