//! The shell's folder chooser reaches removable media: opening a folder for the person offers
//! the mount root under `/media`, so a folder on a drive plugged in now can be chosen.

use tilolang_runtime::files::{Entry, FilesHost};
use tilolang_runtime::{Actor, Value};
use tilolang_tilo::host::Host;

const SHELL: &str = "so.tilo.shell";

fn ada() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// A device holding Ada's home and a drive mounted under `/media`.
struct Disk;

impl Disk {
    fn folder(name: &str, path: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            path: path.to_owned(),
            folder: true,
            size: 0,
            modified: 0,
            hidden: false,
        }
    }
}

impl FilesHost for Disk {
    fn list(&self, path: &str) -> Result<Vec<Entry>, String> {
        match path {
            "~" | "/home/ada" => Ok(vec![Disk::folder("Documents", "/home/ada/Documents")]),
            "/home/ada/Documents" | "/media" => Ok(Vec::new()),
            "/media/USB" => Ok(vec![Disk::folder("Backup", "/media/USB/Backup")]),
            other => Err(format!("`{other}` is not a folder on this device")),
        }
    }

    fn stat(&self, path: &str) -> Result<Option<Entry>, String> {
        if path == "/media" {
            Ok(Some(Disk::folder("media", "/media")))
        } else {
            Ok(None)
        }
    }

    fn read(&self, _path: &str) -> Result<Vec<u8>, String> {
        Err("no file".to_owned())
    }

    fn watch(&self, _path: &str, _run: &str) -> Result<String, String> {
        Ok("watch-1".to_owned())
    }
}

/// The shell's package over a host that runs it, on a device with a drive mounted.
fn shell() -> Host {
    let host = Host::new(ada(), || None).unwrap();
    for meta in tilolang_tilo::platform::platform_packages().unwrap() {
        let app = meta.app.clone();
        host.runtime()
            .load_foreign(meta)
            .unwrap_or_else(|why| panic!("{app}: {why}"));
    }
    let compiled = tilolang_tilo::shell::compile().unwrap_or_else(|why| panic!("{why}"));
    host.load_compiled(compiled.last().expect("the shell's packages"))
        .unwrap();
    host.runtime().set_device_files(Disk);
    host
}

#[test]
fn the_folder_chooser_offers_removable_media() {
    let host = shell();
    let picking = host.runtime().create_root("Picking").unwrap();
    let id = host
        .package(SHELL)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|action| action.name == "open_folder")
        .expect("the shell opens a folder")
        .id
        .clone();
    host.runtime()
        .invoke_action(
            &ada(),
            (
                id.as_str(),
                vec![Value::resource(&picking), Value::text("/home/ada")],
            ),
        )
        .unwrap();

    // The folder is read, and the mount root is the place the picker offers beside it.
    assert_eq!(
        host.runtime().read(&picking, "folder").unwrap(),
        Value::text("/home/ada")
    );
    match host.runtime().read(&picking, "places").unwrap() {
        Value::List(places) => {
            assert_eq!(places.len(), 1, "the mount root is a place: {places:?}");
            let Value::Tuple(fields) = &places[0] else {
                panic!("a place is a host.FileEntry, not {:?}", places[0]);
            };
            assert_eq!(
                fields.get(1),
                Some(&Value::text("/media")),
                "the place is where drives are mounted"
            );
            assert_eq!(
                fields.get(2),
                Some(&Value::Bool(true)),
                "removable media is a folder"
            );
        }
        other => panic!("places is a list, not {other:?}"),
    }
}
