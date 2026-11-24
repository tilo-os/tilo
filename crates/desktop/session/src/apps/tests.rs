use super::*;
use tilo_app_launch::output::OutputTail;
use tilo_app_launch::{End, Event};
use tilo_desktop_policy::Window;

const WORKSPACE: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

fn apps() -> Apps {
    Apps::with_launcher(SessionEnvironment::default(), None, None)
}

/// A package opening a file of the person's is handed it where its sandbox reaches it: the
/// path it is given is in its own view of the documents served to it, and reading it there
/// gives the file's bytes. The device's own path never reaches it.
#[test]
fn a_package_opening_a_file_reads_its_bytes_where_its_sandbox_holds_it() {
    use std::io::Read as _;
    let home = tempfile::tempdir().unwrap();
    let agenda = home.path().join("Agenda.docx");
    std::fs::write(&agenda, b"PK agenda").unwrap();
    let workspace = Workspace::new(WORKSPACE, home.path().join("capabilities"));
    let documents = tilo_documents::Documents::new();
    let launch = Launch::Package {
        app: "so.tilo.word-processor".to_owned(),
        open: Some(tilo_app_launch::Open::File(agenda.clone())),
    };

    let given = handed(&launch, &workspace, Some(&documents)).unwrap();
    let Launch::Package {
        open: Some(tilo_app_launch::Open::File(opened)),
        ..
    } = given.as_ref()
    else {
        panic!("the launch still opens a file: {given:?}");
    };
    assert!(
        opened.starts_with(tilo_documents::SANDBOX_DIR),
        "{}",
        opened.display()
    );
    assert_eq!(opened.file_name(), agenda.file_name(), "named as it is");
    let key = tilo_documents::AppKey::new(WORKSPACE, "so.tilo.word-processor").unwrap();
    let target = tilo_documents::host::resolve(&documents, &key, opened).unwrap();
    let mut bytes = Vec::new();
    std::fs::File::from(
        target
            .open(rustix::fs::OFlags::RDONLY, rustix::fs::Mode::empty())
            .unwrap(),
    )
    .read_to_end(&mut bytes)
    .unwrap();
    assert_eq!(bytes, b"PK agenda");
    let other = tilo_documents::AppKey::new(WORKSPACE, "so.tilo.notes").unwrap();
    assert!(
        tilo_documents::host::resolve(&documents, &other, opened).is_none(),
        "handed to the app that opens it alone"
    );

    assert_eq!(
        handed(&given, &workspace, Some(&documents))
            .unwrap()
            .as_ref(),
        given.as_ref(),
        "reopening the app's own document, as its history names it, opens it again"
    );
    assert!(
        handed(&launch, &workspace, None).is_err(),
        "no app is started on a path its sandbox cannot read"
    );
    let folder = Launch::Package {
        app: "so.tilo.file-explorer".to_owned(),
        open: Some(tilo_app_launch::Open::File(home.path().to_path_buf())),
    };
    assert_eq!(handed(&folder, &workspace, None).unwrap().as_ref(), &folder);
}

/// A launch takes the compositor's render node once the compositor has one, though the session
/// asked before its backend named it; the node it took then stays.
#[test]
fn a_launch_follows_the_render_node_the_compositor_names_later() {
    let apps = apps();
    apps.set_render_node(None);
    apps.follow_render_node(|| None, false);
    assert_eq!(apps.state.borrow().render_node, None);
    assert!(
        !apps.state.borrow().software,
        "nothing is decided before the first frame"
    );
    apps.follow_render_node(|| Some(PathBuf::from("/dev/dri/card1")), true);
    assert_eq!(
        apps.state.borrow().render_node.as_deref(),
        Some(Path::new("/dev/dri/card1"))
    );
    apps.follow_render_node(|| Some(PathBuf::from("/dev/dri/renderD128")), true);
    assert_eq!(
        apps.state.borrow().render_node.as_deref(),
        Some(Path::new("/dev/dri/card1")),
        "the node it took stays"
    );
}

