//! Tilo's API at the session's own socket: a program of this login —
//! `tilolang repl`, `tilo` — answers as the person against the workspaces
//! this desktop holds, because the socket's own permissions and the kernel's
//! account of the peer are what admit it, and the desktop answers as this
//! device's own certificate, kept with its identity. No key is written beside
//! the socket.
//!
//! A request over that socket, as both commands make one: the listing it asks
//! first (`account/workspaces#list`) comes from the workspaces this device
//! entered, and a program it then sends (`POST …/programs`) is carried out in
//! the workspace as the person, through this session's own API.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::json;
use tilo_session::Options;
use tilo_test_env::SpawnUnderEnvLock as _;

use crate::common::{api_account, api_request, until};

/// The headless session that runs as the shell of this computer's device daemon and serves its
/// person's API at `api`: what a program of this login reaches as the person. A computer's session
/// reaches its workspaces over the daemon's shell link, and the API answers from the device's one
/// dispatch on that link, so the daemon is started here as the person's own service starts it,
/// with the person founded on it and their own workspace kept there
/// ([`crate::common::on_its_daemon_with`]).
fn session_with_api(root: &std::path::Path, api: &std::path::Path) -> tilo_session::Session {
    session_serving(root, api, crate::common::seeded_apps_of)
}

/// [`session_with_api`] running the services `services` makes for the device once its daemon
/// serves its person.
fn session_serving(
    root: &std::path::Path,
    api: &std::path::Path,
    services: impl FnOnce(&crate::common::Device) -> Vec<Box<dyn tilo_service::Service>>,
) -> tilo_session::Session {
    crate::common::on_its_daemon_with(
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        |device| Options {
            collab: false,
            state: Some(root.join("state")),
            services: services(device),
            asks: Some(crate::common::person_asked()),
            app_socket: tilo_session::AppSocket::At(root.join("app_wire.sock")),
            api_socket: Some(api.to_owned()),
            ..Options::default()
        },
    )
}

/// Once the thread serving the API is up: the socket's name is there as soon as it is bound, and
/// a connection is what says it serves.
fn served(session: &mut tilo_session::Session, api: &std::path::Path) {
    until(session, "the session's API socket", |_| {
        std::os::unix::net::UnixStream::connect(api).is_ok()
    });
}

/// The workspace the desktop holds, by its id: the list a program of the person's reads first.
fn held_workspace(session: &mut tilo_session::Session, api: &std::path::Path) -> String {
    let mut listed = serde_json::Value::Null;
    until(session, "the desktop's workspace list", |_| {
        listed = api_account(api, "account/workspaces", "list", &json!({}))
            .unwrap_or(serde_json::Value::Null);
        listed
            .get("workspaces")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|workspaces| !workspaces.is_empty())
    });
    listed["workspaces"][0]["id"]
        .as_str()
        .expect("the desktop lists the workspace it entered")
        .to_owned()
}

/// Once the API finds the section at `address`: the session publishes its sections to its
/// daemon, and a program reaches one once the daemon offers it. A section has no read action, so
/// the read answers that; only a section not offered yet is not found.
fn found_at_the_api(session: &mut tilo_session::Session, api: &crate::common::Api, address: &str) {
    crate::common::wait_for(session, "the section at the API", |session| {
        session.settle_services();
        let found = api
            .send(&format!("resources/{address}"), None)
            .answer(session);
        (!found.is_err_and(|why| why.ends_with("(not_found)"))).then_some(())
    });
}

