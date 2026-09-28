use super::*;
use crate::widget::{
    Actions, Button, Card, CardGrid, Column, Element, EmptyState, Scrollable, Text,
};
use automerge::ReadDoc;
use std::cell::Cell;
use std::rc::Rc;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

fn fixture() -> App {
    let cards: Vec<Element> = (0_usize..3)
        .map(|index| {
            Box::new(
                Card::composed(Box::new(Text::new(
                    &"Useful description. ".repeat(index.checked_add(1).unwrap()),
                )))
                .with_title("Connection")
                .with_footer(Box::new(
                    Actions::new(
                        Button::new(
                            &format!("primary-{index}"),
                            "Refresh connections",
                            Some("clicked"),
                        )
                        .primary(),
                    )
                    .secondary(Button::new(
                        &format!("secondary-{index}"),
                        "Details",
                        None,
                    )),
                )),
            ) as Element
        })
        .collect();
    let mut app =
        App::new_with_controlled_inputs(State::new(), Box::new(CardGrid::content_sized(cards)));
    app.init_layout();
    app
}

fn layout(app: &mut App, width: u32) {
    layout_in(app, width, 1600);
}

fn layout_in(app: &mut App, width: u32, height: u32) {
    app.layout.layout_dirty = true;
    run_layout_pass(
        app,
        app.layout.root_node.unwrap(),
        width,
        height,
        1.0,
        false,
        None,
    )
    .unwrap();
}

fn controls(app: &App) -> Vec<(String, KurboRect)> {
    app.focusable_order
        .iter()
        .map(|&node| {
            app.inspect_node(node, |widget| {
                (widget.test_id().unwrap().to_string(), widget.get_rect())
            })
            .unwrap()
        })
        .collect()
}

