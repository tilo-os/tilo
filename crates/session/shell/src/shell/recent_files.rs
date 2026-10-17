//! Documents opened through Tilo, published to `recently-used.xbel`.
//!
//! GTK's and Qt's file choosers, and every legacy app's "Recent" menu, read
//! `$XDG_DATA_HOME/recently-used.xbel` — the freedesktop desktop-bookmark
//! format. A file the person opened from the file manager or search is added
//! there, so a host application's Open dialog offers it.
//!
//! The file is shared with those applications: their bookmarks are kept
//! verbatim, a document already listed is replaced by Tilo's newer entry, and
//! the whole list is bounded to the most recently modified
//! [`MAX_BOOKMARKS`]. It is rewritten atomically, owner-only.
//!
//! Only what is safe to show another program is published: an existing
//! regular file inside the home directory, with no hidden component on the
//! way — `~/.ssh`, `~/.gnupg`, `~/.password-store` and every dot-directory
//! of application secrets are never named.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use tilo_lock::MutexRecover as _;

/// Bookmarks kept in the file, Tilo's and other applications' together.
pub const MAX_BOOKMARKS: usize = 500;

/// Largest existing file read back for merging; anything bigger is not a
/// bookmark list worth preserving and is replaced.
const MAX_EXISTING_BYTES: u64 = 8 * 1024 * 1024;

const APPLICATION: &str = "Tilo";

/// One bookmark as found in, or written to, the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub href: String,
    pub modified: String,
    /// The `<bookmark>` element exactly as it will be written.
    pub xml: String,
}

/// Whether `path` may be published: an existing regular file under `home`
/// with no hidden component below it.
pub fn is_publishable(path: &Path, home: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(home) else {
        return false;
    };
    let visible = relative.components().all(|component| match component {
        Component::Normal(name) => !name.to_string_lossy().starts_with('.'),
        _ => false,
    });
    visible
        && relative.components().next().is_some()
        && std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

/// `file://` URI for an absolute path.
pub fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map_or_else(|()| path.to_string_lossy().into_owned(), String::from)
}

/// The value of attribute `name` in the start tag at the head of `element`.
fn attribute(element: &str, name: &str) -> Option<String> {
    let tag_end = element.find('>')?;
    let tag = element.get(..tag_end)?;
    let needle = format!(" {name}=");
    let start = tag.find(&needle)?.checked_add(needle.len())?;
    let rest = tag.get(start..)?;
    let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'')?;
    let body = rest.get(1..)?;
    let end = body.find(quote)?;
    let raw = body.get(..end)?;
    Some(
        quick_xml::escape::unescape(raw)
            .map_or_else(|_| raw.to_owned(), std::borrow::Cow::into_owned),
    )
}

/// Every `<bookmark>` element of an XBEL document, in order. Malformed
/// trailing content is dropped rather than guessed at.
pub fn parse(document: &str) -> Vec<Bookmark> {
    let mut out = Vec::new();
    let mut rest = document;
    while let Some(start) = rest.find("<bookmark ") {
        let Some(tail) = rest.get(start..) else {
            break;
        };
        let Some(close) = tail.find("</bookmark>") else {
            break;
        };
        let end = close.saturating_add("</bookmark>".len());
        let Some(xml) = tail.get(..end) else {
            break;
        };
        if let Some(href) = attribute(xml, "href") {
            let modified = attribute(xml, "modified").unwrap_or_default();
            out.push(Bookmark {
                href,
                modified,
                xml: xml.to_string(),
            });
        }
        rest = tail.get(end..).unwrap_or_default();
    }
    out
}

