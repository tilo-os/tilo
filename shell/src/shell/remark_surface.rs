//! The compositor's half of "say something about this".
//!
//! An application right-clicked in asks this session for a compose surface;
//! the person writes a sentence into it; the sentence goes into the room. This
//! module is the two ends of that — the queue the request arrives on, and the
//! encoder the finished remark leaves through — with the panel itself drawn by
//! [`ShellOverlayHost`](crate::shell::overlay_host::ShellOverlayHost) in
//! between.
//!
//! # Why the compositor holds both ends
//!
//! Because an application can hold neither.
//!
//! The panel opens *beside* the app's own context menu, and beside a menu near
//! a window's edge is outside the window — which a Wayland client cannot paint
//! into. On a strip of tiled windows that is most of the time, so an app that
//! drew it had it sliced off mid-word with the next application visible past
//! the cut. Only the compositor draws above the windows.
//!
//! And an application must not decide who may read what is written about its
//! own object. The destination list is the *room's* roster; the session holds
//! it, and an app that supplied one would be choosing its own audience.
//!
//! # What the room gets
//!
//! An ordinary chat message. A remark is encoded as chat's own `SendMessage`,
//! under chat's own type id, so it lands in the same document, channel and
//! history as anything typed in the chat window — with two fields beside the
//! body: the address of the thing being talked about, and where it goes.
//!
//! It reaches the room and not one participant, deliberately. A remark
//! addressed to an agent is an instruction that will change a shared document,
//! and narrowing its delivery would put that instruction somewhere the people
//! sharing the document cannot read.

use std::sync::{Arc, Mutex};

use qos_reference::{Remark, ResourceRef, RoomMember};
use ui::collaboration::CollabEvent;

use crate::shell::chrome_state::Chrome;
use crate::shell::collab_broker::{BrokerHandle, ComposeRemarkRequest};
use crate::shell::collab_broker::CrdtRoute;

/// Requests that have arrived and not yet been drawn.
///
/// The broker's reader thread cannot draw one: where the asking window sits,
/// which display it is on and who is in the room are all answers held by the
/// thread that paints. So the question is parked here and taken up on the next
/// frame, which that thread was going to render anyway.
#[derive(Default)]
pub struct PendingComposes {
    queue: Mutex<Vec<ComposeRemarkRequest>>,
}

impl PendingComposes {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Point the session's broker at this queue.
    pub fn install(self: &Arc<Self>, broker: &BrokerHandle) {
        let queue = Arc::clone(self);
        broker.set_remark_composer(Box::new(move |req| {
            if let Ok(mut pending) = queue.queue.lock() {
                pending.push(req);
            }
        }));
    }

    fn take(&self) -> Vec<ComposeRemarkRequest> {
        self.queue
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }
}

/// One request, decoded: what it is about and what to sit beside.
///
/// Separate from the wire shape because the wire carries the address as JSON —
/// the shape belongs to `qos-reference`, and a transport that mirrored it would
/// need revising every time that crate grew a field.
pub struct Compose {
    pub reference: ResourceRef,
    /// The menu rect, still in the asking window's own coordinates. Converted
    /// by the host, which is what knows where that window is.
    pub beside: vello::kurbo::Rect,
    /// The connection that asked, so it can be told when the panel goes. It
    /// has a context menu standing beside a panel it cannot see, and it will
    /// never see the key that ends it — the session holds the keyboard.
    pub peer: u64,
}

/// Decode everything that arrived since the last frame.
///
/// An address that will not parse is dropped with a line saying so rather than
/// opening a panel about nothing: a compose box over no subject asks somebody
/// to write a sentence that could not be delivered anywhere useful.
pub fn take_requests(pending: &PendingComposes) -> Vec<Compose> {
    let mut out = Vec::new();
    for req in pending.take() {
        let reference: ResourceRef = match serde_json::from_str(&req.reference) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    peer = req.peer, room = %req.room, error = %e,
                    "an application asked for a compose surface about an address \
                     that will not decode; nothing was opened",
                );
                continue;
            }
        };
        let (x, y, w, h) = req.beside;
        out.push(Compose {
            reference,
            beside: vello::kurbo::Rect::new(x, y, x + w, y + h),
            peer: req.peer,
        });
    }
    out
}

