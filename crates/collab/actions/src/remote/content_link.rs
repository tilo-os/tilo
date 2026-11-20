//! Content links: an address a browser fetches a workspace's files from
//! itself, over plain HTTPS, rather than through the page.
//!
//! A page asks for one over its session ([`super::RemoteActionMessage::ContentLinkRequest`])
//! naming the files and the name they are saved under; the provider answers with
//! a path on its own web origin, good for a few minutes, where it serves
//! those bytes straight from its store. Two things need that:
//!
//! - dragging a file out of the page onto the computer's desktop or file
//!   manager: the browser fetches the address the drag names with its own
//!   download manager, outside the page, so the bytes must be at an address
//!   the network answers;
//! - saving a file of any size: the page streams the response into the file
//!   the person chose, or lets the browser download it, so a gigabyte never
//!   sits in the page's memory.
//!
//! Several files, or a folder, are served as one zip of them.
//!
//! The link is a capability: whoever holds the address reads what it names
//! until it expires. A provider mints one only for a caller who may read the
//! workspace, names only content of that workspace, and lets it go after
//! [`CONTENT_LINK_TTL_SECS`].

/// What a provider that serves no content links answers.
pub const CONTENT_LINKS_UNAVAILABLE: &str = "links to content are not available here";

/// How long a content link stays good, in seconds: long enough for a drag
/// that hovers, or a Save dialog someone thinks about, and short enough that
/// an address left in a browser's download list no longer reads anything.
pub const CONTENT_LINK_TTL_SECS: u64 = 15 * 60;

/// The most files one link names: a folder past this is saved in parts.
pub const MAX_CONTENT_LINK_FILES: usize = 65_535;

/// One file a link serves: where it goes in what is saved, and its content.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentLinkFile {
    /// Its path in a zip (`Plans/2026/budget.csv`, `/`-separated); for a link
    /// to one file, its name.
    pub path: String,
    /// The hash of its content in the workspace's store, as a Files node
    /// names it: the whole file, or the manifest of its chunks. For a file
    /// kept [`encoded`](Self::encoded), its first piece's.
    pub hash: String,
    /// The file is not one content but a body as a message carried it:
    /// pieces in a transfer encoding, which the provider decodes as it
    /// serves them. So an attachment saves straight from the message it came
    /// in, without being decoded and kept a second time first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoded: Option<EncodedFile>,
}

/// A file kept as a message's body: its pieces and how they decode.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncodedFile {
    /// The references the store keeps the body's pieces under, in order
    /// (sealed ones with their key).
    pub pieces: Vec<String>,
    /// The transfer encoding they are in: `base64`, `quoted-printable` or
    /// `binary`.
    pub encoding: String,
    /// The size of the file once decoded.
    pub size: u64,
}

/// The transfer encodings an [`EncodedFile`] may name.
pub const TRANSFER_ENCODINGS: [&str; 3] = ["base64", "quoted-printable", "binary"];

/// Whether `hash` is a content hash a link may name: 64 hex digits.
fn is_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What a link is asked for.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentLinkAsk {
    /// The name the browser saves it under: the file's own, or the folder's
    /// with `.zip`.
    pub name: String,
    /// The files it serves. One file with `zip` false is served as itself.
    pub files: Vec<ContentLinkFile>,
    /// Serve the files as one zip, even a single one: a folder.
    pub zip: bool,
}

