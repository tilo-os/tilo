//! Workspace settings' Emoji tab: the active workspace's emoji record,
//! opened here the way every app window of the room opens it
//! ([`tilo_app::framework::WorkspaceRecord`]), in the store the session's
//! in-process apps share, so the lens and the room's windows hold the one
//! record. Adding and removing are the record's own actions, decided by the
//! room's grants at the node of the app that keeps it, as from any window.

use std::rc::Rc;
use std::sync::Arc;

use tilo_app::framework::WorkspaceRecord;
use tilo_app::tilo_workspace_emoji::{OWNER, WorkspaceEmoji as Record, workspace_emoji as actions};
use tilo_desktop::workspace_settings::emoji_pane::{
    EmojiEntry, EmojiGrants, EmojiNotice, EmojiPaneModel, Removal, small_refusal,
};
use ui::workspace_emoji::{CustomEmoji, Small, WorkspaceEmoji, make_small};

use super::ShellOverlayHost;

/// Workspace settings changes the record as the session's own settings:
/// how its history names where a change came from.
const SETTINGS_APP: (&str, &str) = ("shell", "Workspace settings");

/// The most a chosen file may be before it is made small.
const MAX_CHOSEN_BYTES: usize = 8 * 1024 * 1024;

/// The workspace's emoji as the Emoji tab holds them.
pub(super) struct WsEmoji {
    room: String,
    author: String,
    /// `None` where no record could be opened (no in-process apps here).
    record: Option<WorkspaceRecord<Record>>,
    /// The emoji as last read, with the record's revision then.
    read: Option<(u64, Vec<EmojiEntry>, Rc<WorkspaceEmoji>)>,
    /// Pictures the person chose, waiting for the chooser's answer.
    choosing: Vec<ui::portal::ContentsChannel>,
    /// Pictures chosen whose making small waits for the image tools.
    waiting: Vec<ui::portal::PickedContents>,
    notice: Option<EmojiNotice>,
}

impl WsEmoji {
    fn open(
        apps: Option<&crate::shell::in_process::SharedInProcessApps>,
        room: &str,
        author: &str,
    ) -> Self {
        let record = apps.and_then(|apps| {
            let records = apps.borrow().workspace_records(room, author);
            WorkspaceRecord::<Record>::open(
                &records,
                OWNER,
                SETTINGS_APP,
                None,
                Some(ui::WakeHandle::new()),
            )
        });
        Self {
            room: room.to_owned(),
            author: author.to_owned(),
            record,
            read: None,
            choosing: Vec::new(),
            waiting: Vec::new(),
            notice: None,
        }
    }

    /// Read the record again when it moved.
    fn refresh(&mut self) {
        let Some(record) = &self.record else {
            return;
        };
        let revision = record.revision();
        if self
            .read
            .as_ref()
            .is_some_and(|(read, ..)| *read == revision)
        {
            return;
        }
        let state = record.get();
        let entries = state
            .emoji
            .iter()
            .map(|(name, emoji)| EmojiEntry {
                name: name.clone(),
                added_by: emoji.added_by.clone(),
            })
            .collect();
        let pictures = Rc::new(WorkspaceEmoji::new(
            state
                .emoji
                .iter()
                .map(|(name, emoji)| CustomEmoji {
                    name: name.clone(),
                    image: Arc::from(emoji.image.bytes()),
                })
                .collect(),
        ));
        self.read = Some((revision, entries, pictures));
    }

