//! The pointing, keyboard and spelling pages' behaviour, driven through a
//! mounted settings app.

use super::tablet::*;
use super::*;
use crate::effects::SettingsEffect;
use crate::host::testing::{INPUT, TEXT_ASSISTANCE, edit_field};
use crate::resource::SettingsShell;
use ui::prelude::TestApp;
use winit::event::WindowEvent;

#[test]
fn mounted_dictionary_actions_add_edit_remove_reset_and_survive_restart() {
    use settings_store::personal_dictionary::{Service, Vocabulary, host::Host};
    use std::sync::Arc;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ui::local_identity::install_source(None);
        }
    }
    let _guard = tilo_test_env::lock();
    let _restore = Restore;
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: [1; 32],
            display_name: String::new(),
            avatar_color: [0; 3],
        },
    ))));
    let profile = tempfile::tempdir().unwrap();
    let service = Arc::new(Service::new(profile.path().join("dictionary")).unwrap());
    let source = Arc::new(Host::new(
        Arc::new(settings_store::HostDocument::new(
            serde_json::Map::new(),
            |_| Ok(()),
        )),
        service.clone(),
        Arc::new(|| Some([1; 32])),
    ));
    let mut app = TestApp::<SettingsShell>::for_person(&"01".repeat(32)).unwrap();
    let mut host =
        ui::personal_dictionary::TestHost::new(source, "so.tilo.settings", [1; 32]).unwrap();
    app.harness().resize((1200, 2200)).unwrap();
    crate::effects::show_pane(&mut app, crate::categories::Category::Spelling);
    app.settle();
    app.harness().click_named("dictionary.word").unwrap();
    app.harness().type_text("Tilo");
    app.harness().click_named("dictionary.add").unwrap();
    app.settle();
    host.flush().unwrap();
    app.settle();
    let reader = service.reader().unwrap();
    assert_eq!(
        reader.snapshot().unwrap().hints("en").collect::<Vec<_>>(),
        ["Tilo"]
    );
    app.effect(&SettingsEffect::from(InputEffect::EditDictionaryWord {
        old_word: "Tilo".into(),
        old_language: String::new(),
        word: "TiLo".into(),
        language: "nl".into(),
        revision: 1,
    }))
    .unwrap();
    host.flush().unwrap();
    app.settle();
    assert_eq!(
        reader.snapshot().unwrap().hints("nl").collect::<Vec<_>>(),
        ["TiLo"]
    );
    let reopened = Service::new(profile.path().join("dictionary")).unwrap();
    assert_eq!(
        reopened.bind(Some([1; 32])).unwrap().snapshot().unwrap(),
        reader.snapshot().unwrap()
    );
    if let Some(path) = std::env::var_os("TILO_DICTIONARY_LIVE_SCREENSHOT") {
        app.harness().render_png(std::path::Path::new(&path));
    }
    app.harness().click_named("dictionary.0.remove").unwrap();
    app.settle();
    host.flush().unwrap();
    app.settle();
    assert!(reader.snapshot().unwrap().words.is_empty());
    app.effect(&SettingsEffect::from(InputEffect::AddDictionaryWord {
        word: "Élodie".into(),
        language: "fr".into(),
        revision: 3,
    }))
    .unwrap();
    host.flush().unwrap();
    app.settle();
    app.harness().click_named("dictionary.reset").unwrap();
    app.settle();
    host.flush().unwrap();
    assert!(reader.snapshot().unwrap().words.is_empty());
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::NoLocalIdentity)));
    app.settle();
    assert!(ui::personal_dictionary::snapshot().is_err());
    assert!(!app.harness().targets().contains_key("dictionary.word"));
    assert!(!app.harness().targets().contains_key("dictionary.0.word"));
}

#[test]
fn personal_dictionary_editor_renders_preserved_words_and_clear_controls() {
    use settings_store::personal_dictionary::{Edit, Service, Vocabulary, Word};
    let profile = tempfile::tempdir().unwrap();
    let service = Service::new(profile.path().join("dictionary")).unwrap();
    let reader = service.bind(Some([1; 32])).unwrap();
    for (word, language) in [("Tilo", None), ("Élodie", Some("fr")), ("東京", Some("ja"))] {
        let revision = reader.snapshot().unwrap().revision;
        service
            .edit(
                &reader,
                revision,
                Edit::Add(Word::new(word, language).unwrap()),
            )
            .unwrap();
    }
    let cx = PaneCx::in_file(profile.path().join("settings.json"));
    let area = InputArea::new(&cx);
    let mut pane = MouseKeyboardPane::new(cx, &area);
    pane.dictionary = Some(reader.snapshot().unwrap());
    let frame = ui::headless::FrameHarness::new(
        ui::State::new(),
        crate::widgets::page(vec![pane.dictionary_card()]),
        (1100, 760),
    );
    for control in [
        "dictionary.word",
        "dictionary.language",
        "dictionary.add",
        "dictionary.0.save",
        "dictionary.0.remove",
        "dictionary.reset",
    ] {
        assert!(
            frame.targets().contains_key(control),
            "missing shared dictionary control: {control}"
        );
    }
    if let Some(path) = std::env::var_os("TILO_DICTIONARY_SCREENSHOT") {
        frame.render_png(std::path::Path::new(&path));
    }
}

