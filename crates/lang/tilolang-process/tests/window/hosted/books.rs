//! Books' shelf as its shots start it: the app's own scenario imports the walk's EPUB through
//! the library's own action, and the window lays the book out as one column of the shelf.
//!
//! A person who watches a folder picks it in the system's chooser, as every app reaches a
//! folder: the library keeps the reference the chooser answered, its EPUBs become books, and a
//! folder no chooser answered is neither read nor watched. A person who presses *Add a book*
//! picks the book itself in the chooser, on the connection the session opened for the app, and
//! the EPUB they answer is the one the library takes: its type is the book's own, and one they
//! dismiss runs nothing.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use tilo_dialogs::portal::{
    LaunchChannel, LaunchRequest, MultiChannel, OpenFileChannel, OpenFileOutcome, OpenedFile,
    PickRequest, PortalChannel,
};
use tilolang_tilo::runtime::Value;

use super::{BOOKS, Session};

/// The book a person drops in the folder they watch.
const TIDE_GLASS: &str =
    include_str!("../../../../tilolang-runtime/tests/fixtures/tideglass.epub.b64");
/// The book that arrives in it while it is watched.
const SALT_ROAD: &str =
    include_str!("../../../../tilolang-runtime/tests/fixtures/saltroad.epub.b64");

/// The bytes of the book `base64` holds.
fn epub(base64: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(base64.trim())
        .expect("the fixture is a base64 EPUB")
}

/// The laid-out card that says `name` to a reader, at any depth.
fn card(node: &ui::headless::InspectionNode, name: &str) -> Option<[f64; 4]> {
    let says = node
        .semantics
        .as_ref()
        .and_then(|semantics| semantics.name.as_deref())
        .is_some_and(|said| said == name);
    if says {
        return Some(node.rect);
    }
    node.children.iter().find_map(|child| card(child, name))
}

/// The person's chooser at the window: it answers the folder `root` for a folder question and
/// reopens what stands under it, as the session's chooser does for what the person chose. A
/// reference outside it is refused, in the chooser's own words, and kept as the attempt it was.
struct Chosen {
    root: std::path::PathBuf,
    asked: Mutex<Vec<PickRequest>>,
    refused: Mutex<Vec<std::path::PathBuf>>,
}

impl Chosen {
    /// A chooser whose person answers with the folder `root`.
    fn at(root: &std::path::Path) -> Arc<Self> {
        Arc::new(Self {
            root: root.to_path_buf(),
            asked: Mutex::new(Vec::new()),
            refused: Mutex::new(Vec::new()),
        })
    }