    /// What the Emoji tab shows, with `grants` deciding its controls.
    pub(super) fn model(&self, grants: EmojiGrants) -> EmojiPaneModel<'_> {
        let Some((_, entries, pictures)) = &self.read else {
            return EmojiPaneModel::unavailable(&self.author);
        };
        EmojiPaneModel {
            available: self.record.is_some(),
            entries,
            pictures: Some(pictures),
            grants,
            me: &self.author,
            notice: self.notice.as_ref(),
            choosing: !self.choosing.is_empty(),
        }
    }

    fn add_small(
        record: &WorkspaceRecord<Record>,
        file: &str,
        small: &Small,
    ) -> Result<(), EmojiNotice> {
        let Small::Ready(png) = small else {
            return Err(EmojiNotice::not_added(
                file,
                small_refusal(small).unwrap_or_default(),
            ));
        };
        let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
        let name = record
            .get()
            .free_name(stem)
            .ok_or_else(|| EmojiNotice::unnamed(file))?;
        record
            .invoke(&Record::ADD_EMOJI.with(actions::AddEmoji {
                name,
                image: tilo_app::tilo_workspace_emoji::EmojiImage(png.clone()).to_base64(),
            }))
            .map_err(|error| EmojiNotice::not_added(file, error.to_string()))
    }

    /// Take up what the chooser answered and add each picture chosen.
    /// Whether anything changed.
    fn settle(&mut self) -> bool {
        if self.choosing.is_empty() && self.waiting.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut chosen = std::mem::take(&mut self.waiting);
        let mut notice = None;
        self.choosing.retain(|channel| match channel.try_recv() {
            Ok(ui::portal::ContentsOutcome::Picked(files)) => {
                chosen.extend(files);
                changed = true;
                false
            }
            Ok(ui::portal::ContentsOutcome::Failed(error)) => {
                tracing::warn!(%error, "no picture was chosen for an emoji");
                notice = Some(EmojiNotice::not_chosen(error));
                changed = true;
                false
            }
            Ok(ui::portal::ContentsOutcome::Cancelled)
            | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                changed = true;
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
        });
        for file in chosen {
            let small = make_small(&file.bytes);
            if small == Small::Pending {
                self.waiting.push(file);
                continue;
            }
            let added = match &self.record {
                Some(record) => Self::add_small(record, &file.name, &small),
                None => Err(EmojiNotice::unavailable(&file.name)),
            };
            if let Err(why) = added {
                tracing::warn!(file = %file.name, why = ?why.detail, "the chosen picture was not added as an emoji");
                notice = Some(why);
            }
            changed = true;
        }
        if notice.is_some() {
            self.notice = notice;
        }
        changed
    }
}

/// What the Emoji tab shows: `held`'s emoji with `grants` deciding its
/// controls, or nothing while they are not open (another tab is shown).
pub(super) fn pane_model<'a>(
    held: Option<&'a WsEmoji>,
    grants: Option<EmojiGrants>,
    acting_user: &'a str,
) -> EmojiPaneModel<'a> {
    match (held, grants) {
        (Some(held), Some(grants)) => held.model(grants),
        _ => EmojiPaneModel::unavailable(acting_user),
    }
}

impl ShellOverlayHost {
    /// Open the active workspace's emoji when the Emoji tab is shown (only
    /// then: the record opens on first use) and answer the grants deciding
    /// its controls.
    pub(super) fn prepare_ws_emoji(&mut self) -> Option<EmojiGrants> {
        let shown = self.chrome.as_ref().is_some_and(|chrome| {
            chrome.borrow().ws_settings_tab == tilo_desktop::workspace_settings::WsTab::Emoji
        });
        if !shown {
            return None;
        }
        let (room, author, grants) = self.active_emoji_grants()?;
        self.ws_emoji_for(&room, &author);
        Some(grants)
    }

    /// The active workspace's emoji, opened for `room` as `author` when they
    /// are not yet, read again when they moved.
    pub(super) fn ws_emoji_for(&mut self, room: &str, author: &str) -> &WsEmoji {
        let stale = self
            .ws_emoji
            .as_ref()
            .is_none_or(|held| held.room != room || held.author != author);
        if stale {
            let opened = self.chrome.as_ref().map_or_else(
                || WsEmoji::open(None, room, author),
                |chrome| WsEmoji::open(chrome.borrow().hosted_inputs.in_process(), room, author),
            );
            self.ws_emoji = Some(opened);
        }
        let held = self
            .ws_emoji
            .get_or_insert_with(|| WsEmoji::open(None, room, author));
        held.refresh();
        held
    }

    /// What the room's grants let the person here do with the active
    /// workspace's emoji now, and the room and person they are for.
    pub(super) fn active_emoji_grants(&self) -> Option<(String, String, EmojiGrants)> {
        let state = self.chrome.as_ref()?.borrow();
        let forced = state.test_only_owner_override;
        let tree = state.tree();
        let user = state.active_local_user_in(&tree);
        let ws = tree.desktop.active_workspace()?;
        let grants = forced.map_or_else(
            || EmojiGrants::of(ws, &user, tilo_clock::unix_now()),
            |forced| EmojiGrants {
                add: forced,
                remove_own: forced,
                remove_any: forced,
            },
        );
        let room = ws.room().to_string();
        drop(tree);
        drop(state);
        Some((room, user, grants))
    }

