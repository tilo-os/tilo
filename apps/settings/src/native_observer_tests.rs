//! The native-input observer reads the app without touching its document.
//!
//! The snapshot a native audit asks for must describe the live state (the
//! selected category, the layout class, the document heads) without
//! committing the document's pending changes: the observer is a witness, not
//! a writer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::SettingsApp;
use crate::categories::Category;
use automerge::{AutoCommit, ChangeHash};
use ui::headless::Framed;
use ui::widget::Widget;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, PhysicalKey};

const SEARCH: &str = "settings.search";

fn root() -> automerge::ObjId {
    automerge::ROOT
}

/// Settings over the production headless host, which settles the narrow
/// breakpoint's structural pass before the first input as the live host does.
fn hosted(config: &crate::prefs::test_env::ScratchConfig, w: f32, h: f32) -> Framed<SettingsApp> {
    Framed::new(
        ui::State::new(),
        config.app(),
        ui::headless::viewport((w, h)),
        ui::Theme::dark(),
    )
}

fn wide(config: &crate::prefs::test_env::ScratchConfig) -> Framed<SettingsApp> {
    hosted(config, 1200.0, 1800.0)
}

fn snapshot(h: &Framed<SettingsApp>) -> serde_json::Value {
    Framed::harness(h).native_input_snapshot().unwrap()
}

fn heads(h: &mut Framed<SettingsApp>) -> Vec<ChangeHash> {
    Framed::harness_mut(h).state_mut().doc.get_heads()
}

/// Recompose after a change made directly on the app.
fn rebuild(h: &mut Framed<SettingsApp>) {
    Framed::harness_mut(h).relayout();
}

fn select(h: &mut Framed<SettingsApp>, category: Category) {
    h.select(category);
    rebuild(h);
}

/// One Backspace press handed to the root against the last frame's layout;
/// the next [`rebuild`] runs the frame that follows it.
fn press_backspace(h: &mut Framed<SettingsApp>) {
    let mut ev = ui::synthetic_input::char_key("");
    if let WindowEvent::KeyboardInput { event, .. } = &mut ev {
        event.logical_key = Key::Named(winit::keyboard::NamedKey::Backspace);
        event.text = None;
    }
    Framed::harness_mut(h).dispatch_to_root(&ev);
}

fn sidebar_row_count(root: &dyn Widget) -> usize {
    let own = root
        .as_any()
        .and_then(|value| value.downcast_ref::<ui::widget::NavList>())
        .map_or(0, |list| list.visible_item_ids().len());
    root.children()
        .into_iter()
        .map(sidebar_row_count)
        .fold(own, usize::saturating_add)
}

