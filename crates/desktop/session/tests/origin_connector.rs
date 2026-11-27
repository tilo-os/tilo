//! A tilolang app whose types hold an outside service's resources keeps them in step with that
//! service in a real session (`docs/architecture/apps.md` §1, §8).
//! The app is the `YouTube` client (`apps/tl/tube`), its package a fixture installed as an app
//! whose signed manifest declares `origins = ["youtube"]` . The device's daemon starts the app's
//! connector as a sandboxed process — here the launcher is a stand-in that runs a child of this
//! test binary as that process — and the child runs the platform's
//! `tilolang_youtube::connector::serve` loop over a scripted service: the same connector the system
//! ships, answering a script instead of `YouTube` .
//! What this proves is the whole path and nothing test-only about it: a search action an instance
//! runs queues an outside effect, the session's services hand the effect to the connector over the
//! socket the daemon started, and the answer lands in the client's record — its videos and the
//! search's settled state — which the session publishes and its app wire reads. A video the answer
//! brought, played, is asked for its stream the same way, and the record holds where it plays from
//! while the client's session plays it. The client's call into the playback service is another
//! app's action, so the person answers it first, as their asked record: their yes plays the video,
//! their way out plays nothing.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session outlives every call"
)]

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::json;
use tilo_session::{AppActions, AppSocket, Launches, Options, Session};
use tilo_session_client::daemon_documents::{DaemonShell, inherited_link, whole};

use crate::common::private_home;

/// The `YouTube` client, as a package names it.
const TUBE: &str = "so.tilo.tube";
/// The channel the scripted `YouTube` service answers with.
const NAME: &str = "Low Level Learning";
/// The video the scripted service answers with, and the service's own key for it.
const VIDEO: &str = "I rewrote my window compositor in Rust";
const KEY: &str = "v-compositor";
/// Where the scripted service says the video's bytes play from.
const STREAM: &str = "https://media.example/v-compositor.mp4";
/// Set only in the child this test starts: the connector process.
const CHILD_ENV: &str = "TILO_YOUTUBE_CONNECTOR_TEST_CHILD";
/// The directory the launcher stand-in writes what it ran into, and the child says it attached
/// there: the test reads them to see the connector process started and took the socket.
const DIR_ENV: &str = "TILO_YOUTUBE_CONNECTOR_TEST_DIR";
/// The origin the launcher was asked for, which it names to the child it starts.
const NAME_ENV: &str = "TILO_YOUTUBE_CONNECTOR_TEST_ORIGIN";

/// Where the stand-in launcher records what it was asked.
fn launcher_ran(dir: &Path) -> std::path::PathBuf {
    dir.join("launcher-ran")
}

/// Where the connector process of `origin` records that it took the socket it was handed.
fn child_ran(dir: &Path, origin: &str) -> std::path::PathBuf {
    dir.join(format!("child-{origin}-ran"))
}

/// The child side, run in a copy of this test binary: it is the app's connector process. Without
/// [`CHILD_ENV`] it is not the child and has nothing to do.
#[test]
fn youtube_connector_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    // The launcher's release: the descriptor the connector's socket came in on. The stand-in
    // launcher is the launcher's sandbox here: it hands the socket straight into this process,
    // where a real launch would hand it in as the `tilolang-origin` service.
    if runtime_portal::launch_hold::wait_on_stdin().is_err() {
        return;
    }
    let Some(socket) = runtime_portal::launch_hold::take_window_link() else {
        return;
    };
    if let (Some(dir), Some(origin)) = (std::env::var_os(DIR_ENV), std::env::var_os(NAME_ENV)) {
        let _ = std::fs::write(child_ran(Path::new(&dir), &origin.to_string_lossy()), "");
    }
    let attachments = runtime_portal::origin_connector::attachments(socket, |failed| {
        if let Some(error) = failed {
            eprintln!("the connector's socket failed: {error}");
        }
    });
    // The production connector loop, over the scripted service rather than YouTube.
    tilolang_youtube::connector::serve(&tilolang_youtube::scripted(scripted()), attachments);
}

