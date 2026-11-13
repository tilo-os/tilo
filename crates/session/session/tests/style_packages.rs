//! A person's desk style as their own package in their workspace (`docs/designs/app-language/
//! shell.md` §6, "Make it mine" and "Agents make styles with the same commands a person types").

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use std::time::Duration;

use tilo_compositor::{KeyRepeat, OutputConfig};
use tilo_session::{Options, Session};
use tilo_surface_host::DeskReading;
use tilo_surface_host::shell_part::DESK_RESOURCE;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PERSON: &str = "Pat";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        shell: true,
        ..Options::default()
    }
}

fn session() -> Session {
    let mut session =
        Session::headless(options(), &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)])
            .expect("session");
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    session
}

fn desk(session: &Session) -> DeskReading {
    serde_json::from_value(session.read(DESK_RESOURCE).expect("the desk")).expect("the desk reads")
}

fn style_state(session: &Session, style: &str) -> Option<(bool, String)> {
    desk(session)
        .styles
        .into_iter()
        .find(|found| found.style == style)
        .map(|found| (found.loads, found.reason))
}

/// The workspace a test's "make it mine" and installs write into, and the app store they install
/// into: pinned for the whole process, as the store already is.
fn workspace() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("tilo-style-workspace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a workspace for the person's packages");
        tilo_test_env::pin("TILO_PACKAGES_DIR", &dir);
        dir
    })
}

/// Run the session action `action` with `arguments` as the person, and answer what it completed
/// with (`None` for a step the session accepted and answers later), or the refusal it answered
/// with its reason.
fn style_action(
    session: &mut Session,
    action: &str,
    arguments: serde_json::Value,
) -> Result<Option<serde_json::Value>, String> {
    let resource = tilo_session::style_packages::RESOURCE;
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    let started = session
        .invoke(resource, action, &arguments)
        .map_err(|error| error.to_string())?;
    Ok(started.completed().and_then(|output| output.value))
}

/// Step 1 of "Make it mine": the shipped Strips style becomes the person's own package, with a
/// view of its own the switch offers and the desk draws.
///
/// This fails without the change: there was no command that copied a shipped style, and the
/// person had only the four the shell ships.
#[test]
fn make_it_mine_copies_strips_into_a_selectable_style() {
    let _settled = ui::motion::testing::settled();
    common::app_store();
    let _ = workspace();
    // A session whose agent door a REPL reaches: the door is how the switch runs its own action
    // (`shell.show_style`), as a press in the bar's menu does.
    let root = common::private_home();
    let program = std::env::current_exe().expect("this test's program");
    let questions = tilo_service::Questions::new();
    let mut session = common::headless(Options {
        collab: false,
        action_plane: tilo_session::ActionPlane::At(root.path().join("plane.sock")),
        agent_door: Some(root.path().join("agent.sock")),
        admit_repl: vec![program],
        services: vec![Box::new(common::seeded_apps())],
        questions: Some(questions),
        ..options()
    });
    common::until(&mut session, "the shell draws its bar", |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label == "Showing Strips")
    });
    // The door, connected before the build runs, so its state is up: the copy is switched to by
    // the shell's own action, as a press in the bar's menu runs it.
    let mut repl = common::Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
    // The copy is made and installed, answering the package it went in as.
    let answer = style_action(
        &mut session,
        "make-it-mine",
        serde_json::json!({"style": "so.tilo.shell:strips"}),
    )
    .expect("make it mine ran")
    .expect("completed");
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["package"], tilo_session::style_packages::COPY_ID);
    // The copy is the person's own short style package: one .tl file, its own `app` header, the
    // shell it uses, and one view of `shell.Arrangement` whose body draws the desk through the
    // shell's own template.
    let source = workspace().join(tilo_session::style_packages::COPY_ID);
    let text = std::fs::read_to_string(source.join("app.tl")).expect("the copy's app.tl");
    assert!(
        text.contains(&format!("app {}", tilo_session::style_packages::COPY_ID)),
        "the copy is the person's own package: {text}"
    );
    assert!(
        text.contains("shell.desk_style(a, false)") && text.contains("shell.strips_view"),
        "the copy draws the desk through the shell's own chrome and its places: {text}"
    );
    // The store moved, so the session reads it again: the copy's style is offered and loads.
    let copy = format!("{}:strips", tilo_session::style_packages::COPY_ID);
    common::until(&mut session, "the copy is offered", |session| {
        style_state(session, &copy).is_some()
    });
    assert_eq!(
        style_state(&session, &copy),
        Some((true, String::new())),
        "the copy loads"
    );
    assert!(
        style_state(&session, "so.tilo.shell:strips").is_some(),
        "the shipped style stands beside the copy"
    );
    // Choosing it draws the desk: the copy draws the shell's own chrome and its places, so the
    // screen shows what the shipped Strips shows, the place's own empty state.
    let (answer, failed) = repl.run(
        &mut session,
        &format!("shell.show_style(shell.viewing, shown: \"{copy}\")"),
    );
    assert!(!failed, "the desk shows the copy: {answer}");
    common::until(&mut session, "the copy draws the desk", |session| {
        common::on_screen(session, "No window here yet").is_some()
    });
    assert!(
        style_state(&session, "so.tilo.shell:strips").is_some(),
        "the shipped style is untouched"
    );
    // Editing the copy and installing it again is the loop an agent runs: `check` reads it,
    // `install` puts the edited code in the store under the same package id, and the store
    // moving makes the session read it again — the change reaches the desk with no restart.
    // The edit stops drawing through the shell's chrome: the copy shows its places flat, so the
    // bar's own style switch is gone from the screen while the copy is chosen.
    let edited = text
        .replacen("/// Strips\n", "/// My strips\n", 1)
        .replace("shell.desk_style(a, false) {", "stack(fill) {");
    assert!(
        edited.contains("stack(fill) {"),
        "the edit reaches the body"
    );
    std::fs::write(source.join("app.tl"), &edited).expect("the copy is edited");
    let checked = style_action(
        &mut session,
        "check",
        serde_json::json!({"package": tilo_session::style_packages::COPY_ID}),
    )
    .expect("check ran")
    .expect("completed");
    assert_eq!(checked["ok"], true, "{checked}");
    let installed = style_action(
        &mut session,
        "install",
        serde_json::json!({"package": tilo_session::style_packages::COPY_ID}),
    )
    .expect("install ran")
    .expect("completed");
    assert_eq!(installed["ok"], true, "{installed}");
    // The copy the person chose is now the edited one, drawn without the shell's chrome: the
    // bar's style switch, which only `shell.desk_style` draws, is gone from the screen, and the
    // copy's places still stand. The reinstall redraws it where it stands: no switch needed.
    common::until(&mut session, "the edited copy draws", |session| {
        common::on_screen(session, "No window here yet").is_some()
            && common::preset_menu(session).is_none()
    });
    // The switch lists the edited copy by its new name, and it loads: a reinstall clears a
    // failure, as `desk_styles::a_fixed_style_package_brings_the_style_back_live` holds.
    common::until(
        &mut session,
        "the switch names the edited copy",
        |session| {
            desk(session)
                .styles
                .iter()
                .any(|style| style.style == copy && style.line == "My strips" && style.loads)
        },
    );
}