/// A compositor that has drawn with no GPU to import from draws in software, and the apps it
/// opens are handed the software renderer's pin, where this machine has one.
#[cfg(target_os = "linux")]
#[test]
fn a_compositor_that_drew_without_a_gpu_pins_its_apps_to_the_software_renderer() {
    let apps = apps();
    apps.follow_render_node(|| None, true);
    let state = apps.state.borrow();
    assert!(state.software);
    assert_eq!(state.render_node, None);
    match launcher::gpu::software_pin(&launcher::gpu::IcdSearch::default()) {
        Ok(pin) => {
            for (key, value) in pin.env() {
                assert_eq!(
                    state.environment.variables().get(key),
                    Some(&value),
                    "{key}"
                );
            }
        }
        Err(_) => assert!(
            !state.environment.forwarded.contains_key("VK_DRIVER_FILES"),
            "no software renderer here, so nothing is pinned"
        ),
    }
}

/// The desktop the apps are started from, as wide as an output: where their tabs and the
/// answers in them stand.
fn shell() -> ShellDesktop {
    let desktop = crate::platform::surfaces::desktop(7);
    desktop.with_policy(|policy| policy.set_viewport_width(1200.0));
    desktop
}

/// Record that this session started launch `id` of `app` as process `pid`, `lived` ago, and
/// how far it got: what [`Apps::spawn`] writes when it starts an app, without a process here.
fn note_launch(
    apps: &Apps,
    id: u64,
    app: &str,
    pid: Option<u32>,
    instance: Option<u64>,
    lived: std::time::Duration,
    got: Got,
) {
    let started = std::time::Instant::now()
        .checked_sub(lived)
        .expect("the clock reads past the launch");
    apps.state.borrow_mut().launched.insert(
        id,
        Launching {
            asked: app.to_owned(),
            app: app.to_owned(),
            workspace: WORKSPACE.to_owned(),
            pid,
            instance,
            window: None,
            tab: None,
            started,
            got,
        },
    );
}

/// What the desktop shows after `end` ended the app `asked` , which this session started with
/// a window that came; `asked_to_end` when the person's own Quit ended it.
fn showed_after(end: End, asked_to_end: bool) -> Vec<String> {
    showed_after_in(end, asked_to_end, || false)
}

/// [`showed_after`] in a session that `session_ending` says was told to end, or not.
fn showed_after_in(end: End, asked_to_end: bool, session_ending: fn() -> bool) -> Vec<String> {
    let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
    apps.state.borrow_mut().session_ending = session_ending;
    let desktop = shell();
    apps.attach(desktop.clone());
    note_launch(
        &apps,
        1,
        "example.Canvas",
        None,
        None,
        std::time::Duration::ZERO,
        Got::Landed,
    );
    apps.state.borrow().spawner.report(Event {
        id: 1,
        app: "example.Canvas".to_owned(),
        title: "Canvas".to_owned(),
        end,
        killed: false,
        asked_to_end,
        said: OutputTail::default(),
    });
    apps.follow();
    texts(&desktop)
}

/// An app that ends cleanly before the window the person asked for came never opened: its tab
/// says it could not open, as for an app that crashed, instead of waiting on a window that is
/// not coming. Chat did that on the e2e-ws-branch box and left nothing anywhere.
#[test]
fn an_app_that_ends_cleanly_before_its_window_came_could_not_open() {
    let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
    let desktop = shell();
    apps.attach(desktop.clone());
    desktop
        .with_policy(|policy| {
            policy.handle().dispatch(
                tilo_session_core::desktop::navigation::ChromeEffect::OpenApp {
                    app: "example.Canvas".into(),
                    beside: None,
                    background: None,
                    float: None,
                },
            )
        })
        .unwrap();
    desktop.pass();
    let asked = desktop.take_launches();
    assert_eq!(asked.len(), 1, "the person asked for Canvas");
    // The launch was asked for the tab the shell held for it, and ended before its window
    // came.
    note_launch(
        &apps,
        1,
        "example.Canvas",
        None,
        asked[0].tab,
        std::time::Duration::ZERO,
        Got::Arriving,
    );
    apps.state.borrow().spawner.report(Event {
        id: 1,
        app: "example.Canvas".to_owned(),
        title: "Canvas".to_owned(),
        end: End::exited(0),
        killed: false,
        asked_to_end: false,
        said: OutputTail::default(),
    });
    apps.follow_ended();
    let shown = texts(&desktop);
    assert!(
        shown.iter().any(|text| text.contains("could not open")),
        "the tab says Canvas could not open: {shown:?}"
    );
}