impl ContentLinkAsk {
    /// Why this ask is not one a provider mints a link for, if it is not.
    ///
    /// # Errors
    ///
    /// No name, no files, more than one file not zipped, too many files, or
    /// a path a zip may not hold.
    pub fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.contains(['/', '\\', '\0']) {
            return Err(format!("{:?} is no name to save under", self.name));
        }
        if !self.zip && self.files.len() != 1 {
            return Err("a link to more than one file is a zip".into());
        }
        if self.files.len() > MAX_CONTENT_LINK_FILES {
            return Err(format!(
                "a link names at most {MAX_CONTENT_LINK_FILES} files"
            ));
        }
        for file in &self.files {
            let safe = !file.path.is_empty()
                && !file.path.starts_with('/')
                && !file.path.contains(['\\', '\0'])
                && file
                    .path
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..");
            if !safe {
                return Err(format!("{:?} is no path in a zip", file.path));
            }
            if !is_hash(&file.hash) {
                return Err(format!("{:?} is no content hash", file.hash));
            }
            if let Some(encoded) = &file.encoded {
                let first = encoded.pieces.first().map(String::as_str);
                if first.and_then(tilo_content::content_address).as_deref()
                    != Some(file.hash.as_str())
                {
                    return Err(format!("{:?} names its first piece as its hash", file.path));
                }
                if let Some(bad) = encoded
                    .pieces
                    .iter()
                    .find(|piece| !tilo_content::valid_reference(piece))
                {
                    return Err(format!("{bad:?} is no content reference"));
                }
                if !TRANSFER_ENCODINGS.contains(&encoded.encoding.as_str()) {
                    return Err(format!("{:?} is no transfer encoding", encoded.encoding));
                }
            }
        }
        Ok(())
    }
}

/// The link a provider minted.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentLink {
    /// Its path on the provider's web origin (`/api/content/<token>/<name>`).
    pub path: String,
    /// How many seconds from the answer it stays good.
    pub expires_in_secs: u64,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    fn file(path: &str) -> ContentLinkFile {
        ContentLinkFile {
            path: path.into(),
            hash: "ab".repeat(32),
            encoded: None,
        }
    }

    #[test]
    fn a_link_names_a_file_or_a_zip_of_safe_paths() {
        let one = ContentLinkAsk {
            name: "plan.md".into(),
            files: vec![file("plan.md")],
            zip: false,
        };
        assert!(one.check().is_ok());
        let folder = ContentLinkAsk {
            name: "Plans.zip".into(),
            files: vec![file("Plans/a.md"), file("Plans/2026/b.csv")],
            zip: true,
        };
        assert!(folder.check().is_ok());
        let empty_folder = ContentLinkAsk {
            files: Vec::new(),
            ..folder.clone()
        };
        assert!(
            empty_folder.check().is_ok(),
            "an empty folder is an empty zip"
        );

        let two_unzipped = ContentLinkAsk {
            zip: false,
            ..folder.clone()
        };
        assert!(two_unzipped.check().is_err());
        for path in ["../etc/passwd", "/abs", "a//b", "a/./b", "a\\b", ""] {
            let bad = ContentLinkAsk {
                files: vec![file(path)],
                ..folder.clone()
            };
            assert!(bad.check().is_err(), "{path:?}");
        }
        let unnamed = ContentLinkAsk {
            name: "a/b.zip".into(),
            ..folder.clone()
        };
        assert!(unnamed.check().is_err());
        let not_a_hash = ContentLinkAsk {
            files: vec![ContentLinkFile {
                path: "a".into(),
                hash: "tilo.content:xyz".into(),
                encoded: None,
            }],
            ..folder
        };
        assert!(not_a_hash.check().is_err());

        let encoded = |pieces: Vec<String>, encoding: &str| ContentLinkAsk {
            name: "scan.pdf".into(),
            files: vec![ContentLinkFile {
                path: "scan.pdf".into(),
                hash: "ab".repeat(32),
                encoded: Some(EncodedFile {
                    pieces,
                    encoding: encoding.into(),
                    size: 10,
                }),
            }],
            zip: false,
        };
        let pieces = vec!["ab".repeat(32), "cd".repeat(32)];
        assert!(encoded(pieces.clone(), "base64").check().is_ok());
        assert!(
            encoded(pieces, "uuencode").check().is_err(),
            "an encoding no provider decodes"
        );
        assert!(
            encoded(vec!["cd".repeat(32)], "base64").check().is_err(),
            "its hash is its first piece"
        );
        assert!(
            encoded(vec!["ab".repeat(32), "piece".into()], "binary")
                .check()
                .is_err()
        );
    }
}
