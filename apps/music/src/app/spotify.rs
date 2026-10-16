//! Spotify jobs act on the provider and return its observed state. They are
//! not document mutations and never claim that remote audio played locally.
use super::MusicApp;
use crate::{
    actions::MusicEffect,
    backend::{SpotifyOperation, SpotifyReply},
};

pub(super) struct PendingJob {
    invocation: tilo_actions::InvocationId,
    started: ui::time::Instant,
    remote_play: bool,
    library_page: Option<u32>,
}
use ui::widget::Widget as _;

impl MusicApp {
    pub(super) fn open_sources_menu(&mut self) {
        self.context = ui::widget::context_menu::ContextMenu::new();
        self.menu_actions.clear();
        self.ctx_track = None;
        self.ctx_in_playlist = None;
        self.ctx_queue_index = None;
        self.menu_anchor_row = None;
        let choices = if self.spotify_catalogue {
            vec![
                ("music-browse-spotify", MusicEffect::BrowseSpotify),
                (
                    "music-connect-spotify-library",
                    MusicEffect::ConnectSpotifyLibrary,
                ),
                ("music-browse-youtube", MusicEffect::BrowseYouTube),
                (
                    "music-spotify-playback",
                    MusicEffect::ConnectSpotifyPlayback,
                ),
                ("music-disconnect-spotify", MusicEffect::DisconnectSpotify),
            ]
        } else {
            vec![
                ("music-connect-spotify", MusicEffect::ConnectSpotify),
                ("music-browse-youtube", MusicEffect::BrowseYouTube),
            ]
        };
        for (key, effect) in choices {
            self.push_menu(
                crate::i18n::t_args(key, &[]),
                super::Action::SpotifyEffect(effect),
            );
        }
        if self.spotify_catalogue
            && self.home.is_some()
            && self.spotify_library_more
            && self.spotify_library_offset <= 1950
        {
            self.push_menu(
                crate::i18n::t("music-spotify-library-more"),
                super::Action::SpotifyEffect(MusicEffect::SpotifyLibraryMore),
            );
        }
        let anchor = self
            .source_buttons
            .iter()
            .find(|button| button.test_id() == Some("music-sources"))
            .map_or_else(
                || vello::kurbo::Rect::from_origin_size(self.mouse, (0.0, 0.0)),
                ui::widget::Widget::get_rect,
            );
        self.context.set_text_scale(self.context_text_scale);
        self.context.open_anchored(anchor, self.rect);
        self.dirty = true;
    }

    pub(super) fn artist_followed(&self, id: &str) -> bool {
        if !id.starts_with("spotify:artist:") {
            return self.lib.is_artist_followed(id);
        }
        self.artist_pages
            .get(id)
            .and_then(|page| page.provider_followed)
            .unwrap_or_else(|| {
                self.home.as_ref().is_some_and(|home| {
                    home.sections
                        .iter()
                        .flat_map(|section| &section.items)
                        .any(|item| item.kind == super::HomeItemKind::Artist && item.id == id)
                })
            })
    }
    pub(super) fn refresh_spotify_control_flags(&mut self) {
        let page_ready = self.spotify_catalogue
            && self.home.is_some()
            && self.spotify_library_more
            && self.spotify_library_offset <= 1950
            && !self
                .spotify_jobs
                .values()
                .any(|job| job.library_page.is_some());
        for button in &mut self.source_buttons {
            match button.test_id() {
                Some("music-spotify-library-more") => button.set_enabled(page_ready),
                Some("music-spotify-create-playlist") => button.set_enabled(self.spotify_catalogue),
                _ => {}
            }
        }
    }

    pub(super) fn source_widget_mode(&self) -> u8 {
        match self.view {
            super::View::Home => 2,
            super::View::Playlists if self.spotify_catalogue => 3,
            super::View::Queue if self.spotify_catalogue || self.spotify_remote_queue.is_some() => {
                1
            }
            _ => 0,
        }
    }

    pub(super) fn spotify_remote_rows(&self) -> Vec<ui::framework::LiveRow> {
        let Some((snapshot, _)) = &self.spotify_remote_queue else {
            return Vec::new();
        };
        let mut rows: Vec<_> = snapshot
            .get("remote_queue")
            .and_then(|queue| queue.get("queue"))
            .unwrap_or(&serde_json::Value::Null)
            .as_array()
            .into_iter()
            .flatten()
            .take(50)
            .enumerate()
            .map(|(index, item)| {
                ui::framework::LiveRow::new(
                    format!("music.spotify-remote:{index}"),
                    "music.spotify-remote-item",
                    format!(
                        "{}\n{}",
                        crate::i18n::t("music-spotify-remote-queue"),
                        remote_item_label(item)
                    ),
                )
            })
            .collect();
        if let Some(current) = snapshot
            .get("remote_queue")
            .and_then(|queue| queue.get("currently_playing"))
            .filter(|current| current.is_object())
        {
            rows.insert(
                0,
                ui::framework::LiveRow::new(
                    "music.spotify-remote:current",
                    "music.spotify-remote-item",
                    remote_item_label(current),
                ),
            );
        }
        rows
    }
    /// A menu emits an inert typed intent; the host re-resolves the root's
    /// effect declaration and participant authority before starting the job.
    pub(super) fn queue_spotify_effect_control(&mut self, effect: &MusicEffect) {
        use ui::tilo_resource::Effect as _;
        let Some(spec) = MusicEffect::specs()
            .into_iter()
            .find(|spec| spec.id == effect.id())
        else {
            return;
        };
        let resource = tilo_actions::Resource::new("music.library", "music.library", "Music")
            .with_action(spec.clone());
        let arguments = effect.arguments();
        let Ok(binding) =
            tilo_actions::ActionBinding::new(&resource, &spec, move |()| arguments.clone())
        else {
            return;
        };
        if self.spotify_menu_controls.len() >= 16 {
            return;
        }
        let mut emitter = self.control_domain.as_ref().map_or_else(
            || ui::widget::ActionEmitter::new(binding.clone()),
            |domain| ui::widget::ActionEmitter::new_in(binding.clone(), domain),
        );
        if emitter.emit(()).is_ok() {
            self.spotify_menu_controls.push(emitter);
        }
    }

    pub(super) fn append_spotify_playlist_choices(&mut self, track: &str) {
        if !track.starts_with("spotify:track:") {
            return;
        }
        let playlists: Vec<_> = self
            .catalogue_entities()
            .into_iter()
            .filter(|(id, (_, kind))| {
                id.starts_with("spotify:playlist:")
                    && *kind == crate::backend::HomeItemKind::Playlist
            })
            .take(40)
            .collect();
        if !playlists.is_empty() {
            self.context
                .push(ui::widget::context_menu::MenuItem::separator());
            self.menu_actions.push(super::Action::Back);
        }
        for (playlist, (name, _)) in playlists {
            let label =
                crate::i18n::t_args("music-spotify-playlist-choice", &[("name", name.into())]);
            self.push_menu(
                label,
                super::Action::SpotifyEffect(MusicEffect::SpotifyPlaylistAdd {
                    playlist,
                    track: track.into(),
                }),
            );
        }
    }

