use super::{Arc, CallTarget, Command, NativeWorkspaceOwner, Operation, Reply, Ticket, Weak};
use crate::network::actor::{HostIdentity, native_ingress::NOT_CONNECTED};
use crate::shell::{native_workspace_owner::NativeCommittedView, state::RoomId};
use runtime_collab::native_media::Roster;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tilo_joined_room::voice::Channel;
use tilo_relay_protocol::voice::CallSummary;
use tilo_relay_session::coedit::{RelayCoeditSession, VoiceEvent};

/// How often a room is asked for its calls while someone looks at them.
const ROSTER_REFRESH: Duration = Duration::from_secs(1);

/// The relay session of a joined room's workspace, when it is connected.
pub(crate) type Sessions<'a> = &'a dyn Fn(u64) -> Option<Arc<RelayCoeditSession>>;

#[derive(Default)]
pub(crate) struct Registry {
    tickets: Vec<Weak<Ticket>>,
    /// Each room's calls, by its relay workspace, as last listed.
    listed: HashMap<u64, Listed>,
    /// Woken whenever a room's call list changes: the call services that
    /// asked for rosters, so a voice channel shows a join or leave at once.
    signals: Vec<Weak<tokio::sync::Notify>>,
}
#[derive(Default)]
struct Listed {
    asked: Option<Instant>,
    calls: Vec<CallSummary>,
}
impl Registry {
    pub(crate) fn handle(
        &mut self,
        command: Command,
        identity: Option<&HostIdentity>,
        sessions: Sessions<'_>,
    ) {
        if command.reply.is_closed() {
            return;
        }
        self.tickets.retain(|ticket| ticket.strong_count() != 0);
        let result = match identity {
            None => Err("native device identity unavailable".into()),
            Some(identity) => match command.operation {
                Operation::Open { owner, target } => self.open(owner, &target, identity, sessions),
                Operation::OpenWindows { owner, room } => {
                    self.open_windows(owner, room, identity, sessions)
                }
                Operation::Heartbeat { ticket } => self
                    .known(&ticket)
                    .and_then(|()| {
                        let session = sessions(ticket.workspace()).ok_or(NOT_CONNECTED)?;
                        ticket.heartbeat(&session, |view| check_identity(view, identity))
                    })
                    .map(|()| Reply::Sent)
                    .inspect_err(|_| ticket.stop()),
                Operation::Show { ticket, showing } => self
                    .known(&ticket)
                    .and_then(|()| ticket.show(sessions(ticket.workspace()).as_deref(), showing))
                    .map(|()| Reply::Sent),
                Operation::End { ticket } => self.known_pointer(&ticket).map(|()| {
                    ticket.leave(sessions(ticket.workspace()).as_deref());
                    Reply::Left
                }),
                Operation::Rosters {
                    owner,
                    target,
                    signal,
                } => {
                    self.signals.retain(|known| known.strong_count() != 0);
                    if !self
                        .signals
                        .iter()
                        .any(|known| std::ptr::eq(known.as_ptr(), Arc::as_ptr(&signal)))
                    {
                        self.signals.push(Arc::downgrade(&signal));
                    }
                    self.rosters(&owner, &target, identity, sessions)
                }
            },
        };
        // Dropping a successful Open reply also drops the only strong ticket.
        let _ = command.reply.send(result);
    }
    /// Apply what the governor of workspace `ws` said about its calls.
    pub(crate) fn voice(
        &mut self,
        ws: u64,
        event: &VoiceEvent,
        session: Option<&RelayCoeditSession>,
    ) {
        if let VoiceEvent::Calls(result) = event {
            let calls = result.clone().unwrap_or_default();
            let listed = self.listed.entry(ws).or_default();
            if listed.calls != calls {
                listed.calls = calls;
                for signal in self.signals.iter().filter_map(Weak::upgrade) {
                    signal.notify_waiters();
                }
            }
            return;
        }
        self.tickets.retain(|ticket| ticket.strong_count() != 0);
        let tickets: Vec<_> = self
            .tickets
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|ticket| ticket.workspace() == ws)
            .collect();
        for ticket in &tickets {
            let claimed = |id: [u8; 32]| {
                tickets
                    .iter()
                    .any(|other| !Arc::ptr_eq(other, ticket) && other.id() == Some(id))
            };
            if ticket.apply(event, session, claimed) {
                break;
            }
        }
    }
    /// The room on `ws` was dialled again: its calls join over `session`.
    pub(crate) fn reconnected(&mut self, ws: u64, session: &RelayCoeditSession) {
        // The room tells a connection its call list only once asked over it.
        if let Some(listed) = self.listed.get_mut(&ws) {
            listed.asked = None;
        }
        self.tickets.retain(|ticket| ticket.strong_count() != 0);
        for ticket in self.tickets.iter().filter_map(Weak::upgrade) {
            if ticket.workspace() == ws
                && let Err(error) = ticket.rejoin(session)
            {
                tracing::debug!(%error, ws, "native call could not rejoin");
            }
        }
    }
    fn known(&self, ticket: &Arc<Ticket>) -> Result<(), String> {
        ticket.live()?;
        if ticket.leaving() {
            return Err("native participation is leaving".into());
        }
        self.known_pointer(ticket)
    }
    fn known_pointer(&self, ticket: &Arc<Ticket>) -> Result<(), String> {
        if !self
            .tickets
            .iter()
            .any(|known| known.ptr_eq(&Arc::downgrade(ticket)))
        {
            return Err("foreign native media ticket".into());
        }
        Ok(())
    }
    /// The rosters of `target`'s record as last listed, asking the room
    /// again when that was more than [`ROSTER_REFRESH`] ago.
    fn rosters(
        &mut self,
        owner: &NativeWorkspaceOwner,
        target: &CallTarget,
        identity: &HostIdentity,
        sessions: Sessions<'_>,
    ) -> Result<Reply, String> {
        let room = RoomId::parse(&target.workspace).ok_or("invalid native room route")?;
        let workspace = owner.with_current(room, |view| {
            check_identity(view, identity)?;
            Ok(view.workspace())
        })?;
        let listed = self.listed.entry(workspace).or_default();
        let now = Instant::now();
        if listed
            .asked
            .is_none_or(|asked| now.saturating_duration_since(asked) >= ROSTER_REFRESH)
            && let Some(session) = sessions(workspace)
            && session.voice_calls().is_ok()
        {
            listed.asked = Some(now);
        }
        Ok(Reply::Rosters(rosters_of(
            &listed.calls,
            &target.app,
            &target.record,
        )))
    }
    fn open(
        &mut self,
        owner: NativeWorkspaceOwner,
        target: &CallTarget,
        identity: &HostIdentity,
        sessions: Sessions<'_>,
    ) -> Result<Reply, String> {
        if self.tickets.len() >= 16 {
            return Err("native media capacity".into());
        }
        let room = RoomId::parse(&target.workspace).ok_or("invalid native room route")?;
        let workspace = owner.with_current(room, |view| {
            check_identity(view, identity)?;
            Ok(view.workspace())
        })?;
        let session = sessions(workspace).ok_or(NOT_CONNECTED)?;
        let ticket = Ticket::open(
            owner,
            room,
            &Channel {
                app: target.app.clone(),
                record: target.record.clone(),
                channel: target.channel.clone(),
            },
            &session,
            |view| check_identity(view, identity),
        )?;
        self.tickets.push(Arc::downgrade(&ticket));
        Ok(Reply::Opened(ticket))
    }
}