    /// What it was asked, oldest first.
    fn asked(&self) -> Vec<PickRequest> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The references outside the chosen folder it refused, oldest first.
    fn refused(&self) -> Vec<std::path::PathBuf> {
        self.refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// A channel no test reaches for: the questions this chooser never asks.
fn unanswered<T>() -> std::sync::mpsc::Receiver<T> {
    std::sync::mpsc::channel().1
}

impl tilo_dialogs::FilePicker for Chosen {
    fn pick(&self, _: PickRequest) -> PortalChannel {
        unanswered()
    }

    fn launch(&self, _: LaunchRequest) -> LaunchChannel {
        unanswered()
    }

    fn pick_multiple(&self, _: PickRequest) -> MultiChannel {
        unanswered()
    }

    fn open_handle(&self, request: PickRequest) -> OpenFileChannel {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        let (sender, receiver) = std::sync::mpsc::channel();
        let answer = if request.is_directory() {
            match tilo_dialogs::portal::open_directory_handle(&self.root) {
                Ok(folder) => OpenFileOutcome::Opened(OpenedFile::at(folder, &self.root)),
                Err(error) => OpenFileOutcome::Failed(error.to_string()),
            }
        } else {
            OpenFileOutcome::Failed("a folder is what this asks for".to_owned())
        };
        let _ = sender.send(answer);
        receiver
    }

    fn reopen(&self, reference: &std::path::Path, _write: bool) -> OpenFileChannel {
        let (sender, receiver) = std::sync::mpsc::channel();
        let answer = if reference.starts_with(&self.root) {
            match std::fs::File::open(reference) {
                Ok(file) => OpenFileOutcome::Opened(OpenedFile::at(file, reference)),
                Err(error) => OpenFileOutcome::Failed(error.to_string()),
            }
        } else {
            self.refused
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(reference.to_path_buf());
            OpenFileOutcome::Failed("This app can only open files you chose for it.".to_owned())
        };
        let _ = sender.send(answer);
        receiver
    }
}

/// A folder `name` under a temporary home, holding the EPUBs `books` names.
fn shelf(name: &str, books: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("a place to choose from");
    let folder = home.path().join(name);
    std::fs::create_dir(&folder).expect("the folder a person chooses");
    for (book, base64) in books {
        std::fs::write(folder.join(book), epub(base64)).expect("the book in the folder");
    }
    (home, folder)
}

impl Session {
    /// The books the library's root holds.
    fn books(&self) -> Vec<Value> {
        let root = self
            .instance_host()
            .root_of(BOOKS)
            .expect("the library's root");
        match self.instance_host().runtime().read(&root, "books") {
            Ok(Value::List(books)) => books,
            other => panic!("the root lists its books: {other:?}"),
        }
    }

    /// The folders the library watches.
    fn watched(&self) -> Vec<Value> {
        let root = self
            .instance_host()
            .root_of(BOOKS)
            .expect("the library's root");
        match self.instance_host().runtime().read(&root, "folders") {
            Ok(Value::List(folders)) => folders,
            other => panic!("the root lists its watched folders: {other:?}"),
        }
    }

    /// The person's frames until `done` holds, with the sleep a watcher's own wake needs: only
    /// the platform's watcher thread tells the host that the folder changed.
    fn until_books(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let began = std::time::Instant::now();
        loop {
            self.settle();
            if done(self) {
                return;
            }
            assert!(
                began.elapsed() < std::time::Duration::from_secs(120),
                "{what} did not happen"
            );
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }

    /// Press the control named `name` through its own accessibility click, wherever on the page
    /// it stands: the settings' folder control stands below what the window shows, and a press
    /// at a place the page clipped would land on nothing.
    fn press_anywhere(&mut self, name: &str) {
        let node = self
            .window
            .a11y()
            .into_iter()
            .find(|(_, node)| {
                node.label().map(str::trim) == Some(name)
                    && node.supports_action(ui::accesskit::Action::Click)
            })
            .unwrap_or_else(|| {
                let shown: Vec<String> = self
                    .window
                    .a11y()
                    .iter()
                    .filter_map(|(_, node)| node.label().map(|label| label.trim().to_owned()))
                    .collect();
                panic!("the window shows no {name:?} that clicks; it shows {shown:?}")
            })
            .0;
        self.window.a11y_action(node, &ui::a11y::Action::Activate);
    }

    /// Press the Settings segment, then the control that asks the person for a folder to watch.
    fn watch_a_folder(&mut self) {
        self.settle();
        self.press_anywhere("Settings");
        self.settle();
        self.press_anywhere("Watch a folder…");
        self.settle();
    }
}

// The library's scenario is the shelf the shots open on: the book is the one the app's walk
// opens from the person's files, taken in by the library's own import.
#[test]
fn the_shelves_scenario_puts_its_book_on_the_shelf() {
    let mut session = Session::new(BOOKS, "ada");
    session.stage("a shelf with a book");
    session.settle();
    let root = session
        .instance_host()
        .root_of(BOOKS)
        .expect("the library's root");
    let books = session
        .instance_host()
        .runtime()
        .read(&root, "books")
        .expect("the root lists its books");
    assert!(
        matches!(&books, tilolang_tilo::runtime::Value::List(books) if books.len() == 1),
        "the import put one book on the shelf: {books:?}"
    );
    let inspection = session.window.harness().inspection();
    let rect = card(&inspection.root, "The Tide Glass")
        .unwrap_or_else(|| panic!("the shelf lays the book out: {:#?}", inspection.root));
    let (width, _) = session.window.harness().device_size();
    assert!(
        rect[2] < f64::from(width) / 2.0,
        "the card keeps a column's width, not the whole shelf: {rect:?} in {width} px"
    );
    let rgba = session.window.harness().render_rgba();
    let mut lit = 0;
    for y in 60..400 {
        for x in 0..width.min(900) {
            let at = (y as usize * width as usize + x as usize) * 4;
            let (r, g, b) = (rgba[at], rgba[at + 1], rgba[at + 2]);
            let lum = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
            if lum > 110 {
                lit += 1;
            }
        }
    }
    assert!(lit > 100, "the card's words are painted: {lit} lit pixels");
}

/// The person presses *Watch a folder…*, picks a folder in the system's chooser, and its book
/// stands on the shelf; another book dropped in afterwards, while the folder is watched, becomes
/// a book too.
#[test]
fn a_watched_folder_chosen_in_the_chooser_gives_its_books_to_the_shelf() {
    let _settled = ui::motion::testing::settled();
    let (_home, folder) = shelf("Shelf", &[("The Tide Glass.epub", TIDE_GLASS)]);
    let chooser = Chosen::at(&folder);
    let picker: Arc<dyn tilo_dialogs::FilePicker> = chooser.clone();
    Session::choosing_files_with(&picker);
    let mut session = Session::new(BOOKS, "ada");

    session.watch_a_folder();

    // The chooser was asked for a folder, and the book in it stands on the shelf.
    let asked = chooser.asked();
    assert_eq!(asked.len(), 1, "the chooser was asked once");
    assert!(
        asked[0].is_directory(),
        "the person chooses a folder, not a file: {:?}",
        asked[0]
    );
    let books = session.books();
    assert_eq!(
        books.len(),
        1,
        "the watched folder's book is on it: {books:?}"
    );
    // The library keeps the folder the chooser answered, called as the person knows it.
    let watched = session.watched();
    assert_eq!(watched.len(), 1, "one folder is watched: {watched:?}");
    let Value::Ref(folder_record) = &watched[0] else {
        panic!("a watched folder is a resource: {:?}", watched[0]);
    };
    assert_eq!(
        session
            .instance_host()
            .runtime()
            .read(&folder_record.target, "name")
            .ok(),
        Some(Value::Text("Shelf".to_owned())),
        "the settings name the folder as the person chose it"
    );

    // The watch is the person's: the book shows on the shelf, one column wide.
    session.settle();
    session.window.press_named("Shelf");
    session.settle();
    let inspection = session.window.harness().inspection();
    let rect = card(&inspection.root, "The Tide Glass")
        .unwrap_or_else(|| panic!("the shelf lays the book out: {:#?}", inspection.root));
    let (width, _) = session.window.harness().device_size();
    assert!(
        rect[2] < f64::from(width) / 2.0,
        "the card keeps a column's width: {rect:?} in {width} px"
    );

    // A book arriving in the watched folder becomes a book of its own.
    std::fs::write(folder.join("The Salt Road.epub"), epub(SALT_ROAD)).expect("the arriving book");
    session.until_books("the arriving book to reach the shelf", |session| {
        session.books().len() == 2
    });
}

/// A folder the person did not choose is neither read nor watched: its books stay off the shelf
/// and no watch of it is kept.
#[test]
fn a_folder_the_person_did_not_choose_is_refused() {
    let _settled = ui::motion::testing::settled();
    let (_home, folder) = shelf("Shelf", &[("The Tide Glass.epub", TIDE_GLASS)]);
    let other = folder.parent().expect("the home").join("Elsewhere");
    std::fs::create_dir(&other).expect("the folder the person did not choose");
    std::fs::write(other.join("The Salt Road.epub"), epub(SALT_ROAD)).expect("its book");
    let chooser = Chosen::at(&folder);
    let picker: Arc<dyn tilo_dialogs::FilePicker> = chooser.clone();
    Session::choosing_files_with(&picker);
    let mut session = Session::new(BOOKS, "ada");
    session.watch_a_folder();
    assert_eq!(session.books().len(), 1, "the chosen folder's book");
    assert_eq!(session.watched().len(), 1, "the chosen folder's watch");

    let address = format!("tl:{BOOKS}:{}", super::root_id(&session, BOOKS));
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "path".to_owned(),
        serde_json::Value::String(other.to_string_lossy().into_owned()),
    );
    let scan = crate::change_marks::action_id(&mut session, &address, "Scan folder…");
    session
        .invoke(None, None, &address, &scan, &arguments)
        .expect("the scan runs, reading nothing");
    session.settle();
    assert_eq!(
        session.books().len(),
        1,
        "a folder the person did not choose gives no books"
    );

    let watch = crate::change_marks::action_id(&mut session, &address, "Watch folder…");
    session
        .invoke(None, None, &address, &watch, &arguments)
        .expect("the watch is refused without failing the action");
    session.settle();
    assert_eq!(
        session.watched().len(),
        1,
        "a folder the person did not choose is not watched"
    );
    assert!(
        chooser
            .refused()
            .iter()
            .any(|reference| reference == &other),
        "the chooser refused it: {:?}",
        chooser.refused()
    );
}

/// The session's chooser, over the protocol a session serves it on: the app asks for a file on
/// the connection it was handed, the person here answers, and what they chose is opened for the
/// app. The answers it gives, one per question, and the questions it was asked.
struct Servant {
    answers: Mutex<Vec<PersonAnswer>>,
    asked: Mutex<Vec<tilo_dialogs::chooser_service::Question>>,
}

/// What the person does with a question.
enum PersonAnswer {
    /// They choose these files.
    Files(Vec<std::path::PathBuf>),
    /// They dismiss the chooser.
    Cancelled,
}

impl Servant {
    fn new(answers: Vec<PersonAnswer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers),
            asked: Mutex::new(Vec::new()),
        })
    }

    /// The questions it was asked, oldest first.
    fn asked(&self) -> Vec<tilo_dialogs::chooser_service::Question> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl tilo_dialogs::chooser_service::Person for Servant {
    fn choose(
        &self,
        _caller: &tilo_dialogs::chooser_service::Caller,
        _handle: String,
        question: &tilo_dialogs::chooser_service::Question,
    ) -> Result<Vec<std::path::PathBuf>, tilo_dialogs::chooser_service::Asked> {
        use tilo_dialogs::chooser_service::Asked;
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(question.clone());
        let Some(answer) = self
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
        else {
            return Err(Asked::Failed);
        };
        match answer {
            PersonAnswer::Files(paths) => Ok(paths),
            PersonAnswer::Cancelled => Err(Asked::Cancelled),
        }
    }
}

