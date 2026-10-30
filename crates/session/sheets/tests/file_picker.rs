//! The file chooser as a person, a screen reader and an agent meet it:
//! every step a published action, what it lists and answers reaching the
//! service through the seat, and giving files left to the person.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};

use tilo_sheets::Seat as _;
use tilo_sheets::file_picker::{
    FileAnswer, FileMode, FilePickerApp, FilePickerEffect, FilePickerSeat, FileRequest, FileType,
    Showing,
};
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 800);

/// A home with pictures, a folder of trips and a note.
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let pictures = home.path().join("Pictures");
    std::fs::create_dir_all(pictures.join("Trips")).unwrap();
    for name in ["beach.png", "forest.jpg", "notes.txt"] {
        std::fs::write(pictures.join(name), b"bytes").unwrap();
    }
    std::fs::write(pictures.join("Trips").join("alps.png"), b"bytes").unwrap();
    std::fs::create_dir_all(home.path().join("Documents")).unwrap();
    home
}

fn request(home: &Path, mode: FileMode) -> FileRequest {
    FileRequest {
        handle: "request-1".to_owned(),
        title: "Choose a picture".to_owned(),
        app_name: Some("Photos".to_owned()),
        app_id: Some("so.tilo.photos".to_owned()),
        mode,
        initial_dir: home.join("Pictures"),
        roots: vec![home.to_path_buf()],
        ..FileRequest::default()
    }
}

fn seated(request: FileRequest) -> (HeadlessApp<FilePickerApp>, FilePickerSeat) {
    let seat = FilePickerSeat::default();
    seat.presentation().open(true);
    seat.ask(Some(request));
    let mut app =
        HeadlessApp::<FilePickerApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    settled(&mut app, &seat);
    (app, seat)
}

/// Settle until the chooser has read what it was asked to.
fn settled(app: &mut HeadlessApp<FilePickerApp>, seat: &FilePickerSeat) {
    app.settle();
    tilo_test_env::wait_until("the chooser to read the folder", || {
        app.settle();
        !seat.listing().reading
    });
}

fn step(app: &mut HeadlessApp<FilePickerApp>, seat: &FilePickerSeat, effect: &FilePickerEffect) {
    app.effect(effect).unwrap();
    settled(app, seat);
}

fn names(seat: &FilePickerSeat) -> Vec<String> {
    seat.listing()
        .entries
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

fn path(seat: &FilePickerSeat, name: &str) -> String {
    seat.listing()
        .entries
        .into_iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("{name} is listed"))
        .path
        .to_string_lossy()
        .into_owned()
}

#[test]
fn every_scenario_draws_the_chooser_named_by_its_title() {
    use ui::framework::ResourceApp as _;
    for scenario in FilePickerApp::scenarios() {
        let mut app = HeadlessApp::<FilePickerApp>::capture_at(Some(scenario), VIEWPORT).unwrap();
        let pixels = app.harness().render_rgba_transparent();
        assert!(
            pixels.chunks(4).any(|pixel| pixel[3] > 0),
            "{scenario} draws"
        );
    }
}

#[test]
fn opening_a_file_lists_the_folder_and_answers_with_the_choice() {
    let home = home();
    let (mut app, seat) = seated(request(home.path(), FileMode::Open));
    let listing = seat.listing();
    assert_eq!(listing.folder, home.path().join("Pictures"));
    assert_eq!(listing.showing, Showing::Folder);
    for name in ["Trips", "beach.png", "forest.jpg", "notes.txt"] {
        assert!(names(&seat).contains(&name.to_owned()), "{name} is listed");
    }
    assert!(!listing.can_confirm, "nothing is chosen yet");
    // A reader meets the chooser and its files.
    assert!(app.harness().a11y_node_named("beach.png").is_some());

    let beach = path(&seat, "beach.png");
    step(
        &mut app,
        &seat,
        &FilePickerEffect::Select {
            path: beach.clone(),
        },
    );
    assert!(seat.listing().can_confirm);
    assert!(seat.take_answer().is_none(), "choosing is not answering");
    step(&mut app, &seat, &FilePickerEffect::Confirm);
    assert_eq!(
        seat.take_answer(),
        Some(FileAnswer::Chosen(vec![PathBuf::from(beach)]))
    );
    assert!(
        !seat.presentation().is_open(),
        "answering puts the sheet away"
    );
}

#[test]
fn folders_open_through_the_list_up_and_any_folder_in_reach() {
    let home = home();
    let (mut app, seat) = seated(request(home.path(), FileMode::Open));
    let trips = path(&seat, "Trips");
    step(&mut app, &seat, &FilePickerEffect::Navigate { path: trips });
    assert_eq!(names(&seat), ["alps.png"]);
    step(&mut app, &seat, &FilePickerEffect::Up);
    assert_eq!(seat.listing().folder, home.path().join("Pictures"));
    let documents = home.path().join("Documents");
    step(
        &mut app,
        &seat,
        &FilePickerEffect::Navigate {
            path: documents.to_string_lossy().into_owned(),
        },
    );
    assert_eq!(seat.listing().folder, documents);
    assert!(seat.listing().entries.is_empty());
}