impl Registry {
    /// Join `room`'s window carrier as this device.
    fn open_windows(
        &mut self,
        owner: NativeWorkspaceOwner,
        room: RoomId,
        identity: &HostIdentity,
        sessions: Sessions<'_>,
    ) -> Result<Reply, String> {
        if self.tickets.len() >= 16 {
            return Err("native media capacity".into());
        }
        let workspace = owner.with_current(room, |view| {
            check_identity(view, identity)?;
            Ok(view.workspace())
        })?;
        let session = sessions(workspace).ok_or(NOT_CONNECTED)?;
        let ticket =
            Ticket::open_windows(owner, room, &session, |view| check_identity(view, identity))?;
        self.tickets.push(Arc::downgrade(&ticket));
        Ok(Reply::Opened(ticket))
    }
}

fn check_identity(view: &NativeCommittedView, identity: &HostIdentity) -> Result<(), String> {
    let now = tilo_clock::try_unix_now_ns().ok_or("native clock precedes epoch")?;
    let (cert, entries) = identity.current_native_material(view.workspace())?;
    if &cert != view.device()
        || identity.device_signing_key.verifying_key().to_bytes() != cert.device_public_key
    {
        return Err("native selected device custody changed".into());
    }
    let mut revoked = tilo_user_identity::RevocationList::new();
    for entry in entries {
        revoked.insert(entry).map_err(|error| error.to_string())?;
    }
    if revoked.is_revoked_at(&cert.device_public_key, now) {
        return Err("native selected device revoked".into());
    }
    Ok(())
}

