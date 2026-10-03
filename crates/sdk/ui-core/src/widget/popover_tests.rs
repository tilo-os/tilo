//! The option menu's keyboard, pointer and semantics; placement and flip for
//! both bodies; and the Panel look: lighter than the window on dark, a lift
//! for the highlight, painted only in the top layer.

use super::*;
use automerge::AutoCommit;
use vello_encoding::DrawTag;
use winit::event::{DeviceId, KeyEvent};
use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};

const WINDOW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

fn synth_device() -> DeviceId {
    unsafe { std::mem::zeroed() }
}

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
        device_id: synth_device(),
        position: winit::dpi::PhysicalPosition::new(pt.x, pt.y),
    }
}

fn left_press() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: synth_device(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

fn key_event(logical: Key) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = logical;
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: synth_device(),
        event: ev,
        is_synthetic: false,
    }
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
            state: &mut doc,
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
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(self.w),
                height: Dimension::length(self.h),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
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
    assert!(dispatch(&mut p, &left_press()));
    assert_eq!(p.take_selection(), Some(2));
    assert!(!p.is_open());

    let dismissed = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = dismissed.clone();
    let mut p = opened().on_dismiss(move || flag.set(true));
    let outside = Point::new(p.rect().x1 + 80.0, p.rect().y1 + 80.0);
    dispatch(&mut p, &cursor_at(outside));
    assert!(
        dispatch(&mut p, &left_press()),
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
            (p.rect().width() - (120.0 + pad * 2.0)).abs() < 0.5,
            "hugs content"
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

    // Case `the_highlight_is_a_lift_not_an_accent`.
    {
        let theme = Theme::dark();
        let on_panel = theme.on_ground(crate::chrome::panel_ground(&theme));
        let lift = on_panel.tokens.colors.intent.selection;
        let oklch = crate::color::Oklch::from_color(lift);
        assert!(oklch.c <= 0.011, "no hue in the highlight: {oklch:?}");
        let mut p = opened();
        let scene = present(&mut p, &theme);
        let lift_rgba = lift.premultiply().to_rgba8().to_u32();
        assert!(
            scene.encoding().draw_data.contains(&lift_rgba),
            "the highlighted row is filled with the panel's lift"
        );
    }
}
