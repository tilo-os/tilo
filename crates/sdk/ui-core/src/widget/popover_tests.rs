//! The option menu's keyboard, pointer and semantics; placement and flip for
//! both bodies; and the Panel look: lighter than the window on dark, a lift
//! for the highlight, painted only in the top layer.

use super::*;
use automerge::AutoCommit;
use vello_encoding::DrawTag;

const WINDOW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

fn opened() -> Popover {
    let mut p = Popover::new().with_label("Refresh interval").with_items([
        "1 second",
        "2 seconds",
        "5 seconds",
        "10 seconds",
        "30 seconds",
    ]);
    p.set_viewport(WINDOW);
    p.open_at(Rect::new(100.0, 40.0, 180.0, 68.0), Some(1));
    let mut tr = TextRenderer::new();
    p.place(&mut tr, &Theme::dark());
    p
}

fn cursor_at(pt: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(pt.x, pt.y),
    }
}

use crate::synthetic_input::mouse_press;

fn key_event(logical: Key) -> WindowEvent {
    crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
        logical,
        None,
        ElementState::Pressed,
    ))
}

fn key(named: NamedKey) -> WindowEvent {
    key_event(Key::Named(named))
}

fn dispatch<C: Widget + 'static>(p: &mut Popover<C>, event: &WindowEvent) -> bool {
    let mut taffy = TaffyTree::<()>::new();
    let node = taffy.leaf_node(Style::default());
    let mut handled = false;
    let mut focus = None;
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    p.handle_event(
        event,
        &mut crate::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &winit::event::Modifiers::default(),
            state: crate::widget::Edits::lend(&mut doc),
            path: &root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
    handled
}

/// Paint the top layer over `WINDOW`, as hosts do.
fn present<C: Widget + 'static>(p: &mut Popover<C>, theme: &Theme) -> Scene {
    let presence = PresenceCtx::default();
    let previous = presence.push_viewport(WINDOW);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    p.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    p.draw_overlay(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    presence.pop_viewport(previous);
    scene
}

/// Fixed-size content.
struct Fixed {
    rect: Rect,
    w: f32,
    h: f32,
}

impl Widget for Fixed {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::length(self.w),
                height: Dimension::length(self.h),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, (f64::from(self.w), f64::from(self.h)));
    }
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "Fixed"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[test]
fn menu_keyboard_moves_clamps_selects_and_swallows_keys() {
    // Case `arrow_keys_move_highlight_and_are_consumed`.
    {
        let mut p = opened();
        assert_eq!(p.highlighted(), 1, "opens on the selected row");
        assert!(dispatch(&mut p, &key(NamedKey::ArrowDown)));
        assert_eq!(p.highlighted(), 2);
        assert!(dispatch(&mut p, &key(NamedKey::ArrowUp)));
        assert_eq!(p.highlighted(), 1);
    }

    // Case `home_end_and_clamping`.
    {
        let mut p = opened();
        dispatch(&mut p, &key(NamedKey::End));
        assert_eq!(p.highlighted(), 4);
        dispatch(&mut p, &key(NamedKey::Home));
        assert_eq!(p.highlighted(), 0);
        for _ in 0..10 {
            dispatch(&mut p, &key(NamedKey::ArrowDown));
        }
        assert_eq!(p.highlighted(), 4, "no wrap past the end");
        for _ in 0..10 {
            dispatch(&mut p, &key(NamedKey::ArrowUp));
        }
        assert_eq!(p.highlighted(), 0, "no wrap past the start");
    }

    // Case `enter_selects_escape_does_not`.
    {
        let mut p = opened();
        dispatch(&mut p, &key(NamedKey::ArrowDown));
        dispatch(&mut p, &key(NamedKey::ArrowDown));
        assert!(dispatch(&mut p, &key(NamedKey::Enter)));
        assert!(!p.is_open());
        assert_eq!(p.take_selection(), Some(3));
        assert_eq!(p.take_selection(), None, "drained once");
        assert_eq!(p.selected(), Some(3));

        let mut p = opened();
        assert!(dispatch(&mut p, &key(NamedKey::Escape)));
        assert!(!p.is_open());
        assert_eq!(p.take_selection(), None);
    }

    // Case `unrelated_keys_are_swallowed_not_leaked`.
    {
        let mut p = opened();
        assert!(dispatch(&mut p, &key_event(Key::Character("x".into()))));
        assert!(dispatch(&mut p, &key(NamedKey::Tab)));
        assert!(p.is_open());
        assert_eq!(p.take_selection(), None);
    }
}

