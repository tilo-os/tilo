//! Annotations every app shares: comment threads, whatever they are about,
//! and the colours a passage is marked in.
//!
//! A [`Comment`]'s anchor `A` is the app's own (a
//! [`TextAnchor`](crate::TextAnchor) in rich text, cells of a sheet, a spot
//! on a page); who wrote what, when, the answers and whether it is resolved
//! are the same everywhere. A [`HighlightColor`] is one of a small named
//! palette, so a highlight made in one reader reads the same in another, or
//! an exact colour a file brought along.

use tilo_resource::{Collection, Model};

use crate::ClipBlock;

/// A comment thread about `anchor`.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Comment<A: Model> {
    /// What the comment is about.
    pub anchor: A,
    /// Who wrote it.
    #[model(readonly)]
    pub author: String,
    /// When it was written, as an ISO-8601 time; empty when unknown.
    #[model(readonly)]
    pub date: String,
    /// What it says, as plain text.
    #[model(label)]
    pub text: String,
    /// Its paragraphs as written, with their formatting; empty when it is
    /// plain text.
    pub body: Vec<ClipBlock>,
    /// Whether the thread is done with.
    pub resolved: bool,
    /// The answers, in the order they were written.
    pub replies: Collection<Reply>,
}

impl<A: Model> Comment<A> {
    /// A new thread by `author` about `anchor`, saying `text`.
    #[must_use]
    pub fn new(anchor: A, author: String, date: String, text: String) -> Self {
        Self {
            anchor,
            author,
            date,
            text,
            body: Vec::new(),
            resolved: false,
            replies: Collection::new(),
        }
    }
}

/// An answer in a comment thread.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Reply {
    /// Who wrote it.
    #[model(readonly)]
    pub author: String,
    /// When it was written, as an ISO-8601 time; empty when unknown.
    #[model(readonly)]
    pub date: String,
    /// What it says, as plain text.
    #[model(label)]
    pub text: String,
    /// Its paragraphs as written, with their formatting; empty when it is
    /// plain text.
    pub body: Vec<ClipBlock>,
}

impl Reply {
    /// A plain-text answer by `author`.
    #[must_use]
    pub fn new(author: String, date: String, text: String) -> Self {
        Self {
            author,
            date,
            text,
            body: Vec::new(),
        }
    }
}

/// The colour of a highlight or other marking: a named swatch, or an exact
/// colour (`0xRRGGBB`) markup a file brought along keeps rather than being
/// snapped to a swatch. Stored and carried as its key: `yellow`, … or
/// `#rrggbb`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HighlightColor {
    #[default]
    Yellow,
    Green,
    Pink,
    Blue,
    Purple,
    Custom(u32),
}

impl HighlightColor {
    /// The named swatches, as a palette offers them.
    pub const ALL: [Self; 5] = [
        Self::Yellow,
        Self::Green,
        Self::Pink,
        Self::Blue,
        Self::Purple,
    ];

    /// How it is stored: the swatch's name, or `#rrggbb`.
    #[must_use]
    pub fn key(self) -> String {
        match self {
            Self::Custom(rgb) => format!("#{:06x}", rgb & 0x00ff_ffff),
            named => named.name().to_ascii_lowercase(),
        }
    }

    /// The colour stored as `key`; yellow for one that names none.
    #[must_use]
    pub fn parse(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|named| named.name().eq_ignore_ascii_case(key))
            .or_else(|| {
                key.strip_prefix('#')
                    .filter(|hex| hex.len() == 6)
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .map(Self::Custom)
            })
            .unwrap_or_default()
    }

    /// The colour's name: its swatch's tooltip and accessible name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Pink => "Pink",
            Self::Blue => "Blue",
            Self::Purple => "Purple",
            Self::Custom(_) => "Custom",
        }
    }

    /// The named swatch with exactly this colour, or a custom one.
    #[must_use]
    pub fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|named| named.rgb() == (r, g, b))
            .unwrap_or_else(|| {
                Self::Custom((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b))
            })
    }

    /// Its sRGB colour. A marking is the document's own content, so the same
    /// colour is painted on screen and written into a saved file, where it
    /// looks the same in every viewer.
    #[must_use]
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Self::Yellow => (250, 204, 21),
            Self::Green => (34, 197, 94),
            Self::Pink => (236, 72, 153),
            Self::Blue => (59, 130, 246),
            Self::Purple => (139, 92, 246),
            Self::Custom(rgb) => {
                let [_, r, g, b] = rgb.to_be_bytes();
                (r, g, b)
            }
        }
    }
}

impl serde::Serialize for HighlightColor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.key())
    }
}

impl<'de> serde::Deserialize<'de> for HighlightColor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|key| Self::parse(&key))
    }
}

impl tilo_resource::JsonSchema for HighlightColor {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "string",
            "description": "yellow, green, pink, blue, purple, or an exact colour #rrggbb",
        })
    }
}

tilo_resource::json_model!(HighlightColor);

#[cfg(test)]
mod tests {
    use super::HighlightColor;

    #[test]
    fn a_highlight_colour_is_stored_as_its_key() {
        for named in HighlightColor::ALL {
            assert_eq!(HighlightColor::parse(&named.key()), named);
            assert_eq!(
                HighlightColor::from_rgb(named.rgb().0, named.rgb().1, named.rgb().2),
                named
            );
        }
        let exact = HighlightColor::from_rgb(1, 2, 3);
        assert_eq!(exact, HighlightColor::Custom(0x0001_0203));
        assert_eq!(HighlightColor::parse(&exact.key()), exact);
        assert_eq!(HighlightColor::parse("plaid"), HighlightColor::Yellow);
    }
}
