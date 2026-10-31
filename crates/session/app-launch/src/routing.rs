//! Which app opens a document or a link.
//!
//! Opening a file or a link is the desktop's oldest routing question, and the
//! answer is the person's: an app they installed teaches the desktop what it
//! opens (its manifest's `[providers]`), and the machine's own apps say the
//! same in their `.desktop` entries (`MimeType=`). A Tilo app that claims the
//! type comes first, because it is the one that runs confined; the machine's
//! own app is the fallback, not the other way around.
//!
//! A type is matched by MIME first — the manifest's concrete claims and the
//! entry's `MimeType=`, with the type guessed from the path's extension — and
//! by the manifest's extensions as well, since a Tilo app's own formats often
//! have no MIME type registered on the host. A link is matched by its scheme
//! (`x-scheme-handler/https`), and a Tilo app claims one by name.
//!
//! A record is not matched at all: a launch that names a record already names
//! the app that holds it, and the only question left is how to ask that app
//! for the record ([`record`]).

use std::path::Path;

use collaboration::record::RecordId;
use desktop_entry::Scan;
use tilo_installed_apps::InstalledApp;

use crate::launch::{Launch, Open};

/// The app that opens the document at `path`: the first installed Tilo app
/// that claims its type or extension, else the first of the machine's own.
#[must_use]
pub fn document(path: &Path, installed: &[InstalledApp], entries: &Scan) -> Option<Launch> {
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    let mime = tilo_mime::from_path(path).map(str::to_owned);
    let open = Some(Open::File(path.to_path_buf()));

    let claims_extension = |app: &InstalledApp| {
        extension.as_deref().is_some_and(|extension| {
            app.opens_extensions
                .iter()
                .any(|claims| claims.eq_ignore_ascii_case(extension))
        })
    };
    let claims_mime = |app: &InstalledApp| mime.as_deref().is_some_and(|mime| app.opens_mime(mime));
    let app = installed
        .iter()
        .find(|app| claims_mime(app))
        .or_else(|| installed.iter().find(|app| claims_extension(app)));
    if let Some(app) = app {
        return Some(
            Launch::Package {
                app: app.package_id.clone(),
                open: None,
            }
            .opening(open),
        );
    }
    let entry = entries.entries.iter().find(|entry| {
        mime.as_deref().is_some_and(|mime| {
            entry
                .mime_types
                .iter()
                .any(|claims| claims.eq_ignore_ascii_case(mime))
        })
    })?;
    Some(
        Launch::Host {
            desktop_id: entry.id.clone(),
            open: None,
        }
        .opening(open),
    )
}

/// The app that opens `uri`: the first installed Tilo app claiming its
/// scheme, else the first of the machine's own (`x-scheme-handler/<scheme>`).
#[must_use]
pub fn link(uri: &str, installed: &[InstalledApp], entries: &Scan) -> Option<Launch> {
    let scheme = scheme_of(uri)?;
    let open = Some(Open::Uri(uri.to_owned()));
    let app = installed.iter().find(|app| {
        app.opens_uri_schemes
            .iter()
            .any(|claims| claims.eq_ignore_ascii_case(&scheme))
    });
    if let Some(app) = app {
        return Some(
            Launch::Package {
                app: app.package_id.clone(),
                open: None,
            }
            .opening(open),
        );
    }
    let handler = format!("x-scheme-handler/{scheme}");
    let entry = entries.entries.iter().find(|entry| {
        entry
            .mime_types
            .iter()
            .any(|claims| claims.eq_ignore_ascii_case(&handler))
    })?;
    Some(
        Launch::Host {
            desktop_id: entry.id.clone(),
            open: None,
        }
        .opening(open),
    )
}

/// What an app is asked to open to reach the record `record`: the record's
/// own URI ([`RecordId::uri`]), which its launcher hands the app as the
/// record to open. `None` when `record` is not a record id — the launch
/// comes from a window, so its record is checked before it reaches an app's
/// command line.
#[must_use]
pub fn record(record: &str) -> Option<Open> {
    Some(Open::Uri(RecordId::parse(record)?.uri()))
}

