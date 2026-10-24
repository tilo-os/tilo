use super::*;
use crate::shell::quick_settings::{BtDevice, WifiNetwork};
use ui::time::Duration;
use ui::tokens::Motion;
const PANEL: Rect = Rect::new(100., 48., 500., 960.);

fn motion() -> EffectiveMotion {
    EffectiveMotion::new(Motion::default(), false)
}

/// A laptop: Wi-Fi, Bluetooth, a battery and the power daemon.
fn laptop() -> QuickSettingsState {
    let mut state = QuickSettingsState {
        brightness_available: true,
        bluetooth_available: true,
        bluetooth: true,
        radios: os_actions::RadioState {
            wifi_available: true,
            wifi_enabled: true,
            ..os_actions::RadioState::default()
        },
        wifi: true,
        power_profiles: vec![
            PowerProfile::Saver,
            PowerProfile::Balanced,
            PowerProfile::Performance,
        ],
        ..QuickSettingsState::default()
    };
    state.status.battery = Some(tilo_power_mgmt::BatterySnapshot {
        present: true,
        percent: 64,
        state: tilo_power_mgmt::BatteryState::Discharging,
    });
    state.status.volume = Some(os_actions::VolumeState {
        level: 0.4,
        muted: false,
    });
    state.wifi_networks = vec![
        WifiNetwork {
            ssid: "Home".into(),
            signal: 4,
            secure: true,
            security: os_actions::WifiSecurity::Wpa2,
            connected: true,
            known: true,
        },
        WifiNetwork {
            ssid: "Cafe".into(),
            signal: 2,
            secure: true,
            security: os_actions::WifiSecurity::Wpa2,
            connected: false,
            known: false,
        },
        WifiNetwork {
            ssid: "Library".into(),
            signal: 3,
            secure: false,
            security: os_actions::WifiSecurity::Open,
            connected: false,
            known: false,
        },
    ];
    state.bt_devices = vec![BtDevice {
        name: "Headphones".into(),
        address: "02:00:00:00:00:01".into(),
        battery: 80,
        connected: true,
    }];
    state
}

fn laid_out(state: &QuickSettingsState) -> QuickSettingsRoot {
    let mut root = QuickSettingsRoot {
        panel: PANEL,
        ..QuickSettingsRoot::default()
    };
    root.settle_sections(state);
    root
}

fn tile(
    root: &QuickSettingsRoot,
    state: &QuickSettingsState,
    control: QuickSettingsControl,
) -> Rect {
    root.page_layout(state)
        .tiles
        .into_iter()
        .find(|(c, _)| *c == control)
        .map(|(_, rect)| rect)
        .expect("the tile is shown")
}

#[test]
fn the_default_grid_holds_the_everyday_controls_this_machine_has() {
    let root = QuickSettingsRoot::default();
    assert_eq!(root.config().controls, QuickSettingsControl::DEFAULT);
    let mut state = laptop();
    assert_eq!(
        root.visible_controls(&state),
        [
            QuickSettingsControl::Wifi,
            QuickSettingsControl::Bluetooth,
            QuickSettingsControl::DoNotDisturb,
            QuickSettingsControl::NightLight,
            QuickSettingsControl::AirplaneMode,
            QuickSettingsControl::PowerMode,
        ],
        "VPN without profiles and rotation lock without a sensor stay hidden"
    );
    state.vpns = vec![os_actions::VpnConnectionInfo {
        name: "Work".into(),
        uuid: "work-uuid".into(),
        active: false,
        kind: "wireguard".into(),
    }];
    state.rotation_available = true;
    assert_eq!(root.control_count(&state), 8);

    // No radio, no adapter, no daemon: no inert tiles.
    let bare = QuickSettingsState::default();
    assert_eq!(
        root.visible_controls(&bare),
        [
            QuickSettingsControl::DoNotDisturb,
            QuickSettingsControl::NightLight
        ]
    );
}