/// The scripted `YouTube` service: one search's answer.
fn scripted() -> tilolang_youtube::Scripted {
    tilolang_youtube::Scripted {
        searches: [
            (
                "compositor".to_owned(),
                tilolang_youtube::SearchPage {
                    query: "compositor".to_owned(),
                    channels: vec![tilolang_youtube::ChannelItem {
                        key: "UC-lowlevel".to_owned(),
                        name: NAME.to_owned(),
                        ..tilolang_youtube::ChannelItem::default()
                    }],
                    playlists: Vec::new(),
                    videos: vec![tilolang_youtube::VideoItem {
                        key: "v-compositor".to_owned(),
                        title: VIDEO.to_owned(),
                        channel_key: "UC-lowlevel".to_owned(),
                        channel_name: NAME.to_owned(),
                        published: "2026-11-01T12:00:00Z".to_owned(),
                        duration: 900,
                        views: 12_000,
                        ..tilolang_youtube::VideoItem::default()
                    }],
                },
            ),
            // Two channels for a creator two of them share a name with: the canvas shows the rows and
            // the person picks (`several`), rather than guessing.
            (
                "shared".to_owned(),
                tilolang_youtube::SearchPage {
                    query: "shared".to_owned(),
                    channels: vec![
                        tilolang_youtube::ChannelItem {
                            key: "UC-one".to_owned(),
                            name: "Shared Name".to_owned(),
                            ..tilolang_youtube::ChannelItem::default()
                        },
                        tilolang_youtube::ChannelItem {
                            key: "UC-two".to_owned(),
                            name: "Shared Name II".to_owned(),
                            ..tilolang_youtube::ChannelItem::default()
                        },
                    ],
                    playlists: Vec::new(),
                    videos: Vec::new(),
                },
            ),
            // The origin answers an empty page: a search that finishes and finds nothing.
            (
                "empty".to_owned(),
                tilolang_youtube::SearchPage {
                    query: "empty".to_owned(),
                    channels: Vec::new(),
                    playlists: Vec::new(),
                    videos: Vec::new(),
                },
            ),
            // "broken" is scripted to fail: the scripted service answers nothing for it, and the
            // origin settles the search as `failed` with its reason.
        ]
        .into_iter()
        .collect(),
        // The address the video the search found plays from: the client asks for it when the
        // person presses Play.
        streams: [(KEY.to_owned(), STREAM.to_owned())].into_iter().collect(),
        ..tilolang_youtube::Scripted::default()
    }
}

/// Write the launcher the device's daemon starts the apps' connectors through: the stand-in runs
/// this test binary as the connector process, on the hold the launcher is handed. The apps are this
/// run's own build, installed in the store every session of it reads
/// ([`crate::common::app_store`]), the `YouTube` client's signed package among them; its manifest
/// declares the `youtube` origin.
fn connector_launcher(dir: &Path) -> tilo_test_env::StandIn {
    // The launcher the daemon starts connectors through: it runs this test binary as the connector
    // process, keeping the hold the launcher was handed as stdin, and records what it was asked.
    tilo_test_env::stand_in(
        dir.join("tilo-launcher"),
        &format!(
            "export {CHILD_ENV}=1\n\
             export {DIR_ENV}='{dir}'\n\
             origin=''\n\
             last=''\n\
             for arg in \"$@\"; do [ \"$last\" = --tilolang-connector ] && origin=\"$arg\"; last=\"$arg\"; done\n\
             export {NAME_ENV}=\"$origin\"\n\
             printf '%s\\n' \"$*\" >> '{ran}'\n\
             exec '{exe}' --exact origin_connector::youtube_connector_child --nocapture\n",
            dir = dir.display(),
            ran = launcher_ran(dir).display(),
            exe = std::env::current_exe()
                .expect("this test's program")
                .display(),
        ),
    )
    .unwrap()
}

