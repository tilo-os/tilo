//! Content-card framework fixture. No app migration is implied.
//! `cargo run -p ui-examples --example card-composition-shots -- /tmp/card-composition`
//! `TILO_SHOTS_THEME=<preset>` (`light`, `high-contrast`, any `Theme::named`)
//! renders the same captures on that preset; every PNG is composited on the
//! theme's own `bg_app`.
use std::path::PathBuf;
use tilo_ui::headless::FrameHarness;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Actions, Button, Card, CardGrid, Container, Element, Scrollable, Text};

// The automatic authoring surface contains no dimensions or viewport branches.
fn cards() -> Element {
    Box::new(CardGrid::new(vec![
        Box::new(Card::new(Box::new(Text::new("A focused terminal for everyday work.")))
            .with_title("Terminal")
            .with_footer(Actions::new(Button::new("terminal-open", "Open", None).primary()))),
        Box::new(Card::new(Box::new(Text::new("Keep your documents and projects together. Search shared folders, review recent changes, and continue working with your team across connected devices.")))
            .with_title("Workspace")
            .with_footer(Actions::new(Button::new("workspace-install", "Install workspace", None).primary())
                .secondary(Button::new("workspace-details", "Details", None)))),
        Box::new(Card::new(Box::new(Text::new("Your connections are ready. Refresh remote services to discover updates and sync changes from your other devices.")))
            .with_title("Remote connections")
            .with_footer(Actions::new(Button::new("remote-refresh", "Refresh all connections", None)
                .with_icon(tilo_ui::icons::Icon::Refresh).primary())
                .secondary(Button::new("remote-details", "View details", None)))),
    ]))
}

fn page() -> Element {
    Box::new(Scrollable::new_vertical().with_child(Box::new(Container::new().with_child(cards()))))
}

fn shot_theme() -> Theme {
    std::env::var("TILO_SHOTS_THEME")
        .ok()
        .and_then(|name| Theme::named(&name))
        .unwrap_or_else(Theme::dark)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("/tmp/card-composition"), PathBuf::from);
    std::fs::create_dir_all(&output)?;
    for (name, viewport, scale) in [
        ("wide", (1080, 640), 1.0),
        ("narrow", (340, 900), 1.0),
        ("large-text", (1080, 1000), 2.0),
        ("narrow-large-text", (340, 1200), 2.0),
    ] {
        let mut harness = FrameHarness::new_with_theme(
            tilo_ui::State::new(),
            page(),
            viewport,
            shot_theme().with_text_scale(scale),
        );
        harness.render_png(&output.join(format!("{name}.png")));
        std::fs::write(
            output.join(format!("{name}.json")),
            harness.inspection_json()?,
        )?;
        if name == "narrow-large-text" {
            harness.click_at(5.0, 5.0);
            harness.scroll(0.0, -1000.0);
            harness.render_png(&output.join("narrow-large-text-end.png"));
            std::fs::write(
                output.join("narrow-large-text-end.json"),
                harness.inspection_json()?,
            )?;
        }
    }
    let mut resized =
        FrameHarness::new_with_theme(tilo_ui::State::new(), page(), (1080, 640), shot_theme());
    resized.resize((340, 900))?;
    resized.render_png(&output.join("resized-narrow.png"));
    std::fs::write(
        output.join("resized-narrow.json"),
        resized.inspection_json()?,
    )?;
    println!("Content-card captures: {}", output.display());
    Ok(())
}