/// What a person reads when a launch starts nothing: an app this device does not have says so
/// under the card's title, and no refusal shows a package id.
#[test]
fn a_refusal_names_the_app_as_the_shell_names_it() {
    let activity = tilo_app_launch::PlanError::NotInstalled("so.tilo.activity".into());
    assert_eq!(
        refusal_said(&activity),
        "It is not installed on this machine"
    );
    let content = tilo_app_launch::PlanError::ContentUnavailable {
        app: "so.tilo.activity".into(),
    };
    assert!(
        !refusal_said(&content).contains("so.tilo."),
        "no package id: {}",
        refusal_said(&content)
    );
}

/// An app opened with no tab waiting for it (through the applications action, as an agent or
/// a test opens one) that ends at once without ever showing a window never opened: the person
/// is told it could not open, with what the app itself said, whether it exited 0 or not.
/// Chat did that on the e2e-ws-branch box, nineteen times, and left nothing anywhere.
/// `apps` started the app `example.Canvas` as process `id`, `lived` ago, and it ended as `end`
/// saying `said` on stderr.
fn canvas_ended(apps: &Apps, id: u64, lived: std::time::Duration, end: End, said: &[&str]) {
    note_launch(apps, id, "example.Canvas", None, None, lived, Got::Arriving);
    apps.state.borrow().spawner.report(Event {
        id,
        app: "example.Canvas".to_owned(),
        title: "Canvas".to_owned(),
        end,
        killed: false,
        asked_to_end: false,
        said: OutputTail {
            stderr: said.iter().map(|line| (*line).to_owned()).collect(),
            stdout: Vec::new(),
            dropped: 0,
        },
    });
}

/// What the desktop says now, after its pass: each tab's title, and what a tab says in place of
/// a window that did not come or went away, with its button.
fn texts(desktop: &ShellDesktop) -> Vec<String> {
    desktop.pass();
    desktop.with_policy(|policy| {
        let mut shown: Vec<String> = policy.handle().read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(|tab| tab.title.clone())
                .collect()
        });
        for (_, said) in policy.said_in_tabs() {
            shown.extend(
                [said.what, said.why]
                    .into_iter()
                    .filter(|text| !text.is_empty()),
            );
            shown.extend(said.retry);
        }
        shown
    })
}

/// What the answers on the desktop keep behind their Details: what the launch said.
fn details(desktop: &ShellDesktop) -> Vec<String> {
    desktop.pass();
    desktop.with_policy(|policy| {
        policy
            .said_in_tabs()
            .into_iter()
            .map(|(_, said)| said.technical_details)
            .collect()
    })
}

/// An app that ended without ever showing a window never opened, however long it ran first
/// (Chat joined its workspace's record and went, on the e2e-ws-branch box) and however it
/// ended: the person is told it could not open, with what it said, though no tab waited.
#[test]
fn an_app_that_ends_without_ever_showing_a_window_could_not_open() {
    for (n, end) in [End::exited(0), End::exited(1)].into_iter().enumerate() {
        let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
        let desktop = shell();
        apps.attach(desktop.clone());
        let id = u64::try_from(n).unwrap() + 1;
        let lived = std::time::Duration::from_secs(30);
        canvas_ended(&apps, id, lived, end, &["the display refused this app"]);
        apps.follow_ended();
        let shown = texts(&desktop);
        assert!(
            shown.iter().any(|text| text.contains("could not open")),
            "{n}: the person is told Canvas could not open: {shown:?}"
        );
        // What the app printed waits behind the card's Details, kept in the app's own words for
        // whoever opens them.
        let detailed = details(&desktop);
        assert!(
            detailed
                .iter()
                .any(|details| details.contains("the display refused this app")),
            "{n}: with what Canvas said: {detailed:?}"
        );
    }
}