/// A headless session as a computer starts one: the shell of a device daemon of the test's own
/// ([`crate::common::on_its_daemon`]), with the seeded apps published (the `YouTube` client among
/// them) and the client's connector started by that daemon through the test's stand-in launcher.
/// The apps are this run's own build, installed in the store a device has them in: the client's
/// package is a signed `.tapp` whose manifest declares the `youtube` origin, so the daemon starts
/// its connector the way it starts any installed app's.
fn session(root: &Path) -> (Session, DaemonShell) {
    // The store this run installed the apps in, as a device has them: the daemon and the session's
    // own launcher read the packages from there, so the connector's app is the client's package.
    crate::common::app_store();
    let launcher = connector_launcher(root);
    // The launcher the daemon starts a connector through: the test's stand-in, which runs this
    // test binary as the connector process.
    tilo_test_env::pin("TILO_LAUNCHER_BIN", launcher.path());
    let apps = Rc::new(tilo_session::apps::Apps::with_launcher(
        tilo_app_launch::SessionEnvironment::from_process(),
        None,
        Some(launcher.path().to_owned()),
    ));
    // The person's own link to their daemon, beside the session's: what installs the client in
    // the workspace, as the person's own install does.
    let mut person_link = None;
    let mut session = crate::common::on_its_daemon_with(
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        |device| {
            person_link = Some(
                device
                    .daemon
                    .link_person(tilo_test_env::hang_bound())
                    .expect("the daemon makes the person's link"),
            );
            Options {
                collab: true,
                state: Some(root.join("state")),
                services: crate::common::seeded_apps_of(device),
                asks: Some(tilo_service::asks::channel()),
                // The app wire where the launcher hands it to the apps, as a computer's session
                // serves it: what an app's connection, the session's services' own calls as an
                // app among them, is attested on and handed to the daemon.
                app_socket: AppSocket::Launcher,
                mcp_server: Some(root.join("agent.sock")),
                test_control: Some(root.join("test-control.sock")),
                apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
                launch: Some(apps),
                launches: Arc::new(Launches::default()),
                ..Options::default()
            }
        },
    );
    // The shell shows the workspace the daemon holds, and the apps' records are kept there as a
    // person's session keeps them: what the client writes and what its origin answers are the
    // workspace's, where the app wire reads them.
    crate::common::wait_for(&mut session, "the workspace the shell shows", |session| {
        session.handle().read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
    });
    crate::common::kept_in_workspace(&mut session);
    // The person's own link, kept for what they do beside the session: installing the client, and
    // answering the call it makes of another app, as they would on any device of theirs.
    let runtime = tilo_rt::app().handle().clone();
    let person = DaemonShell::new(
        inherited_link(Some(person_link.expect("the person's link")), &runtime)
            .map(|(link, _)| link),
        runtime,
    );
    // The person installs the client in the workspace, through the workspace's own install
    // action: what lets it keep its records there and use what its package names.
    install_in_workspace(&mut session, &person, TUBE);
    // The daemon starts the client's connector through the launcher it was handed, and that
    // process takes the socket the origin is reached over.
    let started = |dir: &Path| {
        std::fs::read_to_string(launcher_ran(dir))
            .is_ok_and(|ran| ran.contains("--tilolang-connector youtube"))
    };
    crate::common::until(&mut session, "the client's connector started", |_| {
        started(root)
    });
    crate::common::until(&mut session, "the client's connector attaching", |_| {
        child_ran(root, "youtube").is_file()
    });
    (session, person)
}

/// Install `app`, as this run built it, in the workspace the session shows, as its person does
/// over their own link `person`: its package handed to the workspace's install action.
fn install_in_workspace(session: &mut Session, person: &DaemonShell, app: &str) {
    let shell = person.clone();
    let workspace = session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
        .expect("the workspace the shell shows");
    let package = crate::common::dev_apps::package(app);
    tilo_runtime::device::install_package(&package, &mut |verb, arguments| {
        let request = crate::common::invocation(
            &tilo_actions::remote::RemoteActionTarget::workspace_governance(&workspace, verb),
            &arguments,
            None,
            None,
        );
        let (shell, workspace) = (shell.clone(), workspace.clone());
        let (answered, answer) = std::sync::mpsc::sync_channel(1);
        let runtime = shell.runtime().clone();
        let _task = runtime.spawn(async move {
            let settled = match shell.link() {
                Ok(link) => link.dispatch().settled(Some(workspace), request).await,
                Err(error) => Err(error),
            };
            let _ = answered.send(settled);
        });
        crate::common::wait_for(session, "the workspace's answer", |_| {
            answer.try_recv().ok()
        })
    })
    .unwrap_or_else(|error| panic!("the person installs {app} in the workspace: {error}"));
}

