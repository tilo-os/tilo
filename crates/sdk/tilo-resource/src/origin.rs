//! Where content came from, when it was written somewhere else first.
//!
//! Content moved into a workspace (an issue from another tracker, a page
//! from another wiki, a message from another chat) was written by someone
//! there, at a time, and has a page there. An action that writes such
//! content takes an [`Origin`] beside its other arguments and keeps it with
//! the item, next to who wrote the item here: the record keeps both. A view
//! shows the item as by its original author in its source, on its original
//! date, and the record's own history still says who moved it and when.
//!
//! The original author is a name and a handle at the source, never a member
//! of the workspace: someone named in an old tracker need not have joined.

use serde::{Deserialize, Serialize};

use crate::{Model, Refusal};

/// The longest name, handle, source or link an origin keeps.
pub const MAX_ORIGIN_CHARS: usize = 2048;

/// Where content was first written, before it came into the workspace.
#[derive(
    Model, Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, crate::JsonSchema,
)]
pub struct Origin {
    /// Its author there, by name (`Jane Doe`).
    pub author: String,
    /// Its author's handle there (`jane`, `jane@example.com`), when it has
    /// one.
    #[serde(default)]
    pub handle: Option<String>,
    /// The tool it was written in (`Jira`, `GitHub`, `Notion`).
    pub source: String,
    /// Its page there.
    #[serde(default)]
    pub url: Option<String>,
    /// When it was written there, in milliseconds since the epoch.
    #[serde(default)]
    pub created_ms: Option<i64>,
    /// When it last changed there, in milliseconds since the epoch.
    #[serde(default)]
    pub updated_ms: Option<i64>,
}

impl Origin {
    /// The origin as it is kept: its texts trimmed, blank optional ones
    /// dropped.
    ///
    /// # Errors
    /// Refuses an origin without an author or a source, a text too long to
    /// keep, or a last change before its creation.
    pub fn checked(self) -> Result<Self, Refusal> {
        let trim = |text: String| text.trim().to_owned();
        let optional = |text: Option<String>| text.map(trim).filter(|text| !text.is_empty());
        let origin = Self {
            author: trim(self.author),
            handle: optional(self.handle),
            source: trim(self.source),
            url: optional(self.url),
            created_ms: self.created_ms,
            updated_ms: self.updated_ms,
        };
        if origin.author.is_empty() {
            return Err(Refusal::new("An origin names who wrote it there"));
        }
        if origin.source.is_empty() {
            return Err(Refusal::new("An origin names the tool it was written in"));
        }
        let texts = [
            Some(&origin.author),
            origin.handle.as_ref(),
            Some(&origin.source),
            origin.url.as_ref(),
        ];
        if texts
            .into_iter()
            .flatten()
            .any(|text| text.chars().count() > MAX_ORIGIN_CHARS)
        {
            return Err(Refusal::new("An origin's texts fit on a line"));
        }
        if let (Some(created), Some(updated)) = (origin.created_ms, origin.updated_ms)
            && updated < created
        {
            return Err(Refusal::new(
                "An origin's last change is not before it was written",
            ));
        }
        Ok(origin)
    }

    /// [`Self::checked`] of an optional origin.
    ///
    /// # Errors
    /// As [`Self::checked`].
    pub fn checked_option(origin: Option<Self>) -> Result<Option<Self>, Refusal> {
        origin.map(Self::checked).transpose()
    }
}