/// A setter of a settings section, called through the API as a generated SDK calls it, is typed
/// by the sections' own surface whichever app shows the section: the person's and the device's
/// sections are both set, as the session reads them. A value of the wrong type is refused, and so are types
/// written for another surface of the sections.
#[test]
fn a_sections_setter_is_typed_by_the_sections_surface_at_the_sessions_api() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("api.sock");
    let mut session = session_serving(root.path(), &socket, |_| {
        vec![Box::new(settings_sections::service::SettingsService::new())]
    });
    served(&mut session, &socket);
    // The workspace a generated SDK acts in: the first the person's account lists.
    let workspace = held_workspace(&mut session, &socket);
    let api = crate::common::Api::in_workspace(&socket, workspace);
    // What `tilo sdk gen` writes the sections' types for: their compiled surface.
    let kept = include_bytes!("../../../desktop/settings-sections/published.json");
    collab_types::surface::SurfaceFile::compiled(kept)
        .expect("the sections' compiled surface parses");
    let kept = std::str::from_utf8(kept).expect("the compiled surface is UTF-8");
    let surface =
        tilo_api::v1::surface_version(tilo_actions::remote::settings_resources::SURFACE_APP, kept);
    let appearance = tilo_app::framework::section_address("appearance", None);
    let power = tilo_app::framework::section_address("power", None);
    assert!(
        power.starts_with("devices/"),
        "power is a section of this device: {power}"
    );
    let set = |session: &mut tilo_session::Session,
               address: &str,
               setter: &str,
               value: serde_json::Value,
               surface: String| {
        api.send_typed(
            &format!("resources/{address}/actions/{setter}"),
            Some(json!({"value": value})),
            Some(surface),
        )
        .answer(session)
    };
    for address in [&appearance, &power] {
        found_at_the_api(&mut session, &api, address);
    }
    for (address, setter, value) in [
        (&appearance, "set-mode", json!("light")),
        (&power, "set-blank-minutes", json!(10)),
    ] {
        let answer = set(&mut session, address, setter, value, surface.clone());
        assert!(
            answer.is_ok(),
            "{address}#{setter} is set through its types: {answer:?}"
        );
    }
    for (address, field, wanted) in [
        (&appearance, "mode", json!("light")),
        (&power, "blank_minutes", json!(10)),
    ] {
        let read = crate::common::wait_for(&mut session, "the setting to change", |session| {
            session.settle_services();
            let read = session.read(address)?.get(field).cloned()?;
            (read == wanted).then_some(read)
        });
        assert_eq!(read, wanted, "{address}.{field} is what the API set");
    }

    let refused = set(&mut session, &appearance, "set-mode", json!(5), surface);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.ends_with("(invalid_arguments)")),
        "a number is no light-or-dark mode: {refused:?}"
    );
    let other = "so.tilo.settings.sections@0000000000000000".to_owned();
    let refused = set(&mut session, &power, "set-blank-minutes", json!(15), other);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.ends_with("(surface_changed)")),
        "types written for another surface of the sections are refused: {refused:?}"
    );
}

/// A program the person said yes to reaches what this device's settings keep, but its yes covers
/// the program, never each change it makes: one that weakens the device's lock (never locking it)
/// still raises the system's own question, named for the program. The person's no refuses the
/// call and changes nothing, and the program answering its own question is refused: only the
/// person answers it, where Tilo asks them.
#[test]
fn a_change_a_confirmed_program_makes_to_the_device_still_asks_the_person() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("api.sock");
    let mut session = session_serving(root.path(), &socket, |_| {
        vec![Box::new(settings_sections::service::SettingsService::new())]
    });
    served(&mut session, &socket);
    // The program acts in the workspace in front: the one whose questions the shell shows.
    held_workspace(&mut session, &socket);
    let api = crate::common::Api::open(&mut session, root.path());
    let power = tilo_app::framework::section_address("power", None);
    let before = crate::common::wait_for(&mut session, "the device's power section", |session| {
        session.settle_services();
        session.read(&power)?.get("lock_minutes").cloned()
    });
    assert_ne!(before, json!(0), "the device locks to begin with");
    found_at_the_api(&mut session, &api, &power);

    // Never locking weakens the lock: the call waits for the person, on the system's question.
    let never = api.send(
        &format!("resources/{power}/actions/set-lock-minutes"),
        Some(json!({"value": 0})),
    );
    crate::common::wait_for(&mut session, "the system's question", |session| {
        session.settle_services();
        session
            .step(std::time::Duration::from_millis(16))
            .expect("step");
        assert_eq!(
            session
                .read(&power)
                .and_then(|read| read.get("lock_minutes").cloned()),
            Some(before.clone()),
            "the lock changed before the person was asked"
        );
        // The call is taken and waits for the person: anything else before they are asked is
        // the call refused or carried out without them.
        if let Some(answered) = never.answered()
            && answered.as_ref().map_or(true, |answer| {
                answer.get("status") != Some(&json!("pending"))
            })
        {
            panic!("the call was answered before the person was asked: {answered:?}");
        }
        crate::common::on_screen_at(
            session,
            "Don't allow",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )
    });

    // The question is the program's, by what the kernel says it runs, and waits in the workspace.
    let program = std::env::current_exe()
        .expect("this test's program")
        .display()
        .to_string();
    let waiting = crate::common::wait_for(&mut session, "the call's asked record", |session| {
        let listed = api.listing(session, "invocations/workspace", None).ok()?;
        crate::common::Api::rows(&listed)
            .find(|row| {
                row["label"]
                    .as_str()
                    .is_some_and(|label| label.contains(&program))
            })
            .cloned()
    });
    // The record's address, as the listing names it: its name carries the program's path.
    let record = waiting["path"]
        .as_str()
        .expect("the record has its address");
    let record = tilo_api::v1::routing::encode_path(record);

    // The program answering its own question is refused, and nothing runs.
    let answered = api
        .send(
            &format!("resources/{record}/actions/resolve"),
            Some(json!({"resolution": "approve"})),
        )
        .answer(&mut session);
    assert!(
        answered.as_ref().is_err_and(|why| {
            why.contains(tilo_actions::remote::asked::CONFIRMED_BY_THE_SYSTEM)
        }),
        "a program never answers the person's question: {answered:?}"
    );
    session.settle_services();
    assert_eq!(
        session
            .read(&power)
            .and_then(|read| read.get("lock_minutes").cloned()),
        Some(before.clone()),
        "the program's own answer changed nothing"
    );

    // The person's no refuses the call, and the lock stands.
    crate::common::press(&mut session, "Don't allow");
    let refused = never.answer(&mut session);
    assert!(
        refused.as_ref().map_or(true, |answer| answer.get("status")
            != Some(&json!("completed"))),
        "the person's no refuses the program's call: {refused:?}"
    );
    session.settle_services();
    assert_eq!(
        session
            .read(&power)
            .and_then(|read| read.get("lock_minutes").cloned()),
        Some(before),
        "declined, the lock is as it was"
    );
}

