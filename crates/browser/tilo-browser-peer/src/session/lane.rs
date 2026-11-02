//! The page's side of a call: the participation it tells the host about and
//! the sealed turns it takes on the host's call lane.
//!
//! A page holds the call's content keys, so everything that names a microphone
//! frame or a picture is sealed here and the host only carries it. The session
//! keeps one [`Page`] for the call it is in, so a turn opens what the call's
//! other devices sent since the last one.

use super::*;

impl PeerHandle {
    /// Ask the host for the calls of `workspace_id`; the answer arrives as
    /// [`VoiceEvent::Calls`](crate::voice::VoiceEvent::Calls).
    pub fn voice_calls(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::VoiceCalls(workspace_id))
    }

    /// Start, end or moderate a call of `workspace_id`; the answer arrives
    /// as [`VoiceEvent::CallResult`](crate::voice::VoiceEvent::CallResult)
    /// for `request`.
    pub fn voice_call(
        &self,
        workspace_id: u64,
        request: [u8; 16],
        operation: tilo_relay_protocol::voice::CallOperation,
    ) -> Result<(), String> {
        self.send(Command::VoiceCall(workspace_id, request, operation))
    }

    /// Take part in `call` of `workspace_id` until [`Self::voice_leave`]:
    /// the session keeps the participation alive across reconnects and
    /// hands on its grants ([`crate::voice`]).
    pub fn voice_join(&self, workspace_id: u64, call: [u8; 32]) -> Result<(), String> {
        self.send(Command::VoiceJoin(workspace_id, call))
    }

    /// Leave the call of `workspace_id`.
    pub fn voice_leave(&self, workspace_id: u64) -> Result<(), String> {
        self.send(Command::VoiceLeave(workspace_id))
    }

    /// Show `showing` besides this session's voice in the call of
    /// `workspace_id` (its camera, a shared screen).
    pub fn voice_show(
        &self,
        workspace_id: u64,
        showing: tilo_relay_protocol::voice::Showing,
    ) -> Result<(), String> {
        self.send(Command::VoiceShow(workspace_id, showing))
    }

    /// Take one turn of `workspace`'s call: the page's microphone frames and
    /// pictures, sealed as the host's device, go to the host that carries the
    /// call, and what the call's devices sent since the last turn comes back
    /// opened ([`LaneTurn`]). `done` receives it.
    ///
    /// Turns are the whole of the page's side of a call: one call at a time,
    /// one turn in flight, sealed and opened with the content keys of the
    /// channel its record is in ([`Self::call_lane_end`] ends it).
    pub fn call_lane_then(
        &self,
        workspace: &str,
        turn: crate::call_media::LaneTurn,
        done: LaneCompletion,
    ) -> Result<(), String> {
        self.send(Command::CallLane(workspace.to_owned(), turn, done))
    }

    /// Leave the call of `workspace` the page was taking turns in: its next
    /// turn is a fresh call, sealed from the start.
    pub fn call_lane_end(&self, workspace: &str) -> Result<(), String> {
        self.send(Command::CallLaneEnd(workspace.to_owned()))
    }
}

impl Task {
    pub(super) fn voice_calls(&self, workspace_id: u64, client: Option<&Link>) -> bool {
        if let Some(client) = client {
            client.send(Envelope::VoiceCalls { workspace_id }).is_ok()
        } else {
            self.emit(&PeerEvent::Voice(crate::voice::VoiceEvent::Calls {
                workspace_id,
                result: Err("not connected to the host".into()),
            }));
            true
        }
    }

    pub(super) fn voice_call(
        &self,
        workspace_id: u64,
        request: [u8; 16],
        operation: tilo_relay_protocol::voice::CallOperation,
        client: Option<&Link>,
    ) -> bool {
        if let Some(client) = client {
            client
                .send(Envelope::VoiceCall {
                    workspace_id,
                    request,
                    operation,
                })
                .is_ok()
        } else {
            self.emit(&PeerEvent::Voice(crate::voice::VoiceEvent::CallResult {
                workspace_id,
                request,
                result: Err("not connected to the host".into()),
            }));
            true
        }
    }

