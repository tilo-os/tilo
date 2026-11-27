use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::{FontChoice, TextStyle, TextWeight};
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

tilo_test_env::scenario! {
    fn a_text_measures_clamps_scales_and_caches;

    phase a_large_title_text_shows_its_pages_title_and_a_paragraph_does_not;
    {
        let text = |role: TextRole| Text::new("Q3 plan").with_role(role);
        assert_eq!(
            Widget::own_page_title(&text(TextRole::LargeTitle)).as_deref(),
            Some("Q3 plan")
        );
        assert_eq!(
            Widget::own_page_title(&text(TextRole::Title)).as_deref(),
            Some("Q3 plan"),
            "the old large title role names the page too"
        );
        assert_eq!(
            Widget::own_page_title(&text(TextRole::Body)),
            None,
            "a paragraph that repeats the words is no heading"
        );
    }

    phase a_truncating_text_measures_to_its_box_and_ends_in_an_ellipsis;
    {
        let said = "Quarterly planning review with the whole team";
        let box_width = 90.0;
        let mut text = Text::new(said).with_truncate(true);
        assert!(
            TextRenderer::new()
                .measure_style(said, None, text.text_style())
                .0
                > box_width * 2.0,
            "the text is longer than its box"
        );
        let mut renderer = lay_out_at(&mut text, box_width);
        let rect = text.get_rect();
        assert!(
            rect.width() <= f64::from(box_width) + 0.1,
            "it takes its box ({}) and no more: {rect:?}",
            box_width
        );
        let line = f64::from(text.text_style().line_height_px_at(renderer.text_scale()));
        assert!(
            rect.height() <= line + 0.5,
            "it stands one line tall ({line}), not {}: {rect:?}",
            rect.height()
        );
        let style = text.text_style().clone();
        let last = drawn_glyph_runs(&mut text, &mut renderer);
        assert_eq!(last.len(), 1, "one line is painted: {last:?}");
        assert_eq!(
            last[0],
            ellipsis_glyph(&mut renderer, &style),
            "the line ends in an ellipsis"
        );
        let free = Widget::measure(
            &mut Text::new(said).with_truncate(true),
            taffy::Size {
                width: None,
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::MinContent,
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        assert!(
            free.width < 1.0,
            "a truncating text's width holds no word: {free:?}"
        );
    }

    phase a_text_at_its_min_content_width_stands_as_tall_as_it_wraps_there;
    {
        // Short words share a line once the longest word sets the width.
        let said = "a b c d e f g h extraordinarily";
        let mut renderer = TextRenderer::new();
        let mut text = Text::new(said);
        let narrowest = Widget::measure(
            &mut text,
            taffy::Size {
                width: None,
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::MinContent,
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        let there = Widget::measure(
            &mut text,
            taffy::Size {
                width: Some(narrowest.width),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(narrowest.width),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        assert_eq!(
            narrowest, there,
            "its min-content size is the size it lays out to at that width"
        );
        let line = text.text_style().line_height_px_at(renderer.text_scale());
        assert!(
            narrowest.height < line * 8.0,
            "short words share lines at the longest word's width: {narrowest:?}"
        );
    }

    phase a_one_line_text_keeps_its_line_whole_unless_it_elides;
    {
        let said = "12:59 PM";
        let mut renderer = tilo_ui_base::font_catalog::fixture_renderer();
        let mut measure = |text: &mut Text, width| {
            Widget::measure(
                text,
                taffy::Size {
                    width: None,
                    height: None,
                },
                taffy::Size {
                    width,
                    height: taffy::AvailableSpace::MaxContent,
                },
                &mut renderer,
            )
        };
        let line = measure(&mut Text::new(said), taffy::AvailableSpace::MaxContent);
        // At its narrowest a wrapping text answers its longest word, which would let a row
        // give it less room than the one line it draws.
        let kept = measure(
            &mut Text::new(said).with_lines(Some(1)),
            taffy::AvailableSpace::MinContent,
        );
        assert!(
            (kept.width - line.width).abs() < 0.5,
            "a one-line text that does not elide asks for its whole line ({kept:?}), not its \
             longest word, where the line is {line:?}"
        );
        let elided = measure(
            &mut Text::new(said)
                .with_lines(Some(1))
                .with_truncate(true),
            taffy::AvailableSpace::MinContent,
        );
        assert!(
            elided.width < 1.0,
            "a one-line text that elides holds no word, so a row may squeeze it to nothing: \
             {elided:?}"
        );
    }

    phase lines_clamps_a_text_at_that_many_lines;
    {
        let said = "Review the launch announcement, compact navigation, translated labels, and \
                the final release checklist";
        let box_width = 140.0;
        let mut text = Text::new(said).with_lines(Some(2));
        let mut renderer = lay_out_at(&mut text, box_width);
        let style = text.text_style().clone();
        let line = f64::from(style.line_height_px_at(renderer.text_scale()));
        let rect = text.get_rect();
        assert!(
            rect.height() <= 2.0 * line + 0.5,
            "two lines hold it ({line} each), not {}: {rect:?}",
            rect.height()
        );
        assert!(
            rect.width() <= f64::from(box_width) + 0.1,
            "it takes its box ({}) and no more: {rect:?}",
            box_width
        );
        let painted = drawn_glyph_runs(&mut text, &mut renderer);
        assert_eq!(painted.len(), 2, "two lines are painted: {painted:?}");
        assert_eq!(
            painted[1],
            ellipsis_glyph(&mut renderer, &style),
            "the second line ends in an ellipsis"
        );
    }


    phase typography_follows_semantic_roles_and_layers_modifiers;
    {
        {
            let theme = Theme::dark().with_text_scale(1.5);
            let mut text = Text::new("Readable").with_role(TextRole::Reading);
            let mut taffy = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            Widget::build_layout(&mut text, &mut taffy, &theme, &mut nodes);
            assert_eq!(text.text_style(), &theme.text(TextRole::Reading));
        }

        {
            let mut text = Text::new("Readable")
                .with_role(TextRole::Reading)
                .with_auto_hyphenation("en-US");
            let mut taffy = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let default_theme = Theme::default();
            Widget::build_layout(&mut text, &mut taffy, &default_theme, &mut nodes);
            let default_size = text.text_style().size;

            let scaled_theme = Theme::dark().with_text_scale(1.5);
            let mut taffy = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            Widget::build_layout(&mut text, &mut taffy, &scaled_theme, &mut nodes);

            assert!(text.text_style().size > default_size);
            assert_eq!(
                text.text_style().size,
                scaled_theme.text(TextRole::Reading).size
            );
            assert!(matches!(
                &text.text_style().hyphenation,
                tilo_ui_base::text_render::Hyphenation::Auto(language) if language == "en-US"
            ));
        }

        {
            let a = Text::new("Order")
                .with_weight(TextWeight::Bold)
                .with_role(TextRole::Title);
            let b = Text::new("Order")
                .with_role(TextRole::Title)
                .with_weight(TextWeight::Bold);
            assert_eq!(a.text_style(), b.text_style());
            assert_eq!(
                a.text_style().size,
                Theme::dark().text(TextRole::Title).size
            );
        }

        {
            use tilo_ui_base::presence::PresenceCtx;
            let mut tr = tilo_ui_base::font_catalog::fixture_renderer();
            let fam = String::from("Adwaita Sans");
            assert!(
                tr.system_families().iter().any(|family| family == &fam),
                "the controlled fixture ships Adwaita Sans",
            );

            let theme = Theme::dark();
            let presence = PresenceCtx::empty();

            let mut label = Text::new("Reading").with_font_family(FontChoice::Named(fam.clone()));
            label.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 400.0, 40.0));
            let mut scene = Scene::new();
            label.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence.draw_env()));

            let _ = tr.take_shape_stats();
            tr.draw_style(
                &mut scene,
                "Reading",
                theme.tokens.colors.text_primary,
                Affine::IDENTITY,
                Some(401.0),
                &theme
                    .text(TextRole::Body)
                    .with_family(FontChoice::Named(fam)),
            );
            let (hits, _m, _ns) = tr.take_shape_stats();
            assert_eq!(hits, 1, "the label shaped its text in the named family");

            tr.draw_style(
                &mut scene,
                "Reading",
                theme.tokens.colors.text_primary,
                Affine::IDENTITY,
                Some(401.0),
                &theme.text(TextRole::Body),
            );
            let (_h, misses, _ns) = tr.take_shape_stats();
            assert_eq!(
                misses, 1,
                "the role face is a separate shape from the named family"
            );
        }
    }

    phase accessibility_exposes_names_headings_live_text_and_hides_empty_text;
    {
        {
            let t = Text::new("Hello world");
            let node = Widget::accessibility(&t).expect("non-empty text is not presentational");
            assert_eq!(node.role, Role::StaticText);
            assert_eq!(node.name.as_deref(), Some("Hello world"));
            assert_eq!(node.value, None);
            assert!(!node.state.focusable, "static text is not focusable");
        }

        {
            for (role, level) in [
                (TextRole::LargeTitle, Some(1)),
                (TextRole::Title, Some(1)),
                (TextRole::Heading, Some(2)),
                (TextRole::Subheading, Some(3)),
                (TextRole::Display, None),
                (TextRole::Body, None),
            ] {
                let node = Widget::accessibility(&Text::new("Projects").with_role(role))
                    .expect("text is accessible");
                match level {
                    Some(level) => assert_eq!(node.role, Role::Heading { level }, "{role:?}"),
                    None => assert_eq!(node.role, Role::StaticText, "{role:?}"),
                }
            }
        }

        {
            let text = Text::new("Details")
                .with_role(TextRole::Heading)
                .with_heading_level(3);
            let node = Widget::accessibility(&text).expect("heading is accessible");
            assert_eq!(node.role, Role::Heading { level: 3 });
            assert_eq!(text.text_style(), &Theme::dark().text(TextRole::Heading));
        }

        {
            let t = Text::new("");
            assert!(
                Widget::accessibility(&t).is_none(),
                "empty text collapses out of the a11y tree",
            );
        }

        {
            use tilo_ui_base::a11y::Politeness;

            let t = Text::new("Incorrect password").with_live(Politeness::Assertive);
            let node = Widget::accessibility(&t).expect("live status remains accessible text");
            assert_eq!(node.role, Role::StaticText);
            assert_eq!(node.name.as_deref(), Some("Incorrect password"));
            assert_eq!(node.live, Some(Politeness::Assertive));
        }

        {
            // A sheet's question is one a screen reader moves to and reads
            // whole, and the session lists as a control by its words.
            let t = Text::new("Shall I file the budget under Spring?").focusable(true);
            let node = Widget::accessibility(&t).expect("a question is accessible");
            assert_eq!(node.name.as_deref(), Some("Shall I file the budget under Spring?"));
            assert!(node.state.focusable, "the person's attention reaches it");
        }
    }
}

fn lay_out_at(w: &mut dyn Widget, width: f32) -> TextRenderer {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
        std::collections::HashMap::new();
    let mut renderer = TextRenderer::new();
    let node = w.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::MaxContent,
            },
            |known, available, id, _, _| {
                node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut renderer)
                })
            },
        )
        .unwrap();
    w.apply_layout(&taffy, node, Point::ZERO);
    renderer
}