/// Put a finished remark in the room.
///
/// The three things only this end can do, and the reason the panel hands the
/// message over rather than sending it:
///
/// * **Mentions are resolved.** The compose surface leaves them empty because
///   resolving `@claude` needs the roster. This has it.
/// * **The message is minted.** An id, an author and a timestamp, chosen once
///   here so the change replays byte-identically on every peer.
/// * **It is encoded and sent.** As chat's own `SendMessage`, under chat's own
///   type id, so it lands in the same document and history as anything typed in
///   the chat window.
///
/// A refusal is reported, never swallowed. Somebody wrote a sentence, pressed
/// Send and watched the panel close; the one thing that must not happen is for
/// that to look identical to a message that was delivered.
pub fn deliver(chrome: &Chrome, broker: &BrokerHandle, remarks: Vec<Remark>) {
    if remarks.is_empty() {
        return;
    }
    let (room, local_id, members) = {
        let st = chrome.borrow();
        let tree = st.tree();
        let room = tree
            .active_workspace()
            .map(|w| w.room.to_string())
            .unwrap_or_default();
        let members: Vec<RoomMember> =
            crate::shell::overlay_host::collaborator_roster_rows(&tree)
                .into_iter()
                .map(|r| RoomMember::new(r.id, r.label, r.is_agent))
                .collect();
        (room, st.local_user_id.clone(), members)
    };
    if room.is_empty() {
        tracing::error!(
            "a remark was written with no active workspace to put it in; it was \
             not delivered",
        );
        return;
    }
    // Who the room thinks wrote this.
    //
    // A remark is minted here and not by an application, so nothing else
    // gets to say who its author was — and this end had only ever known
    // the *machine's* name for the person at it: `QOS_USER_ID`, or the
    // account they logged in as. Every application in the room is keyed
    // by the other spelling, `peer@<liveness handle>`, which is what the
    // broker announces this participant as and what each of their apps
    // authors under. So a remark arrived in a chat under an id no roster
    // could resolve: no name, no monogram, no colour, for somebody the
    // member list beside it was naming correctly.
    //
    // The account name stays as the fallback for a session with no room
    // to be addressed in, which is the one case where there is no other
    // spelling to prefer.
    let author = broker.room_participant_id().unwrap_or(local_id);
    for remark in remarks {
        // Resolved before the move: `with_mentions` consumes the remark, so
        // the borrow of its text has to be finished first.
        let mentions = qos_reference::resolve_mentions(&remark.text, &members);
        let remark = remark.with_mentions(mentions);
        let id = format!("m_{}", uuid::Uuid::new_v4().simple());
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        match remark.to_chat_action(id.as_str(), author.as_str(), ts) {
            Ok(action) => {
                // Whoever sends, says so.
                //
                // This line used to come from the application, off
                // `RemarkHost::just_sent`, and it is the one account of the
                // gesture completing that anything outside the process can
                // read. But an application only reaches that code when it drew
                // the panel itself; a window whose chrome this session draws
                // never composes, never sends, and so could never write it. The
                // send worked and the only evidence of it disappeared with the
                // panel — which looks exactly like a send that gave up.
                //
                // So it is written here instead, by the end that actually puts
                // the message on the wire, and it stays true whichever end drew
                // the panel.
                //
                let to = destination_token(&remark.destination);
                tracing::info!(
                    target: "shell::remark",
                    %to,
                    about = %remark.reference.resource_id().as_str(),
                    text = %remark.text,
                    "remark sent"
                );
                let delivery = remark.delivery();
                tracing::info!(
                    target: "shell::remark",
                    message = %id,
                    about = %remark.reference.resource_id().as_str(),
                    to = ?delivery,
                    addressed = ?delivery.addressed(),
                    "sending a remark into the room"
                );
                // Cluster scope, and this machine only. The point of the verb
                // is to reach an app that is not the sender, so a narrower
                // route would deliver it to nobody. It is not published to the
                // room's other machines either: an action is a request that one
                // app change a document it owns, and every machine applying it
                // independently would mint one edit each. The document change
                // that results crosses on the path that already carries
                // document changes.
                broker.fanout_event(
                    &CollabEvent::ActionArrived {
                        type_id: action.type_id,
                        payload: action.payload,
                        from_user_id: author.clone(),
                    },
                    None,
                    &CrdtRoute::cluster(&room),
                );
            }
            Err(why) => {
                tracing::error!(
                    target: "shell::remark",
                    about = %remark.reference.resource_id().as_str(),
                    error = %why,
                    "a remark was written and could not be sent"
                );
                // And say so to the person who wrote it.
                //
                // The refusal itself is right: chat has no private
                // delivery, and encoding a `Delivery::Private` as a
                // channel row would put a message somebody marked private
                // in front of the whole room -- a disclosure committed by
                // the code that was meant to route it. Refusing is the
                // safe half.
                //
                // Refusing *silently* is not. The compose surface offers
                // the people in the room as destinations, so choosing one
                // is an ordinary thing to do, and until now the whole of
                // what it produced was a log line: the box closed, the
                // message was discarded, and nothing on screen differed
                // from having sent it. A refusal nobody is told about is
                // indistinguishable from a bug, and the person keeps the
                // belief that they said something.
                let mut chrome = chrome.borrow_mut();
                chrome.push_notification_full(
                    crate::shell::notification::Notification::new(
                        crate::shell::i18n::t("remark-not-sent-title"),
                        crate::shell::i18n::t("remark-not-sent-body"),
                    )
                    .with_severity(
                        crate::shell::notification::NotificationSeverity::Warning,
                    )
                    .with_dismiss_after(std::time::Duration::from_secs(30)),
                );
            }
        }
    }
}

