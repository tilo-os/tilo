//! What a person gets when a program they downloaded starts: the offer to
//! add it, the app it becomes with its own artwork, the entry that says its
//! file is gone, and the ways back — as the session's registry serves them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use desktop_entry::downloaded::ProcessFacts;
use serde_json::{Map, Value};
use tilo_actions::remote::downloaded_apps as wire;
use tilo_app_launch::downloaded::{Downloaded, ObservedWindow, RESOURCE};
use tilo_service::Service;
use tilo_test_env::TestEnv;

/// The first eight bytes of a PNG, which is all an icon's format is read
/// from.
const PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00";

/// A type 2 `AppImage`'s identification: an ELF executable whose padding
/// says `AI\x02`, with its section headers ending the ELF. The image itself
/// is not there, so it carries no name of its own.
fn appimage() -> Vec<u8> {
    let mut bytes = vec![0_u8; 64];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[8..11].copy_from_slice(b"AI\x02");
    bytes
}

/// A program from a tarball: the file that runs, and the entry and artwork
/// the archive shipped beside it.
fn tarball() -> Vec<(PathBuf, Vec<u8>)> {
    vec![
        (
            PathBuf::from("Downloads/Dropped/Dropped.AppImage"),
            appimage(),
        ),
        (
            PathBuf::from("Downloads/Dropped/Dropped.desktop"),
            b"[Desktop Entry]\nType=Application\nName=Dropped Tool\nComment=Unpacks and runs\nExec=Dropped.AppImage %U\nIcon=Dropped\nStartupWMClass=org.example.Dropped\n"
                .to_vec(),
        ),
        (PathBuf::from("Downloads/Dropped/Dropped.png"), PNG.to_vec()),
    ]
}

/// A test environment holding the files `tree` (`(path in the home,
/// contents)`) and every XDG directory under it.
fn environment(tree: &[(PathBuf, Vec<u8>)]) -> TestEnv {
    let env = TestEnv::new().expect("a test environment");
    for (path, contents) in tree {
        let path = env.join(path);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a folder");
        std::fs::write(&path, contents).expect("a file");
    }
    env
}

/// The registry of this environment's person, as the session starts it.
fn registry() -> Downloaded {
    Downloaded::start(None).expect("a worker")
}

/// The window a program opened, with its process's facts already read.
fn window(id: u64, app_id: &str, program: &Path) -> (ObservedWindow, ProcessFacts) {
    (
        ObservedWindow {
            id,
            app_id: app_id.to_owned(),
            pid: Some(4_000 + id as u32),
        },
        ProcessFacts {
            exe: program.to_path_buf(),
            appimage: Some(program.to_path_buf()),
            parent_script: None,
        },
    )
}

fn offer_at(want: &Path, service: &mut Downloaded) -> wire::DownloadedOffer {
    let root = read(service);
    root.offers
        .into_iter()
        .find(|offer| Path::new(&offer.file) == want)
        .unwrap_or_else(|| panic!("no offer for {}", want.display()))
}

fn apps(service: &mut Downloaded) -> Vec<wire::DownloadedApp> {
    read(service).apps
}

fn read(service: &mut Downloaded) -> wire::DownloadedApps {
    // As the session does: take in what the worker and the directory watches
    // found, then read.
    service.poll();
    service.settle();
    let value = service.read(RESOURCE).expect("the registry's reading");
    serde_json::from_value(value).expect("a reading")
}

/// Wait for `condition`, which a worker or a directory watch answers on its
/// own thread.
fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{what} did not happen");
}

fn arguments(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect()
}

/// Make `path` runnable, as a program the person downloaded is.
fn executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(path).expect("the file").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("a runnable file");
}

