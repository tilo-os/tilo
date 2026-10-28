//! Frames of a window opening from search beside the one in use, on a held
//! clock: the window in use makes room and the strip pans so both stand
//! whole on screen, the new window's border and gutter included, and the
//! window in use keeps its name in view.
use std::path::PathBuf;
use std::time::Duration;

use tilo_desktop_model::{IdMinter, Tab, Workspace};
use tilo_desktop_surface::navigation::Command;
use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
use ui::theme::Theme;
use ui::widget::{TileContent, TileHost};

/// Windows that say what they are, standing in for applications.
struct Apps;

impl TileSource for Apps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: tilo_desktop_model::TabId, app: &str) -> Option<Tab> {
        Some(app_tab(id, app))
    }
}

fn app_tab(id: tilo_desktop_model::TabId, app: &str) -> Tab {
    let label = app.rsplit('.').next().unwrap_or(app);
    let mut name = label.to_owned();
    if let Some(first) = name.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    Tab {
        id,
        title: name.clone(),
        tile: tilo_desktop_model::TileContent::LocalApp {
            app_id: app.to_owned(),
            label: name,
            note: String::new(),
        },
        app_title: None,
    }
}

/// The frames after the launch that are kept: its first, the room being
/// made, and the landing at rest.
const KEPT: [usize; 5] = [1, 4, 8, 16, AT_REST];

/// The frame by which the landing has come to rest.
const AT_REST: usize = 60;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("target/open-motion-shots"), PathBuf::from);
    std::fs::create_dir_all(&out)?;
    for name in ["dark", "light"] {
        let theme = || {
            if name == "dark" {
                Theme::dark()
            } else {
                Theme::light()
            }
        };
        for width in [1440_u32, 1024] {
            let mut minter = IdMinter::new(9);
            let chat = app_tab(minter.mint(), "so.tilo.chat");
            let tree = seed_tree(&mut minter, "Desk", vec![chat]);
            let (surface, handle) = DesktopSurface::new(tree, 9, Box::new(Apps));
            handle.set_viewport_width(f64::from(width));
            let mut frame = ui::headless::FrameHarness::new_with_theme(
                ui::state::State::new(),
                Box::new(surface),
                (width, 800),
                theme(),
            );
            let mut now = ui::time::now();
            let mut step = |frame: &mut ui::headless::FrameHarness| {
                now = now.checked_add(Duration::from_millis(16)).unwrap_or(now);
                tilo_clock::testing::with_monotonic_time(now, || frame.relayout());
                now
            };
            for _ in 0..4 {
                step(&mut frame);
            }
            handle.dispatch(
                &Command::Launch {
                    generation: "g".into(),
                    app: "so.tilo.weather".into(),
                },
                "g",
            )?;
            for n in 1..=AT_REST {
                let at = step(&mut frame);
                if KEPT.contains(&n) {
                    let path = out.join(format!("open-{name}-{width}-frame{n:02}.png"));
                    tilo_clock::testing::with_monotonic_time(at, || frame.render_png(&path));
                    println!("wrote {}", path.display());
                }
            }
        }
    }
    Ok(())
}
