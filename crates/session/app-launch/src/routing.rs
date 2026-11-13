//! Which app opens a document or a link.
//!
//! Opening a file or a link is the desktop's oldest routing question, and the answer is the
//! person's: the app their workspace's rules or their own defaults name for the type, else a
//! Tilo app that claims it — the one that runs confined and collaborative — else the machine's
//! own default (`mimeapps.list`), else nothing, and the person is asked. The layers are
//! [`desktop_policy::DefaultAppResolver`]'s; this module only turns a path into the kinds of file
//! it is and the answer into a launch, so every way a person opens a file asks the one question.
//!
//! A document is known by the type its name or header declares (`text/plain` for a `.txt`) and by
//! its extension, which is the narrower claim: a format a Tilo app made often has no type on the
//! host, and a choice about `.txt` is more specific than one about `text/plain`. A link is known
//! by its scheme (`https`), which `mimeapps.list` spells `x-scheme-handler/https`.
//!
//! A record is not matched at all: a launch that names a record already names the app that holds
//! it, and the only question left is how to ask that app for the record ([`record`]).

use std::path::Path;

use collaboration::record::RecordId;
use desktop_policy::{
    AppCandidate, DefaultAppResolver, MimeAppsChain, PersonalDefaults, ResourceKind,
    WorkspaceDefaults,
};

use crate::launch::{Launch, Open};

/// What decides which app opens a file here, besides the apps that claim it: the person at this
/// device, their own defaults, their workspace's, and the machine's `mimeapps.list`.
pub struct Defaults<'a> {
    /// The person's id, as their workspaces name them.
    pub person: &'a str,
    /// The person's defaults, which follow them to every device.
    pub personal: &'a PersonalDefaults,
    /// The rules of the workspace the open belongs to, when it belongs to one.
    pub workspace: Option<&'a WorkspaceDefaults>,
    /// The machine's own default applications, in the order they are consulted.
    pub host: Option<&'a MimeAppsChain>,
}

/// The app that opens the document at `path`; `None` when nothing here claims it and the person
/// is to be asked.
#[must_use]
pub fn document(
    path: &Path,
    candidates: &[AppCandidate],
    defaults: &Defaults<'_>,
) -> Option<Launch> {
    documents(path, candidates, defaults).into_iter().next()
}

/// Every app that opens the document at `path`, the one it opens with today first: what a person
/// choosing an app is offered, and what opening it starts.
#[must_use]
pub fn documents(path: &Path, candidates: &[AppCandidate], defaults: &Defaults<'_>) -> Vec<Launch> {
    let kinds = document_kinds(path);
    if kinds.is_empty() {
        return Vec::new();
    }
    let resolver = resolver(candidates, defaults);
    let open = Some(Open::File(path.to_path_buf()));
    let chosen = resolver.resolve_document(
        &kinds,
        defaults.person,
        defaults.personal,
        defaults.workspace,
    );
    let mut claimed = resolver.claimants(&kinds);
    if let Some(selected) = &chosen
        && let Some(chosen) = candidates
            .iter()
            .find(|app| app.package_id == selected.package_id)
    {
        // The app a person chose leads the list even when it claims none of the kinds: it is
        // what opening the file starts.
        claimed.retain(|app| app.package_id != chosen.package_id);
        claimed.insert(0, chosen);
    }
    claimed
        .into_iter()
        .map(|app| launch(app, open.clone()))
        .collect()
}

/// The app that opens `uri`; `None` when no app here claims its scheme.
#[must_use]
pub fn link(uri: &str, candidates: &[AppCandidate], defaults: &Defaults<'_>) -> Option<Launch> {
    let scheme = scheme_of(uri)?;
    let kind = ResourceKind::scheme(&scheme).ok()?;
    let resolver = resolver(candidates, defaults);
    let chosen = resolver.resolve_document(
        std::slice::from_ref(&kind),
        defaults.person,
        defaults.personal,
        defaults.workspace,
    )?;
    let app = candidates
        .iter()
        .find(|app| app.package_id == chosen.package_id)?;
    Some(launch(app, Some(Open::Uri(uri.to_owned()))))
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

/// The kinds of file `path` is, the narrowest first: its extension, then the type its name
/// declares.
fn document_kinds(path: &Path) -> Vec<ResourceKind> {
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|extension| ResourceKind::extension(extension).ok());
    let mime = tilo_mime::from_path(path).and_then(|mime| ResourceKind::mime(mime).ok());
    extension.into_iter().chain(mime).collect()
}

