//! The questions about the person's apps: whether to add a program they downloaded to their
//! apps, and whether to uninstall one.
//! Both are about what the registry of the person's downloaded programs
//! ([`tilo_app_launch::downloaded`]) keeps, and both are answered through it: the reading lists a
//! program running with no entry as an offer, so a program is asked about once while it is offered,
//! and the answer is the registry's own action. Nothing here adds or uninstalls an app by itself:
//! everything is asked first, as the desktop's own question the person answers on the asking view
//! like every other ([`crate::program_asks`]), and whether it happened is read off the registry
//! again rather than assumed.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::future::Future as _;
use std::path::Path;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::remote::downloaded_apps::{self as wire, DownloadedApps, DownloadedDoing};
use tilo_app_launch::downloaded;
use tilo_service::{Answer, AskSender, Asked, Question, Services};

use crate::Shared;
use crate::i18n::{t, t_args};
use crate::platform::notice::{Notice, Notices};
use tilo_session_core::desktop::notifications::NotificationIconKind;
use ui::i18n::ArgValue;

/// The kind of the question that adds a downloaded program to the person's apps.
pub(crate) const ADD_KIND: &str = "app.add";
/// The kind of the question that uninstalls one of the person's apps.
pub(crate) const UNINSTALL_KIND: &str = "app.uninstall";

/// What the add question shows: a program that just started and has no entry in the person's
/// apps. The program has already started; the question only decides whether it gets an entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AddQuestion {
    /// What it calls itself.
    pub(crate) name: String,
    /// The file that starts it; its answer names it.
    pub(crate) file: String,
    /// Whether the file is an `AppImage`: one self-contained file by design, the only kind that
    /// may be moved. A program from a tarball or a loose binary may need the files beside it.
    pub(crate) appimage: bool,
    /// Its artwork, when the program brings a file of it.
    #[serde(default)]
    pub(crate) artwork: Option<String>,
}