/// A request at the socket lists the person's workspaces, and a program it sends to one of them
/// answers as the person: `describe(tasks)` reaches the workspace's tasks.
#[test]
fn a_program_of_this_login_answers_as_the_person_at_the_sessions_own_api() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api.sock");
    let mut session = session_with_api(root.path(), &api);
    served(&mut session, &api);

    // The list a program of the person's reads: workspace this device entered.
    let workspace = held_workspace(&mut session, &api);
    let answer = api_request(
        &api,
        "POST",
        &format!("/v1/workspaces/{workspace}/programs"),
        Some(&json!({"program": "1 + 1"})),
    )
    .expect("the program answered");
    let text = answer
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        text.trim() == "2" && answer.get("status") != Some(&json!("failed")),
        "a program answered as the person: {answer}"
    );
}

/// The socket's directory holds the socket and nothing else: the session's key never leaves the
/// process, so no key file is written beside it.
#[test]
fn the_socket_is_all_its_directory_holds() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api").join("api.sock");
    let mut session = session_with_api(root.path(), &api);
    served(&mut session, &api);

    let directory = api.parent().expect("the socket's directory");
    // The bind stages the socket in a dot-named directory beside it and moves it into place;
    // that one is gone once the socket answers.
    let mut held: Vec<String> = Vec::new();
    until(&mut session, "the socket's directory", |_| {
        held = std::fs::read_dir(directory)
            .expect("the socket's directory")
            .map(|entry| {
                entry
                    .expect("an entry of the socket's directory")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        !held.iter().any(|name| name.starts_with('.'))
    });
    held.sort();
    assert_eq!(
        held,
        vec![tilo_paths::login::API_SOCKET_FILE.to_owned()],
        "the socket's directory holds the socket and nothing else, never a key: {held:?}"
    );
}

/// A program of this login reaches the workspaces its person holds and no other: one sent to a
/// workspace this computer does not hold is refused, and nothing runs.
#[test]
fn a_program_is_refused_a_workspace_its_person_does_not_hold() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api.sock");
    let mut session = session_with_api(root.path(), &api);
    served(&mut session, &api);
    let held = held_workspace(&mut session, &api);

    let elsewhere = uuid::Uuid::new_v4().to_string();
    assert_ne!(held, elsewhere);
    let refused = api_request(
        &api,
        "POST",
        &format!("/v1/workspaces/{elsewhere}/programs"),
        Some(&json!({"program": "1 + 1"})),
    );
    // Refused by the API's answer, not by a connection that failed.
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| !why.contains("API socket")),
        "a workspace the person does not hold is refused: {refused:?}"
    );
}

