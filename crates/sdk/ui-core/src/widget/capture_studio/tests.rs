use super::*;

const SCREEN: Rect = Rect::new(0.0, 0.0, 1600.0, 1000.0);

fn studio() -> CaptureStudio {
    let mut s = CaptureStudio {
        rect: SCREEN,
        ..CaptureStudio::default()
    };
    s.open();
    s
}

fn window(id: &str, visible: Rect, virtual_rect: Rect) -> CaptureTarget {
    CaptureTarget {
        id: id.into(),
        title: id.into(),
        app_id: format!("so.tilo.{id}"),
        visible_rect: visible,
        virtual_rect,
        complete_rect: None,
        focused: false,
        on_active_strip: true,
    }
}

/// Two windows side by side on the strip: Notes on screen and focused,
/// Files half past the right edge.
fn two_windows() -> Vec<CaptureTarget> {
    let mut notes = window(
        "notes",
        Rect::new(0.0, 40.0, 900.0, 1000.0),
        Rect::new(0.0, 40.0, 900.0, 1000.0),
    );
    notes.focused = true;
    let files = window(
        "files",
        Rect::new(910.0, 40.0, 1600.0, 1000.0),
        Rect::new(910.0, 40.0, 2310.0, 1000.0),
    );
    vec![notes, files]
}

/// A press at `from`, the pointer to `to`, and the release.
fn drag(s: &mut CaptureStudio, from: Point, to: Point, shift: bool) {
    s.click(from, shift);
    if let Some(drag) = s.drag.as_mut() {
        drag.set_current(to);
    }
    s.finish_drag();
}

fn names(s: &CaptureStudio) -> Vec<String> {
    s.test_rects().into_iter().map(|(name, _)| name).collect()
}

fn the_intent(s: &mut CaptureStudio) -> CaptureIntent {
    s.commit(s.primary_destination());
    s.take_intents()
        .pop()
        .expect("the shutter queues a capture")
}

#[test]
fn screenshot_controls_are_always_half_transparent() {
    for theme in [Theme::dark(), Theme::light()] {
        let opacity = theme.tokens.colors.surfaces.see_through_opacity;
        assert!(
            opacity < 1.0,
            "the capture chrome's material lets the screen show through"
        );
        assert!((opacity - 0.5).abs() < 1e-6, "half-transparent: {opacity}");
        let ground = crate::chrome::panel_ground(&theme);
        assert!(
            crate::backdrop::see_through_fill(&theme, ground).components[3] < 1.0,
            "the bar's fill is translucent"
        );
    }
}

#[test]
fn several_selections_are_shift_on_the_selection_tool_not_a_mode() {
    let mut s = studio();
    assert_eq!(s.source_mode(), SourceMode::Region);
    let names_now = names(&s);
    assert!(
        !names_now
            .iter()
            .any(|n| n.contains("several") || n.contains("multi")),
        "there is no Several target: {names_now:?}"
    );
    assert_eq!(TARGETS.len(), 4, "Selection, Window, Screen, Strip");

    drag(
        &mut s,
        Point::new(100.0, 100.0),
        Point::new(400.0, 300.0),
        false,
    );
    assert_eq!(s.regions().len(), 1);
    // A plain drag elsewhere replaces the selection.
    drag(
        &mut s,
        Point::new(600.0, 500.0),
        Point::new(900.0, 700.0),
        false,
    );
    assert_eq!(s.regions(), &[Rect::new(600.0, 500.0, 900.0, 700.0)]);
    assert!(s.remove_rects().is_empty(), "one selection needs no remove");
    drag(
        &mut s,
        Point::new(100.0, 100.0),
        Point::new(400.0, 300.0),
        true,
    );
    assert_eq!(s.regions().len(), 2);
    // Shift inside a selection starts another rather than moving it.
    drag(
        &mut s,
        Point::new(650.0, 550.0),
        Point::new(700.0, 600.0),
        true,
    );
    assert_eq!(s.regions().len(), 3);
    // Each selection carries its own remove, just outside its top-right
    // corner (clear of the corner handle), shown while the pointer is on it.
    let removes = s.remove_rects();
    assert_eq!(removes.len(), 3);
    for (region, remove) in s.regions().iter().zip(&removes) {
        assert!(
            remove.x0 >= region.x1 && remove.y1 <= region.y0,
            "{remove:?} off {region:?}"
        );
        assert!(
            remove.x0 - region.x1 < remove.width() && region.y0 - remove.y1 < remove.height(),
            "beside its corner"
        );
    }
    s.cursor = Point::new(1500.0, 950.0);
    assert!(!s.remove_shown(0), "hidden while the pointer is elsewhere");
    s.cursor = s.regions()[0].center();
    assert!(s.remove_shown(0) && !s.remove_shown(1));
    let remove = removes[0].center();
    s.cursor = remove;
    s.click(remove, false);
    assert_eq!(s.regions().len(), 2);
    // And a plain drag on empty canvas replaces them all.
    drag(
        &mut s,
        Point::new(1200.0, 100.0),
        Point::new(1400.0, 300.0),
        false,
    );
    assert_eq!(s.regions().len(), 1);
}

