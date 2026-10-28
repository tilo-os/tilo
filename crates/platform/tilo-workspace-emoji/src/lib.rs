//! A workspace's own emoji: a name and a small picture each, kept in one
//! record of the workspace's room that every app of the workspace opens, so
//! `:name:` draws the same picture in a reaction, a chat message or a note.
//!
//! The record is Chat's ([`OWNER`]): its actions are decided against Chat's
//! committed surface, by the room's grants, wherever they are invoked. Any
//! member who may make undoable changes (an Editor) adds emoji and removes
//! their own; removing someone else's is moderation, as removing someone
//! else's message is.
//!
//! The picture lives inside the record rather than in a content store:
//! content is kept per room *and app*, so an emoji added from Notes would be
//! unreadable to Chat. That bounds it: [`MAX_IMAGE_BYTES`] keeps the picture,
//! and the action that adds it, below the size at which an action's payload
//! moves to the adding app's content store.

use base64::Engine as _;
use tilo_resource::{ActionCx, Doc, Map, Model, ModelError, Refusal, Settable, Slot, resource};

#[cfg(test)]
mod tests;

/// The app whose record this is, and whose surface decides its changes:
/// Chat ships wherever a workspace is used, natively and in the browser.
pub const OWNER: &str = "so.tilo.chat";

/// The record's resource kind.
pub const KIND: &str = "tilo.workspace.emoji";

/// The largest picture an emoji keeps, in bytes. An action's text or byte
/// runs of 16 KiB or more are kept apart as content of the app invoking it
/// (`tilo_actions::CONTENT_PART_THRESHOLD`), which other apps of the room
/// cannot read; the picture travels as base64, so it stays below 12 KiB.
pub const MAX_IMAGE_BYTES: usize = 12_000;

/// The most emoji a workspace keeps.
pub const MAX_EMOJI: usize = 250;

/// The longest a person id stored with an emoji may be.
const MAX_AUTHOR: usize = 4096;

/// An emoji's picture, PNG or WebP, kept inside the record as one bytes
/// value: written whole, last writer wins. Its JSON form (an action's
/// argument, an agent's read) is the base64 of the bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmojiImage(pub Vec<u8>);

impl EmojiImage {
    /// The picture's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// The picture as an action's argument carries it.
    #[must_use]
    pub fn to_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.0)
    }

    /// A picture an action's argument carries.
    ///
    /// # Errors
    /// The text is not base64.
    pub fn from_base64(text: &str) -> Result<Self, Refusal> {
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map(Self)
            .map_err(|_| Refusal::new("the picture is not base64"))
    }

    /// Whether this is a picture an emoji may keep: PNG or WebP, at most
    /// [`MAX_IMAGE_BYTES`].
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.0.is_empty() && self.0.len() <= MAX_IMAGE_BYTES && is_png_or_webp(&self.0)
    }
}

/// Whether `bytes` start as a PNG or a WebP file does.
#[must_use]
pub fn is_png_or_webp(bytes: &[u8]) -> bool {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    bytes.starts_with(PNG)
        || bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")
}

impl Settable for EmojiImage {}
impl Model for EmojiImage {
    tilo_resource::__own_table!(EmojiImage);
    tilo_resource::leaf_paths!();
    const SCALAR: bool = true;
    fn value_schema() -> serde_json::Value {
        serde_json::json!({ "type": "string", "contentEncoding": "base64" })
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.to_base64())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(text) => {
                Self::from_base64(text).map_err(|_| ModelError::invalid("not base64"))
            }
            _ => Err(ModelError::shape("a base64 picture")),
        }
    }
    fn load(
        _: &Doc,
        value: &automerge::Value<'_>,
        _: &automerge::ObjId,
    ) -> Result<Self, ModelError> {
        match value {
            automerge::Value::Scalar(scalar) => match scalar.as_ref() {
                automerge::ScalarValue::Bytes(data) => Ok(Self(data.clone())),
                automerge::ScalarValue::Null => Ok(Self::default()),
                _ => Err(ModelError::shape("a picture")),
            },
            automerge::Value::Object(_) => Err(ModelError::shape("a picture")),
        }
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        slot.put_scalar(doc, automerge::ScalarValue::Bytes(self.0.clone()))
    }
}

/// One emoji of the workspace.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Emoji {
    /// Its picture.
    pub image: EmojiImage,
    /// Who added it: they may remove it again.
    pub added_by: String,
    /// When, in milliseconds since the Unix epoch.
    pub added_at: i64,
}

/// The workspace's emoji, by name (without colons).
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = WorkspaceEmoji::valid)]
pub struct WorkspaceEmoji {
    pub emoji: Map<Emoji>,
}

