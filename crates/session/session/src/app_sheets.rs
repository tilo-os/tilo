//! The questions about the person's apps, on their sheets: whether to add a
//! program they downloaded to their apps ([`tilo_sheets::add_app`]), and
//! whether to uninstall one ([`tilo_sheets::uninstall`]).
//!
//! Both are about what the registry of the person's downloaded programs
//! ([`tilo_app_launch::downloaded`]) keeps, and both are answered through
//! it: the reading lists a program running with no entry as an offer, so a
//! program is asked about once while it is offered, and the answer is the
//! registry's own action. Nothing here adds or uninstalls an app by itself:
//! everything is asked first, and whether it happened is read off the
//! registry again rather than assumed.

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::remote::downloaded_apps::{self as wire, DownloadedApps};
use tilo_app_launch::downloaded;
use tilo_desktop_surface::DesktopNotifications;
use tilo_desktop_surface::notifications::Notification;
use tilo_service::Services;
use tilo_sheets::Seat as _;
use tilo_sheets::add_app::{AddAppEffect, AddSeat, Offer};
use tilo_sheets::uninstall::{App, Going, Removal, UninstallEffect, UninstallSeat};

use crate::i18n::{t, t_args};
use ui::i18n::ArgValue;

/// The two app sheets, and what they were answered.
pub(crate) struct AppSheets {
    /// The add sheet's seat; `None` when the sheet did not mount.
    add: Option<AddSeat>,
    /// The uninstall sheet's seat; `None` when the sheet did not mount.
    uninstall: Option<UninstallSeat>,
    /// The registry's actions and reading, as the session publishes them.
    services: Rc<RefCell<Services>>,
    /// The desktop's notices: what an answer did.
    notices: DesktopNotifications,
    /// The files the add sheet asked about, so a program that keeps running
    /// is asked about once. A file the registry no longer offers is
    /// forgotten, so a program whose app was taken off the apps is asked
    /// about again when it runs.
    asked: RefCell<BTreeSet<String>>,
    /// The apps the person asked to uninstall, waiting for the sheet; they
    /// are asked about one at a time.
    wanted: RefCell<VecDeque<String>>,
    /// The uninstalls answered, until the registry's reading shows them
    /// gone.
    uninstalling: RefCell<Vec<Uninstalling>>,
}

/// An app the person said to uninstall, and what the sheet said would go.
#[derive(Clone, Debug)]
struct Uninstalling {
    desktop_id: String,
    name: String,
    removal: Removal,
    going: Going,
}

impl AppSheets {
    pub(crate) fn new(
        add: Option<AddSeat>,
        uninstall: Option<UninstallSeat>,
        services: Rc<RefCell<Services>>,
        notices: DesktopNotifications,
    ) -> Self {
        Self {
            add,
            uninstall,
            services,
            notices,
            asked: RefCell::new(BTreeSet::new()),
            wanted: RefCell::new(VecDeque::new()),
            uninstalling: RefCell::new(Vec::new()),
        }
    }

    /// The add sheet's seat: whether it is up and what it asks; `None` when
    /// the sheet did not mount.
    pub(crate) fn add(&self) -> Option<&AddSeat> {
        self.add.as_ref()
    }

    /// The uninstall sheet's seat; `None` when the sheet did not mount.
    pub(crate) fn uninstall(&self) -> Option<&UninstallSeat> {
        self.uninstall.as_ref()
    }

    /// Ask the person whether to uninstall the app whose entry is
    /// `desktop_id`: the question stands on the uninstall sheet until they
    /// answer it.
    pub(crate) fn ask_uninstall(&self, desktop_id: &str) {
        let mut wanted = self.wanted.borrow_mut();
        if !wanted.iter().any(|queued| queued == desktop_id) {
            wanted.push_back(desktop_id.to_owned());
        }
    }

    /// Carry out what the person answered on either sheet, ask the next
    /// question, and say what a finished uninstall did.
    pub(crate) fn follow(&self) {
        let reading = self.reading();
        self.follow_add(reading.as_ref());
        self.follow_uninstall(reading.as_ref());
    }