#[test]
fn menu_pointer_selection_and_dismissal() {
    let mut p = opened();
    let row2 = p.item_rect(2).expect("published row geometry");
    assert_eq!(p.item_at(row2.center()), Some(2));
    assert!(p.item_rect(p.items().len()).is_none());
    assert!(
        row2.height() >= f64::from(Theme::dark().tokens.metrics.min_pointer_target),
        "a row is a pointer target"
    );
    dispatch(&mut p, &cursor_at(row2.center()));
    assert_eq!(p.highlighted(), 2, "the pointer moves the highlight");
    assert!(dispatch(&mut p, &mouse_press()));
    assert_eq!(p.take_selection(), Some(2));
    assert!(!p.is_open());

    let dismissed = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = dismissed.clone();
    let mut p = opened().on_dismiss(move || flag.set(true));
    let outside = Point::new(p.rect().x1 + 80.0, p.rect().y1 + 80.0);
    dispatch(&mut p, &cursor_at(outside));
    assert!(
        dispatch(&mut p, &mouse_press()),
        "the outside click is swallowed"
    );
    assert!(!p.is_open());
    assert!(dismissed.get());
    assert_eq!(p.take_selection(), None);
}

#[test]
fn menu_semantics_focus_and_reopen() {
    use crate::a11y::Role;
    let p = opened();
    let node = Widget::accessibility(&p).expect("a menu is semantic");
    assert_eq!(node.role, Role::Menu);
    assert_eq!(node.name.as_deref(), Some("Refresh interval"));
    assert_eq!(node.state.expanded, Some(true));
    assert_eq!(node.children.len(), 5);
    for (i, child) in node.children.iter().enumerate() {
        assert_eq!(child.role, Role::MenuItem);
        assert_eq!(child.pos_in_set, Some(i + 1));
        assert_eq!(child.set_size, Some(5));
    }
    assert!(node.children[1].state.focused && node.children[1].state.selected);
    assert!(Widget::traps_focus(&p) && Widget::modal_capture(&p).is_some());

    let closed = Popover::new().with_items(["a", "b"]);
    let node = Widget::accessibility(&closed).unwrap();
    assert!(node.state.hidden);
    assert_eq!(node.state.expanded, Some(false));
    assert!(!Widget::traps_focus(&closed) && Widget::modal_capture(&closed).is_none());

    let mut p = opened();
    p.select(3);
    assert_eq!(p.take_selection(), Some(3));
    p.open_at(Rect::new(100.0, 40.0, 180.0, 68.0), p.selected());
    assert_eq!(p.highlighted(), 3, "reopening lands on the applied option");
}

#[test]
fn text_content_is_laid_out_on_one_line() {
    let theme = Theme::light();
    let mut tr = TextRenderer::new();
    let label = "Shared with Ana, Ben and Chloé";
    let mut p = Popover::with_content(crate::widget::Text::new(label))
        .anchored(Rect::new(640.0, 150.0, 760.0, 182.0))
        .with_viewport(WINDOW)
        .open(true);
    p.place(&mut tr, &theme);
    let (w, h) = tr.measure_style(label, None, &theme.text(crate::tokens::TextRole::Body));
    let content = p.content().get_rect();
    assert!(
        content.width() >= f64::from(w) - 0.5 && content.height() < f64::from(h) * 1.5,
        "one line: content {content:?}, text {w}x{h}, panel {:?}",
        p.rect()
    );
}