/// A launch that failed says the app's own last word, not the launcher's account of how it ended:
/// "tilo-launcher: … ended: exit status: 1" is nothing a person can act on, so it is no reason the
/// card gives.
#[test]
fn a_launchers_account_of_a_failed_launch_is_not_its_reason() {
    let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
    let desktop = shell();
    apps.attach(desktop.clone());
    canvas_ended(
        &apps,
        1,
        std::time::Duration::from_secs(1),
        End::exited(1),
        &[
            "the display refused this app",
            "tilo-launcher: so.tilo.canvas ended: exit status: 1",
        ],
    );
    apps.follow_ended();
    let shown = texts(&desktop);
    assert!(
        shown
            .iter()
            .any(|text| text.contains("the display refused this app")),
        "the app's own reason is what the card says: {shown:?}"
    );
    assert!(
        !shown.iter().any(|text| text.contains("tilo-launcher:")),
        "the launcher's account is never the reason: {shown:?}"
    );
}

/// An app that showed a window and later ended cleanly opened: its end is no notice, even once
/// its window is gone from the shell.
#[test]
fn an_app_that_showed_a_window_and_ended_is_no_notice() {
    let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
    let desktop = shell();
    apps.attach(desktop.clone());
    let lived = std::time::Duration::from_secs(30);
    // A process no longer running stands for the launch's: only its descent matters here.
    let pid = 4_242;
    note_launch(
        &apps,
        1,
        "example.Canvas",
        Some(pid),
        None,
        lived,
        Got::Arriving,
    );
    let window = Window {
        id: 11,
        app_id: Some("example.Canvas".into()),
        title: Some("Canvas".into()),
        pid: Some(pid),
        ..Window::default()
    };
    desktop
        .with_policy(|policy| policy.map(window))
        .expect("the window maps");
    texts(&desktop);
    // The session's pass reads the window the desktop placed as this launch's own.
    apps.follow_ended();
    assert_eq!(
        apps.launch_state("example.Canvas", WORKSPACE),
        Some(LaunchState::Landed),
        "a window of the launch's own process appeared"
    );
    desktop.with_policy(|policy| policy.unmap(11));
    texts(&desktop);
    apps.state.borrow().spawner.report(Event {
        id: 1,
        app: "example.Canvas".to_owned(),
        title: "Canvas".to_owned(),
        end: End::exited(0),
        killed: false,
        asked_to_end: false,
        said: OutputTail::default(),
    });
    apps.follow_ended();
    let shown = texts(&desktop);
    assert!(
        !shown.iter().any(|text| text.contains("could not open")),
        "Canvas opened, and closing is no notice: {shown:?}"
    );
}

#[test]
fn an_app_closed_for_memory_says_so_instead_of_crashing() {
    let closed = showed_after(End::signalled(9).remembering(Cause::Kernel), false);
    assert!(
        closed
            .iter()
            .any(|text| text.contains("was closed to free memory")),
        "the notice says the session ended it: {closed:?}"
    );
    assert!(
        !closed.iter().any(|text| text.contains("stopped working")),
        "a closing the session did is not a crash: {closed:?}"
    );
    assert!(
        closed.iter().any(|text| text == "Reopen"),
        "the notice offers a way back: {closed:?}"
    );

    let runaway = showed_after(End::signalled(9).remembering(Cause::Runaway), false);
    assert!(
        runaway
            .iter()
            .any(|text| text.contains("was using too much memory")),
        "a runaway app is told apart: {runaway:?}"
    );

    let crashed = showed_after(End::signalled(9), false);
    assert!(
        crashed
            .iter()
            .any(|text| text.starts_with("Canvas stopped working")),
        "without a cause the same end is still a crash: {crashed:?}"
    );
}