/// Tilo's bookmark for `path`, opened at `now` (RFC 3339, UTC).
pub fn bookmark(path: &Path, mime: &str, now: &str) -> Bookmark {
    let href = file_uri(path);
    let (h, t, m) = (
        quick_xml::escape::escape(&href),
        quick_xml::escape::escape(now),
        quick_xml::escape::escape(mime),
    );
    let xml = format!(
        "<bookmark href=\"{h}\" added=\"{t}\" modified=\"{t}\" visited=\"{t}\">\n    \
         <info>\n      <metadata owner=\"http://freedesktop.org\">\n        \
         <mime:mime-type type=\"{m}\"/>\n        <bookmark:applications>\n          \
         <bookmark:application name=\"{APPLICATION}\" exec=\"&apos;xdg-open&apos; %u\" \
         modified=\"{t}\" count=\"1\"/>\n        </bookmark:applications>\n      \
         </metadata>\n    </info>\n  </bookmark>"
    );
    Bookmark {
        href,
        modified: now.to_string(),
        xml,
    }
}

/// `existing` with `added` in front, any older entry for the same document
/// dropped, bounded to the [`MAX_BOOKMARKS`] most recently modified.
pub fn merge(existing: Vec<Bookmark>, added: Bookmark) -> Vec<Bookmark> {
    let mut all: Vec<Bookmark> = existing
        .into_iter()
        .filter(|bookmark| bookmark.href != added.href)
        .collect();
    all.push(added);
    // RFC 3339 UTC timestamps order as strings; newest first.
    all.sort_by(|a, b| b.modified.cmp(&a.modified));
    all.truncate(MAX_BOOKMARKS);
    all
}

/// The XBEL document for `bookmarks`.
pub fn render(bookmarks: &[Bookmark]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xbel version=\"1.0\"\n      \
         xmlns:bookmark=\"http://www.freedesktop.org/standards/desktop-bookmarks\"\n      \
         xmlns:mime=\"http://www.freedesktop.org/standards/shared-mime-info\"\n>\n",
    );
    for bookmark in bookmarks {
        out.push_str("  ");
        out.push_str(&bookmark.xml);
        out.push('\n');
    }
    out.push_str("</xbel>\n");
    out
}

