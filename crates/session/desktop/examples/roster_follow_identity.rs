//! Fixture-only screenshots of production People focus across presence changes.
use std::error::Error;
use std::path::Path;
use tilo_desktop::collaborators::{CollaboratorsModel, CollaboratorsOverlay, RosterRow};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

fn rows(theme: &Theme) -> Vec<RosterRow> {
    [("Ada", 'A'), ("Bob", 'B')]
        .into_iter()
        .map(|(name, initial)| RosterRow {
            id: name.to_ascii_lowercase(),
            label: name.into(),
            initial,
            color: theme.tokens.colors.presence.of_rgb((114, 139, 180)),
            is_agent: false,
            role: None,
            invited: false,
            activity: None,
            status: None,
            followable: true,
        })
        .collect()
}

fn paint(overlay: &mut CollaboratorsOverlay, rows: &[RosterRow], theme: &Theme) -> Scene {
    let mut scene = Scene::new();
    overlay.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        theme,
        &CollaboratorsModel {
            host: Rect::new(0.0, 0.0, 800.0, 600.0),
            rows,
            own_initial: 'Q',
            own_color: theme.tokens.colors.presence.of_rgb((170, 120, 130)),
            unavailable: None,
            may_invite: false,
            followed_id: None,
            following_pairs: &[],
            own_status: None,
            own_activity: None,
            own_status_automatic: false,
            status_sharing: ui::StatusSharing::Automatic,
            status_editing: false,
            status_selection: ui::text_selection::Selection::default(),
            screen_exposure: None,
        },
        None,
    );
    scene
}

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("provide output directory")?;
    let out = Path::new(&directory);
    std::fs::create_dir_all(out)?;
    for (name, theme) in [
        ("light", Theme::light()),
        ("dark125", Theme::dark().with_text_scale(1.25)),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let mut renderer = ui::offscreen::OffscreenRenderer::new(800, 600);
        let mut overlay = CollaboratorsOverlay::default();
        let mut people = rows(&theme);
        let _ = paint(&mut overlay, &people, &theme);
        if !overlay.focus_accessibility_path(&[5]) {
            return Err("painted Ada Follow is not addressable".into());
        }
        people.swap(0, 1);
        let scene = paint(&mut overlay, &people, &theme);
        renderer.render_png(
            &scene,
            theme.tokens.colors.surfaces.base,
            out.join(format!("{name}-reordered.png")),
        );
        people.retain(|row| row.id != "ada");
        let scene = paint(&mut overlay, &people, &theme);
        renderer.render_png(
            &scene,
            theme.tokens.colors.surfaces.base,
            out.join(format!("{name}-departed.png")),
        );
    }
    Ok(())
}