#[test]
fn the_window_picker_is_only_there_while_window_is_chosen() {
    let mut s = studio();
    s.set_candidates(two_windows());
    for source in [SourceMode::Region, SourceMode::Display, SourceMode::Strip] {
        s.set_source_mode(source);
        let shown = names(&s);
        assert!(
            !shown
                .iter()
                .any(|n| n == "capture.window-pick" || n.starts_with("capture.app:")),
            "{source:?} shows nothing about windows: {shown:?}"
        );
    }
    s.set_source_mode(SourceMode::Window);
    let shown = names(&s);
    assert!(shown.iter().any(|n| n == "capture.window-pick"));
    // No list of windows, off screen or not, anywhere in the tool.
    assert!(
        !shown
            .iter()
            .any(|n| n.contains("picker") || n.contains("offscreen")),
        "{shown:?}"
    );
}

#[test]
fn a_window_capture_is_the_whole_window_with_what_is_off_screen() {
    let mut s = studio();
    let windows = two_windows();
    s.set_candidates(windows.clone());
    s.set_source_mode(SourceMode::Window);
    // Files is half off screen; pick it by clicking it.
    s.click(Point::new(1200.0, 500.0), false);
    assert_eq!(s.take_window_pick().as_deref(), Some("files"));
    let intent = the_intent(&mut s);
    assert_eq!(intent.source, SourceMode::Window);
    assert_eq!(
        intent.targets,
        vec![windows[1].clone()],
        "the whole window is captured, not its visible part"
    );
    assert!(intent.anchors.is_empty());
    assert_eq!(intent.destination, CaptureDestination::FileAndClipboard);

    // A window wholly off screen is still captured whole.
    let mut s = studio();
    let mut away = window("away", Rect::ZERO, Rect::new(1700.0, 40.0, 2600.0, 1000.0));
    away.focused = true;
    s.set_candidates(vec![away.clone()]);
    s.set_source_mode(SourceMode::Window);
    let intent = the_intent(&mut s);
    assert_eq!(intent.targets, vec![away]);
    assert!(intent.regions.is_empty());
}

#[test]
fn the_picked_window_follows_focus_moved_the_usual_way() {
    let mut s = studio();
    let mut windows = two_windows();
    s.set_candidates(windows.clone());
    s.set_source_mode(SourceMode::Window);
    assert_eq!(s.selected_target.as_deref(), Some("notes"));
    assert!(
        s.lets_desktop_navigation_through(),
        "the keys and the strip move focus while a window is chosen"
    );
    // Focus moved to Files with the keyboard.
    windows[0].focused = false;
    windows[1].focused = true;
    s.set_candidates(windows);
    assert_eq!(s.selected_target.as_deref(), Some("files"));
    s.set_source_mode(SourceMode::Region);
    assert!(!s.lets_desktop_navigation_through());
}

#[test]
fn a_selection_inside_a_window_is_anchored_to_it_and_one_across_windows_is_a_screen_region() {
    let mut s = studio();
    let windows = two_windows();
    s.set_candidates(windows.clone());
    drag(
        &mut s,
        Point::new(100.0, 140.0),
        Point::new(500.0, 440.0),
        false,
    );
    let intent = the_intent(&mut s);
    assert_eq!(
        intent.anchors,
        vec![RegionAnchor {
            target: "notes".into(),
            local: Rect::new(100.0, 100.0, 500.0, 400.0),
        }],
        "the selection is in Notes' own coordinates"
    );
    assert_eq!(intent.targets, vec![windows[0].clone()]);

    // A selection spanning both windows is a region of the screen.
    let mut s = studio();
    s.set_candidates(two_windows());
    drag(
        &mut s,
        Point::new(800.0, 140.0),
        Point::new(1100.0, 440.0),
        false,
    );
    let intent = the_intent(&mut s);
    assert!(intent.anchors.is_empty());
    assert!(intent.targets.is_empty());
    assert_eq!(intent.regions, vec![Rect::new(800.0, 140.0, 1100.0, 440.0)]);
}

