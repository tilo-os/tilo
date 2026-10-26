//! The pictures people chose for themselves, as this process learnt them.
//!
//! A person's picture belongs to their profile, which the hosts of their
//! rooms keep signed by their root (their name and picture); the host
//! process fetches it and puts it here, under the person's root key (hex).
//! The places a person shows up know them by other ids too (the device
//! their presence comes from), so the host also names whose each of those
//! is ([`alias`]). Every avatar drawn for one of those ids then wears the
//! picture ([`crate::widget::AvatarFace`]), with no call site of its own:
//! the bar, the roster, the people overlay, a profile card.

use std::collections::BTreeMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tilo_lock::RwLockRecover as _;
use vello::peniko::ImageData;

/// Each person's picture, by their root key.
static PHOTOS: RwLock<BTreeMap<String, ImageData>> = RwLock::new(BTreeMap::new());
/// Whose each other id (a device) is: the person's root key.
static ALIASES: RwLock<BTreeMap<String, String>> = RwLock::new(BTreeMap::new());
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// `person` (root key, hex) wears `image` from now on; `None` takes their
/// picture away (they went back to their initials).
pub fn set(person: &str, image: Option<ImageData>) {
    let mut photos = PHOTOS.write_recover();
    match image {
        Some(image) => {
            photos.insert(person.to_owned(), image);
        }
        None => {
            if photos.remove(person).is_none() {
                return;
            }
        }
    }
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// `identity` (a device's key, say) is `person`'s.
pub fn alias(identity: &str, person: &str) {
    if identity == person {
        return;
    }
    let mut aliases = ALIASES.write_recover();
    if aliases.get(identity).is_some_and(|known| known == person) {
        return;
    }
    aliases.insert(identity.to_owned(), person.to_owned());
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// The picture of whoever `identity` names (their root key, or an id
/// [`alias`]ed to it), if they chose one.
#[must_use]
pub fn photo(identity: &str) -> Option<ImageData> {
    if identity.is_empty() {
        return None;
    }
    let photos = PHOTOS.read_recover();
    if photos.is_empty() {
        return None;
    }
    if let Some(image) = photos.get(identity) {
        return Some(image.clone());
    }
    let aliases = ALIASES.read_recover();
    photos.get(aliases.get(identity)?).cloned()
}

/// Changes whenever a picture or whose an id is changes: a host redraws
/// what shows avatars when it moves.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::peniko::{Blob, ImageAlphaType, ImageFormat};

    fn image() -> ImageData {
        ImageData {
            data: Blob::new(std::sync::Arc::new(vec![255u8; 4])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        }
    }

    #[test]
    fn a_picture_shows_under_the_person_and_every_id_of_theirs_until_taken_away() {
        let before = generation();
        assert!(photo("people-photos-ada").is_none());
        set("people-photos-ada", Some(image()));
        alias("people-photos-ada-laptop", "people-photos-ada");
        assert!(generation() > before);
        assert!(photo("people-photos-ada").is_some());
        assert!(photo("people-photos-ada-laptop").is_some());
        assert!(photo("people-photos-bo").is_none());
        set("people-photos-ada", None);
        assert!(photo("people-photos-ada-laptop").is_none());
    }
}