fn picker_node(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some("input.source.add_picker") {
        return Some(node);
    }
    node.children.iter().find_map(picker_node)
}

fn spelling_language_value(node: &ui::headless::InspectionNode) -> Option<&str> {
    if let Some(semantics) = &node.semantics
        && semantics.role == ui::a11y::Role::ComboBox
        && semantics.name.as_deref() == Some(t("input-spell-language").as_str())
    {
        return semantics.value.as_deref();
    }
    node.children.iter().find_map(spelling_language_value)
}

pub(super) fn show_add_source(app: &mut TestApp<SettingsShell>, viewport: (u32, u32)) {
    crate::effects::show_pane(app, crate::categories::Category::Keyboard);
    app.settle();
    // The layouts to add are read off the UI thread; the chooser offers
    // them once the read has landed.
    tilo_test_env::wait_until("the device's layouts are read", || {
        app.settle();
        app.with_widget::<crate::settings_app::SettingsApp, _>(|settings| {
            !crate::panes::mouse_keyboard::area(settings)
                .unwrap()
                .catalog_pending()
        }) == Some(true)
    });
    for _ in 0..5 {
        app.settle();
        let _ = app.harness().poll_frame();
    }
    for _ in 0..30 {
        app.settle();
        let rect = app
            .harness()
            .targets()
            .get("input.source.add_picker")
            .copied()
            .unwrap();
        if rect.y0 >= 0.0 && rect.y1 <= f64::from(viewport.1) {
            return;
        }
        app.harness().send(&ui::synthetic_input::cursor_moved(
            f64::from(viewport.0) - 60.0,
            f64::from(viewport.1) / 2.0,
        ));
        app.harness().scroll(0.0, -3.0);
    }
    panic!("the actual Settings input-source chooser must become visible by ordinary scrolling");
}