#[test]
fn root_controls_reflect_present_hardware_and_migrated_config() {
    // Wired only: the network slot is the wired tile.
    let root = QuickSettingsRoot::default();
    let state = QuickSettingsState {
        wired: Some(os_actions::WiredInfo {
            connected: true,
            device: Some("enp0s31f6".into()),
            ..Default::default()
        }),
        ..QuickSettingsState::default()
    };
    assert!(
        root.visible_controls(&state)
            .contains(&QuickSettingsControl::Wifi)
    );
    assert_eq!(
        QuickSettingsRoot::rendered_label(QuickSettingsControl::Wifi, &state),
        "Wired network"
    );
    assert_eq!(
        QuickSettingsRoot::section_of(QuickSettingsControl::Wifi, &state),
        None,
        "a wired network has nothing to open"
    );
    let names: Vec<_> = root
        .accessibility_snapshot(&state)
        .children
        .iter()
        .filter_map(|node| node.name.clone())
        .collect();
    assert!(names.contains(&"Wired network".to_owned()));
    assert!(!names.contains(&"Bluetooth".to_owned()));

    // A saved layout that kept Airplane mode keeps it removable without
    // a radio.
    let kept = QuickSettingsRoot::with_config(QuickSettingsConfig {
        controls: vec![
            QuickSettingsControl::Wifi,
            QuickSettingsControl::NightLight,
            QuickSettingsControl::AirplaneMode,
        ],
    });
    assert!(
        kept.edit_controls(&QuickSettingsState::default())
            .contains(&QuickSettingsControl::NightLight)
    );
    assert!(
        !kept
            .edit_controls(&QuickSettingsState::default())
            .contains(&QuickSettingsControl::MobileData)
    );
}

#[test]
fn semantic_targets_cover_every_visible_root_control() {
    let state = laptop();
    let root = laid_out(&state);
    let targets = root.semantic_rects(&state);
    for required in [
        "quick_settings_edit",
        "quick_settings_settings",
        "quick_settings_lock",
        "quick_settings_power",
        "quick_settings_devices",
        "quick_settings_clipboard",
        "quick_settings_notifications",
        "quick_settings_battery",
        "quick_settings_brightness",
        "quick_settings_volume",
        "quick_settings_network",
        "quick_settings_network_details",
        "quick_settings_bluetooth",
        "quick_settings_bluetooth_details",
        "quick_settings_night_light",
        "quick_settings_power_mode",
    ] {
        assert!(
            targets
                .iter()
                .any(|(name, rect)| name == required && rect.area() > 0.0),
            "missing semantic target {required}"
        );
    }
    // No two targets overlap: a tile's switch and chevron are two.
    for (i, (a, ra)) in targets.iter().enumerate() {
        for (b, rb) in targets.iter().skip(i + 1) {
            if a == "quick_settings_edit" || b == "quick_settings_edit" {
                continue;
            }
            let overlap = ra.intersect(*rb);
            assert!(
                overlap.width() <= 1e-9 || overlap.height() <= 1e-9,
                "{a} {ra:?} overlaps {b} {rb:?}"
            );
        }
    }
}

#[test]
fn a_tile_turns_its_control_on_and_its_chevron_opens_its_section() {
    let mut state = laptop();
    let mut root = laid_out(&state);
    let wifi = tile(&root, &state, QuickSettingsControl::Wifi);
    let body = Point::new(wifi.x0 + 12.0, wifi.center().y);
    assert_eq!(
        root.handle_click(body, PANEL, &mut state),
        RootAction::Changed(PendingRootActions {
            wifi: Some(false),
            ..Default::default()
        }),
        "the body turns Wi-Fi off"
    );
    assert_eq!(state.section, None, "and opens nothing");

    let chevron = root.expand_rect(wifi).center();
    assert_eq!(
        root.handle_click(chevron, PANEL, &mut state),
        RootAction::SectionOpened(QsSection::Wifi)
    );
    assert_eq!(state.section, Some(QsSection::Wifi));
    assert!(state.wifi, "opening the list does not change the radio");

    // One section at a time: Bluetooth's chevron moves the open one.
    root.settle_sections(&state);
    let bluetooth = tile(&root, &state, QuickSettingsControl::Bluetooth);
    assert_eq!(
        root.handle_click(root.expand_rect(bluetooth).center(), PANEL, &mut state),
        RootAction::SectionOpened(QsSection::Bluetooth)
    );
    assert_eq!(state.section, Some(QsSection::Bluetooth));
    // Its chevron again closes it, and Esc closes whatever is open.
    root.settle_sections(&state);
    assert_eq!(
        root.handle_click(root.expand_rect(bluetooth).center(), PANEL, &mut state),
        RootAction::LayoutChanged
    );
    assert_eq!(state.section, None);
    state.open_section(QsSection::Battery);
    assert!(root.close_section(&mut state));
    assert_eq!(state.section, None);
    assert!(!root.close_section(&mut state));
}