#[test]
fn placement_flips_and_stays_in_the_window() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();

    // Case `below_and_leading_aligned_with_the_anchor`.
    {
        let anchor = Rect::new(200.0, 100.0, 280.0, 132.0);
        let mut p = Popover::with_content(Fixed {
            rect: Rect::ZERO,
            w: 120.0,
            h: 60.0,
        })
        .anchored(anchor)
        .with_viewport(WINDOW)
        .open(true);
        p.place(&mut tr, &theme);
        assert_eq!(p.resolved_side(), PopoverSide::Below);
        assert!(p.rect().y0 > anchor.y1 && (p.rect().x0 - anchor.x0).abs() < 1e-9);
        let pad = f64::from(theme.tokens.spacing.s4);
        assert!(
            (p.rect().width() - (121.0 + pad * 2.0)).abs() < 0.5,
            "hugs content (plus the pixel of rounding slack)"
        );
        let content = p.content().get_rect();
        assert!(
            (content.x0 - (p.rect().x0 + pad)).abs() < 0.5,
            "content sits inside the padding"
        );
    }

    // Case `flips_above_when_there_is_no_room_below`.
    {
        let anchor = Rect::new(200.0, 540.0, 280.0, 572.0);
        let mut p = Popover::with_content(Fixed {
            rect: Rect::ZERO,
            w: 120.0,
            h: 120.0,
        })
        .anchored(anchor)
        .with_viewport(WINDOW)
        .open(true);
        p.place(&mut tr, &theme);
        assert_eq!(p.resolved_side(), PopoverSide::Above);
        assert!(p.rect().y1 < anchor.y0);
    }

    // Case `slides_inside_the_window_on_the_cross_axis`.
    {
        let anchor = Rect::new(760.0, 100.0, 790.0, 130.0);
        let mut p = opened();
        p.open_at(anchor, None);
        p.place(&mut tr, &theme);
        assert!(p.rect().x1 <= WINDOW.x1 + 1e-9, "{:?}", p.rect());
    }

    // Case `the_top_layer_uses_the_window_when_no_viewport_is_set`.
    {
        let anchor = Rect::new(200.0, 560.0, 280.0, 590.0);
        let mut p = Popover::new()
            .with_items(["One", "Two", "Three"])
            .with_label("Count");
        p.open_at(anchor, None);
        present(&mut p, &theme);
        assert_eq!(p.resolved_side(), PopoverSide::Above);
    }
}

#[test]
fn the_panel_is_painted_in_the_top_layer_lighter_than_the_window() {
    // Case `draw_paints_nothing_draw_overlay_paints_the_panel`.
    {
        let mut p = opened();
        let mut scene = Scene::new();
        p.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert!(scene.encoding().draw_tags.is_empty());
        assert!(
            !present(&mut p, &Theme::dark())
                .encoding()
                .draw_tags
                .is_empty()
        );
        let mut closed = Popover::new().with_items(["a"]);
        let scene = present(&mut closed, &Theme::dark());
        assert!(
            scene
                .encoding()
                .draw_tags
                .iter()
                .all(|t| *t != DrawTag::COLOR)
        );
    }

    // Case `popover_is_lighter_than_the_window_on_dark`.
    {
        use crate::a11y::relative_luminance;
        for theme in [
            Theme::dark(),
            Theme::named("dark-soft").unwrap(),
            Theme::named("dark-cool").unwrap(),
        ] {
            let panel = crate::chrome::panel_ground(&theme);
            let content = theme.tokens.colors.surfaces.content;
            let base = theme.tokens.colors.surfaces.base;
            assert!(
                relative_luminance(panel) > relative_luminance(content)
                    && relative_luminance(panel) > relative_luminance(base),
                "{}: a menu is lighter than the window and the desktop on dark",
                theme.name
            );
        }
    }

    // Case `the_armed_row_is_inverted_as_in_every_menu`.
    {
        let theme = Theme::dark();
        let inverted = theme.tokens.colors.intent.inverted;
        let oklch = crate::color::Oklch::from_color(inverted);
        assert!(oklch.c <= 0.011, "no hue in the highlight: {oklch:?}");
        let mut p = opened();
        let scene = present(&mut p, &theme);
        let inverted_rgba = inverted.premultiply().to_rgba8().to_u32();
        assert!(
            scene.encoding().draw_data.contains(&inverted_rgba),
            "the armed row is the inverted control (HIG §9.3), as in the \
             dropdown list and the context menu"
        );
    }
}