impl AddQuestion {
    /// The file's name, as the question names it.
    pub(crate) fn file_name(&self) -> String {
        Path::new(&self.file).file_name().map_or_else(
            || self.file.clone(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

/// The answer to the add question that adds the program and moves its `AppImage` into
/// Applications; the other adds it where it is.
pub(crate) const MOVE_CHOICE: usize = 1;

/// What the uninstall question shows: the app, and every copy of it that goes to the Trash.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UninstallQuestion {
    /// The app's entry.
    pub(crate) desktop_id: String,
    /// The name the person knows it by.
    pub(crate) name: String,
    /// The files that go to the Trash, the one it starts first.
    pub(crate) copies: Vec<String>,
    /// The person's home, which the folders are said from.
    #[serde(default)]
    pub(crate) home: Option<String>,
}

/// The two questions about the person's apps, and what they were answered.
pub(crate) struct AppSheets {
    /// Where the person is asked.
    questions: AskSender,
    /// The add question up for the person, and the program it is about.
    adding: RefCell<Option<(Asked, AddQuestion)>>,
    /// The uninstall question up for the person, and the app it is about.
    removing: RefCell<Option<(Asked, UninstallQuestion)>>,
    /// The registry's actions and reading, as the session publishes them.
    services: Rc<RefCell<Services>>,
    /// The person's notices: what an answer did.
    notices: Rc<dyn Notices>,
    /// The files the add question asked about, so a program that keeps running is asked about
    /// once. A file the registry no longer offers is forgotten, so a program whose app was taken
    /// off the apps is asked about again when it runs.
    asked: RefCell<BTreeSet<String>>,
    /// The apps the person asked to uninstall, waiting for the question; they are asked about one
    /// at a time.
    wanted: RefCell<VecDeque<String>>,
    /// The uninstalls answered, until the registry's reading shows them gone.
    uninstalling: RefCell<Vec<Uninstalling>>,
    /// The last failure the person was told of: the registry's failures are said once, in the order
    /// they happened.
    reported: Cell<u64>,
    /// Whether the person was told that their apps' registry does not read back here, which takes
    /// their apps off the launcher: said once.
    told: Cell<bool>,
}

/// An app the person said to uninstall, and what the question said would go.
#[derive(Clone, Debug)]
struct Uninstalling {
    desktop_id: String,
    name: String,
    copies: usize,
}

/// What a question the person was asked came to, polled on the session's loop: their answer, or
/// `None` for one they declined, and nothing yet while it still stands.
fn answered(asked: &mut Asked) -> Option<Option<Answer>> {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match std::pin::Pin::new(asked).poll(&mut context) {
        std::task::Poll::Ready(answer) => Some(answer),
        std::task::Poll::Pending => None,
    }
}

impl AppSheets {
    pub(crate) fn new(
        questions: AskSender,
        services: Rc<RefCell<Services>>,
        notices: Rc<dyn Notices>,
    ) -> Self {
        Self {
            questions,
            adding: RefCell::new(None),
            removing: RefCell::new(None),
            services,
            notices,
            asked: RefCell::new(BTreeSet::new()),
            wanted: RefCell::new(VecDeque::new()),
            uninstalling: RefCell::new(Vec::new()),
            reported: Cell::new(0),
            told: Cell::new(false),
        }
    }

    /// Ask the person whether to uninstall `app` , named by its entry's id or by the name they know
    /// it by: the question stands until they answer it. Asked about its id,
    /// so the same app asked about twice is asked once.
    pub(crate) fn ask_uninstall(&self, app: &str, reading: Option<&DownloadedApps>) {
        let asked = reading
            .and_then(|reading| app_named(reading, app))
            .map_or_else(|| app.trim().to_owned(), |app| app.desktop_id.clone());
        let mut wanted = self.wanted.borrow_mut();
        if !wanted.iter().any(|queued| *queued == asked) {
            wanted.push_back(asked);
        }
    }

    /// Carry out what the person answered to either question, ask the next question, and say what a
    /// finished uninstall did — or why one did not happen.
    pub(crate) fn follow(&self, reading: Option<&DownloadedApps>) {
        self.follow_add(reading);
        self.follow_uninstall(reading);
        self.follow_failures(reading);
    }

    /// Whether a question about the person's apps is up: they are asked one at a time, in front of
    /// what they are doing.
    fn is_asking(&self) -> bool {
        self.adding.borrow().is_some() || self.removing.borrow().is_some()
    }

    /// Add programs: the answer to the add question, then the next program the registry offers
    /// that the person has not been asked about.
    fn follow_add(&self, reading: Option<&DownloadedApps>) {
        let offers: &[wire::DownloadedOffer] = reading.map_or(&[], |reading| &reading.offers);
        // A file the registry no longer offers is asked about again when it runs, as an app taken
        // off the apps comes back as a program with no entry.
        self.asked
            .borrow_mut()
            .retain(|file| offers.iter().any(|offer| offer.file == *file));
        let came = {
            let mut adding = self.adding.borrow_mut();
            let came = adding.as_mut().and_then(|(asked, _)| answered(asked));
            came.and_then(|answer| adding.take().map(|(_, offer)| (answer, offer)))
        };
        if let Some((answer, offer)) = came {
            self.asked.borrow_mut().insert(offer.file.clone());
            let mut arguments = Map::new();
            arguments.insert(wire::FILE_PARAM.to_owned(), Value::from(offer.file));
            match answer {
                Some(answer) => {
                    if offer.appimage && answer.choice == Some(MOVE_CHOICE) {
                        arguments.insert(wire::MOVE_PARAM.to_owned(), Value::from(true));
                    }
                    self.invoke(downloaded::RESOURCE, wire::ADD, &arguments, false);
                }
                // Any other way of putting the question away is an answer too: that file is not
                // asked about again.
                None => self.invoke(downloaded::RESOURCE, wire::DECLINE, &arguments, false),
            }
        }
        if self.is_asking() {
            return;
        }
        let Some(offer) = offers
            .iter()
            .find(|offer| !offer.declined && !self.asked.borrow().contains(&offer.file))
        else {
            return;
        };
        let offer = offer_of(offer);
        let asked = self.questions.ask(Question {
            handle: format!("{ADD_KIND}:{}", offer.file),
            kind: ADD_KIND.to_owned(),
            app_id: None,
            label: offer.name.clone(),
            details: serde_json::to_value(&offer).unwrap_or_default(),
        });
        *self.adding.borrow_mut() = Some((asked, offer));
    }

    /// Uninstall an app: the answer to the uninstall question, what the reading says came of an
    /// earlier one, then the next app the person asked about.
    fn follow_uninstall(&self, reading: Option<&DownloadedApps>) {
        let came = {
            let mut removing = self.removing.borrow_mut();
            let came = removing.as_mut().and_then(|(asked, _)| answered(asked));
            came.and_then(|answer| removing.take().map(|(_, app)| (answer, app)))
        };
        if let Some((Some(_), app)) = came {
            self.uninstalling.borrow_mut().push(Uninstalling {
                desktop_id: app.desktop_id.clone(),
                name: app.name.clone(),
                copies: app.copies.len(),
            });
            // The person answered the question the registry's uninstall asks.
            self.invoke(
                &wire::app(&app.desktop_id),
                wire::UNINSTALL,
                &Map::new(),
                true,
            );
        }
        if let Some(reading) = reading {
            let mut finished = Vec::new();
            self.uninstalling.borrow_mut().retain(|pending| {
                let gone = reading
                    .apps
                    .iter()
                    .all(|app| app.desktop_id != pending.desktop_id);
                if gone {
                    finished.push(pending.clone());
                }
                !gone
            });
            for pending in finished {
                self.tell_uninstalled(&pending);
            }
        }
        if self.is_asking() {
            return;
        }
        let Some(asked) = self.wanted.borrow_mut().pop_front() else {
            return;
        };
        match question(reading, &asked) {
            Some(app) => {
                let asked = self.questions.ask(Question {
                    handle: format!("{UNINSTALL_KIND}:{}", app.desktop_id),
                    kind: UNINSTALL_KIND.to_owned(),
                    app_id: None,
                    label: app.name.clone(),
                    details: serde_json::to_value(&app).unwrap_or_default(),
                });
                *self.removing.borrow_mut() = Some((asked, app));
            }
            None => {
                self.notices.show(
                    &Notice::new(
                        t("app-list-app-name"),
                        1,
                        t("app-list-gone-title"),
                        t("app-list-gone-body"),
                    )
                    .about(NotificationIconKind::Generic),
                );
            }
        }
    }

    /// Invoke one of the registry's actions on `resource` : the route [`crate::Session::invoke`]
    /// takes, `confirmed` where the person answered the question asked them.
    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
        confirmed: bool,
    ) {
        let mut services = self.services.borrow_mut();
        let started = if confirmed {
            services.invoke_confirmed(resource, action, arguments)
        } else {
            services.invoke(resource, action, arguments)
        };
        match started {
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                tracing::warn!(%error, resource, action, "the person's apps");
            }
            None => tracing::warn!(resource, action, "no registry of the person's apps"),
        }
    }

    /// Say what the registry could not do: the person answered a question or chose a row and
    /// nothing happened, so this is why. Said once each, as the registry counts its attempts.
    fn follow_failures(&self, reading: Option<&DownloadedApps>) {
        let Some(reading) = reading else {
            return;
        };
        let mut reported = self.reported.get();
        for failure in &reading.failures {
            if failure.attempt <= reported {
                continue;
            }
            reported = failure.attempt;
            // A failed uninstall leaves the app where it was: nothing more comes of it, and the
            // notice for the failure is the one to show.
            self.uninstalling
                .borrow_mut()
                .retain(|pending| pending.desktop_id != failure.subject);
            let name = ArgValue::Text(failure.name.clone());
            let title = match failure.doing {
                DownloadedDoing::Add => t_args("app-list-add-failed", &[("name", name)]),
                DownloadedDoing::Locate => t_args("app-list-locate-failed", &[("name", name)]),
                DownloadedDoing::Remove => t_args("app-list-remove-failed", &[("name", name)]),
                DownloadedDoing::Uninstall => {
                    t_args("app-list-uninstall-failed", &[("name", name)])
                }
            };
            self.notices.show(
                &Notice::new(t("app-list-app-name"), 2, title, failure.detail.clone())
                    .about(NotificationIconKind::Generic),
            );
        }
        self.reported.set(reported);
    }

    /// Say what a finished uninstall did, as the question said what would go.
    fn tell_uninstalled(&self, pending: &Uninstalling) {
        let name = ArgValue::Text(pending.name.clone());
        let title = t_args("app-list-uninstalled", &[("name", name)]);
        let body = if pending.copies > 1 {
            t("app-list-uninstalled-files")
        } else {
            t("app-list-uninstalled-file")
        };
        self.notices.show(
            &Notice::new(t("app-list-app-name"), 1, title, body)
                .about(NotificationIconKind::Generic),
        );
    }
}

impl Shared {
    /// The registry's reading now: the apps the person has and the programs
    /// running with no entry; `None` when the session publishes no registry,
    /// or when what this computer kept does not read back — which the person
    /// is told once, because their apps are gone from the launcher until it
    /// does.
    pub(crate) fn downloaded_apps(&self) -> Option<DownloadedApps> {
        let reading = self.services.borrow().read(downloaded::RESOURCE)?;
        match serde_json::from_value(reading) {
            Ok(apps) => Some(apps),
            Err(error) => {
                // Said once: the registry is read on every pass of the session's loop, and a log
                // that repeats it says nothing new.
                if !self.app_sheets.told.replace(true) {
                    tracing::warn!(%error, "the downloaded apps registry does not read back");
                    self.notifications
                        .borrow()
                        .tell(&crate::notices::downloaded_apps_notice());
                }
                None
            }
        }
    }

