//! A workspace's own emoji, as every view of a window draws them.
//!
//! A workspace keeps its emoji (a name and a small picture each) in a record
//! of its own, the same in every app of the workspace. The window's host
//! opens that record and registers it for the window ([`register`]) against
//! the window's wake scope, as it registers the window's content store;
//! any view, widget or text layout of the window then reads it with
//! [`current`] or [`emoji`] and draws `:name:` as the picture, with no
//! plumbing of its own. Where the workspace has no emoji of a name, or the
//! window none registered, `:name:` stays text.
//!
//! Adding and removing emoji are actions of that record, which the room's
//! grants decide ([`EmojiSource::can_manage`]): the shared picker offers
//! "Add emoji" only to whoever may.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use vello::peniko::ImageData;

use crate::wake::WakeHandle;

/// The largest picture a workspace emoji keeps, in bytes, once made small
/// ([`EmojiSource::add`] makes it so): it travels inside the action that
/// adds it, as base64, below the size at which an action's payload is kept
/// apart as the invoking app's content, which another app could not read.
pub const MAX_IMAGE_BYTES: usize = 12_000;
/// The side a workspace emoji's picture is kept at, at most, in pixels:
/// sharp at twice the size a reaction pill or a line of text draws it.
pub const IMAGE_SIDE: u32 = 64;
/// The most emoji a workspace keeps.
pub const MAX_EMOJI: usize = 250;

/// One workspace emoji.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomEmoji {
    /// Its name, without colons (`party_parrot`).
    pub name: String,
    /// Its picture, PNG or WebP.
    pub image: Arc<[u8]>,
}

/// The emoji of a workspace, by name, with their pictures decoded once.
#[derive(Default)]
pub struct WorkspaceEmoji {
    emoji: Vec<CustomEmoji>,
    decoded: RefCell<HashMap<String, Option<ImageData>>>,
}

impl std::fmt::Debug for WorkspaceEmoji {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.emoji.iter().map(|emoji| &emoji.name))
            .finish()
    }
}

impl PartialEq for WorkspaceEmoji {
    fn eq(&self, other: &Self) -> bool {
        self.emoji == other.emoji
    }
}

impl WorkspaceEmoji {
    /// These emoji, sorted by name; a name given twice keeps its first.
    #[must_use]
    pub fn new(mut emoji: Vec<CustomEmoji>) -> Self {
        emoji.sort_by(|a, b| a.name.cmp(&b.name));
        emoji.dedup_by(|b, a| a.name == b.name);
        Self {
            emoji,
            decoded: RefCell::default(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.emoji.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.emoji.len()
    }

    /// Every emoji, by name.
    pub fn iter(&self) -> impl Iterator<Item = &CustomEmoji> {
        self.emoji.iter()
    }

    /// The emoji called `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&CustomEmoji> {
        self.emoji
            .binary_search_by(|emoji| emoji.name.as_str().cmp(name))
            .ok()
            .and_then(|at| self.emoji.get(at))
    }

    /// The emoji a reaction's or a text's `:name:` names, when the workspace
    /// has it.
    #[must_use]
    pub fn resolve(&self, written: &str) -> Option<&CustomEmoji> {
        tilo_resource::custom_emoji_name(written).and_then(|name| self.get(name))
    }

    /// The emoji whose names hold `query`, ignoring case: those starting
    /// with it first.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<&CustomEmoji> {
        let query = query.trim().trim_matches(':').to_ascii_lowercase();
        let mut found: Vec<(bool, &CustomEmoji)> = self
            .emoji
            .iter()
            .filter_map(|emoji| {
                let name = emoji.name.to_ascii_lowercase();
                name.find(&query).map(|at| (at != 0, emoji))
            })
            .collect();
        found.sort_by_key(|(later, _)| *later);
        found.into_iter().map(|(_, emoji)| emoji).collect()
    }

    /// The picture of the emoji called `name`, decoded: `None` while it is
    /// still to decode (the decoders are fetched on first use in a browser)
    /// or when the workspace has no such emoji or its picture is broken.
    #[must_use]
    pub fn image(&self, name: &str) -> Option<ImageData> {
        if let Some(decoded) = self.decoded.borrow().get(name) {
            return decoded.clone();
        }
        let emoji = self.get(name)?;
        let decoded = match crate::image_store::decode_held(&emoji.image, IMAGE_SIDE) {
            crate::image_store::HeldPicture::Ready(image) => Some(image),
            crate::image_store::HeldPicture::Broken => None,
            crate::image_store::HeldPicture::Pending => return None,
        };
        self.decoded
            .borrow_mut()
            .insert(name.to_owned(), decoded.clone());
        decoded
    }
}

/// What a window's host offers its views of the workspace's emoji: the
/// workspace's emoji record, opened for the window.
pub trait EmojiSource {
    /// The emoji now.
    fn emoji(&self) -> Rc<WorkspaceEmoji>;

    /// Moves whenever the emoji changed: a view that drew them repaints.
    fn revision(&self) -> u64;

    /// Whether the person may add and remove the workspace's emoji.
    fn can_manage(&self) -> bool;

    /// Add `image` (PNG, JPEG or WebP of any size) as the emoji `name`: made
    /// small first ([`IMAGE_SIDE`], [`MAX_IMAGE_BYTES`]), then added by the
    /// record's action, attributed to the person.
    ///
    /// # Errors
    /// Why it was not added: no grant, a name taken or not allowed, a
    /// picture that does not decode or stays too large.
    fn add(&self, name: &str, image: &[u8]) -> Result<(), String>;