/// The type a file chooser filters `path` by.
pub fn mime_type(path: &Path) -> String {
    mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// Add `path` to the bookmark file `xbel`, stamped `now`. Does nothing for a
/// path [`is_publishable`] refuses.
///
/// # Errors
///
/// Returns the I/O error that prevented the file from being replaced.
pub fn record(xbel: &Path, home: &Path, path: &Path, now: &str) -> std::io::Result<bool> {
    if !is_publishable(path, home) {
        return Ok(false);
    }
    let existing = std::fs::metadata(xbel)
        .ok()
        .filter(|meta| meta.len() <= MAX_EXISTING_BYTES)
        .and_then(|_| std::fs::read_to_string(xbel).ok())
        .map(|text| parse(&text))
        .unwrap_or_default();
    let merged = merge(existing, bookmark(path, &mime_type(path), now));
    if let Some(parent) = xbel.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tilo_files::native::atomic::write_with_mode(xbel, render(&merged).as_bytes(), 0o600)?;
    Ok(true)
}

/// Where the session's bookmark file lives.
pub fn xbel_path() -> Option<PathBuf> {
    tilo_paths::xdg::data_home().map(|dir| dir.join("recently-used.xbel"))
}

/// Publish a document the person just opened. Written off the compositor
/// thread; concurrent opens are serialised so none overwrites another.
pub fn note_opened(path: &Path) {
    let (Some(xbel), Some(home)) = (xbel_path(), tilo_paths::home()) else {
        return;
    };
    let Ok(path) = path.canonicalize() else {
        return;
    };
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6fZ")
        .to_string();
    let spawned = std::thread::Builder::new()
        .name("recent-files".into())
        .spawn(move || {
            static WRITER: Mutex<()> = Mutex::new(());
            let _serial = WRITER.lock_recover();
            if let Err(error) = record(&xbel, &home, &path, &now) {
                tracing::warn!(%error, "could not update recently-used.xbel");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start the recent-files writer");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GTK_ENTRY: &str = "<bookmark href=\"file:///home/u/Notes/plan.txt\" \
        added=\"2026-01-01T00:00:00.000000Z\" modified=\"2026-01-01T00:00:00.000000Z\" \
        visited=\"2026-01-01T00:00:00.000000Z\">\n    <info>\n      <metadata \
        owner=\"http://freedesktop.org\">\n        <mime:mime-type type=\"text/plain\"/>\n      \
        </metadata>\n    </info>\n  </bookmark>";

    fn home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Documents")).unwrap();
        std::fs::write(dir.path().join("Documents/report 1&2.pdf"), b"%PDF").unwrap();
        std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
        std::fs::write(dir.path().join(".ssh/id_ed25519"), b"secret").unwrap();
        std::fs::write(dir.path().join("Documents/.hidden.txt"), b"x").unwrap();
        dir
    }

    #[test]
    fn a_recorded_document_round_trips_through_the_file_with_other_apps_entries_kept() {
        let home = home();
        let xbel = home.path().join(".local/share/recently-used.xbel");
        std::fs::create_dir_all(xbel.parent().unwrap()).unwrap();
        std::fs::write(&xbel, render(&parse(GTK_ENTRY))).unwrap();
        let doc = home.path().join("Documents/report 1&2.pdf");

        assert!(record(&xbel, home.path(), &doc, "2026-09-26T10:00:00.000000Z").unwrap());
        let text = std::fs::read_to_string(&xbel).unwrap();
        assert!(
            text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xbel version=\"1.0\"")
        );
        assert!(
            text.contains("xmlns:mime=\"http://www.freedesktop.org/standards/shared-mime-info\"")
        );
        assert!(text.trim_end().ends_with("</xbel>"));
        let back = parse(&text);
        assert_eq!(back.len(), 2, "{text}");
        let ours = &back[0];
        assert_eq!(ours.href, file_uri(&doc));
        assert!(
            ours.href.ends_with("/Documents/report%201&2.pdf"),
            "{}",
            ours.href
        );
        assert!(
            ours.xml
                .contains("<mime:mime-type type=\"application/pdf\"/>")
        );
        assert!(ours.xml.contains("<bookmark:application name=\"Tilo\""));
        assert_eq!(
            back[1].xml, GTK_ENTRY,
            "another app's bookmark is kept verbatim"
        );

        // Opening it again replaces rather than duplicates.
        assert!(record(&xbel, home.path(), &doc, "2026-09-27T10:00:00.000000Z").unwrap());
        let again = parse(&std::fs::read_to_string(&xbel).unwrap());
        assert_eq!(again.len(), 2);
        assert_eq!(again[0].modified, "2026-09-27T10:00:00.000000Z");

        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&xbel).unwrap().permissions(),
        ) & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn secrets_hidden_files_and_paths_outside_home_are_never_published() {
        let home = home();
        let xbel = home.path().join("recently-used.xbel");
        for refused in [
            home.path().join(".ssh/id_ed25519"),
            home.path().join("Documents/.hidden.txt"),
            home.path().join("Documents"),
            home.path().join("Documents/missing.txt"),
            PathBuf::from("/etc/hostname"),
        ] {
            assert!(
                !record(&xbel, home.path(), &refused, "2026-09-26T10:00:00Z").unwrap(),
                "{refused:?}"
            );
        }
        assert!(!xbel.exists(), "nothing publishable, nothing written");
    }

    #[test]
    fn the_list_is_bounded_to_the_most_recent() {
        let existing: Vec<Bookmark> = (0..MAX_BOOKMARKS)
            .map(|i| {
                bookmark(
                    Path::new(&format!("/home/u/f{i}")),
                    "text/plain",
                    &format!("2026-01-01T00:00:{:02}.{i:06}Z", i % 60),
                )
            })
            .collect();
        let newest = bookmark(
            Path::new("/home/u/new"),
            "text/plain",
            "2026-09-26T00:00:00Z",
        );
        let merged = merge(existing, newest.clone());
        assert_eq!(merged.len(), MAX_BOOKMARKS);
        assert_eq!(merged[0], newest);
    }

    #[test]
    fn attributes_are_escaped_and_read_back() {
        let b = bookmark(Path::new("/home/u/a\"b<c>.txt"), "text/plain", "t");
        assert!(!b.xml.contains("a\"b"));
        assert_eq!(parse(&render(std::slice::from_ref(&b)))[0].href, b.href);
    }
}