    /// Ask the person whether to uninstall `app`, as the registry reads now.
    pub(crate) fn ask_uninstall(&self, app: &str) -> bool {
        let reading = self.downloaded_apps();
        self.app_sheets.ask_uninstall(app, reading.as_ref());
        true
    }
}

/// What the uninstall question asks: the app `asked` names, and every copy of it that would go to
/// the Trash.
fn question(reading: Option<&DownloadedApps>, asked: &str) -> Option<UninstallQuestion> {
    let app = app_named(reading?, asked)?;
    let copies = if app.copies.is_empty() {
        vec![app.file.clone()]
    } else {
        app.copies.clone()
    };
    Some(UninstallQuestion {
        desktop_id: app.desktop_id.clone(),
        name: app.name.clone(),
        copies,
        home: tilo_paths::home().map(|home| home.to_string_lossy().into_owned()),
    })
}

/// The app of `reading` that `asked` names: by its entry's id, or by the name the person knows it
/// by (trimmed, in any case).
fn app_named<'a>(reading: &'a DownloadedApps, asked: &str) -> Option<&'a wire::DownloadedApp> {
    let asked = asked.trim();
    reading
        .apps
        .iter()
        .find(|app| app.desktop_id == asked)
        .or_else(|| {
            reading
                .apps
                .iter()
                .find(|app| app.name.eq_ignore_ascii_case(asked))
        })
}

