//! The wasm host: what it refuses, what it bounds, and a module's painter drawing and taking the
//! pointer through the toolkit's one path.

use std::fmt::Write as _;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Size};
use vello::peniko::Color;
use winit::event::{DeviceId, WindowEvent};

use super::wasm::{self, Refusal};
use super::*;
use crate::text_render::TextRenderer;
use crate::widget::{Button, Widget};
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
    let finish = crate::tokens::Finish::named("test.wasm-triangles", [("levels", 8.0)]).unwrap();
    assert_eq!(finish.param("levels"), Some(8.0));
    let theme = Theme::dark()
        .with_painter_set(SurfaceRole::Control, set)
        .unwrap();
    let mut button = Button::new("go", "Go");
    button.apply_layout_external(Rect::new(100.0, 50.0, 220.0, 90.0));
    let mut scene = Scene::new();
    button.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        &crate::presence::PresenceCtx::default(),
    );
    assert!(moved_to(&mut button, (160.0, 60.0)));
    assert!(moved_to(&mut button, (116.0, 88.0)));
    assert!(!moved_to(&mut button, (104.0, 54.0)));
    assert!(!moved_to(&mut button, (216.0, 54.0)));
}

#[test]
fn a_module_that_imports_anything_is_refused() {
    let module = wat::parse_str(
        r#"(module
            (import "env" "clock" (func (result i64)))
            (memory (export "memory") 1)
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 0))
            (func (export "tilo_theme_call") (param i32) (result i64) (i64.const 0)))"#,
    )
    .unwrap();
    assert_eq!(
        wasm::load(&module).err(),
        Some(Refusal::Imports("env::clock".into()))
    );
    assert!(matches!(wasm::load(b"not wasm"), Err(Refusal::Invalid(_))));
    let no_call = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 0)))"#,
    )
    .unwrap();
    assert!(matches!(wasm::load(&no_call), Err(Refusal::Exports(_))));
    // Parameters that are not a list are refused at load.
    assert_eq!(
        wasm::load(&canned(&[1, 2], &[0])).err(),
        Some(Refusal::Params)
    );
}

/// A painter that loops is stopped by its budget, and its parts go to the toolkit from then on; one
/// that starts by looping is never loaded.
#[test]
fn a_looping_painter_is_stopped_and_left_to_the_toolkit() {
    let looping = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 2)
            (data (i32.const 1024) "{params}")
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 65536))
            (func (export "tilo_theme_call") (param i32) (result i64)
                (if (i32.eq (i32.load8_u (i32.const 65536)) (i32.const 3))
                    (then (return (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const 4)))))
                (loop $forever (br $forever))
                (i64.const 0)))"#,
        params = data(&wire::params_answer(&[])),
    ))
    .unwrap();
    let painter = wasm::load(&looping).unwrap();
    let paint = || {
        painter.paint(
            Part::ButtonContainer,
            Size::new(120.0, 40.0),
            PartState::settled(States::REST),
            1.0,
            &PartTokens::default(),
        )
    };
    assert!(!painter.is_broken());
    assert_eq!(paint(), None);
    assert!(painter.is_broken());
    assert_eq!(paint(), None, "a broken set is not called again");
    let starts_looping = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func $start (loop $forever (br $forever)))
            (start $start)
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 0))
            (func (export "tilo_theme_call") (param i32) (result i64) (i64.const 0)))"#,
    )
    .unwrap();
    assert!(matches!(
        wasm::load(&starts_looping),
        Err(Refusal::Start(_))
    ));
}

/// Memory past the cap is refused, an answer past the bound or out of the module's memory is not read,
/// and bytes that are not an answer break the set.
#[test]
fn a_painter_stays_inside_its_memory_and_answers_are_bounded() {
    let greedy = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 2)
            (data (i32.const 1024) "{params}")
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 65536))
            (func (export "tilo_theme_call") (param i32) (result i64)
                (if (i32.eq (i32.load8_u (i32.const 65536)) (i32.const 3))
                    (then (return (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const 4)))))
                (drop (memory.grow (i32.const 4096)))
                (i64.const 0)))"#,
        params = data(&wire::params_answer(&[])),
    ))
    .unwrap();
    let state = PartState::settled(States::REST);
    let size = Size::new(10.0, 10.0);
    let tokens = PartTokens::default();
    let painter = wasm::load(&greedy).unwrap();
    assert_eq!(
        painter.paint(Part::ButtonContainer, size, state, 1.0, &tokens),
        None
    );
    assert!(painter.is_broken(), "growing past the cap traps");
    // An answer claiming more bytes than the module's memory holds.
    let past_memory = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 2)
            (data (i32.const 1024) "{params}")
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 65536))
            (func (export "tilo_theme_call") (param i32) (result i64)
                (if (i32.eq (i32.load8_u (i32.const 65536)) (i32.const 3))
                    (then (return (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const 4)))))
                (i64.or (i64.shl (i64.const 100000) (i64.const 32)) (i64.const 1000000))))"#,
        params = data(&wire::params_answer(&[])),
    ))
    .unwrap();
    let painter = wasm::load(&past_memory).unwrap();
    assert_eq!(
        painter.paint(Part::ButtonContainer, size, state, 1.0, &tokens),
        None
    );
    assert!(painter.is_broken());
    let garbage = wasm::load(&canned(&wire::params_answer(&[]), &[7, 7, 7])).unwrap();
    assert_eq!(
        garbage.paint(Part::ButtonContainer, size, state, 1.0, &tokens),
        None
    );
    assert!(garbage.is_broken());
    let declines = wasm::load(&canned(
        &wire::params_answer(&[]),
        &wire::paint_answer(None),
    ))
    .unwrap();
    assert_eq!(
        declines.paint(Part::ButtonContainer, size, state, 1.0, &tokens),
        None
    );
    assert!(!declines.is_broken());
}