#[test]
fn dropdown_language_mounted_settings_flow_is_clear_and_keyboard_operable() {
    // The pane follows the identity the process has bound, which the
    // dictionary test swaps: a swap mid-test rebuilds the page under it.
    let _env = tilo_test_env::lock();
    let mut failures = Vec::new();
    for (name, viewport, theme) in [
        ("light-wide", (1472, 920), Theme::light()),
        ("dark-narrow", (420, 640), Theme::dark().with_ui_scale(1.5)),
        (
            "contrast-narrow",
            (420, 640),
            Theme::high_contrast().with_ui_scale(1.5),
        ),
    ] {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.harness().resize(viewport).unwrap();
        app.harness().set_theme(theme);
        crate::effects::show_pane(&mut app, crate::categories::Category::Spelling);
        app.settle();
        assert_eq!(
            spelling_language_value(&app.harness().inspection().root),
            Some(t("input-spell-language-session").as_str()),
            "the valid empty stable value must show its human language label",
        );
        let active_before = app.settings::<InputPrefs>().get().keyboard_layout;
        show_add_source(&mut app, viewport);
        let snapshot = app.harness().inspection();
        let control = picker_node(&snapshot.root)
            .unwrap()
            .semantics
            .as_ref()
            .unwrap();
        if control.value.is_some() {
            failures.push(format!(
                "{name}: uninstalled source looks selected: {:?}",
                control.value
            ));
        }
        let out = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("dropdown-review")
            .join(name);
        app.harness().render_png(&out.join("closed.png"));
        app.harness()
            .click_named("input.source.add_picker")
            .unwrap();
        app.settle();
        app.harness().render_png(&out.join("open.png"));
        app.harness().type_text("zzzz");
        app.harness().render_png(&out.join("no-matches.png"));
        app.harness().key("Escape");
        app.settle();
        app.harness()
            .click_named("input.source.add_picker")
            .unwrap_or_else(|error| {
                panic!(
                    "{name}: the chooser is still on the page after Escape closed its list \
                     ({error}); showing {:?}",
                    app.with_widget::<crate::settings_app::SettingsApp, _>(|settings| (
                        settings.selected(),
                        settings.sidebar_open()
                    ))
                )
            });
        app.settle();
        app.harness().type_text("French");
        app.harness().render_png(&out.join("filtered.png"));
        app.harness().key("Return");
        app.settle();
        tilo_test_env::wait_until("normal typed AddSource completes", || {
            app.settle();
            app.settings::<InputPrefs>()
                .get()
                .keyboard_layouts
                .contains(&"fr".into())
        });
        let prefs = app.settings::<InputPrefs>().get();
        assert_ne!(
            prefs.keyboard_layout, active_before,
            "adding a layout from the page types with it"
        );
        app.harness().render_png(&out.join("selected.png"));
        assert!(app.harness().component_contract_violations().is_empty());
        let audit = app.harness().native_app_audit();
        std::fs::write(
            out.join("native-audit.json"),
            serde_json::to_vec_pretty(&audit).unwrap(),
        )
        .unwrap();
        assert!(audit.is_certified(), "{name}: {audit:?}");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn pane_with(input: &InputPrefs, catalog: Vec<CatalogEntry>) -> MouseKeyboardPane {
    let profile = tempfile::tempdir().unwrap();
    let cx = PaneCx::in_file(profile.path().join("settings.json"));
    cx.settings::<InputPrefs>().set(input).unwrap();
    let area = InputArea {
        catalog: SourceCatalog::ready(catalog, None),
        live: std::cell::Cell::new(false),
        devices: std::rc::Rc::new(RefCell::new((None, None))),
        following: RefCell::new(std::rc::Weak::new()),
        editor: ui::LocalState::new(keyboard_editor::EditorState {
            view: if cx.scene().is_some() {
                keyboard_editor::SourceView::Configured
            } else {
                keyboard_editor::SourceView::Native
            },
            ..Default::default()
        }),
        osk_always: false,
        document: cx.storage().settings_document().clone(),
    };
    MouseKeyboardPane::new(cx, &area)
}

/// A shown pane reads the attached devices when the compositor's
/// snapshot changes, never on a timer; only a snapshot that cannot be
/// watched is read once a second.
#[test]
fn attached_devices_are_read_when_their_snapshot_changes_not_on_a_timer() {
    let long_ago = || {
        ui::time::now()
            .checked_sub(std::time::Duration::from_secs(5))
            .unwrap()
    };
    let mut pane = pane_with(&InputPrefs::default(), catalog());
    pane.hardware_live = true;
    let watch = std::rc::Rc::new(crate::watch::Watched::by_hand());
    pane.devices_watch = Some(std::rc::Rc::clone(&watch));
    assert!(pane.devices_due(), "the first sync reads them");
    assert!(!pane.devices_due());
    pane.next_device_poll = Some(long_ago());
    assert!(!pane.devices_due(), "an idle pane reads nothing on a timer");
    assert_eq!(pane.next_wake_deadline(), None, "nor sets a deadline");
    watch.touch();
    assert!(pane.devices_due(), "a changed snapshot is read");
    assert!(!pane.devices_due(), "once");

    let root = tempfile::tempdir().unwrap();
    pane.devices_watch = Some(std::rc::Rc::new(crate::watch::Watched::files(&[root
        .path()
        .join("absent")
        .join("input-state.json")])));
    assert!(pane.next_wake_deadline().is_some());
    pane.next_device_poll = Some(long_ago());
    assert!(
        pane.devices_due(),
        "an unwatchable snapshot is read each second"
    );
}

/// The area watches the file the compositor publishes the attached
/// devices in, and every pane showing them shares that one watch.
#[test]
fn the_input_area_watches_the_compositors_input_snapshot() {
    let runtime = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let snapshot = settings_store::hardware::input_snapshot_path().unwrap();
    std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    let pane = pane_with(&InputPrefs::default(), catalog());
    let area = InputArea {
        catalog: SourceCatalog::ready(catalog(), None),
        live: std::cell::Cell::new(true),
        devices: std::rc::Rc::new(RefCell::new((None, None))),
        following: RefCell::new(std::rc::Weak::new()),
        editor: ui::LocalState::new(keyboard_editor::EditorState::default()),
        osk_always: false,
        document: pane.cx.storage().settings_document().clone(),
    };
    let watch = area.follow().expect("a live area follows its devices");
    assert!(watch.is_complete());
    assert!(std::rc::Rc::ptr_eq(&watch, &area.follow().unwrap()));
    assert!(watch.take(), "the first look reads the snapshot");
    InputHardwareSnapshot::default().save().unwrap();
    tilo_test_env::wait_until("a published snapshot to be flagged", || watch.take());
}

fn catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry {
            id: "us".into(),
            label: "English (US)".into(),
        },
        CatalogEntry {
            id: "de".into(),
            label: "German".into(),
        },
        CatalogEntry {
            id: "fr".into(),
            label: "French".into(),
        },
    ]
}

