//! What the window asks the player for, one request per verb: status, focus, the library's rows, shared playlists and bookmarks, and the playhead presence publishes.

use super::*;

impl PlayerApp {
    /// Invoke `effect` through the framework's door, as the palette and
    /// agents do: its availability and permission are checked there.
    pub(super) fn request(&self, effect: &VideoEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    pub(super) fn formats_visible(&self) -> bool {
        self.missing_codec.is_some() && self.open_error.is_some()
    }

    /// Where the empty state's action (Open a video, or the formats remedy)
    /// stands among the player's accessible children, while it shows.
    pub(super) fn status_access_index(&self) -> Option<usize> {
        self.accessibility()?.children.iter().position(|node| {
            matches!(
                node.action_target.as_deref(),
                Some("video-player.formats" | "video-player.open")
            )
        })
    }

    /// Give the keyboard to the library sheet's control at `path`, the
    /// order [`Widget::accessibility`] lists them in: "Back to video" where
    /// the library fills the window, the three tabs, then the shared tab's
    /// actions and rows or the local rows.
    pub(super) fn focus_player_control(&mut self, path: &[usize]) -> bool {
        if !self.library_visible {
            return false;
        }
        let back = self.library_back_rect.width() > 0.0;
        let index = match path {
            [0] if back => {
                self.library_focus_id = Some("library-back".to_owned());
                self.dirty = true;
                return true;
            }
            [index] => index.saturating_sub(usize::from(back)),
            _ => return false,
        };
        let shared = self.library_tab == LibraryTab::Shared;
        let focus = match index {
            0 => Some("library-tab-queue".to_owned()),
            1 => Some("library-tab-recent".to_owned()),
            2 => Some("library-tab-shared".to_owned()),
            3 if shared => Some("shared-watch-later".to_owned()),
            4 if shared => Some("shared-bookmark".to_owned()),
            index if shared => shared_accessibility_targets(self)
                .get(index.saturating_sub(5))
                .map(|(focus, _, _)| focus.clone()),
            index => self.local_row_focus(index.saturating_sub(3)),
        };
        let Some(focus) = focus else {
            return false;
        };
        self.library_focus_id = Some(focus);
        self.dirty = true;
        true
    }

    /// The focus id of the local list's control at `library_index`: each
    /// row's Play, then Clear on the recent tab, then each row's Remove.
    pub(super) fn local_row_focus(&self, library_index: usize) -> Option<String> {
        let play_count = self.library_row_hits.len();
        let clears = self.library_tab == LibraryTab::Recent && !self.recent.is_empty();
        if let Some(row) = self.library_row_hits.get(library_index) {
            Some(row.id.clone())
        } else if clears && library_index == play_count {
            Some("library-clear".into())
        } else {
            let remove_index = library_index.saturating_sub(play_count + usize::from(clears));
            self.library_row_hits
                .iter()
                .filter_map(|row| row.remove.as_ref().map(|_| row.id.as_str()))
                .nth(remove_index)
                .map(|id| format!("library-remove-{id}"))
        }
    }

    /// Where the local list's control at `library_index` is, in the order
    /// [`Self::local_row_focus`] names them.
    pub(super) fn local_row_rect(&self, library_index: usize) -> Option<Rect> {
        let play_count = self.library_row_hits.len();
        let clears = self.library_tab == LibraryTab::Recent && !self.recent.is_empty();
        if let Some(row) = self.library_row_hits.get(library_index) {
            Some(row.play)
        } else if clears && library_index == play_count {
            Some(self.library_clear_rect)
        } else {
            let remove_index = library_index.saturating_sub(play_count + usize::from(clears));
            self.library_row_hits
                .iter()
                .filter_map(|row| row.remove)
                .nth(remove_index)
        }
    }

    pub(super) fn request_status_action(&self) {
        if self.formats_visible() {
            self.request(&VideoEffect::OpenFormats);
        } else {
            self.request_open_action();
        }
    }

    pub(super) fn request_open_action(&self) {
        self.request(&VideoEffect::OpenVideo { path: None });
    }

    pub(super) fn request_library_play(&self, item_id: &str) {
        let resource = if self.queue.iter().any(|item| item.id == item_id) {
            crate::actions::queue_item_resource_id(item_id)
        } else {
            crate::actions::recent_item_resource_id(item_id)
        };
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect_on(
                &resource,
                &VideoEffect::PlayQueued {
                    item: item_id.to_owned(),
                },
            );
        }
    }