/// Name a destination in one whitespace-free token.
///
/// The id, never the label the selector shows. A label is localised prose —
/// `the room`, `der Raum` — and the send line names the destination in a field
/// beside two others, so a reader that finds the end of a field by finding a
/// space takes `the` for the whole destination and then cannot find the field
/// after it. The id is also the stable half: it does not move when the wording
/// does, or when the session is running in another language.
fn destination_token(destination: &qos_reference::Destination) -> String {
    match destination.target() {
        qos_reference::Target::Channel(id) => format!("channel:{id}"),
        qos_reference::Target::Participant(id) => format!("participant:{id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_reference::Destination;

    /// The send line is read field by field by whatever is watching a session,
    /// and those readers find the end of a field by finding a space. So the
    /// destination has to survive being written into one.
    ///
    /// Asserted against a destination whose *label* has a space in it, because
    /// that is the real case rather than a hypothetical one: the room's option
    /// is built from a localised string, and in English it reads `the room`.
    #[test]
    fn a_destination_is_one_word_however_its_label_reads() {
        let room = Destination::channel(qos_reference::ROOM_CONVERSATION, "the room");
        let token = destination_token(&room);
        assert!(
            !token.chars().any(char::is_whitespace),
            "the room's destination token carried whitespace: {token:?}",
        );
        assert!(
            token.contains(qos_reference::ROOM_CONVERSATION),
            "the token should name the channel it goes to, got {token:?}",
        );

        let person = Destination::participant("u_xaver", "Xaver Emmerling", false);
        let token = destination_token(&person);
        assert!(
            !token.chars().any(char::is_whitespace),
            "a participant's destination token carried whitespace: {token:?}",
        );
        assert!(
            token.contains("u_xaver"),
            "the token should name the participant it goes to, got {token:?}",
        );
    }
}