/// The native observer tracks search rebuilds/backspace and never commits pending changes.
#[test]
fn native_observer_tracks_search_without_committing_pending_changes() {
    // Case `native_observer_tracks_search_rebuild_and_backspace`.
    {
        let config = crate::prefs::test_env::ScratchConfig::new();
        let mut h = wide(&config);
        Framed::harness_mut(&mut h).click_named(SEARCH).unwrap();
        let before = snapshot(&h);
        assert_eq!(before["ready"], true);
        assert_eq!(before["rendered_result_count"], sidebar_row_count(&*h));
        assert_eq!(
            before["search_focused"],
            true,
            "observer={before:#}; focused={:?}",
            Framed::harness(&h).focused()
        );
        let original_heads = heads(&mut h);
        Framed::harness_mut(&mut h).type_text("z");
        let typed = snapshot(&h);
        assert_eq!(typed["local_search_text"], "z");
        assert_eq!(typed["synchronized_search_query"], "z");
        assert_eq!(typed["search_widget_text"], "z");
        assert_eq!(typed["search_caret_byte"], 1);
        assert_eq!(typed["search_focused"], true);
        assert_eq!(typed["rendered_result_count"], sidebar_row_count(&*h));
        assert_ne!(
            typed["rendered_category_ids"],
            before["rendered_category_ids"]
        );
        // Native key construction, delivered through the same root widget route.
        press_backspace(&mut h);
        let immediate = snapshot(&h);
        assert_eq!(immediate["local_search_text"], "");
        assert_eq!(immediate["synchronized_search_query"], "z");
        rebuild(&mut h);
        let after = snapshot(&h);
        assert_eq!(after["search_caret_byte"], 0);
        assert_eq!(after["search_focused"], true);
        assert_eq!(
            after["rendered_category_ids"],
            before["rendered_category_ids"]
        );
        assert_eq!(heads(&mut h), original_heads);
        assert_eq!(
            after["effective_document_heads"],
            before["effective_document_heads"]
        );
    }

    // Case `native_observer_does_not_commit_original_pending_changes`.
    {
        use automerge::transaction::Transactable;
        let config = crate::prefs::test_env::ScratchConfig::new();
        let app = config.app();
        let mut doc = AutoCommit::new();
        doc.put(root(), "diagnostic_test", "pending").unwrap();
        let debug_before = format!("{doc:?}");
        let pending = doc.pending_ops();
        let a = app.native_input_snapshot(&doc, &root()).unwrap();
        let b = app.native_input_snapshot(&doc, &root()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a["ready"], false);
        assert_eq!(a["original_pending_ops"], pending);
        assert_eq!(format!("{doc:?}"), debug_before);
        assert_eq!(doc.pending_ops(), pending);
        assert!(!a["effective_document_heads"].as_array().unwrap().is_empty());
        assert!(a.get("document_content").is_none());
    }
}

fn named_widget_identity(widget: &dyn Widget, name: &str) -> Option<usize> {
    if widget.name() == name {
        return Some(std::ptr::from_ref(widget).cast::<()>() as usize);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| named_widget_identity(child, name))
}

#[test]
fn search_preserves_production_network_pane_but_category_switch_replaces_it() {
    let config = crate::prefs::test_env::ScratchConfig::new();
    let mut h = wide(&config);
    let network = named_widget_identity(&*h, "NetworkPane").unwrap();
    Framed::harness_mut(&mut h).click_named(SEARCH).unwrap();
    Framed::harness_mut(&mut h).type_text("z");
    assert_eq!(named_widget_identity(&*h, "NetworkPane"), Some(network));
    select(&mut h, Category::About);
    assert_eq!(h.selected(), super::Category::About);
    assert_eq!(named_widget_identity(&*h, "NetworkPane"), None);
}

#[test]
fn narrow_search_retains_focus_through_filter_and_backspace() {
    let config = crate::prefs::test_env::ScratchConfig::new();
    let mut h = hosted(&config, 440.0, 820.0);
    assert!(h.is_narrow());
    // Reach the list using the real narrow back control, not a screenshot override.
    let back = h.back_button_rect();
    assert!(back.width() > 0.0);
    Framed::harness_mut(&mut h).click_at(back.center().x, back.center().y);
    rebuild(&mut h);
    assert!(h.sidebar_open());
    Framed::harness_mut(&mut h).click_named(SEARCH).unwrap();
    Framed::harness_mut(&mut h).type_text("z");
    let typed = snapshot(&h);
    assert_eq!(typed["search_focused"], true);
    assert_eq!(typed["search_caret_byte"], 1);
    assert_eq!(typed["local_search_text"], "z");
    assert_eq!(typed["sidebar_open"], true);
    assert_eq!(typed["narrow"], true);
    press_backspace(&mut h);
    rebuild(&mut h);
    let restored = snapshot(&h);
    assert_eq!(restored["search_focused"], true);
    assert_eq!(restored["search_caret_byte"], 0);
    assert_eq!(restored["local_search_text"], "");
}

