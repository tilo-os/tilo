//! What a launch string names.
//!
//! The desktop surface hands a [`crate::TileSource`] one string per launch:
//! a Tilo package id, `host:<desktop-id>`, `window:<n>`, or an app opening a
//! record or content. The vocabulary is shared with what the surface and the
//! desk already build ([`tilo_desktop_surface::session::record_launch`]); this
//! module reads it back so a session can tell what it is being asked to start
//! before it starts anything.

use std::path::PathBuf;

/// The launch id prefix of one of the machine's own applications: `host:`
/// and its desktop-file id, as the host apps search provider names it.
pub const HOST_PREFIX: &str = "host:";
/// The launch id prefix of a window on its way: `window:` and its id.
pub const WINDOW_PREFIX: &str = "window:";
/// The launch id prefix of an app opening one of its records.
pub const RECORD_PREFIX: &str = "record:";
/// The launch id prefix of an app opening content another app handed over.
pub const CONTENT_PREFIX: &str = "content:";
/// The launch id prefix of the tab a crashed app's notice opens under.
pub const CRASHED_PREFIX: &str = "crashed:";

/// What an app is opened with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    /// A document, as an absolute host path, for the app's `argv[1]`.
    File(PathBuf),
    /// A validated link, forwarded as one opaque argument.
    Uri(String),
}

/// What a launch string asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    /// A Tilo package, by package id.
    Package { app: String, open: Option<Open> },
    /// One of the machine's own applications, by desktop-file id.
    Host {
        desktop_id: String,
        open: Option<Open>,
    },
    /// A window the compositor is already adopting: no process starts.
    Window { id: u64 },
    /// A Tilo app opening the record it keeps under `record`.
    Record { app: String, record: String },
    /// A Tilo app opening content, by content address hash.
    Content { app: String, hash: String },
    /// The notice a crashed app leaves behind: the app it names, nothing to
    /// start until the person asks for it again.
    Crashed { app: String },
}

impl Launch {
    /// What `launch` asks for, or `None` when no kind of launch it is.
    ///
    /// A `window:` id that is not a number, a `host:` id that is empty, and
    /// a `record:`/`content:` without both halves are all `None`: a surface
    /// asking for one of those has a bug, and starting something arbitrary
    /// on its behalf would be worse than starting nothing.
    #[must_use]
    pub fn parse(launch: &str) -> Option<Self> {
        if let Some(id) = launch.strip_prefix(WINDOW_PREFIX) {
            return id.parse().ok().map(|id| Self::Window { id: id });
        }
        if let Some(desktop_id) = launch.strip_prefix(HOST_PREFIX) {
            return (!desktop_id.is_empty()).then(|| Self::Host {
                desktop_id: desktop_id.to_owned(),
                open: None,
            });
        }
        if let Some(rest) = launch.strip_prefix(RECORD_PREFIX) {
            let (app, record) = rest.split_once(':')?;
            return (!app.is_empty() && !record.is_empty()).then(|| Self::Record {
                app: app.to_owned(),
                record: record.to_owned(),
            });
        }
        if let Some(rest) = launch.strip_prefix(CONTENT_PREFIX) {
            let (app, hash) = rest.split_once(':')?;
            return (!app.is_empty() && !hash.is_empty()).then(|| Self::Content {
                app: app.to_owned(),
                hash: hash.to_owned(),
            });
        }
        if let Some(app) = launch.strip_prefix(CRASHED_PREFIX) {
            return (!app.is_empty()).then(|| Self::Crashed {
                app: app.to_owned(),
            });
        }
        // A plain name is a package id: Tilo's own app ids are reverse-DNS
        // and contain neither `:` nor whitespace, so anything left is one.
        let package =
            !launch.is_empty() && !launch.contains(':') && !launch.chars().any(char::is_whitespace);
        package.then(|| Self::Package {
            app: launch.to_owned(),
            open: None,
        })
    }

    /// The app this launch is about, for a tab's title, a crash notice or a
    /// log line: a package id or a desktop-file id.
    #[must_use]
    pub fn app(&self) -> &str {
        match self {
            Self::Package { app, .. }
            | Self::Record { app, .. }
            | Self::Content { app, .. }
            | Self::Crashed { app } => app,
            Self::Host { desktop_id, .. } => desktop_id,
            Self::Window { .. } => "",
        }
    }

