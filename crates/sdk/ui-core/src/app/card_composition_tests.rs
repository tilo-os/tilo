use super::*;
use crate::widget::{Actions, Button, Card, CardGrid, Element, Text};
use automerge::ReadDoc;

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
    app.layout.layout_dirty = true;
    run_layout_pass(
        app,
        app.layout.root_node.unwrap(),
        width,
        1600,
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
fn content_card_host_geometry_focus_click_and_resize_are_coherent_before_paint() {
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