#[test]
fn an_outside_effect_reaches_the_connector_and_the_answer_lands_in_the_record() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let dir = private_home();
    let (mut session, person) = session(dir.path());
    let (root, video) = searched(&mut session);

    // The person presses the video page's Play. Tube's call into playback is another app's action,
    // so it waits for the person as their asked record instead of running: nothing plays yet, and
    // the record of the video keeps no address until the client's code runs.
    play(&mut session, &video);
    let workspace = workspace(&session);
    let (action, asked) = waiting_ask(&mut session, &person, &workspace);
    assert!(
        action.ends_with("#play_list"),
        "the person is asked about the client's call into playback, which is {action}"
    );
    assert!(
        !playing(&session, &root, KEY),
        "nothing plays before the person answers"
    );

    // Their yes runs the call the press made, as the shell's own answer does: the asked record's
    // `resolve`, called on the person's connection as any action. The client's code goes on to
    // call playback once more, which waits for them the same way.
    resolve(&mut session, &person, &workspace, &asked, "approve");
    // It runs, and the client's code goes on to its next call.
    let _ = asked_ended(&mut session, &person, &workspace, &asked);
    let (action, asked) = waiting_ask(&mut session, &person, &workspace);
    assert!(
        action.ends_with("#resume"),
        "the person is asked about the client's next call into playback, which is {action}"
    );
    resolve(&mut session, &person, &workspace, &asked, "approve");
    crate::common::until(
        &mut session,
        "the video's address in the record",
        |session| streaming(session, &video),
    );
    crate::common::until(&mut session, "the player holds the video", |session| {
        playing(session, &root, KEY)
    });
}

/// The person declines the call a press into playback made: nothing plays, and the call ends in
/// the dispatch's words.
#[test]
fn a_play_the_person_declines_into_playback_runs_nothing() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let dir = private_home();
    let (mut session, person) = session(dir.path());
    let (root, video) = searched(&mut session);

    play(&mut session, &video);
    let workspace = workspace(&session);
    let (action, asked) = waiting_ask(&mut session, &person, &workspace);
    assert!(
        action.ends_with("#play_list"),
        "the person is asked about the client's call into playback, which is {action}"
    );

    // Their way out declines it: the call ends, and nothing plays.
    resolve(&mut session, &person, &workspace, &asked, "decline");
    let ended = asked_ended(&mut session, &person, &workspace, &asked)
        .expect("the declined call keeps its record");
    assert_eq!(
        ended.status,
        tilo_action_services::workspace_invocations::InvocationStatus::Cancelled,
        "the call the person declined ends, not run: {ended:?}"
    );
    for _ in 0..50 {
        session
            .step(std::time::Duration::from_millis(16))
            .expect("step");
    }
    assert!(
        !playing(&session, &root, KEY),
        "the call the person declined plays nothing"
    );
}

