//! Rich text as a component shows it: paragraphs with their marks, and how each reference in
//! them reads for the person reading.
//!
//! The paragraphs are tilo-doc's, as the app keeps them (kind, text, marks counted in
//! characters, attributes). A reference is a run marked `mention`, valued with what it refers
//! to (`tilo_reference::embed`); beside the paragraphs, [`RichText::embeds`] says how each
//! reference reads for this reader: their own inline view of it, resolved where the text was
//! read, under their permissions. The component draws only what it is given.
//!
//! A window's patches are encoded without field names (postcard), so every field is always
//! written: none is skipped when empty.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Rich text, with how its references read for the person reading it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichText {
    pub blocks: Vec<Block>,
    /// How each reference in the text reads, by the `mention` mark's value. A reference with
    /// no entry reads as its run.
    #[serde(default)]
    pub embeds: Vec<Embed>,
}

/// One paragraph: its kind (`paragraph`, `heading`, `list_item`, `quote`, `divider`), its
/// text, the marks over runs of it and its attributes (`level`, `ordered`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub marks: Vec<Mark>,
    #[serde(default)]
    pub attrs: BTreeMap<String, Attr>,
}

/// A mark over the characters `start..end` of a paragraph's text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub value: Attr,
}

/// The value of a mark or an attribute.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attr {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

/// How one reference reads for the person reading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Embed {
    /// The `mention` mark's value naming what it refers to.
    pub reference: String,
    pub view: InlineView,
}

/// A reference as it reads inline: the reader's inline view of its type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InlineView {
    /// A person: their avatar and what the room calls them.
    Person { identity: String, name: String },
    /// A resource the reader can read: its title as its app publishes it now, the icon of its
    /// type, and what its type shows beside the title (a task's state and number).
    Resource {
        title: String,
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        detail: Option<String>,
    },
    /// A resource kept where the text was read cannot say how it reads (another app's): it
    /// reads as its run, and opens by its reference.
    Elsewhere,
    /// A resource the reader cannot read: it is shown as existing, without its title or
    /// anything else of it, and offers to ask for access.
    Unreadable,
}

impl RichText {
    /// How the reference `reference` reads, when the text says.
    #[must_use]
    pub fn embed(&self, reference: &str) -> Option<&InlineView> {
        self.embeds
            .iter()
            .find(|embed| embed.reference == reference)
            .map(|embed| &embed.view)
    }

    /// Every string it holds, for a receiver that bounds what it is sent.
    pub fn strings(&self) -> impl Iterator<Item = &str> {
        self.blocks
            .iter()
            .flat_map(|block| {
                std::iter::once(block.kind.as_str())
                    .chain(std::iter::once(block.text.as_str()))
                    .chain(block.marks.iter().flat_map(|mark| {
                        std::iter::once(mark.name.as_str()).chain(mark.value.text())
                    }))
                    .chain(
                        block.attrs.iter().flat_map(|(key, value)| {
                            std::iter::once(key.as_str()).chain(value.text())
                        }),
                    )
            })
            .chain(self.embeds.iter().flat_map(|embed| {
                std::iter::once(embed.reference.as_str()).chain(embed.view.strings())
            }))
    }
}

impl Attr {
    fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Bool(_) | Self::Int(_) | Self::Float(_) => None,
        }
    }
}

impl InlineView {
    fn strings(&self) -> Vec<&str> {
        match self {
            Self::Person { identity, name } => vec![identity, name],
            Self::Resource {
                title,
                icon,
                detail,
            } => std::iter::once(title.as_str())
                .chain(icon.as_deref())
                .chain(detail.as_deref())
                .collect(),
            Self::Elsewhere | Self::Unreadable => Vec::new(),
        }
    }
}