#[test]
fn a_section_opens_in_place_under_its_row_and_the_tiles_stay_put() {
    let mut state = laptop();
    let root = laid_out(&state);
    let before = root.page_layout(&state);
    let height_before = root.preferred_height(&state);

    state.open_section(QsSection::Wifi);
    let root = laid_out(&state);
    let after = root.page_layout(&state);
    assert_eq!(before.tiles.len(), after.tiles.len());
    let slot = after
        .sections
        .iter()
        .find(|slot| slot.section == QsSection::Wifi)
        .expect("the Wi-Fi section shows");
    let wifi = tile(&root, &state, QuickSettingsControl::Wifi);
    assert!(slot.rect.y0 > wifi.y1, "under the Wi-Fi tile's row");
    assert_eq!(slot.rect.x0, after.sliders.x0, "full width");
    assert_eq!(slot.rect.x1, after.sliders.x1);
    let grown = slot.rect.height() + RELATED_GAP;
    for ((control, a), (_, b)) in before.tiles.iter().zip(&after.tiles) {
        if a.y0 <= wifi.y0 {
            assert_eq!(a, b, "{control:?} on or above the row stays put");
        } else {
            assert!(
                (b.y0 - a.y0 - grown).abs() < 1e-6,
                "{control:?} moves down with it"
            );
        }
    }
    assert!((after.bottom - before.bottom - grown).abs() < 1e-6);
    assert!(
        (root.preferred_height(&state) - height_before - grown).abs() < 1e-6,
        "the panel grows by the section"
    );
}

#[test]
fn opening_and_closing_a_section_moves_on_the_disclosure_motion() {
    let mut state = laptop();
    let mut root = laid_out(&state);
    let start = ui::time::now();
    let closed = root.page_layout_at(PANEL, &state, start);
    assert!(closed.sections.is_empty());

    state.open_section(QsSection::Wifi);
    root.follow_sections(&state, motion(), MotionSource::Pointer, start);
    assert!(root.is_animating(start));
    let full = sections::layout(
        QsSection::Wifi,
        &state,
        root.section_width(),
        root.section_metrics(),
    )
    .height;
    let settles = root.reveal[&QsSection::Wifi]
        .settles_at()
        .expect("it moves");
    let mut last = -1.0;
    let mut last_bottom = closed.bottom;
    let steps = 12;
    for step in 1..=steps {
        let at = start + (settles - start) * step / steps;
        let page = root.page_layout_at(PANEL, &state, at);
        let shown = page.sections.first().map_or(0.0, |slot| slot.shown);
        assert!(shown >= last, "the reveal only grows: {shown} after {last}");
        assert!(page.bottom >= last_bottom, "the page grows with it");
        last = shown;
        last_bottom = page.bottom;
    }
    assert!((last - full).abs() < 0.5, "it ends fully open");
    assert!(!root.is_animating(settles + Duration::from_millis(1)));

    // Closing runs it back to nothing; the content stays laid out at
    // full height under the shrinking clip.
    let closing = settles + Duration::from_millis(5);
    state.section = None;
    root.follow_sections(&state, motion(), MotionSource::Pointer, closing);
    let midway = root.page_layout_at(PANEL, &state, closing + Duration::from_millis(40));
    let slot = midway.sections.first().expect("still closing");
    assert!(slot.shown < full && (slot.rect.height() - full).abs() < 1e-9);
    let ends = root.reveal[&QsSection::Wifi]
        .settles_at()
        .expect("it moves");
    let gone = root.page_layout_at(PANEL, &state, ends + Duration::from_millis(1));
    assert!(gone.sections.is_empty());
    assert!((gone.bottom - closed.bottom).abs() < 1e-6);
}

