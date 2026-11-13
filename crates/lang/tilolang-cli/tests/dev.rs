//! The desktop loop publishes each saved build where the running app reads it, and starts the
//! window again on a new build of the runtime binary or a saved source, however the file was
//! replaced.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::path::{Path, PathBuf};

use tilo_test_env::SpawnUnderEnvLock as _;
use tilolang_cli::package::{self, BuildOptions};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::upgrade::{Upgrade, compare};

mod support;

use support::{copy_package, packages};

#[test]
fn a_saved_build_is_the_generation_the_app_reads() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &source);
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let out = scratch.path().join("out");
    let options = BuildOptions {
        dir: source.clone(),
        search: Vec::new(),
        out,
        tilo_runtime: package::DEFAULT_TILO_RUNTIME.into(),
        tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.into(),
        version: "0.1.0".into(),
    };
    let publish = |app: &package::Published, updates: &Path| {
        tilo_app_module::publish_with(updates, |generation| {
            app.bundle
                .write(generation)
                .map_err(|error| std::io::Error::other(error.to_string()))
        })
        .unwrap()
    };
    let first = package::published(&options, &identity).unwrap();
    let app = first.last().unwrap();
    let updates = scratch.path().join("updates");
    assert_eq!(publish(app, &updates), 1);
    assert_eq!(tilo_app_module::current(&updates).unwrap(), Some(1));
    let read = Bundle::read(&updates.join("1")).unwrap();
    assert_eq!(read.meta.app, app.app, "the app reads its own package");
    assert_eq!(read.meta.title, app.meta.title);
    assert!(
        !options.out.exists() || std::fs::read_dir(&options.out).unwrap().next().is_none(),
        "a desktop build stages, packs and signs nothing"
    );
    assert_eq!(
        package::build(&options, &identity)
            .unwrap()
            .last()
            .unwrap()
            .digest,
        app.digest,
        "the same build has the same digest whether or not it is installed"
    );

    // A save inside the package's views: the next build is published as the next generation, and
    // the one the running app still reads stays.
    let file = source.join("app.tl");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace(
            "/// Add one to the counter.",
            "/// Add one to the counter, whatever it shows.",
        ),
    )
    .unwrap();
    let second = package::published(&options, &identity).unwrap();
    let app = second.last().unwrap();
    assert_eq!(
        compare(&first.last().unwrap().meta, &app.meta),
        Upgrade::InPlace,
        "a view's text is taken in place"
    );
    assert_eq!(publish(app, &updates), 2);
    assert!(updates.join("1").is_dir(), "what the app runs stays");
    let read = Bundle::read(&updates.join("2")).unwrap();
    assert_eq!(read.meta.app, app.app);
}

/// A `tilolang dev` on this desktop whose runtime is a script standing in for the window's program:
/// the loop starts it, stops it and starts it again as it does the runtime, and the script records
/// each start.
#[cfg(unix)]
struct Loop {
    child: std::process::Child,
    scratch: tempfile::TempDir,
    package: PathBuf,
    runtime: PathBuf,
    starts: PathBuf,
    state: PathBuf,
    reported: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(unix)]
impl Loop {
    fn start() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let package = scratch.path().join("counter");
        copy_package(&packages().join("counter"), &package);
        let bin = scratch.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let runtime = bin.join("tilolang-app");
        let starts = scratch.path().join("starts.log");
        std::fs::write(&starts, "").unwrap();
        write_program(&runtime, "the window's program", &starts);
        let state = scratch.path().join("state");
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_tilolang"))
            .arg("dev")
            .arg(&package)
            .arg("--runtime")
            .arg(&runtime)
            .arg("--repo")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .env("XDG_STATE_HOME", &state)
            .env("XDG_DATA_HOME", scratch.path().join("data"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap();
        let reported = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        for stream in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let reported = std::sync::Arc::clone(&reported);
            std::thread::spawn(move || {
                use std::io::BufRead as _;
                for line in std::io::BufReader::new(stream).lines() {
                    let Ok(line) = line else { return };
                    reported.lock().unwrap().push(line);
                }
            });
        }
        Self {
            child,
            scratch,
            package,
            runtime,
            starts,
            state,
            reported,
        }
    }