/// Where [`sandboxed_program`] connects, when this test's program is started as it.
const SANDBOXED_AT: &str = "TILO_TEST_OWN_API_SANDBOXED_AT";

/// A program of this login run confined, in a user namespace of its own as a sandbox runs it: it
/// reaches the API socket's path, since it is the same account, and asks it what a program of
/// the person's asks first. It says what came back. Started by
/// [`a_sandboxed_program_of_this_login_is_refused_at_the_api_socket`]; run on its own it asks
/// nothing.
#[test]
fn sandboxed_program() {
    let Some(socket) = std::env::var_os(SANDBOXED_AT) else {
        return;
    };
    let socket = std::path::PathBuf::from(socket);
    if let Err(error) = std::os::unix::net::UnixStream::connect(&socket) {
        println!("unreachable {error}");
        return;
    }
    match api_account(&socket, "account/workspaces", "list", &json!({})) {
        Ok(answer) => println!("answered {answer}"),
        Err(why) => println!("refused {why}"),
    }
}

/// A program of this login that a sandbox confined is the same account, and reaches the socket,
/// but is no program of the person's own: the session takes its connection and hands it to
/// nobody, so it is answered nothing, while the person's own program beside it is answered.
#[test]
fn a_sandboxed_program_of_this_login_is_refused_at_the_api_socket() {
    use std::os::unix::process::CommandExt as _;
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api.sock");
    let mut session = session_with_api(root.path(), &api);
    served(&mut session, &api);
    // The person's own program is answered.
    held_workspace(&mut session, &api);

    let mut command = std::process::Command::new(std::env::current_exe().expect("this test"));
    command
        .args([
            "--exact",
            "own_api::sandboxed_program",
            "--nocapture",
            "--test-threads",
            "1",
            "-q",
        ])
        .env(SANDBOXED_AT, &api);
    // SAFETY: between fork and exec the child calls only `unshare`, which is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            if libc::unshare(libc::CLONE_NEWUSER) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let ran = command
        .output_locked()
        .expect("a program in a user namespace of its own starts");
    let said = String::from_utf8_lossy(&ran.stdout);
    let said = said
        .lines()
        .find(|line| {
            ["refused ", "answered ", "unreachable "]
                .iter()
                .any(|outcome| line.starts_with(outcome))
        })
        .unwrap_or_default();
    assert!(
        said.starts_with("refused "),
        "a sandboxed program reaches the socket and is answered nothing: {said:?}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
}

/// Where [`api_program`] connects, when a copy of this test's program is started as it.
const PROGRAM_AT: &str = "TILO_TEST_OWN_API_PROGRAM_AT";

/// A program of this login that asks the API socket what a program of the person's asks first,
/// and says what came back. Started from a copy of this test's program by
/// [`a_program_acts_as_the_person_only_once_they_say_yes_to_it`]; run on its own it asks nothing.
#[test]
fn api_program() {
    let Some(socket) = std::env::var_os(PROGRAM_AT) else {
        return;
    };
    let socket = std::path::PathBuf::from(socket);
    say_what_the_api_answered(&socket);
    // Asked to go on, it connects again as the same run, or runs another program in its place.
    let Some(then) = std::env::var_os(THEN) else {
        return;
    };
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .expect("the test says when to go on");
    if then == AGAIN {
        say_what_the_api_answered(&socket);
    } else {
        use std::os::unix::process::CommandExt as _;
        let failed = program_command(std::path::Path::new(&then), &socket)
            .env_remove(THEN)
            .exec();
        panic!("the program runs another in its place: {failed}");
    }
}

/// What a program says after it went on ([`THEN`]): connect again ([`AGAIN`]), or run the program
/// at this path in its own place, in the same process.
const THEN: &str = "TILO_TEST_OWN_API_THEN";
const AGAIN: &str = "again";

/// Ask the API socket at `socket` what a program asks first, and say what came back.
fn say_what_the_api_answered(socket: &std::path::Path) {
    match api_account(socket, "account/workspaces", "list", &json!({})) {
        Ok(answer) => println!("answered {answer}"),
        Err(why) => println!("refused {why}"),
    }
}

/// Where [`launcher_program`] starts the programs it is asked to, when a copy of this test's
/// program is started as it.
const LAUNCHER_AT: &str = "TILO_TEST_OWN_API_LAUNCHER_AT";

/// A program that starts commands for others, as a launcher of the login does (`systemd-run
/// --user`, a terminal started on request): each line it reads names a program, which it starts
/// asking the API socket, saying what that program says.
#[test]
fn launcher_program() {
    let Some(socket) = std::env::var_os(LAUNCHER_AT) else {
        return;
    };
    for line in std::io::stdin().lines().map_while(Result::ok) {
        let started = program_command(std::path::Path::new(&line), std::path::Path::new(&socket))
            .stdout(std::process::Stdio::inherit())
            .status_locked();
        assert!(started.is_ok_and(|ran| ran.success()), "{line} ran");
    }
}

/// `program`, a copy of this test's program, run as [`api_program`] asking the API socket at
/// `api`.
fn program_command(program: &std::path::Path, api: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .args([
            "--exact",
            "own_api::api_program",
            "--nocapture",
            "--test-threads",
            "1",
            "-q",
        ])
        .env(PROGRAM_AT, api);
    command
}

/// This test's program copied to `name` under `root`: another program, by the executable the
/// kernel names for it, which nobody has said anything about.
fn program_at(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let program = root.join(name);
    std::fs::copy(std::env::current_exe().expect("this test"), &program)
        .expect("a copy of this test's program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("the copy runs");
    std::fs::canonicalize(&program).expect("the copy's path")
}

/// `program` asking the API socket at `api`, running.
fn asking(program: &std::path::Path, api: &std::path::Path) -> std::process::Child {
    program_command(program, api)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("the program starts")
}

/// What `program` was answered, once it ends: `answered …` or `refused …`.
fn told(session: &mut tilo_session::Session, mut program: std::process::Child) -> String {
    until(session, "the program to be answered", |_| {
        program.try_wait().expect("the program runs").is_some()
    });
    let ran = program.wait_with_output().expect("the program's output");
    String::from_utf8_lossy(&ran.stdout)
        .lines()
        .find(|line| line.starts_with("answered ") || line.starts_with("refused "))
        .unwrap_or_else(|| {
            panic!(
                "the program says what it was answered: {}",
                String::from_utf8_lossy(&ran.stderr)
            )
        })
        .to_owned()
}

/// A program of the person's login that nobody confirmed acts as no one: being local grants
/// nothing. It reaches the API socket, is put to the person by the program it is, and when no yes
/// comes it is refused in words, before any of its calls reaches the daemon as anyone.
#[test]
fn a_program_nobody_confirmed_is_refused_and_acts_as_no_one() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api.sock");
    let program = program_at(root.path(), "unconfirmed-program");
    let mut session = session_with_api(root.path(), &api);
    served(&mut session, &api);

    let mut connecting = asking(&program, &api);
    // The person is asked about the program that connected, at the path it runs from, and does
    // not say yes. Questions about other programs (this test's own) are not this one.
    let asked =
        crate::common::wait_for(&mut session, "the program's question", |_| {
            // A program that ended was answered without its question, or never asked: what it said
            // is why.
            if connecting.try_wait().expect("the program runs").is_some() {
                let ran = connecting.stdout.take().zip(connecting.stderr.take()).map(
                    |(mut out, mut err)| {
                        use std::io::Read as _;
                        let mut said = String::new();
                        let _ = out.read_to_string(&mut said);
                        let _ = err.read_to_string(&mut said);
                        said
                    },
                );
                panic!(
                    "the program ended before the person was asked about it: {}; asked: {:?}",
                    ran.unwrap_or_default(),
                    crate::common::person_asked_now()
                        .iter()
                        .map(|question| (&question.kind, &question.details))
                        .collect::<Vec<_>>()
                );
            }
            crate::common::person_asked_now()
                .into_iter()
                .find(|question| {
                    question.kind == "session.api-program"
                        && question.details["program"] == json!(program.display().to_string())
                })
        });
    assert!(
        crate::common::person_declines(&asked.handle),
        "the question waits for its answer"
    );
    let said = told(&mut session, connecting);
    assert_eq!(
        said, "refused You didn’t let this program act as you. (not_allowed)",
        "the program is refused, in words, and answered nothing as the person"
    );
}

/// [`session_with_api`] as a person's login runs it: with its desktop services on the session bus
/// `bus`, whose answers about this device Settings lists.
fn login_with_api(
    root: &std::path::Path,
    api: &std::path::Path,
    bus: &tilo_test_env::PrivateBus,
) -> tilo_session::Session {
    crate::common::on_its_daemon_with(
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        |device| {
            let wake = tilo_service::LoopWake::default();
            let (side, link) = tilo_session::portal_link::channel(wake.handle());
            let desktop = std::rc::Rc::new(
                tilo_session::desktop_services::DesktopServices::start(
                    &tilo_session::desktop_services::Buses {
                        session: tilo_session::desktop_services::Bus::Address(
                            bus.address().to_owned(),
                        ),
                        system: tilo_session::desktop_services::Bus::None,
                    },
                    &wake.handle(),
                    std::sync::Arc::new(tilo_portal::NoCapture),
                    &side,
                    None,
                )
                .expect("the login's desktop services start"),
            );
            let mut services = crate::common::seeded_apps_of(device);
            services.extend(desktop.services());
            let portal = desktop
                .portal()
                .map(|serving| link.with_captures(serving.input_captures().clone()));
            Options {
                collab: false,
                state: Some(root.join("state")),
                services,
                wake,
                desktop: Some(desktop),
                portal,
                asks: Some(crate::common::person_asked()),
                app_socket: tilo_session::AppSocket::At(root.join("app_wire.sock")),
                api_socket: Some(api.to_owned()),
                ..Options::default()
            }
        },
    )
}

/// A program running, and the lines it says as it says them.
struct Running {
    program: std::process::Child,
    said: std::sync::mpsc::Receiver<String>,
}

impl Running {
    /// `command` started, told when to go on through its standard input.
    fn start(mut command: std::process::Command) -> Self {
        use std::io::BufRead as _;
        let mut program = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn_locked()
            .expect("the program starts");
        let out = program.stdout.take().expect("its output");
        let (says, said) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if says.send(line).is_err() {
                    break;
                }
            }
        });
        Self { program, said }
    }

    /// What the program was answered next: `answered …` or `refused …`.
    fn answered(&self, session: &mut tilo_session::Session) -> String {
        crate::common::wait_for(session, "the program to say what it was answered", |_| {
            self.said
                .try_iter()
                .find(|line| line.starts_with("answered ") || line.starts_with("refused "))
        })
    }

    /// Tell the program to go on, with `line`.
    fn go_on(&mut self, line: &str) {
        use std::io::Write as _;
        let input = self.program.stdin.as_mut().expect("its input");
        writeln!(input, "{line}").expect("the program reads");
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.program.kill();
        let _ = self.program.wait();
    }
}

