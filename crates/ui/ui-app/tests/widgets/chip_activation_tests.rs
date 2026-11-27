use std::cell::*;
use std::collections::*;
use std::path::*;
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
use tilo_ui_app::widget::internals::chip::PillMetrics;
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
use winit::event::*;
use winit::keyboard::*;

tilo_test_env::scenario! {
    /// A chip that is pressed or opens a menu.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_chip_that_acts_has_a_fingers_area_and_one_resource;

    phase a_chip_that_navigates_is_a_focusable_button;
    {
        let opened = Rc::new(Cell::new(0));
        let counter = Rc::clone(&opened);
        let mut chip = Chip::new("design")
            .with_id("tag-design")
            .on_open(move || counter.set(counter.get() + 1));
        assert!(chip.is_focusable());
        let node = Widget::accessibility(&chip).unwrap();
        assert_eq!(node.role, tilo_ui_base::a11y::Role::Button);
        assert_eq!(node.name.as_deref(), Some("design"));
        assert_eq!(chip.test_id(), Some("tag-design"));
        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_action(
            &mut chip,
            &tilo_ui_base::a11y::Action::Activate,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(opened.get(), 1, "Enter, Space and AT press the chip");
        let mut harness = tilo_ui_app::app::driver::Driver::new(
            tilo_ui_app::State::new(),
            Box::new(tilo_ui_app::widget::Row::new().push(Box::new(chip))),
            (400, 100),
        );
        let [x, y, w, h] = harness.inspection().root.children[0].rect;
        harness.click_at(x + w / 2.0, y + h / 2.0);
        assert_eq!(opened.get(), 2);
        assert!(!Chip::new("label").is_focusable());
    }

    // On a touchscreen a chip a person presses takes a finger's target of height in its line,
    // with its pill the selector height centred in it, so a press above or below the pill
    // lands on the chip and never on what is next to it; a label chip, or a chip under a
    // mouse, keeps the pill's height.
    phase a_pressed_chip_takes_a_fingers_area_on_touch;
    {
        use tilo_ui_base::interaction::InteractionEnvironment;
        let pill = f64::from(tilo_ui_base::tokens::Metrics::BASE.selector_height);
        let finger = f64::from(InteractionEnvironment::TOUCH.minimum_target());
        let chips: [(&str, fn() -> Chip, bool); 3] = [
            (
                "navigating",
                || Chip::new("3 replies").on_click(|| {}),
                true,
            ),
            ("removable", || Chip::new("planning").removable(), true),
            ("label", || Chip::new("draft"), false),
        ];
        for (name, chip, pressed) in chips {
            for (input, spare) in [
                (InteractionEnvironment::POINTER, pill),
                (
                    InteractionEnvironment::TOUCH,
                    if pressed { finger } else { pill },
                ),
            ] {
                let mut harness = tilo_ui_app::app::driver::Driver::new(
                    tilo_ui_app::State::new(),
                    Box::new(tilo_ui_app::widget::Row::new().push(Box::new(chip()))),
                    (400, 100),
                );
                harness.set_interaction_environment(input);
                let [_, _, _, h] = harness.inspection().root.children[0].rect;
                assert!(
                    (h - spare).abs() < 0.5,
                    "the {name} chip takes {spare} for {input:?}, not {h}"
                );
            }
        }
    }

    // A chip that both runs a press and offers its own removal publishes the × as a control of
    // its own: an agent removes the chip through it, under an id no other spec of the chip
    // shadows, and the open press, a closure, is never published beside a typed action.
    phase a_chip_with_a_press_and_a_remove_publishes_its_remove_as_its_own_control;
    {
        let opened = Rc::new(Cell::new(0));
        let removed = Rc::new(Cell::new(0));
        let counted_open = Rc::clone(&opened);
        let counted_remove = Rc::clone(&removed);
        let chip = Chip::new("Book: The Tideglass")
            .with_id("view-chip:book")
            .on_open(move || counted_open.set(counted_open.get() + 1))
            .on_remove(move || counted_remove.set(counted_remove.get() + 1));

        let mut harness = tilo_ui_app::app::driver::Driver::new(
            tilo_ui_app::State::new(),
            Box::new(tilo_ui_app::widget::Row::new().push(Box::new(chip))),
            (400, 100),
        );
        let resources = harness.resources();
        let published: Vec<&tilo_actions::ResourceSpec> = resources
            .iter()
            .filter(|resource| {
                resource
                    .id
                    .as_str()
                    .starts_with(tilo_ui_app::headless::CONTROL_RESOURCE_PREFIX)
            })
            .collect();
        let ids: std::collections::HashSet<&str> =
            published.iter().map(|resource| resource.id.as_str()).collect();
        assert_eq!(ids.len(), published.len(), "one spec per control id");
        assert!(
            published
                .iter()
                .filter(|resource| resource.label == "Book: The Tideglass")
                .all(|resource| resource.actions.iter().all(|action| action.id != "activate")),
            "the open press is not published: {ids:?}"
        );
        let remove = published
            .iter()
            .find(|resource| resource.label == "Remove Book: The Tideglass")
            .expect("the chip publishes its ×");
        assert!(
            remove.actions.iter().any(|action| action.id == "activate"),
            "the × is pressed"
        );
        let remove = remove.id.as_str().to_owned();
        harness
            .invoke_published(&remove, "activate", &serde_json::Map::new())
            .expect("the remove answers");
        assert_eq!(removed.get(), 1, "the chip's own remove handler ran");

        let press = harness.a11y_nodes_named("Book: The Tideglass");
        assert_eq!(press.len(), 1, "the chip itself is one control");
        harness
            .perform_a11y_action(press[0], &tilo_ui_base::a11y::Action::Activate)
            .expect("the press answers");
        assert_eq!(opened.get(), 1, "the chip's own press handler ran");
    }

    phase a_menu_chip_reserves_its_chevron_and_a_wide_face_centres_its_label;
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let plain = ChipFace::intrinsic_width("Sort", ChipLead::None, false, &mut text, &theme);
        let menu = PillMetrics::of(&theme, TextRole::Label).intrinsic_width(
            "Sort",
            ChipLead::None,
            true,
            0.0,
            &mut text,
        );
        assert!(menu > plain);
        let chip = Chip::new("Sort").menu().on_open(|| {});
        let node = Widget::accessibility(&chip).unwrap();
        assert_eq!(node.state.expanded, Some(false));
        let paint_x = |width: f64, text: &mut TextRenderer| {
            let mut scene = Scene::new();
            ChipFace::new(Rect::new(0.0, 0.0, width, ChipFace::height(&theme)), "q").paint(
                &mut scene,
                Affine::IDENTITY,
                text,
                &theme,
            );
            scene.encoding().resources.glyph_runs[0]
                .transform
                .translation[0]
        };
        let narrow = paint_x(plain.max(28.0), &mut text);
        let wide = paint_x(120.0, &mut text);
        assert!(f64::from(wide) > 40.0, "centred: {wide}");
        assert!(wide > narrow);
    }
}