#[test]
fn the_wifi_section_turns_wifi_on_and_joins_networks_or_asks_for_a_password() {
    let mut state = laptop();
    state.wifi = false;
    state.radios.wifi_enabled = false;
    state.open_section(QsSection::Wifi);
    let mut root = laid_out(&state);
    let stops = root.root_stops(&state);
    assert!(stops.contains(&RootStop::Section(SectionStop::TurnOn)));
    let turn_on = root.stop_rect(RootStop::Section(SectionStop::TurnOn), &state);
    assert_eq!(
        root.handle_click(turn_on.center(), PANEL, &mut state),
        RootAction::Changed(PendingRootActions {
            wifi: Some(true),
            ..Default::default()
        })
    );

    state.wifi = true;
    root.settle_sections(&state);
    let press = |root: &mut QuickSettingsRoot, state: &mut QuickSettingsState, index: usize| {
        let rect = root.stop_rect(RootStop::Section(SectionStop::Network(index)), state);
        root.handle_click(rect.center(), PANEL, state)
    };
    assert_eq!(
        press(&mut root, &mut state, 1),
        RootAction::Wifi(WifiIntent::AskPassword("Cafe".into())),
        "a protected network this machine does not know asks for its password"
    );
    assert_eq!(
        press(&mut root, &mut state, 2),
        RootAction::Wifi(WifiIntent::Join("Library".into()))
    );
    state.wifi_networks[1].known = true;
    assert_eq!(
        press(&mut root, &mut state, 1),
        RootAction::Wifi(WifiIntent::Join("Cafe".into())),
        "a known one joins without asking"
    );

    // The password is asked in place, under its network.
    state.wifi_networks[1].known = false;
    state.wifi_password = Some(crate::shell::quick_settings::WifiPasswordPrompt::new(
        "Cafe",
    ));
    root.settle_sections(&state);
    let prompt = root
        .wifi_password_rect(&state)
        .expect("the prompt is inside the section");
    let cafe = root.stop_rect(RootStop::Section(SectionStop::Network(1)), &state);
    assert!(prompt.y0 > cafe.y1);
    let settings = root.stop_rect(RootStop::Section(SectionStop::Settings), &state);
    assert!(settings.y0 >= prompt.y1, "the rows below make room for it");
}

#[test]
fn the_volume_chevron_chooses_where_sound_plays_under_the_sliders() {
    let mut state = laptop();
    state.sound_outputs = vec![
        crate::shell::quick_settings::SoundOutput {
            node: 41,
            name: "Speakers".into(),
            current: true,
        },
        crate::shell::quick_settings::SoundOutput {
            node: 57,
            name: "Headphones".into(),
            current: false,
        },
    ];
    let mut root = laid_out(&state);
    root.sync_displays(&state);
    let wifi_before = tile(&root, &state, QuickSettingsControl::Wifi);
    let chevron = root.stop_rect(RootStop::SoundExpand, &state);
    let volume = root.slider_face_rect(root.sliders_block(), root.volume_slider());
    assert!(
        chevron.x0 >= volume.x1,
        "the chevron is beside the slider, not on it"
    );
    assert_eq!(
        root.handle_click(chevron.center(), PANEL, &mut state),
        RootAction::SectionOpened(QsSection::Sound)
    );
    root.settle_sections(&state);
    let headphones = root.stop_rect(RootStop::Section(SectionStop::Output(1)), &state);
    assert!(headphones.y0 > volume.y1, "under the sliders");
    assert!(
        tile(&root, &state, QuickSettingsControl::Wifi).y0 > wifi_before.y0,
        "the tiles move down to make room"
    );
    assert_eq!(
        root.handle_click(headphones.center(), PANEL, &mut state),
        RootAction::SoundOutput(57)
    );
    let speakers = root.stop_rect(RootStop::Section(SectionStop::Output(0)), &state);
    assert_eq!(
        root.handle_click(speakers.center(), PANEL, &mut state),
        RootAction::None,
        "where sound already plays has nothing to do"
    );
}

