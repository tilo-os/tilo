#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "isolated app validation"
)]
use automerge::{ROOT, ReadDoc};
use ui::headless::Driver;

#[test]
fn writing_surface_captures_edit_save_reopen_and_overflow() {
    let out = std::path::PathBuf::from(std::env::var("QOS_TASK3_CAPTURE_DIR").unwrap());
    std::fs::create_dir_all(&out).unwrap();
    for (name, viewport, scale) in [
        ("wide", (1000, 700), 1.0),
        ("narrow", (480, 600), 1.0),
        ("double", (640, 480), 2.0),
        ("narrow-double", (480, 600), 2.0),
    ] {
        let profile = out.join(format!("profile-{name}"));
        let (state, root) = qos_scratchpad::open(&profile).unwrap();
        let theme = ui::Theme::dark().with_text_scale(scale);
        let mut driver = Driver::new_with_theme(state, root, viewport, theme.clone());
        qos_scratchpad::register(driver.action_registry_mut());
        let editor = *driver.targets().get("note-editor").unwrap();
        let save = *driver.targets().get("save-note").unwrap();
        assert!(editor.height() > 120.0, "{name}: {editor:?}");
        assert!(editor.y1 <= save.y0 && save.y1 < f64::from(viewport.1));
        driver.render_window_png(&out.join(format!("{name}-empty.png")));
        std::fs::write(
            out.join(format!("{name}-empty.json")),
            driver.inspection_json().unwrap(),
        )
        .unwrap();
        driver.click_at(editor.x0 + 30.0, editor.y1 - 25.0);
        let body = format!("Lower writing area works — {name} café 日本語");
        driver.type_text(&body);
        driver.click_named("save-note").unwrap();
        assert_eq!(
            driver
                .state_mut()
                .doc
                .get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .to_str(),
            Some(body.as_str())
        );
        driver.render_window_png(&out.join(format!("{name}-saved.png")));
        drop(driver);
        let (state, root) = qos_scratchpad::open(&profile).unwrap();
        assert_eq!(
            state.doc.get(ROOT, "body").unwrap().unwrap().0.to_str(),
            Some(body.as_str())
        );
        let mut driver = Driver::new_with_theme(state, root, viewport, theme);
        qos_scratchpad::register(driver.action_registry_mut());
        driver.render_window_png(&out.join(format!("{name}-reopened.png")));
        let editor = *driver.targets().get("note-editor").unwrap();
        driver.click_at(editor.x0 + 30.0, editor.y1 - 25.0);
        driver.set_modifiers(&["ctrl".into()]);
        driver.key("End");
        driver.set_modifiers(&[]);
        let more = "\nA longer thought with enough detail to keep writing. ".repeat(45)
            + "\nFinal line — the end is reachable.";
        driver.send(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            more.clone(),
        )));
        let long_editor = *driver.targets().get("note-editor").unwrap();
        assert!(
            long_editor.height() > f64::from(viewport.1),
            "{name}: {long_editor:?}"
        );
        driver.click_at(editor.x0 + 30.0, editor.y0 + 30.0);
        driver.scroll(0.0, 10000.0);
        let top = *driver.targets().get("note-editor").unwrap();
        driver.render_window_png(&out.join(format!("{name}-long-top.png")));
        driver.scroll(0.0, -10000.0);
        let bottom = *driver.targets().get("note-editor").unwrap();
        assert!(
            bottom.y0 < top.y0 - 200.0,
            "{name}: top {top:?}, bottom {bottom:?}"
        );
        assert!(
            (bottom.y1 - editor.y1).abs() < 1.0,
            "bottom must reach viewport: {bottom:?}, {editor:?}"
        );
        assert_eq!(*driver.targets().get("save-note").unwrap(), save);
        driver.render_window_png(&out.join(format!("{name}-long-bottom.png")));
        driver.click_named("save-note").unwrap();
        let saved = driver
            .state_mut()
            .doc
            .get(ROOT, "body")
            .unwrap()
            .unwrap()
            .0
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(saved, body + &more);
        drop(driver);
        let (state, _) = qos_scratchpad::open(&profile).unwrap();
        assert_eq!(
            state.doc.get(ROOT, "body").unwrap().unwrap().0.to_str(),
            Some(saved.as_str())
        );
        println!(
            "{name}: editor {editor:?}, save {save:?}, long {long_editor:?}, bottom {bottom:?}; persisted {} bytes",
            saved.len()
        );
    }
}
