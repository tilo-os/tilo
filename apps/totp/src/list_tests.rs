use super::*;
use crate::model::demo_accounts;
use crate::test_events::{cursor_at, left_press, wheel};
use vello::Scene;
use vello::kurbo::Affine;

const VIEWPORT: Rect = Rect::new(0.0, 0.0, 700.0, 800.0);
const COLUMN: Rect = Rect::new(40.0, 0.0, 660.0, 800.0);

/// A list of the demo accounts, laid out the way a paint lays it out.
fn laid_out(viewport: Rect) -> (AccountList, TextRenderer, Theme) {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut list = AccountList::new();
    list.set_viewport(viewport, Rect::new(40.0, viewport.y0, 660.0, viewport.y1));
    list.set_accounts(demo_accounts());
    list.layout(&mut text, &theme);
    (list, text, theme)
}

fn center(rect: Rect) -> Point {
    rect.center()
}

/// Feed one pointer event the way the host does and report whether it was
/// consumed.
fn send(list: &mut AccountList, event: &WindowEvent) -> bool {
    let mut handled = false;
    list.handle_event(event, 1.0, &mut handled);
    handled
}

fn click(list: &mut AccountList, p: Point) -> Vec<ListEvent> {
    send(list, &cursor_at(p));
    send(list, &left_press());
    list.take_events()
}

/// Row geometry: equal rows that stack, and one trailing column for the codes.
#[test]
fn rows_stack_and_share_one_trailing_column() {
    let (list, _, _) = laid_out(VIEWPORT);
    let rows: Vec<Rect> = (0..list.len()).map(|i| list.row_rect(i)).collect();

    // Case `rows_are_equally_tall_and_do_not_overlap`.
    for pair in rows.windows(2) {
        assert!((pair[0].height() - pair[1].height()).abs() < 0.01);
        assert!(pair[1].y0 >= pair[0].y1 - 0.01, "rows must not overlap");
    }
    assert!(
        rows[0].y0 >= VIEWPORT.y0,
        "the first row stands inside the viewport"
    );

    // Case `every_row_spans_the_readable_column`.
    for row in &rows {
        assert!((row.x0 - COLUMN.x0).abs() < 0.01 && (row.x1 - COLUMN.x1).abs() < 0.01);
    }

    // Case `codes_and_rings_line_up_down_the_list_whatever_the_digit_count`.
    let code_edges: Vec<f64> = (0..list.len())
        .map(|i| list.at(i, list.layouts[i].code).x1)
        .collect();
    assert!(
        code_edges.windows(2).all(|p| (p[0] - p[1]).abs() < 0.01),
        "the code column ends at the same x in every row: {code_edges:?}"
    );

    // Case `hover_actions_never_reach_into_the_code`.
    for (i, layout) in list.layouts.iter().enumerate() {
        for action in &layout.actions {
            assert!(action.x1 <= layout.code.x0 + 0.01, "row {i}: {action:?}");
        }
    }
}

/// What the pointer is over decides what a click does.
#[test]
fn clicks_select_copy_and_reach_the_hover_actions() {
    // Case `the_body_selects_and_the_code_copies`.
    {
        let (mut list, _, _) = laid_out(VIEWPORT);
        let id = list.account_at(1).unwrap().id;
        let row = list.row_rect(1);
        let body = Point::new(row.x0 + 60.0, row.center().y);
        assert_eq!(click(&mut list, body), vec![ListEvent::Select(id)]);
        assert_eq!(list.selected(), Some(id), "the clicked row is selected");

        let code = center(list.at(1, list.layouts[1].code));
        assert_eq!(click(&mut list, code), vec![ListEvent::Copy(id)]);
    }

    // Case `a_hovered_row_offers_edit_reorder_and_delete`.
    {
        let (mut list, _, _) = laid_out(VIEWPORT);
        let id = list.account_at(2).unwrap().id;
        let row = list.row_rect(2);
        // Hover the row first: its actions appear under the pointer.
        send(
            &mut list,
            &cursor_at(Point::new(row.x0 + 60.0, row.center().y)),
        );
        let actions = list.layouts[2].actions.clone();
        assert_eq!(
            actions.len(),
            4,
            "edit, up, down and delete in a middle row"
        );
        let expected = [
            ListEvent::Edit(id),
            ListEvent::MoveUp(id),
            ListEvent::MoveDown(id),
            ListEvent::Delete(id),
        ];
        for (action, event) in actions.iter().zip(expected) {
            let target = center(list.at(2, *action));
            assert_eq!(click(&mut list, target), vec![event]);
            send(
                &mut list,
                &cursor_at(Point::new(row.x0 + 60.0, row.center().y)),
            );
        }
        assert_eq!(
            list.selected(),
            None,
            "an action works on its row without selecting it"
        );
    }

    // Case `the_ends_of_the_list_offer_only_the_moves_that_exist`.
    {
        let (list, _, _) = laid_out(VIEWPORT);
        let bottom = list.len() - 1;
        assert_eq!(
            list.layouts[0].actions.len(),
            3,
            "no move up on the first row"
        );
        assert_eq!(
            list.layouts[bottom].actions.len(),
            3,
            "no move down on the last row"
        );
    }

    // Case `a_row_without_a_key_offers_set_up_in_place_of_a_code`.
    {
        let (mut list, mut text, theme) = laid_out(VIEWPORT);
        let mut accounts = demo_accounts();
        accounts[0].secret.clear();
        let id = accounts[0].id;
        list.set_accounts(accounts);
        list.layout(&mut text, &theme);
        let layout = &list.layouts[0];
        assert!(layout.code.area() == 0.0, "no code to copy");
        let button = center(list.at(0, layout.set_up));
        assert_eq!(click(&mut list, button), vec![ListEvent::SetUp(id)]);
    }

    // Case `a_click_outside_every_row_does_nothing`.
    {
        let (mut list, _, _) = laid_out(VIEWPORT);
        let below = Point::new(100.0, 780.0);
        assert!(click(&mut list, below).is_empty());
    }
}