#[test]
fn a_pip_of_a_window_names_the_window_not_a_screen_rect() {
    let mut s = studio();
    let windows = two_windows();
    s.set_candidates(windows.clone());
    s.set_mode(CaptureMode::Pip);
    s.set_source_mode(SourceMode::Window);
    let intent = the_intent(&mut s);
    assert_eq!(intent.mode, CaptureMode::Pip);
    assert_eq!(intent.destination, CaptureDestination::FloatingWindow);
    assert_eq!(intent.source, SourceMode::Window);
    assert_eq!(intent.targets[0].id, "notes", "the window itself, by id");
}

#[test]
fn the_bar_is_two_choices_the_shutter_and_options_and_nothing_else() {
    let mut s = studio();
    let items: Vec<BarItem> = s.bar_items().into_iter().map(|(item, _)| item).collect();
    assert_eq!(
        items,
        vec![
            BarItem::Output(0),
            BarItem::Output(1),
            BarItem::Output(2),
            BarItem::Shutter,
            BarItem::Target(0),
            BarItem::Target(1),
            BarItem::Target(2),
            BarItem::Target(3),
            BarItem::Options,
        ]
    );
    let bar = s.setup_bar();
    for (item, rect) in s.bar_items() {
        assert!(
            bar.contains(rect.origin()) && bar.contains(Point::new(rect.x1, rect.y1)),
            "{item:?} inside the bar"
        );
    }
    // No shortcut helpers anywhere in the tool: a labelled button has no
    // tooltip, and the shutter's names it alone, without a keycap.
    s.cursor = s.bar_item(BarItem::Output(1)).center();
    assert_eq!(s.bar_tip(), None, "the Recording button carries its label");
    s.cursor = s.bar_item(BarItem::Target(2)).center();
    assert_eq!(s.bar_tip(), None, "the Screen button carries its label");
    s.cursor = s.bar_item(BarItem::Output(2)).center();
    let (_, tip) = s.bar_tip().expect("the short PiP label is spelled out");
    assert_eq!(
        tip,
        studio_text("widget-capture-mode-pip-name"),
        "no shortcut"
    );
    s.cursor = s.bar_item(BarItem::Shutter).center();
    let (_, tip) = s.bar_tip().expect("the unlabelled shutter is named");
    assert_eq!(tip, s.shutter_name(), "the name alone, no shortcut");
    assert!(names(&s).iter().all(|n| !n.contains("hint")));
    // Enter and Space are the shutter.
    s.set_source_mode(SourceMode::Display);
    assert!(s.key(&Key::Named(NamedKey::Space), false));
    assert_eq!(s.take_intents().len(), 1);
}

#[test]
fn the_shutter_is_the_centre_of_the_bar_whatever_is_chosen() {
    let mut s = studio();
    let first = s.bar_item(BarItem::Shutter);
    for mode in OUTPUTS {
        for source in TARGETS {
            s.set_mode(mode);
            s.set_source_mode(source);
            let bar = s.setup_bar();
            let shutter = s.bar_item(BarItem::Shutter);
            assert!(
                (shutter.center().x - bar.center().x).abs() < 0.5,
                "{mode:?}/{source:?}: shutter at {shutter:?}, bar {bar:?}"
            );
            assert_eq!(
                shutter, first,
                "{mode:?}/{source:?}: the shutter never moves"
            );
            for (item, rect) in s.bar_items() {
                match item {
                    BarItem::Output(_) => assert!(rect.x1 < shutter.x0, "{item:?} left of it"),
                    BarItem::Target(_) | BarItem::Options => {
                        assert!(rect.x0 > shutter.x1, "{item:?} right of it");
                    }
                    _ => {}
                }
            }
        }
    }
}