#[resource(kind = "tilo.workspace.emoji", label = "Workspace emoji")]
impl WorkspaceEmoji {
    /// Add emoji.
    ///
    /// A picture (PNG or WebP, base64, at most 12 000 bytes; 64 pixels a
    /// side draws sharply) under a name of 2 to 32 letters, digits and
    /// underscores, written `:name:` in reactions and text.
    #[action]
    pub fn add_emoji(
        &mut self,
        cx: &mut ActionCx,
        #[param("The name, without colons: 2 to 32 letters, digits and underscores")] name: String,
        #[param("The picture, PNG or WebP, base64")] image: String,
    ) -> Result<(), Refusal> {
        if !tilo_resource::valid_custom_emoji_name(&name) {
            return Err(Refusal::new(
                "an emoji's name is 2 to 32 letters, digits and underscores",
            ));
        }
        if self.emoji.contains_key(&name) {
            return Err(Refusal::new(format!("there is already an emoji :{name}:")));
        }
        if self.emoji.len() >= MAX_EMOJI {
            return Err(Refusal::new(format!(
                "a workspace keeps at most {MAX_EMOJI} emoji"
            )));
        }
        let image = EmojiImage::from_base64(&image)?;
        if !image.valid() {
            return Err(Refusal::new(format!(
                "an emoji's picture is a PNG or WebP of at most {MAX_IMAGE_BYTES} bytes"
            )));
        }
        let added_by = author(cx)?;
        self.emoji.insert(
            name,
            Emoji {
                image,
                added_by,
                added_at: cx.now_ms(),
            },
        );
        Ok(())
    }

    /// Remove emoji.
    ///
    /// One you added.
    #[action]
    pub fn remove_emoji(
        &mut self,
        cx: &mut ActionCx,
        #[param("The emoji's name, without colons")] name: String,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let emoji = self.emoji.get(&name).ok_or_else(|| no_such(&name))?;
        if emoji.added_by != me {
            return Err(Refusal::new(format!(
                ":{name}: was added by someone else; removing it is moderation"
            )));
        }
        self.emoji.remove(&name);
        Ok(())
    }

    /// Remove anyone's emoji.
    ///
    /// Someone else's emoji, as a moderator: the room's roles decide who may
    /// (Moderators, administrators and the owner).
    #[action(moderation)]
    pub fn remove_any_emoji(
        &mut self,
        #[param("The emoji's name, without colons")] name: String,
    ) -> Result<(), Refusal> {
        self.emoji
            .remove(&name)
            .map(drop)
            .ok_or_else(|| no_such(&name))
    }
}

impl WorkspaceEmoji {
    fn valid(&self) -> Result<(), Refusal> {
        if self.emoji.len() > MAX_EMOJI {
            return Err(Refusal::new(format!(
                "a workspace keeps at most {MAX_EMOJI} emoji"
            )));
        }
        for (name, emoji) in self.emoji.iter() {
            if !tilo_resource::valid_custom_emoji_name(name) {
                return Err(Refusal::new(format!("{name:?} cannot name an emoji")));
            }
            if !emoji.image.valid() {
                return Err(Refusal::new(format!(
                    ":{name}:'s picture is not a PNG or WebP of at most {MAX_IMAGE_BYTES} bytes"
                )));
            }
            if emoji.added_by.is_empty() || emoji.added_by.len() > MAX_AUTHOR {
                return Err(Refusal::new(format!(":{name}: names nobody who added it")));
            }
        }
        Ok(())
    }

    /// The emoji's names, sorted: what a bridge maps to the other side's
    /// emoji, and what `:name:` may name here.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.emoji.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// The emoji called `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Emoji> {
        self.emoji.get(name)
    }

    /// A name for an emoji made from `stem` (a file's name without its
    /// extension) that no emoji of this workspace has yet: letters, digits
    /// and underscores only, at most 32, with `_2`, `_3`, … appended where
    /// the name is taken. `None` when nothing of `stem` can name one.
    #[must_use]
    pub fn free_name(&self, stem: &str) -> Option<String> {
        let mut base: String = stem
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        while base.contains("__") {
            base = base.replace("__", "_");
        }
        let mut base = base.trim_matches('_').to_owned();
        base.truncate(tilo_resource::reactions::MAX_CUSTOM_NAME);
        if base.len() < 2 {
            base = format!("emoji_{base}");
            base.truncate(base.trim_end_matches('_').len());
        }
        if !tilo_resource::valid_custom_emoji_name(&base) {
            return None;
        }
        if !self.emoji.contains_key(&base) {
            return Some(base);
        }
        (2..=MAX_EMOJI + 1).find_map(|n| {
            let suffix = format!("_{n}");
            let mut name = base.clone();
            name.truncate(tilo_resource::reactions::MAX_CUSTOM_NAME.saturating_sub(suffix.len()));
            name.push_str(&suffix);
            (!self.emoji.contains_key(&name)).then_some(name)
        })
    }
}

fn no_such(name: &str) -> Refusal {
    Refusal::new(format!("there is no emoji :{name}:"))
}

/// Who the action is by: an emoji is always someone's.
fn author(cx: &ActionCx) -> Result<String, Refusal> {
    cx.author()
        .filter(|author| !author.is_empty() && author.len() <= MAX_AUTHOR)
        .map(str::to_owned)
        .ok_or_else(|| Refusal::new("an emoji is always added by someone"))
}