    pub(super) fn request_library_remove(&self, item_id: &str) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect_on(
                &crate::actions::queue_item_resource_id(item_id),
                &VideoEffect::RemoveQueued {
                    item: item_id.to_owned(),
                },
            );
        }
    }

    pub(super) fn request_library_clear(&self) {
        self.request(&VideoEffect::ClearRecent);
    }

    pub(super) fn request_library_hide(&self) {
        self.request(&VideoEffect::HideLibrary);
    }

    /// Back to the picture from a library that fills the window: the library
    /// is one action away again, through the bar's sidebar toggle, the
    /// `L` shortcut or the palette.
    pub(super) fn request_video_show(&mut self) {
        self.request_library_hide();
        self.player.reveal_controls();
        self.dirty = true;
    }

    pub(super) fn request_library_tab(&self, tab: LibraryTab) {
        self.request(&VideoEffect::ShowLibrary {
            tab: tab.name().to_owned(),
        });
    }

    /// The Library button and `L` shortcut choose a concrete, replay-safe
    /// effect from the current presentation. Nothing is a toggle: a replay
    /// or delayed agent invocation must not invert a newer user choice.
    pub(super) fn request_library_visibility_change(&self) {
        if self.library_visible {
            self.request_library_hide();
        } else {
            self.request_library_tab(self.library_tab);
        }
    }

    pub(super) fn request_annotation_jump(&self, annotation_id: &str) {
        self.request(&VideoEffect::JumpToAnnotation {
            annotation: annotation_id.to_owned(),
        });
    }

    /// Submit a shared library verb or field edit through the framework's
    /// door. The library shown follows once the document presents the
    /// change.
    pub(super) fn dispatch_shared(&self, submit: impl ui::framework::Submit<PlayerResource>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(submit);
        }
    }

    pub(super) fn delete_annotation(&self, id: &str) {
        self.dispatch_shared(PlayerResource::DELETE_ANNOTATION.with(Id::from_raw(id)));
    }

    pub(super) fn delete_playlist(&self, id: &str) {
        self.dispatch_shared(PlayerResource::DELETE_PLAYLIST.with(Id::from_raw(id)));
    }

    /// The open video, added to the shared Watch later playlist; `None`
    /// without an open video or when it is already there.
    pub(super) fn watch_later(&self) -> Option<verbs::AddToWatchLater> {
        let media_ref = self.current_media_ref.clone()?;
        (!self.shared_library.on_watch_later(&media_ref)).then(|| verbs::AddToWatchLater {
            media_ref,
            title: self.title.clone(),
        })
    }

    pub(super) fn add_current_to_watch_later(&self) {
        if let Some(add) = self.watch_later() {
            self.dispatch_shared(PlayerResource::ADD_TO_WATCH_LATER.with(add));
        }
    }

    /// A bookmark of the moment the player shows, on the open video.
    pub(super) fn bookmark(&self) -> Option<verbs::AddAnnotation> {
        Some(verbs::AddAnnotation {
            media_ref: self.current_media_ref.clone()?,
            time_ms: self.player.position_ms(),
            text: None,
        })
    }

    pub(super) fn bookmark_current_time(&self) {
        if let Some(bookmark) = self.bookmark() {
            self.dispatch_shared(PlayerResource::ADD_ANNOTATION.with(bookmark));
        }
    }

    /// The shared library verbs bound to the playhead, for the palette and
    /// agents: bookmark the moment shown, add the open video to Watch later.
    pub(crate) fn playhead_declarations(&self) -> Vec<ActionDecl<PlayerResource>> {
        let Some(bookmark) = self.bookmark() else {
            return Vec::new();
        };
        let paths = verbs::AddAnnotation::paths();
        let mut declarations: Vec<ActionDecl<PlayerResource>> = PlayerResource::ADD_ANNOTATION
            .bind(paths.media_ref, bookmark.media_ref)
            .and(paths.time_ms, bookmark.time_ms)
            .declaration("bookmark-current-time", "Bookmark this moment")
            .into_iter()
            .map(|decl| decl.describe("Add a shared bookmark at the playback time shown."))
            .collect();
        if let Some(add) = self.watch_later() {
            let paths = verbs::AddToWatchLater::paths();
            declarations.extend(
                PlayerResource::ADD_TO_WATCH_LATER
                    .bind(paths.media_ref, add.media_ref)
                    .and(paths.title, add.title)
                    .declaration(
                        "add-this-video-to-watch-later",
                        "Add this video to Watch later",
                    )
                    .map(|decl| {
                        decl.describe("Add the open video to the shared Watch later playlist.")
                    }),
            );
        }
        declarations
    }
}