/// The person's search through the session's app wire, as any action: the outside effect it queues
/// goes to the connector the session started for the app's origin. The answer lands in the client's
/// record (the video the scripted service found and the channel it names), and this answers the
/// client's root and the video it found.
fn searched(session: &mut Session) -> (String, String) {
    // The client's root is published by the seeded apps, and the video the scripted service answers
    // with is not in it: only the scenario's own are.
    crate::common::until(session, "the client's root", |session| {
        session
            .resources()
            .iter()
            .any(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
    });
    assert!(
        !record_says(session, VIDEO),
        "the answer's video is not in the record before the search"
    );

    let root = session
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
        .expect("the client's root");
    // The client's search: the one action of its root that asks a query.
    let search = root
        .actions
        .iter()
        .find(|action| action.params().iter().any(|param| param.name == "query"))
        .map_or_else(
            || {
                panic!(
                    "the client offers its search: {:?}",
                    root.actions
                        .iter()
                        .map(|action| (&action.id, &action.label))
                        .collect::<Vec<_>>()
                )
            },
            |action| action.id.clone(),
        );
    session
        .invoke(
            root.id.as_str(),
            &search,
            &serde_json::Map::from_iter([("query".to_owned(), json!("compositor"))]),
        )
        .expect("the search action runs");

    // The answer lands in the client's record: the video the scripted service found and the channel
    // it names, each a resource of the client's the app wire reads, and the search settled
    // (`search_state` `done` , so the client's view stops saying it searches).
    crate::common::until(session, "the answer in the record", |session| {
        record_says(session, VIDEO) && record_says(session, NAME)
    });
    let videos = client_items(session, "Video");
    assert!(
        videos.iter().any(|(_, title)| title == VIDEO),
        "the video YouTube answered with is a resource of the client's: {videos:?}"
    );
    let channels = client_items(session, "Channel");
    assert!(
        channels.iter().any(|(_, name)| name == NAME),
        "the channel YouTube answered with is a resource of the client's: {channels:?}"
    );
    let video = videos
        .iter()
        .find(|(_, title)| title == VIDEO)
        .map(|(id, _)| id.clone())
        .expect("the video the search found");
    (root.id.as_str().to_owned(), video)
}

/// The person presses the video page's Play: the client asks the service where the video's bytes
/// play from, and its code plays the video in the client's session.
fn play(session: &mut Session, video: &str) {
    let resources = session.resources();
    let page = resource(&resources, video).expect("the video is a resource of the client's");
    let play = page
        .actions
        .iter()
        .find(|action| action.id == "play")
        .map_or_else(
            || {
                panic!(
                    "the video offers its Play: {:?}",
                    page.actions
                        .iter()
                        .map(|action| (&action.id, &action.label))
                        .collect::<Vec<_>>()
                )
            },
            |action| action.id.clone(),
        );
    session
        .invoke(video, &play, &serde_json::Map::new())
        .expect("the press is asked before it runs");
}

/// The workspace the session shows, by its id.
fn workspace(session: &Session) -> String {
    session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
        .expect("the workspace the shell shows")
}

/// The asked record of the call that waits for the person in `workspace`, as the device's daemon
/// lists its invocations: what `resolve` is called on, and the app's action it asks about.
fn waiting_ask(session: &mut Session, person: &DaemonShell, workspace: &str) -> (String, String) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(asked) = invocations(session, person, workspace)
            .into_iter()
            .find(|invocation| {
                invocation.status
                    == tilo_action_services::workspace_invocations::InvocationStatus::Waiting
            })
            .and_then(|invocation| invocation.asked)
        {
            return (asked.action.clone(), asked.record.record.clone());
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a call waits for the person's answer: none did"
        );
        session
            .step(std::time::Duration::from_millis(16))
            .expect("step");
    }
}

/// Wait until the call whose asked record is `record` no longer waits for the person, and say
/// what its record holds then, when it holds it.
fn asked_ended(
    session: &mut Session,
    person: &DaemonShell,
    workspace: &str,
    record: &str,
) -> Option<tilo_action_services::workspace_invocations::Invocation> {
    let deadline = tilo_test_env::hang_deadline();
    let holds = |invocation: &tilo_action_services::workspace_invocations::Invocation| {
        invocation
            .asked
            .as_ref()
            .is_some_and(|asked| asked.record.record == record)
    };
    loop {
        let held = invocations(session, person, workspace);
        if !held.iter().any(|invocation| {
            invocation.status
                == tilo_action_services::workspace_invocations::InvocationStatus::Waiting
                && holds(invocation)
        }) {
            return held.into_iter().find(holds);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the call whose asked record is {record} ended: it still waits"
        );
        session
            .step(std::time::Duration::from_millis(16))
            .expect("step");
    }
}

/// Answer the asked record `record`, as the person on their own connection does: its own
/// `resolve`, called by its address as any caller calls it, with `resolution` (`approve` or
/// `decline`).
fn resolve(
    session: &mut Session,
    person: &DaemonShell,
    workspace: &str,
    record: &str,
    resolution: &str,
) {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
    let target = tilo_actions::remote::RemoteActionTarget {
        provider: tilo_action_services::workspace_invocations::PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(record, tilo_actions::remote::asked::RESOLVE),
    };
    let request =
        crate::common::invocation(&target, &json!({"resolution": resolution}), None, None);
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(request))
        .encode()
        .expect("an invocation encodes");
    let call = ShellCall::Action {
        workspace: Some(workspace.to_owned()),
        frame,
        caller: None,
        pressed: None,
    };
    let shell = person.clone();
    let reply = answered(session, person, async move { shell.ask(call).await });
    assert!(
        matches!(reply, ShellReply::Action(_)),
        "the person's answer reached the asked record: {reply:?}"
    );
}

