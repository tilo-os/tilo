use super::*;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{Button, Card, CardGrid, Column, Container, Element};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn fixture(count: usize) -> (App, Rc<RefCell<Vec<usize>>>) {
    let clicks = Rc::new(RefCell::new(Vec::new()));
    let cards: Vec<Element> = (0..count)
        .map(|i| {
            let clicks = clicks.clone();
            Box::new(Card::new(Box::new(
                Button::new(&format!("card-{i}"), &format!("Card {i}"), None)
                    .on_click(move || clicks.borrow_mut().push(i)),
            ))) as Element
        })
        .collect();
    let root = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(20.0)
        .push(Box::new(
            CardGrid::new(cards)
                .with_min_card_width(260.0)
                .with_card_height(180.0)
                .with_gap(16.0),
        ))
        .push(Box::new(
            Container::new().with_no_padding().with_height(10.0),
        ));
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.init_layout();
    (app, clicks)
}

fn layout(app: &mut App, width: u32) {
    app.layout.layout_dirty = true;
    run_layout_pass(
        app,
        app.layout.root_node.unwrap(),
        width,
        2000,
        1.0,
        false,
        None,
    )
    .unwrap();
}

fn button_rects(app: &App) -> Vec<KurboRect> {
    app.root_widget.children()[0]
        .children()
        .iter()
        .map(|card| card.children()[0].children()[0].get_rect())
        .collect()
}

#[test]
fn card_grid_has_geometry_focus_and_input_before_paint() {
    // Case `card_grid_buttons_have_host_focus_and_geometry_before_paint`.
    {
        let (mut app, clicks) = fixture(6);
        layout(&mut app, 900);
        assert_eq!(
            app.focusable_order.len(),
            6,
            "all card buttons enter host focus traversal"
        );
        let before = button_rects(&app);
        for (index, &node) in app.focusable_order.iter().enumerate() {
            assert_eq!(
                app.inspect_node(node, |widget| widget.name().to_owned())
                    .unwrap(),
                "Button"
            );
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_rect()).unwrap(),
                before[index]
            );
            assert!(before[index].width() > 0.0 && before[index].height() > 0.0);
            assert_eq!(
                app.inspect_node(node, |widget| widget.accessibility().unwrap().semantic_key)
                    .unwrap(),
                Some(format!("card-{index}"))
            );
        }
        let target = before[4].center();
        assert!(app.dispatch_semantic_click(target.x, target.y));
        assert_eq!(*clicks.borrow(), vec![4]);
        assert_eq!(app.focused_node, Some(app.focusable_order[4]));
        let order = app.focusable_order.clone();
        let mut scene = Scene::new();
        app.root_widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut app.text_renderer,
            &app.theme,
            &PresenceCtx::default(),
        );
        assert_eq!(
            button_rects(&app),
            before,
            "paint must not establish or change geometry"
        );
        assert_eq!(app.focusable_order, order);
    }

    // Case `card_grid_headless_uses_same_narrow_geometry_and_click_target`.
    {
        let (mut app, clicks) = fixture(2);
        let root = std::mem::replace(&mut app.root_widget, Box::new(Column::new()));
        let mut driver = crate::headless::Driver::new_with_controlled_inputs(
            State::new(),
            root,
            (200, 600),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("grid-test".into()),
        );
        let snapshot = driver.inspection();
        let grid = &snapshot.root.children[0];
        assert_eq!(grid.rect, [0.0, 0.0, 200.0, 376.0]);
        assert_eq!(grid.children[1].rect, [0.0, 196.0, 200.0, 180.0]);
        driver.click_named("card-1").unwrap();
        assert_eq!(*clicks.borrow(), vec![1]);
    }

    // Case `card_grid_under_scrollable_clips_input_before_paint`.
    {
        let (mut app, clicks) = fixture(6);
        let child = std::mem::replace(&mut app.root_widget, Box::new(Column::new()));
        app.root_widget = Box::new(crate::widget::Scrollable::new_vertical().with_child(child));
        app.init_layout();
        app.layout.layout_dirty = true;
        let root_node = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root_node, 200, 190, 1.0, true, None).unwrap();
        let root = app.root_widget.children()[0];
        assert_eq!(root.children()[0].get_rect().height(), 1160.0);
        let hidden_button = root.children()[0].children()[1].children()[0].children()[0].get_rect();
        assert!(hidden_button.y0 >= 190.0);
        let target = hidden_button.center();
        app.dispatch_semantic_click(target.x, target.y);
        assert!(
            clicks.borrow().is_empty(),
            "off-viewport card must not receive pointer activation"
        );
        let visible_button =
            app.root_widget.children()[0].children()[0].children()[0].children()[0].children()[0]
                .get_rect()
                .center();
        app.dispatch_semantic_click(visible_button.x, visible_button.y);
        assert_eq!(*clicks.borrow(), vec![0]);
    }

    // Case `card_grid_rebuilt_card_body_has_prepaint_geometry_and_input`.
    {
        let requested = Rc::new(Cell::new(1));
        let clicks = Rc::new(RefCell::new(Vec::new()));
        let body = GrowingBody {
            column: Column::new(),
            requested: requested.clone(),
            count: 0,
            dirty: false,
            clicks: clicks.clone(),
        };
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(
                CardGrid::new(vec![Box::new(Card::new(Box::new(body)))]).with_card_height(180.0),
            ),
        );
        app.root_widget.update(&mut app.state.doc, &app.state.root);
        app.init_layout();
        layout(&mut app, 300);
        assert_eq!(app.focusable_order.len(), 1);
        app.root_widget.take_structure_dirty();
        requested.set(2);
        assert!(app.root_widget.update(&mut app.state.doc, &app.state.root));
        assert!(app.root_widget.take_structure_dirty());
        app.rebuild_structure();
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 300, 2000, 1.0, true, None).unwrap();
        assert_eq!(app.focusable_order.len(), 2);
        let rects = app
            .focusable_order
            .iter()
            .map(|&node| app.inspect_node(node, |widget| widget.get_rect()).unwrap())
            .collect::<Vec<_>>();
        let control = crate::widget::ControlSize::Regular.height(&Theme::dark());
        assert_eq!(rects[0].height(), control);
        assert_eq!(rects[1].height(), control);
        assert_eq!(rects[1].y0 - rects[0].y1, 8.0);
        let target = rects[1].center();
        assert!(app.dispatch_semantic_click(target.x, target.y));
        assert_eq!(*clicks.borrow(), vec![1]);
        let mut scene = Scene::new();
        app.root_widget.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut app.text_renderer,
            &app.theme,
            &PresenceCtx::default(),
        );
        for (&node, before) in app.focusable_order.iter().zip(rects) {
            assert_eq!(
                app.inspect_node(node, |widget| widget.get_rect()).unwrap(),
                before
            );
        }
    }

    // Case `card_grid_nested_below_minimum_uses_actual_parent_allocation`.
    {
        let nested = CardGrid::new(vec![
            Box::new(Button::new("nested-a", "A", None)),
            Box::new(Button::new("nested-b", "B", None)),
        ])
        .with_min_card_width(300.0)
        .with_card_height(60.0)
        .with_gap(16.0);
        let outer = CardGrid::new(vec![Box::new(Card::new(Box::new(nested)))])
            .with_min_card_width(260.0)
            .with_card_height(180.0);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(outer));
        app.init_layout();
        layout(&mut app, 200);
        let card = app.root_widget.children()[0];
        let surface = card.children()[0];
        let nested = surface.children()[0];
        assert_eq!(card.get_rect().width(), 200.0);
        assert!(nested.get_rect().width() < 200.0);
        let buttons = nested.children();
        assert_eq!(buttons[1].get_rect().y0 - buttons[0].get_rect().y0, 76.0);
        assert!(
            buttons
                .iter()
                .all(|button| button.get_rect().x1 <= card.get_rect().x1)
        );
        assert_eq!(app.focusable_order.len(), 2);
    }
}

