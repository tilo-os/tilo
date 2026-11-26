//! A document editor embedded in another widget: hit, reflowed and laid out where that widget
//! draws it.

use automerge::AutoCommit;
use tilo_rich_edit::{BlockContent, BlockEditor};
use tilo_ui::widget::Embedded;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

fn click(
    embedded: &mut Embedded<BlockEditor>,
    doc: &mut tilo_ui_base::widget::Edits<'_>,
    at: Point,
) {
    let device_id = winit::event::DeviceId::dummy();
    for event in [
        WindowEvent::CursorMoved {
            device_id,
            position: PhysicalPosition::new(at.x, at.y),
        },
        WindowEvent::MouseInput {
            device_id,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::MouseInput {
            device_id,
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    ] {
        let mut handled = false;
        embedded.handle_event(
            &event,
            1.0,
            &Modifiers::default(),
            doc,
            &automerge::ROOT,
            &mut handled,
        );
    }
}

tilo_test_env::scenario! {
    /// A document editor embedded in another widget.
    /// Each phase was a test of its own and builds its own fixture.
    fn an_embedded_editor_is_hit_reflowed_and_laid_out_where_it_is_drawn;

    phase a_child_under_a_transform_is_hit_and_reported_where_it_is_drawn;
    {
        let theme = Theme::dark();
        let mut text = tilo_ui_base::font_catalog::fixture_renderer();
        let mut doc = AutoCommit::new();
        let mut embedded = Embedded::new(BlockEditor::new().with_wrap_lines(true));
        embedded.widget_mut()
            .present_blocks(&[BlockContent::paragraph("a", "a line of text")]);
        // The box is laid out at (100, 100)..(500, ..) in its own space and
        // drawn at half size, 10 px in.
        embedded.set_transform(Affine::translate((10.0, 10.0)) * Affine::scale(0.5));
        let height = embedded.layout(Rect::new(100.0, 100.0, 500.0, 100.0), &theme, &mut text);
        assert!(height > 0.0);
        let drawn = embedded.rect();
        assert!((drawn.x0 - 60.0).abs() < 1e-9 && (drawn.width() - 200.0).abs() < 1e-9);
        click(
            &mut embedded,
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            Point::new(drawn.x0 + 20.0, drawn.y0 + 4.0),
        );
        assert!(embedded.widget().is_focused(), "the click reached the box");
        assert!(
            embedded.take_focus_request(),
            "the owner is asked to take focus"
        );
        assert!(!embedded.take_focus_request(), "once");
        let caret = embedded.ime_cursor_rect().expect("a focused line has a caret");
        assert!(
            drawn.inflate(1.0, 1.0).contains(caret.center()),
            "{caret:?} within {drawn:?}"
        );
    }

    phase a_menu_opened_on_small_words_near_an_edge_stays_inside_the_owner;
    {
        let theme = Theme::dark();
        let mut text = tilo_ui_base::font_catalog::fixture_renderer();
        let mut embedded = Embedded::new(BlockEditor::new().with_wrap_lines(true));
        embedded.widget_mut()
            .present_blocks(&[BlockContent::paragraph("a", "words at the edge")]);
        embedded.set_transform(Affine::translate((600.0, 570.0)) * Affine::scale(0.5));
        embedded.layout(Rect::new(0.0, 0.0, 400.0, 0.0), &theme, &mut text);
        let owner = Rect::new(0.0, 0.0, 800.0, 600.0);
        let presence = PresenceCtx::empty();
        presence.viewport.set(Some(owner));
        embedded.draw(&mut tilo_ui_base::DrawCtx::new(&mut Scene::new(), Affine::IDENTITY, &mut text, &theme, presence.draw_env()));
        assert_eq!(
            presence.viewport.get(),
            Some(owner),
            "the owner's viewport is put back"
        );
        assert!(embedded.widget_mut().open_text_menu(Point::new(380.0, 10.0)));
        embedded.draw_overlay(&mut tilo_ui_base::DrawCtx::new(&mut Scene::new(), Affine::IDENTITY, &mut text, &theme, presence.draw_env()));
        let menu = embedded.modal_capture().expect("the menu is open");
        assert!(
            menu.x0 >= owner.x0 - 1e-6
                && menu.x1 <= owner.x1 + 1e-6
                && menu.y0 >= owner.y0 - 1e-6
                && menu.y1 <= owner.y1 + 1e-6,
            "{menu:?} inside {owner:?}"
        );
        assert!(
            menu.width() > 100.0,
            "drawn at the owner's scale, not the words': {menu:?}"
        );
    }

    phase retained_unfocused_editor_reflows_when_the_host_text_scale_changes;
    {
        let normal = Theme::dark();
        let large = Theme::high_contrast().with_text_scale(2.0);
        let fixture = [BlockContent::paragraph("private", "Unsent Studio context")];
        for width in [220.0, 180.0] {
            let mut text = tilo_ui_base::font_catalog::fixture_renderer();
            let mut embedded = Embedded::new(BlockEditor::new().with_wrap_lines(true));
            embedded.widget_mut().present_blocks(&fixture);
            embedded.widget_mut()
                .focus_block(0, tilo_rich_edit::CursorPlacement::End);
            embedded.widget_mut().select_before_caret(8);
            embedded.widget_mut().set_focus(false);
            let before = embedded
                .widget()
                .blocks()
                .first()
                .unwrap()
                .line
                .selection_range();
            for _ in 0..3 {
                embedded.layout(Rect::new(0.0, 0.0, 220.0, 0.0), &normal, &mut text);
                embedded.draw(&mut tilo_ui_base::DrawCtx::new(&mut Scene::new(), Affine::IDENTITY, &mut text, &normal, PresenceCtx::empty().draw_env()));
            }

            text.set_text_scale(large.text_scale);
            let rect = Rect::new(0.0, 0.0, width, 0.0);
            let retained_height = embedded.layout(rect, &large, &mut text);
            let mut fresh = Embedded::new(BlockEditor::new().with_wrap_lines(true));
            fresh.widget_mut().present_blocks(&fixture);
            let fresh_height = fresh.layout(rect, &large, &mut text);
            assert!(fresh_height > 40.0, "the large-text fixture must wrap");
            assert!(
                (retained_height - fresh_height).abs() < 0.01,
                "retained editor measured {retained_height}, fresh editor {fresh_height} at width {width}"
            );
            let block = embedded.widget().blocks().first().unwrap();
            assert_eq!(block.line.value(), "Unsent Studio context");
            assert_eq!(block.line.selection_range(), before);
            assert!(!embedded.widget().is_focused());
        }
    }
}