    pub(super) fn open_spotify_playlist_menu(&mut self, playlist: &str) {
        self.context = ui::widget::context_menu::ContextMenu::new();
        self.menu_actions.clear();
        self.push_menu(
            crate::i18n::t("music-spotify-create-playlist"),
            super::Action::BeginNewSpotifyPlaylist,
        );
        self.push_menu(
            crate::i18n::t("music-spotify-rename-playlist"),
            super::Action::BeginRenamePlaylist(playlist.into()),
        );
        self.push_menu(
            crate::i18n::t("music-spotify-save-playlist"),
            super::Action::SpotifyEffect(MusicEffect::SpotifySave {
                uri: playlist.into(),
            }),
        );
        self.push_menu(
            crate::i18n::t("music-spotify-unsave-playlist"),
            super::Action::SpotifyEffect(MusicEffect::SpotifyUnsave {
                uri: playlist.into(),
            }),
        );
        self.open_context_menu_at_pointer();
    }

    pub(super) fn spotify_playlist_name(&self, playlist: &str) -> Option<String> {
        self.catalogue_entities()
            .get(playlist)
            .map(|(name, _)| name.clone())
    }
    pub(super) fn prepare_spotify_queue(&self) {
        use crate::model::RepeatMode;
        let next = if self.pb.shuffle || !self.pb.playing {
            None
        } else if self.pb.repeat == RepeatMode::One {
            self.pb.current.as_deref()
        } else {
            self.pb
                .queue
                .get(self.pb.queue_index.saturating_add(1))
                .or_else(|| {
                    (self.pb.repeat == RepeatMode::All)
                        .then(|| self.pb.queue.first())
                        .flatten()
                })
                .map(String::as_str)
        };
        self.audio
            .prepare_spotify_next(next.filter(|id| id.starts_with("spotify:track:")));
    }
    /// Capture-only provider fixtures. No grant, network request or claimed audio.
    pub(super) fn snap_spotify(&mut self, scenario: &str) {
        use crate::backend::{HomeContent, HomeItem, HomeItemKind, HomeSection, SpotifyDevice};
        self.backend_name = "Spotify".into();
        self.spotify_catalogue = true;
        self.home_requested = true;
        self.home = Some(HomeContent {
            sections: vec![HomeSection {
                title: crate::i18n::t("music-spotify-library"),
                items: [
                    (
                        "spotify:collection:liked",
                        "Spotify Liked Songs",
                        "Your saved songs",
                    ),
                    (
                        "spotify:playlist:1234567890123456789012",
                        "After hours",
                        "A quiet place to land",
                    ),
                    (
                        "spotify:playlist:abcdefghijklmnopqrstuv",
                        "On the move",
                        "Keep your day moving",
                    ),
                ]
                .into_iter()
                .map(|(id, title, subtitle)| HomeItem {
                    id: id.into(),
                    title: title.into(),
                    subtitle: subtitle.into(),
                    kind: HomeItemKind::Playlist,
                    cover_url: String::new(),
                    video_id: String::new(),
                    artist_id: String::new(),
                    artist_name: String::new(),
                    album_id: String::new(),
                    album_title: String::new(),
                    duration_s: 0,
                })
                .collect(),
            }],
        });
        if scenario == "spotify-devices" {
            self.spotify_devices = vec![
                SpotifyDevice {
                    id: "studio".into(),
                    name: "Studio speakers".into(),
                    active: true,
                    restricted: false,
                },
                SpotifyDevice {
                    id: "phone".into(),
                    name: "Phone".into(),
                    active: false,
                    restricted: false,
                },
                SpotifyDevice {
                    id: "restricted".into(),
                    name: "Unavailable device".into(),
                    active: false,
                    restricted: true,
                },
            ];
            self.spotify_remote_state = serde_json::json!({"device":{"id":"studio","name":"Studio speakers"},"is_playing":false});
            self.rebuild_spotify_device_buttons();
        }
        if scenario == "spotify-error" {
            self.source_error = Some(crate::i18n::t("music-spotify-signin-expired"));
        }
        if scenario == "spotify-queue" {
            self.view = super::View::Queue;
            self.spotify_remote_queue = Some((
                serde_json::json!({"remote_queue":{
                "currently_playing":{"name":"Evening light","artists":[{"name":"Example artist"}]},
                "queue":[{"name":"On the move","artists":[{"name":"Example artist"}]}, {"name":"A conversation about music","type":"episode","show":{"name":"Example podcast"}}]
            },"truncated":false}),
                ui::time::now(),
            ));
        }
        if scenario == "spotify-create" {
            self.on_begin_new_playlist(None);
            self.name_prompt = Some(super::NamePrompt::NewSpotifyPlaylist);
            self.name_input = "Evening music".into();
        }
    }
    pub(super) fn queue_spotify_library_control(&mut self, track: &str, save: bool) {
        use ui::tilo_resource::Effect as _;
        let effect = if save {
            MusicEffect::SpotifySave { uri: track.into() }
        } else {
            MusicEffect::SpotifyUnsave { uri: track.into() }
        };
        let Some(resource) = self.publish_resource(&crate::actions::track_resource_id(track))
        else {
            return;
        };
        let Ok(binding) = tilo_actions::ActionBinding::unit_for(&resource, effect.id()) else {
            return;
        };
        if self.spotify_menu_controls.len() >= 16 {
            return;
        }
        let mut emitter = self.control_domain.as_ref().map_or_else(
            || ui::widget::ActionEmitter::new(binding.clone()),
            |domain| ui::widget::ActionEmitter::new_in(binding.clone(), domain),
        );
        if emitter.emit(()).is_ok() {
            self.spotify_menu_controls.push(emitter);
        }
    }
    pub(super) fn begin_spotify_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        effect: &MusicEffect,
    ) -> Result<ui::framework::Effected, String> {
        if !self.allow_network {
            return Err("Spotify needs network access".into());
        }
        if self.source_invocation.is_some() || self.spotify_jobs.len() >= 16 {
            return Err("Wait for the current Spotify requests to finish".into());
        }
        let operation = if matches!(effect, MusicEffect::SpotifyLibraryMore) {
            if !self.spotify_catalogue
                || self.home.is_none()
                || !self.spotify_library_more
                || self.spotify_library_offset > 1950
                || self
                    .spotify_jobs
                    .values()
                    .any(|job| job.library_page.is_some())
            {
                return Err(crate::i18n::t("music-spotify-library-page-unavailable"));
            }
            SpotifyOperation::LibraryPage {
                offset: self.spotify_library_offset,
                artists_after: self.home.as_ref().and_then(|home| {
                    home.sections
                        .iter()
                        .flat_map(|section| &section.items)
                        .filter(|item| item.kind == super::HomeItemKind::Artist)
                        .filter_map(|item| item.id.strip_prefix("spotify:artist:"))
                        .next_back()
                        .map(str::to_string)
                }),
            }
        } else {
            operation(effect).ok_or("Invalid Spotify operation")?
        };
        let library_page = if let SpotifyOperation::LibraryPage { offset, .. } = operation {
            Some(offset)
        } else {
            None
        };
        let remote_play = matches!(operation, SpotifyOperation::RemotePlay { .. });
        let request_id = self
            .next_worker_request_id()
            .ok_or("Music request identities exhausted")?;
        self.ensure_worker();
        self.worker
            .as_ref()
            .ok_or("Music worker unavailable")?
            .request(crate::stream::StreamRequest::Spotify {
                request_id,
                operation,
            })?;
        self.spotify_jobs.insert(
            request_id,
            PendingJob {
                invocation,
                started: ui::time::now(),
                remote_play,
                library_page,
            },
        );
        if matches!(effect, MusicEffect::SpotifyQueue) {
            self.view = super::View::Queue;
            self.set_scroll_y(0.0);
        }
        self.dirty = true;
        Ok(ui::framework::Effected::Pending)
    }

    pub(super) fn finish_spotify_operation(
        &mut self,
        request_id: u64,
        result: Result<SpotifyReply, String>,
    ) {
        let Some(job) = self.spotify_jobs.remove(&request_id) else {
            return;
        };
        let invocation = job.invocation;
        let result = result.and_then(|reply| {
            if let Some(offset) = job.library_page {
                self.merge_spotify_library_page(offset, &reply.state)?;
            }
            Ok(reply)
        });
        let update = match result {
            Ok(reply) => {
                if let Some(uri) = reply.state.get("uri").and_then(serde_json::Value::as_str)
                    && uri.starts_with("spotify:artist:")
                    && let Some(saved) = reply
                        .state
                        .get("saved")
                        .and_then(serde_json::Value::as_bool)
                    && let Some(page) = self.artist_pages.get_mut(uri)
                {
                    page.provider_followed = Some(saved);
                }
                if reply.state.get("remote_queue").is_some() {
                    self.spotify_remote_queue = Some((reply.state.clone(), ui::time::now()));
                }
                if job.remote_play {
                    self.audio.pause();
                    self.pb.playing = false;
                    self.prepare_spotify_queue();
                    self.report_device_profile_error();
                }
                if reply.state.get("device").is_some() || reply.state.is_null() {
                    self.spotify_devices = reply.devices;
                    self.rebuild_spotify_device_buttons();
                    self.spotify_remote_state = reply.state.clone();
                }
                if let Some(playlist) = reply
                    .state
                    .get("playlist")
                    .and_then(serde_json::Value::as_str)
                {
                    self.playlist_pages.remove(playlist);
                    self.playlist_requested.remove(playlist);
                    if reply
                        .state
                        .get("created")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                    {
                        self.view = super::View::Playlist(playlist.into());
                        self.set_scroll_y(0.0);
                    }
                }
                if reply.state.get("playlist").is_some() || reply.state.get("uri").is_some() {
                    self.home = None;
                    self.home_requested = false;
                    self.spotify_library_offset = 50;
                    self.spotify_library_more = true;
                }
                self.source_error = None;
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(reply.state),
                        resources: Vec::new(),
                    },
                }
            }
            Err(message) => {
                self.source_error = Some(message.clone());
                self.notify(super::ToastKind::Error, message.clone());
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "music.spotify_operation_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                }
            }
        };
        self.resource_stream_results.push(update);
        self.dirty = true;
    }

    fn merge_spotify_library_page(
        &mut self,
        offset: u32,
        state: &serde_json::Value,
    ) -> Result<(), String> {
        if offset != self.spotify_library_offset
            || state["library_page"].as_u64() != Some(u64::from(offset))
        {
            return Err(crate::i18n::t("music-spotify-library-page-unavailable"));
        }
        let page: crate::backend::HomeContent = serde_json::from_value(state["home_page"].clone())
            .map_err(|_| crate::i18n::t("music-spotify-library-page-invalid"))?;
        let more = state["has_more"]
            .as_bool()
            .ok_or_else(|| crate::i18n::t("music-spotify-library-page-invalid"))?;
        if page.sections.len() > 4 || page.sections.iter().any(|section| section.items.len() > 50) {
            return Err(crate::i18n::t("music-spotify-library-page-invalid"));
        }
        let mut home = self
            .home
            .clone()
            .ok_or_else(|| crate::i18n::t("music-spotify-library-page-unavailable"))?;
        for section in page.sections {
            if let Some(existing) = home
                .sections
                .iter_mut()
                .find(|existing| existing.title == section.title)
            {
                for item in section.items {
                    if !existing.items.iter().any(|known| known.id == item.id) {
                        existing.items.push(item);
                    }
                }
            } else {
                home.sections.push(section);
            }
        }
        self.absorb_home(&home);
        self.home = Some(home);
        self.spotify_library_offset = offset + 50;
        self.spotify_library_more = more;
        Ok(())
    }

    pub(super) fn expire_spotify_jobs(&mut self) {
        let expired: Vec<u64> = self
            .spotify_jobs
            .iter()
            .filter_map(|(id, job)| {
                (job.started.elapsed() > ui::time::Duration::from_secs(120)).then_some(*id)
            })
            .collect();
        for id in expired {
            if let Some(job) = self.spotify_jobs.remove(&id) {
                let message = crate::i18n::t("music-spotify-request-timeout");
                self.source_error = Some(message.clone());
                self.resource_stream_results
                    .push(tilo_actions::InvocationUpdate::Failed {
                        invocation: job.invocation,
                        error: tilo_actions::ActionFailure {
                            code: "music.spotify_timeout".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    });
                self.dirty = true;
            }
        }
    }

    fn rebuild_spotify_device_buttons(&mut self) {
        use ui::tilo_resource::Effect as _;
        self.spotify_device_buttons = self
            .spotify_devices
            .iter()
            .take(12)
            .flat_map(|device| {
                let mut effects = vec![
                    MusicEffect::SpotifyTransfer {
                        device: device.id.clone(),
                    },
                    MusicEffect::SpotifyRemotePause {
                        device: device.id.clone(),
                    },
                ];
                if let Some(track) = self
                    .pb
                    .current
                    .as_ref()
                    .filter(|track| track.starts_with("spotify:track:"))
                {
                    effects.push(MusicEffect::SpotifyRemotePlay {
                        device: device.id.clone(),
                        track: track.clone(),
                    });
                }
                effects.into_iter().filter_map(move |effect| {
                    let address = format!("music.spotify-device:{}", device.id);
                    let id = format!("{address}:{}", effect.id());
                    let key = match &effect {
                        MusicEffect::SpotifyRemotePause { .. } => "music-spotify-pause-device",
                        MusicEffect::SpotifyRemotePlay { .. } => "music-spotify-play-device",
                        _ => "music-spotify-select-device",
                    };
                    let label = crate::i18n::t_args(key, &[("device", (&device.name).into())]);
                    let resource =
                        tilo_actions::Resource::new(&address, "music.spotify-device", &device.name)
                            .with_action(effect.bound());
                    let binding =
                        tilo_actions::ActionBinding::unit_for(&resource, effect.id()).ok()?;
                    Some(
                        ui::widget::Button::new(&id, &label)
                            .with_enabled(!device.restricted)
                            .tooltip(crate::i18n::t("music-spotify-device-help"))
                            .on_action(binding),
                    )
                })
            })
            .collect();
        self.open_file_layout_visible = None;
    }

    fn catalogue_entities(
        &self,
    ) -> std::collections::BTreeMap<String, (String, crate::backend::HomeItemKind)> {
        use crate::backend::HomeItemKind;
        let mut entities = std::collections::BTreeMap::new();
        if let Some(home) = &self.home {
            for section in &home.sections {
                for item in &section.items {
                    entities.insert(item.id.clone(), (item.title.clone(), item.kind));
                }
            }
        }
        if let Some((_, bundle)) = &self.live_bundle {
            for album in &bundle.albums {
                entities.insert(album.id.clone(), (album.title.clone(), HomeItemKind::Album));
            }
            for artist in &bundle.artists {
                entities.insert(
                    artist.id.clone(),
                    (artist.name.clone(), HomeItemKind::Artist),
                );
            }
            for playlist in &bundle.playlists {
                entities.insert(
                    playlist.id.clone(),
                    (playlist.title.clone(), HomeItemKind::Playlist),
                );
            }
        }
        for page in self.artist_pages.values() {
            entities.insert(page.id.clone(), (page.name.clone(), HomeItemKind::Artist));
            for album in &page.albums {
                entities.insert(album.id.clone(), (album.title.clone(), HomeItemKind::Album));
            }
        }
        for page in self.album_pages.values() {
            entities.insert(page.id.clone(), (page.title.clone(), HomeItemKind::Album));
        }
        for page in self.playlist_pages.values() {
            entities.insert(
                page.id.clone(),
                (page.title.clone(), HomeItemKind::Playlist),
            );
        }
        entities
    }

    pub(super) fn catalogue_rows(&self) -> Vec<ui::framework::LiveRow> {
        use crate::backend::HomeItemKind;
        self.catalogue_entities()
            .into_iter()
            .filter_map(|(id, (label, kind))| {
                let (address, kind, place) = match kind {
                    HomeItemKind::Album => (
                        crate::actions::album_resource_id(&id),
                        "music.album",
                        crate::actions::Place::Album(id.clone()),
                    ),
                    HomeItemKind::Artist => (
                        crate::actions::artist_resource_id(&id),
                        "music.artist",
                        crate::actions::Place::Artist(id.clone()),
                    ),
                    HomeItemKind::Playlist => (
                        crate::actions::playlist_resource_id(&id),
                        "music.playlist",
                        crate::actions::Place::Playlist(id.clone()),
                    ),
                    HomeItemKind::Track => return None,
                };
                let mut row = ui::framework::LiveRow::new(address, kind, label)
                    .effect(&MusicEffect::Open { place });
                if id.starts_with("spotify:") && id != "spotify:collection:liked" {
                    row = if kind == "music.artist" {
                        row.operation(crate::actions::bound(
                            &MusicEffect::SpotifySave { uri: id.clone() },
                            &crate::i18n::t("music-spotify-follow-artist"),
                        ))
                        .operation(crate::actions::bound(
                            &MusicEffect::SpotifyUnsave { uri: id },
                            &crate::i18n::t("music-spotify-unfollow-artist"),
                        ))
                    } else {
                        row.effect(&MusicEffect::SpotifySave { uri: id.clone() })
                            .effect(&MusicEffect::SpotifyUnsave { uri: id })
                    };
                }
                Some(row)
            })
            .collect()
    }

    pub(super) fn known_catalogue_entity(&self, address: &str) -> bool {
        self.catalogue_rows().iter().any(|row| row.id() == address)
    }

    pub(super) fn spotify_device_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.spotify_devices
            .iter()
            .map(|device| {
                let mut row = ui::framework::LiveRow::new(
                    format!("music.spotify-device:{}", device.id),
                    "music.spotify-device",
                    if device.active {
                        format!(
                            "{} • {}",
                            device.name,
                            crate::i18n::t("music-spotify-active")
                        )
                    } else {
                        device.name.clone()
                    },
                );
                if !device.restricted {
                    row = row
                        .effect(&MusicEffect::SpotifyTransfer {
                            device: device.id.clone(),
                        })
                        .effect(&MusicEffect::SpotifyRemotePause {
                            device: device.id.clone(),
                        });
                    if let Some(track) = self
                        .pb
                        .current
                        .as_ref()
                        .filter(|id| id.starts_with("spotify:track:"))
                    {
                        row = row.effect(&MusicEffect::SpotifyRemotePlay {
                            device: device.id.clone(),
                            track: track.clone(),
                        });
                    }
                }
                row
            })
            .collect()
    }
}