/// `program` asking the API socket at `api`, then going on as `then` says ([`THEN`]).
fn running(program: &std::path::Path, api: &std::path::Path, then: &std::path::Path) -> Running {
    let mut command = program_command(program, api);
    command.env(THEN, then);
    Running::start(command)
}

/// The question the person is asked about the program at `path`, once it is asked.
fn asked_about(
    session: &mut tilo_session::Session,
    path: &std::path::Path,
) -> tilo_service::Question {
    crate::common::wait_for(session, "the program's question", |_| {
        crate::common::person_asked_now()
            .into_iter()
            .find(|question| {
                question.kind == "session.api-program"
                    && question.details["program"] == json!(path.display().to_string())
            })
    })
}

/// The person's answer about a run of the program at `program`, as Settings lists it
/// (`session:privacy`), once one stands: the row "Take back" is pressed on.
fn answer_for(session: &mut tilo_session::Session, program: &std::path::Path) -> serde_json::Value {
    let run = format!("binary:{}@", program.display());
    crate::common::wait_for(session, "the program's answer in Settings", |session| {
        session.settle_services();
        session
            .read("session:privacy")?
            .get("answers")?
            .as_array()?
            .iter()
            .find(|answer| {
                answer["app"]
                    .as_str()
                    .is_some_and(|app| app.starts_with(&run))
            })
            .cloned()
    })
}

