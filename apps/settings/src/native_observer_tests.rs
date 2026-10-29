//! The native-input observer reads the app without touching its document.
//!
//! The snapshot a native audit asks for must describe the live state (the
//! selected category, the layout class, the document heads) without
//! committing the document's pending changes: the observer is a witness, not
//! a writer.

use crate::Inspected;
use crate::categories::Category;
use automerge::ChangeHash;
use automerge::transaction::Transactable as _;
use ui::widget::Widget;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::PhysicalKey;

/// Settings mounted in its view, as it runs: its pages are view code over
/// their sections, which a window outside the view does not show.
type App = ui::prelude::TestApp<crate::resource::SettingsShell>;

fn mounted(w: u32, h: u32) -> App {
    let mut app = App::new().expect("Settings mounts");
    app.harness().resize((w, h)).unwrap();
    app.settle();
    app
}

/// Whether this host draws a keyboard from installed native data at all. A
/// host without it draws the keys and their positions and prints no legend,
/// so every check below that needs a legend is only meaningful here.
fn draws_native_legends() -> bool {
    tilo_keyboard_map::is_installed("us").unwrap_or(false)
}

fn snapshot(app: &mut App) -> serde_json::Value {
    app.harness().native_input_snapshot().unwrap()
}

fn app_heads(app: &mut App) -> Vec<ChangeHash> {
    app.harness().state_mut().doc.get_heads()
}

