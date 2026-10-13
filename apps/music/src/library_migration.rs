//! Upgrade the former list-backed library through the resource's atomic,
//! deterministic housekeeping migration. Never initialize an existing record.

use std::collections::{BTreeMap, BTreeSet};

use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::tilo_resource::{Collection, Doc, Fields, Id, Model, ModelError, Set};

use crate::model::{Album, Artist, Library, Playlist, Track};

/// Version one covered both the old document bridge and the typed model.
/// Recognize the old bridge by its actual list and nested catalogue, rather
/// than treating every unreadable record as a new empty library.
pub(crate) fn migrate(doc: &mut Doc, from: u32) -> Result<(), ModelError> {
    if from != 1 {
        return Err(ModelError::invalid(format!(
            "no Music migration from {from}"
        )));
    }
    let playlists = unique(doc, &ROOT, "playlists")?;
    if let Some((Value::Object(ObjType::List), playlists)) = playlists {
        let catalogue = object(doc, &ROOT, "library")?
            .ok_or_else(|| ModelError::invalid("legacy Music catalogue is missing"))?;
        for name in ["artists", "albums", "tracks", "recommendations"] {
            if unique(doc, &ROOT, name)?.is_some() {
                return Err(ModelError::invalid("mixed Music library layouts").at(name));
            }
        }
        let library = legacy_library(doc, &catalogue, &playlists)?;
        library.check().map_err(ModelError::refused)?;
        // The old catalogue, tombstones, preference keys and all unrelated
        // state remain. Replacing the playlist slot preserves its original
        // change graph as well as every active item in the new collection.
        library.write_fields(doc, &ROOT)?;
    } else {
        if unique(doc, &ROOT, "library")?.is_some() {
            return Err(ModelError::invalid(
                "unrecognized legacy Music library layout",
            ));
        }
        // Already typed version-one records need only the version marker.
        // Invalid scalars or malformed collections still refuse unchanged.
        Library::read_fields(doc, &ROOT)?
            .check()
            .map_err(ModelError::refused)?;
    }
    Ok(())
}

fn unique(
    doc: &Doc,
    parent: &ObjId,
    key: &str,
) -> Result<Option<(Value<'static>, ObjId)>, ModelError> {
    let values = doc.get_all(parent, key)?;
    if values.len() > 1 {
        return Err(ModelError::invalid("conflicting legacy values; migration refused").at(key));
    }
    Ok(values
        .into_iter()
        .next()
        .map(|(value, id)| (value.to_owned(), id)))
}

fn object(doc: &Doc, parent: &ObjId, key: &str) -> Result<Option<ObjId>, ModelError> {
    match unique(doc, parent, key)? {
        None => Ok(None),
        Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
        Some(_) => Err(ModelError::shape("a map").at(key)),
    }
}

fn text(doc: &Doc, parent: &ObjId, key: &str) -> Result<String, ModelError> {
    match unique(doc, parent, key)? {
        None => Ok(String::new()),
        Some((Value::Object(ObjType::Text), id)) => Ok(doc.text(&id)?),
        Some((value, _)) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ModelError::shape("text").at(key)),
    }
}

fn boolean(doc: &Doc, parent: &ObjId, key: &str, default: bool) -> Result<bool, ModelError> {
    match unique(doc, parent, key)? {
        None => Ok(default),
        Some((value, _)) => value
            .as_bool()
            .ok_or_else(|| ModelError::shape("a boolean").at(key)),
    }
}

fn selections(doc: &Doc, key: &str) -> Result<Set<String>, ModelError> {
    let mut selected = Set::new();
    if let Some(map) = object(doc, &ROOT, key)? {
        for id in doc.keys(&map) {
            if boolean(doc, &map, &id, false)? {
                selected.insert(id);
            }
        }
    }
    Ok(selected)
}

fn catalogue<T: Fields>(
    doc: &Doc,
    parent: &ObjId,
    key: &str,
    identify: fn(&mut T, &str) -> Result<(), ModelError>,
) -> Result<BTreeMap<String, T>, ModelError> {
    let mut items = BTreeMap::new();
    if let Some(map) = object(doc, parent, key)? {
        for id in doc.keys(&map) {
            if id.is_empty() {
                return Err(ModelError::invalid("empty catalogue identity").at(key));
            }
            let entry = object(doc, &map, &id)?
                .ok_or_else(|| ModelError::invalid("missing catalogue item").at(&id))?;
            // Do not flatten a conflicting field into its winning value.
            for field in doc.keys(&entry) {
                unique(doc, &entry, &field)?;
            }
            let mut item = T::read_fields(doc, &entry)?;
            identify(&mut item, &id)?;
            items.insert(id, item);
        }
    }
    Ok(items)
}