#[test]
fn the_power_mode_lives_in_its_tile_and_the_battery_section_links_to_it() {
    let mut state = laptop();
    let mut root = laid_out(&state);
    // The tile's chevron opens the modes under the tile.
    let power = tile(&root, &state, QuickSettingsControl::PowerMode);
    assert_eq!(
        root.handle_click(root.expand_rect(power).center(), PANEL, &mut state),
        RootAction::SectionOpened(QsSection::PowerMode)
    );
    root.settle_sections(&state);
    let saver = root.stop_rect(
        RootStop::Section(SectionStop::Mode(PowerProfile::Saver)),
        &state,
    );
    assert!(saver.y0 > power.y1, "the modes are under the tile");
    assert_eq!(
        root.handle_click(saver.center(), PANEL, &mut state),
        RootAction::Changed(PendingRootActions {
            power: Some(PowerProfile::Saver),
            ..Default::default()
        })
    );
    assert_eq!(state.power_profile, PowerProfile::Saver);
    // The tile's body switches the saver back off.
    let power = tile(&root, &state, QuickSettingsControl::PowerMode);
    assert_eq!(
        root.handle_click(
            Point::new(power.x0 + 12.0, power.center().y),
            PANEL,
            &mut state
        ),
        RootAction::Changed(PendingRootActions {
            power: Some(PowerProfile::Balanced),
            ..Default::default()
        })
    );

    // The Battery section offers no mode of its own: its power mode row
    // takes the person to the tile's section.
    state.open_section(QsSection::Battery);
    root.settle_sections(&state);
    let stops = root.root_stops(&state);
    assert!(
        !stops
            .iter()
            .any(|stop| matches!(stop, RootStop::Section(SectionStop::Mode(_)))),
        "the mode is chosen in one place"
    );
    let link = root.stop_rect(RootStop::Section(SectionStop::PowerMode), &state);
    assert_eq!(
        root.handle_click(link.center(), PANEL, &mut state),
        RootAction::SectionOpened(QsSection::PowerMode)
    );
    assert_eq!(state.section, Some(QsSection::PowerMode));

    // With the tile taken off the page, the row leads to the power
    // settings instead of to nothing.
    let mut config = QuickSettingsConfig::default();
    config
        .controls
        .retain(|control| *control != QuickSettingsControl::PowerMode);
    let mut root = QuickSettingsRoot {
        panel: PANEL,
        ..QuickSettingsRoot::with_config(config)
    };
    state.open_section(QsSection::Battery);
    root.settle_sections(&state);
    let link = root.stop_rect(RootStop::Section(SectionStop::PowerMode), &state);
    assert_eq!(
        root.handle_click(link.center(), PANEL, &mut state),
        RootAction::OpenSettings(Some("settings://power"))
    );
}

#[test]
fn root_focus_order_matches_visible_actions() {
    // Opening has no hidden focus, and the first Tab selects Brightness.
    let mut state = laptop();
    let mut root = laid_out(&state);
    root.keyboard_focus = Some(4);
    root.reset_focus();
    assert_eq!(root.keyboard_focus, None);
    assert!(root.focus_next(&state, false));
    assert_eq!(root.focused_stop(&state), Some(RootStop::Slider(0)));

    // Shift+Tab from no selection lands on Power, whatever hardware
    // precedes it.
    let mut bare = QuickSettingsState::default();
    let mut root = laid_out(&bare);
    root.reset_focus();
    assert!(root.focus_next(&bare, true));
    assert_eq!(
        root.focused_stop(&bare),
        Some(RootStop::Footer(FooterButton::Power))
    );
    assert_eq!(
        root.activate_focused(PANEL, &mut bare),
        RootAction::OpenPowerMenu
    );
    // Device state arriving under focus keeps it on its stop.
    bare.volume_available = true;
    bare.brightness_available = true;
    assert_eq!(
        root.focused_stop(&bare),
        Some(RootStop::Footer(FooterButton::Power))
    );

    // Every stop activates as what it is.
    let mut root = laid_out(&state);
    for (stop, expected) in [
        (
            RootStop::Row(RootRow::Devices),
            RootAction::SectionOpened(QsSection::Devices),
        ),
        (
            RootStop::Row(RootRow::Clipboard),
            RootAction::SectionOpened(QsSection::Clipboard),
        ),
        (
            RootStop::Row(RootRow::Notifications),
            RootAction::Enter(QsLevel::Notifications),
        ),
        (
            RootStop::Footer(FooterButton::Settings),
            RootAction::OpenSettings(None),
        ),
        (RootStop::Footer(FooterButton::Lock), RootAction::Lock),
        (RootStop::Edit, RootAction::LayoutChanged),
    ] {
        // An opened section adds its stops: find this one afresh.
        let stops = root.root_stops(&state);
        let path = stops.iter().position(|s| *s == stop).unwrap();
        assert!(root.focus_accessibility_path(&[path], &state));
        assert_eq!(root.focused_stop(&state), Some(stop));
        assert_eq!(root.activate_focused(PANEL, &mut state), expected);
        root.edit_mode = false;
    }

    // An open section's stops follow its row, before the next row.
    state.open_section(QsSection::Wifi);
    let root = laid_out(&state);
    let stops = root.root_stops(&state);
    let expand = stops
        .iter()
        .position(|s| matches!(s, RootStop::Expand(_, QuickSettingsControl::Wifi)))
        .unwrap();
    let first_section = stops
        .iter()
        .position(|s| matches!(s, RootStop::Section(_)))
        .unwrap();
    let next_row_tile = stops
        .iter()
        .position(|s| matches!(s, RootStop::Control(_, QuickSettingsControl::DoNotDisturb)))
        .unwrap();
    assert!(expand < first_section && first_section < next_row_tile);
}