/// Type `query` into the window bar's search, as the bar hands it to the
/// app: the search narrows the sidebar's list in place. Settings draws no
/// search field of its own.
fn bar_search(app: &mut App, search: &mut ui::widget::SearchDriver, query: &str) {
    let root = app.harness().root_widget_mut();
    if search.in_place() == ui::widget::InPlace::Nothing {
        search.open(ui::window_toolbar::SearchOffer::of(&mut *root).into());
    }
    assert!(
        search.drive(root, query, ui::widget::FindOptions::default(), None, false),
        "the bar's search reached the sidebar"
    );
    app.harness().relayout();
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

/// Whether the window shows the settings the bar's search found in place
/// of the pane: the content is titled for them.
fn shows_found(app: &mut App) -> bool {
    app.harness()
        .a11y_node_named(&crate::i18n::t("settings-found-title"))
        .is_some()
}

/// Whether the window shows the control of the setting `id`.
fn shows_setting(app: &mut App, id: &str) -> bool {
    fn walk(widget: &dyn Widget, setting: &settings_store::index::Setting) -> bool {
        widget.test_id().is_some_and(|id| setting.is_control(id))
            || widget
                .children()
                .into_iter()
                .any(|child| walk(child, setting))
    }
    let setting = settings_store::index::setting(id).expect("a setting Settings indexes");
    walk(app.harness().root_widget(), setting)
}

/// The window's shell: whether it is narrow, whether the list shows, and
/// the content screen's back arrow.
fn shell(app: &mut App) -> (bool, bool, vello::kurbo::Rect) {
    app.with_widget::<ui::widget::NavShell, _>(|shell| {
        (
            !shell.geometry().stack.is_empty(),
            shell.is_sidebar_open(),
            shell.geometry().back,
        )
    })
    .expect("settings shows its shell")
}

/// The native observer tracks search rebuilds/backspace and never commits pending changes.
#[test]
fn native_observer_tracks_search_without_committing_pending_changes() {
    // Case `native_observer_tracks_the_bar_search`.
    {
        let mut h = mounted(1200, 1800);
        let mut search = ui::widget::SearchDriver::default();
        let before = snapshot(&mut h);
        assert_eq!(before["ready"], true);
        assert_eq!(before["search_query"], "");
        assert_eq!(
            before["rendered_result_count"],
            sidebar_row_count(h.harness().root_widget())
        );
        let original_heads = app_heads(&mut h);
        bar_search(&mut h, &mut search, "z");
        let typed = snapshot(&mut h);
        assert_eq!(typed["search_query"], "z");
        assert_eq!(
            typed["rendered_result_count"],
            sidebar_row_count(h.harness().root_widget())
        );
        assert_ne!(
            typed["rendered_category_ids"],
            before["rendered_category_ids"]
        );
        bar_search(&mut h, &mut search, "");
        let after = snapshot(&mut h);
        assert_eq!(after["search_query"], "");
        assert_eq!(
            after["rendered_category_ids"],
            before["rendered_category_ids"]
        );
        assert_eq!(app_heads(&mut h), original_heads);
        assert_eq!(
            after["effective_document_heads"],
            before["effective_document_heads"]
        );
    }

    // Case `native_observer_does_not_commit_original_pending_changes`.
    {
        let mut h = mounted(1200, 760);
        let mut doc = automerge::AutoCommit::new();
        // A change made and not yet committed, as a field write leaves one.
        ui::tilo_resource::write_key(
            &mut doc,
            &automerge::ROOT,
            "diagnostic_test",
            &ui::tilo_resource::Atom("pending".into()),
        )
        .unwrap();
        let debug_before = format!("{doc:?}");
        let pending = doc.pending_ops();
        let (a, b) = h
            .with_widget::<Inspected, _>(|window| {
                (
                    window
                        .native_input_snapshot(&doc, &automerge::ROOT)
                        .unwrap(),
                    window
                        .native_input_snapshot(&doc, &automerge::ROOT)
                        .unwrap(),
                )
            })
            .expect("the window is mounted");
        assert_eq!(a, b);
        assert_eq!(a["original_pending_ops"], pending);
        assert_eq!(format!("{doc:?}"), debug_before);
        assert_eq!(doc.pending_ops(), pending);
        assert!(!a["effective_document_heads"].as_array().unwrap().is_empty());
        assert!(a.get("document_content").is_none());
    }
}

/// The innermost widget in `widget`'s tree that announces `text`.
fn announced_identity(widget: &dyn Widget, text: &str) -> Option<usize> {
    fn announces(node: &ui::a11y::AccessNode, text: &str) -> bool {
        node.name.as_deref() == Some(text)
            || node.children.iter().any(|child| announces(child, text))
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| announced_identity(child, text))
        .or_else(|| {
            widget
                .accessibility()
                .is_some_and(|node| announces(&node, text))
                .then(|| std::ptr::from_ref(widget).cast::<()>() as usize)
        })
}

#[test]
fn search_preserves_the_pane_but_category_switch_replaces_it() {
    let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    crate::effects::show_pane(&mut app, Category::About);
    let heading = crate::i18n::t("settings-about-problems-card");
    let about = announced_identity(app.harness().root_widget(), &heading).unwrap();
    // A query that finds no setting narrows the list and leaves the pane;
    // one that finds settings shows them instead.
    bar_search(&mut app, &mut ui::widget::SearchDriver::default(), "zqx");
    app.settle();
    assert!(!shows_found(&mut app));
    assert_eq!(
        announced_identity(app.harness().root_widget(), &heading),
        Some(about)
    );
    crate::effects::show_pane(&mut app, Category::Printers);
    assert_eq!(
        announced_identity(app.harness().root_widget(), &heading),
        None
    );
}

/// Searching from a narrow window's list narrows the list; what it finds
/// comes in front of it as the rows a wide window shows, and finding
/// nothing more goes back to the list.
#[test]
fn narrow_search_shows_what_it_finds_in_front_of_the_list() {
    let mut h = mounted(440, 820);
    let (narrow, _, back) = shell(&mut h);
    assert!(narrow);
    // Reach the list using the real narrow back control, not a screenshot override.
    assert!(back.width() > 0.0);
    h.harness().click_at(back.center().x, back.center().y);
    h.settle();
    assert!(shell(&mut h).1);
    let all = sidebar_row_count(h.harness().root_widget());
    let mut search = ui::widget::SearchDriver::default();
    bar_search(&mut h, &mut search, "zqx");
    let typed = snapshot(&mut h);
    assert_eq!(typed["search_query"], "zqx");
    assert!(!shows_found(&mut h));
    assert_eq!(
        typed["sidebar_open"], true,
        "finding no setting keeps the list"
    );
    assert!(sidebar_row_count(h.harness().root_widget()) < all);
    bar_search(&mut h, &mut search, "high contrast");
    let found = snapshot(&mut h);
    assert_eq!(found["narrow"], true);
    assert!(shows_found(&mut h));
    assert!(shows_setting(&mut h, "appearance.high_contrast"));
    assert_eq!(found["sidebar_open"], false, "what it found is in front");
    bar_search(&mut h, &mut search, "");
    let restored = snapshot(&mut h);
    assert_eq!(restored["search_query"], "");
    assert_eq!(
        restored["sidebar_open"], true,
        "clearing it goes back to the list"
    );
    assert_eq!(sidebar_row_count(h.harness().root_widget()), all);
}

#[test]
fn native_observer_reports_the_selected_category_and_layout_class() {
    let mut app = mounted(1200, 760);
    crate::effects::show_pane(&mut app, Category::Appearance);
    app.settle();
    let snapshot = snapshot(&mut app);
    assert_eq!(snapshot["ready"], true);
    assert_eq!(snapshot["selected_category"], "appearance");
    assert_eq!(snapshot["narrow"], false);
    assert_eq!(snapshot["sidebar_open"], true);
    assert_eq!(snapshot["nav_id"], crate::sidebar::NAV_ID);
}

/// `visit` the mounted window.
fn window<O>(app: &mut App, visit: impl FnOnce(&mut Inspected) -> O) -> O {
    app.with_widget::<Inspected, _>(visit)
        .expect("the window is mounted")
}

fn observe(app: &mut App, input: ui::widget::PhysicalInput) {
    window(app, |window| window.observe_physical_input(input));
}

/// Open `category`'s page as its sidebar row does.
fn show(app: &mut App, category: Category) {
    crate::effects::show_pane(app, category);
    app.settle();
    // The keyboard is drawn off the UI thread; the page shows it once it is.
    if category == Category::Keyboard {
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| {
                    keyboard_map_value(harness.root_widget()).is_some()
                }),
            "the keyboard is drawn"
        );
    }
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

