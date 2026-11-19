//! The gallery (`apps/tl/gallery`) started again on the profile an earlier process kept, as
//! `tilolang dev` starts it: each start is a process of its own, so what the last one left on disk
//! is all the next one has. A start after the process ended and one after it was killed in the
//! middle of its work both open over the indexes kept, building none anew, and their first frame
//! shows the gallery rather than the loading state of an index being built again.

use std::path::Path;
use std::process::Command;

use tilolang_app::app::TlApp;
use ui::framework::testing::HeadlessApp;
use ui::tilo_resource::profiling::observe;

/// The profile a started process opens, when it is one ([`start`]).
const PROFILE_ENV: &str = "TILO_GALLERY_START_PROFILE";
/// Set when the started process is to die before it closes anything, as a killed one does.
const KILL_ENV: &str = "TILO_GALLERY_START_KILL";

/// What one start of the gallery said: how often its opening ran each scope, and its log.
struct Start {
    scopes: Vec<(String, usize)>,
    log: String,
}

impl Start {
    fn calls(&self, scope: &str) -> usize {
        self.scopes
            .iter()
            .find(|(name, _)| name == scope)
            .map_or(0, |(_, calls)| *calls)
    }
}

/// Start the gallery in a process of its own on the profile `profile`: this test's binary, asked
/// to take this test's started role. `kill` ends it as a kill does, with nothing closed.
fn start(profile: &Path, kill: bool) -> Start {
    let mut command = Command::new(std::env::current_exe().expect("the test binary"));
    command
        .args([
            "gallery_start::a_started_gallery_opens_over_what_the_last_process_kept",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROFILE_ENV, profile);
    if kill {
        command.env(KILL_ENV, "1");
    }
    let output = command.output().expect("the gallery starts");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let log = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        kill || output.status.success(),
        "the gallery started and stopped: {stdout}\n{log}"
    );
    let scopes = stdout
        .lines()
        .filter_map(|line| {
            let (name, calls) = line.strip_prefix("START-SCOPE ")?.split_once(' ')?;
            Some((name.to_owned(), calls.parse().ok()?))
        })
        .collect();
    Start { scopes, log }
}

/// The started role: open the gallery on the profile, print the scopes its opening and first
/// frame took, and stop, as a window closes or as a kill.
fn started(profile: &Path) {
    // What the process warns of is what the test reads of it.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("tilolang_tilo=warn"))
        .with_writer(std::io::stderr)
        .try_init();
    crate::support::installed("gallery");
    let (app, scopes) = observe(|| HeadlessApp::<TlApp>::open_data_dir(profile));
    let mut app = app.expect("the gallery opens");
    for sample in &scopes {
        println!("START-SCOPE {} {}", sample.phase, sample.calls);
    }
    // What the window derives off the frame lands, as it has by the time a person closes the
    // window or the process is killed.
    app.settle();
    if std::env::var_os(KILL_ENV).is_some() {
        // Nothing closes: no destructor is called, as none is in a killed process.
        std::process::abort();
    }
    drop(app);
}

#[test]
fn a_started_gallery_opens_over_what_the_last_process_kept() {
    if let Some(profile) = std::env::var_os(PROFILE_ENV) {
        started(Path::new(&profile));
        return;
    }
    let profile = std::env::temp_dir().join(format!("tilo-gallery-start-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    let _first = start(&profile, false);
    let again = start(&profile, false);
    let _killed = start(&profile, true);
    let after_kill = start(&profile, false);
    for (what, start) in [
        ("a start again", &again),
        ("a start after a kill", &after_kill),
    ] {
        assert!(
            !start.log.contains("could not be opened"),
            "{what} opens the indexes the last process kept: {}",
            start.log
        );
        // An index built anew opens the window on the loading state, and its views only once
        // the build's first step lands, frames later.
        assert_eq!(
            start.calls("tl.view.open_root"),
            1,
            "{what} shows the gallery in its first frame, read from the indexes kept"
        );
    }
    let _ = std::fs::remove_dir_all(&profile);
}