#[test]
fn nothing_outside_the_folders_in_reach_is_opened_or_chosen() {
    let home = home();
    let (mut app, seat) = seated(request(home.path(), FileMode::Open));
    assert!(
        app.effect(&FilePickerEffect::Navigate {
            path: "/etc".to_owned()
        })
        .is_err()
    );
    assert!(
        app.effect(&FilePickerEffect::Select {
            path: "/etc/passwd".to_owned()
        })
        .is_err(),
        "only what is listed can be chosen"
    );
    let notes = path(&seat, "notes.txt");
    assert!(
        app.effect(&FilePickerEffect::Navigate { path: notes })
            .is_err(),
        "a file is not a folder"
    );
    // At the top of what it may reach, there is no Up.
    step(
        &mut app,
        &seat,
        &FilePickerEffect::Navigate {
            path: home.path().to_string_lossy().into_owned(),
        },
    );
    assert!(!seat.listing().can_up);
    step(&mut app, &seat, &FilePickerEffect::Up);
    assert!(seat.listing().refused.is_some(), "the refusal is said");
    assert_eq!(seat.listing().folder, home.path());
}

#[test]
fn several_files_are_chosen_one_by_one_when_the_app_takes_several() {
    let home = home();
    let (mut app, seat) = seated(FileRequest {
        multiple: true,
        ..request(home.path(), FileMode::Open)
    });
    let beach = path(&seat, "beach.png");
    let forest = path(&seat, "forest.jpg");
    step(
        &mut app,
        &seat,
        &FilePickerEffect::Select {
            path: beach.clone(),
        },
    );
    step(
        &mut app,
        &seat,
        &FilePickerEffect::AlsoSelect {
            path: forest.clone(),
        },
    );
    let chosen: Vec<String> = seat
        .listing()
        .entries
        .into_iter()
        .filter(|entry| entry.selected)
        .map(|entry| entry.name)
        .collect();
    assert_eq!(chosen, ["beach.png", "forest.jpg"]);
    step(&mut app, &seat, &FilePickerEffect::Confirm);
    let Some(FileAnswer::Chosen(mut paths)) = seat.take_answer() else {
        panic!("the chooser answers with the files")
    };
    paths.sort();
    assert_eq!(paths, [PathBuf::from(beach), PathBuf::from(forest)]);
}

#[test]
fn one_file_is_the_answer_when_the_app_takes_one() {
    let home = home();
    let (mut app, seat) = seated(request(home.path(), FileMode::Open));
    let beach = path(&seat, "beach.png");
    let forest = path(&seat, "forest.jpg");
    step(&mut app, &seat, &FilePickerEffect::Select { path: beach });
    step(
        &mut app,
        &seat,
        &FilePickerEffect::AlsoSelect {
            path: forest.clone(),
        },
    );
    step(&mut app, &seat, &FilePickerEffect::Confirm);
    assert_eq!(
        seat.take_answer(),
        Some(FileAnswer::Chosen(vec![PathBuf::from(forest)]))
    );
}

#[test]
fn a_type_narrows_the_list_and_all_files_widens_it() {
    let home = home();
    let (mut app, seat) = seated(FileRequest {
        filters: vec![
            FileType {
                label: "PNG".to_owned(),
                extensions: vec!["png".to_owned()],
            },
            FileType {
                label: "JPEG".to_owned(),
                extensions: vec!["jpg".to_owned()],
            },
        ],
        ..request(home.path(), FileMode::Open)
    });
    let listing = seat.listing();
    assert_eq!(
        listing.filters.len(),
        3,
        "the app's two types, then all files"
    );
    assert_eq!(listing.filter, 0);
    assert!(names(&seat).contains(&"beach.png".to_owned()));
    assert!(!names(&seat).contains(&"forest.jpg".to_owned()));
    step(&mut app, &seat, &FilePickerEffect::Filter { index: 1 });
    assert!(names(&seat).contains(&"forest.jpg".to_owned()));
    assert!(!names(&seat).contains(&"beach.png".to_owned()));
    step(&mut app, &seat, &FilePickerEffect::Filter { index: 2 });
    assert!(names(&seat).contains(&"notes.txt".to_owned()));
    step(&mut app, &seat, &FilePickerEffect::Filter { index: 9 });
    assert!(seat.listing().refused.is_some());
}

