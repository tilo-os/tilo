//! A painter set loaded from wasm drawing through the toolkit's one path: the parts it draws, the
//! parameters it names, and the shape that takes the pointer. What the interpreter refuses and bounds
//! is tested beside it (crates/ui/theme-painter-wasm).
use tilo_ui_base::tokens::SurfaceRole;
use tilo_ui_base::theme::Theme;

use std::fmt::Write as _;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::Color;
use winit::event::{DeviceId, WindowEvent};

use tilo_ui_base::part_paint::wasm;
use tilo_ui_base::part_paint::*;
use tilo_ui_base::text_render::TextRenderer;
use crate::widget::Button;
use tilo_ui_base::widget::Widget;
use tilo_theme_sdk::wire;

/// `bytes` as a WAT data string.
fn data(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in bytes {
        let _ = write!(text, "\\{byte:02x}");
    }
    text
}

/// A module that answers a params request with `params` and every other request with `answer`, as fixed
/// bytes, and imports nothing.
fn canned(params: &[u8], answer: &[u8]) -> Vec<u8> {
    let text = format!(
        r#"(module
            (memory (export "memory") 2)
            (data (i32.const 1024) "{params}")
            (data (i32.const 8192) "{answer}")
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 65536))
            (func (export "tilo_theme_call") (param i32) (result i64)
                (if (result i64) (i32.eq (i32.load8_u (i32.const 65536)) (i32.const 3))
                    (then (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const {params_len})))
                    (else (i64.or (i64.shl (i64.const 8192) (i64.const 32)) (i64.const {answer_len}))))))"#,
        params = data(params),
        answer = data(answer),
        params_len = params.len(),
        answer_len = answer.len(),
    );
    wat::parse_str(text).unwrap()
}

/// The triangle a 120 by 40 button is drawn as: its apex at the top centre, its foot a tenth of the
/// width in from each side.
fn triangle() -> BezPath {
    let mut path = BezPath::new();
    path.move_to((60.0, 0.0));
    path.line_to((108.0, 40.0));
    path.line_to((12.0, 40.0));
    path.close_path();
    path
}

fn triangle_module() -> Vec<u8> {
    let mut scene = PartScene::new();
    scene.fill(triangle(), Brush::Solid(Color::from_rgb8(0xd0, 0x30, 0x20)));
    let painted = PaintedPart::new(scene).with_hit(triangle());
    canned(
        &wire::params_answer(&[ParamSpec::new("levels", Some(4.0), 2.0..=16.0)]),
        &wire::paint_answer(Some(&painted)),
    )
}

fn moved_to(button: &mut Button, at: (f64, f64)) -> bool {
    crate::testing::Dispatch::new().send(
        button,
        &WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: at.into(),
        },
    );
    Widget::cursor_shape(button, Point::new(at.0, at.1)).is_some()
}

/// A theme naming a set loaded from wasm draws a button with it, and the button takes the pointer in
/// the shape the module drew, as a built-in set's would.
#[test]
fn a_wasm_set_draws_a_button_and_its_shape_takes_the_pointer() {
    let set = wasm::painter_set("test.wasm-triangles", &triangle_module()).unwrap();
    assert_eq!(
        set.painter().params(),
        vec![ParamSpec::new("levels", Some(4.0), 2.0..=16.0)]
    );
    assert!(register_painter_set(set.clone()));
    let finish = tilo_ui_base::tokens::Finish::named("test.wasm-triangles", [("levels", 8.0)]).unwrap();
    assert_eq!(finish.param("levels"), Some(8.0));
    let theme = Theme::dark()
        .with_painter_set(SurfaceRole::Control, set)
        .unwrap();
    let mut button = Button::new("go", "Go");
    button.apply_layout_external(Rect::new(100.0, 50.0, 220.0, 90.0));
    let mut scene = Scene::new();
    button.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        tilo_ui_base::presence::PresenceCtx::default().draw_env(),
    ));
    assert!(moved_to(&mut button, (160.0, 60.0)));
    assert!(moved_to(&mut button, (116.0, 88.0)));
    assert!(!moved_to(&mut button, (104.0, 54.0)));
    assert!(!moved_to(&mut button, (216.0, 54.0)));
}