    /// Offline, the next connection sends the join.
    pub(super) fn voice_join(
        &mut self,
        workspace_id: u64,
        call: [u8; 32],
        client: Option<&Link>,
    ) -> bool {
        let out = self.voice.join(workspace_id, call);
        client.is_none_or(|client| {
            out.into_iter()
                .all(|envelope| client.send(envelope).is_ok())
        })
    }

    /// Offline, the next connection's heartbeat says it.
    pub(super) fn voice_show(
        &mut self,
        workspace_id: u64,
        showing: tilo_relay_protocol::voice::Showing,
        client: Option<&Link>,
    ) -> bool {
        match (client, self.voice.show(workspace_id, showing)) {
            (Some(client), Some(show)) => client.send(show).is_ok(),
            _ => true,
        }
    }

    pub(super) fn voice_leave(&mut self, workspace_id: u64, client: Option<&Link>) -> bool {
        let out = self.voice.leave(workspace_id);
        match (client, out) {
            (Some(client), Some(leave)) => client.send(leave).is_ok(),
            _ => true,
        }
    }

    /// Take one turn of the page's call: seal what it handed over as the
    /// device bridging it, post the turn to the host, and open the answer for
    /// the page. The page's side of the call waits in `self.lane` between
    /// turns, and comes back with the answer ([`Command::CallLaneAnswer`]).
    pub(super) fn call_lane(
        &mut self,
        workspace: &str,
        turn: &LaneTurn,
        done: LaneCompletion,
        client: Option<&Link>,
    ) {
        let Some(Link::Https(link)) = client else {
            done(Err("calls are carried over the host's HTTPS session".into()));
            return;
        };
        let channel = turn.channel(workspace);
        let Some(keys) = self.channels.content_keys(channel) else {
            done(Err(
                "this page holds no keys for the call's record".to_owned()
            ));
            return;
        };
        // The device the page's frames name: the host that bridges it, and
        // the id the call's lanes reach it from.
        let bridge = self.config.host_cert.device_public_key;
        let roster = Roster::new([bridge]);
        let mut page = match self.lane.take() {
            Some(held) if held.call() == (workspace, turn.call.as_str()) => held,
            _ => match Page::new(
                workspace.to_owned(),
                turn.call.clone(),
                channel,
                bridge,
                &keys,
                roster.clone(),
            ) {
                Ok(page) => page,
                Err(refusal) => {
                    done(Err(refusal.to_string()));
                    return;
                }
            },
        };
        // The channel's membership moved: the call seals under its new epoch.
        if page.epoch() != keys.epoch()
            && let Err(refusal) = page.set_keys(&keys, roster)
        {
            done(Err(refusal.to_string()));
            return;
        }
        let up = turn.up(&mut page);
        let http = Arc::clone(link.client());
        let session = Some(link.session().to_owned());
        let commands = self.commands.clone();
        n0_future::task::spawn(async move {
            let lane = LaneCall::new(page, http, session);
            let answered = lane.turn(up).await;
            let page = lane.into_page();
            if let Some(commands) = commands.upgrade() {
                let _ = commands.send(Command::CallLaneAnswer(page, answered, done));
            }
        });
    }

    /// Hand the page what its turn brought, opened, and keep its side of the
    /// call for the next one.
    pub(super) fn answered(
        &mut self,
        mut page: Page,
        answered: Result<Down, LinkError>,
        done: LaneCompletion,
    ) {
        let answer = answered
            .map_err(|error| error.to_string())
            .map(|down| LaneAnswer::of(crate::call_media::opened(&mut page, &down)));
        if answer.is_ok() {
            self.lane = Some(page);
        }
        done(answer);
    }
}