#[test]
fn saving_names_the_file_and_asks_before_replacing_one() {
    let home = home();
    let (mut app, seat) = seated(FileRequest {
        suggested_name: Some("Holiday.png".to_owned()),
        ..request(home.path(), FileMode::Save)
    });
    assert_eq!(seat.listing().filename, "Holiday.png");
    step(
        &mut app,
        &seat,
        &FilePickerEffect::SetFilename {
            name: "beach.png".to_owned(),
        },
    );
    step(&mut app, &seat, &FilePickerEffect::Confirm);
    let existing = home.path().join("Pictures").join("beach.png");
    assert_eq!(seat.listing().replace.as_deref(), Some(existing.as_path()));
    assert!(seat.take_answer().is_none(), "the person is asked first");
    step(&mut app, &seat, &FilePickerEffect::KeepExisting);
    assert!(seat.listing().replace.is_none());
    step(
        &mut app,
        &seat,
        &FilePickerEffect::SetFilename {
            name: "sunset.png".to_owned(),
        },
    );
    step(&mut app, &seat, &FilePickerEffect::Confirm);
    assert_eq!(
        seat.take_answer(),
        Some(FileAnswer::Chosen(vec![
            home.path().join("Pictures").join("sunset.png")
        ]))
    );
}

#[test]
fn replacing_answers_with_the_existing_file() {
    let home = home();
    let (mut app, seat) = seated(FileRequest {
        suggested_name: Some("beach.png".to_owned()),
        ..request(home.path(), FileMode::Save)
    });
    step(&mut app, &seat, &FilePickerEffect::Confirm);
    assert!(seat.listing().replace.is_some());
    step(&mut app, &seat, &FilePickerEffect::Replace);
    assert_eq!(
        seat.take_answer(),
        Some(FileAnswer::Chosen(vec![
            home.path().join("Pictures").join("beach.png")
        ]))
    );
}

#[test]
fn a_folder_is_chosen_without_opening_it() {
    let home = home();
    let (mut app, seat) = seated(request(home.path(), FileMode::Folder));
    assert_eq!(names(&seat), ["Trips"], "only folders are listed");
    let trips = path(&seat, "Trips");
    step(
        &mut app,
        &seat,
        &FilePickerEffect::Select {
            path: trips.clone(),
        },
    );
    step(&mut app, &seat, &FilePickerEffect::Confirm);
    assert_eq!(
        seat.take_answer(),
        Some(FileAnswer::Chosen(vec![PathBuf::from(trips)]))
    );
}

#[test]
fn recent_shows_what_the_app_was_given_before() {
    let home = home();
    let alps = home.path().join("Pictures").join("Trips").join("alps.png");
    let (mut app, seat) = seated(FileRequest {
        recent: vec![alps.clone()],
        ..request(home.path(), FileMode::Open)
    });
    step(&mut app, &seat, &FilePickerEffect::Recent);
    let listing = seat.listing();
    assert_eq!(listing.showing, Showing::Recent);
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>(),
        [alps]
    );
}

#[test]
fn cancelling_answers_nothing_and_a_new_request_starts_afresh() {
    let home = home();
    let (mut app, seat) = seated(request(home.path(), FileMode::Open));
    let trips = path(&seat, "Trips");
    step(&mut app, &seat, &FilePickerEffect::Navigate { path: trips });
    step(&mut app, &seat, &FilePickerEffect::Cancel);
    assert_eq!(seat.take_answer(), Some(FileAnswer::Cancelled));
    assert!(
        app.effect(&FilePickerEffect::Up).is_err() || seat.take_answer().is_none(),
        "an answered request takes no more steps"
    );
    seat.presentation().gone();
    seat.presentation().open(true);
    seat.ask(Some(FileRequest {
        handle: "request-2".to_owned(),
        ..request(home.path(), FileMode::Open)
    }));
    settled(&mut app, &seat);
    assert_eq!(seat.listing().folder, home.path().join("Pictures"));
    assert!(seat.take_answer().is_none());
}

#[test]
fn escape_cancels_as_the_person_backs_out() {
    let home = home();
    let (mut app, seat) = seated(request(home.path(), FileMode::Open));
    app.harness().key("Escape");
    settled(&mut app, &seat);
    assert_eq!(seat.take_answer(), Some(FileAnswer::Cancelled));
}

#[test]
fn only_the_person_gives_files_but_anyone_may_look_and_cancel() {
    use ui::tilo_resource::Effect as _;
    for declaration in FilePickerEffect::declarations() {
        let gives = matches!(declaration.id, "confirm" | "replace");
        assert_eq!(
            declaration.attended, gives,
            "only giving files needs the person: {}",
            declaration.id
        );
    }
}

#[test]
fn every_step_is_published() {
    let home = home();
    let (mut app, _seat) = seated(request(home.path(), FileMode::Open));
    let published: Vec<String> = app
        .actions()
        .unwrap()
        .into_iter()
        .map(|action| action.address.action)
        .collect();
    for id in [
        "navigate",
        "up",
        "select",
        "also-select",
        "set-filename",
        "filter",
        "search",
        "recent",
        "confirm",
        "replace",
        "keep-existing",
        "cancel",
    ] {
        assert!(
            published.iter().any(|action| action == id),
            "{id} is published: {published:?}"
        );
    }
}