/// A stop the person asked for — Quit in the app's own place — is theirs, as closing the app's
/// last window is: the tab closes quietly, and nothing says it stopped working.
#[test]
fn a_stop_the_person_asked_for_is_not_a_crash() {
    let quit = showed_after(End::signalled(libc::SIGTERM), true);
    assert!(
        !quit.iter().any(|text| text.contains("stopped working")),
        "the person's own Quit is no crash: {quit:?}"
    );
    assert!(
        !quit.iter().any(|text| text == "Reopen"),
        "and nothing offers to reopen what the person just closed: {quit:?}"
    );
    let crashed = showed_after(End::signalled(libc::SIGTERM), false);
    assert!(
        crashed
            .iter()
            .any(|text| text.starts_with("Canvas stopped working")),
        "the same end the session did not ask for is still a crash: {crashed:?}"
    );
}

/// A logout or a shutdown stops the session's whole process group at once: an app it ends in
/// that stroke did not stop working, so its tab is left as the person had it for the next login
/// to open again, and offers no reopen.
#[test]
fn an_app_ended_by_the_session_ending_is_not_a_crash() {
    let ended = showed_after_in(End::signalled(libc::SIGTERM), false, || true);
    assert!(
        !ended.iter().any(|text| text.contains("stopped working")),
        "an app the session's own stop ended is no crash: {ended:?}"
    );
    assert!(
        !ended.iter().any(|text| text == "Reopen"),
        "and its tab offers no reopen: {ended:?}"
    );
}

/// A Windows program is asked by its entry in the manager's list, which is no name a person
/// reads: the tab that answers for it names it as the manager does. One that never showed a
/// window leaves no tab at all — the manager that started it says what became of it.
#[test]
fn a_windows_program_is_named_by_its_name_and_a_windowless_one_leaves_no_tab() {
    const ENTRY: &str = "windows-1c8152bfbdd80870708389a79cf20728";
    let started = |apps: &Apps, instance: Option<u64>| {
        apps.state.borrow_mut().launched.insert(
            1,
            Launching {
                asked: format!("{}{ENTRY}", crate::windows_compat::ASKED_PREFIX),
                app: "app-1c8152bfbdd80870708389a79cf20728".to_owned(),
                workspace: WORKSPACE.to_owned(),
                pid: None,
                instance,
                window: None,
                tab: None,
                started: std::time::Instant::now(),
                got: Got::Arriving,
            },
        );
    };
    let ended = |apps: &Apps| {
        apps.state.borrow().spawner.report(Event {
            id: 1,
            app: "app-1c8152bfbdd80870708389a79cf20728".to_owned(),
            title: "Plink".to_owned(),
            end: End::exited(1),
            killed: false,
            asked_to_end: false,
            said: OutputTail::default(),
        });
        apps.follow_ended();
    };

    // A program that never showed a window: no tab, and nothing that names an entry.
    let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
    let desktop = shell();
    apps.attach(desktop.clone());
    started(&apps, None);
    ended(&apps);
    let shown = texts(&desktop);
    assert!(
        !shown.iter().any(|text| text.contains("could not open")),
        "a Windows program that never showed leaves no tab: {shown:?}"
    );

    // A tab the shell held for it says it did not open, naming the program as its manager does.
    let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
    let desktop = shell();
    apps.attach(desktop.clone());
    desktop
        .with_policy(|policy| {
            policy.handle().dispatch(
                tilo_session_core::desktop::navigation::ChromeEffect::OpenApp {
                    app: "example.Canvas".into(),
                    beside: None,
                    background: None,
                    float: None,
                },
            )
        })
        .unwrap();
    desktop.pass();
    let asked = desktop.take_launches();
    assert_eq!(asked.len(), 1, "a tab is held");
    started(&apps, asked[0].tab);
    ended(&apps);
    let shown = texts(&desktop);
    assert!(
        shown
            .iter()
            .any(|text| text.contains("Plink") && text.contains("could not open")),
        "the tab names Plink, as its manager does: {shown:?}"
    );
    assert!(
        !shown.iter().any(|text| text.contains(ENTRY)),
        "and never the entry it is asked by: {shown:?}"
    );
}

