//! Submenus, row details and shortcuts, and menus hung from an anchor.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::Arc;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::app::driver::Framed;
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
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion"
)]
const WINDOW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

fn cursor_at(p: Point) -> WindowEvent {
    tilo_ui_base::synthetic_input::cursor_moved(p.x, p.y)
}
fn paint(m: &mut ContextMenu) {
    let presence = PresenceCtx::default();
    let previous = presence.push_viewport(WINDOW);
    let mut scene = Scene::new();
    m.draw_overlay_for_tests(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        presence.draw_env(),
    ));
    presence.pop_viewport(previous);
}

fn file_menu(fired: &Arc<AtomicU32>) -> ContextMenu {
    let markdown = Arc::clone(fired);
    ContextMenu::with_items(vec![
        MenuItem::action("Open", || {})
            .with_shortcut(tilo_ui_base::platform::Shortcut::command("O")),
        MenuItem::submenu(
            "New file",
            vec![
                MenuItem::action("Text file", || {}),
                MenuItem::action("Markdown file", move || {
                    markdown.fetch_add(1, Ordering::SeqCst);
                }),
            ],
        ),
        MenuItem::action("Delete", || {})
            .with_shortcut(tilo_ui_base::platform::Shortcut::key("Delete")),
    ])
}

tilo_test_env::scenario! {
    /// Submenus, row details and shortcuts, and menus hung from an anchor.
    /// Each phase was a test of its own and builds its own fixture.
    fn nested_and_anchored_menus_open_run_and_stand_by_their_anchor;

    phase a_submenu_opens_beside_its_row_by_hover_and_by_keyboard;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
        let fired = Arc::new(AtomicU32::new(0));
        let mut m = file_menu(&fired);
        m.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), WINDOW);
        paint(&mut m);
        let row = m.item_rect(1).expect("the submenu row is shown");
        dispatch(&mut m, &cursor_at(row.center()));
        assert!(m.is_submenu_open(), "hover opens the nested menu");
        paint(&mut m);
        let nested = m.nested_menu_for_tests().map(ContextMenu::rect_for_tests).unwrap();
        assert!(
            nested.x0 >= m.rect_for_tests().x1 - 0.5,
            "beside the menu: {nested:?}"
        );
        let nested_row = m
            .submenu_for_tests()
            .and_then(|(_, menu)| menu.item_rect(1))
            .unwrap();
        dispatch(&mut m, &cursor_at(nested_row.center()));
        dispatch(&mut m, &mouse_press());
        dispatch(&mut m, &mouse_release());
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(m.take_submenu_selection(), Some((1, 1)));
        assert!(!m.is_open());

        // The keyboard: Right opens it with focus, Left steps back out.
        let mut m = file_menu(&fired);
        m.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), WINDOW);
        paint(&mut m);
        m.focus_item(1);
        dispatch(&mut m, &named_key(NamedKey::ArrowRight));
        assert!(m.is_submenu_open());
        assert_eq!(
            m.submenu_for_tests().and_then(|(_, menu)| menu.hovered()),
            Some(0)
        );
        dispatch(&mut m, &named_key(NamedKey::ArrowLeft));
        assert!(!m.is_submenu_open());
        assert!(m.is_open(), "Left closes only the nested menu");
        let node = Widget::accessibility(&m).unwrap();
        assert_eq!(node.children[1].state.expanded, Some(false));
        assert_eq!(node.children[0].description.as_deref(), Some("Ctrl+O"));
    }

    // A menu nested in a nested menu (a password manager's logins under
    // Autofill under its own item) follows the pointer and takes a pick.
    phase a_pick_two_menus_deep_runs_and_closes_the_whole_menu;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let fired = Arc::new(AtomicU32::new(0));
        let login = Arc::clone(&fired);
        let mut m = ContextMenu::with_items(vec![
            MenuItem::action("Reload", || {}),
            MenuItem::submenu(
                "Passwords",
                vec![
                    MenuItem::action("Generate", || {}),
                    MenuItem::submenu(
                        "Fill in",
                        vec![MenuItem::action("Personal", move || {
                            login.fetch_add(1, Ordering::SeqCst);
                        })],
                    ),
                ],
            ),
        ]);
        m.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), WINDOW);
        paint(&mut m);
        let row = m.item_rect(1).unwrap();
        dispatch(&mut m, &cursor_at(row.center()));
        paint(&mut m);
        let row = m
            .submenu_for_tests()
            .and_then(|(_, menu)| menu.item_rect(1))
            .unwrap();
        dispatch(&mut m, &cursor_at(row.center()));
        paint(&mut m);
        let deepest = m
            .submenu_for_tests()
            .and_then(|(_, menu)| menu.submenu_for_tests())
            .map(|(_, menu)| menu)
            .expect("hover opens the menu nested in the nested one");
        let target = deepest.item_rect(0).unwrap();
        assert!(
            !m.submenu_for_tests()
                .unwrap()
                .1
                .contains([target.center().x, target.center().y]),
            "fixture: the row is outside the first nested menu"
        );
        dispatch(&mut m, &cursor_at(target.center()));
        dispatch(&mut m, &mouse_press());
        dispatch(&mut m, &mouse_release());
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(m.take_submenu_selection(), Some((1, 1)));
        assert!(!m.is_open(), "the whole menu closes");
    }

    // The pointer in a nested menu hands it the keys: its row is lit where the pointer is,
    // and the arrows move on from there, past a rule, to the row Enter chooses.
    phase a_nested_menu_under_the_pointer_takes_the_arrows;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let fired = Arc::new(AtomicU32::new(0));
        let other = Arc::clone(&fired);
        let mut m = ContextMenu::with_items(vec![
            MenuItem::action("Open", || {}),
            MenuItem::submenu(
                "Open with",
                vec![
                    MenuItem::action("Text Editor", || {}),
                    MenuItem::separator(),
                    MenuItem::action("Other application…", move || {
                        other.fetch_add(1, Ordering::SeqCst);
                    }),
                ],
            ),
            MenuItem::action("Delete", || {}),
        ]);
        m.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), WINDOW);
        paint(&mut m);
        let row = m.item_rect(1).unwrap();
        dispatch(&mut m, &cursor_at(row.center()));
        paint(&mut m);
        let first = m
            .submenu_for_tests()
            .and_then(|(_, menu)| menu.item_rect(0))
            .expect("hover opens the nested menu");
        dispatch(&mut m, &cursor_at(first.center()));
        let nested = |m: &ContextMenu| m.submenu_for_tests().and_then(|(_, menu)| menu.hovered());
        assert_eq!(nested(&m), Some(0), "the row under the pointer is lit");
        dispatch(&mut m, &named_key(NamedKey::ArrowDown));
        assert_eq!(nested(&m), Some(2), "Down moves past the rule in the nested menu");
        assert_eq!(m.hovered(), Some(1), "the outer menu keeps its row");
        dispatch(&mut m, &named_key(NamedKey::Enter));
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(m.take_submenu_selection(), Some((1, 2)));
        assert!(!m.is_open());
    }

    // A filter's value lists how many results share it at the row's end,
    // where a shortcut would stand, and says so to assistive technology.
    phase a_detail_stands_at_the_rows_end_and_is_announced;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let label = "Aurora Skies, the whole discography";
        let mut plain = ContextMenu::with_items(vec![MenuItem::action(label, || {})]);
        let mut counted = ContextMenu::with_items(vec![
            MenuItem::action(label, || {})
                .with_checked(true)
                .with_detail("12"),
        ]);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        for menu in [&mut plain, &mut counted] {
            menu.open_anchored(Rect::new(100.0, 40.0, 140.0, 72.0), WINDOW);
            menu.layout_within(&mut tr, &theme);
        }
        assert!(counted.get_rect().width() > plain.get_rect().width());
        let node = Widget::accessibility(&counted).expect("an open menu is announced");
        assert_eq!(node.children[0].description.as_deref(), Some("12"));
        assert_eq!(node.children[0].state.checked, Some(true));
    }





    phase a_menu_button_opens_its_menu_at_its_own_rect;
    {
        use std::sync::Arc;
use tilo_ui_app::app::driver::Framed;
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
        let fired = Arc::new(AtomicU32::new(0));
        let f = Arc::clone(&fired);
        let button =
            tilo_ui_app::widget::Button::icon_only("more", tilo_ui_base::icons::Icon::MoreHorizontal, "More")
                .with_menu(vec![MenuItem::action("Print", move || {
                    f.fetch_add(1, Ordering::SeqCst);
                })]);
        let mut framed = Framed::new(tilo_ui_app::State::new(), button, (800, 600), Theme::dark());
        let _ = Framed::harness_mut(&mut framed).poll_frame();
        let audit = Framed::harness(&framed).native_app_audit();
        assert!(
            audit.is_certified(),
            "the menu opener has an admitted view action: {:?}",
            audit.issues
        );
        let rect = framed.button().get_rect();
        Framed::harness_mut(&mut framed).click_at(rect.center().x, rect.center().y);
        let _ = Framed::harness_mut(&mut framed).poll_frame();
        assert!(framed.is_open(), "a click opens the menu");
        let menu = framed.menu_mut().get_rect();
        assert!(menu.y0 >= rect.y1, "under the button: {menu:?} vs {rect:?}");
        let node = Widget::accessibility(&*framed).unwrap();
        assert_eq!(node.state.expanded, Some(true));
        let print = framed.menu_mut().item_rect(0).expect("Print is laid out");
        Framed::harness_mut(&mut framed).click_at(print.center().x, print.center().y);
        let _ = Framed::harness_mut(&mut framed).poll_frame();
        assert_eq!(fired.load(Ordering::SeqCst), 1, "the real Print item runs");
        assert!(!framed.is_open(), "choosing Print closes the menu");
        Framed::harness_mut(&mut framed)
            .perform_named("more", &tilo_ui_base::a11y::Action::Activate)
            .expect("assistive activation invokes the same admitted opener");
        let _ = Framed::harness_mut(&mut framed).poll_frame();
        assert!(framed.is_open(), "assistive activation opens the same menu");
        assert_eq!(
            framed.menu_mut().hovered(),
            Some(0),
            "assistive opening selects the first command"
        );
        assert!(
            framed
                .menu_mut()
                .accessibility()
                .unwrap()
                .children
                .iter()
                .any(|item| item.name.as_deref() == Some("Print")),
            "the same Print command is exported after assistive opening"
        );
        let audit = Framed::harness(&framed).native_app_audit();
        assert!(
            audit.is_certified(),
            "open menu retains admitted coverage: {:?}",
            audit.issues
        );
    }
}