#[test]
fn each_focus_stop_marks_exactly_one_accessibility_node() {
    let mut state = laptop();
    state.open_section(QsSection::Wifi);
    let mut root = laid_out(&state);
    let stops = root.root_stops(&state);
    for (index, stop) in stops.iter().enumerate() {
        assert!(root.focus_accessibility_path(&[index], &state));
        let snapshot = root.accessibility_snapshot(&state);
        assert_eq!(snapshot.children.len(), stops.len());
        let focused: Vec<_> = snapshot
            .children
            .iter()
            .enumerate()
            .filter(|(_, node)| node.state.focused)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(focused, [index], "{stop:?}");
        assert_eq!(
            snapshot.children[index]
                .bounds
                .map(ui::a11y::AccessBounds::to_rect),
            Some(root.stop_rect(*stop, &state)),
            "{stop:?} publishes the rect it paints"
        );
    }
}

#[test]
fn the_layout_never_overlaps_and_grows_with_large_text() {
    let mut state = laptop();
    state.open_section(QsSection::Battery);
    for scale in [1.0_f32, 1.5, 2.0] {
        let mut root = QuickSettingsRoot::default();
        root.set_layout_text_scale(scale);
        root.panel = PANEL;
        root.settle_sections(&state);
        let page = root.page_layout(&state);
        let mut rects: Vec<Rect> = vec![page.sliders];
        rects.extend(page.tiles.iter().map(|(_, r)| *r));
        rects.extend(page.sections.iter().map(SectionSlot::clip));
        rects.extend(page.rows.iter().map(|(_, r)| *r));
        rects.extend(page.footer.iter().map(|(_, r)| *r));
        for (i, a) in rects.iter().enumerate() {
            for b in rects.iter().skip(i + 1) {
                let overlap = a.intersect(*b);
                assert!(
                    overlap.width() <= 1e-9 || overlap.height() <= 1e-9,
                    "{a:?} overlaps {b:?} at {scale}"
                );
            }
        }
        for (_, tile) in &page.tiles {
            assert!(
                tile.height() >= root.line_height(TextRole::Label) + root.caption_line_height(),
                "a tile holds its two lines at {scale}"
            );
        }
    }
}

#[test]
fn opening_a_section_changes_only_the_panels_height() {
    let theme = Theme::dark();
    let mut state = laptop();
    let mut root = QuickSettingsRoot {
        panel: PANEL,
        ..QuickSettingsRoot::default()
    };
    root.sync_displays(&state);
    root.settle_sections(&state);
    // The closed page just fits; the network list then overflows it.
    let closed_height = root.preferred_height(&state);
    let viewport = Rect::new(100.0, 48.0, 500.0, 48.0 + closed_height + 1.0);
    let extents = |root: &mut QuickSettingsRoot, state: &QuickSettingsState| {
        root.sync_scroll(viewport, &theme, state);
        let page = root.lay_out(viewport, state);
        let layout = root.page_layout(state);
        let mut spans = vec![("page".to_owned(), page.x0, page.x1)];
        spans.push(("sliders".into(), layout.sliders.x0, layout.sliders.x1));
        for i in 0..root.slider_count() {
            let face = root.slider_face_rect(layout.sliders, i);
            spans.push((format!("slider {i}"), face.x0, face.x1));
        }
        for (control, rect) in &layout.tiles {
            spans.push((format!("{control:?}"), rect.x0, rect.x1));
        }
        for (row, rect) in &layout.rows {
            spans.push((format!("{row:?}"), rect.x0, rect.x1));
        }
        spans
    };
    let closed = extents(&mut root, &state);
    assert!(!root.scrolls(), "the closed page fits {viewport:?}");

    state.open_section(QsSection::Wifi);
    root.settle_sections(&state);
    let open = extents(&mut root, &state);
    assert!(root.scrolls(), "the open network list overflows the panel");
    assert_eq!(open, closed, "nothing moved sideways");
    let slot = root
        .page_layout(&state)
        .sections
        .into_iter()
        .find(|slot| slot.section == QsSection::Wifi)
        .expect("the Wi-Fi section is on the page");
    assert!(
        slot.rect.x0 >= viewport.x0 && slot.rect.x1 <= viewport.x1,
        "the network list stays inside the panel's width: {:?}",
        slot.rect
    );
}