/// The invocations the device's daemon keeps of `workspace`, newest first: what its record of
/// calls answers the person.
fn invocations(
    session: &mut Session,
    person: &DaemonShell,
    workspace: &str,
) -> Vec<tilo_action_services::workspace_invocations::Invocation> {
    let shell = person.clone();
    let workspace = workspace.to_owned();
    let reading = answered(session, person, async move {
        shell.read(whole(&workspace, "tilo.invocations", "")).await
    });
    let value = reading["records"]
        .as_array()
        .and_then(|records| records.first())
        .and_then(|record| record.get("value"))
        .cloned()
        .unwrap_or_else(|| panic!("the invocations read answers a value: {reading}"));
    serde_json::from_value(value["items"].clone()).unwrap_or_else(|error| {
        panic!("the invocations read as invocations: {error}; it says {reading}")
    })
}

/// What the daemon answered `ask` with, running the session's frames meanwhile: the session's own
/// loop is what the daemon's answers land on.
fn answered<T: Send + 'static>(
    session: &mut Session,
    person: &DaemonShell,
    ask: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> T {
    let (answered, answer) = std::sync::mpsc::sync_channel(1);
    let _task = person.runtime().spawn(async move {
        let _ = answered.send(ask.await);
    });
    let deadline = tilo_test_env::hang_deadline();
    loop {
        session
            .step(std::time::Duration::from_millis(2))
            .expect("step");
        match answer.try_recv() {
            Ok(Ok(value)) => return value,
            Ok(Err(error)) => panic!("the person's connection was refused: {error}"),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("the person's connection stopped before it answered")
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the person's connection answered: it never did"
        );
    }
}

/// The resource `id` among `resources`, its own or one of its children's: what the client
/// publishes, nested as each root's children are.
fn resource<'a>(
    resources: &'a [tilo_actions::ResourceSpec],
    id: &str,
) -> Option<&'a tilo_actions::ResourceSpec> {
    resources.iter().find_map(|held| {
        if held.id.as_str() == id {
            Some(held)
        } else {
            resource(&held.children, id)
        }
    })
}

/// Whether the client's record of the video `id` holds where its bytes play from, as the app wire
/// reads it: the address the service answered with.
fn streaming(session: &Session, id: &str) -> bool {
    session.read(id).is_some_and(|reading| {
        let mut texts = Vec::new();
        texts_in(&reading, &mut texts);
        texts.iter().any(|text| text == STREAM)
    })
}

/// Whether the client's session plays the video keyed `key`: the playback the root `root` names
/// as what plays names that video, and is `playing`. A video's page draws its player only then.
fn playing(session: &Session, root: &str, key: &str) -> bool {
    /// The id a playback's state reads `playing` under, as the playback package's ids name it:
    /// a reading names an enum's variant by its id.
    fn playing_id() -> &'static str {
        include_str!("../../../../apps/tl/playback/app.ids")
            .lines()
            .find_map(|line| line.strip_prefix("PlaybackState.playing = "))
            .map(str::trim)
            .expect("the playback package names its playing state")
    }

    let Some(reading) = session.read(root) else {
        return false;
    };
    let mut refs = Vec::new();
    playing_refs(&reading, &mut refs);
    refs.into_iter().any(|id| {
        session.read(&id).is_some_and(|reading| {
            let mut texts = Vec::new();
            texts_in(&reading, &mut texts);
            texts.iter().any(|text| text == key) && texts.iter().any(|text| text == playing_id())
        })
    })
}

/// Every playback `value` names as what plays, at any depth: the root's `playing` is an option,
/// and what it names stands under `some`.
fn playing_refs(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                playing_refs(item, out);
            }
        }
        serde_json::Value::Object(fields) => {
            if let Some(playing) = fields.get("some") {
                playback_refs(playing, out);
            }
            for value in fields.values() {
                playing_refs(value, out);
            }
        }
        _ => {}
    }
}

