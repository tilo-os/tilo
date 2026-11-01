//! One window per app: what a second launch does about the window the app
//! already has.
//!
//! The same app can be asked for twice — a tile clicked again, a search row,
//! a file — and what that means depends on the app. One that keeps a single
//! window is raised: starting it again would give the person a second copy of
//! it, holding the records the first already holds. One that opens another
//! window each time it is opened gets one, asked through the entry's own New
//! Window action where it declares one. A document or a link is always
//! handed to a launch of the app: its own command line carries it (`%f`/`%u`)
//! and the app's toolkit gives it to the process it already runs.
//!
//! The desk is what knows which windows exist, so it hands them in
//! ([`Shown`]) and this module decides ([`Instance::answer`]). Nothing here
//! reads a screen or starts anything.

use desktop_entry::{MainWindows, Scan};

use crate::launch::Launch;

/// How many windows an app keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Windows {
    /// One window: launching the app again raises the one it has. A Tilo
    /// package keeps one, since one process serves its records for a room,
    /// and so does an app whose entry says `SingleMainWindow=true`.
    One,
    /// Another window each time it is opened: the desktop entry
    /// specification's default when an entry declares nothing.
    #[default]
    Many,
}

/// The windows the desk shows, each with the id its app announces itself by.
///
/// Only the windows of the workspace in front belong here: a launch belongs
/// to the workspace the person is looking at, and an app in another one is
/// not what they asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    windows: Vec<(u64, String)>,
}

impl Shown {
    /// The windows `windows` names, each with the id its app announces
    /// itself by.
    #[must_use]
    pub fn new(windows: impl IntoIterator<Item = (u64, String)>) -> Self {
        Self {
            windows: windows.into_iter().collect(),
        }
    }

    /// Whether the desk shows no window at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// The first window of `app` the desk holds.
    #[must_use]
    pub fn of(&self, app: &Instance) -> Option<u64> {
        self.windows
            .iter()
            .find(|(_, announced)| app.announces(announced))
            .map(|(window, _)| *window)
    }
}

/// The app a launch names, as a second launch of it must know it: the ids its
/// windows announce themselves by, and how many windows it keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// A package id or a desktop-file id, as a tab, a notice or a log line
    /// names it.
    pub id: String,
    /// Every string its windows may announce themselves by.
    pub window_identities: Vec<String>,
    /// How many windows it keeps.
    pub windows: Windows,
}

impl Instance {
    /// The app `launch` names, as this device knows it: a Tilo package by its
    /// package id, one of the machine's own apps from the entry it runs from.
    ///
    /// `None` when the launch names no app installed here (planning the
    /// launch says which), and for a window already on its way and a crash
    /// notice, neither of which names an app that could be started again.
    #[must_use]
    pub fn of(launch: &Launch, entries: &Scan) -> Option<Self> {
        match launch {
            Launch::Package { app, .. }
            | Launch::Record { app, .. }
            | Launch::Content { app, .. } => Some(Self {
                id: app.clone(),
                window_identities: package_identities(app),
                windows: Windows::One,
            }),
            Launch::Host { desktop_id, .. } => {
                let entry = entries.find(desktop_id)?;
                Some(Self {
                    id: entry.id.clone(),
                    window_identities: entry.window_identities(),
                    windows: match entry.main_windows {
                        MainWindows::One => Windows::One,
                        MainWindows::Many => Windows::Many,
                    },
                })
            }
            Launch::Window { .. } | Launch::Crashed { .. } => None,
        }
    }

    /// Whether a window announcing itself as `announced` is one of this app's.
    #[must_use]
    pub fn announces(&self, announced: &str) -> bool {
        self.window_identities
            .iter()
            .any(|identity| identity == announced)
    }

    /// What a launch of this app does, given the windows the desk shows.
    #[must_use]
    pub fn answer(&self, launch: &Launch, shown: &Shown) -> Answer {
        let Some(window) = shown.of(self) else {
            return Answer::Start { new_window: false };
        };
        // A document or a link starts the app's own command line with it,
        // and the app's toolkit hands it to the process already running —
        // which is how an app the shell had started opened one too.
        if opens(launch) {
            return Answer::Start { new_window: false };
        }
        match self.windows {
            Windows::One => Answer::Raise { window },
            Windows::Many => Answer::Start { new_window: true },
        }
    }
}

/// What a launch of an app that may already show a window does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Start the app, plan and all. `new_window` when a window of it is
    /// already showing and it opens another on request: the plan asks for it
    /// through the entry's declared New Window action where there is one.
    Start { new_window: bool },
    /// Raise `window`, the one the app already shows; nothing starts.
    Raise { window: u64 },
}

/// Whether the launch names a document, a link, a record or some content for
/// the app to open.
fn opens(launch: &Launch) -> bool {
    match launch {
        Launch::Package { open, .. } | Launch::Host { open, .. } => open.is_some(),
        Launch::Record { .. } | Launch::Content { .. } => true,
        Launch::Window { .. } | Launch::Crashed { .. } => false,
    }
}