/// What a choice gives the app: the path itself, as a test's app reads it in place. Nothing is
/// reopened by its reference, which a session keeps in its own table.
struct Kept;

impl tilo_dialogs::chooser_service::Grants for Kept {
    fn grant(
        &self,
        _caller: &tilo_dialogs::chooser_service::Caller,
        chosen: &std::path::Path,
        choice: tilo_dialogs::chooser_service::Choice,
    ) -> Result<tilo_dialogs::chooser_service::Granted, String> {
        Ok(tilo_dialogs::chooser_service::Granted {
            reference: chosen.to_path_buf(),
            name: chosen
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            host: chosen.to_path_buf(),
            choice,
            save_sibling: None,
        })
    }

    fn document(
        &self,
        _caller: &tilo_dialogs::chooser_service::Caller,
        _reference: &std::path::Path,
    ) -> Option<tilo_dialogs::chooser_service::Granted> {
        None
    }
}

/// A session that opens no link for a chosen file.
struct OpensNothing;

impl tilo_dialogs::chooser_service::Opens for OpensNothing {
    fn open(&self, _workspace: &str, _uri: &str) -> Result<Option<()>, String> {
        Ok(None)
    }
}

/// A chooser the app reaches over a connection the session opened for it, as a session serves
/// one: the session's own end answers here.
fn served_chooser(person: &Arc<Servant>) -> Arc<dyn tilo_dialogs::FilePicker> {
    use std::io::Write as _;
    use tilo_dialogs::chooser_service::{Opens, Person, Serving};

    let (person_end, app_end) = std::os::unix::net::UnixStream::pair().expect("a chooser pair");
    let serving = Arc::new(Serving {
        person: Some(Arc::clone(person) as Arc<dyn Person>),
        grants: Arc::new(Kept),
        opens: Arc::new(OpensNothing) as Arc<dyn Opens>,
    });
    std::thread::Builder::new()
        .name("test-chooser".into())
        .spawn(move || tilo_dialogs::chooser_service::serve_connection(serving, person_end))
        .expect("the chooser's thread");
    // The launcher names the app on the connection before the app reads a word of it.
    let mut app_end = app_end;
    runtime_portal::session_services::write_chooser_handshake(&mut app_end, BOOKS, "workspace")
        .and_then(|()| app_end.flush())
        .expect("the chooser's handshake");
    tilo_dialogs::shell::picker(app_end, tilo_dialogs::unavailable_picker())
        .expect("the person's chooser")
}

