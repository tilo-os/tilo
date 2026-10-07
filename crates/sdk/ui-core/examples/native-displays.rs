//! Synthetic two-device topology rendered locally by the production widget.
//! No desktop, pairing, input capture, video decoder, or privileged service.
use std::path::Path;
use tilo_ui::headless::FrameHarness;
use tilo_ui::state::State;
use tilo_ui::theme::Theme;
use tilo_ui::widget::display_arrangement::{
    ArrangedDisplay, DisplayArrangement, placement::Placement,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args_os()
        .nth(1)
        .ok_or("output directory required")?;
    let out = Path::new(&out);
    std::fs::create_dir_all(out)?;
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let screens = vec![
            ArrangedDisplay {
                connector: "fixture-laptop/eDP-1".into(),
                label: "Laptop · eDP-1".into(),
                number: 1,
                placement: Placement::new(0, 0, 960, 540),
                primary: true,
                movable: true,
            },
            ArrangedDisplay {
                connector: "fixture-desktop/eDP-1".into(),
                label: "Desktop · eDP-1".into(),
                number: 2,
                placement: Placement::new(960, 0, 1280, 720),
                primary: false,
                movable: true,
            },
        ];
        let canvas = DisplayArrangement::new(screens, Vec::new(), tilo_ui::LocalState::new(None));
        let frame =
            FrameHarness::new_with_theme(State::new(), Box::new(canvas), (1000, 320), theme);
        frame.render_png(&out.join(format!("{name}.png")));
    }
    Ok(())
}
