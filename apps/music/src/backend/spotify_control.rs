//! Official Web API controls are independent of the native decoder and queue.
use super::{ACCOUNT, BackendError, SpotifyBackend, USE, raw_id};
use crate::backend::{SpotifyDevice, SpotifyOperation, SpotifyReply};
use reqwest::Method;
use serde_json::{Value, json};

impl SpotifyBackend {
    pub(crate) fn operation(
        &self,
        operation: &SpotifyOperation,
    ) -> Result<SpotifyReply, BackendError> {
        use SpotifyOperation as Op;
        let state = match operation {
            Op::Queue => self.read_remote_queue()?,
            Op::LibraryPage { offset } => self.library_page(*offset)?,
            Op::CreatePlaylist { name } => self.create_private_playlist(name)?,
            Op::Devices => {
                return Ok(SpotifyReply {
                    devices: self.devices()?,
                    state: self.get("me/player", &[])?,
                });
            }
            Op::Save { uri, saved } => {
                valid_library_uri(uri)?;
                self.request_for(
                    USE,
                    ACCOUNT,
                    if *saved {
                        &Method::PUT
                    } else {
                        &Method::DELETE
                    },
                    "me/library",
                    &[("uris", uri)],
                    None,
                )?;
                let checked = self.get("me/library/contains", &[("uris", uri)])?;
                if checked
                    .as_array()
                    .and_then(|items| items.first())
                    .and_then(Value::as_bool)
                    != Some(*saved)
                {
                    return Err(BackendError(
                        "Spotify has not confirmed the library change. Refresh before retrying."
                            .into(),
                    ));
                }
                json!({"uri":uri,"saved":saved})
            }
            Op::RenamePlaylist { playlist, name } => {
                let id = raw_id(playlist, "playlist")?;
                valid_playlist_name(name)?;
                self.check_playlist_edit(id, true)?;
                self.request_for(
                    USE,
                    ACCOUNT,
                    &Method::PUT,
                    &format!("playlists/{id}"),
                    &[],
                    Some(&json!({"name":name})),
                )?;
                let result = self.get(&format!("playlists/{id}"), &[])?;
                if result.get("name").and_then(Value::as_str) != Some(name.as_str()) {
                    return Err(BackendError(
                        "Spotify has not confirmed the playlist name".into(),
                    ));
                }
                json!({"playlist":playlist,"name":name})
            }
            Op::PlaylistAdd { playlist, track } | Op::PlaylistRemove { playlist, track } => {
                let id = raw_id(playlist, "playlist")?;
                raw_id(track, "track")?;
                self.check_playlist_edit(id, false)?;
                let add = matches!(operation, Op::PlaylistAdd { .. });
                let body = if add {
                    json!({"uris":[track]})
                } else {
                    json!({"items":[{"uri":track}]})
                };
                let result = self.request_for(
                    USE,
                    ACCOUNT,
                    if add { &Method::POST } else { &Method::DELETE },
                    &format!("playlists/{id}/items"),
                    &[],
                    Some(&body),
                )?;
                if result
                    .get("snapshot_id")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(BackendError(
                        "Spotify did not confirm a playlist revision".into(),
                    ));
                }
                json!({"playlist":playlist,"snapshot":result.get("snapshot_id"),"added":add})
            }
            Op::Transfer { device }
            | Op::RemotePlay { device, .. }
            | Op::RemotePause { device } => return self.remote_operation(operation, device),
        };
        Ok(SpotifyReply {
            devices: Vec::new(),
            state,
        })
    }

    fn read_remote_queue(&self) -> Result<Value, BackendError> {
        let mut queue = self.get("me/player/queue", &[])?;
        let entries = queue
            .get_mut("queue")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| BackendError("Spotify did not provide its remote queue".into()))?;
        let truncated = entries.len() > 50;
        entries.truncate(50);
        Ok(json!({"remote_queue":queue,"truncated":truncated}))
    }

    fn library_page(&self, offset: u32) -> Result<Value, BackendError> {
        if offset > 1950 || !offset.is_multiple_of(50) {
            return Err(BackendError(
                "Spotify library page is outside the 2,000-item bound".into(),
            ));
        }
        let offset_arg = offset.to_string();
        let albums = self.get("me/albums", &[("limit", "50"), ("offset", &offset_arg)])?;
        let playlists = self.get("me/playlists", &[("limit", "50"), ("offset", &offset_arg)])?;
        for page in [&albums, &playlists] {
            if page
                .get("items")
                .and_then(Value::as_array)
                .is_none_or(|items| items.len() > 50)
            {
                return Err(BackendError(
                    "Spotify returned an invalid library page".into(),
                ));
            }
        }
        Ok(
            json!({"library_page":offset,"home_page":super::library_home(&albums, &playlists),
                    "has_more":!albums.get("next").unwrap_or(&Value::Null).is_null() || !playlists.get("next").unwrap_or(&Value::Null).is_null()}),
        )
    }

    fn create_private_playlist(&self, name: &str) -> Result<Value, BackendError> {
        valid_playlist_name(name)?;
        let created = self.request_for(
            USE,
            ACCOUNT,
            &Method::POST,
            "me/playlists",
            &[],
            Some(&json!({"name":name,"public":false})),
        )?;
        let uri = created.get("uri").and_then(Value::as_str).ok_or_else(|| {
            BackendError(
                "Spotify has not confirmed the created playlist. Refresh before retrying.".into(),
            )
        })?;
        let id = raw_id(uri, "playlist")?;
        let checked = self.get(&format!("playlists/{id}"), &[])?;
        if checked.get("name").and_then(Value::as_str) != Some(name)
            || checked.get("public").and_then(Value::as_bool) != Some(false)
        {
            return Err(BackendError(
                "Spotify has not confirmed the private playlist. Refresh before retrying.".into(),
            ));
        }
        Ok(json!({"playlist":uri,"name":name,"created":true}))
    }

    fn devices(&self) -> Result<Vec<SpotifyDevice>, BackendError> {
        let result = self.get("me/player/devices", &[])?;
        let devices = result
            .get("devices")
            .and_then(Value::as_array)
            .ok_or_else(|| BackendError("Spotify did not provide its devices".into()))?;
        Ok(devices
            .iter()
            .take(100)
            .filter_map(|device| {
                Some(SpotifyDevice {
                    id: device.get("id")?.as_str()?.to_string(),
                    name: device.get("name")?.as_str()?.to_string(),
                    active: device
                        .get("is_active")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    restricted: device
                        .get("is_restricted")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                })
            })
            .collect())
    }

    fn check_playlist_edit(&self, id: &str, rename: bool) -> Result<(), BackendError> {
        let profile = self.get("me", &[])?;
        let user = profile
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| BackendError("Spotify account identity is unavailable".into()))?;
        let playlist = self.get(&format!("playlists/{id}"), &[])?;
        let owner = playlist
            .get("owner")
            .and_then(|owner| owner.get("id"))
            .and_then(Value::as_str)
            == Some(user);
        let collaborator = playlist.get("collaborative").and_then(Value::as_bool) == Some(true)
            && playlist.get("items").is_some_and(Value::is_object);
        if owner || (!rename && collaborator) {
            Ok(())
        } else {
            Err(BackendError(if rename {
                "Only the Spotify playlist owner can rename it. Saving a playlist does not grant editing access."
            } else {
                "Spotify playlist editing requires ownership or collaborator access. Saving a playlist does not grant editing access."
            }.into()))
        }
    }

    fn remote_operation(
        &self,
        operation: &SpotifyOperation,
        device: &str,
    ) -> Result<SpotifyReply, BackendError> {
        if device.is_empty() || device.len() > 512 {
            return Err(BackendError("Invalid Spotify device identity".into()));
        }
        if self.get("me", &[])?.get("product").and_then(Value::as_str) != Some("premium") {
            return Err(BackendError(
                "Spotify Connect controls require Premium".into(),
            ));
        }
        let mut devices = self.devices()?;
        if !devices
            .iter()
            .any(|candidate| candidate.id == device && !candidate.restricted)
        {
            return Err(BackendError(
                "Spotify device is unavailable or does not permit remote controls".into(),
            ));
        }
        match operation {
            SpotifyOperation::Transfer { .. } => {
                self.request_for(
                    USE,
                    ACCOUNT,
                    &Method::PUT,
                    "me/player",
                    &[],
                    Some(&json!({"device_ids":[device],"play":false})),
                )?;
            }
            SpotifyOperation::RemotePlay { track, .. } => {
                raw_id(track, "track")?;
                self.request_for(
                    USE,
                    ACCOUNT,
                    &Method::PUT,
                    "me/player/play",
                    &[("device_id", device)],
                    Some(&json!({"uris":[track]})),
                )?;
            }
            SpotifyOperation::RemotePause { .. } => {
                self.request_for(
                    USE,
                    ACCOUNT,
                    &Method::PUT,
                    "me/player/pause",
                    &[("device_id", device)],
                    None,
                )?;
            }
            _ => return Err(BackendError("Invalid remote player command".into())),
        }
        let state = self.get("me/player", &[])?;
        if state
            .get("device")
            .and_then(|device| device.get("id"))
            .and_then(Value::as_str)
            != Some(device)
        {
            return Err(BackendError(
                "Spotify accepted the command but has not confirmed this device. Refresh devices."
                    .into(),
            ));
        }
        match operation {
            SpotifyOperation::RemotePlay { track, .. }
                if state
                    .get("item")
                    .and_then(|item| item.get("uri"))
                    .and_then(Value::as_str)
                    != Some(track.as_str())
                    || state.get("is_playing").and_then(Value::as_bool) != Some(true) =>
            {
                return Err(BackendError(
                    "Spotify has not confirmed playback of this track".into(),
                ));
            }
            SpotifyOperation::RemotePause { .. }
                if state.get("is_playing").and_then(Value::as_bool) != Some(false) =>
            {
                return Err(BackendError(
                    "Spotify has not confirmed paused playback".into(),
                ));
            }
            _ => {}
        }
        for candidate in &mut devices {
            candidate.active = candidate.id == device;
        }
        Ok(SpotifyReply { devices, state })
    }
}

fn valid_playlist_name(name: &str) -> Result<(), BackendError> {
    if name.trim().is_empty() || name.chars().count() > 100 {
        Err(BackendError("Playlist names need 1–100 characters".into()))
    } else {
        Ok(())
    }
}

fn valid_library_uri(uri: &str) -> Result<(), BackendError> {
    for kind in ["track", "album", "playlist"] {
        if raw_id(uri, kind).is_ok() {
            return Ok(());
        }
    }
    Err(BackendError(
        "This item cannot be saved to a Spotify library".into(),
    ))
}
