use super::*;
use crate::compositor::selection::{
    SelectionStore, SharedSelection, Target, activate_history_item, clear_clipboard_history,
    clear_mirror, clipboard_history, pin_history_item, remove_history_item, set_mirror_text,
};
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::WorkspaceTree;
use std::sync::{Arc, Mutex};
use ui::widget::Container;

fn bridge(store: SharedSelection) -> ClipboardManagerBridge {
    std::rc::Rc::new(move |action| {
        match action {
            ClipboardManagerAction::Snapshot => {}
            ClipboardManagerAction::Activate(id) => {
                let _ = activate_history_item(&store, id);
            }
            ClipboardManagerAction::SetPinned { id, pinned } => {
                let _ = pin_history_item(&store, id, pinned);
            }
            ClipboardManagerAction::Remove(id) => {
                let _ = remove_history_item(&store, id);
            }
            ClipboardManagerAction::ClearUnpinned => clear_clipboard_history(&store),
        }
        clipboard_history(&store)
    })
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn clipboard_history_is_bridged_searched_and_activated_only_explicitly() {
    fn quick_settings_clipboard_bridge_uses_the_compositor_history() {
        let store = Arc::new(Mutex::new(SelectionStore::default()));
        assert!(set_mirror_text(&store, Target::Clipboard, "keep"));
        assert!(set_mirror_text(&store, Target::Clipboard, "discard"));
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        host.set_clipboard_manager_bridge(bridge(store.clone()));

        chrome
            .borrow_mut()
            .open_quick_settings_level(crate::shell::quick_settings::QsLevel::Clipboard);
        assert_eq!(
            chrome.borrow().quick_settings.current_level(),
            &crate::shell::quick_settings::QsLevel::Clipboard
        );
        let history = host.clipboard_manager_call(ClipboardManagerAction::Snapshot);
        assert_eq!(history.len(), 2);
        let keep = history[1].id;
        let discard = history[0].id;
        let _ = host.clipboard_manager_call(ClipboardManagerAction::SetPinned {
            id: keep,
            pinned: true,
        });
        let remaining = host.clipboard_manager_call(ClipboardManagerAction::ClearUnpinned);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, keep);
        assert!(remaining[0].pinned);
        assert!(!remaining.iter().any(|item| item.id == discard));

        let _ = host.clipboard_manager_call(ClipboardManagerAction::Activate(keep));
        assert_eq!(
            crate::compositor::selection::mirror_text(&store, Target::Clipboard).as_deref(),
            Some("keep")
        );
        assert!(
            host.clipboard_manager_call(ClipboardManagerAction::Remove(keep))
                .is_empty()
        );
    }

    fn clipboard_keyboard_selects_then_explicitly_activates_an_item() {
        let store = Arc::new(Mutex::new(SelectionStore::default()));
        assert!(set_mirror_text(&store, Target::Clipboard, "older"));
        assert!(set_mirror_text(&store, Target::Clipboard, "newer"));
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        host.set_clipboard_manager_bridge(bridge(store.clone()));
        chrome
            .borrow_mut()
            .open_quick_settings_level(crate::shell::quick_settings::QsLevel::Clipboard);

        assert!(host.handle_qs_clipboard_keyboard(&Key::Named(NamedKey::ArrowDown), false));
        assert!(host.handle_qs_clipboard_keyboard(&Key::Named(NamedKey::Enter), false));
        assert_eq!(
            crate::compositor::selection::mirror_text(&store, Target::Clipboard).as_deref(),
            Some("older")
        );
        assert!(!chrome.borrow().overlays.quick_settings);
    }

    fn clipboard_provenance_labels_are_concise_and_honest() {
        let local = crate::compositor::selection::ClipboardHistorySnapshot {
            id: [1; 32],
            mime_types: vec!["text/plain".into()],
            text_preview: Some("local".into()),
            byte_len: 5,
            pinned: false,
            source_device: None,
            created_unix_ms: 1_000,
        };
        assert_eq!(clipboard_history_source_label(&local, None), "This device");
        assert_eq!(clipboard_history_relative_time(1_000, 181_000), "3 min ago");
    }

    fn clipboard_search_filters_metadata_before_explicit_activation() {
        let store = Arc::new(Mutex::new(SelectionStore::default()));
        assert!(set_mirror_text(&store, Target::Clipboard, "project alpha"));
        assert!(set_mirror_text(&store, Target::Clipboard, "meeting notes"));
        clear_mirror(&store, Target::Clipboard);
        let tree = WorkspaceTree::bootstrap_demo();
        let chrome = ChromeState::new(tree);
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        host.set_clipboard_manager_bridge(bridge(store.clone()));
        chrome
            .borrow_mut()
            .open_quick_settings_level(crate::shell::quick_settings::QsLevel::Clipboard);

        assert!(host.handle_qs_clipboard_keyboard(&Key::Character("alpha".into()), false));
        let matches = host.filtered_clipboard_history();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text_preview.as_deref(), Some("project alpha"));
        assert_eq!(
            crate::compositor::selection::mirror_text(&store, Target::Clipboard),
            None
        );

        assert!(host.handle_qs_clipboard_keyboard(&Key::Named(NamedKey::Enter), false));
        assert_eq!(
            crate::compositor::selection::mirror_text(&store, Target::Clipboard).as_deref(),
            Some("project alpha")
        );
    }

    quick_settings_clipboard_bridge_uses_the_compositor_history();
    clipboard_keyboard_selects_then_explicitly_activates_an_item();
    clipboard_provenance_labels_are_concise_and_honest();
    clipboard_search_filters_metadata_before_explicit_activation();
}

