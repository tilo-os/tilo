//! Scratch probe: does the desktop switch with windows placed? Deleted before landing.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "probe"
)]

mod common;

use common::on_screen;
use serde_json::{Map, Value, json};

#[test]
fn probe() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = common::seeded_session(root.path());
    let mut repl = common::Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
    common::until(&mut session, "the bar", |s| {
        on_screen(s, "Notifications").is_some()
    });

    println!("BEFORE: {:?}", common::preset_menu(&session));

    // Place a note exactly as my test does.
    let notes: Vec<String> = {
        fn find(res: &[tilo_actions::ResourceSpec], out: &mut Vec<String>) {
            for r in res {
                if r.kind.ends_with(".Note") {
                    out.push(r.id.as_str().to_owned());
                }
                find(&r.children, out);
            }
        }
        let mut out = Vec::new();
        find(&session.resources(), &mut out);
        out
    };
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let w = tree.active_workspace().expect("workspace");
        (
            w.slot.0.to_string(),
            w.strips.first().expect("row").id.0,
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace, "row": row, "strip": strip, "place": place,
        "resource": notes[0], "x": 100, "y": 120, "width": 480, "height": 360,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .expect("place");
    for _ in 0..30 {
        session
            .step(std::time::Duration::from_millis(16))
            .expect("step");
    }

    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_preset(shell.viewing, shown: shell.Preset.desktop)",
    );
    println!("SHOW: failed={failed} {answer}");
    for _ in 0..60 {
        session
            .step(std::time::Duration::from_millis(16))
            .expect("step");
    }
    println!(
        "AFTER REPL: menu={:?} dock={:?}",
        common::preset_menu(&session),
        on_screen(&session, "Dock")
    );
    session
        .run_shell_setting("apply", vec![Value::from("windows_desktop")])
        .expect("apply");
    for _ in 0..60 {
        session
            .step(std::time::Duration::from_millis(16))
            .expect("step");
    }
    println!(
        "AFTER APPLY: menu={:?} dock={:?}",
        common::preset_menu(&session),
        on_screen(&session, "Dock")
    );
    println!("AFTER APPLY labels={:?}", session.accessible_labels());
    println!("labels={:?}", session.accessible_labels());
    println!("parts={:?}", session.shell_parts());
    println!("refusals={:?}", session.shell_refusals());
    let shot = common::shot(&mut session, "probe-switch");
    println!("shot {}x{}", shot.width, shot.height);
}