    /// The same launch opening `open`.
    #[must_use]
    pub fn opening(self, open: Option<Open>) -> Self {
        match (self, open) {
            (Self::Package { app, .. }, open) => Self::Package { app, open },
            (Self::Host { desktop_id, .. }, open) => Self::Host { desktop_id, open },
            (launch, _) => launch,
        }
    }

    /// The launch string that asks for this launch again: what a person's
    /// Reopen queues, so a notice brings back the same app the same way.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Package { app, .. } => app.clone(),
            Self::Host { desktop_id, .. } => format!("{HOST_PREFIX}{desktop_id}"),
            Self::Window { id } => format!("{WINDOW_PREFIX}{id}"),
            Self::Record { app, record } => format!("{RECORD_PREFIX}{app}:{record}"),
            Self::Content { app, hash } => format!("{CONTENT_PREFIX}{app}:{hash}"),
            Self::Crashed { app } => format!("{CRASHED_PREFIX}{app}"),
        }
    }

    /// The launch string that opens `app`'s crash notice.
    #[must_use]
    pub fn crashed(app: &str) -> String {
        format!("{CRASHED_PREFIX}{app}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_launch_is_read_back_as_what_asked_for_it() {
        assert_eq!(
            Launch::parse("so.tilo.notes"),
            Some(Launch::Package {
                app: "so.tilo.notes".into(),
                open: None
            })
        );
        assert_eq!(
            Launch::parse("host:org.gnome.TextEditor.desktop"),
            Some(Launch::Host {
                desktop_id: "org.gnome.TextEditor.desktop".into(),
                open: None
            })
        );
        assert_eq!(Launch::parse("window:41"), Some(Launch::Window { id: 41 }));
        assert_eq!(
            Launch::parse("record:so.tilo.sheets:budget"),
            Some(Launch::Record {
                app: "so.tilo.sheets".into(),
                record: "budget".into()
            })
        );
        assert_eq!(
            Launch::parse(&format!("content:so.tilo.photos:{}", "0".repeat(64))),
            Some(Launch::Content {
                app: "so.tilo.photos".into(),
                hash: "0".repeat(64)
            })
        );
        assert_eq!(
            Launch::crashed("so.tilo.canvas"),
            "crashed:so.tilo.canvas",
            "the notice reopens the app it names"
        );
        assert_eq!(
            Launch::parse("crashed:so.tilo.canvas"),
            Some(Launch::Crashed {
                app: "so.tilo.canvas".into()
            })
        );
    }

    #[test]
    fn a_launch_writes_itself_back_as_the_string_that_asked_for_it() {
        for launch in [
            "so.tilo.notes",
            "host:org.gnome.TextEditor.desktop",
            "window:41",
            "record:so.tilo.sheets:budget",
            "crashed:so.tilo.canvas",
        ] {
            let parsed = Launch::parse(launch).expect("a launch");
            assert_eq!(parsed.id(), launch, "{launch} is asked for again as it was");
        }
    }

    #[test]
    fn a_launch_naming_nothing_usable_starts_nothing() {
        for launch in [
            "",
            "window:",
            "window:soon",
            "host:",
            "record:so.tilo.sheets",
            "record::budget",
            "content:so.tilo.photos",
            "crashed:",
            "not a package id",
            "some:unknown:kind",
        ] {
            assert_eq!(Launch::parse(launch), None, "{launch:?}");
        }
    }

    #[test]
    fn a_launch_names_the_app_a_notice_or_a_title_wants() {
        let launch = Launch::parse("record:so.tilo.sheets:budget").expect("a record launch");
        assert_eq!(launch.app(), "so.tilo.sheets");
        let launch = Launch::Host {
            desktop_id: "org.gnome.Nautilus.desktop".into(),
            open: None,
        }
        .opening(Some(Open::File("/home/you/report.odt".into())));
        assert_eq!(launch.app(), "org.gnome.Nautilus.desktop");
        assert_eq!(
            launch,
            Launch::Host {
                desktop_id: "org.gnome.Nautilus.desktop".into(),
                open: Some(Open::File("/home/you/report.odt".into())),
            }
        );
    }
}
