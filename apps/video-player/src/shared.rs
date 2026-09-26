//! Durable collaborative playlists and timestamp annotations.
//!
//! Media capabilities stay device-private: shared rows contain only an opaque
//! media reference and human metadata. Each entity occupies an independent
//! root key so concurrent first writes never contend on a parent object.

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionContext, ActionError, ActionRegistry, Doc, ExecutionLocus};

pub const ACTION_TYPE: &str = "video-player.shared-action";
const PLAYLIST_PREFIX: &str = "video-player.shared.playlist:";
const PLAYLIST_TOMBSTONE_PREFIX: &str = "video-player.shared.playlist-tombstone:";
const ITEM_PREFIX: &str = "video-player.shared.playlist-item:";
const ITEM_TOMBSTONE_PREFIX: &str = "video-player.shared.playlist-item-tombstone:";
const ANNOTATION_PREFIX: &str = "video-player.shared.annotation:";
const ANNOTATION_TOMBSTONE_PREFIX: &str = "video-player.shared.annotation-tombstone:";
pub const MAX_SHARED_ROWS: usize = 10_000;
const MAX_TEXT_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Playlist {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistItem {
    pub id: String,
    pub playlist_id: String,
    pub media_ref: String,
    pub title: String,
    pub position: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimestampAnnotation {
    pub id: String,
    pub media_ref: String,
    pub time_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedLibrary {
    pub playlists: Vec<Playlist>,
    pub items: Vec<PlaylistItem>,
    pub annotations: Vec<TimestampAnnotation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SharedAction {
    CreatePlaylist {
        id: String,
        name: String,
    },
    RenamePlaylist {
        id: String,
        before: String,
        after: String,
    },
    DeletePlaylist {
        id: String,
        observed_item_ids: Vec<String>,
    },
    AddPlaylistItem {
        id: String,
        playlist_id: String,
        media_ref: String,
        title: String,
        position: i64,
    },
    MovePlaylistItem {
        id: String,
        before: i64,
        after: i64,
    },
    RemovePlaylistItem {
        id: String,
    },
    CreateAnnotation {
        id: String,
        media_ref: String,
        time_ms: u64,
        text: String,
    },
    EditAnnotation {
        id: String,
        before: String,
        after: String,
    },
    MoveAnnotation {
        id: String,
        before_ms: u64,
        after_ms: u64,
    },
    DeleteAnnotation {
        id: String,
    },
}

fn key(prefix: &str, id: &str) -> String {
    format!("{prefix}{id}")
}

fn valid_id(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

fn text_ok(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_TEXT_BYTES
}

fn tombstoned(doc: &Doc, prefix: &str, id: &str) -> bool {
    doc.get(&ROOT, key(prefix, id)).ok().flatten().is_some()
}

fn object(doc: &Doc, prefix: &str, id: &str) -> Option<ObjId> {
    doc.get(&ROOT, key(prefix, id))
        .ok()
        .flatten()
        .and_then(|(value, object)| matches!(value, Value::Object(ObjType::Map)).then_some(object))
}

fn string(doc: &Doc, parent: &ObjId, field: &str) -> Option<String> {
    doc.get(parent, field)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|value| value.as_str().map(str::to_string))
}

fn integer(doc: &Doc, parent: &ObjId, field: &str) -> Option<i64> {
    doc.get(parent, field)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|value| value.as_i64())
}

fn create_object(
    doc: &mut Doc,
    prefix: &str,
    tombstones: &str,
    id: &str,
) -> Result<ObjId, ActionError> {
    if !valid_id(id) {
        return Err(ActionError::Refused(
            "a stable non-empty id is required".into(),
        ));
    }
    if tombstoned(doc, tombstones, id) {
        return Err(ActionError::Refused(
            "a tombstoned id cannot be reused".into(),
        ));
    }
    if object(doc, prefix, id).is_some() {
        return Err(ActionError::Refused("that stable id already exists".into()));
    }
    Ok(doc.put_object(&ROOT, key(prefix, id), ObjType::Map)?)
}

fn live_object(doc: &Doc, prefix: &str, tombstones: &str, id: &str) -> Result<ObjId, ActionError> {
    if tombstoned(doc, tombstones, id) {
        return Err(ActionError::Refused("that entity was deleted".into()));
    }
    object(doc, prefix, id)
        .ok_or_else(|| ActionError::Refused("that entity no longer exists".into()))
}

fn write_tombstone(doc: &mut Doc, prefix: &str, id: &str) -> Result<(), ActionError> {
    if !valid_id(id) {
        return Err(ActionError::Refused(
            "a stable non-empty id is required".into(),
        ));
    }
    doc.put(&ROOT, key(prefix, id), true)?;
    Ok(())
}

impl Action for SharedAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::CreatePlaylist { .. }
            | Self::RenamePlaylist { .. }
            | Self::DeletePlaylist { .. }
            | Self::AddPlaylistItem { .. }
            | Self::MovePlaylistItem { .. }
            | Self::RemovePlaylistItem { .. } => self.apply_playlist(doc)?,
            Self::CreateAnnotation { .. }
            | Self::EditAnnotation { .. }
            | Self::MoveAnnotation { .. }
            | Self::DeleteAnnotation { .. } => self.apply_annotation(doc)?,
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

impl SharedAction {
    fn apply_playlist(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::CreatePlaylist { id, name } => {
                if !text_ok(name) {
                    return Err(ActionError::Refused("playlist name is required".into()));
                }
                let row = create_object(doc, PLAYLIST_PREFIX, PLAYLIST_TOMBSTONE_PREFIX, id)?;
                doc.put(&row, "name", name.trim())?;
            }
            Self::RenamePlaylist { id, before, after } => {
                if !text_ok(after) {
                    return Err(ActionError::Refused("playlist name is required".into()));
                }
                let row = live_object(doc, PLAYLIST_PREFIX, PLAYLIST_TOMBSTONE_PREFIX, id)?;
                if string(doc, &row, "name").as_deref() != Some(before.as_str()) {
                    return Err(ActionError::Refused(
                        "playlist name changed; refresh before renaming".into(),
                    ));
                }
                doc.put(&row, "name", after.trim())?;
            }
            Self::DeletePlaylist {
                id,
                observed_item_ids,
            } => {
                write_tombstone(doc, PLAYLIST_TOMBSTONE_PREFIX, id)?;
                for item_id in observed_item_ids {
                    write_tombstone(doc, ITEM_TOMBSTONE_PREFIX, item_id)?;
                }
            }
            Self::AddPlaylistItem {
                id,
                playlist_id,
                media_ref,
                title,
                position,
            } => {
                let _ = live_object(doc, PLAYLIST_PREFIX, PLAYLIST_TOMBSTONE_PREFIX, playlist_id)?;
                if !valid_id(media_ref) || !text_ok(title) {
                    return Err(ActionError::Refused(
                        "valid media identity and title are required".into(),
                    ));
                }
                let row = create_object(doc, ITEM_PREFIX, ITEM_TOMBSTONE_PREFIX, id)?;
                doc.put(&row, "playlist_id", playlist_id)?;
                doc.put(&row, "media_ref", media_ref)?;
                doc.put(&row, "title", title.trim())?;
                doc.put(&row, "position", *position)?;
            }
            Self::MovePlaylistItem { id, before, after } => {
                let row = live_object(doc, ITEM_PREFIX, ITEM_TOMBSTONE_PREFIX, id)?;
                if integer(doc, &row, "position") != Some(*before) {
                    return Err(ActionError::Refused(
                        "playlist order changed; refresh before moving".into(),
                    ));
                }
                doc.put(&row, "position", *after)?;
            }
            Self::RemovePlaylistItem { id } => write_tombstone(doc, ITEM_TOMBSTONE_PREFIX, id)?,
            _ => return Err(ActionError::Refused("invalid playlist action route".into())),
        }
        Ok(())
    }

    fn apply_annotation(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::CreateAnnotation {
                id,
                media_ref,
                time_ms,
                text,
            } => {
                if !valid_id(media_ref) || text.len() > MAX_TEXT_BYTES {
                    return Err(ActionError::Refused(
                        "valid media identity and bounded annotation text are required".into(),
                    ));
                }
                let row = create_object(doc, ANNOTATION_PREFIX, ANNOTATION_TOMBSTONE_PREFIX, id)?;
                doc.put(&row, "media_ref", media_ref)?;
                let time = i64::try_from(*time_ms)
                    .map_err(|_| ActionError::Refused("timestamp is too large".into()))?;
                doc.put(&row, "time_ms", time)?;
                doc.put(&row, "text", text)?;
            }
            Self::EditAnnotation { id, before, after } => {
                if after.len() > MAX_TEXT_BYTES {
                    return Err(ActionError::Refused("annotation text is too long".into()));
                }
                let row = live_object(doc, ANNOTATION_PREFIX, ANNOTATION_TOMBSTONE_PREFIX, id)?;
                if string(doc, &row, "text").as_deref() != Some(before.as_str()) {
                    return Err(ActionError::Refused(
                        "annotation changed; refresh before editing".into(),
                    ));
                }
                doc.put(&row, "text", after)?;
            }
            Self::MoveAnnotation {
                id,
                before_ms,
                after_ms,
            } => {
                let row = live_object(doc, ANNOTATION_PREFIX, ANNOTATION_TOMBSTONE_PREFIX, id)?;
                let before = i64::try_from(*before_ms)
                    .map_err(|_| ActionError::Refused("timestamp is too large".into()))?;
                let after = i64::try_from(*after_ms)
                    .map_err(|_| ActionError::Refused("timestamp is too large".into()))?;
                if integer(doc, &row, "time_ms") != Some(before) {
                    return Err(ActionError::Refused(
                        "annotation timestamp changed; refresh before moving".into(),
                    ));
                }
                doc.put(&row, "time_ms", after)?;
            }
            Self::DeleteAnnotation { id } => write_tombstone(doc, ANNOTATION_TOMBSTONE_PREFIX, id)?,
            _ => {
                return Err(ActionError::Refused(
                    "invalid annotation action route".into(),
                ));
            }
        }
        Ok(())
    }
}

pub fn history_context(action: &SharedAction) -> ActionContext {
    let mut context = ActionContext::for_action("so.tilo.video-player", "Video Player", action);
    let (label, id, kind) = match action {
        SharedAction::CreatePlaylist { id, .. } => (
            "Create playlist",
            crate::actions::playlist_resource_id(id),
            "media.playlist",
        ),
        SharedAction::RenamePlaylist { id, .. } => (
            "Rename playlist",
            crate::actions::playlist_resource_id(id),
            "media.playlist",
        ),
        SharedAction::DeletePlaylist { id, .. } => (
            "Delete playlist",
            crate::actions::playlist_resource_id(id),
            "media.playlist",
        ),
        SharedAction::AddPlaylistItem { id, .. } => (
            "Add playlist item",
            crate::actions::playlist_item_resource_id(id),
            "media.playlist-item",
        ),
        SharedAction::MovePlaylistItem { id, .. } => (
            "Move playlist item",
            crate::actions::playlist_item_resource_id(id),
            "media.playlist-item",
        ),
        SharedAction::RemovePlaylistItem { id } => (
            "Remove playlist item",
            crate::actions::playlist_item_resource_id(id),
            "media.playlist-item",
        ),
        SharedAction::CreateAnnotation { id, .. } => (
            "Add timestamp annotation",
            crate::actions::annotation_resource_id(id),
            "media.annotation",
        ),
        SharedAction::EditAnnotation { id, .. } => (
            "Edit timestamp annotation",
            crate::actions::annotation_resource_id(id),
            "media.annotation",
        ),
        SharedAction::MoveAnnotation { id, .. } => (
            "Move timestamp annotation",
            crate::actions::annotation_resource_id(id),
            "media.annotation",
        ),
        SharedAction::DeleteAnnotation { id } => (
            "Delete timestamp annotation",
            crate::actions::annotation_resource_id(id),
            "media.annotation",
        ),
    };
    context.action_label = label.into();
    context.resource = Some(tilo_actions::ActionResource {
        id,
        kind: kind.into(),
        label: label.into(),
    });
    context
}

pub fn read(doc: &Doc) -> SharedLibrary {
    let mut state = SharedLibrary::default();
    for property in doc.keys(&ROOT).take(MAX_SHARED_ROWS.saturating_mul(3)) {
        if let Some(id) = property.strip_prefix(PLAYLIST_PREFIX) {
            if tombstoned(doc, PLAYLIST_TOMBSTONE_PREFIX, id) {
                continue;
            }
            if let Some(row) = object(doc, PLAYLIST_PREFIX, id)
                && let Some(name) = string(doc, &row, "name")
            {
                state.playlists.push(Playlist {
                    id: id.into(),
                    name,
                });
            }
        } else if let Some(id) = property.strip_prefix(ITEM_PREFIX) {
            if tombstoned(doc, ITEM_TOMBSTONE_PREFIX, id) {
                continue;
            }
            if let Some(row) = object(doc, ITEM_PREFIX, id)
                && let (Some(playlist_id), Some(media_ref), Some(title), Some(position)) = (
                    string(doc, &row, "playlist_id"),
                    string(doc, &row, "media_ref"),
                    string(doc, &row, "title"),
                    integer(doc, &row, "position"),
                )
            {
                state.items.push(PlaylistItem {
                    id: id.into(),
                    playlist_id,
                    media_ref,
                    title,
                    position,
                });
            }
        } else if let Some(id) = property.strip_prefix(ANNOTATION_PREFIX) {
            if tombstoned(doc, ANNOTATION_TOMBSTONE_PREFIX, id) {
                continue;
            }
            if let Some(row) = object(doc, ANNOTATION_PREFIX, id)
                && let (Some(media_ref), Some(time_ms), Some(text)) = (
                    string(doc, &row, "media_ref"),
                    integer(doc, &row, "time_ms"),
                    string(doc, &row, "text"),
                )
                && let Ok(time_ms) = u64::try_from(time_ms)
            {
                state.annotations.push(TimestampAnnotation {
                    id: id.into(),
                    media_ref,
                    time_ms,
                    text,
                });
            }
        }
    }
    state
        .playlists
        .sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    state.items.sort_by(|a, b| {
        a.playlist_id
            .cmp(&b.playlist_id)
            .then_with(|| a.position.cmp(&b.position))
            .then_with(|| a.id.cmp(&b.id))
    });
    state.items.retain(|item| {
        state
            .playlists
            .iter()
            .any(|playlist| playlist.id == item.playlist_id)
    });
    state.annotations.sort_by(|a, b| {
        a.media_ref
            .cmp(&b.media_ref)
            .then_with(|| a.time_ms.cmp(&b.time_ms))
            .then_with(|| a.id.cmp(&b.id))
    });
    state
}

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<SharedAction>(ACTION_TYPE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;

    fn seed() -> Doc {
        let mut doc = Doc::new();
        doc.put(&ROOT, "seed", true).unwrap();
        doc.commit();
        doc
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn shared_playlists_and_annotations_converge_with_absolute_edits() {
        // Case `divergent_replicas_converge_and_tombstones_prevent_resurrection`.
        {
            let mut base = seed();
            let mut left = Doc::load(&base.save()).unwrap();
            let mut right = Doc::load(&base.save()).unwrap();
            SharedAction::CreatePlaylist {
                id: "p-left".into(),
                name: "Left".into(),
            }
            .apply(&mut left)
            .unwrap();
            SharedAction::CreateAnnotation {
                id: "a-deleted".into(),
                media_ref: "media-1".into(),
                time_ms: 42,
                text: "note".into(),
            }
            .apply(&mut left)
            .unwrap();
            left.commit();
            SharedAction::CreatePlaylist {
                id: "p-right".into(),
                name: "Right".into(),
            }
            .apply(&mut right)
            .unwrap();
            SharedAction::DeleteAnnotation {
                id: "a-deleted".into(),
            }
            .apply(&mut right)
            .unwrap();
            right.commit();
            let mut lr = Doc::load(&left.save()).unwrap();
            let mut rl = Doc::load(&right.save()).unwrap();
            lr.merge_incoming(&mut right).unwrap();
            rl.merge_incoming(&mut left).unwrap();
            assert_eq!(read(&lr), read(&rl));
            assert_eq!(read(&lr).playlists.len(), 2);
            assert!(
                read(&lr).annotations.is_empty(),
                "delete tombstone wins over an offline create"
            );
            assert!(
                SharedAction::CreateAnnotation {
                    id: "a-deleted".into(),
                    media_ref: "media-1".into(),
                    time_ms: 7,
                    text: String::new()
                }
                .apply(&mut lr)
                .is_err()
            );
        }
        // Case `playlist_membership_order_and_annotation_edits_are_absolute`.
        {
            let mut doc = seed();
            SharedAction::CreatePlaylist {
                id: "p".into(),
                name: "Watch later".into(),
            }
            .apply(&mut doc)
            .unwrap();
            SharedAction::AddPlaylistItem {
                id: "i".into(),
                playlist_id: "p".into(),
                media_ref: "media".into(),
                title: "Clip".into(),
                position: 10,
            }
            .apply(&mut doc)
            .unwrap();
            SharedAction::MovePlaylistItem {
                id: "i".into(),
                before: 10,
                after: 20,
            }
            .apply(&mut doc)
            .unwrap();
            SharedAction::CreateAnnotation {
                id: "a".into(),
                media_ref: "media".into(),
                time_ms: 1000,
                text: "Old".into(),
            }
            .apply(&mut doc)
            .unwrap();
            SharedAction::EditAnnotation {
                id: "a".into(),
                before: "Old".into(),
                after: "New".into(),
            }
            .apply(&mut doc)
            .unwrap();
            SharedAction::MoveAnnotation {
                id: "a".into(),
                before_ms: 1000,
                after_ms: 1250,
            }
            .apply(&mut doc)
            .unwrap();
            let state = read(&doc);
            assert_eq!(state.items[0].position, 20);
            assert_eq!(state.annotations[0].text, "New");
            assert_eq!(state.annotations[0].time_ms, 1250);
            SharedAction::DeletePlaylist {
                id: "p".into(),
                observed_item_ids: vec!["i".into()],
            }
            .apply(&mut doc)
            .unwrap();
            let deleted = read(&doc);
            assert!(deleted.playlists.is_empty());
            assert!(deleted.items.is_empty());
            assert!(
                SharedAction::AddPlaylistItem {
                    id: "i".into(),
                    playlist_id: "p".into(),
                    media_ref: "media".into(),
                    title: "Resurrected".into(),
                    position: 30,
                }
                .apply(&mut doc)
                .is_err()
            );
        }
    }
}