/// An `AppImage` dropped in the person's Downloads is offered with the name
/// it gives itself, and adding it where they said moves it into their
/// Applications folder.
#[test]
fn an_appimage_dropped_in_downloads_is_offered_and_added_to_applications() {
    let env = environment(&[(PathBuf::from("Downloads/Dropped.AppImage"), appimage())]);
    let dropped = env.join("Downloads/Dropped.AppImage");
    let mut service = registry();
    let (observed, facts) = window(1, "org.example.Dropped", &dropped);
    service.handle().observed(observed, facts);

    let offer = offer_at(&dropped, &mut service);
    assert_eq!(offer.name, "Dropped");
    assert_eq!(offer.kind, wire::DownloadedKind::AppImage);
    assert!(!offer.declined);
    assert!(apps(&mut service).is_empty(), "nothing is added by itself");

    service
        .invoke(
            RESOURCE,
            wire::ADD,
            &arguments(&[
                (
                    wire::FILE_PARAM,
                    Value::String(dropped.display().to_string()),
                ),
                (wire::MOVE_PARAM, Value::Bool(true)),
            ]),
        )
        .expect("add is offered");
    let moved = env.join("Applications/Dropped.AppImage");
    until("the program is in Applications", || moved.is_file());
    let app = apps(&mut service).pop().expect("the added app");
    assert_eq!(app.name, "Dropped");
    assert_eq!(Path::new(&app.file), moved);
    assert!(app.present);
    assert_eq!(app.launch, format!("host:{}", app.desktop_id));
    assert!(
        read(&mut service).offers.is_empty(),
        "the offer is answered"
    );
}

/// A program from a tarball is offered with the artwork its folder brings,
/// and the app it becomes shows that artwork.
#[test]
fn an_added_program_shows_its_artwork() {
    let env = environment(&tarball());
    let program = env.join("Downloads/Dropped/Dropped.AppImage");
    let mut service = registry();
    let (observed, facts) = window(2, "org.example.Dropped", &program);
    service.handle().observed(observed, facts);

    let offer = offer_at(&program, &mut service);
    assert_eq!(offer.name, "Dropped Tool");
    assert_eq!(offer.kind, wire::DownloadedKind::AppImage);
    let artwork = offer
        .artwork
        .clone()
        .expect("the artwork its folder brings");
    assert!(Path::new(&artwork).is_file(), "{artwork} is a file");

    service
        .invoke(
            RESOURCE,
            wire::ADD,
            &arguments(&[(
                wire::FILE_PARAM,
                Value::String(program.display().to_string()),
            )]),
        )
        .expect("add is offered");
    let app = apps(&mut service).pop().expect("the added app");
    assert_eq!(app.name, "Dropped Tool");
    assert_eq!(app.comment.as_deref(), Some("Unpacks and runs"));
    assert_eq!(app.artwork.as_deref(), Some(artwork.as_str()));
    assert!(Path::new(app.artwork.as_deref().expect("artwork")).is_file());
    assert_eq!(app.copies, vec![app.file.clone()]);
    // What the entry itself says, not just the reading.
    let entry = std::fs::read_to_string(
        desktop_entry::downloaded::Store::in_data_home(env.data())
            .entry(&app.desktop_id)
            .expect("the entry Tilo wrote")
            .path,
    )
    .expect("the entry Tilo wrote");
    assert!(entry.contains(&format!("Icon={artwork}")), "{entry}");
}

/// Deleting the file behind an app reads as the file being gone, and
/// pointing the entry at the file where it is now brings the app back.
#[test]
fn a_deleted_file_reads_missing_until_the_person_finds_it() {
    let env = environment(&tarball());
    let program = env.join("Downloads/Dropped/Dropped.AppImage");
    let mut service = registry();
    let (observed, facts) = window(3, "org.example.Dropped", &program);
    service.handle().observed(observed, facts);
    service
        .invoke(
            RESOURCE,
            wire::ADD,
            &arguments(&[(
                wire::FILE_PARAM,
                Value::String(program.display().to_string()),
            )]),
        )
        .expect("add is offered");
    assert!(apps(&mut service).pop().expect("the added app").present);

    std::fs::remove_file(&program).expect("delete the program");
    until("the entry says its file is gone", || {
        apps(&mut service).first().is_some_and(|app| !app.present)
    });
    let app = apps(&mut service).pop().expect("the app");
    let resource = service
        .resources()
        .into_iter()
        .flat_map(|resource| resource.children)
        .find(|child| child.id.as_str() == wire::app(&app.desktop_id))
        .expect("the app's resource");
    assert!(
        resource
            .actions
            .iter()
            .any(|action| action.id == wire::LOCATE),
        "an app whose file is gone offers Locate"
    );

    let found = env.join("Applications/Dropped.AppImage");
    std::fs::create_dir_all(found.parent().expect("a folder")).expect("a folder");
    std::fs::write(&found, appimage()).expect("the file where the person put it");
    executable(&found);
    service
        .invoke(
            wire::app(&app.desktop_id).as_str(),
            wire::LOCATE,
            &arguments(&[(wire::PATH_PARAM, Value::String(found.display().to_string()))]),
        )
        .expect("Locate is offered");
    let app = apps(&mut service).pop().expect("the app");
    assert!(app.present, "the app starts again from where it was found");
    assert_eq!(Path::new(&app.file), found);
}