/// Every playback `value` names, at any depth: each reference whose type is a `Playback`.
fn playback_refs(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                playback_refs(item, out);
            }
        }
        serde_json::Value::Object(fields) => {
            if let (Some(id), Some(ty)) = (
                fields.get("ref").and_then(serde_json::Value::as_str),
                fields.get("ty").and_then(serde_json::Value::as_str),
            ) && ty.ends_with("#Playback")
            {
                out.push(id.to_owned());
            }
            for value in fields.values() {
                playback_refs(value, out);
            }
        }
        _ => {}
    }
}

/// Every text `value` holds, at any depth: a record's fields, its items' and its enum variants'.
fn texts_in(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                texts_in(item, out);
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values() {
                texts_in(value, out);
            }
        }
        _ => {}
    }
}

/// The client's items of the kind `kind` (`Video`, `Channel` ): each its published id and the text
/// of each of its fields that is a text, read through the session's app wire.
fn client_items(session: &Session, kind: &str) -> Vec<(String, String)> {
    let Some(root) = session
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
    else {
        return Vec::new();
    };
    let Some(reading) = session.read(root.id.as_str()) else {
        return Vec::new();
    };
    let wanted = format!("#{kind}");
    let mut items = Vec::new();
    for list in reading
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, value)| value["list"].as_array())
    {
        for item in list {
            let (Some(id), Some(ty)) = (item["ref"].as_str(), item["ty"].as_str()) else {
                continue;
            };
            if !ty.ends_with(&wanted) {
                continue;
            }
            if let Some(fields) = session.read(id) {
                for text in fields
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(_, value)| value.as_str())
                {
                    items.push((id.to_owned(), text.to_owned()));
                }
            }
        }
    }
    items
}

/// "youtube videos of <creator>" typed on the canvas places that creator's channel, live: the
/// channel is not in the client's records when the person types, and the client's own search
/// action asks its origin (here a **scripted test origin**, not `YouTube`), whose answer becomes
/// the channel's record and then the place. Nothing here is special-cased for `YouTube`: the query
/// names the client by its app and the channel by its type, and the app's own search action
/// answers it (search.md §4, §7; shell.md §8 "Finding what to place").
#[test]
fn a_live_canvas_query_places_a_channel_the_client_never_held() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let (mut session, _person) = session(root.path());

    // The channel the scripted test origin answers for "compositor" is not held before the query:
    // only the client's own scenario channels are.
    crate::common::until(&mut session, "the client's root", |session| {
        session
            .resources()
            .iter()
            .any(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
    });
    assert!(
        !record_says(&session, NAME),
        "the channel the scripted test origin answers with is not in the record before the query"
    );

    // The person types `youtube videos of <creator>` on the canvas, as the canvas entry's own
    // `place-found` action receives it.
    session
        .invoke(
            "shell:layout",
            "place-found",
            serde_json::json!({"text": "youtube videos of compositor", "x": 80, "y": 60})
                .as_object()
                .expect("arguments"),
        )
        .expect("the arranging action runs");

    // The client's own search action asked its origin, and the answer became the channel's
    // record: the channel the scripted test origin named, held now by the client.
    crate::common::until(
        &mut session,
        "the answered channel in the record",
        |session| record_says(session, NAME),
    );
    // And that channel's page is the place, not the client's feed.
    crate::common::until(&mut session, "the answered channel placed", |session| {
        placed(session)
            .iter()
            .any(|(kind, label)| kind == &format!("tl.{TUBE}.Channel") && label == NAME)
    });
    let held = placed(&session);
    assert!(
        held.iter()
            .any(|(kind, label)| kind == &format!("tl.{TUBE}.Channel") && label == NAME),
        "the query places the answered channel's page: {held:?}"
    );
    assert!(
        !held
            .iter()
            .any(|(kind, _)| kind == &format!("tl.{TUBE}.YouTube")),
        "the client's front page is not the place: {held:?}"
    );
}

/// What stands on the canvas, by the kind and label of the resource each place shows.
fn placed(session: &Session) -> Vec<(String, String)> {
    crate::common::placed_resources(session)
}

