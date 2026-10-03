//! Offscreen shots for the shared text-input primitives that slides / docs /
//! sheets adopt: [`tilo_ui::widget::TextInput`] with a live selection highlight,
//! the click-to-type [`tilo_ui::widget::NumberInput`] (value + steppers), and the
//! embeddable [`tilo_ui::widget::ColorPicker`].
//!
//! Every widget is exercised through its real five-pass pipeline and event
//! handler — no private-field pokes. The point is a picture you can eyeball
//! that the selection band renders, the number field + steppers lay out, and
//! the colour picker embeds cleanly.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- text_shots`
//! Out:  `$TEXT_SHOTS_OUT` (default `/tmp/text_shots`)

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ROOT};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::dpi::PhysicalPosition;
use winit::event::{Modifiers, WindowEvent};

use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{ColorPicker, NumberInput, TextInput, Widget};

/// Lay a single widget out at `origin` through the real taffy pass and return
/// nothing (the widget stores its own rect). Used for the widgets whose
/// `build_layout` carries an intrinsic size (`NumberInput`, `ColorPicker`).
fn layout_one(w: &mut dyn Widget, origin: Point) {
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let node = w.build_layout(&mut taffy, &theme, &mut nm);
    let layout_result = taffy.compute_layout_with_measure(
        node,
        Size {
            width: AvailableSpace::MaxContent,
            height: AvailableSpace::MaxContent,
        },
        |known, available, node_id, _, _| {
            nm.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                (*p).measure(known, available, &mut tr)
            })
        },
    );
    if let Err(e) = layout_result {
        eprintln!("layout failed: {e}");
        return;
    }
    w.apply_layout(&taffy, node, origin);
}

fn dispatch(w: &mut dyn Widget, doc: &mut AutoCommit, ev: &WindowEvent) {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let Ok(node) = taffy.new_leaf(taffy::Style::default()) else {
        return;
    };
    let mut handled = false;
    let mut focus_target = None;
    w.handle_event(
        ev,
        &mut tilo_ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::default(),
            state: doc,
            path: &ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &taffy,
        },
    );
}

fn cursor(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: PhysicalPosition::new(x, y),
    }
}
pub fn run(_args: &[String]) {
    let dir = std::env::var("TEXT_SHOTS_OUT").unwrap_or_else(|_| "/tmp/text_shots".to_string());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);

    let theme = Theme::dark();
    let presence = PresenceCtx::default();
    let mut tr = TextRenderer::new();
    let mut doc = AutoCommit::new();

    // --- TextInput with a live selection ---------------------------------
    let mut ti = TextInput::local().with_initial_value("Hello Gallery!");
    ti.apply_layout_external(Rect::new(40.0, 40.0, 340.0, 80.0));
    ti.set_focus(true);
    // Populate glyph metrics, then select the word "Gallery" so the
    // continuous highlight band paints.
    {
        let mut s = Scene::new();
        ti.draw(&mut s, Affine::IDENTITY, &mut tr, &theme, &presence);
    }
    ti.set_selection(6, 13); // "Gallery"

    // --- NumberInput (font size, click-to-type + steppers) ---------------
    let mut ni = NumberInput::new(16.0)
        .with_range(8.0, 72.0)
        .with_step(1.0)
        .with_suffix("px");
    layout_one(&mut ni, Point::new(40.0, 100.0));
    // Hover the + stepper so its active state is visible in the shot.
    let ni_rect = ni.get_rect();
    dispatch(
        &mut ni,
        &mut doc,
        &cursor(ni_rect.x1 - 16.0, ni_rect.y0 + 16.0),
    );

    // --- ColorPicker (embeddable inspector control) ----------------------
    let mut cp = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
    layout_one(&mut cp, Point::new(40.0, 150.0));

    // --- Compose one frame -----------------------------------------------
    let mut scene = Scene::new();
    ti.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    ni.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    cp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let w = 400u32;
    let h = 400u32;
    let mut r = OffscreenRenderer::new(w, h);
    let out = dir.join("text-primitives.png");
    r.render_png(&scene, theme.tokens.colors.surfaces.content, &out);
    println!("wrote {}", out.display());

    // Sanity assertions on the exercised state.
    assert_eq!(
        ti.selection_range_public(),
        Some((6, 13)),
        "word selection is live"
    );
    assert!(
        (ni.value() - 16.0).abs() < f64::EPSILON,
        "number field seeded"
    );
    assert_eq!(cp.hex_string(), "#6366F1", "picker seeded to indigo");
    println!("OK: selection band + number field + color picker rendered");
}