#[test]
fn card_hosts_are_coherent_before_paint_and_activate_through_the_real_route() {
    // Case `content_card_host_geometry_focus_click_and_resize_are_coherent_before_paint`.
    {
        let mut app = fixture();
        layout(&mut app, 1000);
        let initial = controls(&app);
        assert_eq!(
            initial
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            [
                "secondary-0",
                "primary-0",
                "secondary-1",
                "primary-1",
                "secondary-2",
                "primary-2"
            ]
        );
        let target = initial[3].1.center();
        assert!(app.dispatch_semantic_click(target.x, target.y));
        assert_eq!(app.focused_node, Some(app.focusable_order[3]));
        assert!(
            app.state
                .doc
                .get(automerge::ROOT, "clicked")
                .unwrap()
                .is_some()
        );
        let mut scene = Scene::new();
        app.root_widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut app.text_renderer,
            &app.theme,
            &PresenceCtx::default(),
        );
        assert_eq!(
            controls(&app),
            initial,
            "painting must not establish geometry"
        );
        for width in [240, 535, 536, 820, 1000, 240] {
            layout(&mut app, width);
            let mut fresh = fixture();
            layout(&mut fresh, width);
            assert_eq!(
                controls(&app),
                controls(&fresh),
                "fresh vs resized at {width}"
            );
            assert_eq!(
                app.inspect_node(app.focused_node.unwrap(), |widget| widget
                    .test_id()
                    .unwrap()
                    .to_string())
                    .unwrap(),
                "primary-1"
            );
            for (_, rect) in controls(&app) {
                assert!(rect.x0 >= -1.0 && rect.x1 <= f64::from(width) + 1.0);
                assert!(rect.height() >= 40.0);
            }
        }
    }

    // Case `short_card_stack_scroll_and_tab_activate_final_action_once_from_enter`.
    {
        let invocations = Rc::new(Cell::new(0));
        let activated = invocations.clone();
        let mut content = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0);
        for index in 0..8 {
            content = content.push(Box::new(Card::composed(Box::new(Text::new(&format!(
                "Application {index}: {}",
                "A useful multi-line description that remains inside its card. ".repeat(3)
            ))))));
        }
        content = content.push(Box::new(
            Button::new("final-action", "Open all applications", None).on_click(move || {
                activated.set(activated.get() + 1);
            }),
        ));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(Scrollable::new_vertical().with_child(Box::new(content))),
        );
        app.init_layout();
        layout_in(&mut app, 480, 360);

        let final_node = *app
            .focusable_order
            .last()
            .expect("final action is focusable");
        let before = app
            .inspect_node(final_node, |widget| widget.get_rect())
            .unwrap();
        assert!(before.y0 >= 360.0, "final action starts below the viewport");

        let wheel = WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -10_000.0)),
            phase: TouchPhase::Moved,
        };
        assert!(send_widget_then_host_fallback(&mut app, &wheel));
        layout_in(&mut app, 480, 360);
        let after = app
            .inspect_node(final_node, |widget| widget.get_rect())
            .unwrap();
        assert!(
            after.y0 >= 0.0 && after.y1 <= 360.0,
            "final action is reachable after scrolling: {after:?}"
        );

        for _ in 0..app.focusable_order.len() {
            assert!(app.focus_tab(false));
        }
        assert_eq!(app.focused_node, Some(final_node));

        assert!(send_widget_then_host_fallback(
            &mut app,
            &named_key_event(NamedKey::Enter, ElementState::Pressed)
        ));
        assert_eq!(invocations.get(), 1, "Enter adds exactly one invocation");
        send_widget_then_host_fallback(
            &mut app,
            &named_key_event(NamedKey::Enter, ElementState::Released),
        );
        assert_eq!(
            invocations.get(),
            1,
            "the key release must not invoke the action again"
        );
        let mut already_handled = true;
        app.handle_unconsumed_activation_key(
            &named_key_event(NamedKey::Enter, ElementState::Pressed),
            &mut already_handled,
        );
        assert_eq!(
            invocations.get(),
            1,
            "a widget-consumed Enter press must not invoke the host fallback"
        );
    }

    // Case `scrollable_empty_state_action_uses_the_real_tab_and_enter_host_route`.
    {
        let root = EmptyState::new("No messages are available")
            .with_icon(crate::icons::Icon::Inbox)
            .with_message("Reconnect to load messages.")
            .with_action("Review connection settings")
            .with_action_path("empty.review")
            .into_scrollable()
            .with_height(50.0);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        layout_in(&mut app, 240, 50);

        let action_node = *app
            .focusable_order
            .iter()
            .find(|&&node| {
                app.inspect_node(node, |widget| widget.name() == "EmptyState")
                    .unwrap_or(false)
            })
            .expect("empty-state action is in host focus order");
        let wheel = WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -10_000.0)),
            phase: TouchPhase::Moved,
        };
        assert!(send_widget_then_host_fallback(&mut app, &wheel));
        layout_in(&mut app, 240, 50);

        for _ in 0..app.focusable_order.len() {
            if app.focused_node == Some(action_node) {
                break;
            }
            assert!(app.focus_tab(false));
        }
        assert_eq!(app.focused_node, Some(action_node));
        assert!(send_widget_then_host_fallback(
            &mut app,
            &named_key_event(NamedKey::Enter, ElementState::Pressed),
        ));
        assert!(matches!(
            app.state.doc.get(automerge::ROOT, "empty.review"),
            Ok(Some((automerge::Value::Scalar(value), _)))
                if value.as_ref().as_bool() == Some(true)
        ));
    }
}

fn named_key_event(key: NamedKey, state: ElementState) -> WindowEvent {
    let logical_key = Key::Named(key);
    let event = KeyEvent::synthetic(
        winit::event::RawKeyEvent {
            physical_key: PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
            state,
        },
        logical_key.clone(),
        None,
        KeyLocation::Standard,
        false,
        None,
        logical_key,
    );
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

fn send_widget_then_host_fallback(app: &mut App, event: &WindowEvent) -> bool {
    let mut handled = false;
    let mut focus_target = None;
    app.root_widget.handle_event(
        event,
        &mut crate::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &app.input.modifiers,
            state: &mut app.state.doc,
            path: &app.state.root,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: app.layout.root_node.unwrap(),
            taffy: &app.layout.taffy,
        },
    );
    if let Some(target) = focus_target {
        app.set_focused_node(Some(target));
    }
    app.handle_unconsumed_activation_key(event, &mut handled);
    handled
}