#[test]
fn the_bar_moves_away_from_what_is_chosen() {
    let mut s = studio();
    let at_rest = s.setup_bar();
    assert!(at_rest.y0 > SCREEN.center().y, "bottom centre at rest");
    drag(
        &mut s,
        Point::new(400.0, 600.0),
        Point::new(1200.0, 990.0),
        false,
    );
    let moved = s.setup_bar();
    assert!(moved.y1 < SCREEN.center().y, "the bar went to the top");
    assert!(moved.intersect(s.regions()[0]).area() <= 0.0);
}

#[test]
fn rare_settings_live_in_the_options_popover() {
    let mut s = studio();
    s.system_audio_available = true;
    s.microphone_available = true;
    let choices = |s: &CaptureStudio| -> Vec<OptionChoice> {
        s.option_sections()
            .into_iter()
            .flat_map(|(_, rows)| rows.into_iter().map(|(_, choice, _)| choice))
            .collect()
    };
    let shot = choices(&s);
    assert!(shot.contains(&OptionChoice::Delay(3)));
    assert!(shot.contains(&OptionChoice::Pointer));
    assert!(!shot.contains(&OptionChoice::Microphone));
    // Combining selections is offered once there are several to combine.
    let combine = |s: &CaptureStudio| choices(s).contains(&OptionChoice::Layout(PipLayout::Auto));
    assert!(!combine(&s));
    drag(
        &mut s,
        Point::new(100.0, 100.0),
        Point::new(300.0, 300.0),
        false,
    );
    assert!(!combine(&s), "one selection has nothing to combine");
    drag(
        &mut s,
        Point::new(500.0, 100.0),
        Point::new(700.0, 300.0),
        true,
    );
    assert!(combine(&s));
    s.set_mode(CaptureMode::Recording);
    let recording = choices(&s);
    assert!(recording.contains(&OptionChoice::SystemAudio));
    assert!(recording.contains(&OptionChoice::Microphone));
    assert!(recording.contains(&OptionChoice::KeepRecent));
    s.set_source_mode(SourceMode::Window);
    assert!(
        !choices(&s)
            .iter()
            .any(|choice| matches!(choice, OptionChoice::Layout(_))),
        "combining selections only applies to selections"
    );
    // Several selections are arranged as they are unless asked otherwise.
    assert_eq!(CaptureStudio::default().pip_layout, PipLayout::Auto);
}

#[test]
fn while_recording_the_bar_is_the_stop_shutter_and_the_time() {
    let mut s = studio();
    s.set_mode(CaptureMode::Recording);
    s.set_source_mode(SourceMode::Display);
    let intent = the_intent(&mut s);
    assert_eq!(intent.recording_command, Some(RecordingCommand::Start));
    assert!(s.is_recording_start_pending());
    s.set_recording_active(true);
    assert!(
        s.modal_capture().is_none(),
        "recording leaves the desktop usable"
    );
    let items: Vec<BarItem> = s.bar_items().into_iter().map(|(item, _)| item).collect();
    assert_eq!(items, vec![BarItem::Time, BarItem::Shutter]);
    assert_eq!(s.shutter_kind(), CaptureKind::Recording);
    // The bar can be moved out of the way.
    let before = s.status_bar();
    s.cursor = Point::new(before.x0 + 4.0, before.center().y);
    s.press_status(s.cursor);
    s.cursor = Point::new(before.x0 + 4.0, before.center().y - 300.0);
    assert!(s.handle_status_event(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(0.0, 0.0),
    }));
    assert!((s.status_bar().y0 - (before.y0 - 300.0)).abs() < 1e-6);
    // The shutter stops it.
    let stop = s.bar_item(BarItem::Shutter).center();
    s.press_status(stop);
    let stop_intent = s.take_intents().pop().expect("stop");
    assert_eq!(stop_intent.recording_command, Some(RecordingCommand::Stop));
}

#[test]
fn every_screenshot_target_holds_the_windows_and_recording_stays_live() {
    let mut s = studio();
    s.set_candidates(two_windows());
    for source in TARGETS {
        s.set_source_mode(source);
        assert!(
            s.is_choosing_screenshot(),
            "{source:?}: the windows stay held, so changing the target never ends the hold"
        );
    }
    s.set_mode(CaptureMode::Recording);
    assert!(!s.is_choosing_screenshot(), "a recording stays live");
    s.set_mode(CaptureMode::Pip);
    assert!(
        !s.is_choosing_screenshot(),
        "a picture in picture stays live"
    );
}