    /// Remove the emoji `name`.
    ///
    /// # Errors
    /// Why it was not removed.
    fn remove(&self, name: &str) -> Result<(), String>;

    /// Let the person choose a picture and add it as an emoji named after
    /// it, as the picker's "Add emoji" does. The host's chooser answers
    /// later; the emoji arrives with the record's next change.
    fn choose_and_add(&self);
}

/// A picture made one a workspace emoji keeps ([`make_small`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Small {
    /// A PNG at most [`IMAGE_SIDE`] a side and [`MAX_IMAGE_BYTES`] long.
    Ready(Vec<u8>),
    /// The image tools are still to come (a browser fetches them on first
    /// use): ask again once they are in.
    Pending,
    /// It is no picture this can read (PNG, JPEG or WebP).
    NotAPicture,
    /// Even made smaller than [`IMAGE_SIDE`] it stays too large.
    TooLarge,
}

/// `image` (PNG, JPEG or WebP of any size) made a picture a workspace
/// emoji keeps: fitted within [`IMAGE_SIDE`], aspect kept, as a PNG; a
/// picture too detailed to fit [`MAX_IMAGE_BYTES`] at that side is made
/// smaller still, down to half of it.
#[must_use]
pub fn make_small(image: &[u8]) -> Small {
    let mut side = IMAGE_SIDE;
    while side >= IMAGE_SIDE / 2 {
        match crate::image_store::small_png(image, side) {
            crate::image_store::SmallPicture::Ready(png) if png.len() <= MAX_IMAGE_BYTES => {
                return Small::Ready(png);
            }
            crate::image_store::SmallPicture::Ready(_) => side = side.saturating_sub(8),
            crate::image_store::SmallPicture::Pending => return Small::Pending,
            crate::image_store::SmallPicture::Broken => return Small::NotAPicture,
        }
    }
    Small::TooLarge
}

impl Small {
    /// Why it was not made small, for the person; `None` when it was.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        match self {
            Self::Ready(_) => None,
            Self::Pending => Some("the picture tools are still loading; try again".into()),
            Self::NotAPicture => Some("that is not a PNG, JPEG or WebP picture".into()),
            Self::TooLarge => Some("that picture is too detailed for an emoji".into()),
        }
    }
}

thread_local! {
    static SOURCES: RefCell<Vec<(WakeHandle, Rc<dyn EmojiSource>)>> =
        const { RefCell::new(Vec::new()) };
}

/// Keep `source` as the workspace emoji of the window whose wake scope
/// `window` names, in place of any it had. Windows that closed are
/// forgotten.
pub fn register(window: &WakeHandle, source: Rc<dyn EmojiSource>) {
    SOURCES.with(|sources| {
        let mut sources = sources.borrow_mut();
        sources.retain(|(handle, _)| handle.is_active() && !handle.same_open_scope(window));
        sources.push((window.clone(), source));
    });
}

/// The workspace emoji of the window whose code is running: the source
/// registered for the wake scope entered now. `None` outside a window, or
/// for one whose host registered none.
#[must_use]
pub fn current() -> Option<Rc<dyn EmojiSource>> {
    let here = WakeHandle::new();
    SOURCES.with(|sources| {
        sources
            .borrow()
            .iter()
            .find(|(handle, _)| handle.same_open_scope(&here))
            .map(|(_, source)| Rc::clone(source))
    })
}

/// The workspace emoji of the window whose code is running; none outside a
/// window or where its host registered none.
#[must_use]
pub fn emoji() -> Rc<WorkspaceEmoji> {
    current().map_or_else(Rc::default, |source| source.emoji())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> CustomEmoji {
        CustomEmoji {
            name: name.to_owned(),
            image: Arc::from(&b"not a picture"[..]),
        }
    }

    #[test]
    fn emoji_are_found_by_name_and_by_written_form() {
        let emoji = WorkspaceEmoji::new(vec![named("tilo"), named("party_parrot"), named("tilo")]);
        assert_eq!(emoji.len(), 2, "a name given twice keeps its first");
        assert_eq!(emoji.get("tilo").map(|e| e.name.as_str()), Some("tilo"));
        assert!(emoji.resolve(":party_parrot:").is_some());
        assert!(emoji.resolve("party_parrot").is_none());
        assert!(emoji.resolve(":missing:").is_none());
        let found: Vec<&str> = emoji.search("PA").iter().map(|e| e.name.as_str()).collect();
        assert_eq!(found, ["party_parrot"]);
        let found: Vec<&str> = emoji.search(":t").iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            found,
            ["tilo", "party_parrot"],
            "a name starting with it first"
        );
        assert!(
            emoji.image("tilo").is_none(),
            "a broken picture draws nothing"
        );
    }

    #[test]
    fn a_picture_is_made_a_small_png() {
        let mut large = image::RgbaImage::new(300, 150);
        for (x, y, pixel) in large.enumerate_pixels_mut() {
            *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, 90, 255]);
        }
        let mut encoded = Vec::new();
        image::DynamicImage::ImageRgba8(large)
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let Small::Ready(png) = make_small(&encoded) else {
            panic!("a JPEG is made a small PNG");
        };
        assert!(png.len() <= MAX_IMAGE_BYTES);
        let small = image::load_from_memory(&png).unwrap();
        assert_eq!((small.width(), small.height()), (64, 32), "aspect kept");
        assert_eq!(make_small(b"not a picture"), Small::NotAPicture);
    }

    #[test]
    fn no_window_has_no_emoji() {
        assert!(current().is_none());
        assert!(emoji().is_empty());
    }
}