/// Lay `root` out in a popover `viewport` shorter than its page, then
/// Tab through every stop: each lands fully inside the shown part of the
/// popover, scrolled there the way a list follows its focus.
#[test]
fn a_short_popover_scrolls_every_stop_into_view() {
    let mut state = laptop();
    state.open_section(QsSection::Wifi);
    let mut root = QuickSettingsRoot {
        panel: PANEL,
        ..QuickSettingsRoot::default()
    };
    root.settle_sections(&state);
    let viewport = Rect::new(100.0, 48.0, 500.0, 448.0);
    let theme = Theme::dark();
    root.sync_displays(&state);
    root.sync_scroll(viewport, &theme, &state);
    root.lay_out(viewport, &state);
    assert!(root.scrolls(), "a page taller than {viewport:?} scrolls");
    let stops = root.root_stops(&state);
    root.reset_focus();
    let mut deepest: f64 = 0.0;
    for stop in &stops {
        root.focus_next(&state, false);
        assert_eq!(root.focused_stop(&state), Some(*stop));
        let shown = root.visible_rect();
        let rect = root.stop_rect(*stop, &state);
        if *stop == RootStop::Edit {
            continue;
        }
        assert!(
            rect.y0 >= shown.y0 - 1e-6 && rect.y1 <= shown.y1 + 1e-6,
            "{stop:?} at {rect:?} is not shown in {shown:?}"
        );
        deepest = deepest.max(root.scroll_offset());
    }
    assert!(deepest > 0.0, "reaching the last stops scrolled the page");
}

#[test]
fn edit_mode_adds_removes_and_reorders_by_keyboard_pointer_and_explicit_remove() {
    // Keyboard reorder, then add from the available chips.
    let state = laptop();
    let mut root = QuickSettingsRoot {
        edit_mode: true,
        panel: PANEL,
        ..QuickSettingsRoot::default()
    };
    root.keyboard_focus = Some(0);
    assert!(root.reorder_focused(&state, 1));
    assert_eq!(root.config.controls[1], QuickSettingsControl::Wifi);

    // The row body starts a reorder; only its explicit action removes.
    let mut root = QuickSettingsRoot {
        edit_mode: true,
        panel: PANEL,
        ..QuickSettingsRoot::default()
    };
    let row = root.edit_row(PANEL, 1);
    let bluetooth = QuickSettingsControl::Bluetooth;
    assert_eq!(root.edit_click(row.center(), &state), RootAction::None);
    assert!(root.config.controls.contains(&bluetooth));
    let _ = root.finish_drag(&state);
    assert_eq!(
        root.edit_click(root.edit_action_rect(row).center(), &state),
        RootAction::LayoutChanged
    );
    assert!(!root.config.controls.contains(&bluetooth));
    // It is offered back as available and adds again.
    let (_, chip) = root
        .available_chips(&state)
        .into_iter()
        .find(|(c, _)| *c == bluetooth)
        .expect("Bluetooth is offered back");
    assert_eq!(
        root.edit_click(chip.center(), &state),
        RootAction::LayoutChanged
    );
    assert!(root.config.controls.contains(&bluetooth));

    // Done is the last edit stop and leaves edit mode with no stale focus.
    let edit_count = root.edit_controls(&state).len();
    root.keyboard_focus = Some(edit_count.saturating_add(1));
    let mut state = state;
    assert_eq!(
        root.activate_focused(PANEL, &mut state),
        RootAction::LayoutChanged
    );
    assert!(!root.edit_mode());
    assert_eq!(root.keyboard_focus, None);
}

#[test]
fn edit_help_copy_uses_font_safe_keyboard_names() {
    let copy = QuickSettingsRoot::edit_help_copy();
    assert_eq!(copy, "Drag or press Ctrl + Up/Down to reorder");
    assert!(!copy.contains('·'), "no middle-dot join in chrome copy");
}

#[test]
fn each_monitor_gets_a_named_brightness_slider() {
    let mut state = laptop();
    state.displays = vec![
        crate::shell::chrome_state::DisplayBrightness {
            connector: "eDP-1".into(),
            name: "Built-in display".into(),
            level: 0.5,
        },
        crate::shell::chrome_state::DisplayBrightness {
            connector: "DP-2".into(),
            name: "Dell U2720Q".into(),
            level: 0.75,
        },
    ];
    let mut root = laid_out(&state);
    root.sync_displays(&state);
    assert_eq!(root.slider_count(), 3);
    assert_eq!(root.slider_name(1), "Brightness, Dell U2720Q");
    assert!(
        root.semantic_rects(&state)
            .into_iter()
            .any(|(name, _)| name == "quick_settings_brightness_1")
    );
    assert_eq!(
        root.slider_commit(&state, 1),
        SliderCommit::DisplayBrightness(1, 75.0)
    );
}