    /// The registry's reading now: the apps the person has and the programs
    /// running with no entry; `None` when the session publishes no registry.
    fn reading(&self) -> Option<DownloadedApps> {
        self.services
            .borrow()
            .read(downloaded::RESOURCE)
            .and_then(|reading| serde_json::from_value(reading).ok())
    }

    /// Whether a question about the person's apps is up: they are asked one
    /// at a time, in front of what they are doing.
    fn standing(&self) -> bool {
        self.add
            .as_ref()
            .is_some_and(|seat| seat.presentation().is_open())
            || self
                .uninstall
                .as_ref()
                .is_some_and(|seat| seat.presentation().is_open())
    }

    /// Add programs: the answer to the add sheet, then the next program the
    /// registry offers that the person has not been asked about.
    fn follow_add(&self, reading: Option<&DownloadedApps>) {
        let Some(seat) = &self.add else {
            return;
        };
        let offers: &[wire::DownloadedOffer] = reading.map_or(&[], |reading| &reading.offers);
        // A file the registry no longer offers is asked about again when it
        // runs, as an app taken off the apps comes back as a program with
        // no entry.
        self.asked
            .borrow_mut()
            .retain(|file| offers.iter().any(|offer| offer.file == *file));
        for effect in seat.take_asked() {
            // The answer is about the program the sheet showed.
            let Some(offer) = seat.offer() else {
                continue;
            };
            let file = offer.file.to_string_lossy().into_owned();
            seat.presentation().close();
            seat.presentation().gone();
            self.asked.borrow_mut().insert(file.clone());
            let mut arguments = Map::new();
            arguments.insert(wire::FILE_PARAM.to_owned(), Value::from(file));
            match effect {
                AddAppEffect::Add {
                    move_to_applications,
                } => {
                    if let Some(moved) = move_to_applications {
                        arguments.insert(wire::MOVE_PARAM.to_owned(), Value::from(moved));
                    }
                    self.invoke(downloaded::RESOURCE, wire::ADD, &arguments, false);
                }
                AddAppEffect::Decline => {
                    self.invoke(downloaded::RESOURCE, wire::DECLINE, &arguments, false);
                }
            }
        }
        if self.standing() {
            return;
        }
        let Some(offer) = offers
            .iter()
            .find(|offer| !offer.declined && !self.asked.borrow().contains(&offer.file))
        else {
            return;
        };
        seat.show(offer_of(offer));
        seat.presentation().open(false);
    }