    /// Open the chooser for a picture to add, when the person may add one.
    pub(super) fn choose_workspace_emoji(&mut self) {
        let Some((room, author, grants)) = self.active_emoji_grants() else {
            return;
        };
        if !grants.add {
            return;
        }
        self.ws_emoji_for(&room, &author);
        let Some(held) = self.ws_emoji.as_mut() else {
            return;
        };
        if held.record.is_none() || !held.choosing.is_empty() {
            return;
        }
        // The toolkit's chooser for an emoji's picture, as every app's
        // emoji picker opens it.
        let lookup = ui::i18n::I18nManager::lookup_plain;
        let request = ui::portal::PickRequest::open(
            &lookup("workspace-emoji-choose-title"),
            &lookup("workspace-emoji-choose-accept"),
        )
        .filter(
            ui::portal::FileFilter::new(&ui::i18n::I18nManager::lookup_plain(
                "workspace-emoji-image-files",
            ))
            .glob("*.png")
            .glob("*.jpg")
            .glob("*.jpeg")
            .glob("*.webp")
            .mimetype("image/png")
            .mimetype("image/jpeg")
            .mimetype("image/webp"),
        )
        .max_contents_bytes(MAX_CHOSEN_BYTES)
        .thread_name("workspace-emoji-choose");
        held.notice = None;
        held.choosing.push(request.send_contents());
        self.bump_chrome();
    }

    /// Remove the emoji `name` as `how`, when the room's grants let the
    /// person; why not reaches them in the tab.
    pub(super) fn remove_workspace_emoji(&mut self, name: &str, how: Removal) {
        let Some((room, author, grants)) = self.active_emoji_grants() else {
            return;
        };
        let allowed = match how {
            Removal::Own => grants.remove_own,
            Removal::Moderation => grants.remove_any,
        };
        if !allowed {
            return;
        }
        self.ws_emoji_for(&room, &author);
        let Some(held) = self.ws_emoji.as_mut() else {
            return;
        };
        let Some(record) = &held.record else {
            return;
        };
        let removal = match how {
            Removal::Own => Record::REMOVE_EMOJI.with(actions::RemoveEmoji { name: name.into() }),
            Removal::Moderation => {
                Record::REMOVE_ANY_EMOJI.with(actions::RemoveAnyEmoji { name: name.into() })
            }
        };
        held.notice = record
            .invoke(&removal)
            .err()
            .map(|error| EmojiNotice::not_removed(name, error.to_string()));
        self.bump_chrome();
    }

    pub(super) fn dismiss_workspace_emoji_notice(&mut self) {
        if let Some(held) = self.ws_emoji.as_mut() {
            held.notice = None;
        }
        self.bump_chrome();
    }

    /// Add `pictures` (a name and a PNG, JPEG or WebP each) to the active
    /// workspace's emoji as the person here, the way the tab's Add does once
    /// a picture is chosen: for a capture of the Emoji tab.
    ///
    /// # Errors
    /// The record is not open here, or refused one.
    pub fn seed_workspace_emoji_for_shots(
        &mut self,
        pictures: &[(&str, Vec<u8>)],
    ) -> Result<(), String> {
        let (room, author, _) = self
            .active_emoji_grants()
            .ok_or_else(|| "no active workspace".to_owned())?;
        self.ws_emoji_for(&room, &author);
        let held = self
            .ws_emoji
            .as_ref()
            .ok_or_else(|| "the workspace's emoji are not open".to_owned())?;
        let record = held
            .record
            .as_ref()
            .ok_or_else(|| "the workspace's emoji are not open".to_owned())?;
        for (name, picture) in pictures {
            WsEmoji::add_small(record, &format!("{name}.png"), &make_small(picture))
                .map_err(|notice| format!("{}: {:?}", notice.title, notice.detail))?;
        }
        Ok(())
    }

    /// Scroll the Emoji tab's grid when `event` is a wheel over the open
    /// lens and the grid has more than it shows. Whether it took the event.
    pub(super) fn route_ws_emoji_wheel(&mut self, event: &winit::event::WindowEvent) -> bool {
        let winit::event::WindowEvent::MouseWheel { delta, .. } = event else {
            return false;
        };
        if !self.overlay_flags().ws_settings
            || !self
                .last_cursor
                .is_some_and(|p| self.ws_settings_rect().contains(p))
            || self.workspace_settings_overlay.emoji_layout().max_scroll <= 0.0
        {
            return false;
        }
        let dy: f64 = match *delta {
            winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * 30.0,
            winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
        };
        if self.workspace_settings_overlay.scroll_emoji(dy) {
            self.bump_chrome();
        }
        true
    }

    /// Take up the chooser's answers, once a frame.
    pub(super) fn settle_workspace_emoji(&mut self) {
        if self.ws_emoji.as_mut().is_some_and(WsEmoji::settle) {
            self.bump_chrome();
        }
    }
}
