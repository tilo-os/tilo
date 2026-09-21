//! Wide/narrow and light/dark offscreen renders of the chart catalogue.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tilo_ui::State;
use tilo_ui::num::f32_of_u32;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::vello::Scene;
use tilo_ui::vello::kurbo::{Affine, Point};
use tilo_ui::widget::Widget;

use super::chart_gallery_scene;

fn shoot(theme: &Theme, width: u32, height: u32, inspected: bool, out: &Path) {
    let mut root = if inspected {
        chart_gallery_scene::build_inspected(theme)
    } else {
        chart_gallery_scene::build(theme)
    };
    let mut state = State::new();
    root.sync(&mut state.doc, &state.root);

    let mut text = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = root.build_layout(&mut taffy, theme, &mut map);
    let _ = taffy.compute_layout_with_measure(
        node,
        Size {
            width: AvailableSpace::Definite(f32_of_u32(width)),
            height: AvailableSpace::Definite(f32_of_u32(height)),
        },
        |known, available, id, _, _| {
            map.get(&id).map_or(Size::ZERO, |&widget| unsafe {
                (*widget).measure(known, available, &mut text)
            })
        },
    );
    root.apply_layout(&taffy, node, Point::ZERO);
    let mut scene = Scene::new();
    root.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &PresenceCtx::default(),
    );
    OffscreenRenderer::new(width, height).render_png(&scene, theme.bg_app, out);
    println!("wrote {}", out.display());
}

pub fn run(_args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let dir = PathBuf::from(
        std::env::var("CHART_GALLERY_SHOTS_OUT")
            .unwrap_or_else(|_| "target/chart-gallery-shots".into()),
    );
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("could not create {}: {error}", dir.display());
        return;
    }
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        shoot(
            &theme,
            1200,
            3000,
            false,
            &dir.join(format!("chart-gallery-{name}-wide.png")),
        );
        shoot(
            &theme,
            560,
            5940,
            false,
            &dir.join(format!("chart-gallery-{name}-narrow.png")),
        );
    }
    shoot(
        &Theme::dark(),
        1200,
        3000,
        true,
        &dir.join("chart-gallery-inspected-wide.png"),
    );
    shoot(
        &Theme::dark(),
        560,
        5940,
        true,
        &dir.join("chart-gallery-inspected-narrow.png"),
    );
}