#[test]
fn card_grid_reflows_rows_and_retains_edit_state() {
    // Case `card_grid_reflows_retained_nodes_and_keeps_explicit_rows`.
    {
        let (mut app, _) = fixture(6);
        layout(&mut app, 900);
        let order = app.focusable_order.clone();
        app.set_focused_node(Some(order[4]));
        for (width, height, second_row_y) in [
            (900, 376.0, 196.0),
            (600, 572.0, 196.0),
            (200, 1160.0, 196.0),
            (900, 376.0, 196.0),
        ] {
            layout(&mut app, width);
            let grid = app.root_widget.children()[0];
            assert_eq!(grid.get_rect().height(), height);
            assert_eq!(grid.get_rect().width(), f64::from(width));
            let cards = grid.children();
            let columns = match width {
                900 => 3,
                600 => 2,
                _ => 1,
            };
            assert_eq!(cards[columns].get_rect().y0, second_row_y);
            for card in cards {
                assert_eq!(card.get_rect().height(), 180.0);
                assert!(card.get_rect().x1 <= f64::from(width));
            }
            assert_eq!(app.root_widget.children()[1].get_rect().y0, height + 20.0);
            assert_eq!(
                app.focusable_order, order,
                "resize retains focus identities"
            );
            assert_eq!(app.focused_node, Some(order[4]));
        }
    }

    // Case `card_grid_long_content_does_not_turn_fixed_rows_into_minimums`.
    {
        let body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(crate::widget::Text::new(
                &"Long content wraps across lines. ".repeat(80),
            )))
            .push(Box::new(Button::new("long-action", "Action", None)));
        let root = CardGrid::new(vec![Box::new(Card::new(Box::new(body)))])
            .with_card_height(180.0)
            .with_min_card_width(260.0);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        layout(&mut app, 200);
        assert_eq!(app.root_widget.get_rect().height(), 180.0);
        assert_eq!(app.root_widget.children()[0].get_rect().height(), 180.0);
    }

    // Case `card_grid_retains_local_edit_state_across_width_changes`.
    {
        let input = crate::widget::TextInput::local().with_initial_value("saved value");
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CardGrid::new(vec![Box::new(Card::new(Box::new(input)))])),
        );
        app.init_layout();
        layout(&mut app, 900);
        let node = app.focusable_order[0];
        app.set_focused_node(Some(node));
        let state = app
            .inspect_node(node, |widget| crate::undo::get_state(widget))
            .unwrap();
        assert!(state.is_some());
        for width in [200, 600, 900] {
            layout(&mut app, width);
            assert_eq!(app.focusable_order, vec![node]);
            assert_eq!(app.focused_node, Some(node));
            assert_eq!(
                app.inspect_node(node, |widget| crate::undo::get_state(widget))
                    .unwrap(),
                state
            );
        }
    }

    // Case `card_grid_forwards_retained_list_structure_changes_to_host`.
    {
        use crate::widget::{ListModel, VirtualList};
        use std::cell::Cell;
        struct GrowingModel {
            keys: Rc<RefCell<Vec<u64>>>,
            changed: Rc<Cell<bool>>,
        }
        impl ListModel for GrowingModel {
            type Key = u64;
            fn len(&self) -> usize {
                self.keys.borrow().len()
            }
            fn key_at(&self, index: usize) -> Option<u64> {
                self.keys.borrow().get(index).copied()
            }
            fn index_of(&self, key: &u64) -> Option<usize> {
                self.keys
                    .borrow()
                    .iter()
                    .position(|candidate| candidate == key)
            }
            fn poll_changed(&mut self) -> bool {
                self.changed.replace(false)
            }
        }
        let keys = Rc::new(RefCell::new(vec![1, 2]));
        let changed = Rc::new(Cell::new(false));
        let list = VirtualList::new(
            GrowingModel {
                keys: keys.clone(),
                changed: changed.clone(),
            },
            32.0,
            |_, _, key| Box::new(Button::new(&format!("row-{key}"), "Row", None)),
        )
        .with_viewport_height(120.0);
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CardGrid::new(vec![Box::new(Card::new(Box::new(list)))])),
        );
        app.init_layout();
        layout(&mut app, 300);
        let button_keys = |app: &App| {
            app.focusable_order
                .iter()
                .filter_map(|&node| {
                    app.inspect_node(node, |widget| {
                        if widget.name() == "Button" {
                            widget
                                .accessibility()
                                .and_then(|access| access.semantic_key)
                        } else {
                            None
                        }
                    })
                    .unwrap()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(button_keys(&app), vec!["row-1", "row-2"]);
        app.root_widget.take_structure_dirty();
        keys.borrow_mut().push(3);
        changed.set(true);
        assert!(app.root_widget.update(&mut app.state.doc, &app.state.root));
        app.root_widget.sync(&mut app.state.doc, &app.state.root);
        assert!(app.root_widget.take_structure_dirty());
        app.rebuild_structure();
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 300, 2000, 1.0, true, None).unwrap();
        assert_eq!(button_keys(&app), vec!["row-1", "row-2", "row-3"]);
        // Rebuilding and post-build sync must preserve actual delegate geometry.
        for node in &app.focusable_order {
            assert!(app.layout.taffy.layout(*node).unwrap().size.height > 0.0);
            assert!(
                app.inspect_node(*node, |widget| widget.get_rect().height())
                    .unwrap()
                    > 0.0
            );
        }
    }

    // Case `card_grid_empty_and_single_keep_intrinsic_height`.
    {
        for (count, height) in [(0, 0.0), (1, 180.0)] {
            let (mut app, _) = fixture(count);
            layout(&mut app, 200);
            assert_eq!(app.root_widget.children()[0].get_rect().height(), height);
            assert_eq!(app.focusable_order.len(), count);
        }
    }
}

struct GrowingBody {
    column: Column,
    requested: Rc<Cell<usize>>,
    count: usize,
    dirty: bool,
    clicks: Rc<RefCell<Vec<usize>>>,
}
impl Widget for GrowingBody {
    fn update(&mut self, _: &mut AppModel, _: &ModelPath) -> bool {
        if self.count == self.requested.get() {
            return false;
        }
        self.count = self.requested.get();
        self.column = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_gap(8.0);
        for index in 0..self.count {
            let clicks = self.clicks.clone();
            self.column = std::mem::take(&mut self.column).push(Box::new(
                Button::new(&format!("dynamic-{index}"), "Action", None)
                    .on_click(move || clicks.borrow_mut().push(index)),
            ));
        }
        self.dirty = true;
        true
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.column.build_layout(tree, theme, map)
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.column.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.column.draw(scene, transform, text, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.column.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.column.children()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.column.children_mut()
    }
    fn get_rect(&self) -> KurboRect {
        self.column.get_rect()
    }
    fn name(&self) -> &'static str {
        "GrowingBody"
    }
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}
