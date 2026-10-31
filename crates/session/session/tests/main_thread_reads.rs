//! The session's loop walks no path once the desktop is up.
//!
//! One path lookup can wait seconds for a busy disk, and on the loop that is
//! a frozen desktop: the artwork of a notification that just arrived, the
//! apps search lists, and who sits at this device are all answered from what
//! a worker read, never by opening, listing or stating a path on the thread
//! the person is using.
//!
//! The audit (`tilo_liveness::main_thread_files`) is a filter on the process's
//! main thread, and `cargo test` runs a test on a thread of its own. So this
//! target runs without `libtest` (`harness = false` in the manifest): its
//! `main` is the main thread, the session's loop runs on it, and the audit is
//! armed here once startup is over. `cargo test --test main_thread_reads`
//! runs it; nextest asks it for a libtest listing, which it has none of, and
//! so lists no case.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

fn main() {
    // nextest lists every test target before it runs any (`--list --format
    // terse`) and reads libtest's format from the output; this target has no
    // cases to list. Cargo runs a `harness = false` target itself.
    if std::env::args().skip(1).any(|arg| arg == "--list") {
        return;
    }
    #[cfg(target_os = "linux")]
    drive();
}

/// A session driven the way the binary is built and the person uses it, from
/// before its loop runs to the audit's report.
#[cfg(target_os = "linux")]
fn drive() {
    use std::fmt::Write as _;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    use tilo_compositor::OutputConfig;
    use tilo_desktop::desktop_actions::{DESK_RESOURCE, SEARCH_RESOURCE};
    use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
    use tilo_session::{Lists, Options, SearchOptions, Session};

    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    // Who sits here, as the binary reads it before it builds anything.
    tilo_session::identity::install();
    // The desktop services run beside a session in no bus at all: nothing
    // here reaches the caller's session bus, and an arrival is pushed into
    // the daemon's handle the way the bus would hand it over.
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
            // What the binary reads of this device for search: the installed
            // apps, the machine's own and the catalogue, followed by a worker.
            search: SearchOptions {
                lists: Lists::of_device(),
                ..SearchOptions::default()
            },
            wake,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("the session");
    for _ in 0..3 {
        session.step(Duration::from_millis(1)).expect("a turn");
    }

    // Startup is over: from here on the loop answers everything from a
    // worker, and the audit holds it to that.
    tilo_liveness::main_thread_files::arm().expect("the audit arms on the main thread");

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

    // Search opens over the desk and takes a query, its rows found in the
    // lists a worker keeps current.
    invoke(
        &mut session,
        DESK_RESOURCE,
        "open-search",
        serde_json::json!({}),
    );
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a turn");
    }
    if find(&session.resources(), SEARCH_RESOURCE).is_some() {
        invoke(
            &mut session,
            SEARCH_RESOURCE,
            "search-for",
            serde_json::json!({"query": "probe"}),
        );
        for _ in 0..4 {
            session.step(Duration::from_millis(1)).expect("a turn");
        }
    }

    // Who sits at this device, as the workspace sheet's people rows and the
    // apps naming an author ask it.
    let _ = ui::local_identity::bound();
    // Which identity each room presents, as every publish and frame asks it.
    if let Ok(store) = tilo_identity_host::registry::IdentityRegistryStore::from_env() {
        let _ = store.load_reconciled_cached();
    }

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

/// Invoke `action` of `resource`, as the person choosing it does.
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
