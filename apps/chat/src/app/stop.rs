//! An agent at work for the person at this window, and their way to stop it.
//!
//! Which replies agents are still at work on, for whom, and which their person stopped is read
//! from the parts the window holds ([`crate::summons::answering`]) at every reload: a second
//! window and a second device of the same person find the same, and nothing here keeps it.
//! While such a reply is unfinished the conversation's box offers its person the stop where it
//! offers send, and the reply's own menu offers it too. Both invoke Chat's `stop-agent` on the
//! reply, the action an agent, an automation and the command line invoke; Chat's check there
//! stays the judge of who stops what, and what it refuses is said where the person pressed.

use ui::tilo_resource::MemberId;

use super::ChatApp;
use crate::messages::chat_part;
use crate::model::Id;
use crate::parts::ChatPart;
use crate::summons::Answering;

/// A box's send giving way to its stop and back: what is shown changes in place.
#[derive(Debug, Default)]
pub(crate) struct StopSwap {
    main: Option<ui::motion::CrossFade<bool>>,
    thread: Option<ui::motion::CrossFade<bool>>,
}

/// How a box's send and its stop are drawn in one frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SwapFrame {
    /// How opaque the control shown is.
    pub(crate) shown: f32,
    /// How opaque the control it took the place of still is, while that one leaves.
    pub(crate) leaving: Option<f32>,
}

/// What stands where a box sends, in one frame: the reply its stop stops, while it offers one,
/// and how the two controls are drawn while one takes the other's place.
#[derive(Debug)]
pub(crate) struct SendPlace {
    /// The reply whose work the stop stops; `None` while the box sends.
    pub(crate) stop: Option<Id>,
    pub(crate) swap: SwapFrame,
}

impl StopSwap {
    /// Follow the box (`thread`: the thread's, else the conversation's) to showing its stop
    /// (`stops`) or its send, and say how each is drawn now. What the conversation holds decides
    /// which, on this window as on every other, so the change arrives here rather than answering
    /// a hand: it fades, and the first frame of a box shows it at rest.
    pub(crate) fn follow(
        &mut self,
        thread: bool,
        stops: bool,
        theme: &ui::theme::Theme,
    ) -> SwapFrame {
        // Read at the frame's instant: every pass of one frame then sees the same fade.
        let now = ui::motion::frame_now();
        let fade = if thread {
            &mut self.thread
        } else {
            &mut self.main
        };
        let fade = fade.get_or_insert_with(|| ui::motion::CrossFade::new(stops));
        fade.arrive(stops, theme.motion(), now);
        SwapFrame {
            shown: fade.incoming(now),
            leaving: fade.outgoing(now).map(|(_, opacity)| opacity),
        }
    }

    /// Whether a swap still moves: frames are asked for until it rests.
    pub(crate) fn moving(&self) -> bool {
        let now = ui::motion::frame_now();
        [&self.main, &self.thread]
            .into_iter()
            .flatten()
            .any(|fade| fade.is_moving(now))
    }
}

impl ChatApp {
    /// Read which replies agents are at work on, and which their person stopped, from the parts
    /// `held`: as a reload reads everything else of the conversation.
    pub(crate) fn read_answering(&mut self, held: &crate::messages::Messages) {
        self.answering = held
            .parts()
            .iter()
            .flat_map(|(_, part)| crate::summons::answering(part))
            .collect();
    }

    /// The turn the reply `reply` answers, while an agent is at work on it or once its person
    /// stopped it.
    fn answered(&self, reply: &str) -> Option<&Answering> {
        self.answering
            .iter()
            .find(|turn| turn.reply.as_str() == reply)
    }

    /// Whether the person at this window stops the work on `reply` now: an agent is still at
    /// work on it, for them. Chat's own check (`stop-agent`) lets the person a turn is for
    /// through and nobody else, so nobody else is offered it.
    pub(crate) fn stops(&self, reply: &str) -> bool {
        self.can_participate()
            && self.answered(reply).is_some_and(|turn| {
                turn.going && self.is_me(&MemberId::person(turn.person.as_str()))
            })
    }

    /// The reply a box stops: the newest one an agent is still at work on for this window's
    /// person, in the conversation shown, or for the thread's box in the thread that is open.
    pub(crate) fn stop_offered(&self, thread: bool) -> Option<Id> {
        let open = self.thread_open.as_deref();
        self.messages
            .iter()
            .rev()
            .filter(|message| !thread || (open.is_some() && message.parent.as_deref() == open))
            .find(|message| self.stops(&message.id))
            .map(|message| message.id.clone())
    }

    /// Who stopped the turn `reply` answers, for a reply whose person stopped it: said to that
    /// person as theirs, and to everybody else by their name.
    pub(crate) fn stopped_words(&self, reply: &str) -> Option<String> {
        let turn = self.answered(reply).filter(|turn| turn.stopped)?;
        let person = MemberId::person(turn.person.as_str());
        Some(if self.is_me(&person) {
            crate::i18n::t("chat-agent-stopped-by-you")
        } else {
            crate::i18n::t_args(
                "chat-agent-stopped-by",
                &[("person", self.user_name(&person).into())],
            )
        })
    }

    /// Stop the work on `reply`, as the person at this window: Chat's `stop-agent` on the reply
    /// that shows the work. What Chat's check refuses changes nothing and is said under the box.
    pub(crate) fn stop_work(&mut self, doc: &mut ui::widget::Edits<'_>, reply: &str) {
        let stop = ChatPart::STOP_AGENT.with(chat_part::StopAgent {
            message: ui::tilo_resource::Id::from_raw(reply),
        });
        self.not_stopped = self.try_apply_part(doc, stop).err();
        self.reload(doc);
    }

    /// When the workspace refused a stop this window had applied, say so where it was pressed.
    pub(crate) fn settle_refused_stop(&mut self) {
        let Some(chain) = &self.chain else {
            return;
        };
        let refused = chain.dispatcher().refused();
        let reason = refused.with(|refused| {
            refused
                .as_ref()
                .filter(|refused| refused.action == "stop-agent")
                .map(|refused| refused.reason.clone())
        });
        let Some(reason) = reason else {
            return;
        };
        refused.set(None);
        self.not_stopped = Some(reason);
    }
}