/// The program an offer asks about, as the add question shows it.
fn offer_of(offer: &wire::DownloadedOffer) -> AddQuestion {
    AddQuestion {
        file: offer.file.clone(),
        name: offer.name.clone(),
        appimage: offer.kind == wire::DownloadedKind::AppImage,
        artwork: offer.artwork.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use desktop_entry::downloaded::{Identity, ProcessFacts, Program, ProgramKind, Store};
    use tilo_app_launch::downloaded::{Downloaded, ObservedWindow};
    use tilo_test_env::TestHome;

    /// A type 2 `AppImage` 's identification: an ELF executable whose padding says `AI\x02` . The
    /// image carries no name of its own, so it is named after its file.
    fn appimage() -> Vec<u8> {
        let mut bytes = vec![0_u8; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[8..11].copy_from_slice(b"AI\x02");
        bytes
    }

    /// The session's side of the app sheets: the registry of the person's downloaded programs, the
    /// two sheets' mounts, and the session's notices.
    struct Session {
        sheets: AppSheets,
        services: Rc<RefCell<Services>>,
        notices: crate::notifications::told::Told,
        /// The person's side of the questions the session asks.
        person: tilo_service::AskReceiver,
        home: TestHome,
    }

    /// A session whose person has `files` in their home (path, contents) and is running `running`
    /// programs (the window's app id, the file in their home it runs).
    fn session(files: &[(&str, &[u8])], running: &[(&str, &str)]) -> Session {
        let home = TestHome::new().expect("a home");
        for (path, contents) in files {
            write(&home.home().join(path), contents);
        }
        let registry = Downloaded::start(None).expect("a registry");
        for (ordinal, (app_id, path)) in running.iter().enumerate() {
            let opened = u64::try_from(ordinal).expect("a count");
            let file = home.home().join(path);
            registry.handle().observed(
                ObservedWindow {
                    id: opened,
                    app_id: (*app_id).to_owned(),
                    pid: Some(u32::try_from(ordinal).expect("a count")),
                    artwork: None,
                },
                ProcessFacts {
                    exe: file.clone(),
                    appimage: Some(file),
                    parent_script: None,
                },
            );
        }
        let mut services = Services::default();
        services.add(Box::new(registry));
        let services = Rc::new(RefCell::new(services));
        let notices = crate::notifications::told::Told::default();
        let (asker, person) = tilo_service::asks::channel();
        let sheets = AppSheets::new(
            asker,
            Rc::clone(&services),
            Rc::new(notices.clone()) as Rc<dyn Notices>,
        );
        Session {
            sheets,
            services,
            notices,
            person,
            home,
        }
    }

    /// A runnable file at `path` , with `contents` .
    fn write(path: &std::path::Path, contents: &[u8]) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("a folder");
        std::fs::write(path, contents).expect("a file");
        let mut permissions = std::fs::metadata(path).expect("the file").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("a runnable file");
    }

    /// The app `name` , made of `copies` (the first is the one it starts).
    fn app_at(home: &TestHome, key: &str, name: &str, copies: &[PathBuf]) -> String {
        for copy in copies {
            write(copy, &appimage());
        }
        let store = Store::in_data_home(home.data());
        let identity = Identity {
            key: key.to_owned(),
            name: name.to_owned(),
            icon: None,
            comment: None,
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: Vec::new(),
            startup_wm_class: None,
            field_code: None,
        };
        // Each add points the entry at its copy and keeps the one before it.
        let mut desktop_id = String::new();
        for copy in copies {
            desktop_id = store
                .add(
                    &identity,
                    &Program {
                        path: copy.clone(),
                        kind: ProgramKind::AppImage,
                    },
                )
                .expect("the app");
        }
        // The entries are this test's own writes: the memoised scan is told to read them now, as a
        // writer in this process that must see its own change does, rather than waiting for a
        // directory notification that says the same thing later — or, on a machine whose
        // application directories cannot be watched, never.
        desktop_entry::cache::refresh();
        desktop_id
    }

    impl Session {
        /// Frames of the session's loop: what the registry's worker found lands, is read, and the
        /// sheets follow it.
        fn settle(&self) {
            for _ in 0..4 {
                self.services.borrow_mut().settle();
                self.services.borrow_mut().poll();
                let reading = self.reading();
                self.sheets.follow(reading.as_ref());
            }
        }

        /// Settle until `condition` , which the registry answers on its own thread or through the
        /// sheet the person answered.
        fn until(&self, what: &str, mut condition: impl FnMut() -> bool) {
            // What this waits for is the registry's own work, so the bound is the suite's, not a
            // second count.
            let deadline = tilo_test_env::hang_deadline();
            while Instant::now() < deadline {
                self.settle();
                if condition() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("{what} did not happen");
        }

        /// The registry's reading now.
        fn reading(&self) -> Option<DownloadedApps> {
            self.services
                .borrow()
                .read(downloaded::RESOURCE)
                .map(|reading| serde_json::from_value(reading).expect("a reading"))
        }

        /// The apps the person has now.
        fn apps(&self) -> Vec<wire::DownloadedApp> {
            self.reading()
                .map(|reading| reading.apps)
                .unwrap_or_default()
        }

        /// The offers there now.
        fn offers(&self) -> Vec<wire::DownloadedOffer> {
            self.reading()
                .map(|reading| reading.offers)
                .unwrap_or_default()
        }

        /// The titles and bodies of the notices the session shows now.
        fn notices(&self) -> Vec<(String, String)> {
            self.notices.told()
        }

        /// The questions the person is asked now, of `kind`.
        fn asked(&self, kind: &str) -> Vec<tilo_service::Question> {
            self.person
                .pending()
                .into_iter()
                .filter(|question| question.kind == kind)
                .collect()
        }

        /// The one add question asked now, as it reads.
        fn adding(&self) -> Option<(String, AddQuestion)> {
            let question = self.asked(ADD_KIND).pop()?;
            let asked = serde_json::from_value(question.details).expect("an add question");
            Some((question.handle, asked))
        }

        /// The one uninstall question asked now, as it reads.
        fn removing(&self) -> Option<(String, UninstallQuestion)> {
            let question = self.asked(UNINSTALL_KIND).pop()?;
            let asked = serde_json::from_value(question.details).expect("an uninstall question");
            Some((question.handle, asked))
        }

        /// The person picks the `choice` th answer to the question `handle`, or says yes where it
        /// offers no choices.
        fn answer(&self, handle: &str, choice: Option<usize>) {
            assert!(
                self.person.answer(
                    handle,
                    tilo_service::Answer {
                        choice,
                        ..tilo_service::Answer::default()
                    }
                ),
                "the question waits for its answer"
            );
        }

        /// The person declines the question `handle`.
        fn decline(&self, handle: &str) {
            assert!(
                self.person.decline(handle),
                "the question waits for its answer"
            );
        }
    }

    #[test]
    fn a_program_running_with_no_entry_is_asked_about_and_adding_it_puts_it_in_apps() {
        let session = session(
            &[("Downloads/Krita.AppImage", &appimage())],
            &[("org.example.Krita", "Downloads/Krita.AppImage")],
        );
        session.until("the question about the program", || {
            session.adding().is_some()
        });

        // The question names the program and the file it runs from, and offers to move it, since an
        // AppImage is one self-contained file.
        let (handle, asked) = session.adding().expect("the question");
        assert_eq!(asked.name, "Krita");
        assert_eq!(
            PathBuf::from(&asked.file),
            session.home.home().join("Downloads/Krita.AppImage")
        );
        assert!(asked.appimage);

        // Answered with the move, the program becomes an app in Applications.
        session.answer(&handle, Some(MOVE_CHOICE));
        session.until("the app", || !session.apps().is_empty());
        let moved = session.home.home().join("Applications/Krita.AppImage");
        assert!(moved.is_file(), "the AppImage is in Applications");
        assert!(
            !session
                .home
                .home()
                .join("Downloads/Krita.AppImage")
                .exists(),
            "and no longer in Downloads"
        );
        let app = session.apps().pop().expect("the app");
        assert_eq!(app.name, "Krita");
        assert_eq!(PathBuf::from(&app.file), moved);
        assert!(
            session.offers().is_empty(),
            "the question was answered, so there is no offer left"
        );
        assert!(session.adding().is_none(), "and it is asked no more");
    }

    #[test]
    fn adding_where_the_file_is_leaves_it_where_the_person_downloaded_it() {
        let session = session(
            &[("Downloads/Krita.AppImage", &appimage())],
            &[("org.example.Krita", "Downloads/Krita.AppImage")],
        );
        session.until("the question about the program", || {
            session.adding().is_some()
        });
        let (handle, _) = session.adding().expect("the question");
        session.answer(&handle, Some(0));
        session.until("the app", || !session.apps().is_empty());
        let downloaded = session.home.home().join("Downloads/Krita.AppImage");
        assert!(downloaded.is_file(), "the file stays where it was");
        assert_eq!(
            PathBuf::from(&session.apps()[0].file),
            downloaded,
            "the app starts the file the person downloaded"
        );
    }

    #[test]
    fn not_adding_it_keeps_it_off_the_apps_and_is_not_asked_again() {
        let session = session(
            &[("Downloads/Krita.AppImage", &appimage())],
            &[("org.example.Krita", "Downloads/Krita.AppImage")],
        );
        session.until("the question about the program", || {
            session.adding().is_some()
        });
        let (handle, _) = session.adding().expect("the question");
        session.decline(&handle);
        session.until("the answer", || {
            session.offers().first().is_some_and(|offer| offer.declined)
        });

        // It is no app, its file stays, and the offer stays marked so search can still add it.
        assert!(session.apps().is_empty(), "nothing was added");
        assert!(
            session
                .home
                .home()
                .join("Downloads/Krita.AppImage")
                .is_file()
        );

        // It keeps running, and it is not asked about again.
        for _ in 0..4 {
            session.settle();
        }
        assert!(session.adding().is_none(), "asked only once");
    }

    #[test]
    fn one_question_stands_at_a_time_and_the_next_program_follows_it() {
        let session = session(
            &[
                ("Downloads/Krita.AppImage", &appimage()),
                ("Downloads/Inkscape.AppImage", &appimage()),
            ],
            &[
                ("org.example.Krita", "Downloads/Krita.AppImage"),
                ("org.example.Inkscape", "Downloads/Inkscape.AppImage"),
            ],
        );
        session.until("both programs offered", || session.offers().len() == 2);
        session.settle();
        assert_eq!(session.asked(ADD_KIND).len(), 1, "never both at once");
        let (handle, first) = session.adding().expect("one of them is asked about");
        assert!(
            session
                .offers()
                .iter()
                .any(|offer| offer.name == first.name),
            "the question is about one of the programs offered"
        );

        // Answering it, the other program is asked about.
        session.decline(&handle);
        session.until("the other program", || {
            session
                .adding()
                .is_some_and(|(_, asked)| asked.name != first.name)
        });
        assert_eq!(session.asked(ADD_KIND).len(), 1, "one question stands");
    }

    #[test]
    fn an_app_the_person_uninstalls_goes_with_every_copy_and_the_notice_says_so() {
        let session = session(&[], &[]);
        let applications = session
            .home
            .home()
            .join("Applications/Tilo Imager.AppImage");
        let downloads = session.home.home().join("Downloads/tilo-imager.AppImage");
        let desktop_id = app_at(
            &session.home,
            "tilo-imager",
            "Tilo Imager",
            &[applications.clone(), downloads.clone()],
        );
        // The entry is written once per copy; the registry reads both, and the question is built
        // from the reading that lists every copy.
        session.until("the app, with every copy of it", || {
            session
                .apps()
                .first()
                .is_some_and(|app| app.copies.len() == 2)
        });

        // The question lists every copy that goes, and where they are.
        let reading = session.reading();
        session.sheets.ask_uninstall(&desktop_id, reading.as_ref());
        session.until("the question about the app", || {
            session.removing().is_some()
        });
        let (handle, asked) = session.removing().expect("the question");
        assert_eq!(asked.name, "Tilo Imager");
        assert_eq!(
            asked.copies,
            vec![
                downloads.to_string_lossy().into_owned(),
                applications.to_string_lossy().into_owned()
            ]
        );
        assert_eq!(
            asked.home.as_deref().map(PathBuf::from).as_deref(),
            Some(session.home.home())
        );

        // Answered, both copies are in the Trash, the app is off their apps,
        // and the notice names both.
        session.answer(&handle, None);
        session.until("every copy to go", || {
            !applications.exists() && !downloads.exists()
        });
        session.until("the app to go", || session.apps().is_empty());
        let trashed = session.home.data().join("Trash/files");
        assert!(trashed.join("Tilo Imager.AppImage").is_file(), "the app");
        assert!(trashed.join("tilo-imager.AppImage").is_file(), "its copy");
        assert_eq!(
            session.notices(),
            [(
                "Uninstalled Tilo Imager".to_owned(),
                "All its copies are in the Trash.".to_owned()
            )]
        );
    }

    #[test]
    fn keeping_an_app_leaves_it_and_its_file_where_they_are() {
        let session = session(&[], &[]);
        let file = session
            .home
            .home()
            .join("Applications/Tilo Imager.AppImage");
        let desktop_id = app_at(
            &session.home,
            "tilo-imager",
            "Tilo Imager",
            std::slice::from_ref(&file),
        );
        session.until("the app", || !session.apps().is_empty());
        let reading = session.reading();
        session.sheets.ask_uninstall(&desktop_id, reading.as_ref());
        session.until("the question about the app", || {
            session.removing().is_some()
        });
        let (handle, _) = session.removing().expect("the question");
        session.decline(&handle);
        session.settle();
        assert!(session.removing().is_none(), "the question is answered");
        assert!(file.is_file(), "the app's file stays");
        assert_eq!(session.apps().len(), 1, "and so does the app");
        assert!(session.notices().is_empty(), "nothing to report");
    }

    #[test]
    fn asking_about_an_app_the_person_has_not_got_says_so() {
        let session = session(&[], &[]);
        session.settle();
        let reading = session.reading();
        session
            .sheets
            .ask_uninstall("tilo-downloaded-not-here", reading.as_ref());
        session.until("the notice", || !session.notices().is_empty());
        assert!(session.removing().is_none(), "nothing is asked");
        assert_eq!(
            session.notices(),
            [(
                "App not found".to_owned(),
                "It's no longer in your apps.".to_owned()
            )]
        );
    }
}