/// What a program the person said no to is told.
const DECLINED: &str = "refused You didn’t let this program act as you. (not_allowed)";

/// The person's yes to a program stands for that run of it, for this session, and Settings lists it
/// beside their other answers about this device. Taken back there, the same run is asked about
/// again.
#[test]
fn a_programs_yes_stands_for_its_run_and_settings_takes_it_back() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api.sock");
    let program = program_at(root.path(), "listed-program");
    let bus = tilo_test_env::PrivateBus::start();
    let mut session = login_with_api(root.path(), &api, &bus);
    served(&mut session, &api);

    let mut run = running(&program, &api, std::path::Path::new(AGAIN));
    let asked = asked_about(&mut session, &program);
    assert!(crate::common::person_answers_to(
        &asked.handle,
        crate::common::allowed_for(1)
    ));
    let said = run.answered(&mut session);
    assert!(said.starts_with("answered "), "{said}");

    let answer = answer_for(&mut session, &program);
    assert_eq!(answer["capability"], json!("api"), "{answer}");
    assert_eq!(answer["allowed"], json!(true), "{answer}");
    assert_eq!(answer["standing"], json!("session"), "{answer}");
    assert_eq!(answer["name"], json!("listed-program"), "{answer}");
    assert_eq!(answer["this_device"], json!(true), "{answer}");

    // Taken back, as Settings' "Take back" takes it: the same run is asked about again.
    let consent = answer["$address"]
        .as_str()
        .expect("the answer names the consent it is")
        .to_owned();
    session
        .invoke(&consent, "forget", &serde_json::Map::new())
        .expect("the answer is taken back");
    run.go_on("");
    let asked = asked_about(&mut session, &program);
    assert!(crate::common::person_declines(&asked.handle));
    assert_eq!(run.answered(&mut session), DECLINED);
}