/// A press that asks for a file reaches the session's chooser on the connection the session
/// handed the app, and the file the person picked there is the one the app's action runs with:
/// the book they chose stands on the shelf.
#[test]
fn a_press_that_asks_for_a_file_reaches_the_sessions_chooser_and_uses_what_it_answered() {
    let _settled = ui::motion::testing::settled();
    let (_home, folder) = shelf("Shelf", &[("The Tide Glass.epub", TIDE_GLASS)]);
    let book = folder.join("The Tide Glass.epub");
    let person = Servant::new(vec![PersonAnswer::Files(vec![book])]);
    let picker: Arc<dyn tilo_dialogs::FilePicker> = served_chooser(&person);
    Session::choosing_files_with(&picker);
    let mut session = Session::new(BOOKS, "ada");

    session.press_anywhere("Add a book");
    session.settle();

    let asked = person.asked();
    assert_eq!(asked.len(), 1, "the person is asked once: {asked:?}");
    assert!(
        !asked[0].directory && !asked[0].save,
        "the question asks for a file to open: {:?}",
        asked[0]
    );
    let books = session.books();
    assert_eq!(
        books.len(),
        1,
        "the book they chose is on the shelf: {books:?}"
    );
    let inspection = session.window.harness().inspection();
    assert!(
        card(&inspection.root, "The Tide Glass").is_some(),
        "the shelf lays the chosen book out: {:#?}",
        inspection.root
    );
}

/// The person dismisses the chooser: the app runs nothing, and nothing is said against it.
#[test]
fn a_pressed_file_the_person_cancels_runs_nothing() {
    let _settled = ui::motion::testing::settled();
    let person = Servant::new(vec![PersonAnswer::Cancelled]);
    let picker: Arc<dyn tilo_dialogs::FilePicker> = served_chooser(&person);
    Session::choosing_files_with(&picker);
    let mut session = Session::new(BOOKS, "ada");

    session.press_anywhere("Add a book");
    session.settle();

    assert_eq!(person.asked().len(), 1, "the person was asked");
    assert!(
        session.books().is_empty(),
        "a cancelled choice adds no book: {:?}",
        session.books()
    );
}
