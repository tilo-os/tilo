//! Bounded reviewed browser migration values, independent of application UI.
use serde::{Deserialize, Serialize};
use tilo_actions::ActionError;

pub const MAX_IMPORT_ENTRIES: usize = 10_000;
pub const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_BUNDLE_ENTRIES: usize = 100_000;
pub const MAX_BUNDLE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookmarkImport {
    pub url: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryImport {
    pub url: String,
    pub title: String,
    pub last_visit: u64,
    pub visit_count: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBatch {
    /// Fingerprint of the reviewed source snapshot, never an ambient filesystem path.
    pub source: [u8; 32],
    pub bookmarks: Vec<BookmarkImport>,
    pub history: Vec<HistoryImport>,
}

/// Complete bounded reviewed snapshot; chunk identities bind the source, ordinal and content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBundle {
    pub source: [u8; 32],
    pub chunks: Vec<ImportBatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_tree: Option<crate::imported_folders::ImportedTree>,
}
impl ImportBundle {
    /// Decode a complete bounded action payload.
    /// # Errors
    /// Refuses oversized, malformed or incorrectly bound chunks.
    pub fn decode(value: &str) -> Result<Self, ActionError> {
        if value.len() > MAX_BUNDLE_BYTES {
            return Err(refused());
        }
        let bundle: Self = serde_json::from_str(value).map_err(|_| refused())?;
        bundle.validate()?;
        Ok(bundle)
    }
    /// Partition a complete normalized capture without discarding rows.
    /// # Errors
    /// Refuses sources exceeding bounded custody or invalid chunk metadata.
    pub fn from_batch(mut batch: ImportBatch) -> Result<Self, ActionError> {
        if batch.bookmarks.len().saturating_add(batch.history.len()) > MAX_BUNDLE_ENTRIES {
            return Err(refused());
        }
        let source = batch.source;
        let mut chunks = Vec::new();
        while !batch.bookmarks.is_empty() || !batch.history.is_empty() {
            let bookmarks: Vec<_> = batch
                .bookmarks
                .drain(..batch.bookmarks.len().min(500))
                .collect();
            let history: Vec<_> = batch
                .history
                .drain(..batch.history.len().min(500))
                .collect();
            let mut chunk = ImportBatch {
                source,
                bookmarks,
                history,
            };
            chunk.validate()?;
            chunk.source = chunk_binding(&source, chunks.len(), &chunk);
            chunks.push(chunk);
        }
        let bundle = Self {
            source,
            chunks,
            imported_tree: None,
        };
        bundle.validate()?;
        Ok(bundle)
    }
    pub fn validate(&self) -> Result<(), ActionError> {
        if let Some(tree) = &self.imported_tree {
            tree.validate()?;
            if tree.source != self.source {
                return Err(refused());
            }
        }
        if serde_json::to_vec(self).map_err(|_| refused())?.len() > MAX_BUNDLE_BYTES {
            return Err(refused());
        }
        if self.chunks.len() > 200 {
            return Err(refused());
        }
        let mut bytes = 0usize;
        let mut rows = 0usize;
        let mut ids = std::collections::BTreeSet::new();
        let mut bookmarks = std::collections::BTreeSet::new();
        let mut history = std::collections::BTreeSet::new();
        for (ordinal, chunk) in self.chunks.iter().enumerate() {
            chunk.validate()?;
            if chunk.source != chunk_binding(&self.source, ordinal, chunk) {
                return Err(refused());
            }
            if !ids.insert(chunk.source) {
                return Err(refused());
            }
            for row in &chunk.bookmarks {
                if !bookmarks.insert(&row.url) {
                    return Err(refused());
                }
                bytes = bytes
                    .saturating_add(row.url.len())
                    .saturating_add(row.title.len());
            }
            for row in &chunk.history {
                if !history.insert(&row.url) {
                    return Err(refused());
                }
                bytes = bytes
                    .saturating_add(row.url.len())
                    .saturating_add(row.title.len());
            }
            rows = rows
                .saturating_add(chunk.bookmarks.len())
                .saturating_add(chunk.history.len());
        }
        if bytes > MAX_BUNDLE_BYTES || rows > MAX_BUNDLE_ENTRIES {
            return Err(refused());
        }
        Ok(())
    }
    /// Bookmark-only projection, with chunk identities rebound to the selected data.
    #[must_use]
    pub fn bookmark_bundle(&self) -> Self {
        let mut bundle = self.clone();
        for (ordinal, chunk) in bundle.chunks.iter_mut().enumerate() {
            chunk.history.clear();
            chunk.source = chunk_binding(&bundle.source, ordinal, chunk);
        }
        bundle
    }

    /// Visits enter only the current person's private resource.
    #[must_use]
    pub fn history_bundle(&self) -> ImportBundle {
        let mut bundle = self.clone();
        bundle.imported_tree = None;
        for (ordinal, chunk) in bundle.chunks.iter_mut().enumerate() {
            chunk.bookmarks.clear();
            chunk.source = chunk_binding(&bundle.source, ordinal, chunk);
        }
        bundle
    }
}

fn chunk_binding(source: &[u8; 32], ordinal: usize, chunk: &ImportBatch) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-browser-complete-chunk-v1");
    hash.update(source);
    hash.update(&(ordinal as u64).to_le_bytes());
    hash.update(&(chunk.bookmarks.len() as u64).to_le_bytes());
    let field = |hash: &mut blake3::Hasher, text: &str| {
        hash.update(&(text.len() as u64).to_le_bytes());
        hash.update(text.as_bytes());
    };
    for row in &chunk.bookmarks {
        field(&mut hash, &row.url);
        field(&mut hash, &row.title);
    }
    hash.update(&(chunk.history.len() as u64).to_le_bytes());
    for row in &chunk.history {
        field(&mut hash, &row.url);
        field(&mut hash, &row.title);
        hash.update(&row.last_visit.to_le_bytes());
        hash.update(&row.visit_count.to_le_bytes());
    }
    *hash.finalize().as_bytes()
}