/// The pane lays out and imported repeat settings show the live rate or disabled.
#[test]
#[expect(
    clippy::float_cmp,
    reason = "off, pass-through and clamped slider values are exact sentinels, not computed"
)]
fn input_pane_lays_out_and_shows_imported_repeat_settings() {
    {
        let mut pane = pane_with(&InputPrefs::default(), catalog());
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    {
        let disabled = pane_with(
            &InputPrefs {
                key_repeat: 0.5,
                key_repeat_delay_ms: Some(500),
                key_repeat_rate_hz: Some(0),
                ..Default::default()
            },
            catalog(),
        );
        assert_eq!(disabled.key_repeat_slider_value(), 0.0);
        assert_eq!(
            disabled.key_repeat_description(),
            "Repeat is off. Move the slider to turn it on."
        );

        let exact = pane_with(
            &InputPrefs {
                key_repeat: 0.5,
                key_repeat_delay_ms: Some(275),
                key_repeat_rate_hz: Some(37),
                ..Default::default()
            },
            catalog(),
        );
        assert!((exact.key_repeat_slider_value() - 0.675).abs() < f64::EPSILON);
        assert_eq!(
            exact.key_repeat_description(),
            "37 keys per second, starts after 275 ms"
        );

        let partial = pane_with(
            &InputPrefs {
                key_repeat: 0.25,
                key_repeat_rate_hz: Some(50),
                ..Default::default()
            },
            catalog(),
        );
        assert_eq!(partial.key_repeat_slider_value(), 0.25);
        assert_eq!(
            partial.key_repeat_description(),
            "Faster repeat starts sooner"
        );

        let clamped = pane_with(
            &InputPrefs {
                key_repeat_delay_ms: Some(200),
                key_repeat_rate_hz: Some(1_000),
                ..Default::default()
            },
            catalog(),
        );
        assert_eq!(clamped.key_repeat_slider_value(), 1.0);
    }
}

#[test]
fn pointer_sections_follow_the_attached_devices() {
    let with = |kinds: &[InputDeviceKind]| InputHardwareSnapshot {
        devices: kinds
            .iter()
            .map(|kind| settings_store::hardware::InputDeviceSnapshot {
                kind: *kind,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let everything = InputSections::for_devices(None);
    assert!(everything.touchpad && everything.mouse && everything.pointer);

    let desktop = InputSections::for_devices(Some(&with(&[
        InputDeviceKind::Mouse,
        InputDeviceKind::Keyboard,
    ])));
    assert!(!desktop.touchpad, "no touchpad, no touchpad settings");
    assert!(desktop.mouse && desktop.pointer);
    assert!(!desktop.tablet);

    let laptop = InputSections::for_devices(Some(&with(&[InputDeviceKind::Touchpad])));
    assert!(laptop.touchpad && laptop.pointer && !laptop.mouse);

    let keyboard_only = InputSections::for_devices(Some(&with(&[InputDeviceKind::Keyboard])));
    assert!(
        !keyboard_only.pointer,
        "buttons need a pointer to belong to"
    );

    let pad_only = InputSections::for_devices(Some(&with(&[InputDeviceKind::TabletPad])));
    assert!(pad_only.tablet);
}

#[test]
fn tablet_and_pad_choices_travel_as_stable_ids() {
    let actions = [
        StylusButtonAction::Default,
        StylusButtonAction::Right,
        StylusButtonAction::Disabled,
        StylusButtonAction::Shortcut(ShortcutAction::Search),
    ];
    for action in actions {
        assert_eq!(
            stylus_button_for_id(&stylus_button_id(action)),
            Some(action)
        );
    }
    assert_eq!(
        stylus_button_id(StylusButtonAction::Shortcut(
            ShortcutAction::WorkspaceOverview
        )),
        "shortcut:workspace_overview"
    );
    assert_eq!(stylus_button_for_id("Right click"), None);
    let choices = stylus_button_choices();
    assert_eq!(choices[3], ("right".into(), "Right click".into()));
    assert!(
        choices.iter().any(|(id, _)| id == "shortcut:search"),
        "every listed shortcut can go on a pen button"
    );

    assert_eq!(pad_button_for_id("none"), Some(PadButtonChoice::Nothing));
    let mut pad = TabletConfig::default();
    set_pad_button(&mut pad, 2, PadButtonChoice::Shortcut(ShortcutAction::Lock));
    assert_eq!(
        pad_button_for_id(&pad_button_id(&pad, 2)),
        Some(PadButtonChoice::Shortcut(ShortcutAction::Lock))
    );
    set_pad_button(&mut pad, 2, PadButtonChoice::ShowMap);
    assert_eq!(pad_button_id(&pad, 2), "show-map");
    assert!(
        pad.pad_buttons.is_empty(),
        "the map button runs no shortcut"
    );
    set_pad_button(&mut pad, 0, PadButtonChoice::ShowMap);
    assert_eq!(pad.pad_map_button, Some(0), "one button shows the map");
    assert_eq!(pad_button_id(&pad, 2), "none");
    assert_eq!(pad_button_for_id("left"), None, "a pad runs shortcuts only");

    for id in ["native", "left", "back", "shortcut:search"] {
        let action = eraser_for_id(id).expect("an offered eraser choice");
        assert_eq!(eraser_id(action), id);
    }
    assert!(
        eraser_choices()
            .iter()
            .any(|(id, _)| id == "shortcut:search"),
        "every listed shortcut can go on the eraser"
    );
    assert_eq!(eraser_for_id("disabled"), None);
    for (id, _) in dial_choices() {
        let action = dial_for_id(&id).expect("an offered dial choice");
        assert_eq!(dial_id(action), id);
    }
    assert_eq!(dial_for_id("shortcut:search"), None);

    assert_eq!(tablet_output_for_id(WHOLE_DESKTOP), Ok(None));
    assert_eq!(tablet_output_choices()[0].0, WHOLE_DESKTOP);
}

#[test]
fn each_tablet_is_one_card_holding_its_own_pad_buttons() {
    let _cfg = crate::prefs::test_env::ConfigGuard::new();
    let device = |kind, product, name: &str, group: &str, pad_buttons| {
        settings_store::hardware::InputDeviceSnapshot {
            key: settings_store::hardware::input_device_key(0x056a, product, name),
            name: name.into(),
            kind,
            pad_buttons,
            physical_device: group.into(),
            ..Default::default()
        }
    };
    let snapshot = InputHardwareSnapshot {
        devices: vec![
            device(
                InputDeviceKind::Tablet,
                0x0374,
                "Wacom Intuos S Pen",
                "event4",
                0,
            ),
            device(
                InputDeviceKind::Tablet,
                0x0357,
                "Wacom Intuos Pro M Pen",
                "event7",
                0,
            ),
            // Listed after both pens, in the other order: each pad still
            // belongs to its own tablet.
            device(
                InputDeviceKind::TabletPad,
                0x0357,
                "Wacom Intuos Pro M Pad",
                "event7",
                8,
            ),
            device(
                InputDeviceKind::TabletPad,
                0x0374,
                "Wacom Intuos S Pad",
                "event4",
                4,
            ),
            // A remote is a pad no tablet has: a device of its own.
            device(
                InputDeviceKind::TabletPad,
                0x0331,
                "Wacom ExpressKey Remote Pad",
                "event9",
                18,
            ),
        ],
        ..Default::default()
    };
    let mut pane = pane_with(&InputPrefs::default(), catalog());
    pane.observe_devices(Some(&snapshot));
    let cards = pane.tablet_cards();
    assert_eq!(cards.len(), 3, "one card per device, never one per pad");

    let said: Vec<Vec<String>> = cards
        .into_iter()
        .map(|card| crate::widgets::announced_text(crate::widgets::page(vec![card]).as_ref()))
        .collect();
    let says = |card: &[String], needle: &str| card.iter().any(|line| line == needle);
    let pad_buttons = |card: &[String]| {
        (1..=32)
            .filter(|number| says(card, &format!("Pad button {number}")))
            .count()
    };
    assert!(says(&said[0], "Wacom Intuos S"), "{:#?}", said[0]);
    assert!(says(&said[0], &t("input-tablet-map-to")), "{:#?}", said[0]);
    assert_eq!(pad_buttons(&said[0]), 4, "{:#?}", said[0]);
    assert!(says(&said[1], "Wacom Intuos Pro M"), "{:#?}", said[1]);
    assert_eq!(pad_buttons(&said[1]), 8, "{:#?}", said[1]);
    assert!(says(&said[2], "Wacom ExpressKey Remote"), "{:#?}", said[2]);
    assert!(!says(&said[2], &t("input-tablet-map-to")), "{:#?}", said[2]);
    assert_eq!(pad_buttons(&said[2]), 18, "{:#?}", said[2]);
    assert!(
        said.iter()
            .flatten()
            .all(|line| !line.ends_with(" buttons")),
        "no card is titled as a separate pad"
    );
}

#[test]
fn a_card_appears_for_each_attached_tablet() {
    let _cfg = crate::prefs::test_env::ConfigGuard::new();
    let device = |kind, name: &str, pad_buttons| settings_store::hardware::InputDeviceSnapshot {
        key: settings_store::hardware::input_device_key(0x056a, 0x0374, name),
        name: name.into(),
        kind,
        pad_buttons,
        ..Default::default()
    };
    let mut pad = device(InputDeviceKind::TabletPad, "Wacom Intuos S Pad", 4);
    pad.pad_rings = vec![4];
    pad.pad_strips = vec![1, 1];
    pad.pad_mode_buttons = vec![0];
    let snapshot = InputHardwareSnapshot {
        devices: vec![
            device(InputDeviceKind::Tablet, "Wacom Intuos S Pen", 0),
            pad,
            device(InputDeviceKind::Mouse, "Mouse", 0),
        ],
        ..Default::default()
    };
    let mut pane = pane_with(&InputPrefs::default(), catalog());
    pane.observe_devices(Some(&snapshot));
    assert!(pane.take_structure_dirty(), "new devices rebuild the page");
    assert_eq!(pane.tablets.len(), 1);
    assert_eq!(pane.pads.len(), 1);
    assert_eq!(device_title(&pane.tablets[0]), "Wacom Intuos S");
    assert_eq!(device_title(&pane.pads[0]), "Wacom Intuos S buttons");
    assert_eq!(
        pane.tablet_cards().len(),
        1,
        "the pen and pad of one tablet are one card"
    );
    assert_eq!(
        pad_dials(&pane.pads[0]),
        [
            (PadDial::Ring(0), 4),
            (PadDial::Strip(0), 1),
            (PadDial::Strip(1), 1)
        ],
        "a row per ring mode and per strip"
    );
    assert_eq!(dial_label(PadDial::Ring(0), 1, 4), "Ring 1, mode 2");
    assert_eq!(dial_label(PadDial::Strip(1), 0, 1), "Strip 2");
    pane.observe_devices(Some(&snapshot));
    assert!(
        !pane.take_structure_dirty(),
        "nothing changed, nothing rebuilt"
    );

    let mut theme_taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let _ = pane.build_layout(&mut theme_taffy, &Theme::dark(), &mut node_map);

    pane.observe_devices(Some(&InputHardwareSnapshot::default()));
    assert!(pane.take_structure_dirty(), "unplugging removes the cards");
    assert!(pane.tablets.is_empty() && pane.pads.is_empty());
}

#[test]
fn touchpad_choices_travel_as_stable_ids() {
    for method in TouchpadClickMethod::ALL {
        assert_eq!(click_method_for_id(click_method_id(method)), Some(method));
    }
    for method in TouchpadScrollMethod::ALL {
        assert_eq!(scroll_method_for_id(scroll_method_id(method)), Some(method));
    }
    assert_eq!(click_method_for_id("Button areas"), None);
    let labels = localized_choices(&CLICK_METHOD_CHOICES);
    assert_eq!(labels[1], ("areas".into(), "Bottom-right corner".into()));
}

#[test]
fn keyboard_preview_actions_are_published_and_do_not_apply_until_requested() {
    use ui::tilo_resource::Effect as _;
    // The pane follows the identity the process has bound, which the
    // dictionary test swaps: a swap mid-test rebuilds the page under it.
    let _env = tilo_test_env::lock();
    let mut app = TestApp::<SettingsShell>::new().unwrap();
    let before = app.settings::<InputPrefs>().get();
    let preview = InputEffect::PreviewLayout {
        source: "de".into(),
    };
    let model = InputEffect::SetKeyboardModel {
        model: "pc104".into(),
    };
    let levels = InputEffect::SetPreviewExtraLevels { on: true };
    let effects = [
        preview.clone(),
        model.clone(),
        levels.clone(),
        InputEffect::SetPreviewLevel { level: "1".into() },
        InputEffect::ShowLiveKeyboardMap,
        InputEffect::ApplyPreview,
    ];
    let actions = app.actions().unwrap();
    for effect in &effects {
        assert!(
            actions
                .iter()
                .any(|action| action.address.action == effect.id()),
            "the actual Settings publisher must advertise {}",
            effect.id()
        );
    }
    assert!(set(&mut app, preview));
    assert!(set(&mut app, model));
    assert!(set(&mut app, levels));
    let inspected = app.settings::<InputPrefs>().get();
    assert_eq!(inspected.keyboard_layout, before.keyboard_layout);
    assert_eq!(inspected.keyboard_layouts, before.keyboard_layouts);
    assert_eq!(inspected.keyboard_model, "pc104");
    assert_eq!(inspected.keyboard_preview.as_deref(), Some("de"));
    assert!(inspected.keyboard_preview_extra_levels);
    assert!(set(&mut app, InputEffect::ApplyPreview));
    assert_eq!(app.settings::<InputPrefs>().get().keyboard_layout, "de");
    assert!(!set(
        &mut app,
        InputEffect::PreviewLayout {
            source: "../../private".into()
        }
    ));
    assert!(!set(
        &mut app,
        InputEffect::SetKeyboardModel {
            model: "../../private".into()
        }
    ));
    assert!(!set(
        &mut app,
        InputEffect::SetPreviewLevel { level: "99".into() }
    ));
    // A level is chosen only for the levels the drawn keyboard supplies. A
    // host with no installed keyboard data draws the keys without legends,
    // so it supplies none and refuses every level.
    let drawn = app
        .call_on(
            "settings.keyboard-map",
            InputEffect::InspectKeyboardMap.id(),
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap()["selected_symbols"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let level = InputEffect::SetPreviewLevel { level: "1".into() };
    assert_eq!(
        set(&mut app, level),
        drawn > 0,
        "a level the drawn keyboard supplies is chosen"
    );
    assert_eq!(
        app.settings::<InputPrefs>().get().keyboard_preview_level,
        usize::from(drawn > 0)
    );
    assert!(set(&mut app, InputEffect::ShowLiveKeyboardMap));
    let input = app.settings::<InputPrefs>().get();
    assert!(input.keyboard_preview.is_none());
    assert_eq!(input.keyboard_preview_level, 0);
    assert_eq!(input.keyboard_layout, "de");
}

#[test]
fn keyboard_map_read_is_published_as_a_read_and_never_changes_preferences() {
    use ui::tilo_resource::Effect as _;
    // The pane follows the identity the process has bound, which the
    // dictionary test swaps: a swap mid-test rebuilds the page under it.
    let _env = tilo_test_env::lock();
    let mut app = TestApp::<SettingsShell>::with_role(ui::permissions::Role::Viewer).unwrap();
    let effect = InputEffect::InspectKeyboardMap;
    let actions = app.actions().unwrap();
    let action = actions
        .iter()
        .find(|action| action.address.action == effect.id())
        .unwrap();
    assert_eq!(action.contract.kind, ui::tilo_actions::ActionKind::Read);
    let address = action.address.clone();
    let before = app.settings::<InputPrefs>().get();
    let value = app
        .call_on(&address.resource, &address.action, serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(value["page_visible"], false);
    assert_eq!(value["pressed_physical_keys"], serde_json::json!([]));
    assert_eq!(app.settings::<InputPrefs>().get(), before);
}

#[test]
fn keyboard_map_read_uses_the_mounted_preview_and_live_key_release_state() {
    use ui::tilo_resource::Effect as _;
    // The pane follows the identity the process has bound, which the
    // dictionary test swaps: a swap mid-test rebuilds the page under it.
    let _env = tilo_test_env::lock();
    let mut app = TestApp::<SettingsShell>::new().unwrap();
    let read = |app: &mut TestApp<SettingsShell>| {
        app.call_on(
            "settings.keyboard-map",
            InputEffect::InspectKeyboardMap.id(),
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap()
    };
    crate::effects::show_pane(&mut app, crate::categories::Category::Keyboard);
    assert!(set(
        &mut app,
        InputEffect::PreviewLayout {
            source: "de".into()
        }
    ));
    assert!(set(
        &mut app,
        InputEffect::InspectKeyboardKey { key: "AD06".into() }
    ));
    let before = app.settings::<InputPrefs>().get();
    app.settle();
    app.harness().dispatch_to_root(&WindowEvent::Focused(true));
    let preview = read(&mut app);
    assert_eq!(preview["mode"], "preview");
    assert_eq!(preview["available"], true);
    assert_eq!(preview["source"], "de");
    assert_eq!(preview["inspected_key"], "AD06");
    // The read reports the legends of the layout the preview draws. A host
    // with no installed keyboard data draws the keys without them, and the
    // preferred layout's "z" is never reported in their place.
    let symbols = preview["selected_symbols"].as_array().unwrap();
    let without_keyboard_data = preview["native_layout_available"] == false;
    assert!(
        symbols.iter().any(|symbol| symbol == "z")
            || (without_keyboard_data && symbols.iter().all(|symbol| symbol == "AD06")),
        "{symbols:?}"
    );
    let key = |state| {
        ui::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
            winit::event::RawKeyEvent {
                physical_key: winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyY),
                state,
            },
            winit::keyboard::Key::Character("z".into()),
            Some("z".into()),
            winit::keyboard::KeyLocation::Standard,
            false,
            Some("z".into()),
            winit::keyboard::Key::Character("z".into()),
        ))
    };
    app.harness()
        .dispatch_to_root(&key(winit::event::ElementState::Pressed));
    // A pressed physical key is named wherever the drawing came from.
    assert_eq!(
        read(&mut app)["pressed_physical_keys"],
        serde_json::json!(if preview["native_layout_available"] == true {
            vec!["AD06"]
        } else {
            Vec::<&str>::new()
        })
    );
    app.harness()
        .dispatch_to_root(&key(winit::event::ElementState::Released));
    assert_eq!(
        read(&mut app)["pressed_physical_keys"],
        serde_json::json!([])
    );
    assert_eq!(app.settings::<InputPrefs>().get(), before);
    assert!(set(&mut app, InputEffect::ShowLiveKeyboardMap));
    let live = read(&mut app);
    assert_eq!(live["mode"], "live");
    assert_eq!(
        live["available"], false,
        "native evidence is mandatory, never preferred-layout fallback"
    );
    assert_eq!(live["selected_symbols"], serde_json::json!([]));
    app.harness()
        .dispatch_to_root(&key(winit::event::ElementState::Pressed));
    app.harness().dispatch_to_root(&WindowEvent::Focused(false));
    let blurred = read(&mut app);
    assert_eq!(blurred["foreground"], false);
    assert_eq!(blurred["pressed_physical_keys"], serde_json::json!([]));
    let about = crate::categories::Category::About;
    app.effect_on(
        &crate::effects::pane_row_id(about),
        &crate::effects::open_pane(about),
    )
    .unwrap();
    let off_page = read(&mut app);
    assert_eq!(off_page["page_visible"], false);
    assert_eq!(off_page["selected_symbols"], serde_json::json!([]));
    assert_eq!(off_page["pressed_physical_keys"], serde_json::json!([]));
}

#[test]
fn physical_key_inspection_is_published_validated_and_never_applies_layout() {
    use ui::tilo_resource::Effect as _;
    // The pane follows the identity the process has bound, which the
    // dictionary test swaps: a swap mid-test rebuilds the page under it.
    let _env = tilo_test_env::lock();
    let mut app = TestApp::<SettingsShell>::new().unwrap();
    let before = app.settings::<InputPrefs>().get();
    let inspect = InputEffect::InspectKeyboardKey { key: "AD06".into() };
    assert!(
        app.actions()
            .unwrap()
            .iter()
            .any(|action| action.address.action == inspect.id())
    );
    assert!(set(&mut app, inspect));
    let inspected = app.settings::<InputPrefs>().get();
    assert_eq!(inspected.keyboard_inspected_key, "AD06");
    assert_eq!(inspected.keyboard_layout, before.keyboard_layout);
    assert_eq!(inspected.keyboard_layouts, before.keyboard_layouts);
    assert_eq!(inspected.keyboard_preview, before.keyboard_preview);
    assert!(!set(
        &mut app,
        InputEffect::InspectKeyboardKey {
            key: "../../private".into()
        }
    ));
    assert!(!set(
        &mut app,
        InputEffect::InspectKeyboardKey { key: "AE13".into() }
    ));
    assert!(set(
        &mut app,
        InputEffect::SetKeyboardModel {
            model: "jp106".into()
        }
    ));
    assert!(set(
        &mut app,
        InputEffect::InspectKeyboardKey { key: "AE13".into() }
    ));
    assert!(set(
        &mut app,
        InputEffect::SetKeyboardModel {
            model: "pc104".into()
        }
    ));
    assert_eq!(
        app.settings::<InputPrefs>().get().keyboard_inspected_key,
        "AD01",
        "a model switch must not leave an absent JIS position selected"
    );
}

fn set(app: &mut TestApp<SettingsShell>, effect: InputEffect) -> bool {
    app.effect(&SettingsEffect::from(effect)).is_ok()
}

/// Pointer and keyboard choices are the person's `input` section, the
/// touchpad switch this device's, spelling the shared
/// `text_assistance` section; a value outside the offered ones is refused.
#[test]
fn input_effects_write_the_input_section() {
    // The pane follows the identity the process has bound, which the
    // dictionary test swaps: a swap mid-test rebuilds the page under it.
    let _env = tilo_test_env::lock();
    let mut app = TestApp::<SettingsShell>::new().unwrap();
    edit_field(&mut app, INPUT, "touchpad_pointer_speed", 0.9).unwrap();
    edit_field(&mut app, INPUT, "mouse_natural_scroll", true).unwrap();
    edit_field(&mut app, INPUT, "touchpad_enabled", false).unwrap();
    assert!(set(
        &mut app,
        InputEffect::SetComposeKey { key: "ralt".into() }
    ));
    assert!(set(
        &mut app,
        InputEffect::SetCapsLock {
            behaviour: "ctrl".into()
        }
    ));
    edit_field(&mut app, INPUT, "touchpad_click_method", "areas").unwrap();
    edit_field(&mut app, TEXT_ASSISTANCE, "check_spelling", false).unwrap();
    let input = app.settings::<InputPrefs>().get();
    assert_eq!(input.touchpad_pointer_speed, Some(0.9));
    assert!(input.mouse_natural_scroll);
    assert!(!input.touchpad_enabled);
    let active = settings_store::keyboard_sources::effective(
        &input.rich_input(),
        settings_store::keyboard_sources::ALL,
    );
    assert_eq!(active.compose_key.as_deref(), Some("ralt"));
    assert_eq!(active.caps_lock.as_deref(), Some("ctrl"));
    assert_eq!(input.touchpad_click_method, TouchpadClickMethod::Areas);
    assert!(!app.settings::<TextAssistancePrefs>().get().check_spelling);
    edit_field(&mut app, TEXT_ASSISTANCE, "spelling_language", "de_DE").unwrap();
    assert_eq!(
        app.settings::<TextAssistancePrefs>()
            .get()
            .spelling_language,
        "de_DE"
    );
    edit_field(&mut app, TEXT_ASSISTANCE, "spelling_language", "").unwrap();
    assert_eq!(
        app.settings::<TextAssistancePrefs>()
            .get()
            .spelling_language,
        "",
        "empty is the session's language"
    );

    assert!(edit_field(&mut app, INPUT, "mouse_pointer_speed", 4.0).is_err());
    assert!(!set(
        &mut app,
        InputEffect::SetComposeKey { key: "f13".into() }
    ));
    assert_eq!(app.settings::<InputPrefs>().get(), input);
}

/// Input sources are added, ordered, switched and removed by id; the
/// last one stays.
#[test]
fn input_sources_are_added_ordered_and_removed() {
    // The pane follows the identity the process has bound, which the
    // dictionary test swaps: a swap mid-test rebuilds the page under it.
    let _env = tilo_test_env::lock();
    let mut app = TestApp::<SettingsShell>::new().unwrap();
    let sources = |app: &TestApp<SettingsShell>| {
        let input = app.settings::<InputPrefs>().get();
        (input.keyboard_layouts.clone(), input.keyboard_layout)
    };
    assert!(set(
        &mut app,
        InputEffect::AddSource {
            source: "de".into()
        }
    ));
    assert_eq!(sources(&app), (vec!["us".into(), "de".into()], "de".into()));
    assert!(!set(
        &mut app,
        InputEffect::AddSource {
            source: "xx".into()
        }
    ));
    assert!(set(
        &mut app,
        InputEffect::MoveSourceUp {
            source: "de".into()
        }
    ));
    assert!(set(
        &mut app,
        InputEffect::SetActiveSource {
            source: "us".into()
        }
    ));
    assert_eq!(sources(&app), (vec!["de".into(), "us".into()], "us".into()));
    assert!(set(
        &mut app,
        InputEffect::RemoveSource {
            source: "us".into()
        }
    ));
    assert_eq!(sources(&app), (vec!["de".into()], "de".into()));
    assert!(!set(
        &mut app,
        InputEffect::RemoveSource {
            source: "de".into()
        }
    ));
}

/// A tablet's choices are kept per device id; a pad's controls by
/// button and dial.
#[test]
fn tablet_effects_keep_each_device_apart() {
    // The pane follows the identity the process has bound, which the
    // dictionary test swaps: a swap mid-test rebuilds the page under it.
    let _env = tilo_test_env::lock();
    let mut app = TestApp::<SettingsShell>::new().unwrap();
    assert!(set(
        &mut app,
        InputEffect::SetTabletEraser {
            device: "pen-a".into(),
            action: "right".into(),
        }
    ));
    assert!(set(
        &mut app,
        InputEffect::SetPadDial {
            device: "pad-a".into(),
            dial: "ring".into(),
            index: 0,
            mode: 1,
            action: "zoom".into(),
        }
    ));
    assert!(!set(
        &mut app,
        InputEffect::SetTabletButton {
            device: "pen-a".into(),
            button: "fourth".into(),
            action: "left".into(),
        }
    ));
    let input = app.settings::<InputPrefs>().get();
    assert_eq!(input.tablets["pen-a"].eraser, EraserAction::Right);
    assert_eq!(
        input.tablets["pad-a"].dial_action(PadDial::Ring(0), 1),
        PadDialAction::Zoom
    );
    assert_eq!(
        parse_dial_action_id("strip-1-mode-0"),
        Some(("strip", 1, 0))
    );
    assert_eq!(
        parse_dial_action_id(&dial_action_id(PadDial::Ring(2), 3)),
        Some(("ring", 2, 3))
    );
}
