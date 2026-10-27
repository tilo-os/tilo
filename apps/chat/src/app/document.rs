//! Reading the room's document back into the window: the snapshot the
//! conversation shows, the composers' reload, the people the room announces,
//! and the read the person has reached.

use std::collections::{HashMap, HashSet};

use automerge::AutoCommit;

use crate::model::{self, Id, Message, User};

use super::{ChatApp, unread_mentions};

impl ChatApp {
    /// Rebuild the snapshot from the CRDT. Called at the tail of `sync` and
    /// after every mutation so `draw` always reflects the latest document.
    /// Count each channel's unread mentions of this peer, the badge that
    /// says someone is waiting on you, not just talking. Telling about a new
    /// one is the desktop's, for every app's mentions (`ui::mentions`).
    fn note_mentions(&mut self, per_channel: &[(Id, Vec<Message>)]) {
        let me: Vec<&str> = std::iter::once(self.me.id.as_str())
            .chain(self.former_ids.iter().map(String::as_str))
            .collect();
        let mentions: HashMap<Id, u32> = per_channel
            .iter()
            .filter(|(id, _)| *id != self.active_channel)
            .map(|(id, messages)| {
                let unread = self
                    .channels
                    .iter()
                    .find(|c| c.id == *id)
                    .map_or(0, |c| usize::try_from(c.unread).unwrap_or(usize::MAX));
                (id.clone(), unread_mentions(messages, unread, &me))
            })
            .collect();
        for c in &mut self.channels {
            c.mentions = mentions.get(&c.id).copied().unwrap_or_default();
        }
    }

    pub(crate) fn reload(&mut self, doc: &AutoCommit) {
        self.read_kept_messages(doc);
        let chat = ui::tilo_resource::profiling::measure("chat.reload.read", || {
            crate::projection::read(doc)
        });
        #[cfg(test)]
        {
            *self.last_read.borrow_mut() = std::rc::Rc::new(chat.clone());
        }
        let held = self.held_messages();
        // A deletion the person confirmed has gone: offer its Undo.
        if let Some((message, part, channel)) = self.asked_delete.take() {
            if held.contains(&message) {
                self.asked_delete = Some((message, part, channel));
            } else if let Some(part) = part {
                *self.deleted.get_mut() = Some((part, channel));
            }
        }
        self.chain_seen = self.chain.as_ref().map(ui::framework::Chain::revision);
        self.channels = chat.channel_rows();
        self.categories = chat.category_rows();
        for category in &mut self.categories {
            category.collapsed = self.collapsed.contains(&category.id);
        }
        // Keep the active channel valid (e.g. after it was deleted).
        if self.direct_person.is_none()
            && self.channel(&self.active_channel).is_none()
            && let Some(first) = self
                .ws_channels()
                .into_iter()
                .find(|c| c.kind == model::ChannelKind::Text)
                .map(|c| c.id.clone())
        {
            self.active_channel = first;
        }
        // Unread is per-peer derived state, not stored: the channel in view is
        // always read (0); every other channel shows its top-level messages
        // beyond this peer's read watermark. The watermark itself is advanced
        // by `mark_active_read` while a channel is viewed / on open.
        for c in &mut self.channels {
            c.unread = if c.id == self.active_channel {
                0
            } else {
                chat.unread(&held, &c.id, &self.me.id)
            };
        }
        // Read every channel's messages once per reload: the open channel,
        // search and the room's people directory all project from them
        // (reading them three times dominated typing with a long history).
        let per_channel: Vec<(Id, Vec<Message>)> = self
            .channels
            .iter()
            .map(|channel| (channel.id.clone(), chat.message_rows(&held, &channel.id)))
            .collect();
        self.note_mentions(&per_channel);
        self.messages_revision += 1;
        self.messages = per_channel
            .iter()
            .find(|(id, _)| *id == self.active_channel)
            .map_or_else(
                || chat.message_rows(&held, &self.active_channel),
                |(_, messages)| messages.clone(),
            );
        self.search_messages = per_channel
            .iter()
            .flat_map(|(_, messages)| messages.iter().cloned())
            .collect();
        self.search_users = chat.user_rows();
        self.reports = chat
            .feedback
            .iter()
            .filter_map(|(id, report)| {
                Some((
                    id.to_string(),
                    crate::feedback::message::message(report)?.text,
                ))
            })
            .collect();
        // Drop the body views of messages no longer loaded.
        let shown: HashSet<&str> = self.messages.iter().map(|m| m.id.as_str()).collect();
        self.body_views
            .borrow_mut()
            .retain(|(_, id), _| shown.contains(id.as_str()));
        self.agent_authors = chat.agent_authors();
        self.room_files = chat.files;
        // Who is in a call is projected only from the authenticated local
        // voice service's response.
        self.refresh_native_streams();
        self.users = self.room_people_from(&chat, &per_channel);
        self.sync_reload_composers(doc);
        // A share started, switched or ended reaches the room as soon as
        // the record says so, not at the window's next settled frame,
        // which an idle window may not have for a long while.
        self.follow_share_grants();
    }

