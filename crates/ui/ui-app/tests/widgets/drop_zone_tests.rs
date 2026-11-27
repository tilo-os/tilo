use std::cell::RefCell;
use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::Rc;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::*;
use winit::keyboard::*;

tilo_test_env::scenario! {
    /// A drop zone.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_drop_zone_shows_settles_and_reports_a_drop;

    phase a_drag_over_takes_your_colour_and_rest_is_quiet;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rect = Rect::new(0.0, 0.0, 400.0, 200.0);
        let paint = |face: DropZoneFace<'_>, text: &mut TextRenderer| {
            let mut scene = Scene::new();
            face.paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, text, &theme),
                rect,
                0.0,
            );
            scene.encoding().draw_tags.len()
        };
        let face = DropZoneFace::new("Drop an image to flash").message("ISO or IMG");
        let rest = paint(face, &mut text);
        let over = paint(face.drag_over(true), &mut text);
        assert_eq!(over, rest + 1, "only the tint fill is added");
        let height = face.height(&mut text, &theme, 400.0, 0.0);
        let with_action = face.height(&mut text, &theme, 400.0, 32.0);
        assert!(with_action > height + 32.0);
        assert!(face.height(&mut text, &Theme::dark().with_text_scale(2.0), 400.0, 0.0) > height);
    }

    phase a_zone_with_a_button_settles_with_the_button_centred_under_its_text;
    {
        let zone = DropZone::new("picture", "Drop a picture")
            .with_action(Button::new("picture-choose", "Choose"));
        let mut harness = tilo_ui_app::app::driver::Driver::new_with_theme(
            tilo_ui_app::State::new(),
            Box::new(zone),
            (320, 568),
            Theme::light(),
        );
        assert_eq!(harness.take_skip_report(), None, "the layout settles");
        fn find<'a>(
            node: &'a tilo_ui_app::headless::InspectionNode,
            name: &str,
        ) -> Option<&'a tilo_ui_app::headless::InspectionNode> {
            if node.widget == name {
                return Some(node);
            }
            node.children.iter().find_map(|child| find(child, name))
        }
        let inspection = harness.inspection();
        let zone = find(&inspection.root, "DropZone").expect("the zone").rect;
        let button = find(&inspection.root, "Button").expect("its button").rect;
        let (zone_mid, button_mid) = (zone[0] + zone[2] / 2.0, button[0] + button[2] / 2.0);
        assert!(
            (zone_mid - button_mid).abs() <= 1.0,
            "the button {button:?} is centred in the zone {zone:?}"
        );
    }

    phase dropped_files_reach_the_callback;
    {
        let got = Rc::new(RefCell::new(Vec::new()));
        let sink = got.clone();
        let zone = DropZone::new("import", "Drop books here")
            .on_files(move |files| sink.borrow_mut().extend(files));
        let mut harness =
            tilo_ui_app::app::driver::Driver::new(tilo_ui_app::State::new(), Box::new(zone), (400, 300));
        let [x, y, w, h] = harness.inspection().root.rect;
        harness.click_at(x + w / 2.0, y + h / 2.0);
        harness.send(&WindowEvent::DroppedFile("/home/ada/notes.epub".into()));
        assert_eq!(
            got.borrow().as_slice(),
            [PathBuf::from("/home/ada/notes.epub")]
        );
    }
}
