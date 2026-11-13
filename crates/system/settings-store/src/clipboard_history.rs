//! This device's clipboard history: the copies the person can paste from again, with what they
//! pinned. A copy is one format's bytes, text as UTF-8 or one binary format. Secret copies never
//! reach this section, so nothing here decides what may be kept.
//!
//! ```json
//! "clipboard": {
//!   "history": { "copies": [{ "mime": "text/plain;charset=utf-8",
//!                             "bytes": "aGVsbG8=", "created_ms": 1761900000000,
//!                             "pinned": true, "from": null }] }
//! }
//! ```

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use crate::Key;

pub const SECTION: &str = "clipboard";

pub const MAX_COPIES: usize = 50;
pub const MAX_COPY_BYTES: usize = 256 * 1024;
pub const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredCopy {
    pub mime: String,
    /// Its bytes, base64: the document is JSON.
    pub bytes: String,
    /// When it was copied, in Unix milliseconds.
    pub created_ms: u64,
    pub pinned: bool,
    #[serde(default)]
    pub from: Option<String>,
}

impl StoredCopy {
    #[must_use]
    pub fn of(mime: &str, bytes: &[u8], created_ms: u64, pinned: bool, from: Option<&str>) -> Self {
        Self {
            mime: mime.to_owned(),
            bytes: BASE64.encode(bytes),
            created_ms,
            pinned,
            from: from.map(str::to_owned),
        }
    }

    /// The payload bytes; empty when the stored text is not base64.
    #[must_use]
    pub fn payload(&self) -> Vec<u8> {
        BASE64.decode(self.bytes.as_bytes()).unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredHistory {
    #[serde(default)]
    pub copies: Vec<StoredCopy>,
}

impl StoredHistory {
    #[must_use]
    pub fn kept(&self) -> Vec<StoredCopy> {
        let mut total = 0usize;
        let mut kept = Vec::new();
        for copy in &self.copies {
            let bytes = copy.payload();
            let size = bytes.len();
            if copy.mime.is_empty()
                || size == 0
                || size > MAX_COPY_BYTES
                || total.saturating_add(size) > MAX_HISTORY_BYTES
            {
                continue;
            }
            total = total.saturating_add(size);
            kept.push(copy.clone());
            if kept.len() == MAX_COPIES {
                break;
            }
        }
        kept
    }
}

pub const HISTORY: Key<StoredHistory> = Key::new(SECTION, "history", StoredHistory::default);

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(text: &str, pinned: bool) -> StoredCopy {
        StoredCopy::of(
            "text/plain;charset=utf-8",
            text.as_bytes(),
            1_761_900_000_000,
            pinned,
            None,
        )
    }

    #[test]
    fn a_copy_survives_the_document_and_pinned_ones_are_marked() {
        let sealed = StoredHistory {
            copies: vec![copy("hello", true), copy("bye", false)],
        };
        let document = serde_json::to_value(&sealed).expect("stored");
        let back: StoredHistory = serde_json::from_value(document).expect("read");
        assert_eq!(back, sealed);
        assert_eq!(back.kept()[0].payload(), b"hello");
        assert!(back.kept()[0].pinned);
        assert_eq!(back.kept()[1].payload(), b"bye");
    }

    #[test]
    fn what_cannot_be_kept_is_dropped_and_the_rest_stays_in_order() {
        let huge = StoredCopy::of("image/png", &vec![b'x'; MAX_COPY_BYTES + 1], 1, false, None);
        let empty = StoredCopy::of("text/plain", b"", 2, false, None);
        let nameless = StoredCopy::of("", b"x", 3, false, None);
        let not_base64 = StoredCopy {
            mime: "text/plain".into(),
            bytes: "not base64!".into(),
            created_ms: 4,
            pinned: false,
            from: None,
        };
        let history = StoredHistory {
            copies: vec![
                copy("first", true),
                huge,
                empty,
                nameless,
                not_base64,
                copy("last", false),
            ],
        };
        let kept = history.kept();
        let texts: Vec<String> = kept
            .iter()
            .map(|copy| String::from_utf8_lossy(&copy.payload()).into_owned())
            .collect();
        assert_eq!(texts, ["first", "last"]);
    }

    #[test]
    fn the_history_is_this_devices() {
        assert_eq!(HISTORY.section(), SECTION);
        assert!(HISTORY.default_value().copies.is_empty());
    }
}