/// Scrolling follows the wheel, stays in range and can reveal a row.
#[test]
fn the_list_scrolls_within_its_range_and_reveals_a_row() {
    // A short viewport: the five rows do not fit.
    let short = Rect::new(0.0, 0.0, 700.0, 180.0);
    let (mut list, _, _) = laid_out(short);
    assert!(
        list.max_scroll() > 0.0,
        "the rows overflow a short viewport"
    );

    // Case `the_wheel_scrolls_and_clamps`.
    send(&mut list, &cursor_at(Point::new(300.0, 90.0)));
    assert!(
        send(&mut list, &wheel(-1.0)),
        "a wheel notch scrolls the list"
    );
    assert!(list.scroll_offset() > 0.0);
    for _ in 0..20 {
        send(&mut list, &wheel(-1.0));
    }
    assert!((list.scroll_offset() - list.max_scroll()).abs() < 0.01);
    for _ in 0..40 {
        send(&mut list, &wheel(1.0));
    }
    assert!(
        list.scroll_offset().abs() < 0.01,
        "scrolling back stops at the top"
    );

    // Case `a_row_scrolled_out_of_view_has_no_bounds_until_revealed`.
    let bottom = list.account_at(list.len() - 1).unwrap().id;
    assert!(list.account_bounds(bottom).is_none());
    list.reveal(bottom);
    let bounds = list.account_bounds(bottom).expect("revealed");
    assert!(bounds.y1 <= short.y1 && bounds.y0 >= short.y0);

    // Case `a_row_scrolled_out_of_view_cannot_be_hit`.
    let first = list.account_at(0).unwrap().id;
    assert!(list.account_bounds(first).is_none());
    assert!(
        click(&mut list, Point::new(100.0, 4.0)).len() <= 1,
        "the strip above the first visible row never reaches a hidden one"
    );
}

/// The empty list asks for its one action.
#[test]
fn an_empty_list_offers_one_action() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut list = AccountList::new();
    list.set_viewport(VIEWPORT, COLUMN);
    let mut scene = Scene::new();
    list.draw(&mut PaintCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
    ));
    let action = list.empty_action_rect();
    assert!(action.area() > 0.0, "the empty state paints its action");
    assert_eq!(click(&mut list, action.center()), vec![ListEvent::Add]);
    assert!(click(&mut list, Point::new(5.0, 5.0)).is_empty());
}

/// Painting uses the laid-out geometry, in every state a row can be in.
#[test]
fn painting_every_row_state_paints() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut list = AccountList::new();
    list.set_viewport(VIEWPORT, COLUMN);
    let mut accounts = demo_accounts();
    accounts[1].secret.clear();
    list.set_accounts(accounts);
    list.set_selected(list.account_at(0).map(|a| a.id));
    let paint = |list: &mut AccountList, text: &mut TextRenderer, theme: &Theme| {
        let mut scene = Scene::new();
        list.draw(&mut PaintCtx::new(
            &mut scene,
            Affine::IDENTITY,
            text,
            theme,
        ));
        scene.encoding().is_empty()
    };

    // Case `a_resting_list_paints`.
    assert!(!paint(&mut list, &mut text, &theme));

    // Case `a_hovered_row_and_a_hovered_code_paint`.
    let row = list.row_rect(2);
    send(
        &mut list,
        &cursor_at(Point::new(row.x0 + 60.0, row.center().y)),
    );
    assert!(!paint(&mut list, &mut text, &theme));
    let code = center(list.at(2, list.layouts[2].code));
    send(&mut list, &cursor_at(code));
    assert_eq!(list.hover, Some((2, Zone::Code)));
    assert!(!paint(&mut list, &mut text, &theme));

    // Case `reduced_motion_and_light_themes_paint`.
    let mut calm = Theme::light();
    calm.reduce_motion = true;
    assert!(!paint(&mut list, &mut text, &calm));
}
