//! The `/` block menu must capture keyboard focus: while it's open, ↑/↓
//! navigate the menu — not the page list behind it — Enter inserts the
//! chosen block, Esc closes. Regression guard for the case where the
//! sidebar retained focus (after a row click) and swallowed the arrows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::framework_support::{add_note, notes, open, send as dispatch, surface};
use ui::widget::BlockKind;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::NamedKey;

use ui::synthetic_input::cursor_moved;

fn mouse(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

use ui::synthetic_input::char_key;

use ui::synthetic_input::named_key;

#[test]
fn slash_menu_owns_arrow_and_enter_keys() {
    eprintln!("phase slash_menu_arrows_navigate_the_menu_not_the_page_list");
    {
        let mut app = notes();
        let welcome = add_note(&mut app, "Welcome", "");
        // A second page so a stray ArrowDown on the page list would be visible
        // (it would move the selection off Welcome).
        let other = add_note(&mut app, "Other", "");
        open(&mut app, other);

        // Click Welcome's row: this selects it AND gives the sidebar focus,
        // while loading the page focuses the editor too — the exact state in
        // which the sidebar used to steal the arrows.
        let r0 = surface(&mut app, |notes| notes.sidebar_row_center_for_test(0)).unwrap();
        dispatch(&mut app, &cursor_moved(r0.x, r0.y));
        dispatch(&mut app, &mouse(ElementState::Pressed));
        dispatch(&mut app, &mouse(ElementState::Released));
        assert_eq!(
            surface(&mut app, |notes| notes.selected_for_test()),
            Some(welcome)
        );

        // Type `/` in the editor → the block menu opens.
        dispatch(&mut app, &char_key("/"));
        assert!(
            surface(&mut app, |notes| notes.editor_slash_menu_visible_for_test()),
            "typing `/` opens the block menu"
        );
        assert_eq!(
            surface(&mut app, |notes| notes
                .main_panel_mut()
                .editor()
                .slash_menu()
                .highlighted()
                .cloned()),
            Some(BlockKind::Paragraph),
            "the menu starts on the first row"
        );

        // ArrowDown must move the MENU highlight, not the page selection.
        dispatch(&mut app, &named_key(NamedKey::ArrowDown));
        assert_eq!(
            surface(&mut app, |notes| notes.selected_for_test()),
            Some(welcome),
            "the page selection must NOT move while the `/` menu owns the arrows"
        );
        assert_eq!(
            surface(&mut app, |notes| notes
                .main_panel_mut()
                .editor()
                .slash_menu()
                .highlighted()
                .cloned()),
            Some(BlockKind::Heading(1)),
            "ArrowDown advances the block-menu highlight"
        );

        // Escape closes the menu.
        dispatch(&mut app, &named_key(NamedKey::Escape));
        assert!(
            !surface(&mut app, |notes| notes.editor_slash_menu_visible_for_test()),
            "Escape closes the block menu"
        );
    }

    eprintln!("phase slash_menu_enter_inserts_the_chosen_block");
    {
        // A fresh empty note gives a clean single-paragraph editor.
        let mut app = notes();
        let page = add_note(&mut app, "Empty", "");
        open(&mut app, page);

        // Open the menu, arrow to Heading 1, Enter to convert the block.
        dispatch(&mut app, &char_key("/"));
        assert!(surface(&mut app, |notes| notes.editor_slash_menu_visible_for_test()));
        dispatch(&mut app, &named_key(NamedKey::ArrowDown));
        dispatch(&mut app, &named_key(NamedKey::Enter));

        assert!(
            !surface(&mut app, |notes| notes.editor_slash_menu_visible_for_test()),
            "Enter commits and closes the menu"
        );
        assert!(
            surface(&mut app, |notes| matches!(
                notes.main_panel_mut().editor().blocks()[0].kind,
                BlockKind::Heading(1)
            )),
            "Enter inserted/converted the block to the chosen kind (Heading 1)"
        );
    }
}