fn dispatch_preview_key(h: &mut App, state: ElementState) {
    let logical = winit::keyboard::Key::Character("z".into());
    dispatch_preview_event(
        h,
        &ui::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
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
        )),
    );
}

fn dispatch_preview_event(h: &mut App, event: &WindowEvent) {
    h.harness().dispatch_to_root(event);
}

#[test]
fn keyboard_map_observes_foreground_physical_keys_while_the_list_is_narrowed() {
    let _env = tilo_test_env::lock();
    let mut h = mounted(1200, 1800);
    show(&mut h, Category::Keyboard);
    bar_search(&mut h, &mut ui::widget::SearchDriver::default(), "zqx");
    assert!(!shows_found(&mut h), "the keyboard pane stays");
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyY);
    let compiled = crate::keyboard_preview::compilation_count();
    let original_heads = app_heads(&mut h);
    assert_eq!(
        keyboard_map_pressed(h.harness().root_widget(), key),
        Some(false)
    );
    dispatch_preview_key(&mut h, ElementState::Pressed);
    if draws_native_legends() {
        assert_eq!(
            keyboard_map_pressed(h.harness().root_widget(), key),
            Some(true)
        );
    }
    assert_eq!(
        app_heads(&mut h),
        original_heads,
        "pressed positions stay ephemeral"
    );
    // The same held physical position survives an ordinary recomposition
    // while the bar's search keeps the list narrowed.
    h.harness().relayout();
    if draws_native_legends() {
        assert_eq!(
            keyboard_map_pressed(h.harness().root_widget(), key),
            Some(true)
        );
    }
    assert_eq!(
        crate::keyboard_preview::compilation_count(),
        compiled,
        "recomposition must retain the bounded compiled preview"
    );
    assert_eq!(
        h.harness().native_input_snapshot().unwrap()["search_query"],
        "zqx"
    );
    dispatch_preview_key(&mut h, ElementState::Released);
    assert_eq!(
        keyboard_map_pressed(h.harness().root_widget(), key),
        Some(false)
    );
    dispatch_preview_key(&mut h, ElementState::Pressed);
    dispatch_preview_event(&mut h, &WindowEvent::Focused(false));
    assert_eq!(
        keyboard_map_pressed(h.harness().root_widget(), key),
        Some(false)
    );
    dispatch_preview_event(&mut h, &WindowEvent::Focused(true));
    dispatch_preview_key(&mut h, ElementState::Pressed);
    show(&mut h, Category::About);
    assert_eq!(keyboard_map_pressed(h.harness().root_widget(), key), None);
    show(&mut h, Category::Keyboard);
    assert_eq!(
        keyboard_map_pressed(h.harness().root_widget(), key),
        Some(false)
    );
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
    let _env = tilo_test_env::lock();
    let mut h = mounted(1200, 1800);
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
    observe(&mut h, ui::widget::PhysicalInput::Layout(Some(original)));
    window(&mut h, |w| {
        crate::panes::mouse_keyboard::show_live_map(crate::panes::mouse_keyboard::area(w).unwrap());
    });
    show(&mut h, Category::Keyboard);
    assert_eq!(
        keyboard_map_symbols(h.harness().root_widget(), key).unwrap()[0].text,
        "z"
    );
    let value = keyboard_map_value(h.harness().root_widget()).unwrap();
    if draws_native_legends() {
        assert!(value.contains("AD06: z"));
    }
    assert!(
        !value.contains("AD06: y"),
        "live accessibility must not claim the preferred US symbol"
    );
    show(&mut h, Category::About);
    let active = snapshot("Z");
    observe(&mut h, ui::widget::PhysicalInput::Layout(Some(active)));
    observe(
        &mut h,
        ui::widget::PhysicalInput::Key {
            key,
            state: ElementState::Pressed,
        },
    );
    show(&mut h, Category::Keyboard);
    assert_eq!(
        keyboard_map_symbols(h.harness().root_widget(), key).unwrap()[0].text,
        "Z"
    );
    assert!(
        !draws_native_legends()
            || keyboard_map_value(h.harness().root_widget())
                .unwrap()
                .contains("AD06: Z")
    );
    assert_eq!(
        keyboard_map_pressed(h.harness().root_widget(), key),
        Some(false),
        "off-page presses never enter the map"
    );
    let original_heads = app_heads(&mut h);
    observe(&mut h, ui::widget::PhysicalInput::Focused(false));
    assert_eq!(
        keyboard_map_symbols(h.harness().root_widget(), key),
        None,
        "actual blur revokes native configuration"
    );
    let value = keyboard_map_value(h.harness().root_widget()).unwrap();
    assert!(!value.contains("AD06: y"));
    assert!(!value.contains("AD06: Z"));
    assert_eq!(
        app_heads(&mut h),
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
        Some(ui::widget::find_test_id(widget, id)?.get_rect())
    }
    let _env = tilo_test_env::lock();
    let mut h = mounted(440, 1800);
    window(&mut h, |w| {
        crate::panes::mouse_keyboard::show_live_map(crate::panes::mouse_keyboard::area(w).unwrap());
    });
    show(&mut h, Category::Keyboard);
    let map = find_rect(h.harness().root_widget(), "input.keyboard_map").unwrap();
    assert!(map.width() > 300.0, "the visual map remains prominent");
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyQ);
    for text in ["λ", "Λ"] {
        observe(
            &mut h,
            ui::widget::PhysicalInput::Layout(Some(std::sync::Arc::new(
                winit::keyboard::KeyboardLayout {
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
                },
            ))),
        );
        h.harness().relayout();
        let value = keyboard_map_value(h.harness().root_widget()).unwrap();
        assert!(
            !draws_native_legends() || value.contains(&format!("AD01: {text}")),
            "native symbols update the map's accessible value: {value}"
        );
        assert!(
            !value.contains("AD01: q"),
            "no preview fallback in live mode"
        );
    }
    observe(&mut h, ui::widget::PhysicalInput::Focused(false));
    h.harness().relayout();
    assert!(
        !keyboard_map_value(h.harness().root_widget())
            .unwrap()
            .contains("AD01: Λ")
    );
}

