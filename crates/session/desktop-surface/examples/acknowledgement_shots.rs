//! Captures of an acknowledgement ("Link copied") over a desktop with
//! windows, in the themes a person may use. The material only reads in
//! context, so the pill is shown over real windows, not a flat backdrop.
use std::path::PathBuf;
use tilo_desktop_model::{IdMinter, Tab, Workspace};
use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
use ui::theme::Theme;
use ui::widget::{TileContent, TileHost};

/// Windows that say what they are, standing in for applications.
struct Windows;

impl TileSource for Windows {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args().nth(1).map_or_else(
        || PathBuf::from("target/acknowledgement-shots"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&out)?;
    for (name, theme, size) in [
        ("dark", Theme::dark(), (1280_u32, 800_u32)),
        ("light", Theme::light(), (1280, 800)),
        (
            "large-text",
            Theme::dark().with_text_scale(2.0),
            (1280, 800),
        ),
        ("narrow", Theme::light(), (420, 760)),
    ] {
        let theme = theme.with_reduce_motion(true);
        let mut minter = IdMinter::new(7);
        let tabs = ["Chat", "Notes"]
            .into_iter()
            .map(|title| Tab {
                id: minter.mint(),
                title: title.into(),
                tile: tilo_desktop_model::TileContent::LocalApp {
                    app_id: format!("so.tilo.{}", title.to_lowercase()),
                    label: title.into(),
                    note: String::new(),
                },
                app_title: None,
            })
            .collect();
        let tree = seed_tree(&mut minter, "Desk", tabs);
        let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Windows));
        let mut frame = ui::headless::FrameHarness::new_with_theme(
            ui::state::State::new(),
            Box::new(surface),
            size,
            theme,
        );
        for _ in 0..4 {
            frame.relayout();
        }
        handle.acknowledge("Link copied: open it on the new device");
        frame.relayout();
        frame.render_png(&out.join(format!("{name}.png")));
    }
    Ok(())
}
