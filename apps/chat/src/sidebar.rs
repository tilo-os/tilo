//! The room's channels in the window's shell (`resource.rs`): the shell
//! keeps the sidebar, its width, its drawer in a narrow window and its
//! keys, and this window says what it lists.
//!
//! The room is the one truth. Each settle this window declares the
//! sidebar's places (the person's contacts, the channels outside any
//! category, then each category as a section) through a [`NavFeed`];
//! what the person chooses, folds, asks a menu of and renames there comes
//! back as the window's own hits and actions.

use std::collections::BTreeMap;

use ui::icons::Icon;
use ui::widget::{Element, NavAction, NavFeed, NavItem, NavList, NavSection, WindowBarLink};

use crate::app::{ChatApp, Hit, MenuAction, MenuTarget};
use crate::model::{Channel, ChannelKind};

const CONTACTS: &str = "contacts";
const CHANNELS: &str = "channels";
const ADD_CHANNEL: &str = "add-channel";

/// The sidebar id of channel `id`.
pub(crate) fn channel_place(id: &str) -> String {
    format!("channel:{id}")
}

/// The sidebar id of category `id`'s section.
pub(crate) fn category_place(id: &str) -> String {
    format!("category:{id}")
}

fn person_place(key: &str) -> String {
    format!("person:{key}")
}

/// What a sidebar id names.
#[derive(Debug, PartialEq, Eq)]
enum Place {
    Channel(String),
    Category(String),
    Person(String),
    /// Someone in voice channel `channel`'s call.
    InCall {
        channel: String,
    },
}

fn place(id: &str) -> Option<Place> {
    if let Some(channel) = id.strip_prefix("channel:") {
        return Some(Place::Channel(channel.to_owned()));
    }
    if let Some(category) = id.strip_prefix("category:") {
        return Some(Place::Category(category.to_owned()));
    }
    if let Some(key) = id.strip_prefix("person:") {
        return Some(Place::Person(key.to_owned()));
    }
    let rest = id.strip_prefix("in-call:")?;
    let (channel, _) = rest.split_once('/')?;
    Some(Place::InCall {
        channel: channel.to_owned(),
    })
}

/// The shell's sidebar: the list `places` shows.
pub(crate) fn sidebar(places: &NavFeed) -> Element {
    Box::new(
        NavList::new(Vec::new())
            .with_id("chat.channels")
            .with_accessible_name(crate::i18n::t("chat-channels-title"))
            .fed_by(places)
            .fed_menus(places)
            .fed_renames(places),
    )
}

impl ChatApp {
    /// This window in a shell: its sidebar shows what `places` shows, and
    /// the shell's own buttons join the window bar through `bar`.
    #[must_use]
    pub fn in_shell(mut self, places: &NavFeed, bar: WindowBarLink) -> Self {
        self.places = places.clone();
        self.bar = Some(bar);
        self.show_sidebar();
        self
    }

    /// Hand the shell's sidebar the room's places, with the conversation
    /// that is open chosen.
    pub(crate) fn show_sidebar(&self) {
        let mut sections = Vec::new();
        if !self.person_contacts.is_empty() {
            let contacts = self
                .person_contacts
                .iter()
                .map(|contact| {
                    NavItem::new(
                        person_place(&contact.public_key),
                        &self.contact_name(contact),
                    )
                })
                .collect();
            sections.push(NavSection::new(
                CONTACTS,
                crate::i18n::t("chat-contacts-section"),
                contacts,
            ));
        }
        let drafts = self.draft_lines();
        sections.push(NavSection::untitled(
            CHANNELS,
            self.channel_items("", &drafts),
        ));
        let keeps_layout = self.keeps_layout();
        for category in self.sidebar_categories() {
            let mut section = NavSection::new(
                category_place(&category.id),
                &category.name,
                self.channel_items(&category.id, &drafts),
            )
            .collapsible()
            .with_menu();
            if category.collapsed {
                section = section.collapsed();
            }
            if keeps_layout {
                section = section
                    .trailing_action(NavAction::new(
                        ADD_CHANNEL,
                        Icon::Plus,
                        crate::i18n::t("chat-add-channel"),
                    ))
                    .editable_title();
            }
            sections.push(section);
        }
        // The chosen row is what the content shows: the call while its
        // stage fills the window, else the conversation open.
        let selected = if self.call.active && self.call.expanded {
            Some(channel_place(&self.call.channel))
        } else if let Some(key) = &self.direct_person {
            Some(person_place(key))
        } else {
            (!self.active_channel.is_empty()).then(|| channel_place(&self.active_channel))
        };
        self.places.show(sections, selected);
    }

    /// The channels of one category (`""` for those outside any), each
    /// voice channel holding the people in its call.
    fn channel_items(&self, category: &str, drafts: &BTreeMap<String, String>) -> Vec<NavItem> {
        self.category_channels(category)
            .into_iter()
            .map(|channel| self.channel_item(channel, drafts))
            .collect()
    }