/// Who is in the active calls of record `record` of app `app`, one roster
/// per voice channel with anyone in it or a bridge on it, sorted by
/// channel; each person once, with whose camera is on and who shares a
/// screen (on any of their devices), who is muted or deafened (on every
/// device they are in the call on), and the people a bridge carries in as
/// its guests.
pub(super) fn rosters_of(calls: &[CallSummary], app: &str, record: &str) -> Vec<Roster> {
    use runtime_collab::native_media::Guest;
    use std::collections::{BTreeMap, BTreeSet};
    #[derive(Default)]
    struct Channel {
        people: BTreeSet<String>,
        cameras: BTreeSet<String>,
        screens: BTreeSet<String>,
        guests: BTreeMap<String, Guest>,
        bridges: BTreeSet<String>,
        /// Heard on some device: not muted, whatever their others say.
        unmuted: BTreeSet<String>,
        /// Hearing the call on some device: not deafened.
        hearing: BTreeSet<String>,
    }
    let mut by_channel: BTreeMap<&str, Channel> = BTreeMap::new();
    for call in calls
        .iter()
        .filter(|call| call.state.active && call.app_id == app && call.record_id == record)
    {
        let channel = by_channel.entry(call.channel_id.as_str()).or_default();
        for participant in &call.participants {
            if let Some(bridged) = &participant.bridged {
                channel.bridges.insert(bridged.service.clone());
                if let Some(id) = super::authority::call_id(participant) {
                    channel.guests.entry(id.clone()).or_insert_with(|| Guest {
                        id,
                        name: bridged.name.clone(),
                        avatar: bridged.avatar.clone(),
                        service: bridged.service.clone(),
                        linked: bridged.linked.map(hex::encode),
                        muted: participant.showing.muted,
                    });
                }
                continue;
            }
            let person = hex::encode(participant.user_public_key);
            if participant.showing.camera {
                channel.cameras.insert(person.clone());
            }
            if participant.showing.screen {
                channel.screens.insert(person.clone());
            }
            if !participant.showing.muted {
                channel.unmuted.insert(person.clone());
            }
            if !participant.showing.deafened {
                channel.hearing.insert(person.clone());
            }
            channel.people.insert(person);
        }
    }
    by_channel
        .into_iter()
        .map(|(channel, people)| Roster {
            channel_id: channel.to_owned(),
            muted: people.people.difference(&people.unmuted).cloned().collect(),
            // Deafened is muted too: someone heard anywhere is neither.
            deafened: people
                .people
                .difference(&people.unmuted)
                .filter(|person| !people.hearing.contains(*person))
                .cloned()
                .collect(),
            people: people.people.into_iter().collect(),
            cameras: people.cameras.into_iter().collect(),
            screens: people.screens.into_iter().collect(),
            // A guest the room names in a way the toolkit refuses is left
            // out rather than costing everyone the roster.
            guests: people.guests.into_values().filter(Guest::valid).collect(),
            bridges: people.bridges.into_iter().collect(),
        })
        .filter(|roster| {
            !(roster.people.is_empty() && roster.guests.is_empty() && roster.bridges.is_empty())
                && roster.valid()
        })
        .collect()
}