/// Whether any of the client's records holds `text` in a text field, as the session's app wire reads
/// them.
fn record_says(session: &Session, text: &str) -> bool {
    ["Video", "Channel"].into_iter().any(|kind| {
        client_items(session, kind)
            .iter()
            .any(|(_, held)| held == text)
    })
}

/// The query a place of `session` shows, by the place whose record is that query: the session's
/// own layout reading, which the shell draws it from.
fn query_shown(session: &Session) -> Option<tilo_session_core::layout::LayoutQueryReading> {
    let layout = session.layout().borrow();
    let reading = layout.reading();
    // The place a query stands in shows the query's own resource (`shell:query:<place>`).
    reading
        .places
        .iter()
        .find(|place| place.resource.starts_with("shell:query:"))
        .and_then(|place| layout.query_of(place.key.parse().ok()?))
}

/// Type `text` on the canvas at the shell, as the canvas entry's own `place-found` receives it.
fn type_on_canvas(session: &mut Session, text: &str) {
    session
        .invoke(
            "shell:layout",
            "place-found",
            serde_json::json!({"text": text, "x": 80, "y": 60})
                .as_object()
                .expect("arguments"),
        )
        .expect("the arranging action runs");
}

/// A headless session over the scripted test origin, with the client's root published and nothing
/// the scripted service answers held yet.
fn live_session(root: &Path) -> Session {
    let (mut session, _person) = session(root);
    crate::common::until(&mut session, "the client's root", |session| {
        session
            .resources()
            .iter()
            .any(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
    });
    session
}

/// "youtube videos of <creator>" that the origin answers with two channels of the same name leaves
/// the query with its rows, not a guessed place: a press on a row shows that channel in
/// the same place (search.md §4, §7; shell.md §8 "Finding what to place").
#[test]
fn a_live_canvas_query_that_matches_several_shows_the_rows_it_found() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let mut session = live_session(root.path());

    type_on_canvas(&mut session, "youtube videos of shared");

    // Two channels came back, so the place stands as a query with two rows, not a channel.
    crate::common::until(&mut session, "the query settles with several", |session| {
        query_shown(session).is_some_and(|query| query.state == "several")
    });
    let query = query_shown(&session).expect("the place shows its query");
    assert_eq!(
        query.matches.len(),
        2,
        "the two channels it found: {query:?}"
    );
    assert!(
        query
            .matches
            .iter()
            .all(|found| found.kind == format!("tl.{TUBE}.Channel")),
        "each row is a channel of the client's: {:?}",
        query.matches
    );
    let rows: Vec<String> = query
        .matches
        .iter()
        .map(|found| found.title.clone())
        .collect();
    assert!(
        rows.contains(&"Shared Name".to_owned()) && rows.contains(&"Shared Name II".to_owned()),
        "the rows are the channels it found: {rows:?}"
    );

    // A press on a row shows that channel in the same place: the query's own `show-match` action.
    let place = query.key.clone();
    let address = query.matches[0].address.clone();
    session
        .invoke(
            &format!("shell:query:{place}"),
            "show-match",
            serde_json::json!({ "resource": address })
                .as_object()
                .expect("arguments"),
        )
        .expect("the row places its channel");
    crate::common::until(&mut session, "the row's channel placed", |session| {
        placed(session)
            .iter()
            .any(|(kind, _)| kind == &format!("tl.{TUBE}.Channel"))
    });
}

/// The origin answers an empty page: the search finishes and no channel of the kind it named came
/// in, so the query settles `.none` — "Nothing matched", with Retry — and nothing is placed
/// (shell.md §8, search.md §7).
#[test]
fn a_live_canvas_query_the_origin_answers_empty_settles_matched_nothing() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let mut session = live_session(root.path());

    type_on_canvas(&mut session, "youtube videos of empty");

    crate::common::until(&mut session, "the query matches nothing", |session| {
        query_shown(session).is_some_and(|query| query.state == "none")
    });
    let query = query_shown(&session).expect("the place shows its query");
    assert!(
        query.matches.is_empty(),
        "it found nothing to list: {:?}",
        query.matches
    );
    assert!(
        !placed(&session)
            .iter()
            .any(|(kind, _)| kind == &format!("tl.{TUBE}.Channel")),
        "no channel is placed: {:?}",
        placed(&session)
    );
}