/// Super+V opens history where the text goes and pastes the choice; history
/// browsed from Quick Settings stays under the tray and only copies.
#[test]
fn super_v_opens_history_at_the_caret_and_pastes_the_choice() {
    let store = Arc::new(Mutex::new(SelectionStore::default()));
    assert!(set_mirror_text(&store, Target::Clipboard, "older"));
    assert!(set_mirror_text(&store, Target::Clipboard, "newer"));
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    host.set_clipboard_manager_bridge(bridge(store.clone()));

    // Browsed from Quick Settings: under the tray, top right.
    chrome
        .borrow_mut()
        .open_quick_settings_level(crate::shell::quick_settings::QsLevel::Clipboard);
    let tray = host.quick_settings_rect_for_tests();
    assert!(tray.x1 > 1400.0 && tray.y0 < 100.0, "{tray:?}");
    assert!(host.handle_qs_clipboard_keyboard(&Key::Named(NamedKey::Enter), false));
    assert!(
        host.take_clipboard_pastes().is_empty(),
        "choosing in Quick Settings only copies"
    );

    // Super+V while typing at a caret in the left half of the screen.
    let caret = Rect::new(300.0, 400.0, 302.0, 418.0);
    host.set_text_caret(Some(caret));
    assert!(host.run_shortcut_action(settings_store::hardware::ShortcutAction::ClipboardHistory));
    assert_eq!(
        chrome.borrow().quick_settings.current_level(),
        &crate::shell::quick_settings::QsLevel::Clipboard
    );
    let at = host.quick_settings_rect_for_tests();
    assert!(
        (at.x0 - caret.x0).abs() < 1.0 && at.y0 > caret.y1 && at.y0 - caret.y1 < 24.0,
        "history opens just below the caret, not under the tray: {at:?}"
    );
    let history = host.filtered_clipboard_history();
    let older = history[1].id;
    assert!(host.handle_qs_clipboard_keyboard(&Key::Named(NamedKey::ArrowDown), false));
    assert!(host.handle_qs_clipboard_keyboard(&Key::Named(NamedKey::Enter), false));
    assert!(!chrome.borrow().overlays.quick_settings);
    assert_eq!(
        crate::compositor::selection::mirror_text(&store, Target::Clipboard).as_deref(),
        Some("older"),
        "the choice is also the clipboard"
    );
    assert_eq!(host.take_clipboard_pastes(), vec![older], "and is pasted");
    assert_eq!(
        crate::compositor::selection::history_item_text(&store, older).as_deref(),
        Some("older"),
        "the full text of the chosen item is what the compositor commits"
    );

    // Back (Escape, the chevron) leaves history opened to paste altogether,
    // rather than revealing the Quick Settings root under the tray.
    assert!(host.run_shortcut_action(settings_store::hardware::ShortcutAction::ClipboardHistory));
    chrome.borrow_mut().quick_settings_back();
    assert!(!chrome.borrow().overlays.quick_settings);
    assert!(host.take_clipboard_pastes().is_empty());

    // Reopening Quick Settings another way forgets the paste origin.
    chrome.borrow_mut().toggle_quick_settings();
    chrome
        .borrow_mut()
        .quick_settings
        .push_level(crate::shell::quick_settings::QsLevel::Clipboard);
    assert_eq!(chrome.borrow().quick_settings.paste_origin, None);
    assert!(host.quick_settings_rect_for_tests().x1 > 1400.0);
}

/// Without a caret, Super+V opens history over the focused window, never
/// across the screen at the tray.
#[test]
fn super_v_without_a_caret_opens_over_the_focused_window() {
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    let window = Rect::new(40.0, 60.0, 640.0, 860.0);
    chrome
        .borrow_mut()
        .open_clipboard_history_to_paste(crate::shell::chrome_state::PasteOrigin {
            caret: None,
            window: Some(window),
        });
    let at = host.quick_settings_rect_for_tests();
    assert!(
        (at.center().x - window.center().x).abs() < 1.0 && at.y0 > window.y0,
        "{at:?}"
    );
}
