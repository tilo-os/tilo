//! Pictures of the card a window shows when its app stops answering.
//!
//! One desk with two windows, the first one's app silent: the card stands
//! over that window, the other stays as it was. Captured in both themes at
//! the wide and the compact viewport, through the production frame path
//! ([`ui::headless::FrameHarness`]).
//!
//! ```text
//! cargo run -p tilo-desktop --example hung-card-shots -- ~/.cache/hung-shots
//! ```

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    reason = "a capture binary reports the files it wrote"
)]

use std::path::PathBuf;

use tilo_desktop_model::{IdMinter, Tab, TileContent, Workspace};
use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
use ui::headless::FrameHarness;
use ui::state::State;
use ui::theme::Theme;
use ui::widget::{TileContent as Shown, TileHost};

/// The windows' own pictures: a note and a letter, named by their tab.
struct Windows;

impl TileSource for Windows {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        let (detail, resting) = match tab.title.as_str() {
            "Notes" => (
                "Shopping list\nmilk, bread, coffee beans, a new lamp for the hall",
                "Yesterday, 18:42",
            ),
            _ => (
                "Re: Thursday\nYes, the room is booked from seven. See you there.",
                "Nadia, this morning",
            ),
        };
        TileHost::new(Shown::Notice {
            title: format!("{}\n\n{detail}", tab.title),
            detail: resting.to_owned(),
        })
    }
}

fn desk(theme: Theme, viewport: (u32, u32)) -> (FrameHarness, tilo_desktop_surface::DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let window = |minter: &mut IdMinter, title: &str, app: &str, surface_id: u64| Tab {
        id: minter.mint(),
        title: title.to_owned(),
        tile: TileContent::Foreign {
            app_id: app.to_owned(),
            surface_id,
        },
        app_title: None,
    };
    let notes = window(&mut minter, "Notes", "so.tilo.notes", 41);
    let mail = window(&mut minter, "Mail", "so.tilo.mail", 42);
    let tree = seed_tree(&mut minter, "Desk", vec![notes, mail]);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Windows));
    let mut frame = FrameHarness::for_root(State::new(), surface, viewport, theme);
    frame.relayout();
    frame.poll_frame().expect("a frame draws the desk");
    (frame, handle)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("hung-shots"), PathBuf::from);
    std::fs::create_dir_all(&output)?;
    for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        for (size, viewport) in [("wide", (1280, 800)), ("compact", (420, 800))] {
            let (mut frame, handle) = desk(theme.clone(), viewport);
            // The first window's app stopped answering; the desk's host says
            // so, and the card is drawn in the frame this asks for.
            handle.set_unresponsive_windows(vec![41]);
            frame.request_layout();
            frame.poll_frame().expect("a frame draws the card");
            frame.render_png(&output.join(format!("{theme_name}-{size}.png")));
            println!("{theme_name} {size}: {}", output.display());
        }
    }
    Ok(())
}