/// The sliders sit straight on the panel: rendered through the production
/// `draw` onto the panel ground, the air around and between the slider
/// bands is that ground, never a Control surface of its own, while each
/// track's unfilled well still stands off that ground. An off tile in the
/// same render keeps its card, so the probe can see one. Every preset.
#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel probes inside a small, positive render"
)]
fn the_sliders_sit_on_the_panel_not_in_a_card_of_their_own() {
    let state = laptop();
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let mut root = laid_out(&state);
        let ground = ui::chrome::shell::panel_ground(&theme);
        let (width, height) = (PANEL.width() as u32, PANEL.height() as u32);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        root.draw(
            &mut scene,
            Affine::translate(-PANEL.origin().to_vec2()),
            &mut text,
            &theme,
            PANEL,
            None,
            &state,
        );
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut target =
            tilo_cpu_raster::Target::new(&mut pixels, width, height, width as usize * 4).unwrap();
        tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options::default()).render(
            scene.encoding(),
            &mut target,
            None,
            ground,
        );
        let at = |p: Point| {
            let (x, y) = ((p.x - PANEL.x0) as usize, (p.y - PANEL.y0) as usize);
            let start = (y * width as usize + x) * 4;
            [
                pixels[start],
                pixels[start + 1],
                pixels[start + 2],
                pixels[start + 3],
            ]
        };
        let bare = at(Point::new(PANEL.x0 + 4.0, PANEL.y0 + 4.0));

        let layout = root.page_layout(&state);
        let sliders = layout.sliders;
        let first = root.slider_face_rect(sliders, 0);
        let second = root.slider_face_rect(sliders, 1);
        for probe in [
            Point::new(sliders.x0 + 3.0, first.center().y),
            Point::new(sliders.x0 + 3.0, second.center().y),
            Point::new(
                first.x0 + 4.0,
                f64::midpoint(first.y1, second.y0).min(second.y0 - 1.0),
            ),
            // Over the first track, clear of it and of the knob.
            Point::new(first.x0 + 2.0, first.y0 + 2.0),
        ] {
            assert_eq!(
                at(probe),
                bare,
                "the panel ground shows at {probe:?}: no card under the sliders"
            );
        }

        for face in [first, second] {
            let well = at(Point::new(face.x1 - 4.0, face.center().y));
            let apart = well
                .iter()
                .zip(bare)
                .map(|(a, b)| a.abs_diff(b))
                .max()
                .unwrap_or(0);
            assert!(
                apart >= 8,
                "the unfilled well {well:?} stands off the panel ground {bare:?}"
            );
        }

        let off = tile(&root, &state, QuickSettingsControl::DoNotDisturb);
        assert_ne!(
            at(Point::new(off.x0 + 3.0, off.center().y)),
            bare,
            "an off tile keeps its Control card, so the probe sees cards"
        );
    }
}

#[test]
fn every_slider_track_is_one_length_with_or_without_the_sound_chevron() {
    for outputs in [0, 2] {
        let mut state = laptop();
        state.sound_outputs = (0..outputs)
            .map(|node| crate::shell::quick_settings::SoundOutput {
                node,
                name: format!("Output {node}"),
                current: node == 0,
            })
            .collect();
        let mut root = laid_out(&state);
        root.sync_displays(&state);
        let block = root.sliders_block();
        let widths: Vec<f64> = (0..root.slider_count())
            .map(|i| root.slider_geometry_for(block, i).0.width())
            .collect();
        assert!(widths.len() >= 2, "brightness and volume both show");
        assert!(
            widths.iter().all(|w| (w - widths[0]).abs() < 1e-9),
            "with {outputs} outputs the tracks are {widths:?}"
        );
    }
}

#[test]
fn the_volume_icon_is_a_mute_switch() {
    let mut state = laptop();
    let mut root = laid_out(&state);
    root.sync_displays(&state);
    let icon = root.stop_rect(RootStop::VolumeMute, &state);
    let volume = root.slider_face_rect(root.sliders_block(), root.volume_slider());
    assert!(icon.x1 <= volume.x0, "the icon sits beside the slider");
    assert_eq!(
        root.handle_click(icon.center(), PANEL, &mut state),
        RootAction::ToggleMute
    );
    assert!(
        root.semantic_rects(&state)
            .into_iter()
            .any(|(name, _)| name == "quick_settings_volume_mute")
    );
}