/// The resolver that decides among the apps this device has, with the machine's own lists when
/// the caller read them.
fn resolver<'a>(candidates: &'a [AppCandidate], defaults: &Defaults<'a>) -> DefaultAppResolver<'a> {
    let resolver = DefaultAppResolver::new(candidates);
    match defaults.host {
        Some(chain) => resolver.with_system_defaults(chain),
        None => resolver,
    }
}

/// `app` opening `open` : one of the machine's own applications when it is one, else a package.
fn launch(app: &AppCandidate, open: Option<Open>) -> Launch {
    match app.host_desktop_id() {
        Some(desktop_id) => Launch::Host {
            desktop_id: desktop_id.to_owned(),
            open: None,
        }
        .opening(open),
        None => Launch::Package {
            app: app.package_id.clone(),
            open: None,
        }
        .opening(open),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_policy::{MimeAppsList, WorkspaceDefaultPolicy};

    /// An installed app whose signed manifest declares what it opens.
    fn app(id: &str, extensions: &[&str], mimes: &[&str], schemes: &[&str]) -> AppCandidate {
        let list = |claims: &[&str]| {
            claims
                .iter()
                .map(|claim| (*claim).to_owned())
                .collect::<Vec<_>>()
        };
        AppCandidate::package(
            id.to_owned(),
            id.to_owned(),
            &list(extensions),
            &list(mimes),
            &list(schemes),
            false,
        )
    }

    /// One of the machine's own apps, claiming the types its entry names.
    fn host_entry(desktop_id: &str, name: &str, mimes: &[&str]) -> AppCandidate {
        let list = |claims: &[&str]| {
            claims
                .iter()
                .map(|claim| (*claim).to_owned())
                .collect::<Vec<_>>()
        };
        AppCandidate::host(desktop_id, name.to_owned(), &list(mimes))
            .expect("an entry that claims a type")
    }

    fn nothing() -> (PersonalDefaults, WorkspaceDefaults, MimeAppsChain) {
        (
            PersonalDefaults::default(),
            WorkspaceDefaults::default(),
            MimeAppsChain::default(),
        )
    }

    #[test]
    fn a_document_goes_to_the_installed_app_that_claims_its_type_before_the_hosts() {
        let host = host_entry(
            "org.example.Viewer",
            "Viewer",
            &["text/plain", "application/pdf"],
        );
        let notes = app("so.tilo.notes", &["txt", "note"], &["text/plain"], &[]);
        let (personal, workspace, chain) = nothing();
        let defaults = Defaults {
            person: "ada",
            personal: &personal,
            workspace: Some(&workspace),
            host: Some(&chain),
        };
        assert_eq!(
            document(
                Path::new("/home/you/todo.txt"),
                &[host, notes.clone()],
                &defaults
            ),
            Some(Launch::Package {
                app: "so.tilo.notes".into(),
                open: Some(Open::File("/home/you/todo.txt".into())),
            }),
            "a Tilo app runs confined, so it is asked first"
        );
        assert_eq!(
            document(Path::new("/home/you/report.odt"), &[notes], &defaults),
            None,
            "nobody claims it, so the person is asked"
        );
        let viewer = host_entry("org.example.Viewer", "Viewer", &["application/pdf"]);
        assert_eq!(
            document(Path::new("/home/you/paper.pdf"), &[viewer], &defaults),
            Some(Launch::Host {
                desktop_id: "org.example.Viewer".into(),
                open: Some(Open::File("/home/you/paper.pdf".into())),
            })
        );
    }

    #[test]
    fn every_claiming_app_is_listed_with_the_one_opening_it_today_first() {
        let host = host_entry("org.example.Viewer", "Viewer", &["text/plain"]);
        let notes = app("so.tilo.notes", &["txt"], &["text/plain"], &[]);
        // Claims the extension only, and the same type as Notes: listed once.
        let writer = app("so.tilo.word-processor", &["txt"], &[], &[]);
        let (personal, workspace, chain) = nothing();
        let defaults = Defaults {
            person: "ada",
            personal: &personal,
            workspace: Some(&workspace),
            host: Some(&chain),
        };
        let listed = documents(
            Path::new("/home/you/todo.txt"),
            &[writer, notes.clone(), host],
            &defaults,
        );
        assert_eq!(
            listed,
            vec![
                Launch::Package {
                    app: "so.tilo.notes".into(),
                    open: Some(Open::File("/home/you/todo.txt".into())),
                },
                Launch::Package {
                    app: "so.tilo.word-processor".into(),
                    open: Some(Open::File("/home/you/todo.txt".into())),
                },
                Launch::Host {
                    desktop_id: "org.example.Viewer".into(),
                    open: Some(Open::File("/home/you/todo.txt".into())),
                },
            ],
            "the type's claims before the extension's, the host's last"
        );
        assert_eq!(
            document(Path::new("/home/you/todo.txt"), &[notes], &defaults),
            Some(listed[0].clone()),
            "opening a document uses the same order"
        );
    }

    #[test]
    fn the_workspaces_default_app_for_the_type_opens_it_and_leads_the_list() {
        let notes = app("so.tilo.notes", &["txt"], &["text/plain"], &[]);
        let writer = app("so.tilo.word-processor", &["txt"], &[], &[]);
        let host = host_entry("org.example.TextEditor", "Text Editor", &["text/plain"]);
        let (personal, mut workspace, chain) = nothing();
        workspace.policy.insert(
            ResourceKind::extension("txt").unwrap(),
            WorkspaceDefaultPolicy::App("so.tilo.word-processor".into()),
        );
        let defaults = Defaults {
            person: "ada",
            personal: &personal,
            workspace: Some(&workspace),
            host: Some(&chain),
        };
        let candidates = [host, notes, writer];
        let wanted = Launch::Package {
            app: "so.tilo.word-processor".into(),
            open: Some(Open::File("/home/you/Budget.txt".into())),
        };
        assert_eq!(
            documents(Path::new("/home/you/Budget.txt"), &candidates, &defaults).first(),
            Some(&wanted),
            "the workspace said which app opens `.txt` files, and it opens them"
        );
        assert_eq!(
            document(Path::new("/home/you/Budget.txt"), &candidates, &defaults),
            Some(wanted)
        );
    }

    #[test]
    fn the_machines_own_default_opens_a_type_no_tilo_app_claims() {
        let viewer = host_entry("org.example.Viewer", "Viewer", &["application/pdf"]);
        let other = host_entry("org.example.Other", "Aardvark", &["application/pdf"]);
        let (personal, workspace, _) = nothing();
        let chain = MimeAppsChain::new(vec![MimeAppsList::parse(
            "[Default Applications]\napplication/pdf=gone.desktop;org.example.Viewer.desktop;\n",
        )]);
        let defaults = Defaults {
            person: "ada",
            personal: &personal,
            workspace: Some(&workspace),
            host: Some(&chain),
        };
        assert_eq!(
            document(
                Path::new("/home/you/paper.pdf"),
                &[other, viewer],
                &defaults
            ),
            Some(Launch::Host {
                desktop_id: "org.example.Viewer".into(),
                open: Some(Open::File("/home/you/paper.pdf".into())),
            }),
            "the machine's own default decides between its apps, not their names"
        );
    }

    #[test]
    fn a_choice_of_a_tilo_app_for_the_mime_type_does_not_beat_one_for_the_extension() {
        let notes = app("so.tilo.notes", &["txt"], &["text/plain"], &[]);
        let writer = app("so.tilo.word-processor", &["txt"], &["text/plain"], &[]);
        let mut personal = PersonalDefaults::default();
        personal.handlers.insert(
            ResourceKind::mime("text/plain").unwrap(),
            "so.tilo.notes".into(),
        );
        personal.handlers.insert(
            ResourceKind::extension("txt").unwrap(),
            "so.tilo.word-processor".into(),
        );
        let workspace = WorkspaceDefaults::default();
        let chain = MimeAppsChain::default();
        let defaults = Defaults {
            person: "ada",
            personal: &personal,
            workspace: Some(&workspace),
            host: Some(&chain),
        };
        assert_eq!(
            document(Path::new("/home/you/todo.txt"), &[notes, writer], &defaults),
            Some(Launch::Package {
                app: "so.tilo.word-processor".into(),
                open: Some(Open::File("/home/you/todo.txt".into())),
            }),
            "a choice about `.txt` is narrower than one about `text/plain`"
        );
    }

    #[test]
    fn a_tilo_apps_own_extension_is_a_claim_of_its_own() {
        let (personal, workspace, chain) = nothing();
        let defaults = Defaults {
            person: "ada",
            personal: &personal,
            workspace: Some(&workspace),
            host: Some(&chain),
        };
        let sheets = app("so.tilo.sheets", &["tilosheet"], &[], &[]);
        // A format no host knows: only the app that made it claims it.
        assert_eq!(
            document(
                Path::new("/home/you/budget.tilosheet"),
                &[sheets],
                &defaults
            ),
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
        let (personal, workspace, _) = nothing();
        let chain = MimeAppsChain::new(vec![MimeAppsList::parse(
            "[Default Applications]\nx-scheme-handler/https=org.example.Viewer.desktop;\n",
        )]);
        let defaults = Defaults {
            person: "ada",
            personal: &personal,
            workspace: Some(&workspace),
            host: Some(&chain),
        };
        let browser = host_entry(
            "org.example.Viewer",
            "Browser",
            &["x-scheme-handler/https", "x-scheme-handler/http"],
        );
        assert_eq!(
            link(
                "https://code.tilo.so",
                std::slice::from_ref(&browser),
                &defaults
            ),
            Some(Launch::Host {
                desktop_id: "org.example.Viewer".into(),
                open: Some(Open::Uri("https://code.tilo.so".into())),
            })
        );
        let tilo_browser = app("so.tilo.browser", &[], &[], &["https"]);
        assert_eq!(
            link(
                "https://code.tilo.so",
                &[browser, tilo_browser],
                &Defaults {
                    person: "ada",
                    personal: &personal,
                    workspace: Some(&workspace),
                    host: None,
                }
            ),
            Some(Launch::Package {
                app: "so.tilo.browser".into(),
                open: Some(Open::Uri("https://code.tilo.so".into())),
            }),
            "a Tilo app that claims the scheme comes first"
        );
        assert_eq!(link("mailto:person@example.com", &[], &defaults), None);
        assert_eq!(
            link("/home/you/a:b", &[], &defaults),
            None,
            "an absolute path is not a link"
        );
    }

    #[test]
    fn a_link_follows_the_persons_choice_of_app_for_its_scheme() {
        let browser = host_entry(
            "org.example.Browser",
            "Browser",
            &["x-scheme-handler/https"],
        );
        let mut personal = PersonalDefaults::default();
        personal.handlers.insert(
            ResourceKind::scheme("https").unwrap(),
            "org.example.Browser.desktop".into(),
        );
        let workspace = WorkspaceDefaults::default();
        let chain = MimeAppsChain::default();
        let defaults = Defaults {
            person: "ada",
            personal: &personal,
            workspace: Some(&workspace),
            host: Some(&chain),
        };
        assert_eq!(
            link("https://code.tilo.so", &[browser], &defaults),
            Some(Launch::Host {
                desktop_id: "org.example.Browser".into(),
                open: Some(Open::Uri("https://code.tilo.so".into())),
            })
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

    #[test]
    fn a_document_is_known_by_its_extension_before_the_type_it_names() {
        assert_eq!(
            document_kinds(Path::new("/home/you/Budget.TXT")),
            vec![
                ResourceKind::extension("txt").unwrap(),
                ResourceKind::mime("text/plain").unwrap(),
            ]
        );
        assert!(document_kinds(Path::new("/home/you/Makefile")).is_empty());
    }
}
