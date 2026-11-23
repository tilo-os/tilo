//! The gallery (`apps/tl/gallery`) started again on the profile an earlier process kept, as
//! `tilolang dev` starts it: each start is a process of its own, so what the last one left on disk
//! is all the next one has. A start after the process ended and one after it was killed in the
//! middle of its work both open over the indexes kept, building none anew. No start opens an
//! index file on a frame: that waits for the disk, which on a slow one is a fraction of a second.

use std::path::Path;
use std::process::Command;

use tilo_test_env::SpawnUnderEnvLock;
use tilolang_process::window::app::TlApp;
use ui::framework::testing::HeadlessApp;
use ui::tilo_resource::profiling::observe;

/// The profile a started process opens, when it is one ([`start`]).
const PROFILE_ENV: &str = "TILO_GALLERY_START_PROFILE";
/// Set when the started process is to die before it closes anything, as a killed one does.
const KILL_ENV: &str = "TILO_GALLERY_START_KILL";

/// One scope a start ran on its frames: its name, calls and milliseconds.
type Scope = (String, usize, u64);

/// What one start of the gallery said: the scopes its opening and first frame took, those of
/// the frames that took in what was read off them, and its log.
struct Start {
    /// Whether the gallery's sidebar came to list its pages.
    shown: bool,
    opening: Vec<Scope>,
    settling: Vec<Scope>,
    log: String,
}

fn calls(scopes: &[Scope], scope: &str) -> usize {
    scopes
        .iter()
        .filter(|(name, ..)| name == scope)
        .map(|(_, calls, _)| *calls)
        .sum()
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
    let output = command.output_locked().expect("the gallery starts");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let log = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        kill || output.status.success(),
        "the gallery started and stopped: {stdout}\n{log}"
    );
    let scopes = |when: &str| -> Vec<Scope> {
        stdout
            .lines()
            .filter_map(|line| {
                let mut words = line.strip_prefix(when)?.split_whitespace();
                let (name, calls, ms) = (words.next()?, words.next()?, words.next()?);
                Some((name.to_owned(), calls.parse().ok()?, ms.parse().ok()?))
            })
            .collect()
    };
    Start {
        // The harness puts the test's name before the first line the test prints.
        shown: stdout.lines().any(|line| line.ends_with("SHOWN true")),
        opening: scopes("OPENING "),
        settling: scopes("SETTLING "),
        log,
    }
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
    let (app, opening) = observe(|| HeadlessApp::<TlApp>::open_data_dir(profile));
    let mut app = app.expect("the gallery opens");
    // What the window reads off the frame lands, and its pages are listed, as they are by the
    // time a person closes the window or the process is killed.
    let listed = |app: &mut HeadlessApp<TlApp>| {
        app.harness()
            .targets()
            .keys()
            .any(|name| name.contains("NavList[") && name.contains("/row["))
    };
    let (shown, settling) = observe(|| {
        // Each pass takes in what was read off the frame, then waits for the next wake: a first
        // start builds every index from the document, which on a busy machine takes seconds.
        let began = std::time::Instant::now();
        while began.elapsed() < std::time::Duration::from_secs(120) {
            app.settle();
            if listed(&mut app) {
                return true;
            }
            app.harness()
                .wait_until(ui::time::Duration::from_millis(50), |_| false);
        }
        false
    });
    println!("SHOWN {shown}");
    if !shown {
        let targets: Vec<String> = app.harness().targets().keys().take(40).cloned().collect();
        eprintln!("the window shows only: {targets:?}");
    }
    for (when, scopes) in [("OPENING", &opening), ("SETTLING", &settling)] {
        for sample in scopes {
            println!(
                "{when} {} {} {}",
                sample.phase,
                sample.calls,
                sample.elapsed_ns / 1_000_000
            );
        }
    }
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
    let first = start(&profile, false);
    let again = start(&profile, false);
    let _killed = start(&profile, true);
    let after_kill = start(&profile, false);
    for (what, start) in [
        ("the first start", &first),
        ("a start again", &again),
        ("a start after a kill", &after_kill),
    ] {
        // Where the opening's time went on this machine, for whoever reads the test's output.
        let mut costliest = start.opening.clone();
        costliest.sort_by_key(|(_, _, ms)| std::cmp::Reverse(*ms));
        let costliest: Vec<String> = costliest
            .iter()
            .take(12)
            .map(|(name, calls, ms)| format!("{name}={ms}ms/{calls}"))
            .collect();
        eprintln!("{what}: {}", costliest.join(" "));
        // Opening an index file waits for the disk: a frame never does.
        assert_eq!(
            calls(&start.opening, "tl.index.open") + calls(&start.settling, "tl.index.open"),
            0,
            "{what} opens its indexes off the frame"
        );
        let tail: String = start
            .log
            .lines()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            start.shown,
            "{what} shows the gallery once its indexes are in; its log, newest first: {tail}"
        );
    }
    for (what, start) in [
        ("a start again", &again),
        ("a start after a kill", &after_kill),
    ] {
        assert!(
            !start.log.contains("could not be opened"),
            "{what} opens the indexes the last process kept: {}",
            start.log
        );
    }
    let _ = std::fs::remove_dir_all(&profile);
}
