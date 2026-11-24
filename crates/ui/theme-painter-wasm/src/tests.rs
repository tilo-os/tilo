//! What the interpreter refuses, what it bounds, and a call that fails leaving its parts to the
//! toolkit.

use std::fmt::Write as _;

use tilo_theme_sdk::kurbo::Size;
use tilo_theme_sdk::wire;

use super::*;

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
        load(&module).err(),
        Some(Refusal::Imports("env::clock".into()))
    );
    assert!(matches!(load(b"not wasm"), Err(Refusal::Invalid(_))));
    let no_call = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 0)))"#,
    )
    .unwrap();
    assert!(matches!(load(&no_call), Err(Refusal::Exports(_))));
    // Parameters that are not a list are refused at load.
    assert_eq!(load(&canned(&[1, 2], &[0])).err(), Some(Refusal::Params));
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
    let painter = load(&looping).unwrap();
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
    assert!(matches!(load(&starts_looping), Err(Refusal::Start(_))));
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
    let painter = load(&greedy).unwrap();
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
    let painter = load(&past_memory).unwrap();
    assert_eq!(
        painter.paint(Part::ButtonContainer, size, state, 1.0, &tokens),
        None
    );
    assert!(painter.is_broken());
    let garbage = load(&canned(&wire::params_answer(&[]), &[7, 7, 7])).unwrap();
    assert_eq!(
        garbage.paint(Part::ButtonContainer, size, state, 1.0, &tokens),
        None
    );
    assert!(garbage.is_broken());
    let declines = load(&canned(
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

#[test]
fn a_set_runs_on_the_machine_code_the_device_daemon_compiled_for_it() {
    let params = wire::params_answer(&[tilo_theme_sdk::ParamSpec::new(
        "levels",
        Some(4.0),
        2.0..=16.0,
    )]);
    let painted = tilo_theme_sdk::PaintedPart::new(tilo_theme_sdk::PartScene::new());
    let answer = wire::paint_answer(Some(&painted));
    let module = canned(&params, &answer);

    let dir = tempfile::tempdir().unwrap();
    let cache = tilo_wasm_aot::Cache::at(dir.path()).unwrap();
    let entry = cache.ensure(&module).unwrap();

    // The guest itself, so a refusal names what the compiled path saw.
    #[cfg(not(target_arch = "wasm32"))]
    compiled::Guest::new(&module, &entry).expect("the compiled guest starts");
    let compiled = load_with(&module, Some(&entry)).unwrap();
    assert!(
        matches!(&*compiled.guest.lock_recover(), Exec::Compiled(_)),
        "the set must run on the daemon's compiled module, not fall back to the interpreter"
    );
    let interpreted = load(&module).unwrap();
    assert_eq!(compiled.params(), interpreted.params());
    let paint = |painter: &WasmPainter| {
        painter.paint(
            tilo_theme_sdk::Part::ButtonContainer,
            Size::new(120.0, 40.0),
            tilo_theme_sdk::PartState::default(),
            0.0,
            &tilo_theme_sdk::PartTokens::default(),
        )
    };
    assert_eq!(paint(&compiled).is_some(), paint(&interpreted).is_some());
    assert!(!compiled.is_broken());
}