#[test]
fn native_observer_reports_the_selected_category_and_layout_class() {
    let config = crate::prefs::test_env::ScratchConfig::new();
    let app = config.app().with_category(Category::Appearance);
    let app = ui::headless::Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport((1200.0, 760.0)),
        ui::Theme::dark(),
    );
    let doc = AutoCommit::new();
    let snapshot = app.native_input_snapshot(&doc, &root()).unwrap();
    assert_eq!(snapshot["ready"], true);
    assert_eq!(snapshot["selected_category"], "appearance");
    assert_eq!(snapshot["narrow"], false);
    assert_eq!(snapshot["sidebar_open"], true);
    assert_eq!(snapshot["nav_id"], crate::sidebar::NAV_ID);
}

fn keyboard_map_pressed(widget: &dyn Widget, key: PhysicalKey) -> Option<bool> {
    if let Some(map) = widget
        .as_any()
        .and_then(|widget| widget.downcast_ref::<ui::KeyboardMap>())
    {
        return Some(map.is_pressed(key));
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| keyboard_map_pressed(child, key))
}

fn dispatch_preview_key(h: &mut Framed<SettingsApp>, state: ElementState) {
    let logical = winit::keyboard::Key::Character("z".into());
    dispatch_preview_event(
        h,
        &WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            is_synthetic: false,
            event: winit::event::KeyEvent::synthetic(
                winit::event::RawKeyEvent {
                    physical_key: PhysicalKey::Code(winit::keyboard::KeyCode::KeyY),
                    state,
                },
                logical.clone(),
                Some("z".into()),
                winit::keyboard::KeyLocation::Standard,
                false,
                Some("z".into()),
                logical,
            ),
        },
    );
}

fn dispatch_preview_event(h: &mut Framed<SettingsApp>, event: &WindowEvent) {
    Framed::harness_mut(h).dispatch_to_root(event);
}

#[test]
fn keyboard_map_observes_foreground_physical_keys_while_search_keeps_editing() {
    let config = crate::prefs::test_env::ScratchConfig::new();
    let mut h = wide(&config);
    select(&mut h, Category::Keyboard);
    Framed::harness_mut(&mut h).click_named(SEARCH).unwrap();
    let compiled = crate::keyboard_preview::compilation_count();
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyY);
    let original_heads = heads(&mut h);
    assert_eq!(keyboard_map_pressed(&*h, key), Some(false));
    dispatch_preview_key(&mut h, ElementState::Pressed);
    assert_eq!(keyboard_map_pressed(&*h, key), Some(true));
    assert_eq!(h.search_state.with(Clone::clone), "z");
    assert_eq!(
        heads(&mut h),
        original_heads,
        "pressed positions stay ephemeral"
    );
    // Search invalidates the retained content. The same held physical position
    // survives that ordinary recomposition while editing remains focused.
    rebuild(&mut h);
    assert_eq!(keyboard_map_pressed(&*h, key), Some(true));
    assert_eq!(
        crate::keyboard_preview::compilation_count(),
        compiled,
        "search recomposition must retain the bounded compiled preview"
    );
    let snapshot = snapshot(&h);
    assert_eq!(snapshot["search_focused"], true);
    dispatch_preview_key(&mut h, ElementState::Released);
    assert_eq!(keyboard_map_pressed(&*h, key), Some(false));
    assert_eq!(h.search_state.with(Clone::clone), "z");
    dispatch_preview_key(&mut h, ElementState::Pressed);
    dispatch_preview_event(&mut h, &WindowEvent::Focused(false));
    assert_eq!(keyboard_map_pressed(&*h, key), Some(false));
    dispatch_preview_event(&mut h, &WindowEvent::Focused(true));
    dispatch_preview_key(&mut h, ElementState::Pressed);
    select(&mut h, Category::About);
    assert_eq!(keyboard_map_pressed(&*h, key), None);
    select(&mut h, Category::Keyboard);
    assert_eq!(keyboard_map_pressed(&*h, key), Some(false));
}

fn keyboard_map_symbols(
    widget: &dyn Widget,
    key: PhysicalKey,
) -> Option<Vec<winit::keyboard::KeyboardLayoutSymbol>> {
    if let Some(map) = widget
        .as_any()
        .and_then(|widget| widget.downcast_ref::<ui::KeyboardMap>())
    {
        return map.active_symbols(key);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| keyboard_map_symbols(child, key))
}

