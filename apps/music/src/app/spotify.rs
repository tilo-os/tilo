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
    retryable: bool,
    remote_play: bool,
}
use ui::widget::Widget as _;

impl MusicApp {
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
            let label = ui::i18n::I18nManager::format(
                "music-spotify-playlist-choice",
                &[("name", name.into())],
            );
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
            ui::i18n!("music-spotify-rename-playlist"),
            super::Action::BeginRenamePlaylist(playlist.into()),
        );
        self.push_menu(
            ui::i18n!("music-spotify-save-playlist"),
            super::Action::SpotifyEffect(MusicEffect::SpotifySave {
                uri: playlist.into(),
            }),
        );
        self.push_menu(
            ui::i18n!("music-spotify-unsave-playlist"),
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
                title: ui::i18n!("music-spotify-library"),
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
            self.source_error = Some(ui::i18n!("music-spotify-signin-expired"));
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
        let operation = operation(effect).ok_or("Invalid Spotify operation")?;
        let retryable = !matches!(
            operation,
            SpotifyOperation::PlaylistAdd { .. } | SpotifyOperation::PlaylistRemove { .. }
        );
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
                retryable,
                remote_play,
            },
        );
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
        let update = match result {
            Ok(reply) => {
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
                }
                self.home = None;
                self.home_requested = false;
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
                self.toast = Some((message.clone(), ui::time::now()));
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "music.spotify_operation_failed".into(),
                        message,
                        details: None,
                        retryable: job.retryable,
                    },
                }
            }
        };
        self.resource_stream_results.push(update);
        self.dirty = true;
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
                let message = ui::i18n!("music-spotify-request-timeout");
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
                    let label =
                        ui::i18n::I18nManager::format(key, &[("device", (&device.name).into())]);
                    let resource =
                        tilo_actions::Resource::new(&address, "music.spotify-device", &device.name)
                            .with_action(effect.bound());
                    let binding =
                        tilo_actions::ActionBinding::unit_for(&resource, effect.id()).ok()?;
                    Some(
                        ui::widget::Button::new(&id, &label)
                            .content_sized()
                            .with_enabled(!device.restricted)
                            .tooltip(ui::i18n!("music-spotify-device-help"))
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
                if id.starts_with("spotify:")
                    && id != "spotify:collection:liked"
                    && kind != "music.artist"
                {
                    row = row
                        .effect(&MusicEffect::SpotifySave { uri: id.clone() })
                        .effect(&MusicEffect::SpotifyUnsave { uri: id });
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
                        format!("{} • {}", device.name, ui::i18n!("music-spotify-active"))
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

impl ui::widget::controls::ActionControls for MusicApp {
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &ui::widget::controls::ControlResult,
    ) {
        if let Err(error) = result {
            self.toast = Some((error.to_string(), ui::time::now()));
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
        ui::widget::Widget::children_mut(self)
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
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
        MusicEffect::SpotifyDevices => SpotifyOperation::Devices,
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
    use ui::widget::controls::ActionControls as _;

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
                retryable: false,
                remote_play: false,
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
        let hit = app.hits.iter().find(|hit| matches!(&hit.action,
            super::super::Action::Nav(super::super::View::Playlist(id)) if id == "spotify:collection:liked"
        )).expect("playlist art is an actionable tile");
        assert!(hit.rect.width() > 100.0 && hit.rect.height() > 100.0);
        let action = hit.action.clone();
        let mut doc = automerge::AutoCommit::new();
        app.dispatch(action, &mut ui::widget::Edits::lend(&mut doc));
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