#[test]
fn keyboard_map_read_matches_actual_focused_native_symbols_and_revokes_on_blur() {
    let _env = tilo_test_env::lock();
    let mut h = mounted(1200, 1800);
    window(&mut h, |w| {
        crate::panes::mouse_keyboard::show_live_map(crate::panes::mouse_keyboard::area(w).unwrap());
    });
    show(&mut h, Category::Keyboard);
    let key = PhysicalKey::Code(winit::keyboard::KeyCode::KeyQ);
    observe(&mut h, ui::widget::PhysicalInput::Focused(true));
    observe(
        &mut h,
        ui::widget::PhysicalInput::Layout(Some(std::sync::Arc::new(
            winit::keyboard::KeyboardLayout {
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
            },
        ))),
    );
    let original_heads = app_heads(&mut h);
    let actual = window(&mut h, |w| w.keyboard_inspection()).unwrap();
    assert_eq!(actual["available"], draws_native_legends());
    assert_eq!(actual["mode"], "live");
    assert!(
        actual["source"].is_null(),
        "preferred/preview source is not live native evidence"
    );
    assert_eq!(actual["native_group"], 1);
    assert_eq!(actual["native_modifiers"], serde_json::json!([1, 2, 4]));
    assert_eq!(
        actual["selected_symbols"],
        if draws_native_legends() {
            serde_json::json!(["Λ"])
        } else {
            serde_json::json!([])
        }
    );
    observe(
        &mut h,
        ui::widget::PhysicalInput::Key {
            key,
            state: ElementState::Pressed,
        },
    );
    assert_eq!(
        window(&mut h, |w| w.keyboard_inspection()).unwrap()["pressed_physical_keys"],
        if draws_native_legends() {
            serde_json::json!(["AD01"])
        } else {
            serde_json::json!([])
        }
    );
    observe(
        &mut h,
        ui::widget::PhysicalInput::Key {
            key,
            state: ElementState::Released,
        },
    );
    assert_eq!(
        window(&mut h, |w| w.keyboard_inspection()).unwrap()["pressed_physical_keys"],
        serde_json::json!([])
    );
    observe(&mut h, ui::widget::PhysicalInput::Focused(false));
    let blurred = window(&mut h, |w| w.keyboard_inspection()).unwrap();
    assert_eq!(blurred["available"], false);
    assert_eq!(blurred["native_layout_available"], false);
    assert_eq!(blurred["selected_symbols"], serde_json::json!([]));
    observe(&mut h, ui::widget::PhysicalInput::Focused(true));
    observe(
        &mut h,
        ui::widget::PhysicalInput::Layout(Some(std::sync::Arc::new(
            winit::keyboard::KeyboardLayout {
                depressed: 0,
                latched: 0,
                locked: 0,
                group: 0,
                keys: vec![],
            },
        ))),
    );
    let missing = window(&mut h, |w| w.keyboard_inspection()).unwrap();
    assert_eq!(
        missing["available"], false,
        "missing selected native position is not usable evidence"
    );
    assert_eq!(missing["selected_symbols"], serde_json::json!([]));
    assert_eq!(
        app_heads(&mut h),
        original_heads,
        "reading and physical feedback never edit the document"
    );
}