fn keyboard_map_value(widget: &dyn Widget) -> Option<String> {
    if let Some(map) = widget
        .as_any()
        .and_then(|widget| widget.downcast_ref::<ui::KeyboardMap>())
    {
        return map.accessibility()?.value;
    }
    widget.children().into_iter().find_map(keyboard_map_value)
}

#[test]
fn focused_seat_configuration_survives_page_recomposition_without_off_page_key_capture() {
    let config = crate::prefs::test_env::ScratchConfig::new();
    let mut h = wide(&config);
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyY);
    let snapshot = |text: &str| {
        std::sync::Arc::new(winit::keyboard::KeyboardLayout {
            depressed: 1,
            latched: 0,
            locked: 0,
            group: 1,
            keys: vec![winit::keyboard::KeyboardLayoutKey {
                physical: key,
                symbols: vec![winit::keyboard::KeyboardLayoutSymbol {
                    name: text.into(),
                    text: text.into(),
                }],
            }],
        })
    };
    let original = snapshot("z");
    h.observe_physical_input(ui::widget::PhysicalInput::Layout(Some(original)));
    crate::panes::mouse_keyboard::show_live_map(h.input());
    select(&mut h, Category::Keyboard);
    assert_eq!(keyboard_map_symbols(&*h, key).unwrap()[0].text, "z");
    let value = keyboard_map_value(&*h).unwrap();
    assert!(value.contains("AD06: z"));
    assert!(
        !value.contains("AD06: y"),
        "live accessibility must not claim the preferred US symbol"
    );
    select(&mut h, Category::About);
    let active = snapshot("Z");
    h.observe_physical_input(ui::widget::PhysicalInput::Layout(Some(active)));
    h.observe_physical_input(ui::widget::PhysicalInput::Key {
        key,
        state: ElementState::Pressed,
    });
    select(&mut h, Category::Keyboard);
    assert_eq!(keyboard_map_symbols(&*h, key).unwrap()[0].text, "Z");
    assert!(keyboard_map_value(&*h).unwrap().contains("AD06: Z"));
    assert_eq!(
        keyboard_map_pressed(&*h, key),
        Some(false),
        "off-page presses never enter the map"
    );
    let original_heads = heads(&mut h);
    h.observe_physical_input(ui::widget::PhysicalInput::Focused(false));
    assert_eq!(
        keyboard_map_symbols(&*h, key),
        None,
        "actual blur revokes native configuration"
    );
    let value = keyboard_map_value(&*h).unwrap();
    assert!(!value.contains("AD06: y"));
    assert!(!value.contains("AD06: Z"));
    assert_eq!(
        heads(&mut h),
        original_heads,
        "configuration and press feedback stay out of CRDT state"
    );
}

/// On a narrow page the keyboard map stays the page's prominent element and
/// still reads the seat's real symbols live through its own accessible value.
/// The redesigned page drops the separate key selector and details dump (this
/// test used to look for `input.map.inspect-key` and
/// `input.keyboard-map.details`); inspection is the map plus its action.
#[test]
fn narrow_keyboard_map_stays_prominent_and_live_readable() {
    fn find_rect(widget: &dyn Widget, id: &str) -> Option<vello::kurbo::Rect> {
        if widget.test_id() == Some(id) {
            return Some(widget.get_rect());
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find_rect(child, id))
    }
    let config = crate::prefs::test_env::ScratchConfig::new();
    let mut h = hosted(&config, 600.0, 1800.0);
    crate::panes::mouse_keyboard::show_live_map(h.input());
    select(&mut h, Category::Keyboard);
    let map = find_rect(&*h, "input.keyboard_map").unwrap();
    assert!(map.width() > 300.0, "the visual map remains prominent");
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyQ);
    for text in ["λ", "Λ"] {
        h.observe_physical_input(ui::widget::PhysicalInput::Layout(Some(
            std::sync::Arc::new(winit::keyboard::KeyboardLayout {
                depressed: 1,
                latched: 0,
                locked: 0,
                group: 1,
                keys: vec![winit::keyboard::KeyboardLayoutKey {
                    physical: key,
                    symbols: vec![winit::keyboard::KeyboardLayoutSymbol {
                        name: text.into(),
                        text: text.into(),
                    }],
                }],
            }),
        )));
        rebuild(&mut h);
        let value = keyboard_map_value(&*h).unwrap();
        assert!(
            value.contains(&format!("AD01: {text}")),
            "native symbols update the map's accessible value: {value}"
        );
        assert!(
            !value.contains("AD01: q"),
            "no preview fallback in live mode"
        );
    }
    h.observe_physical_input(ui::widget::PhysicalInput::Focused(false));
    rebuild(&mut h);
    assert!(!keyboard_map_value(&*h).unwrap().contains("AD01: Λ"));
}

