//! The session's loop walks no path once the desktop is up.
//! One path lookup can wait seconds for a busy disk, and on the loop that is a frozen desktop: the
//! artwork of a notification that just arrived, the apps search lists, and who sits at this device
//! are all answered from what a worker read, never by opening, listing or stating a path on the
//! thread the person is using.
//! The audit (`tilo_liveness::main_thread_files`) is a filter on the process's main thread, and
//! `cargo test` runs a test on a thread of its own. So this target runs without `libtest` (`harness
//! = false` in the manifest): its `main` is the main thread, the session's loop runs on it, and the
//! audit is armed here once startup is over. `cargo test --test main_thread_reads` runs it; nextest
//! asks it for a libtest listing, which it has none of, and so lists no case.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

#[cfg(target_os = "linux")]
use tilo_compositor::InputEvent;
#[cfg(target_os = "linux")]
use ui::shortcut_table::{ShortcutAction, ShortcutConfig};

fn main() {
    // nextest lists every test target before it runs any (`--list --format terse`) and reads
    // libtest's format from the output; this target has no cases to list. Cargo runs a
    // `harness = false` target itself.
    if std::env::args().skip(1).any(|arg| arg == "--list") {
        return;
    }
    #[cfg(target_os = "linux")]
    drive();
}

/// An app's notification arrives: its banner asks for the app's artwork.
#[cfg(target_os = "linux")]
fn notification(
    desktop: &tilo_session::desktop_services::DesktopServices,
    session: &mut tilo_session::Session,
) {
    use std::time::Duration;

    // An app's notification arrives: its banner asks for the app's artwork.
    let mut sent = tilo_notifications_service::Notification::new(1);
    "Build finished".clone_into(&mut sent.summary);
    "All tests passed".clone_into(&mut sent.body);
    sent.caller = Some(tilo_notifications_service::Caller {
        app_id: "org.example.Probe".to_owned(),
        workspace: None,
    });
    sent.desktop_entry = Some("org.example.Probe.desktop".to_owned());
    sent.app_icon = Some("org.example.Probe".to_owned());
    desktop.notifications().push(sent);
    for _ in 0..8 {
        session.step(Duration::from_millis(1)).expect("a turn");
    }
}

/// Change a setting the way a setter does: the session keeps it for the workspace it does not hold
/// yet, and leaves it for the processes outside itself, each from workers of their own.
#[cfg(target_os = "linux")]
fn changed_setting(session: &mut tilo_session::Session) {
    use std::time::Duration;

    // A setting the person changes: the session keeps it for the workspace it does not hold yet,
    // and leaves the settings for the processes outside the session, both from workers of their
    // own. A setter writes a section this way, and the change is made on the loop because that is
    // where the model a press runs in does.
    let mut learned = serde_json::Map::new();
    learned.insert("enabled".to_owned(), serde_json::Value::Bool(false));
    let mut hints = serde_json::Map::new();
    hints.insert("learned".to_owned(), serde_json::Value::Object(learned));
    settings_store::update_section("control_hints", hints).expect("the setting is kept");
    for _ in 0..8 {
        session.step(Duration::from_millis(1)).expect("a turn");
    }
    // What waits for the workspace is kept by now; the settings the processes outside read are
    // the worker's to leave.
    settings_store::flush_pending_writes();
    // Both files hold the change once the workers wrote them, and they are read on a thread of
    // their own: the loop walks neither.
    let kept = settings_store::config_path(std::path::Path::new(settings_store::outside::WAITING));
    let left = settings_store::outside::path();
    let held = std::thread::spawn(move || {
        // What waits is a map of sections; what the session leaves holds them under `settings`.
        let shows = |path: &Option<std::path::PathBuf>, bare: bool| {
            (0..200).any(|_| {
                let shows = path
                    .as_ref()
                    .and_then(|path| std::fs::read(path).ok())
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                    .is_some_and(|held| {
                        let held = if bare { &held } else { &held["settings"] };
                        held.get("control_hints")
                            .and_then(|hints| hints.get("learned"))
                            .and_then(|learned| learned.get("enabled"))
                            == Some(&serde_json::Value::Bool(false))
                    });
                if !shows {
                    std::thread::sleep(Duration::from_millis(10));
                }
                shows
            })
        };
        (shows(&kept, true), shows(&left, false))
    })
    .join()
    .expect("the reader runs");
    assert!(
        held.0,
        "the session kept the change for the workspace it does not hold yet"
    );
    assert!(
        held.1,
        "the session left the change for the processes outside it"
    );
}

