//! Voice from the roster: who is in which voice channel, as the room's
//! calls say (the session's native media service), and "Join" on a
//! person's row, which asks the app holding that channel to join its call,
//! as a person's click in that app or an agent's call would. Joining is
//! being in the call with the microphone on, unless the person last chose to
//! join muted (the calls' own rule). The channel this device is in is what
//! its people see as this person's call; leaving clears it.

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
    application_action_target,
};
use tilo_desktop::collaborators::{RosterRow, VoiceChannel};

use crate::shell::native_media::RoomCall;
use crate::shell::remote_actions::BrokerActionHost;

/// The app whose voice channels the roster joins: Chat holds a room's
/// voice channels, each a call row with its `join-call` action.
const CHAT_APP: &str = "so.tilo.chat";
/// The kind of Chat's workspace record, the root of its resource ids.
const CHAT_RECORD_KIND: &str = "chat.workspace";
/// Chat's action on a voice channel's call row that joins its call.
const JOIN_CALL_ACTION: &str = "join-call";

/// The action that joins `call` in workspace `room`: its app's `join-call`
/// on the channel's call row, delivered to the app (opened into the
/// workspace first if it is not running). `None` for a channel of an app
/// whose calls this does not know how to join.
pub(crate) fn join_call_target(room: &str, call: &VoiceChannel) -> Option<RemoteActionTarget> {
    if call.app != CHAT_APP || call.channel.is_empty() {
        return None;
    }
    let row = tilo_resource::items::item_resource_id_in(
        CHAT_RECORD_KIND,
        "channels",
        &tilo_resource::Id::from_raw(call.channel.as_str()),
    );
    Some(application_action_target(
        room,
        &call.app,
        format!("{row}/call"),
        JOIN_CALL_ACTION,
    ))
}

/// Decorate `rows` with the calls they are in: `call` set for everyone in
/// one, and `join` for those in a call this device is not in. `person_of`
/// names the person behind a row (a person id, as calls list people);
/// `own` is the call this device is in.
pub(crate) fn decorate_rows(
    rows: &mut [RosterRow],
    calls: &[RoomCall],
    person_of: impl Fn(&RosterRow) -> Option<String>,
    own: Option<&(String, String, String)>,
) {
    for row in rows.iter_mut() {
        let Some(person) = person_of(row) else {
            continue;
        };
        let Some(call) = calls
            .iter()
            .find(|call| call.people.iter().any(|in_call| *in_call == person))
        else {
            continue;
        };
        row.call = Some(call.channel.clone());
        let mine = own.is_some_and(|(app, record, channel)| {
            *app == call.app && *record == call.record && *channel == call.channel
        });
        row.join = (!mine && !row.is_agent).then(|| VoiceChannel {
            app: call.app.clone(),
            record: call.record.clone(),
            channel: call.channel.clone(),
        });
    }
}

impl super::ShellOverlayHost {
    /// Install the session's calls, once its broker has bound.
    pub(crate) fn set_native_media(
        &mut self,
        media: crate::shell::native_media::NativeMediaService,
    ) {
        self.native_media = Some(media);
    }

    /// The voice channel this device is in, in the active workspace: what
    /// its people see as this person's call. `None` outside a call.
    pub(super) fn own_voice_channel(&self) -> Option<String> {
        let room = self.active_room()?;
        let media = self.native_media.as_ref()?;
        media
            .own_call(&room.to_string())
            .map(|(_, _, channel)| channel)
    }

    /// Who is in which voice channel of the active workspace, onto the
    /// roster's `rows`, with the Join each row offers.
    pub(super) fn decorate_calls(&self, rows: &mut [RosterRow]) {
        let (Some(media), Some(room), Some(chrome)) = (
            self.native_media.as_ref(),
            self.active_room(),
            self.chrome.as_ref(),
        ) else {
            return;
        };
        let workspace = room.to_string();
        let calls = media.room_calls(&workspace);
        if calls.is_empty() {
            return;
        }
        let own = media.own_call(&workspace);
        let principals = chrome.borrow().tree().room_peer_principals(room);
        decorate_rows(
            rows,
            &calls,
            |row| match row.id.strip_prefix("peer@") {
                Some(handle) => handle
                    .parse::<u64>()
                    .ok()
                    .and_then(|handle| principals.get(&handle))
                    .map(|person| person.to_string()),
                None => Some(row.id.clone()),
            },
            own.as_ref(),
        );
    }