/// A copy whose source does not compile is `check`'s diagnostics and cannot be installed: the
/// person's own mistake is named, and the store still holds the copy that last loaded.
#[test]
fn a_broken_copy_is_a_diagnostic_and_never_installs() {
    let _settled = ui::motion::testing::settled();
    common::app_store();
    let dir = workspace().join("mine.broken");
    std::fs::create_dir_all(&dir).expect("a directory for the broken copy");
    std::fs::write(dir.join("app.ids"), "").expect("an ids lock");
    std::fs::write(
        dir.join("app.tl"),
        "app mine.broken \"Broken\"\n\nuse so.tilo.shell as shell\n\n\
         /// Broken style\nview broken(a: shell.Arrangement) {\n    ui.Text(oops\n}\n",
    )
    .expect("the broken source");
    let mut session = session();
    common::until(&mut session, "the shell draws", |session| {
        !session.shell_parts().is_empty()
    });
    let checked = style_action(
        &mut session,
        "check",
        serde_json::json!({"package": "mine.broken"}),
    )
    .expect_err("a check that fails answers no value");
    assert!(
        checked.contains("E003"),
        "the diagnostic names the parse error: {checked}"
    );
    let installed = style_action(
        &mut session,
        "install",
        serde_json::json!({"package": "mine.broken"}),
    )
    .expect_err("a broken copy never installs");
    assert!(!installed.is_empty(), "the refusal says why: {installed}");
    assert!(
        style_state(&session, "mine.broken:broken").is_none(),
        "nothing broken reached the store"
    );
}

/// The same commands through the REPL's door: a person (or their agent) typing at the
/// session's agent door runs "make it mine" and gets a style the switch offers, the very call
/// the canvas chat's agent makes (`shell.md` §6, "Every step is an action in the broker's
/// catalogue, so the REPL does the same").
#[test]
fn the_repl_makes_a_style_with_the_same_call() {
    let _settled = ui::motion::testing::settled();
    common::app_store();
    let _ = workspace();
    // The session whose door this REPL reaches, admitting it to act as the person.
    let root = common::private_home();
    let questions = tilo_service::Questions::new();
    let apps = common::seeded_apps();
    let program = std::env::current_exe().expect("this test's program");
    let mut session = tilo_session::Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(apps)],
            questions: Some(questions),
            action_plane: tilo_session::ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(root.path().join("agent.sock")),
            admit_repl: vec![program],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    common::until(&mut session, "the shell draws", |session| {
        !session.shell_parts().is_empty()
    });
    let mut repl = common::Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
    // The command as a person types it: one call on the session's own resource.
    let (answer, failed) = repl.call(
        &mut session,
        "tilo",
        &serde_json::json!({
            "program": "session.make_it_mine(style: \"so.tilo.shell:strips\")"
        }),
    );
    assert!(!failed, "the REPL made the style: {answer}");
    let copy = format!("{}:strips", tilo_session::style_packages::COPY_ID);
    // The copy is offered by the switch, as it is from the person's own press.
    common::until(&mut session, "the copy is offered", |session| {
        desk(session).styles.iter().any(|style| style.style == copy)
    });
    // The REPL switches to it by the same door, and the desk draws the copy.
    let (answer, failed) = repl.run(
        &mut session,
        &format!("shell.show_style(shell.viewing, shown: \"{copy}\")"),
    );
    assert!(!failed, "the REPL switches to the copy: {answer}");
    common::until(&mut session, "the copy draws the desk", |session| {
        common::on_screen(session, "No window here yet").is_some()
    });
}
