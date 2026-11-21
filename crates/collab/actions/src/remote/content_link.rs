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
    /// names it: the whole file, or the manifest of its chunks.
    pub hash: String,
}

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

/// The media type a browser shows `name` as in a tab of its own, when it is
/// one it plays or shows safely: films, sound, pictures and plain text.
/// Nothing executable (HTML, SVG, scripts) is ever shown from a link, nor a
/// PDF, which a browser will not show sandboxed; those are saved.
#[must_use]
pub fn viewable(name: &str) -> Option<&'static str> {
    let (_, extension) = name.rsplit_once('.')?;
    Some(match extension.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {

    use super::*;

    fn file(path: &str) -> ContentLinkFile {
        ContentLinkFile {
            path: path.into(),
            hash: "ab".repeat(32),
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
            }],
            ..folder
        };
        assert!(not_a_hash.check().is_err());
    }
}
