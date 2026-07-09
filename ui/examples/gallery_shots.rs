//! Offscreen renders of the component gallery in **both** light and dark,
//! so the showcase can be self-inspected without opening a window. Drives the
//! real five-pass pipeline (sync → build_layout → compute_layout →
//! apply_layout → draw) on the same widget tree the interactive `gallery`
//! example builds.
//!
//! Run:  cargo run -p ui --example gallery_shots
//! Out:  $GALLERY_SHOTS_OUT (default /tmp/gallery_shots) — gallery-dark.png,
//!       gallery-light.png

use std::collections::HashMap;
use std::path::PathBuf;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use ui::State;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

#[path = "common/gallery_scene.rs"]
mod gallery_scene;

const WIDTH: u32 = 1200;
const HEIGHT: u32 = 1940;

fn shoot(theme: &Theme, out: PathBuf) {
    let mut state = State::new();
    let mut root = gallery_scene::build_showcase(&mut state, theme);

    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    // sync pulls the seeded control state out of the CRDT.
    root.sync(&mut state.doc, &state.root);

    let node = root.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(WIDTH as f32),
                height: AvailableSpace::Definite(HEIGHT as f32),
            },
            |known, available, node_id, _, _| {
                node_map
                    .get(&node_id)
                    .map(|&p| unsafe { (*p).measure(known, available, &mut tr) })
                    .unwrap_or(Size::ZERO)
            },
        )
        .unwrap();
    root.apply_layout(&taffy, node, Point::ZERO);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    root.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {}", out.display());
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("GALLERY_SHOTS_OUT").unwrap_or_else(|_| "/tmp/gallery_shots".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);

    shoot(&Theme::dark(), dir.join("gallery-dark.png"));
    shoot(&Theme::light(), dir.join("gallery-light.png"));
}
