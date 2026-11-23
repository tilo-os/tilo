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

use collab_types::document::DocumentId;
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
    let scheme = tilo_actions::remote::link_scheme(uri)?;
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
/// own URI ([`DocumentId::uri`]), which its launcher hands the app as the
/// record to open. `None` when `record` is not a record id — the launch
/// comes from a window, so its record is checked before it reaches an app's
/// command line.
#[must_use]
pub fn record(record: &str) -> Option<Open> {
    Some(Open::Uri(DocumentId::parse(record)?.uri()))
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

    fn list(claims: &[&str]) -> Vec<String> {
        claims
            .iter()
            .map(|claim| (*claim).to_owned())
            .collect::<Vec<_>>()
    }

    /// An installed app whose signed manifest declares what it opens.
    fn app(id: &str, extensions: &[&str], mimes: &[&str], schemes: &[&str]) -> AppCandidate {
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
        AppCandidate::host(desktop_id, name.to_owned(), &list(mimes))
            .expect("an entry that claims a type")
    }

    /// What decides which app opens a file here: the person's defaults and their
    /// workspace's, held together so a flow may change one and ask again.
    struct Choice {
        personal: PersonalDefaults,
        workspace: WorkspaceDefaults,
        host: MimeAppsChain,
    }

    impl Choice {
        fn new() -> Self {
            Self {
                personal: PersonalDefaults::default(),
                workspace: WorkspaceDefaults::default(),
                host: MimeAppsChain::default(),
            }
        }

        fn defaults(&self) -> Defaults<'_> {
            Defaults {
                person: "ada",
                personal: &self.personal,
                workspace: Some(&self.workspace),
                host: Some(&self.host),
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one flow through opening documents and links, which is what it proves"
    )]
    fn a_document_or_a_link_opens_in_the_app_the_person_or_their_workspace_chose_for_it() {
        let notes = app("so.tilo.notes", &["txt", "note"], &["text/plain"], &[]);
        let writer = app("so.tilo.word-processor", &["txt"], &[], &[]);
        let sheets = app("so.tilo.sheets", &["tilosheet"], &[], &[]);
        let viewer = host_entry(
            "org.example.Viewer",
            "Viewer",
            &["text/plain", "application/pdf"],
        );
        let mut choice = Choice::new();

        // One person opening files: a Tilo app that claims the type runs confined, so it is
        // asked before the machine's own viewer; a kind nobody claims asks the person.
        assert_eq!(
            document(
                Path::new("/home/you/todo.txt"),
                &[viewer.clone(), notes.clone()],
                &choice.defaults()
            ),
            Some(Launch::Package {
                app: "so.tilo.notes".into(),
                open: Some(Open::File("/home/you/todo.txt".into())),
            }),
            "a Tilo app runs confined, so it is asked first"
        );
        assert_eq!(
            document(
                Path::new("/home/you/report.odt"),
                std::slice::from_ref(&notes),
                &choice.defaults()
            ),
            None,
            "nobody claims it, so the person is asked"
        );
        let pdf_viewer = host_entry("org.example.Viewer", "Viewer", &["application/pdf"]);
        assert_eq!(
            document(
                Path::new("/home/you/paper.pdf"),
                &[pdf_viewer],
                &choice.defaults()
            ),
            Some(Launch::Host {
                desktop_id: "org.example.Viewer".into(),
                open: Some(Open::File("/home/you/paper.pdf".into())),
            })
        );

        // Choosing an app for a file lists every app that claims it, the one that opens it
        // today first: the type's claims before the extension's, the host's last.
        let listed = documents(
            Path::new("/home/you/todo.txt"),
            &[writer.clone(), notes.clone(), viewer.clone()],
            &choice.defaults(),
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
            document(
                Path::new("/home/you/todo.txt"),
                std::slice::from_ref(&notes),
                &choice.defaults()
            ),
            Some(listed[0].clone()),
            "opening a document uses the same order"
        );

        // The workspace said which app opens `.txt` files: it opens them, and leads the list.
        choice.workspace.policy.insert(
            ResourceKind::extension("txt").unwrap(),
            WorkspaceDefaultPolicy::App("so.tilo.word-processor".into()),
        );
        let text_editor = host_entry("org.example.TextEditor", "Text Editor", &["text/plain"]);
        let candidates = [text_editor, notes.clone(), writer.clone()];
        let wanted = Launch::Package {
            app: "so.tilo.word-processor".into(),
            open: Some(Open::File("/home/you/Budget.txt".into())),
        };
        assert_eq!(
            documents(
                Path::new("/home/you/Budget.txt"),
                &candidates,
                &choice.defaults()
            )
            .first(),
            Some(&wanted),
            "the workspace said which app opens `.txt` files, and it opens them"
        );
        assert_eq!(
            document(
                Path::new("/home/you/Budget.txt"),
                &candidates,
                &choice.defaults()
            ),
            Some(wanted)
        );

        // No Tilo app claims a PDF: the machine's own default decides between its apps, not
        // their names.
        let other = host_entry("org.example.Other", "Aardvark", &["application/pdf"]);
        choice.host = MimeAppsChain::new(vec![MimeAppsList::parse(
            "[Default Applications]\napplication/pdf=gone.desktop;org.example.Viewer.desktop;\n",
        )]);
        assert_eq!(
            document(
                Path::new("/home/you/paper.pdf"),
                &[other, viewer],
                &choice.defaults()
            ),
            Some(Launch::Host {
                desktop_id: "org.example.Viewer".into(),
                open: Some(Open::File("/home/you/paper.pdf".into())),
            }),
            "the machine's own default decides between its apps, not their names"
        );

        // The person chose an app for `text/plain` and another for `.txt`: a choice about
        // `.txt` is narrower than one about `text/plain`.
        choice.personal.handlers.insert(
            ResourceKind::mime("text/plain").unwrap(),
            "so.tilo.notes".into(),
        );
        choice.personal.handlers.insert(
            ResourceKind::extension("txt").unwrap(),
            "so.tilo.word-processor".into(),
        );
        assert_eq!(
            document(
                Path::new("/home/you/todo.txt"),
                &[notes, writer],
                &choice.defaults()
            ),
            Some(Launch::Package {
                app: "so.tilo.word-processor".into(),
                open: Some(Open::File("/home/you/todo.txt".into())),
            }),
            "a choice about `.txt` is narrower than one about `text/plain`"
        );

        // A format no host knows: only the app that made it claims it.
        assert_eq!(
            document(
                Path::new("/home/you/budget.tilosheet"),
                &[sheets],
                &choice.defaults()
            ),
            Some(Launch::Package {
                app: "so.tilo.sheets".into(),
                open: Some(Open::File("/home/you/budget.tilosheet".into())),
            })
        );

        // A document is known by its extension before the type its name declares.
        assert_eq!(
            document_kinds(Path::new("/home/you/Budget.TXT")),
            vec![
                ResourceKind::extension("txt").unwrap(),
                ResourceKind::mime("text/plain").unwrap(),
            ]
        );
        assert!(document_kinds(Path::new("/home/you/Makefile")).is_empty());

        // A record is asked for by its own URI, whatever the file it is kept in.
        let note = DocumentId::for_file("/home/you/Notes.note");
        assert_eq!(
            record(note.as_str()),
            Some(Open::Uri(format!("tilo-document:{}", note.as_str()))),
            "the app is asked for the record, not for the file it is kept in"
        );
        let minted = DocumentId::minted();
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

        // A link goes to the app for its scheme: the machine's own default, then a Tilo app
        // that claims the scheme ahead of it.
        choice.host = MimeAppsChain::new(vec![MimeAppsList::parse(
            "[Default Applications]\nx-scheme-handler/https=org.example.Viewer.desktop;\n",
        )]);
        let browser = host_entry(
            "org.example.Viewer",
            "Browser",
            &["x-scheme-handler/https", "x-scheme-handler/http"],
        );
        assert_eq!(
            link(
                "https://code.tilo.so",
                std::slice::from_ref(&browser),
                &choice.defaults()
            ),
            Some(Launch::Host {
                desktop_id: "org.example.Viewer".into(),
                open: Some(Open::Uri("https://code.tilo.so".into())),
            })
        );
        let tilo_browser = app("so.tilo.browser", &[], &[], &["https"]);
        let without_the_machines = Defaults {
            host: None,
            ..choice.defaults()
        };
        assert_eq!(
            link(
                "https://code.tilo.so",
                &[browser, tilo_browser],
                &without_the_machines
            ),
            Some(Launch::Package {
                app: "so.tilo.browser".into(),
                open: Some(Open::Uri("https://code.tilo.so".into())),
            }),
            "a Tilo app that claims the scheme comes first"
        );
        assert_eq!(
            link("mailto:person@example.com", &[], &choice.defaults()),
            None
        );
        assert_eq!(
            link("/home/you/a:b", &[], &choice.defaults()),
            None,
            "an absolute path is not a link"
        );

        // And the person's own choice of app for a scheme opens links of it there.
        choice.personal.handlers.insert(
            ResourceKind::scheme("https").unwrap(),
            "org.example.Browser.desktop".into(),
        );
        let person_browser = host_entry(
            "org.example.Browser",
            "Browser",
            &["x-scheme-handler/https"],
        );
        assert_eq!(
            link(
                "https://code.tilo.so",
                &[person_browser],
                &choice.defaults()
            ),
            Some(Launch::Host {
                desktop_id: "org.example.Browser".into(),
                open: Some(Open::Uri("https://code.tilo.so".into())),
            })
        );

        // A link of the scheme the YouTube app claims opens there, and with nothing claiming it
        // there is no app to open it.
        let tube = app("so.tilo.tube", &[], &[], &["tube"]);
        assert_eq!(
            link(
                "tube:search?q=quiet",
                std::slice::from_ref(&tube),
                &choice.defaults()
            ),
            Some(Launch::Package {
                app: "so.tilo.tube".into(),
                open: Some(Open::Uri("tube:search?q=quiet".into())),
            })
        );
        assert_eq!(
            link("tube:watch?v=abc", &[], &choice.defaults()),
            None,
            "nothing here claims it"
        );
    }
}