/// Saying no to the offer keeps it — search can still add the app — and
/// marks it, so nothing asks about that file again.
#[test]
fn declining_keeps_the_offer_and_stops_asking() {
    let env = environment(&[(PathBuf::from("Downloads/Dropped.AppImage"), appimage())]);
    let dropped = env.join("Downloads/Dropped.AppImage");
    let mut service = registry();
    let (observed, facts) = window(4, "org.example.Dropped", &dropped);
    service.handle().observed(observed, facts);
    assert!(!offer_at(&dropped, &mut service).declined);

    service
        .invoke(
            RESOURCE,
            wire::DECLINE,
            &arguments(&[(
                wire::FILE_PARAM,
                Value::String(dropped.display().to_string()),
            )]),
        )
        .expect("decline is offered");
    let offer = offer_at(&dropped, &mut service);
    assert!(offer.declined, "the file is not asked about again");
    assert_eq!(
        offer.artwork, None,
        "nothing is kept for a file they said no to"
    );

    // Run again: the offer is there, already marked, and the app is not.
    service.handle().closed(4);
    let (observed, facts) = window(5, "org.example.Dropped", &dropped);
    service.handle().observed(observed, facts);
    assert!(offer_at(&dropped, &mut service).declined);
    assert!(apps(&mut service).is_empty());
}

/// Taking an app off the person's apps leaves its file where it is;
/// uninstalling moves the file to the trash and takes the app off.
#[test]
fn removing_leaves_the_file_and_uninstalling_trashes_it() {
    let env = environment(&[(PathBuf::from("Downloads/Kept.AppImage"), appimage())]);
    let kept = env.join("Downloads/Kept.AppImage");
    let mut service = registry();
    let (observed, facts) = window(6, "org.example.Kept", &kept);
    service.handle().observed(observed, facts);
    service
        .invoke(
            RESOURCE,
            wire::ADD,
            &arguments(&[(wire::FILE_PARAM, Value::String(kept.display().to_string()))]),
        )
        .expect("add is offered");
    let app = apps(&mut service).pop().expect("the added app");

    service
        .invoke(
            wire::app(&app.desktop_id).as_str(),
            wire::REMOVE,
            &Map::new(),
        )
        .expect("remove is offered");
    assert!(apps(&mut service).is_empty());
    assert!(kept.is_file(), "remove takes only the entry off");

    // Added again, then uninstalled: every copy goes to the trash.
    let (observed, facts) = window(7, "org.example.Kept", &kept);
    service.handle().observed(observed, facts);
    service
        .invoke(
            RESOURCE,
            wire::ADD,
            &arguments(&[(wire::FILE_PARAM, Value::String(kept.display().to_string()))]),
        )
        .expect("add is offered");
    let app = apps(&mut service).pop().expect("the added app");
    service
        .invoke(
            wire::app(&app.desktop_id).as_str(),
            wire::UNINSTALL,
            &Map::new(),
        )
        .expect("uninstall is offered");
    assert!(apps(&mut service).is_empty());
    assert!(!kept.exists(), "the program went to the trash");
}

/// Closing a program's window drops its offer: search lists what is running,
/// not what ran.
#[test]
fn an_offer_goes_when_the_program_closes() {
    let env = environment(&[(PathBuf::from("Downloads/Gone.AppImage"), appimage())]);
    let gone = env.join("Downloads/Gone.AppImage");
    let mut service = registry();
    let (observed, facts) = window(8, "org.example.Gone", &gone);
    service.handle().observed(observed, facts);
    offer_at(&gone, &mut service);

    service.handle().closed(8);
    until("the offer is gone with the window", || {
        read(&mut service).offers.is_empty()
    });
}