    /// What the person has not sent, in every channel that has something:
    /// read once, so a channel's row costs a lookup rather than a document
    /// read.
    ///
    /// A draft is shown where its channel is listed, so a half-written
    /// message is found again rather than lost in a composer the person has
    /// to remember to open.
    fn draft_lines(&self) -> BTreeMap<String, String> {
        let Some(record) = &self.drafts_record else {
            return BTreeMap::new();
        };
        let keys: Vec<String> = record.get().drafts.keys().cloned().collect();
        record
            .record()
            .read(|doc| {
                keys.into_iter()
                    .filter_map(|key| {
                        // A thread's draft is keyed `channel::t::message`; the
                        // channel's own row speaks for it.
                        let channel = key.split_once("::").map_or(key.as_str(), |(at, _)| at);
                        let text = crate::projection::draft_text(doc, &key);
                        let line = crate::drafts::line(&text)?;
                        Some((channel.to_owned(), line))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn channel_item(&self, channel: &Channel, drafts: &BTreeMap<String, String>) -> NavItem {
        let voice = channel.kind == ChannelKind::Voice;
        let mut item = NavItem::new(channel_place(&channel.id), &channel.name)
            .icon(if voice { Icon::Volume2 } else { Icon::Hash })
            // Unread talk reads in the name; a mention of you is the count.
            .unread(channel.unread > 0 && !voice);
        if channel.mentions > 0 && !voice {
            item = item.live_count(channel.mentions);
        }
        if let Some(draft) = drafts.get(&channel.id) {
            item = item.secondary(draft.clone());
        }
        if self.keeps_layout() {
            item = item.editable_label();
        }
        if voice {
            let now = ui::time::now();
            let people: Vec<NavItem> = self
                .voice_people(&channel.id)
                .into_iter()
                .map(|person| {
                    let name = self.call_name(&person);
                    let speaking = self.speaking.level(&person, now) > 0.0;
                    NavItem::new(format!("in-call:{}/{person}", channel.id), name)
                        .person(&person, self.call_colour(&person), speaking)
                        .person_face(self.call_face_name(&person), self.call_picture(&person))
                        .without_actions()
                })
                .collect();
            if !people.is_empty() {
                item = item.children(people).expanded();
            }
            // Before anyone joins: whose voices this channel's call also
            // reaches.
            let services = self.call_bridges(&channel.id);
            if !services.is_empty() {
                item = item.secondary(crate::i18n::t_args(
                    "chat-call-bridged-short",
                    &[("services", services.join(", ").into())],
                ));
            }
        }
        item
    }

    /// Run what the person did in the shell's sidebar as the window's own
    /// hits and actions, then show the sidebar as they leave it.
    pub(crate) fn take_sidebar_events(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if let Some(asked) = self.sidebar_rename.take() {
            // Asked from assistive technology: no event hands over the
            // keyboard, and the field opens all the same.
            self.places.begin_rename(&asked, &mut None);
        }
        for chosen in self.places.take_chosen() {
            match place(&chosen) {
                Some(Place::Channel(id) | Place::InCall { channel: id }) => {
                    self.on_hit(Hit::Channel(id), doc);
                }
                Some(Place::Person(key)) => self.on_hit(Hit::DirectPerson(key), doc),
                Some(Place::Category(_)) | None => {}
            }
        }
        for (section, action) in self.places.take_section_actions() {
            if let (Some(Place::Category(category)), ADD_CHANNEL) = (place(&section), &*action) {
                self.on_hit(Hit::AddChannel { category }, doc);
            }
        }
        for (id, open) in self.places.take_expands() {
            if let Some(Place::Category(category)) = place(&id)
                && self.collapsed.contains(&category) == open
            {
                self.toggle_category(&category);
            }
        }
        for (id, lift, at) in self.places.take_menus() {
            self.open_sidebar_menu(&id, lift, at);
        }
        for (id, name) in self.places.take_renames() {
            let target = match place(&id) {
                Some(Place::Channel(channel)) => MenuTarget::Channel(channel),
                Some(Place::Category(category)) => MenuTarget::Category(category),
                _ => continue,
            };
            self.rename_place(&target, &name, doc);
        }
        self.show_sidebar();
    }

    /// The menu of the channel or category `id`, opening at `at` with the
    /// row `lift` marked.
    pub(crate) fn open_sidebar_menu(
        &mut self,
        id: &str,
        lift: vello::kurbo::Rect,
        at: vello::kurbo::Point,
    ) {
        let (target, items) = match place(id) {
            Some(Place::Channel(channel)) if self.keeps_layout() => (
                MenuTarget::Channel(channel),
                MenuAction::for_channel().to_vec(),
            ),
            Some(Place::Category(category)) => (
                MenuTarget::Category(category),
                self.layout_menu(MenuAction::for_category().to_vec()),
            ),
            _ => return,
        };
        let mut menu = self.context_menu(target, at, items);
        menu.widget.anchor_row(lift);
        self.menu = Some(menu);
        self.canonical_dirty = true;
    }
}

#[cfg(test)]
impl ChatApp {
    /// The sections this window hands the shell's sidebar now.
    pub(crate) fn sidebar_sections(&self) -> Vec<NavSection> {
        self.show_sidebar();
        self.places
            .shown()
            .map(|(sections, _)| sections)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_place_names_what_it_shows() {
        assert_eq!(
            place(&channel_place("c_general")),
            Some(Place::Channel("c_general".into()))
        );
        assert_eq!(
            place(&category_place("cat_main")),
            Some(Place::Category("cat_main".into()))
        );
        assert_eq!(
            place(&person_place("ab12")),
            Some(Place::Person("ab12".into()))
        );
        assert_eq!(
            place("in-call:c_standup/u_dana"),
            Some(Place::InCall {
                channel: "c_standup".into()
            })
        );
        assert_eq!(place(CHANNELS), None);
    }
}