    fn started(&self) -> Vec<u32> {
        std::fs::read_to_string(&self.starts)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("start ")?.parse().ok())
            .collect()
    }

    fn wait_starts(&self, count: usize) -> Vec<u32> {
        tilo_test_env::wait_for(format!("the window started {count} times"), || {
            let started = self.started();
            (started.len() >= count).then_some(started)
        })
    }

    fn updates(&self) -> PathBuf {
        tilo_test_env::wait_for("the loop to publish a build", || {
            let build = std::fs::read_dir(self.state.join("tilolang/dev-build"))
                .ok()?
                .flatten()
                .map(|entry| entry.path().join("updates"))
                .find(|dir| dir.is_dir() && dir.join("current").is_file())?;
            Some(build)
        })
    }

    /// Replace the runtime with a new build, as a build does: a new file, renamed over the old one,
    /// which leaves a new inode where the old file was.
    fn replace_runtime(&self, note: &str) {
        let staged = self.scratch.path().join("bin/.tilolang-app.new");
        write_program(&staged, note, &self.starts);
        std::fs::rename(&staged, &self.runtime).unwrap();
    }

    fn wait_report(&self, text: &str) {
        tilo_test_env::wait_until(format!("the loop to report {text:?}"), || {
            self.reported
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains(text))
        });
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let reported = self.reported.lock().unwrap().clone();
        for line in &reported {
            assert!(
                !line.contains("the app stopped") && !line.contains("the app ended"),
                "{reported:#?}"
            );
        }
    }
}

/// Write the runtime's stand-in program, which records each of its starts in `starts` and runs
/// until it is asked to stop.
#[cfg(unix)]
fn write_program(path: &Path, note: &str, starts: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let text = format!(
        "#!/bin/sh\n# {note}\necho \"start $$\" >> {}\ntrap 'exit 0' TERM\nwhile :; do sleep 0.2; done\n",
        starts.display()
    );
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A new build of the runtime reaches the window: the file is published beside the old one and
/// renamed over it, and the loop starts the window again on it.
#[cfg(unix)]
#[test]
fn a_new_build_of_the_runtime_replaced_by_rename_restarts_the_window() {
    let dev = Loop::start();
    let first = dev.wait_starts(1);
    dev.replace_runtime("the window's program, built again");
    let started = dev.wait_starts(2);
    assert_ne!(first[0], started[1], "{started:?}");
    dev.wait_report("the app runs on the new runtime");
    dev.stop();
}

/// A build that replaces the directory holding the runtime reaches the window too: the loop watches
/// the directory above it, and takes a new watch of the replaced one.
#[cfg(unix)]
#[test]
fn a_runtime_directory_replaced_whole_restarts_the_window() {
    let dev = Loop::start();
    let first = dev.wait_starts(1);
    let moved = dev.scratch.path().join("bin.moved");
    std::fs::rename(dev.runtime.parent().unwrap(), &moved).unwrap();
    std::fs::create_dir(dev.runtime.parent().unwrap()).unwrap();
    write_program(
        &dev.runtime,
        "the window's program, from a new directory",
        &dev.starts,
    );
    let started = dev.wait_starts(2);
    assert_ne!(first[0], started[1], "{started:?}");
    dev.wait_report("the app runs on the new runtime");
    dev.stop();
}

/// A saved source reaches the app: the editor's file is renamed over the package's, and the loop
/// publishes the new build as the next generation.
#[cfg(unix)]
#[test]
fn a_saved_source_replaced_by_rename_reaches_the_app() {
    let dev = Loop::start();
    dev.wait_starts(1);
    let updates = dev.updates();
    wait_generation(&updates, Some(1));
    let file = dev.package.join("app.tl");
    let text = std::fs::read_to_string(&file).unwrap();
    let saved = text.replace(
        "/// Add one to the counter.",
        "/// Add one to the counter, whatever it shows.",
    );
    assert_ne!(saved, text, "the save changes the source");
    let staged = dev.package.join(".app.tl.saved");
    std::fs::write(&staged, saved).unwrap();
    std::fs::rename(&staged, &file).unwrap();
    wait_generation(&updates, Some(2));
    dev.stop();
}

/// A save in a subdirectory of the package reaches the app too: a translation in its `locales/` is
/// part of a build, and the loop watches the package as a tree.
#[cfg(unix)]
#[test]
fn a_translation_saved_in_the_packages_locales_reaches_the_app() {
    let dev = Loop::start();
    dev.wait_starts(1);
    let updates = dev.updates();
    wait_generation(&updates, Some(1));
    std::fs::write(
        dev.package.join("locales/de-DE.ftl"),
        "d-so-tilo-lang-counter = Zähler\n",
    )
    .unwrap();
    wait_generation(&updates, Some(2));
    dev.stop();
}

fn wait_generation(updates: &Path, generation: Option<u64>) {
    tilo_test_env::wait_until(
        format!("the app's builds to reach generation {generation:?}"),
        || tilo_app_module::current(updates).unwrap() == generation,
    );
}
