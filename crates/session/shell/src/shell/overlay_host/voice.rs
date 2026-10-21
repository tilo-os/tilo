//! Voice from the roster: who is in which voice channel, as each person's
//! own device says in their presence ([`ui::CallPresence`]: the channel's id
//! and name, sent only to the people who may see where they are), and
//! "Join" on a person's row, which asks Chat to join that channel's call,
//! opening Chat into the workspace first when it is closed, as a person's
//! click in Chat or an agent's call would. Joining is being in the call
//! muted, listening; the microphone is asked for on the first unmute (the
//! calls' own rule). The channel this device is in is what its people see as
//! this person's call; leaving clears it.

use tilo_action_broker::collab_broker_capnp::PublishedSurface;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
    application_action_target,
};
use tilo_desktop::collaborators::{RosterRow, VoiceChannel};

use crate::shell::remote_actions::BrokerActionHost;

/// The app whose voice channels the roster joins: Chat holds a room's
/// voice channels, each a call row with its `join-call` action.
const CHAT_APP: &str = "so.tilo.chat";
/// The kind of Chat's workspace record, the root of its resource ids.
const CHAT_RECORD_KIND: &str = "chat.workspace";
/// Chat's action on a voice channel's call row that joins its call.
const JOIN_CALL_ACTION: &str = "join-call";

/// Chat's resource id of voice channel `channel`'s row.
fn channel_row(channel: &str) -> String {
    tilo_resource::items::item_resource_id_in(
        CHAT_RECORD_KIND,
        "channels",
        &tilo_resource::Id::from_raw(channel),
    )
}

/// The action that joins `call` in workspace `room`: Chat's `join-call` on
/// the channel's call row, delivered to Chat (opened into the workspace
/// first if it is not running). `None` for a channel of an app whose calls
/// this does not know how to join.
pub(crate) fn join_call_target(room: &str, call: &VoiceChannel) -> Option<RemoteActionTarget> {
    if call.app != CHAT_APP || call.channel.is_empty() {
        return None;
    }
    Some(application_action_target(
        room,
        &call.app,
        format!("{}/call", channel_row(&call.channel)),
        JOIN_CALL_ACTION,
    ))
}

/// The name of voice channel `channel` as `app` (Chat) last published its
/// channel row in workspace `room`: the row's label.
pub(crate) fn channel_name(
    surfaces: &[PublishedSurface],
    (room, app): (&str, &str),
    channel: &str,
) -> Option<String> {
    if app != CHAT_APP {
        return None;
    }
    let row = channel_row(channel);
    surfaces
        .iter()
        .filter(|surface| surface.app_id == app && surface.route.cluster_id == room)
        .flat_map(|surface| &surface.actions)
        .find(|action| action.resource == row)
        .map(|action| action.resource_label.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// Decorate `rows` with the calls their people say they are in: `call` set
/// to the channel's name (empty when it was not said) for everyone in one,
/// and `join` for those in a call this device is not in. `call_of` reads a
/// row's person's published call; `own` is the channel this device is in.
pub(crate) fn decorate_rows(
    rows: &mut [RosterRow],
    call_of: impl Fn(&RosterRow) -> Option<ui::CallPresence>,
    own: Option<&str>,
) {
    for row in rows.iter_mut() {
        let Some(call) = call_of(row) else {
            row.call = None;
            row.join = None;
            continue;
        };
        row.join = (own != Some(call.channel.as_str()) && !row.is_agent).then(|| VoiceChannel {
            app: CHAT_APP.into(),
            record: String::new(),
            channel: call.channel.clone(),
        });
        row.call = Some(call.name);
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

    /// The voice channel this device is in, in the active workspace, as its
    /// people see it: the channel's id and, from Chat's published channel
    /// row, its name. `None` outside a call.
    pub(super) fn own_voice_call(&self) -> Option<ui::CallPresence> {
        let room = self.active_room()?.to_string();
        let (app, _, channel) = self.native_media.as_ref()?.own_call(&room)?;
        let name = self
            .action_broker()
            .and_then(|broker| channel_name(&broker.surfaces(), (&room, &app), &channel))
            .unwrap_or_default();
        Some(ui::CallPresence { channel, name })
    }

    /// Who is in which voice channel of the active workspace, as their
    /// presence says, onto the roster's `rows`, with the Join each offers.
    pub(super) fn decorate_calls(
        &self,
        rows: &mut [RosterRow],
        presence: &ui::presence::PresenceCtx,
    ) {
        let own = self
            .active_room()
            .zip(self.native_media.as_ref())
            .and_then(|(room, media)| media.own_call(&room.to_string()))
            .map(|(_, _, channel)| channel);
        decorate_rows(
            rows,
            |row| {
                let id = crate::shell::desk_widgets::team::presence_id(presence, row)?;
                ui::CallPresence::decode(presence.call_of(&id)?)
            },
            own.as_deref(),
        );
    }

    /// Ask Chat, through `broker`, to join the call `target` names; the
    /// reason it refused, if it did.
    fn ask_to_join(
        broker: crate::shell::collab_broker::AppActionBroker,
        caller: &Caller,
        target: RemoteActionTarget,
    ) -> Option<String> {
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
        let polled = BrokerActionHost::new(broker)
            .invoke_job(caller, request)
            .poll_ready();
        match polled {
            // Waiting: Chat is opening, and takes it once it does.
            PendingPoll::Ready(Ok(_)) | PendingPoll::Waiting => None,
            PendingPoll::Ready(Err(error)) => Some(error.to_string()),
            PendingPoll::Abandoned => Some(crate::shell::i18n::t("people-join-call-not-taken")),
        }
    }

    /// Join `call` (a row's Join): asked of Chat, which opens into the
    /// workspace first when it is closed. Says so when it could not.
    pub(super) fn join_voice(&self, call: &VoiceChannel) {
        let (Some(chrome), Some(room)) = (self.chrome.clone(), self.active_room()) else {
            return;
        };
        let workspace = room.to_string();
        let target = join_call_target(&workspace, call);
        let caller =
            Caller::local_person(chrome.borrow().local_user_for_room(&room)).in_room(workspace);
        let refusal = target.zip(self.action_broker()).map_or_else(
            || Some(crate::shell::i18n::t("people-join-call-not-taken")),
            |(target, broker)| Self::ask_to_join(broker, &caller, target),
        );
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

    fn voice() -> ui::CallPresence {
        ui::CallPresence {
            channel: "c_voice".into(),
            name: "voice".into(),
        }
    }

    /// Someone whose presence says they are in a call is shown in it, by
    /// the channel's name, and offers Join; someone in the call this device
    /// is already in offers no Join; someone in no call shows none.
    #[test]
    fn the_roster_shows_who_is_in_a_call_and_offers_to_join_it() {
        let mut rows = vec![row("ada"), row("ben")];
        let call_of = |row: &RosterRow| (row.id == "ada").then(voice);
        decorate_rows(&mut rows, call_of, None);
        assert_eq!(rows[0].call.as_deref(), Some("voice"));
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

        let mut rows = vec![row("ada")];
        decorate_rows(&mut rows, call_of, Some("c_voice"));
        assert_eq!(rows[0].call.as_deref(), Some("voice"), "still shown in it");
        assert_eq!(rows[0].join, None, "already in that call: nothing to join");

        let mut rows = vec![row("ada")];
        decorate_rows(&mut rows, |_| None, None);
        assert_eq!((rows[0].call.clone(), rows[0].join.clone()), (None, None));
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