    /// Join `call` (a row's Join): still one of the room's calls, asked of
    /// the app that holds it. Says so when it could not.
    pub(super) fn join_voice(&mut self, call: &VoiceChannel) {
        let (Some(chrome), Some(room)) = (self.chrome.clone(), self.active_room()) else {
            return;
        };
        let workspace = room.to_string();
        let listed = self.native_media.as_ref().is_some_and(|media| {
            media.room_calls(&workspace).iter().any(|listed| {
                listed.app == call.app
                    && listed.record == call.record
                    && listed.channel == call.channel
            })
        });
        let refusal = match (
            listed,
            join_call_target(&workspace, call),
            self.action_broker(),
        ) {
            (false, _, _) => Some(crate::shell::i18n::t("people-join-call-ended")),
            (true, Some(target), Some(broker)) => {
                let caller = Caller::local_person(chrome.borrow().local_user_for_room(&room))
                    .in_room(workspace.clone());
                let request = InvokeRequest {
                    on_behalf_of: None,
                    invocation: InvocationId::new(),
                    target,
                    argument: InvokeArgument::None,
                    context: None,
                    confirmation: None,
                    revision: None,
                    wait_ms: None,
                    appearance: None,
                };
                match BrokerActionHost::new(broker)
                    .invoke_job(&caller, request)
                    .poll_ready()
                {
                    PendingPoll::Ready(Ok(_)) => None,
                    PendingPoll::Ready(Err(error)) => Some(error.to_string()),
                    PendingPoll::Waiting | PendingPoll::Abandoned => {
                        Some(crate::shell::i18n::t("people-join-call-not-taken"))
                    }
                }
            }
            (true, _, _) => Some(crate::shell::i18n::t("people-join-call-not-taken")),
        };
        if let Some(reason) = refusal {
            chrome.borrow_mut().push_warning_notification(
                crate::shell::i18n::t("people-join-call-failed"),
                reason,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> RosterRow {
        RosterRow {
            id: id.into(),
            label: id.into(),
            initial: 'A',
            color: vello::peniko::Color::BLACK,
            is_agent: false,
            is_bot: false,
            role: None,
            invited: false,
            activity: None,
            status: None,
            recent: Vec::new(),
            followable: true,
            availability: ui::Availability::Available,
            call: None,
            join: None,
        }
    }

    fn voice(people: &[&str]) -> RoomCall {
        RoomCall {
            app: CHAT_APP.into(),
            record: String::new(),
            channel: "c_voice".into(),
            people: people.iter().map(|person| (*person).to_owned()).collect(),
        }
    }

    /// Someone in one of the room's calls is shown in it and offers Join;
    /// someone in the call this device is already in offers no Join; someone
    /// in no call shows none.
    #[test]
    fn the_roster_shows_who_is_in_a_call_and_offers_to_join_it() {
        let mut rows = vec![row("ada"), row("ben"), row("peer@7")];
        let calls = [voice(&["ada-key", "chioma-key"])];
        let person_of = |row: &RosterRow| match row.id.as_str() {
            "ada" => Some("ada-key".to_owned()),
            "ben" => Some("ben-key".to_owned()),
            _ => Some("chioma-key".to_owned()),
        };
        decorate_rows(&mut rows, &calls, person_of, None);
        assert_eq!(rows[0].call.as_deref(), Some("c_voice"));
        assert_eq!(
            rows[0].join,
            Some(VoiceChannel {
                app: CHAT_APP.into(),
                record: String::new(),
                channel: "c_voice".into(),
            })
        );
        assert_eq!(rows[1].call, None, "in no call");
        assert_eq!(rows[1].join, None);
        assert!(rows[2].join.is_some());

        let own = (CHAT_APP.to_owned(), String::new(), "c_voice".to_owned());
        let mut rows = vec![row("ada")];
        decorate_rows(&mut rows, &calls, person_of, Some(&own));
        assert_eq!(
            rows[0].call.as_deref(),
            Some("c_voice"),
            "still shown in it"
        );
        assert_eq!(rows[0].join, None, "already in that call: nothing to join");
    }

    /// Join asks Chat for `join-call` on the channel's call row, in this
    /// workspace, the same action a click in Chat or an agent's call takes.
    #[test]
    fn join_asks_chat_to_join_the_channel_call() {
        let call = VoiceChannel {
            app: CHAT_APP.into(),
            record: String::new(),
            channel: "c_voice".into(),
        };
        let target = join_call_target("room-1", &call).expect("a Chat call joins");
        assert_eq!(
            target,
            application_action_target(
                "room-1",
                CHAT_APP,
                "chat.workspace/channels/c_voice/call",
                "join-call"
            )
        );
        let elsewhere = VoiceChannel {
            app: "so.example.other".into(),
            ..call
        };
        assert_eq!(join_call_target("room-1", &elsewhere), None);
    }
}