/// A program the person quits before its window came leaves no tab behind: the tab the shell
/// held for that window closes, rather than waiting forever or answering a quit it did not cause.
#[test]
fn a_stop_the_person_asked_for_before_the_window_came_closes_its_tab() {
    let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
    let desktop = shell();
    apps.attach(desktop.clone());
    desktop
        .with_policy(|policy| {
            policy.handle().dispatch(
                tilo_session_core::desktop::navigation::ChromeEffect::OpenApp {
                    app: "example.Canvas".into(),
                    beside: None,
                    background: None,
                    float: None,
                },
            )
        })
        .unwrap();
    desktop.pass();
    let asked = desktop.take_launches();
    assert_eq!(asked.len(), 1, "the person asked for Canvas");
    note_launch(
        &apps,
        1,
        "example.Canvas",
        None,
        asked[0].tab,
        std::time::Duration::ZERO,
        Got::Arriving,
    );
    apps.state.borrow().spawner.report(Event {
        id: 1,
        app: "example.Canvas".to_owned(),
        title: "Canvas".to_owned(),
        end: End::signalled(libc::SIGTERM),
        killed: false,
        asked_to_end: true,
        said: OutputTail::default(),
    });
    apps.follow_ended();
    let shown = texts(&desktop);
    assert!(
        !shown.iter().any(|text| text.contains("could not open")),
        "the person's Quit did not fail to open anything: {shown:?}"
    );
    assert!(
        !shown.iter().any(|text| text.contains("stopped working")),
        "and it is no crash: {shown:?}"
    );
    let tab = asked[0].tab.expect("a tab was held");
    assert!(
        !desktop.with_policy(|policy| policy.handle().read(|tree| tree.tab(tab).is_some())),
        "the tab held for the window is closed: {shown:?}"
    );
}

/// A hosted window's instance is handed the file its launch opened as the content it is: the same
/// `File` a chooser gives the app, its hash and its name, never a path its process has nowhere to
/// read.
#[test]
fn a_hosted_window_opens_a_file_by_its_content_and_name() {
    let hash = "ab".repeat(32);
    let launch =
        Launch::parse(&format!("content:so.tilo.notes:{hash}:plan.md")).expect("a content launch");
    let opens = window_opens(&launch, None, true).expect("a hosted window opens a file");
    assert_eq!(
        opens.content,
        Some(tilo_app_launch::ContentOpen {
            hash,
            name: Some("plan.md".into()),
        })
    );
    assert_eq!(
        opens.open, None,
        "an app with no path of its own is handed none"
    );
    // A window that holds its app's data opens the path itself, and no content.
    assert_eq!(
        window_opens(&launch, None, false)
            .expect("a window of an app that holds its data")
            .content,
        None
    );
    // A path is no file for a hosted window: nothing in its process could read it.
    let path = Launch::Package {
        app: "so.tilo.notes".into(),
        open: Some(tilo_app_launch::Open::File("plan.md".into())),
    };
    assert!(
        window_opens(&path, None, true).is_err(),
        "a hosted window is handed no path"
    );
    // A link is the text a hosted window's `open-link` shows, never a path its process reads.
    let link = Launch::Package {
        app: "so.tilo.tube".into(),
        open: Some(tilo_app_launch::Open::Uri("tube:watch?v=abc".into())),
    };
    let opens = window_opens(&link, None, true).expect("a hosted window opens a link");
    assert_eq!(opens.link.as_deref(), Some("tube:watch?v=abc"));
    assert_eq!(opens.open, None, "a link is no file path");
    // A window of an app that holds its data takes the link as the one argument it opens with.
    assert_eq!(
        window_opens(&link, None, false)
            .expect("a window of an app that holds its data")
            .open
            .as_deref(),
        Some("tube:watch?v=abc")
    );
    // The process a hosted window starts is asked for the window alone: the link rides its
    // instance, and a tilolang process takes no link on its command line.
    assert_eq!(
        process_launch(&link, true),
        Launch::Package {
            app: "so.tilo.tube".into(),
            open: None,
        }
    );
    assert_eq!(process_launch(&link, false), link);
}