/// The scheme `uri` names, when it is one: a letter followed by letters,
/// digits, `+`, `-` or `.`, then a colon. Anything else is not a link this
/// desktop routes — in particular not a bare path.
#[must_use]
pub fn scheme_of(uri: &str) -> Option<String> {
    let (scheme, rest) = uri.split_once(':')?;
    let valid = scheme.starts_with(|first: char| first.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
        && !scheme.is_empty()
        && !rest.is_empty();
    valid.then(|| scheme.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn app(id: &str, extensions: &[&str], mimes: &[&str], schemes: &[&str]) -> InstalledApp {
        InstalledApp {
            package_id: id.into(),
            display_name: id.into(),
            localized_names: std::collections::BTreeMap::new(),
            keywords: Vec::new(),
            category: None,
            version: String::new(),
            manifest_path: PathBuf::new(),
            payload_root: PathBuf::new(),
            has_gui: true,
            opens_extensions: extensions.iter().map(|ext| (*ext).to_owned()).collect(),
            opens_mime_types: mimes.iter().map(|mime| (*mime).to_owned()).collect(),
            opens_uri_schemes: schemes.iter().map(|scheme| (*scheme).to_owned()).collect(),
            opens_secondary: false,
            offers: Vec::new(),
        }
    }

    /// The machine's apps, as a scan of `source` alone would list them.
    fn entries(source: &str) -> Scan {
        let directory = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("org.example.Viewer.desktop");
        std::fs::write(&path, source).expect("the entry");
        let mut scan = desktop_entry::scan_dirs(&[directory.path().to_path_buf()], &[]);
        assert_eq!(scan.entries.len(), 1, "{:?}", scan.stats);
        // The directory goes away with the test; only the parsed entry is
        // read afterwards.
        scan.dirs.clear();
        scan
    }

    #[test]
    fn a_document_goes_to_the_installed_app_that_claims_its_type_before_the_hosts() {
        let scan = entries(
            "[Desktop Entry]\nType=Application\nName=Viewer\nExec=viewer %f\n\
             MimeType=text/plain;application/pdf;\n",
        );
        let host = app("org.example.Viewer.desktop", &[], &[], &[]);
        let notes = app("so.tilo.notes", &["txt", "note"], &["text/plain"], &[]);
        assert_eq!(
            document(
                Path::new("/home/you/todo.txt"),
                &[host, notes.clone()],
                &scan
            ),
            Some(Launch::Package {
                app: "so.tilo.notes".into(),
                open: Some(Open::File("/home/you/todo.txt".into())),
            }),
            "a Tilo app runs confined, so it is asked first"
        );
        assert_eq!(
            document(Path::new("/home/you/report.odt"), &[notes], &scan),
            None,
            "nobody claims it"
        );
        assert_eq!(
            document(Path::new("/home/you/paper.pdf"), &[], &scan),
            Some(Launch::Host {
                desktop_id: "org.example.Viewer".into(),
                open: Some(Open::File("/home/you/paper.pdf".into())),
            })
        );
    }

    #[test]
    fn a_tilo_apps_own_extension_is_a_claim_of_its_own() {
        let scan = entries(
            "[Desktop Entry]\nType=Application\nName=Other\nExec=other %f\nMimeType=text/plain;\n",
        );
        let sheets = app("so.tilo.sheets", &["tilosheet"], &[], &[]);
        // A format no host knows: only the app that made it claims it.
        assert_eq!(
            document(Path::new("/home/you/budget.tilosheet"), &[sheets], &scan),
            Some(Launch::Package {
                app: "so.tilo.sheets".into(),
                open: Some(Open::File("/home/you/budget.tilosheet".into())),
            })
        );
    }

    #[test]
    fn a_record_is_asked_for_by_its_own_uri_and_nothing_else_is_a_record() {
        let note = RecordId::for_file("/home/you/Notes.note");
        assert_eq!(
            record(note.as_str()),
            Some(Open::Uri(format!("tilo-record:{}", note.as_str()))),
            "the app is asked for the record, not for the file it is kept in"
        );
        let minted = RecordId::minted();
        assert_eq!(
            record(minted.as_str()),
            Some(Open::Uri(minted.uri())),
            "a record with no file of its own opens the same way"
        );
        assert_eq!(record(""), None, "an empty record names nothing");
        assert_eq!(record("not a record"), None);
        assert_eq!(
            record("file:relative/path"),
            None,
            "a record id names an absolute path or a minted id"
        );
    }

    #[test]
    fn a_link_goes_to_the_app_that_claims_its_scheme() {
        let scan = entries(
            "[Desktop Entry]\nType=Application\nName=Browser\nExec=browser %u\n\
             MimeType=x-scheme-handler/https;x-scheme-handler/http;\n",
        );
        assert_eq!(
            link("https://code.tilo.so", &[], &scan),
            Some(Launch::Host {
                desktop_id: "org.example.Viewer".into(),
                open: Some(Open::Uri("https://code.tilo.so".into())),
            })
        );
        let browser = app("so.tilo.browser", &[], &[], &["https"]);
        assert_eq!(
            link("https://code.tilo.so", &[browser], &scan),
            Some(Launch::Package {
                app: "so.tilo.browser".into(),
                open: Some(Open::Uri("https://code.tilo.so".into())),
            })
        );
        assert_eq!(link("mailto:person@example.com", &[], &scan), None);
        assert_eq!(
            link("/home/you/a:b", &[], &scan),
            None,
            "an absolute path is not a link"
        );
    }

    #[test]
    fn only_a_real_scheme_names_a_link() {
        assert_eq!(scheme_of("https://example.com"), Some("https".into()));
        assert_eq!(scheme_of("web+tilo:thing"), Some("web+tilo".into()));
        assert_eq!(scheme_of("1http://example.com"), None);
        assert_eq!(scheme_of("ht tp://example.com"), None);
        assert_eq!(scheme_of("https:"), None);
        assert_eq!(scheme_of("no-scheme-here"), None);
    }
}