fn identify(id: &mut String, key: &str) -> Result<(), ModelError> {
    if !id.is_empty() && id != key {
        return Err(ModelError::invalid(
            "catalogue identity differs from its key",
        ));
    }
    key.clone_into(id);
    Ok(())
}

fn legacy_library(
    doc: &Doc,
    catalogue_root: &ObjId,
    playlists: &ObjId,
) -> Result<Library, ModelError> {
    Ok(Library {
        artists: catalogue::<Artist>(doc, catalogue_root, "artists", |item, key| {
            identify(&mut item.id, key)
        })?,
        albums: catalogue::<Album>(doc, catalogue_root, "albums", |item, key| {
            identify(&mut item.id, key)
        })?,
        tracks: catalogue::<Track>(doc, catalogue_root, "tracks", |item, key| {
            identify(&mut item.id, key)
        })?,
        playlists: legacy_playlists(doc, playlists)?,
        liked: selections(doc, "liked")?,
        saved_albums: selections(doc, "saved_albums")?,
        followed_artists: selections(doc, "followed_artists")?,
        recommendations: preferences(doc)?,
    })
}

fn legacy_playlists(doc: &Doc, list: &ObjId) -> Result<Collection<Playlist>, ModelError> {
    let deleted: BTreeSet<_> = object(doc, &ROOT, "playlist_tombstones")?
        .map(|map| doc.keys(&map).collect())
        .unwrap_or_default();
    let mut playlists = Collection::new();
    for index in 0..doc.length(list) {
        let values = doc.get_all(list, index)?;
        let [(Value::Object(ObjType::Map), entry)] = values.as_slice() else {
            return Err(
                ModelError::invalid("invalid or conflicting legacy playlist").at("playlists"),
            );
        };
        let id = text(doc, entry, "id")?;
        if id.is_empty() || playlists.contains(&Id::from_raw(&id)) {
            return Err(ModelError::invalid("empty or duplicate playlist identity").at("playlists"));
        }
        if deleted.contains(&id) {
            continue;
        }
        let playlist = Playlist {
            id: id.clone(),
            name: text(doc, entry, "name")?,
            description: text(doc, entry, "description")?,
            cover: text(doc, entry, "cover")?,
            tracks: memberships(doc, entry)?,
        };
        playlists.push(Id::from_raw(id), playlist);
    }
    Ok(playlists)
}

fn memberships(doc: &Doc, playlist: &ObjId) -> Result<Collection<String>, ModelError> {
    let mut entries = Vec::new();
    let order = object(doc, playlist, "track_order")?;
    if let Some(membership) = object(doc, playlist, "track_membership")? {
        for id in doc.keys(&membership) {
            if boolean(doc, &membership, &id, false)? {
                let rank = match order
                    .as_ref()
                    .map(|order| unique(doc, order, &id))
                    .transpose()?
                    .flatten()
                {
                    None => 0,
                    Some((value, _)) => value
                        .as_i64()
                        .or_else(|| value.as_u64().and_then(|rank| i64::try_from(rank).ok()))
                        .ok_or_else(|| ModelError::shape("an integer rank").at(&id))?,
                };
                entries.push((rank, id));
            }
        }
    }
    // This is the former projection's order, including its stable tie-break.
    entries.sort();
    Ok(entries
        .into_iter()
        .map(|(_, id)| (Id::from_raw(&id), id))
        .collect())
}

fn preferences(doc: &Doc) -> Result<crate::recommendations::Preferences, ModelError> {
    let mut preferences = crate::recommendations::Preferences {
        likes: boolean(doc, &ROOT, "recommendations/input/likes", true)?,
        saved_albums: boolean(doc, &ROOT, "recommendations/input/saved-albums", true)?,
        followed_artists: boolean(doc, &ROOT, "recommendations/input/followed-artists", true)?,
        ..Default::default()
    };
    for key in doc.keys(ROOT) {
        if let Some(id) = key.strip_prefix("recommendations/preferred-artist/") {
            if boolean(doc, &ROOT, &key, false)? {
                preferences.preferred_artists.insert(id.to_owned());
            }
        } else if let Some(id) = key.strip_prefix("recommendations/dismissed/")
            && boolean(doc, &ROOT, &key, false)?
        {
            preferences.dismissed.insert(id.to_owned());
        }
    }
    Ok(preferences)
}

#[cfg(test)]
mod tests;