#[test]
fn a_trailing_popover_hangs_from_its_anchors_trailing_edge() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let anchor = Rect::new(600.0, 10.0, 640.0, 42.0);
    let mut p = Popover::with_content(Fixed {
        rect: Rect::ZERO,
        w: 200.0,
        h: 80.0,
    })
    .anchored(anchor)
    .aligned(PanelAlign::Trailing)
    .with_viewport(WINDOW)
    .open(true);
    p.place(&mut tr, &theme);
    assert!((p.rect().x1 - anchor.x1).abs() < 1e-9, "{:?}", p.rect());
    assert!(p.rect().y0 > anchor.y1);
    // At the leading edge of the window it flips to the leading edge.
    let mut p = Popover::with_content(Fixed {
        rect: Rect::ZERO,
        w: 200.0,
        h: 80.0,
    })
    .anchored(Rect::new(10.0, 10.0, 50.0, 42.0))
    .aligned(PanelAlign::Trailing)
    .with_viewport(WINDOW)
    .open(true);
    p.place(&mut tr, &theme);
    assert!(p.rect().x0 >= WINDOW.x0, "{:?}", p.rect());
}

#[test]
fn a_prompt_popover_submits_its_one_field_with_enter() {
    let mut p = Popover::prompt("Go to offset", "0x0000", "Hex or decimal, up to 0x3fff")
        .with_field_chars(12)
        .with_viewport(WINDOW);
    p.open_prompt(Rect::new(100.0, 40.0, 180.0, 68.0), "0x10");
    let mut tr = TextRenderer::new();
    p.place(&mut tr, &Theme::dark());
    assert!(p.is_open());
    assert!(p.content().is_focused(), "the field is focused on open");
    assert_eq!(p.content().value(), "0x10");
    let mut typed = key_event(Key::Character("2".into()));
    if let WindowEvent::KeyboardInput { event, .. } = &mut typed {
        event.text = Some("2".into());
    }
    dispatch(&mut p, &typed);
    assert_eq!(
        p.content().value(),
        "2",
        "the selected initial value is replaced"
    );
    assert_eq!(p.take_submitted(), None);
    dispatch(&mut p, &key(NamedKey::Enter));
    assert_eq!(p.take_submitted(), Some("2".to_string()));
    assert!(!p.is_open(), "submitting closes it");
    let node = Widget::accessibility(p.content()).expect("the field is announced");
    assert_eq!(node.name.as_deref(), Some("Go to offset"));
}

#[test]
fn a_hosted_popover_lays_its_content_out_in_the_window_tree() {
    use crate::headless::Framed;
    use crate::widget::{Column, TextInput};
    let content = Column::new()
        .push(Box::new(TextInput::local().with_placeholder("Label")))
        .push(Box::new(TextInput::local().with_placeholder("Colour")));
    let popover = Popover::with_content(content)
        .anchored(Rect::new(100.0, 40.0, 180.0, 68.0))
        .open(true);
    let mut framed = Framed::new(crate::State::new(), popover, (800, 600), Theme::dark());
    let _ = Framed::harness_mut(&mut framed).poll_frame();
    let second = framed.content().children()[1].get_rect();
    assert!(
        framed.rect().contains(second.center()),
        "the content stands inside the panel: {second:?} in {:?}",
        framed.rect()
    );
    let c = second.center();
    Framed::harness_mut(&mut framed).click_at(c.x, c.y);
    let focused: Vec<bool> = framed
        .content()
        .children()
        .iter()
        .map(|w| w.is_focused())
        .collect();
    assert_eq!(
        focused,
        [false, true],
        "a click focuses the field it lands on"
    );
}

#[test]
fn a_popover_button_opens_its_popover_at_its_own_rect() {
    use crate::headless::Framed;
    let button = crate::widget::Button::new("filters", "Filters").with_popover(Fixed {
        rect: Rect::ZERO,
        w: 160.0,
        h: 60.0,
    });
    let mut framed = Framed::new(crate::State::new(), button, (800, 600), Theme::dark());
    let _ = Framed::harness_mut(&mut framed).poll_frame();
    let rect = framed.get_rect();
    assert!(!framed.is_open());
    Framed::harness_mut(&mut framed).click_at(rect.center().x, rect.center().y);
    let _ = Framed::harness_mut(&mut framed).poll_frame();
    assert!(framed.is_open(), "a click opens it");
    let panel = framed.popover().rect();
    assert!(
        panel.y0 >= rect.y1,
        "under the button: {panel:?} vs {rect:?}"
    );
    // Leading-aligned with it, but never closer to the window's edge than
    // the panel margin.
    let margin = crate::widget::floating_surface::viewport_margin(&Theme::dark());
    assert!(
        (panel.x0 - rect.x0.max(margin)).abs() < 1.0,
        "leading-aligned with it: {panel:?} vs {rect:?}"
    );
}