    fn sync_reload_composers(&mut self, doc: &AutoCommit) {
        // Point each composer at its surface's draft; the editors read it on
        // their next sync (`sync_composers`), keeping the caret while typing.
        if self.direct_person.is_some() {
            self.sync_person_draft();
        } else {
            self.composer_main.set_key(&self.active_channel);
            self.composer_main.sync(doc);
            self.draft_main = self.composer_main.value();
        }
        let channel = self
            .channel(&self.active_channel)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.composer_main
            .set_placeholder(&format!("Message #{channel}"));
        if self.direct_person.is_some() {
            self.composer_main
                .set_placeholder(&crate::i18n::t("chat-person-chat-placeholder"));
        }
        self.composer_main
            .set_local_author(Some(self.me.id.clone()));
        let thread_key = self.thread_draft_key().unwrap_or_default();
        self.composer_thread.set_key(&thread_key);
        self.composer_thread.sync(doc);
        self.draft_thread = if thread_key.is_empty() {
            String::new()
        } else {
            self.composer_thread.value()
        };
        self.composer_thread
            .set_placeholder(&crate::i18n::t("chat-thread-reply-placeholder"));
        self.composer_thread
            .set_local_author(Some(self.me.id.clone()));
        self.composer_main
            .set_read_only(!self.main_composer_can_edit());
        self.composer_thread.set_read_only(!self.can_participate());
        self.set_composer_focus();
    }

    /// Bring both composers up to date with their drafts. `--shots` stages
    /// its mid-compose draft here.
    pub(crate) fn sync_composers(&mut self, doc: &AutoCommit) {
        for composer in [&mut self.composer_main, &mut self.composer_thread] {
            composer.sync(doc);
        }
        self.draft_main = self.composer_main.value();
        self.draft_thread = self.composer_thread.value();
    }