/// The strings a package's window may announce itself by: its id, and its
/// trailing component, which is what a toolkit defaults a Wayland `app_id`
/// to when the manifest does not say.
#[must_use]
pub fn package_identities(package_id: &str) -> Vec<String> {
    let mut identities = vec![package_id.to_owned()];
    if let Some(tail) = package_id.rsplit('.').next()
        && tail != package_id
        && !tail.is_empty()
    {
        identities.push(tail.to_owned());
    }
    identities
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::launch::Open;

    fn entries(desktop: &str, id: &str) -> Scan {
        let entry =
            desktop_entry::entry::parse(desktop, id, Path::new("/apps"), &[]).expect("parses");
        Scan {
            entries: vec![entry],
            ..Scan::default()
        }
    }

    fn notes() -> Instance {
        let launch = Launch::parse("so.tilo.notes").expect("a package launch");
        Instance::of(&launch, &Scan::default()).expect("a Tilo package is named by its id")
    }

    fn editor(main_windows: &str) -> (Launch, Instance) {
        let entries = entries(
            &format!(
                "[Desktop Entry]\nType=Application\nName=Editor\nExec=/usr/bin/editor\n\
                 StartupWMClass=org.example.Editor\n{main_windows}"
            ),
            "org.example.Editor.desktop",
        );
        let launch = Launch::parse("host:org.example.Editor.desktop").expect("a host launch");
        let instance = Instance::of(&launch, &entries).expect("the entry is there");
        (launch, instance)
    }

    /// An app that keeps one window is raised: a second launch of it starts
    /// nothing, whether its window announces the package id or the toolkit's
    /// own short id for it.
    #[test]
    fn a_second_launch_of_a_running_app_raises_the_window_it_has() {
        let notes = notes();
        assert_eq!(notes.windows, Windows::One);
        let launch = Launch::parse("so.tilo.notes").expect("a launch");
        for announced in ["so.tilo.notes", "notes"] {
            let shown = Shown::new([(4, "so.tilo.chat".to_owned()), (12, announced.to_owned())]);
            assert_eq!(
                notes.answer(&launch, &shown),
                Answer::Raise { window: 12 },
                "the window announcing itself as {announced}"
            );
        }
        assert_eq!(
            notes.answer(&launch, &Shown::new([(4, "so.tilo.chat".to_owned())])),
            Answer::Start { new_window: false },
            "a window of another app is not the one it has"
        );
        assert_eq!(
            notes.answer(&launch, &Shown::default()),
            Answer::Start { new_window: false }
        );
    }

    /// An app whose entry declares one main window is raised too, and its
    /// window is found by the class it sets.
    #[test]
    fn an_app_that_declares_one_main_window_is_raised() {
        let (launch, editor) = editor("SingleMainWindow=true\n");
        assert_eq!(editor.windows, Windows::One);
        let shown = Shown::new([(7, "org.example.Editor".to_owned())]);
        assert_eq!(editor.answer(&launch, &shown), Answer::Raise { window: 7 });
    }

    /// An app that opens another window each time is started again when it is
    /// already showing, and asked for a new window: its entry's own New
    /// Window action is what the plan uses.
    #[test]
    fn an_app_that_opens_more_windows_gets_another_one_when_asked() {
        let (launch, editor) = editor("");
        assert_eq!(editor.windows, Windows::Many);
        let shown = Shown::new([(7, "org.example.Editor.desktop".to_owned())]);
        assert_eq!(
            editor.answer(&launch, &shown),
            Answer::Start { new_window: true }
        );
        assert_eq!(
            editor.answer(&launch, &Shown::default()),
            Answer::Start { new_window: false },
            "nothing showing: the entry's own command line, no new-window action"
        );
    }

    /// A document or a link always starts the app's own command line: the app
    /// hands it to the process it already runs, as an app the shell had
    /// started received one.
    #[test]
    fn a_launch_with_a_document_or_a_link_goes_to_the_running_app() {
        let notes = notes();
        let shown = Shown::new([(12, "so.tilo.notes".to_owned())]);
        let document = Launch::parse("so.tilo.notes")
            .expect("a launch")
            .opening(Some(Open::File("/home/you/notes.tilo-note".into())));
        assert_eq!(
            notes.answer(&document, &shown),
            Answer::Start { new_window: false }
        );
        let (launch, editor) = editor("");
        let shown = Shown::new([(7, "org.example.Editor".to_owned())]);
        assert_eq!(
            editor.answer(
                &launch.opening(Some(Open::Uri("https://example.test/a".into()))),
                &shown
            ),
            Answer::Start { new_window: false },
            "a link reaches the running app through the entry's %u"
        );
        let record = Launch::parse("record:so.tilo.notes:ideas").expect("a record launch");
        assert_eq!(
            notes.answer(&record, &shown),
            Answer::Start { new_window: false },
            "a record the app opens in the process it has"
        );
    }

    /// A launch that names no app installed here, or no app at all, decides
    /// nothing: planning it says why it starts nothing.
    #[test]
    fn a_launch_naming_no_installed_app_decides_nothing() {
        let scan = Scan::default();
        for launch in ["window:12", "host:org.example.Missing.desktop"] {
            let launch = Launch::parse(launch).expect("a launch");
            assert_eq!(Instance::of(&launch, &scan), None, "{launch:?}");
        }
        assert_eq!(
            Instance::of(
                &Launch::parse("crashed:so.tilo.canvas").expect("a notice"),
                &scan
            ),
            None,
            "a notice starts nothing until the person asks for the app again"
        );
    }
}