/// A yes is the person's for the program the process ran when they gave it: the same process, once
/// it runs another executable in its place (`tilo`, here), is asked about again, and refused when
/// they say no.
#[test]
fn a_run_said_yes_to_that_execs_another_program_is_asked_again() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api.sock");
    let first = program_at(root.path(), "first-program");
    let tilo = program_at(root.path(), "tilo");
    let mut session = session_with_api(root.path(), &api);
    served(&mut session, &api);

    let mut run = running(&first, &api, &tilo);
    let asked = asked_about(&mut session, &first);
    assert!(crate::common::person_answers_to(
        &asked.handle,
        crate::common::allowed_for(1)
    ));
    let said = run.answered(&mut session);
    assert!(said.starts_with("answered "), "{said}");

    run.go_on("");
    let asked = asked_about(&mut session, &tilo);
    assert!(crate::common::person_declines(&asked.handle));
    assert_eq!(run.answered(&mut session), DECLINED);
}

/// A program a launcher starts for another, in the same login, is a run of its own: the person's
/// yes to one run of a program lets no other run of it in, however it was started. The run they
/// said yes to is still let in.
#[test]
fn a_launchers_run_of_a_program_gets_nothing_from_another_runs_yes() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api.sock");
    let tilo = program_at(root.path(), "tilo");
    let launcher = program_at(root.path(), "launcher");
    let mut session = session_with_api(root.path(), &api);
    served(&mut session, &api);

    let mut run = running(&tilo, &api, std::path::Path::new(AGAIN));
    let asked = asked_about(&mut session, &tilo);
    assert!(crate::common::person_answers_to(
        &asked.handle,
        crate::common::allowed_for(1)
    ));
    let said = run.answered(&mut session);
    assert!(said.starts_with("answered "), "{said}");

    let mut launching = std::process::Command::new(&launcher);
    launching
        .args([
            "--exact",
            "own_api::launcher_program",
            "--nocapture",
            "--test-threads",
            "1",
            "-q",
        ])
        .env(LAUNCHER_AT, &api);
    let mut by_the_launcher = Running::start(launching);
    by_the_launcher.go_on(&tilo.display().to_string());
    let asked = asked_about(&mut session, &tilo);
    assert!(crate::common::person_declines(&asked.handle));
    assert_eq!(by_the_launcher.answered(&mut session), DECLINED);

    run.go_on("");
    let said = run.answered(&mut session);
    assert!(
        said.starts_with("answered "),
        "the run the person said yes to is still let in: {said}"
    );
}