    /// Project the document's identity directory onto evidence in the room's
    /// channels. Channel activity and voice membership are durable evidence;
    /// the local identity is included so a brand-new room remains a valid
    /// one-person surface.
    pub(crate) fn room_people_from(
        &self,
        chat: &crate::record::Chat,
        per_channel: &[(Id, Vec<Message>)],
    ) -> Vec<User> {
        let mut ids: HashSet<Id> = HashSet::from([self.me.id.clone()]);
        ids.extend(self.former_ids.iter().cloned());

        for channel in &self.channels {
            let messages = per_channel
                .iter()
                .find(|(id, _)| *id == channel.id)
                .map(|(_, messages)| messages);
            for message in messages.into_iter().flatten() {
                ids.insert(message.author.clone());
                // A member who writes only from a linked account on a
                // bridged service is still this room's person: their
                // bridged messages read as theirs, by their name and colour,
                // never as a namesake from that service.
                if let Some(linked) = message
                    .appearance
                    .as_ref()
                    .and_then(|appearance| appearance.persona.as_ref()?.linked.as_deref())
                {
                    ids.insert(linked.to_owned());
                }
                for reaction in &message.reactions {
                    ids.extend(reaction.reactors.iter().map(|reactor| reactor.by.clone()));
                }
                ids.extend(message.mentions.iter().map(|mention| mention.user.clone()));
            }
        }

        let mut people: Vec<_> = chat
            .user_rows()
            .into_iter()
            .filter(|user| ids.contains(&user.id))
            .collect();
        for (id, name) in &self.kept_names {
            if ids.contains(id) && !people.iter().any(|user| user.id == *id) {
                people.push(User {
                    id: id.clone(),
                    name: name.clone(),
                    initials: ui::widget::initials_for_name(name),
                    color_idx: 0,
                    status: model::Presence::Offline,
                    is_agent: false,
                });
            }
        }
        if !people.iter().any(|user| self.is_me(&user.id)) {
            people.push(User {
                id: self.me.id.clone(),
                name: self.me.name.clone(),
                initials: self.me.initials.clone(),
                color_idx: self.me.color_idx,
                status: model::Presence::Online,
                is_agent: false,
            });
        }
        people.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        people
    }

    /// Announce this window's person into the shared roster once, under the
    /// name this device knows them by: what every member shows for them
    /// until the room names them. A repeat writes nothing.
    pub(super) fn announce_me(&mut self, doc: &AutoCommit) {
        let Some(me) = self.announce.take() else {
            return;
        };
        let chat = crate::projection::read(doc);
        let color = u32::try_from(me.color_idx).unwrap_or_default();
        if (chat.channels.is_empty() && chat.people.is_empty())
            || chat.people.iter().any(|person| {
                person.id.0 == me.id
                    && person.name == me.name
                    && person.initials == me.initials
                    && person.color == color
                    && person.status == model::Presence::Online
            })
        {
            return;
        }
        self.submit(
            crate::resource::Chat::INTRODUCE.with(crate::record::chat::Introduce {
                name: me.name,
                initials: me.initials,
                color,
            }),
        );
    }

    /// Advance the local read watermark for the channel currently in view so
    /// its unread badge stays cleared and doesn't reappear once the user
    /// navigates away. Idempotent (a no-op when nothing new arrived), so it is
    /// safe to call every frame.
    pub(crate) fn mark_active_read(&mut self, doc: &AutoCommit) {
        if self.direct_person.is_some() || self.active_channel.is_empty() || !self.timeline_at_end {
            return;
        }
        let chat = crate::projection::read(doc);
        let read = self
            .held_messages()
            .read_to_end(&ui::tilo_resource::Id::from_raw(
                self.active_channel.clone(),
            ));
        if read <= chat.read_mark(&self.active_channel, &self.me.id) {
            return;
        }
        self.note_refused_read();
        if self
            .read_refused
            .as_ref()
            .is_some_and(|(channel, refused)| *channel == self.active_channel && read <= *refused)
        {
            return;
        }
        // Read state is a durable collaboration fact.  It is authored through
        // the same door as the palette's "Mark as read", not a side write
        // that peers/history cannot account for.  The carried position keeps
        // a message that arrives meanwhile unread.  Nobody pressed anything
        // for it, so a refusal is not said over the window.
        self.read_asked = Some((self.active_channel.clone(), read));
        self.submit_on_its_own(crate::resource::Chat::MARK_READ.with(
            crate::record::chat::MarkRead {
                channel: ui::tilo_resource::Id::from_raw(self.active_channel.clone()),
                part: u64::try_from(read.part).unwrap_or_default(),
                seen: u32::try_from(read.seen).unwrap_or(u32::MAX),
            },
        ));
        if let Some(channel) = self
            .channels
            .iter_mut()
            .find(|channel| channel.id == self.active_channel)
        {
            channel.unread = 0;
        }
    }
}