pub(super) fn remote_item_label(item: &serde_json::Value) -> String {
    let title = item["name"].as_str().unwrap_or("?");
    let artist = item
        .get("artists")
        .and_then(|artists| artists.get(0))
        .and_then(|artist| artist.get("name"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            item.get("show")
                .and_then(|show| show.get("name"))
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("");
    if artist.is_empty() {
        title.into()
    } else {
        format!("{title}\n{artist}")
    }
}

impl ui::widget::controls::ActionControls for MusicApp {
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &ui::widget::controls::ControlResult,
    ) {
        if let Err(error) = result {
            self.notify(super::ToastKind::Error, error.to_string());
            self.dirty = true;
        }
        for child in ui::widget::Widget::children_mut(self) {
            ui::widget::controls::on_control_invocation_result(child, address, result);
        }
    }
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        self.control_domain = Some(domain.clone());
        // These retained controls appear on different destinations. Install the
        // same owner custody before a hidden control joins the physical tree.
        for button in &mut self.source_buttons {
            ui::widget::controls::install_control_custody(button, domain, attachment);
        }

        for emitter in &mut self.spotify_menu_controls {
            let _ = emitter.install_custody(domain, attachment);
        }
        for child in ui::widget::Widget::children_mut(self) {
            ui::widget::controls::install_control_custody(child, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        while !self.spotify_menu_controls.is_empty() {
            if let Some(invocation) = self.spotify_menu_controls.remove(0).take() {
                return Some(invocation);
            }
        }
        self.search_opener
            .take()
            .or_else(|| self.history.take())
            .or_else(|| {
                ui::widget::Widget::children_mut(self)
                    .into_iter()
                    .find_map(ui::widget::controls::take_control_invocation)
            })
    }

    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        bindings.extend(
            self.spotify_menu_controls
                .iter()
                .map(|emitter| emitter.metadata("music.spotify-menu")),
        );
        for child in ui::widget::Widget::children(self) {
            bindings.extend(ui::widget::controls::control_action_bindings(child));
        }
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for emitter in &mut self.spotify_menu_controls {
            emitter.refresh_availability(allowed);
        }
        for child in ui::widget::Widget::children_mut(self) {
            ui::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }

    fn has_pending_control_invocation(&self) -> bool {
        !self.spotify_menu_controls.is_empty()
            || ui::widget::Widget::children(self)
                .into_iter()
                .any(ui::widget::controls::has_pending_control_invocation)
    }
}

fn operation(effect: &MusicEffect) -> Option<SpotifyOperation> {
    Some(match effect {
        MusicEffect::SpotifyAccountStatus => SpotifyOperation::AccountStatus,
        MusicEffect::SpotifyDevices => SpotifyOperation::Devices,
        MusicEffect::SpotifyQueue => SpotifyOperation::Queue,
        MusicEffect::SpotifyCreatePlaylist { name } => {
            SpotifyOperation::CreatePlaylist { name: name.clone() }
        }
        MusicEffect::SpotifyTransfer { device } => SpotifyOperation::Transfer {
            device: device.clone(),
        },
        MusicEffect::SpotifyRemotePlay { device, track } => SpotifyOperation::RemotePlay {
            device: device.clone(),
            track: track.clone(),
        },
        MusicEffect::SpotifyRemotePause { device } => SpotifyOperation::RemotePause {
            device: device.clone(),
        },
        MusicEffect::SpotifySave { uri } | MusicEffect::SpotifyUnsave { uri } => {
            SpotifyOperation::Save {
                uri: uri.clone(),
                saved: matches!(effect, MusicEffect::SpotifySave { .. }),
            }
        }
        MusicEffect::SpotifyRenamePlaylist { playlist, name } => SpotifyOperation::RenamePlaylist {
            playlist: playlist.clone(),
            name: name.clone(),
        },
        MusicEffect::SpotifyPlaylistAdd { playlist, track } => SpotifyOperation::PlaylistAdd {
            playlist: playlist.clone(),
            track: track.clone(),
        },
        MusicEffect::SpotifyPlaylistRemove { playlist, track } => {
            SpotifyOperation::PlaylistRemove {
                playlist: playlist.clone(),
                track: track.clone(),
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Effect as _;
    use ui::widget::controls::ActionControls as _;

    #[test]
    fn spotify_status_and_provider_page_titles_are_accessible_without_shared_metadata() {
        let mut app = MusicApp::without_profile();
        app.snap_spotify("spotify-home");
        app.source_error = Some("Reconnect Spotify to grant library access".into());
        let tree = app.accessibility().unwrap();
        let page = &tree.children[2];
        assert!(page.children.iter().any(|node| node.name.as_deref()
            == Some(app.source_status().as_str())
            && node.description.as_deref() == app.source_error.as_deref()));
        assert!(
            !page
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some("Empty music library")),
            "remote collections do not announce an empty library"
        );
        let id = "spotify:artist:1234567890123456789012";
        app.artist_pages.insert(
            id.into(),
            crate::backend::ArtistPage {
                id: id.into(),
                name: "Provider artist".into(),
                provider_followed: Some(true),
                ..Default::default()
            },
        );
        app.view = super::super::View::Artist(id.into());
        assert_eq!(
            app.accessibility().unwrap().children[2].name.as_deref(),
            Some("Provider artist")
        );
        assert!(
            app.device_row(&crate::actions::artist_resource_id(id))
                .is_some()
        );
        let album = "spotify:album:1234567890123456789012";
        app.album_pages.insert(
            album.into(),
            crate::backend::AlbumPage {
                id: album.into(),
                title: "Provider album".into(),
                ..Default::default()
            },
        );
        app.view = super::super::View::Album(album.into());
        assert_eq!(
            app.accessibility().unwrap().children[2].name.as_deref(),
            Some("Provider album")
        );
        let playlist = "spotify:playlist:1234567890123456789012";
        app.playlist_pages.insert(
            playlist.into(),
            crate::backend::PlaylistPage {
                id: playlist.into(),
                title: "Provider playlist".into(),
                ..Default::default()
            },
        );
        app.view = super::super::View::Playlist(playlist.into());
        assert_eq!(
            app.accessibility().unwrap().children[2].name.as_deref(),
            Some("Provider playlist")
        );
    }

    #[test]
    fn spotify_artist_follow_is_a_host_effect_and_provider_confirmation_only() {
        use ui::tilo_resource::Effect as _;
        let mut app = MusicApp::without_profile();
        let id = "spotify:artist:1234567890123456789012";
        let page = crate::backend::ArtistPage {
            id: id.into(),
            name: "Artist".into(),
            provider_followed: Some(true),
            ..Default::default()
        };
        app.absorb_artist(&page);
        app.artist_pages.insert(id.into(), page);
        let mut doc = automerge::AutoCommit::new();
        let before = doc.get_heads();
        let mut library = ui::tilo_resource::Replica::<crate::model::Library>::new();
        let refused = library
            .apply(
                ui::tilo_resource::Invoke::edit(&crate::model::Library::follow_artist(id, true))
                    .unwrap(),
            )
            .unwrap_err();
        assert!(refused.to_string().contains("provider-authorized"));
        app.on_toggle_follow(id, &mut ui::widget::Edits::lend(&mut doc));
        let invocation = app
            .take_control_invocation()
            .expect("follow uses the host control channel");
        assert_eq!(invocation.address.resource, "music.library");
        let spec = MusicEffect::specs()
            .into_iter()
            .find(|spec| spec.id == invocation.address.action)
            .unwrap();
        let encoded = spec.build_json(&invocation.arguments).unwrap();
        assert!(
            matches!(MusicEffect::from_payload(&encoded.payload).unwrap(), MusicEffect::SpotifyUnsave {uri} if uri == id)
        );
        assert!(
            app.artist_followed(id),
            "the intent does not claim a provider write succeeded"
        );
        let resource = app
            .device_row(&crate::actions::artist_resource_id(id))
            .unwrap()
            .into_resource();
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == MusicEffect::SpotifySave { uri: id.into() }.id())
        );
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == MusicEffect::SpotifyUnsave { uri: id.into() }.id())
        );
        app.spotify_jobs.insert(
            45,
            PendingJob {
                invocation: tilo_actions::InvocationId::new(),
                started: ui::time::now(),
                remote_play: false,
                library_page: None,
            },
        );
        app.finish_spotify_operation(45, Err("Not confirmed".into()));
        assert!(app.artist_followed(id));
        app.spotify_jobs.insert(
            46,
            PendingJob {
                invocation: tilo_actions::InvocationId::new(),
                started: ui::time::now(),
                remote_play: false,
                library_page: None,
            },
        );
        app.finish_spotify_operation(
            46,
            Ok(SpotifyReply {
                devices: vec![],
                state: serde_json::json!({"uri":id,"saved":false}),
            }),
        );
        assert!(!app.artist_followed(id));
        assert_eq!(
            doc.get_heads(),
            before,
            "Spotify following is not a collaborative document edit"
        );
        assert!(app.take_control_invocation().is_none());
    }

    #[test]
    fn spotify_queue_refresh_is_a_shared_host_control_at_large_text() {
        use ui::widget::Widget as _;
        for scale in [1.0, 2.0] {
            let mut app = MusicApp::without_profile();
            app.snap_spotify("spotify-queue");
            let snapshot = &mut app.spotify_remote_queue.as_mut().unwrap().0;
            snapshot["remote_queue"]["queue"] = serde_json::Value::Array((0..20).map(|i| serde_json::json!({"name":format!("A conversation about music and collaborative listening {i}"),"type":"episode","show":{"name":"Example podcast"}})).collect());
            app.rect = vello::kurbo::Rect::new(0.0, 0.0, 360.0, 900.0);
            let theme = ui::theme::Theme::high_contrast().with_text_scale(scale);
            let mut tree = taffy::TaffyTree::new();
            let root = app.build_layout(&mut tree, &theme, &mut std::collections::HashMap::new());
            let mut text = ui::text_render::TextRenderer::new();
            text.set_text_scale(scale);
            app.render_frame(
                &mut vello::Scene::new(),
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
            );
            assert_eq!(tree.children(root).unwrap().len(), 2);
            assert_eq!(app.children().len(), 2);
            let control = app
                .children()
                .into_iter()
                .find(|control| control.test_id() == Some("music-spotify-queue-refresh"))
                .expect("retained queue refresh control");
            assert_eq!(control.test_id(), Some("music-spotify-queue-refresh"));
            let rect = control.get_rect();
            assert!(rect.width() > 0.0 && rect.x0 >= 0.0 && rect.x1 <= 360.0);
            if scale > 1.5 {
                assert!(
                    app.content_height > 900.0,
                    "remote snapshot precedes the local empty state in scrollable content"
                );
            }
            let bindings = app.control_action_bindings();
            assert!(bindings.iter().any(|binding| matches!(&binding.kind, ui::widget::ControlBindingKind::Action(address) if address.action == <MusicEffect as ui::tilo_resource::Effect>::id(&MusicEffect::SpotifyQueue))));
            app.spotify_catalogue = false;
            app.spotify_remote_queue = None;
            assert!(
                app.children()
                    .iter()
                    .all(|control| control.test_id() != Some("music-spotify-queue-refresh")),
                "invisible provider controls leave the focus tree"
            );
        }
    }

    #[test]
    fn source_picker_only_emits_sign_in_when_a_choice_is_activated() {
        use ui::tilo_resource::Effect as _;
        let mut app = MusicApp::without_profile();
        let effect = MusicEffect::Sources;
        app.begin_effect(tilo_actions::InvocationId::new(), effect)
            .unwrap();
        assert!(
            app.take_control_invocation().is_none(),
            "opening the picker never initiates consent"
        );
        assert!(app.source_invocation.is_none());
        assert_eq!(app.menu_actions.len(), 2);
        assert!(matches!(
            app.menu_actions[0],
            super::super::Action::SpotifyEffect(MusicEffect::ConnectSpotify)
        ));
        assert!(
            app.menu_actions.iter().all(|action| !matches!(
                action,
                super::super::Action::SpotifyEffect(
                    MusicEffect::ConnectSpotifyPlayback | MusicEffect::DisconnectSpotify
                )
            )),
            "an unconnected source offers no playback grant or disconnect operation"
        );
        app.queue_spotify_effect_control(&MusicEffect::ConnectSpotify);
        let intent = app
            .take_control_invocation()
            .expect("choice uses the host action door");
        assert_eq!(intent.address.action, MusicEffect::ConnectSpotify.id());
        assert!(app.take_control_invocation().is_none());
        app.spotify_catalogue = true;
        app.open_sources_menu();
        assert!(app.menu_actions.iter().any(|action| matches!(
            action,
            super::super::Action::SpotifyEffect(MusicEffect::ConnectSpotifyLibrary)
        )));
        assert!(
            app.take_control_invocation().is_none(),
            "opening editing choice never grants extra permissions"
        );
    }

    #[test]
    fn source_picker_labels_fit_a_narrow_window_at_double_text_size() {
        let mut app = MusicApp::without_profile();
        app.snap_spotify("spotify-home");
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 360.0, 800.0);
        let theme = ui::theme::Theme::high_contrast().with_text_scale(2.0);
        let mut text = ui::text_render::TextRenderer::new();
        app.open_sources_menu();
        app.context.layout_within(&mut text, &theme);
        let bounds = app.context.get_rect();
        assert!(bounds.x0 >= 0.0 && bounds.x1 <= 360.0);
        let style = theme.text(ui::tokens::TextRole::Body);
        let available = (bounds.width() - 2.0 * f64::from(theme.tokens.spacing.s3)) as f32;
        for item in app.context.items() {
            if let Some(label) = item.label() {
                assert_eq!(
                    text.elide_to_width_style(label, available, &style),
                    label,
                    "the source choice must stay readable: {label}"
                );
            }
        }
        assert!(app.source_invocation.is_none());
        assert!(app.spotify_jobs.is_empty());
    }

    #[test]
    fn spotify_creation_prompt_cancel_and_reopen_emit_one_host_intent() {
        use ui::tilo_resource::Effect as _;
        let mut app = MusicApp::without_profile();
        app.snap_spotify("spotify-home");
        let mut doc = automerge::AutoCommit::new();
        let before = doc.get_heads();
        app.dispatch(
            super::super::Action::BeginNewSpotifyPlaylist,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.ensure_name_dialog();
        assert!(app.name_dialog.is_some(), "shared dialog is used");
        app.on_close_name_prompt();
        assert!(
            app.take_control_invocation().is_none(),
            "cancel does not create"
        );
        app.dispatch(
            super::super::Action::BeginNewSpotifyPlaylist,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.name_input = "Evening".into();
        app.ensure_name_dialog();
        app.submit_name_prompt(&mut ui::widget::Edits::lend(&mut doc));
        let invocation = app
            .take_control_invocation()
            .expect("host-admitted creation intent");
        let spec = MusicEffect::specs()
            .into_iter()
            .find(|spec| spec.id == invocation.address.action)
            .unwrap();
        let encoded = spec.build_json(&invocation.arguments).unwrap();
        assert!(
            matches!(MusicEffect::from_payload(&encoded.payload).unwrap(), MusicEffect::SpotifyCreatePlaylist { name } if name == "Evening")
        );
        assert!(app.name_prompt.is_none());
        assert!(app.take_control_invocation().is_none());
        assert_eq!(doc.get_heads(), before);
    }

    #[test]
    fn spotify_library_page_replay_and_bad_page_preserve_visible_state() {
        let mut app = MusicApp::without_profile();
        app.snap_spotify("spotify-home");
        let mut page = app.home.clone().unwrap();
        page.sections[0]
            .items
            .retain(|item| item.id.starts_with("spotify:playlist:"));
        let state = serde_json::json!({"library_page":50,"has_more":false,"home_page":page});
        app.merge_spotify_library_page(50, &state).unwrap();
        app.refresh_spotify_control_flags();
        app.open_sources_menu();
        assert!(
            app.menu_actions.iter().all(|action| !matches!(
                action,
                super::super::Action::SpotifyEffect(MusicEffect::SpotifyLibraryMore)
            )),
            "an exhausted provider page is absent from the source menu"
        );
        assert_eq!(
            app.home.as_ref().unwrap().sections[0].items.len(),
            3,
            "repeated IDs are merged once"
        );
        assert_eq!(app.spotify_library_offset, 100);
        assert!(!app.spotify_library_more);
        assert!(
            app.merge_spotify_library_page(50, &state).is_err(),
            "stale offset is refused"
        );
        let bad = serde_json::json!({"library_page":100,"has_more":true,"home_page":null});
        assert!(app.merge_spotify_library_page(100, &bad).is_err());
        assert_eq!(app.spotify_library_offset, 100);
        assert_eq!(app.home.as_ref().unwrap().sections[0].items.len(), 3);
    }

    #[test]
    fn spotify_remote_refresh_publishes_readonly_rows_without_replacing_local_queue() {
        let mut app = MusicApp::without_profile();
        app.snap_spotify("spotify-home");
        app.pb.queue = vec!["local-song".into()];
        let invocation = tilo_actions::InvocationId::new();
        app.spotify_jobs.insert(
            1,
            PendingJob {
                invocation,
                started: ui::time::now(),
                remote_play: false,
                library_page: None,
            },
        );
        app.finish_spotify_operation(1, Ok(SpotifyReply { devices: Vec::new(), state: serde_json::json!({"remote_queue":{"currently_playing":null,"queue":[{"name":"Remote song","artists":[{"name":"Artist"}]}]},"truncated":false}) }));
        assert_eq!(app.pb.queue, ["local-song"]);
        assert!(
            app.home.is_some(),
            "remote refresh keeps library paging state"
        );
        let row = app
            .device_row("music.spotify-remote:0")
            .expect("accessible remote snapshot item")
            .into_resource();
        assert!(row.label.contains("Remote song"));
        assert!(
            row.actions.is_empty(),
            "remote rows do not offer local queue edits"
        );
        let Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
            app.take_effect_result(invocation)
        else {
            panic!("the real remote refresh must settle its pending job");
        };
        assert_eq!(
            output.value.as_ref().unwrap()["remote_queue"]["queue"][0]["name"],
            "Remote song"
        );
        MusicEffect::SpotifyQueue
            .bound()
            .contract
            .result
            .validate(&output)
            .expect("provider snapshot completion must match its declared result");
        app.finish_spotify_operation(1, Err("late reply".into()));
        assert!(app.take_effect_result(invocation).is_none());
        assert!(
            app.source_error.is_none(),
            "duplicate completion is ignored"
        );
        assert_eq!(app.spotify_remote_rows().len(), 1);
    }

    #[test]
    fn spotify_large_library_only_paints_visible_targets_and_keeps_resources_reachable() {
        let mut app = MusicApp::without_profile();
        app.snap_spotify("spotify-home");
        let template = app.home.as_ref().unwrap().sections[0].items[1].clone();
        let items = (0..2000)
            .map(|index| {
                let mut item = template.clone();
                item.id = format!("spotify:playlist:{index:022}");
                item
            })
            .collect();
        app.home.as_mut().unwrap().sections[0].items = items;
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 1280.0, 900.0);
        for scale in [1.0, 2.0] {
            let mut text = ui::text_render::TextRenderer::new();
            text.set_text_scale(scale);
            app.render_frame(
                &mut vello::Scene::new(),
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &ui::theme::Theme::dark().with_text_scale(scale),
            );
            assert!(
                app.hits
                    .iter()
                    .filter(|hit| matches!(
                        hit.action,
                        super::super::Action::Nav(super::super::View::Playlist(_))
                    ))
                    .count()
                    < 32,
                "offscreen cards produce neither paint nor pointer targets"
            );
        }
        let id = crate::actions::playlist_resource_id("spotify:playlist:0000000000000000001999");
        assert!(
            app.device_row(&id).is_some(),
            "render culling does not hide resources"
        );
    }

    #[test]
    fn spotify_artist_release_beyond_the_published_page_remains_reachable_by_resource() {
        let mut app = MusicApp::without_profile();
        let albums: Vec<_> = (0..25)
            .map(|number| crate::backend::ArtistAlbum {
                id: format!("spotify:album:{number:022}"),
                title: format!("Release {number}"),
                year: 2026,
                kind: crate::backend::AlbumKind::Album,
                cover_url: String::new(),
            })
            .collect();
        let last = albums.last().unwrap().id.clone();
        app.artist_pages.insert(
            "spotify:artist:abcdefghijklmnopqrstuv".into(),
            crate::backend::ArtistPage {
                id: "spotify:artist:abcdefghijklmnopqrstuv".into(),
                name: "Artist".into(),
                albums,
                ..Default::default()
            },
        );
        let address = crate::actions::album_resource_id(&last);
        assert!(!app.device_rows().iter().any(|row| row.id() == address));
        assert!(
            app.device_row(&address).is_some(),
            "canonical lookup is not capped by the presentation page"
        );
        assert!(matches!(
            app.begin_effect(
                tilo_actions::InvocationId::new(),
                MusicEffect::Open {
                    place: crate::actions::Place::Album(last.clone()),
                }
            )
            .unwrap(),
            ui::framework::Effected::Done(_)
        ));
        assert!(matches!(&app.view, super::super::View::Album(id) if id == &last));
    }

    #[test]
    fn spotify_jobs_expire_without_accepting_late_success_or_retrying_a_write() {
        let mut app = MusicApp::without_profile();
        let invocation = tilo_actions::InvocationId::new();
        app.spotify_jobs.insert(
            7,
            PendingJob {
                invocation,
                started: ui::time::now()
                    .checked_sub(ui::time::Duration::from_secs(121))
                    .expect("expired deadline fixture"),
                remote_play: false,
                library_page: None,
            },
        );
        app.tick();
        assert!(app.spotify_jobs.is_empty());
        assert!(matches!(app.take_effect_result(invocation),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. })
                if error.code == "music.spotify_timeout" && !error.retryable));
        app.finish_spotify_operation(
            7,
            Ok(SpotifyReply {
                devices: Vec::new(),
                state: serde_json::json!({"snapshot":"late"}),
            }),
        );
        assert!(app.take_effect_result(invocation).is_none());
    }

    #[test]
    fn spotify_home_playlist_has_a_painted_hit_and_opens_the_provider_collection() {
        let mut app = MusicApp::without_profile();
        app.snap_spotify("spotify-home");
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 1280.0, 900.0);
        let mut text = ui::text_render::TextRenderer::new();
        app.render_frame(
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            &mut text,
            &ui::theme::Theme::dark(),
        );
        let action = super::super::Action::Nav(super::super::View::Playlist(
            "spotify:collection:liked".into(),
        ));
        let grid = app
            .media_grids
            .iter()
            .find(|grid| grid.offers(&action))
            .expect("playlist art is an actionable retained tile");
        let rect = grid
            .card_rect("spotify:collection:liked")
            .expect("retained playlist card");
        assert!(rect.width() > 100.0 && rect.height() > 100.0);
        let mut doc = automerge::AutoCommit::new();
        super::super::tests::hosted_click(&mut app, &mut doc, rect.center());
        assert!(
            matches!(&app.view, super::super::View::Playlist(id) if id == "spotify:collection:liked")
        );
        assert!(doc.get_heads().is_empty());
    }

    #[test]
    fn spotify_playlist_picker_emits_provider_edit_without_document_edits() {
        use ui::tilo_resource::Effect as _;
        let mut app = MusicApp::without_profile();
        app.snap_spotify("spotify-home");
        let track = "spotify:track:2234567890123456789012";
        let playlist = "spotify:playlist:1234567890123456789012";
        app.open_add_to_playlist_menu(track);
        let action = app.menu_actions.iter().find(|action| matches!(action,
            super::super::Action::SpotifyEffect(MusicEffect::SpotifyPlaylistAdd { playlist: target, track: song })
                if target == playlist && song == track
        )).expect("provider playlist is selectable").clone();
        let mut doc = automerge::AutoCommit::new();
        let before = doc.get_heads();
        app.dispatch(action, &mut ui::widget::Edits::lend(&mut doc));
        let invocation = app.take_control_invocation().expect("host-bound edit");
        assert_eq!(invocation.address.resource, "music.library");
        let spec = MusicEffect::specs()
            .into_iter()
            .find(|spec| spec.id == invocation.address.action)
            .expect("root publishes effect");
        let encoded = spec
            .build_json(&invocation.arguments)
            .expect("validated arguments");
        let effect = MusicEffect::from_payload(&encoded.payload).expect("typed effect");
        assert!(
            matches!(effect, MusicEffect::SpotifyPlaylistAdd { playlist: target, track: song }
            if target == playlist && song == track)
        );
        assert_eq!(doc.get_heads(), before);
    }

    #[test]
    fn spotify_playlist_rename_dialog_emits_provider_name_without_local_mutation() {
        use ui::tilo_resource::Effect as _;
        let mut app = MusicApp::without_profile();
        app.snap_spotify("spotify-home");
        let playlist = "spotify:playlist:1234567890123456789012";
        app.open_spotify_playlist_menu(playlist);
        let action = app
            .menu_actions
            .iter()
            .find(|action| {
                matches!(action,
                    super::super::Action::BeginRenamePlaylist(target) if target == playlist
                )
            })
            .expect("rename is visible")
            .clone();
        let mut doc = automerge::AutoCommit::new();
        let before = doc.get_heads();
        app.dispatch(action, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.name_input, "After hours");
        app.name_input = "  Evening music  ".into();
        app.on_commit_rename(playlist, &mut ui::widget::Edits::lend(&mut doc));
        let invocation = app.take_control_invocation().expect("host-bound rename");
        assert_eq!(invocation.address.resource, "music.library");
        let spec = MusicEffect::specs()
            .into_iter()
            .find(|spec| spec.id == invocation.address.action)
            .expect("root publishes effect");
        let encoded = spec
            .build_json(&invocation.arguments)
            .expect("validated arguments");
        let effect = MusicEffect::from_payload(&encoded.payload).expect("typed effect");
        assert!(
            matches!(effect, MusicEffect::SpotifyRenamePlaylist { playlist: target, name }
            if target == playlist && name == "Evening music")
        );
        assert!(app.name_prompt.is_none());
        assert_eq!(doc.get_heads(), before);
    }

    #[test]
    fn spotify_track_menu_queues_the_published_host_action_without_document_edits() {
        let mut app = MusicApp::without_profile();
        let track = "spotify:track:1234567890123456789012";
        app.absorb_tracks(&[crate::backend::TrackHit {
            video_id: track.into(),
            title: "A song".into(),
            artist: "An artist".into(),
            artist_id: None,
            album: None,
            album_id: None,
            duration_s: 180,
            cover_url: String::new(),
        }]);
        app.open_track_menu(track.into(), None, None);
        let action = app
            .menu_actions
            .iter()
            .find(|action| {
                matches!(
                    action,
                    super::super::Action::SpotifyLibrary { save: true, .. }
                )
            })
            .expect("Spotify save is visible")
            .clone();
        let mut doc = automerge::AutoCommit::new();
        let before = doc.get_heads();
        app.dispatch(action, &mut ui::widget::Edits::lend(&mut doc));
        let invocation = app.take_control_invocation().expect("host-bound control");
        assert_eq!(
            invocation.address.resource,
            crate::actions::track_resource_id(track)
        );
        let resource = app
            .publish_resource(&invocation.address.resource)
            .expect("canonical track");
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == invocation.address.action)
            .expect("published action");
        let encoded = action
            .build_json(&invocation.arguments)
            .expect("valid bound arguments");
        let effect = <MusicEffect as ui::tilo_resource::Effect>::from_payload(&encoded.payload)
            .expect("Spotify effect");
        assert!(matches!(effect, MusicEffect::SpotifySave { uri } if uri == track));
        assert_eq!(doc.get_heads(), before);
        assert!(app.take_control_invocation().is_none());
    }
}
