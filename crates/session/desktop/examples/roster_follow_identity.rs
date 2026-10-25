//! Fixture-only screenshots of production People focus across presence changes.
use std::error::Error;
use std::path::Path;
use tilo_desktop::collaborators::{
    CollaboratorsModel, CollaboratorsOverlay, RecentChange, RosterRow, RosterStatus,
};
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
            is_bot: false,
            role: None,
            invited: false,
            activity: None,
            status: None,
            recent: Vec::new(),
            followable: true,
            availability: ui::Availability::Available,
            call: None,
            join: None,
            own_device: false,
        })
        .collect()
}

fn paint(overlay: &mut CollaboratorsOverlay, rows: &[RosterRow], theme: &Theme) -> Scene {
    paint_here(overlay, rows, theme, None, 0)
}

fn paint_here(
    overlay: &mut CollaboratorsOverlay,
    rows: &[RosterRow],
    theme: &Theme,
    expanded_id: Option<&str>,
    watching: usize,
) -> Scene {
    let mut scene = Scene::new();
    overlay.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        theme,
        &CollaboratorsModel {
            host: Rect::new(0.0, 0.0, 800.0, 600.0),
            workspace: "Launch",
            rows,
            unavailable: None,
            exposure: None,
            may_invite: false,
            may_pin: false,
            followed_id: None,
            following_pairs: &[],
            expanded_id,
            watching,
            waved: &[],
            spectating: false,
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
        // Controls in order: Ada's Wave, Ada's Follow, …
        if !overlay.focus_accessibility_path(&[2]) {
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
        // Who is here and what they did: a live status, a recent summary
        // (collapsed, then expanded), an agent, and two spectators.
        let mut here = rows(&theme);
        let [ada, scribe] = here.as_mut_slice() else {
            return Err("the fixture has two rows".into());
        };
        ada.activity = Some("Launch notes".into());
        ada.status = Some(RosterStatus {
            text: "Tightening the launch notes".into(),
            source: tilo_desktop::collaborators::LineSource::Written,
        });
        ada.recent = vec![
            RecentChange {
                text: "Rewrote the intro".into(),
                when: "4 min ago".into(),
                entry: None,
            },
            RecentChange {
                text: "Moved the FAQ under Pricing".into(),
                when: "12 min ago".into(),
                entry: None,
            },
        ];
        scribe.label = "Scribe".into();
        scribe.initial = 'S';
        scribe.is_agent = true;
        scribe.activity = Some("Terminal".into());
        let mut overlay = CollaboratorsOverlay::default();
        for (suffix, expanded) in [("collapsed", None), ("expanded", Some("ada"))] {
            let scene = paint_here(&mut overlay, &here, &theme, expanded, 2);
            renderer.render_png(
                &scene,
                theme.tokens.colors.surfaces.base,
                out.join(format!("{name}-here-{suffix}.png")),
            );
        }
    }
    Ok(())
}
