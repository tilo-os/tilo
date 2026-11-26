//! Theme bundles: a document and the painter modules it names, loaded, registered and
//! drawn through the one host; and the Bauhaus theme as shipped.
use tilo_ui_base::part_paint::wasm;
use tilo_ui_base::theme::Theme;

use std::fmt::Write as _;
use std::sync::Arc;

use tilo_theme_bauhaus::Bauhaus;
use tilo_theme_sdk::wire;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::{widget::Button, widget::ButtonRole};
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme_bundle::*;
use tilo_ui_base::tokens::SurfaceRole;
use tilo_ui_base::widget::Widget as _;
use tilo_ui_base::{part_paint::PainterSet, part_paint::PartPainter as _};

const BAUHAUS: &str = include_str!("../../../themes/bauhaus/theme.json");

fn module() -> Vec<u8> {
    let params = wire::params_answer(&Bauhaus.params());
    let declines = wire::paint_answer(None);
    let data = |bytes: &[u8]| {
        let mut text = String::new();
        for byte in bytes {
            let _ = write!(text, "\\{byte:02x}");
        }
        text
    };
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 2)
            (data (i32.const 1024) "{params}")
            (data (i32.const 8192) "{declines}")
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 65536))
            (func (export "tilo_theme_call") (param i32) (result i64)
                (if (result i64) (i32.eq (i32.load8_u (i32.const 65536)) (i32.const 3))
                    (then (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const {params_len})))
                    (else (i64.or (i64.shl (i64.const 8192) (i64.const 32)) (i64.const {declines_len}))))))"#,
        params = data(&params),
        declines = data(&declines),
        params_len = params.len(),
        declines_len = declines.len(),
    ))
    .unwrap()
}

tilo_test_env::scenario! {
    /// Loading a theme bundle.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_bundle_loads_its_painters_or_is_refused;

    phase a_bundle_registers_its_painters_and_its_document_names_them;
    {
        let document = BAUHAUS.replace("\"bauhaus\"", "\"test.bundle-sets\"");
        let mut asked = Vec::new();
        let theme = load(&document, |set| {
            asked.push(set.to_owned());
            Ok(Some(module()))
        })
        .unwrap();
        assert_eq!(asked, ["test.bundle-sets"], "each set is loaded once");
        for role in [
            SurfaceRole::Window,
            SurfaceRole::Control,
            SurfaceRole::Floating,
            SurfaceRole::Bar,
        ] {
            assert_eq!(theme.finish(role).painter(), "test.bundle-sets");
        }
        assert_eq!(theme.finish(SurfaceRole::Bar).param("levels"), Some(3.0));
    }

    phase a_bundle_missing_a_module_or_carrying_a_bad_one_is_refused;
    {
        let named = |name: &str| BAUHAUS.replace("\"bauhaus\"", &format!("{name:?}"));
        assert!(matches!(
            load(&named("test.bundle-missing"), |_| Ok(None)),
            Err(BundleError::Missing(name)) if name == "test.bundle-missing"
        ));
        assert!(matches!(
            load(&named("../escape"), |_| Ok(Some(module()))),
            Err(BundleError::SetName(_))
        ));
        let imports = wat::parse_str(
            r#"(module (import "env" "clock" (func (result i64))) (memory (export "memory") 1))"#,
        )
        .unwrap();
        assert!(matches!(
            load(&named("test.bundle-imports"), |_| Ok(Some(imports.clone()))),
            Err(BundleError::Painter {
                refusal: wasm::Refusal::Imports(_),
                ..
            })
        ));
        let extra = named("test.bundle-extra").replace("\"shadow\": 3.0", "\"glow\": 1.0");
        assert!(matches!(
            load(&extra, |_| Ok(Some(module()))),
            Err(BundleError::Document(_))
        ));
    }
}

#[test]
fn the_bauhaus_theme_is_legible_and_draws_through_the_host() {
    assert!(tilo_ui_base::part_paint::register_painter_set(
        PainterSet::new("bauhaus", Arc::new(Bauhaus))
    ));
    let document: tilo_ui_base::theme::ThemeDocument = serde_json::from_str(BAUHAUS).unwrap();
    assert_eq!(document.validate(), Ok(()));
    assert!(document.findings().is_empty(), "{:?}", document.findings());
    let theme = Theme::from_document(&document).unwrap();
    assert_eq!(theme.finish(SurfaceRole::Control).painter(), "bauhaus");
    let mut button = Button::new("go", "Go").role(ButtonRole::Primary);
    button.apply_layout_external(Rect::new(100.0, 50.0, 220.0, 90.0));
    let mut scene = Scene::new();
    button.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        tilo_ui_base::presence::PresenceCtx::default().draw_env(),
    ));
    let at = (218.0, 88.0);
    crate::testing::Dispatch::new().send(
        &mut button,
        &winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: at.into(),
        },
    );
    assert!(
        button
            .cursor_shape(vello::kurbo::Point::new(at.0, at.1))
            .is_some()
    );
}

/// Staging places the bundle's document and one module for each set it names that is not built in,
/// and nothing else: what a process reads there is the theme package's bundle, never the package.
#[test]
fn staging_places_only_the_bundle_files_a_theme_names() {
    use tilo_ui_base::theme::PresetDesign as _;

    const SET: &str = "test.staged-files";
    let source = tempfile::tempdir().expect("a bundle");
    let dest = tempfile::tempdir().expect("a staged directory");
    let document = BAUHAUS.replace("\"bauhaus\"", &format!("{SET:?}"));
    std::fs::write(source.path().join(DOCUMENT), &document).expect("the document");
    std::fs::write(source.path().join(format!("{SET}.wasm")), module()).expect("the module");
    std::fs::write(source.path().join("payload-extra.txt"), b"not a theme file")
        .expect("a file a bundle does not carry");
    std::fs::write(dest.path().join("stale.txt"), b"yesterday").expect("a stale file");

    stage(source.path(), dest.path()).expect("the bundle stages");
    let mut staged: Vec<String> = std::fs::read_dir(dest.path())
        .expect("the staged directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    staged.sort();
    let mut expected = vec![DOCUMENT.to_owned(), format!("{SET}.wasm")];
    expected.sort();
    assert_eq!(
        staged, expected,
        "the document and the set it names, and nothing else"
    );
    assert_eq!(
        std::fs::read(dest.path().join(format!("{SET}.wasm"))).expect("the staged module"),
        module(),
        "the set's own module"
    );

    // A document naming only built-in sets carries no module, and the one staged before is gone.
    std::fs::write(
        source.path().join(DOCUMENT),
        serde_json::to_string(&tilo_ui_base::theme::Preset::Dark.document())
            .expect("the theme writes"),
    )
    .expect("the document");
    stage(source.path(), dest.path()).expect("the bundle stages again");
    let staged: Vec<String> = std::fs::read_dir(dest.path())
        .expect("the staged directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        staged,
        [DOCUMENT.to_owned()],
        "no module for a built-in set"
    );
}
