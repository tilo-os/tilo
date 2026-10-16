//! Web entries: the base URLs a page or device serves links under.

use super::{HostOrigin, LinkError, encode};

/// An entry base (`https://app.example/`, `https://machine.example:8443/open/`).
/// Every link under it names its workspace: no entry stands for a room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebEntry {
    base: String,
    origin: HostOrigin,
}

impl WebEntry {
    /// # Errors
    ///
    /// Refuses a base that is not `https://host[:port]/path/` (plain http only
    /// on this computer), carries a query or fragment, or ends in `app/`, which
    /// the grammar reserves for the full-tab mode.
    pub fn new(base: &str) -> Result<Self, LinkError> {
        let (scheme, rest) = if let Some(rest) = base.strip_prefix("https://") {
            ("https://", rest)
        } else if let Some(rest) = base.strip_prefix("http://") {
            ("http://", rest)
        } else {
            return Err(LinkError::BadEntry("an entry starts with https://"));
        };
        let (authority, path) = rest
            .split_once('/')
            .ok_or(LinkError::BadEntry("an entry ends in /"))?;
        let origin = HostOrigin::new(&format!("{scheme}{authority}"))?;
        if !path.is_empty() {
            let inner = path
                .strip_suffix('/')
                .ok_or(LinkError::BadEntry("an entry ends in /"))?;
            for part in inner.split('/') {
                if part.is_empty() || encode::decode_segment(part).is_err() {
                    return Err(LinkError::BadEntry("an entry path is plain segments"));
                }
            }
            if inner.rsplit('/').next() == Some("app") {
                return Err(LinkError::BadEntry(
                    "an entry cannot end in app/, which names the full-tab mode",
                ));
            }
        }
        Ok(Self {
            base: base.to_owned(),
            origin,
        })
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    #[must_use]
    pub fn origin(&self) -> &HostOrigin {
        &self.origin
    }
}

/// The entries a page or device knows: the page's own,
/// or natively the entries the device's hosts announced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebEntries {
    entries: Vec<WebEntry>,
}

impl WebEntries {
    #[must_use]
    pub fn new(entries: Vec<WebEntry>) -> Self {
        Self { entries }
    }

    /// The longest entry that `url` starts with, and what follows it.
    #[must_use]
    pub fn find<'u>(&self, url: &'u str) -> Option<(&WebEntry, &'u str)> {
        self.entries
            .iter()
            .filter_map(|entry| {
                url.strip_prefix(entry.base.as_str())
                    .map(|rest| (entry, rest))
            })
            .max_by_key(|(entry, _)| entry.base.len())
    }
}