impl ImportBatch {
    /// Decode an action payload before allocating unbounded imported metadata.
    /// # Errors
    /// Refuses malformed, oversized or invalid data.
    pub fn decode(value: &str) -> Result<Self, ActionError> {
        if value.len() > MAX_IMPORT_BYTES {
            return Err(refused());
        }
        let batch: Self = serde_json::from_str(value).map_err(|_| refused())?;
        batch.validate()?;
        Ok(batch)
    }
    /// Validate canonical URLs, unique identities and bounded metadata.
    /// # Errors
    /// Refuses invalid or oversized import batches.
    pub fn validate(&self) -> Result<(), ActionError> {
        if self.bookmarks.len() > MAX_IMPORT_ENTRIES || self.history.len() > MAX_IMPORT_ENTRIES {
            return Err(refused());
        }
        let mut bytes = 0usize;
        for rows in [
            self.bookmarks
                .iter()
                .map(|r| (&r.url, &r.title))
                .collect::<Vec<_>>(),
            self.history
                .iter()
                .map(|r| (&r.url, &r.title))
                .collect::<Vec<_>>(),
        ] {
            let mut seen = std::collections::BTreeSet::new();
            for (url, title) in rows {
                let normalized = normalized_bookmark_url(url).ok_or_else(refused)?;
                if normalized != *url || title.len() > 4096 || !seen.insert(normalized) {
                    return Err(refused());
                }
                bytes = bytes
                    .checked_add(url.len())
                    .and_then(|n| n.checked_add(title.len()))
                    .ok_or_else(refused)?;
            }
        }
        if bytes > MAX_IMPORT_BYTES || self.history.iter().any(|r| r.visit_count == 0) {
            return Err(refused());
        }
        Ok(())
    }
}
fn refused() -> ActionError {
    ActionError::Refused("Browser import is invalid or exceeds its limit".into())
}

/// Absolute, bounded, canonical HTTP(S) identity used by reviewed imports.
pub fn normalized_bookmark_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.len() > 8 * 1024 {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.into())
}