fn drawn_glyph_runs(w: &mut dyn Widget, renderer: &mut TextRenderer) -> Vec<u32> {
    let mut scene = Scene::new();
    w.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        renderer,
        &Theme::dark(),
        tilo_ui_base::presence::PresenceCtx::default().draw_env(),
    ));
    let encoding = scene.encoding();
    encoding
        .resources
        .glyph_runs
        .iter()
        .filter_map(|run| {
            run.glyphs
                .end
                .checked_sub(1)
                .and_then(|last| encoding.resources.glyphs.get(last))
                .map(|glyph| glyph.id)
        })
        .collect()
}

fn ellipsis_glyph(renderer: &mut TextRenderer, style: &TextStyle) -> u32 {
    let mut scene = Scene::new();
    renderer.draw_style(
        &mut scene,
        &tilo_ui_base::text_render::ELLIPSIS.to_string(),
        vello::peniko::Color::WHITE,
        Affine::IDENTITY,
        None,
        style,
    );
    scene.encoding().resources.glyphs[0].id
}

use tilo_ui_base::a11y::Role;

use tilo_ui_base::i18n::I18nManager;

#[test]
fn fluent_text_interpolates_degrades_and_follows_locale() {
    {
        I18nManager::init("en-US");
        I18nManager::add_resource("en-US", "counter = Count is { $count }".to_string());

        let t = Text::new("counter").with_arg("count", 5);
        let node = Widget::accessibility(&t).expect("non-empty text is not presentational");
        // The placeholder must be replaced, not echoed literally.
        assert_eq!(node.name.as_deref(), Some("Count is 5"));
        assert!(!node.name.unwrap().contains("$count"));
    }

    {
        I18nManager::init("en-US");
        I18nManager::add_resource(
            "en-US",
            "greeting = Hi { $name }, you have { $count } messages".to_string(),
        );

        let t = Text::with_args("greeting", &[("name", "Ada".into()), ("count", 3.into())]);
        let node = Widget::accessibility(&t).expect("resolved");
        assert_eq!(node.name.as_deref(), Some("Hi Ada, you have 3 messages"));
    }

    {
        I18nManager::init("en-US");
        I18nManager::add_resource("en-US", "counter = Count is { $count }".to_string());

        // No `count` supplied — must not panic; the formatter renders a
        // best-effort placeholder rather than crashing.
        let t = Text::new("counter");
        let node = Widget::accessibility(&t).expect("resolved");
        let name = node.name.unwrap();
        assert!(name.contains("Count is"), "got {name:?}");
    }

    {
        I18nManager::init("en-US");
        let t = Text::new("totally-unknown-key").with_arg("x", 1);
        let node = Widget::accessibility(&t).expect("resolved");
        assert_eq!(node.name.as_deref(), Some("totally-unknown-key"));
    }

    {
        fn locale_switch_remeasures_and_repaints_retained_text_and_button() {
            fn named<'a>(node: &'a InspectionNode, id: &str) -> Option<&'a InspectionNode> {
                if node.test_id.as_deref() == Some(id) {
                    return Some(node);
                }
                node.children.iter().find_map(|child| named(child, id))
            }

            use tilo_ui::headless::InspectionNode;
            use tilo_ui::widget::{Button, Column};

            const EN: &str = "title = Apps\naction = Open";
            const DE: &str = "title = Anwendungen\naction = Einstellungen öffnen";
            I18nManager::install("en-US", "en-US", &[("en-US", EN), ("de", DE)]).unwrap();
            let root = Column::new()
                .push(Box::new(Text::new("title")))
                .push(Box::new(Button::new("localized-action", "action")));
            let mut driver = tilo_ui::app::driver::Driver::new_with_controlled_inputs(
                tilo_ui::State::new(),
                Box::new(root),
                (700, 300),
                Theme::dark(),
                tilo_ui_base::font_catalog::fixture_renderer(),
                tilo_action_policy::MemberId::person("text-widget-locale-fixture"),
            );

            let before = driver.inspection();
            let before_button = named(&before.root, "localized-action").unwrap();
            let before_width = before_button.rect[2];
            assert_eq!(
                before_button
                    .semantics
                    .as_ref()
                    .and_then(|node| node.name.as_deref()),
                Some("Open")
            );

            I18nManager::set_locale("de-DE");
            driver.relayout();
            let after = driver.inspection();
            let after_button = named(&after.root, "localized-action").unwrap();
            assert_eq!(
                after_button
                    .semantics
                    .as_ref()
                    .and_then(|node| node.name.as_deref()),
                Some("Einstellungen öffnen")
            );
            assert!(after_button.rect[2] > before_width);
            assert!(driver.inspection_json().unwrap().contains("Anwendungen"));
        }
        locale_switch_remeasures_and_repaints_retained_text_and_button();
    }
}
