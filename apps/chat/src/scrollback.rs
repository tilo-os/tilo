//! Reading back through the chat's parts ([`crate::parts`]), with the SDK's
//! chain pager ([`ui::framework::ChainPager`]).
//!
//! A device holds the head of the chat's messages and whatever earlier
//! parts someone read back to. The timeline reserves the place of the
//! earlier messages above the oldest it shows while there are any; its
//! reader coming within a view of that place opens the part before, one
//! part at a time, and the place shows the part arriving
//! ([`ui::widget::ArrivalFace`]) while its history is on its way. The
//! message at the top of the view stays where it is as older ones come in
//! above it.
//!
//! Read marks are positions in the chain, so they hold without the earlier
//! parts: the "new messages" line goes before the first message after the
//! reader's mark as it stood when they opened the channel, and the unread
//! counts reach back to the mark through the counts each part carries of
//! the ones before it ([`crate::parts::ChatPart::earlier`]), however long
//! the reader was away, with nothing earlier held. A message a link or an
//! agent names that no part held here has is looked for in earlier parts,
//! opened in turn.

use ui::framework::{ChainPager, Paged};
use ui::tilo_resource::Id;

use crate::app::ChatApp;
use crate::parts::{ChatPart, ReadMark};

/// Where the window stands reading back through the chat's parts.
#[derive(Default)]
pub(crate) struct Scrollback {
    pub(crate) pager: Option<ChainPager<ChatPart>>,
    /// The channel in view, the reader's mark in it when they opened it,
    /// and whether anything after that mark was unread then.
    opened: Option<(String, ReadMark, bool)>,
    /// The message the "new messages" line goes before.
    pub(crate) first_unread: Option<String>,
    /// A message to reveal once a part holding it is held.
    pub(crate) seeking: Option<String>,
    /// The message at the top of the view and how far below the view's top
    /// it starts, while the reader looks back: kept there as rows arrive.
    pub(crate) anchor: Option<(String, f64)>,
}

impl Scrollback {
    /// Whether the channel's timeline shows earlier messages as still to
    /// come above what it holds.
    #[must_use]
    pub(crate) fn loading(&self) -> bool {
        self.pager.as_ref().is_some_and(ChainPager::loading)
    }

    /// Whether a settle pass has a part to open: the window asks for one.
    #[must_use]
    pub(crate) fn wants_settle(&self) -> bool {
        self.pager.as_ref().is_some_and(ChainPager::wants_settle)
    }

    /// The reader came within a view of the top of what is held.
    pub(crate) fn reached_top(&mut self) {
        if let Some(pager) = &mut self.pager {
            pager.reached_top();
        }
    }
}

impl ChatApp {
    /// Whether the timeline in view reads back through the chat's parts:
    /// a channel's, not a direct conversation's, once one is in view, and
    /// only while earlier parts may hold its messages.
    pub(crate) fn reads_back(&self) -> bool {
        // A window still choosing its channel (its record not read yet)
        // shows no timeline to read back from.
        self.direct_person.is_none()
            && self.channel(&self.active_channel).is_some()
            && self.scrollback.loading()
            && self.earlier_may_hold_channel()
    }

    /// Whether the parts before those held may hold messages of the channel
    /// in view: not when the head's counts of them say it has none there,
    /// so a channel quiet before (or never used) reads nothing back.
    fn earlier_may_hold_channel(&self) -> bool {
        let pager = self.scrollback.pager.as_ref();
        // A part asked for is shown arriving or unavailable whatever it holds.
        if pager.is_some_and(|pager| !pager.chain().waited().is_empty()) {
            return true;
        }
        self.held_messages()
            .earlier_than_held(&Id::from_raw(self.active_channel.clone()))
            != Some(0)
    }

    /// Read the reader's marks and the "new messages" line, as the record
    /// and the parts are read.
    pub(crate) fn read_scrollback(
        &mut self,
        chat: &crate::record::Chat,
        held: &crate::messages::Messages,
    ) {
        let back = &mut self.scrollback;
        let channel = self.active_channel.clone();
        if channel.is_empty() || self.direct_person.is_some() {
            back.opened = None;
            back.first_unread = None;
            return;
        }
        let id = Id::from_raw(channel.clone());
        if back.opened.as_ref().is_none_or(|(at, ..)| *at != channel) {
            let mark = chat.read_mark(&channel, &self.me.id);
            // A channel never read has no line: everything in it is new.
            let unread = mark != ReadMark::default() && held.unread(&id, mark) > 0;
            back.opened = Some((channel, mark, unread));
        }
        back.first_unread = back
            .opened
            .as_ref()
            .filter(|(.., unread)| *unread)
            .and_then(|(_, mark, _)| held.first_unread(&id, *mark))
            .map(|id| id.as_str().to_owned());
    }

    /// Open the part before the oldest held when the reader reached the top,
    /// a message is looked for or the reader's marks lie in parts not held.
    /// Run where the workspace's records take writes (a settle pass).
    pub(crate) fn settle_scrollback(&mut self) {
        let Some(pager) = &mut self.scrollback.pager else {
            return;
        };
        match pager.settle() {
            Paged::Nothing => {}
            Paged::Opened(_) => {
                self.reselect();
                self.request_relayout();
            }
            Paged::Found => {
                // The part holding it is read at the next sync, where the
                // message is revealed.
                self.reselect();
                self.request_relayout();
            }
            Paged::NotFound => self.scrollback.seeking = None,
        }
    }

    /// Reveal the message looked for once a held part has it.
    pub(crate) fn reveal_sought(&mut self) {
        let Some(message) = self.scrollback.seeking.clone() else {
            return;
        };
        if self.search_messages.iter().any(|item| item.id == message) {
            self.scrollback.seeking = None;
            let _ = self.reveal_held_message(&message);
        }
    }

    /// Look for `message` in the parts before the oldest held, when there
    /// are any: it is revealed once one has it. Whether it is looked for.
    pub(crate) fn seek_earlier(&mut self, message: &str) -> bool {
        let Some(pager) = &mut self.scrollback.pager else {
            return false;
        };
        if !pager.loading() {
            return false;
        }
        let id = Id::from_raw(message);
        pager.seek(move |part: &ChatPart| part.messages.contains(&id));
        self.scrollback.seeking = Some(message.to_owned());
        true
    }
}

#[cfg(test)]
mod tests;