/// A session driven the way the binary is built and the person uses it, from before its loop runs
/// to the audit's report.
#[cfg(target_os = "linux")]
fn drive() {
    use std::fmt::Write as _;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    use tilo_compositor::OutputConfig;
    use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
    use tilo_session::{Lists, Options, Session};
    use tilo_session_core::desktop::verbs::{SEARCH_RESOURCE, SHELL_RESOURCE};

    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    // Who sits here, as the binary reads it before it builds anything.
    tilo_session::identity::install();
    // The font catalog every renderer sets text in, built on a thread as the binary builds it.
    let fonts = tilo_ui_base::warmup::spawn_warmup();
    // The desktop services run beside a session in no bus at all: nothing here reaches the caller's
    // session bus, and an arrival is pushed into the daemon's handle the way the bus would hand it
    // over.
    let wake = tilo_service::LoopWake::default();
    let (side, _link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = Rc::new(
        DesktopServices::start(
            &Buses {
                session: Bus::None,
                system: Bus::None,
            },
            &wake.handle(),
            Arc::new(tilo_portal::NoCapture),
            &side,
            None,
        )
        .expect("the desktop services"),
    );
    let mut session = Session::headless(
        Options {
            desktop: Some(Rc::clone(&desktop)),
            // What the binary reads of this device: the installed apps, the machine's own and the
            // catalogue, followed by a worker.
            lists: Lists::of_device(),
            wake,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("the session");
    for _ in 0..3 {
        session.step(Duration::from_millis(1)).expect("a turn");
    }

    // Startup is over: the font catalog is read; from here the loop answers from workers.
    let _ = fonts.map(std::thread::JoinHandle::join);
    tilo_liveness::main_thread_files::arm().expect("the audit arms on the main thread");

    notification(&desktop, &mut session);

    // Search opens over the shell and takes a query, its rows found in the lists a worker keeps
    // current.
    invoke(
        &mut session,
        SHELL_RESOURCE,
        "search",
        serde_json::json!({}),
    );
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a turn");
    }
    if find(&session.resources(), SEARCH_RESOURCE).is_some() {
        invoke(
            &mut session,
            SEARCH_RESOURCE,
            "search",
            serde_json::json!({"query": "probe"}),
        );
        for _ in 0..4 {
            session.step(Duration::from_millis(1)).expect("a turn");
        }
    }

    // The keyboard-source chord: which keyboard typed last, and the list its own sources are kept
    // under, are read from the devices a worker keeps, so the pass it runs walks no path.
    press(&mut session, ShortcutAction::NextInputSource);
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a turn");
    }

    // Who sits at this device, as the workspace sheet's people rows and the apps naming an author
    // ask it.
    let _ = tilo_ui_base::local_identity::load();
    // Which identity each workspace presents, as every publish and frame asks it.
    if let Ok(store) = tilo_identity::store::IdentityRegistryStore::from_env() {
        let _ = store.load_reconciled_cached();
    }

    changed_setting(&mut session);

    let walks = tilo_liveness::main_thread_files::report().expect("the audit's report");
    let mut listed = String::new();
    for walk in &walks {
        let _ = writeln!(
            listed,
            "\nsite: {}\n{} {:?} ({}x)\n{}",
            walk.site(),
            walk.syscall,
            walk.path,
            walk.count,
            walk.backtrace
        );
    }
    assert!(
        walks.is_empty(),
        "the session's main thread walked {} path(s) after startup; each can wait on the disk \
         and freeze the desktop. Read it on a worker and hand the loop the value \
         (`tilo_files::native::Watched`):{listed}",
        walks.len(),
    );
    println!("the session's main thread walked no path after startup");
}

/// Invoke `action` of `resource` , as the person choosing it does.
#[cfg(target_os = "linux")]
fn invoke(
    session: &mut tilo_session::Session,
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    session
        .invoke(resource, action, &arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
}

/// The resource `id` in what the session publishes, where it is nested.
#[cfg(target_os = "linux")]
fn find(resources: &[tilo_actions::ResourceSpec], id: &str) -> Option<tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

/// What a key sends, evdev codes.
#[cfg(target_os = "linux")]
const KEY_SPACE: u32 = 57;
#[cfg(target_os = "linux")]
const KEY_LEFT_CTRL: u32 = 29;
#[cfg(target_os = "linux")]
const KEY_LEFT_SHIFT: u32 = 42;
#[cfg(target_os = "linux")]
const KEY_LEFT_ALT: u32 = 56;
#[cfg(target_os = "linux")]
const KEY_LEFT_META: u32 = 125;

/// Press the chord the default table binds `action` to, as the person does: the modifiers held,
/// the key tapped, the modifiers let go.
#[cfg(target_os = "linux")]
fn press(session: &mut tilo_session::Session, action: ShortcutAction) {
    let chord = ShortcutConfig::default()
        .bindings
        .get(&action)
        .unwrap_or_else(|| panic!("{action:?} is bound"))
        .clone();
    let held: Vec<u32> = chord
        .modifiers
        .iter()
        .map(|name| match name.as_str() {
            "Super" => KEY_LEFT_META,
            "Ctrl" => KEY_LEFT_CTRL,
            "Alt" => KEY_LEFT_ALT,
            "Shift" => KEY_LEFT_SHIFT,
            other => panic!("no keycode for {other}"),
        })
        .collect();
    for keycode in &held {
        key(session, *keycode, true);
    }
    let tapped = match chord.key.as_str() {
        "space" => KEY_SPACE,
        other => panic!("no keycode for {other}"),
    };
    key(session, tapped, true);
    key(session, tapped, false);
    for keycode in held.iter().rev() {
        key(session, *keycode, false);
    }
}

/// Send `keycode` pressed or let go, and run the turn that takes it.
#[cfg(target_os = "linux")]
fn key(session: &mut tilo_session::Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("the key");
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a turn");
}
