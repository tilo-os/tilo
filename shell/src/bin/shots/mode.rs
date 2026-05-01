//! Three execution modes for the `shots` binary:
//!   - `offscreen` — wgpu adapter, no window; captures PNGs.
//!   - `headless`  — no GPU; dumps state JSON.
//!   - `window`    — real winit window; pre-rolls a scenario then
//!                   yields control to the live event loop.

use std::path::Path;

use vello::peniko::Color;

use quick_ui::offscreen::OffscreenRenderer;

use super::driver::ShellDriver;
use super::scenario::{self, JsonDirSink, NullSink, Scenario, SnapshotSink};

// =====================================================================
// Offscreen mode
// =====================================================================

pub mod offscreen {
    use super::*;

    /// `SnapshotSink` that renders into a PNG file per snapshot under
    /// `dir`, using the supplied `OffscreenRenderer`.
    pub struct PngDirSink<'a> {
        pub dir: &'a Path,
        pub renderer: &'a mut OffscreenRenderer,
    }

    impl<'a> SnapshotSink for PngDirSink<'a> {
        fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
            let scene = driver.paint();
            let path = self.dir.join(format!("{name}.png"));
            self.renderer.render_png(&scene, Color::TRANSPARENT, &path);
            println!("  wrote {}", path.display());
        }
    }

    /// Run a scenario through the offscreen renderer.
    pub fn run_scenario(driver: &mut ShellDriver, scenario: &Scenario, dir: &Path) {
        std::fs::create_dir_all(dir).expect("create offscreen output dir");
        let (w, h) = driver.viewport();
        let mut renderer = OffscreenRenderer::new(w as u32, h as u32);
        println!(
            "shots: offscreen scenario '{}' at {}x{} → {}",
            scenario.name,
            w as u32,
            h as u32,
            dir.display()
        );
        let mut sink = PngDirSink {
            dir,
            renderer: &mut renderer,
        };
        scenario::run(driver, scenario, &mut sink);
    }

    /// Backwards-compat scene-set path: render each named scene with a
    /// fresh driver (so flag mutations don't bleed between scenes).
    pub fn run_scenes(scenes: &[String], dir: &Path, viewport: (u32, u32)) {
        std::fs::create_dir_all(dir).expect("create offscreen output dir");
        let mut renderer = OffscreenRenderer::new(viewport.0, viewport.1);
        println!(
            "shots: offscreen scene-set ({} scenes) at {}x{} → {}",
            scenes.len(),
            viewport.0,
            viewport.1,
            dir.display()
        );
        for name in scenes {
            let mut driver = ShellDriver::new(viewport);
            driver
                .scene_setup(name)
                .unwrap_or_else(|e| panic!("scene_setup({name}) failed: {e}"));
            let scene = driver.paint_settled();
            let path = dir.join(format!("{name}.png"));
            renderer.render_png(&scene, Color::TRANSPARENT, &path);
            println!("  wrote {}", path.display());
        }
    }
}

// =====================================================================
// Headless mode
// =====================================================================

pub mod headless {
    use super::*;

    pub fn run_scenario(driver: &mut ShellDriver, scenario: &Scenario, dir: &Path) {
        std::fs::create_dir_all(dir).expect("create headless output dir");
        let (w, h) = driver.viewport();
        println!(
            "shots: headless scenario '{}' at {}x{} → {}",
            scenario.name,
            w as u32,
            h as u32,
            dir.display()
        );
        let mut sink = JsonDirSink::new(dir);
        scenario::run(driver, scenario, &mut sink);
    }
}

// =====================================================================
// Window mode
// =====================================================================

pub mod window {
    use super::*;

    use quick_desktop::shell::{
        build_nirios_root, persist, seed_demo_editor_presence, sync_tab_presence_from_tree,
    };
    use quick_ui::presence::PresenceKey;
    use quick_ui::{App, State};

    /// Run the live shell in a real winit window. If `pre_roll` is
    /// `Some(_)`, the scenario runs against a throwaway driver first
    /// (so structural mutations to the workspace tree land on disk)
    /// before control hands off to `App::run`.
    ///
    /// Window mode treats `Snapshot` actions as no-ops because the user
    /// is watching the screen — the goal of window-mode-with-scenario
    /// is "drive the shell into a state, then leave it open".
    ///
    /// Caveat: chrome state (overlay flags, palette query) is **not**
    /// persisted — only the WorkspaceTree itself is. So a pre-rolled
    /// `OpenPalette` will not be visible on the live binary's first
    /// frame; a pre-rolled `SwitchWorkspace(1)` will. Useful for
    /// scripted "open the live binary on workspace 2 with these tabs".
    pub fn run(pre_roll: Option<&Scenario>) {
        if let Some(scenario) = pre_roll {
            println!("shots: window-mode pre-roll scenario '{}'", scenario.name);
            let path = persist::state_json_path();
            let tree = persist::load_or_default(&path);
            let mut driver = ShellDriver::with_tree(tree, (1600, 900));
            let mut sink = NullSink;
            scenario::run(&mut driver, scenario, &mut sink);
            let chrome = driver.chrome();
            if let Err(e) = persist::save_tree_to(&chrome.tree, &path) {
                eprintln!("shots: pre-roll persist failed: {e}");
            }
        }

        // Live binary boilerplate — mirror of `quick-shell/src/main.rs`.
        let path = persist::state_json_path();
        let tree = persist::load_or_default(&path);
        let state = State::new();
        let root = build_nirios_root(&tree);
        let mut app = App::new(state, root);
        sync_tab_presence_from_tree(&tree, app.presence_ctx_mut());
        seed_demo_editor_presence(app.presence_ctx_mut());
        app.set_presence_broadcast_scope(Some(PresenceKey::Workspace {
            id: format!("{}", tree.active.0),
        }));
        app.run();
    }
}