    /// Uninstall an app: the answer to the uninstall sheet, what the reading
    /// says came of an earlier one, then the next app the person asked
    /// about.
    fn follow_uninstall(&self, reading: Option<&DownloadedApps>) {
        let Some(seat) = &self.uninstall else {
            return;
        };
        for effect in seat.take_asked() {
            let Some(app) = seat.app() else {
                continue;
            };
            seat.presentation().close();
            seat.presentation().gone();
            if let UninstallEffect::Confirm { .. } = effect {
                let (action, confirmed) = match app.removal {
                    Removal::Uninstall => (wire::UNINSTALL, true),
                    Removal::FromApps => (wire::REMOVE, false),
                };
                self.uninstalling.borrow_mut().push(Uninstalling {
                    desktop_id: app.desktop_id.clone(),
                    name: app.name.clone(),
                    removal: app.removal,
                    going: app.going.clone(),
                });
                self.invoke(&wire::app(&app.desktop_id), action, &Map::new(), confirmed);
            }
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
        if self.standing() {
            return;
        }
        let Some(desktop_id) = self.wanted.borrow_mut().pop_front() else {
            return;
        };
        match question(reading, &desktop_id) {
            Some(app) => {
                seat.show(app);
                seat.presentation().open(false);
            }
            None => {
                self.notices.post(
                    Notification::new(t("app-list-gone-title"), t("app-list-gone-body"))
                        .with_app_name(t("app-list-app-name")),
                );
            }
        }
    }

    /// Invoke one of the registry's actions on `resource`: the same route
    /// [`crate::Session::invoke`] and [`crate::Session::invoke_confirmed`]
    /// take, because the person answered the question the action's contract
    /// asks to confirm.
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

    /// Say what a finished uninstall did, as the question said what would
    /// go.
    fn tell_uninstalled(&self, pending: &Uninstalling) {
        let name = ArgValue::Text(pending.name.clone());
        let (title, body) = match pending.removal {
            Removal::FromApps => (t_args("app-list-removed", &[("name", name)]), String::new()),
            Removal::Uninstall => {
                let title = t_args("app-list-uninstalled", &[("name", name)]);
                let body = match &pending.going {
                    Going::Files { copies, .. } if copies.len() > 1 => {
                        t("app-list-uninstalled-files")
                    }
                    Going::Files { .. } => t("app-list-uninstalled-file"),
                    Going::Data { .. } | Going::Entry | Going::Documents => String::new(),
                };
                (title, body)
            }
        };
        self.notices
            .post(Notification::new(title, body).with_app_name(t("app-list-app-name")));
    }
}

/// What the uninstall sheet is asked: the app whose entry is `desktop_id`,
/// and every copy of it that would go to the Trash.
fn question(reading: Option<&DownloadedApps>, desktop_id: &str) -> Option<App> {
    let app = reading?
        .apps
        .iter()
        .find(|app| app.desktop_id == desktop_id)?;
    let copies: Vec<PathBuf> = if app.copies.is_empty() {
        vec![PathBuf::from(&app.file)]
    } else {
        app.copies.iter().map(PathBuf::from).collect()
    };
    Some(App {
        desktop_id: app.desktop_id.clone(),
        name: app.name.clone(),
        removal: Removal::Uninstall,
        going: Going::Files {
            copies,
            home: tilo_paths::home(),
        },
    })
}

/// The program an offer asks about, as the add sheet shows it.
fn offer_of(offer: &wire::DownloadedOffer) -> Offer {
    Offer {
        file: PathBuf::from(&offer.file),
        name: offer.name.clone(),
        appimage: offer.kind == wire::DownloadedKind::AppImage,
        artwork: offer.artwork.as_deref().map(PathBuf::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, Instant};

    use desktop_entry::downloaded::{Identity, ProcessFacts, Program, ProgramKind, Store};
    use tilo_app_launch::downloaded::{Downloaded, ObservedWindow};
    use tilo_test_env::TestHome;

    /// A type 2 `AppImage`'s identification: an ELF executable whose padding
    /// says `AI\x02`. The image carries no name of its own, so it is named
    /// after its file.
    fn appimage() -> Vec<u8> {
        let mut bytes = vec![0_u8; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[8..11].copy_from_slice(b"AI\x02");
        bytes
    }

    /// The session's side of the app sheets: the registry of the person's
    /// downloaded programs, the two sheets' seats, and the desk's notices.
    struct Desk {
        sheets: AppSheets,
        services: Rc<RefCell<Services>>,
        notices: DesktopNotifications,
        add: AddSeat,
        uninstall: UninstallSeat,
        home: TestHome,
    }

    /// A session whose person has `files` in their home (path, contents) and
    /// is running `running` programs (the window's app id, the file in their
    /// home it runs).
    fn desk(files: &[(&str, &[u8])], running: &[(&str, &str)]) -> Desk {
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
        let notices = DesktopNotifications::default();
        let add = AddSeat::default();
        let uninstall = UninstallSeat::default();
        let sheets = AppSheets::new(
            Some(add.clone()),
            Some(uninstall.clone()),
            Rc::clone(&services),
            notices.clone(),
        );
        Desk {
            sheets,
            services,
            notices,
            add,
            uninstall,
            home,
        }
    }

    /// A runnable file at `path`, with `contents`.
    fn write(path: &std::path::Path, contents: &[u8]) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("a folder");
        std::fs::write(path, contents).expect("a file");
        let mut permissions = std::fs::metadata(path).expect("the file").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("a runnable file");
    }

    /// The app `name`, made of `copies` (the first is the one it starts).
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
        desktop_id
    }

    impl Desk {
        /// Frames of the session's loop: what the registry's worker found
        /// lands, is read, and the sheets follow it.
        fn settle(&self) {
            for _ in 0..4 {
                self.services.borrow_mut().settle();
                self.services.borrow_mut().poll();
                self.sheets.follow();
            }
        }

        /// Settle until `condition`, which the registry answers on its own
        /// thread or through the sheet the person answered.
        fn until(&self, what: &str, mut condition: impl FnMut() -> bool) {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(60) {
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

        /// The offers standing now.
        fn offers(&self) -> Vec<wire::DownloadedOffer> {
            self.reading()
                .map(|reading| reading.offers)
                .unwrap_or_default()
        }

        /// The titles and bodies of the notices the desk shows now.
        fn notices(&self) -> Vec<(String, String)> {
            self.notices
                .log()
                .active()
                .iter()
                .map(|notice| (notice.title.clone(), notice.body.clone()))
                .collect()
        }
    }

    #[test]
    fn a_program_running_with_no_entry_is_asked_about_and_adding_it_puts_it_in_apps() {
        let desk = desk(
            &[("Downloads/Krita.AppImage", &appimage())],
            &[("org.example.Krita", "Downloads/Krita.AppImage")],
        );
        desk.until("the question about the program", || {
            desk.add.offer().is_some()
        });

        // The question names the program and the file it runs from, and
        // offers to move it, since an AppImage is one self-contained file.
        let asked = desk.add.offer().expect("the question");
        assert_eq!(asked.name, "Krita");
        assert_eq!(
            asked.file,
            desk.home.home().join("Downloads/Krita.AppImage")
        );
        assert!(asked.appimage);
        assert!(desk.add.offers_move());

        // Answered with the move, the program becomes an app in
        // Applications.
        desk.add.answer(AddAppEffect::Add {
            move_to_applications: Some(true),
        });
        desk.until("the app", || !desk.apps().is_empty());
        let moved = desk.home.home().join("Applications/Krita.AppImage");
        assert!(moved.is_file(), "the AppImage is in Applications");
        assert!(
            !desk.home.home().join("Downloads/Krita.AppImage").exists(),
            "and no longer in Downloads"
        );
        let app = desk.apps().pop().expect("the app");
        assert_eq!(app.name, "Krita");
        assert_eq!(PathBuf::from(&app.file), moved);
        assert!(
            desk.offers().is_empty(),
            "the question was answered, so there is no offer left"
        );
        assert!(
            !desk.add.presentation().is_drawn(),
            "and the question is taken down"
        );
    }

    #[test]
    fn adding_where_the_file_is_leaves_it_where_the_person_downloaded_it() {
        let desk = desk(
            &[("Downloads/Krita.AppImage", &appimage())],
            &[("org.example.Krita", "Downloads/Krita.AppImage")],
        );
        desk.until("the question about the program", || {
            desk.add.offer().is_some()
        });
        desk.add.answer(AddAppEffect::Add {
            move_to_applications: None,
        });
        desk.until("the app", || !desk.apps().is_empty());
        let downloaded = desk.home.home().join("Downloads/Krita.AppImage");
        assert!(downloaded.is_file(), "the file stays where it was");
        assert_eq!(
            PathBuf::from(&desk.apps()[0].file),
            downloaded,
            "the app starts the file the person downloaded"
        );
    }

    #[test]
    fn not_adding_it_keeps_it_off_the_apps_and_is_not_asked_again() {
        let desk = desk(
            &[("Downloads/Krita.AppImage", &appimage())],
            &[("org.example.Krita", "Downloads/Krita.AppImage")],
        );
        desk.until("the question about the program", || {
            desk.add.offer().is_some()
        });
        desk.add.answer(AddAppEffect::Decline);
        desk.until("the answer", || {
            desk.offers().first().is_some_and(|offer| offer.declined)
        });

        // It is no app, its file stays, and the offer stays marked so search
        // can still add it.
        assert!(desk.apps().is_empty(), "nothing was added");
        assert!(desk.home.home().join("Downloads/Krita.AppImage").is_file());
        assert!(!desk.add.presentation().is_drawn(), "the question is gone");

        // It keeps running, and it is not asked about again.
        for _ in 0..4 {
            desk.settle();
        }
        assert!(!desk.add.presentation().is_open(), "asked only once");
    }

    #[test]
    fn one_question_stands_at_a_time_and_the_next_program_follows_it() {
        let desk = desk(
            &[
                ("Downloads/Krita.AppImage", &appimage()),
                ("Downloads/Inkscape.AppImage", &appimage()),
            ],
            &[
                ("org.example.Krita", "Downloads/Krita.AppImage"),
                ("org.example.Inkscape", "Downloads/Inkscape.AppImage"),
            ],
        );
        desk.until("both programs offered", || desk.offers().len() == 2);
        desk.settle();
        let first = desk.add.offer().expect("one of them is asked about");
        assert!(
            desk.offers().iter().any(|offer| offer.name == first.name),
            "the question is about one of the programs offered"
        );

        // Answering it, the other program is asked about; never both at once.
        desk.add.answer(AddAppEffect::Decline);
        desk.until("the other program", || {
            desk.add.offer().map(|offer| offer.name) != Some(first.name.clone())
        });
        let second = desk.add.offer().expect("the other program");
        assert_ne!(second.name, first.name, "the question moved on");
        assert!(desk.add.presentation().is_open(), "and it is shown");
    }

    #[test]
    fn an_app_the_person_uninstalls_goes_with_every_copy_and_the_notice_says_so() {
        let desk = desk(&[], &[]);
        let applications = desk.home.home().join("Applications/Tilo Imager.AppImage");
        let downloads = desk.home.home().join("Downloads/tilo-imager.AppImage");
        let desktop_id = app_at(
            &desk.home,
            "tilo-imager",
            "Tilo Imager",
            &[applications.clone(), downloads.clone()],
        );
        desk.until("the app", || !desk.apps().is_empty());

        // The question lists every copy that goes, and where they are.
        desk.sheets.ask_uninstall(&desktop_id);
        desk.until("the question about the app", || {
            desk.uninstall.app().is_some()
        });
        let asked = desk.uninstall.app().expect("the question");
        assert_eq!(asked.name, "Tilo Imager");
        assert_eq!(asked.removal, Removal::Uninstall);
        let Going::Files { copies, home } = &asked.going else {
            panic!("a downloaded program goes with its files");
        };
        assert_eq!(*copies, vec![downloads.clone(), applications.clone()]);
        assert_eq!(home.as_deref(), Some(desk.home.home()));

        // Answered, both copies are in the Trash, the app is off their apps,
        // and the notice names both.
        desk.uninstall
            .answer(UninstallEffect::Confirm { remove_data: None });
        desk.until("every copy to go", || {
            !applications.exists() && !downloads.exists()
        });
        desk.until("the app to go", || desk.apps().is_empty());
        let trashed = desk.home.data().join("Trash/files");
        assert!(trashed.join("Tilo Imager.AppImage").is_file(), "the app");
        assert!(trashed.join("tilo-imager.AppImage").is_file(), "its copy");
        assert_eq!(
            desk.notices(),
            [(
                "Uninstalled Tilo Imager".to_owned(),
                "All its copies are in the Trash.".to_owned()
            )]
        );
    }

    #[test]
    fn keeping_an_app_leaves_it_and_its_file_where_they_are() {
        let desk = desk(&[], &[]);
        let file = desk.home.home().join("Applications/Tilo Imager.AppImage");
        let desktop_id = app_at(
            &desk.home,
            "tilo-imager",
            "Tilo Imager",
            std::slice::from_ref(&file),
        );
        desk.until("the app", || !desk.apps().is_empty());
        desk.sheets.ask_uninstall(&desktop_id);
        desk.until("the question about the app", || {
            desk.uninstall.app().is_some()
        });
        desk.uninstall.answer(UninstallEffect::Cancel);
        desk.settle();
        assert!(
            !desk.uninstall.presentation().is_drawn(),
            "the question goes"
        );
        assert!(file.is_file(), "the app's file stays");
        assert_eq!(desk.apps().len(), 1, "and so does the app");
        assert!(desk.notices().is_empty(), "nothing to report");
    }

    #[test]
    fn asking_about_an_app_the_person_has_not_got_says_so() {
        let desk = desk(&[], &[]);
        desk.settle();
        desk.sheets.ask_uninstall("tilo-downloaded-not-here");
        desk.until("the notice", || !desk.notices().is_empty());
        assert!(
            !desk.uninstall.presentation().is_drawn(),
            "nothing is asked"
        );
        assert_eq!(
            desk.notices(),
            [(
                "App not found".to_owned(),
                "It's no longer in your apps.".to_owned()
            )]
        );
    }
}