#[test]
fn keyboard_map_read_matches_actual_focused_native_symbols_and_revokes_on_blur() {
    let config = crate::prefs::test_env::ScratchConfig::new();
    let mut h = wide(&config);
    crate::panes::mouse_keyboard::show_live_map(h.input());
    select(&mut h, Category::Keyboard);
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyQ);
    h.observe_physical_input(ui::widget::PhysicalInput::Focused(true));
    h.observe_physical_input(ui::widget::PhysicalInput::Layout(Some(
        std::sync::Arc::new(winit::keyboard::KeyboardLayout {
            depressed: 1,
            latched: 2,
            locked: 4,
            group: 1,
            keys: vec![winit::keyboard::KeyboardLayoutKey {
                physical: key,
                symbols: vec![winit::keyboard::KeyboardLayoutSymbol {
                    name: "Greek_LAMBDA".into(),
                    text: "Λ".into(),
                }],
            }],
        }),
    )));
    let original_heads = heads(&mut h);
    let actual = h.keyboard_inspection().unwrap();
    assert_eq!(actual["available"], true);
    assert_eq!(actual["mode"], "live");
    assert!(
        actual["source"].is_null(),
        "preferred/preview source is not live native evidence"
    );
    assert_eq!(actual["native_group"], 1);
    assert_eq!(actual["native_modifiers"], serde_json::json!([1, 2, 4]));
    assert_eq!(actual["selected_symbols"], serde_json::json!(["Λ"]));
    h.observe_physical_input(ui::widget::PhysicalInput::Key {
        key,
        state: ElementState::Pressed,
    });
    assert_eq!(
        h.keyboard_inspection().unwrap()["pressed_physical_keys"],
        serde_json::json!(["AD01"])
    );
    h.observe_physical_input(ui::widget::PhysicalInput::Key {
        key,
        state: ElementState::Released,
    });
    assert_eq!(
        h.keyboard_inspection().unwrap()["pressed_physical_keys"],
        serde_json::json!([])
    );
    h.observe_physical_input(ui::widget::PhysicalInput::Focused(false));
    let blurred = h.keyboard_inspection().unwrap();
    assert_eq!(blurred["available"], false);
    assert_eq!(blurred["native_layout_available"], false);
    assert_eq!(blurred["selected_symbols"], serde_json::json!([]));
    h.observe_physical_input(ui::widget::PhysicalInput::Focused(true));
    h.observe_physical_input(ui::widget::PhysicalInput::Layout(Some(
        std::sync::Arc::new(winit::keyboard::KeyboardLayout {
            depressed: 0,
            latched: 0,
            locked: 0,
            group: 0,
            keys: vec![],
        }),
    )));
    let missing = h.keyboard_inspection().unwrap();
    assert_eq!(
        missing["available"], false,
        "missing selected native position is not usable evidence"
    );
    assert_eq!(missing["selected_symbols"], serde_json::json!([]));
    assert_eq!(
        heads(&mut h),
        original_heads,
        "reading and physical feedback never edit the document"
    );
}
