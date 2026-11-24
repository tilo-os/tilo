/// How this person's keyboards and pointing devices behave: the pointer's speed and its
/// acceleration, the touchpad's gestures, key repeat and the keyboard layouts this device has.
view input_pane() {
    state paste_app: Text = ""
    state paste_chord: Text = "Ctrl+V"
    let i = settings.input
    let d = settings.input_device
    ui.Section("Pointing") {
        setting("Pointer speed", "How far the pointer travels for a movement of the device") {
            ui.Slider(
                bind: i.pointer_speed,
                label: "Pointer speed",
                min: -1.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            )
        }
        setting("Scroll speed", "How far a scroll travels for one notch") {
            ui.Slider(
                bind: i.scroll_speed,
                label: "Scroll speed",
                min: 0.2,
                max: 3.0,
                step: 0.1,
                shows_value: true,
            )
        }
        setting("Natural scrolling", "A swipe up moves the page up, as on a touch screen") {
            ui.Toggle(bind: i.touchpad_natural_scroll)
        }
        setting("Natural scrolling with a mouse", "A turn of the wheel moves the page up") {
            ui.Toggle(bind: i.mouse_natural_scroll)
        }
        setting("Left-handed buttons", "The outer button is the main one") {
            ui.Toggle(bind: i.left_handed)
        }
        setting("Acceleration", "How the pointer's speed grows as it is moved faster") {
            ui.TextInput(bind: i.accel_profile, placeholder: "adaptive")
        }
    }
    ui.Section("Touchpad") {
        setting("The touchpad", "Take presses and movements from it") {
            ui.Toggle(bind: d.touchpad_enabled)
        }
        setting("Tap to click", "A tap presses as a click does") {
            ui.Toggle(bind: i.tap_to_click)
        }
        setting("Tap and drag", "A finger put down again drags what is under it") {
            ui.Toggle(bind: i.tap_drag)
        }
        setting("Hold to drag", "A finger kept down drags without pressing") {
            ui.Toggle(bind: i.tap_drag_lock)
        }
        setting(
            "Ignore the touchpad while typing",
            "A palm resting on it does not move the pointer",
        ) {
            ui.Toggle(bind: i.disable_while_typing)
        }
        setting("Middle click from both buttons", "Pressing both buttons is a middle press") {
            ui.Toggle(bind: i.middle_emulation)
        }
        setting("Gestures", "Three and four fingers swipe between what shows") {
            ui.Toggle(bind: i.touchpad_gestures)
        }
        setting("How a press counts", "Buttons under the pad, or a press of the whole pad") {
            ui.EnumField(bind: i.touchpad_click_method, label: "Touchpad click")
        }
        setting("How it scrolls", "Two fingers, or an edge of the pad") {
            ui.EnumField(bind: i.touchpad_scroll_method, label: "Touchpad scroll")
        }
        setting(
            "Not with a mouse plugged in",
            "Leave the touchpad alone while a mouse is connected",
        ) {
            ui.Toggle(bind: d.touchpad_disabled_on_external_mouse)
        }
        setting(
            "Touchpad speed",
            "Its own speed, where the device keeps one; empty follows the pointer's",
        ) {
            ui.Slider(
                bind: i.touchpad_pointer_speed,
                label: "Touchpad speed",
                min: -1.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            )
        }
    }
    ui.Section("Mouse") {
        setting(
            "Mouse speed",
            "Its own speed, where the device keeps one; empty follows the pointer's",
        ) {
            ui.Slider(
                bind: i.mouse_pointer_speed,
                label: "Mouse speed",
                min: -1.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            )
        }
    }
    ui.Section("Keyboard") {
        setting("Key repeat", "How fast a key held down repeats") {
            ui.Slider(
                bind: i.key_repeat,
                label: "Key repeat",
                min: 0.2,
                max: 3.0,
                step: 0.1,
                shows_value: true,
            )
        }
        setting("Before it repeats", "Milliseconds a key is held before it starts repeating") {
            ui.NumberInput(
                bind: i.key_repeat_delay_ms,
                label: "Milliseconds",
                min: 100.0,
                max: 1000.0,
                step: 25.0,
            )
        }
        setting("Repeats each second", "How many times a held key repeats in a second") {
            ui.NumberInput(
                bind: i.key_repeat_rate_hz,
                label: "Times a second",
                min: 2.0,
                max: 60.0,
                step: 1.0,
            )
        }
        setting("Its model", "What kind of keyboard this device has") {
            ui.TextInput(bind: d.keyboard_model, placeholder: "The device's own")
        }
    }
    ui.Section("Keyboard layout") {
        if i.keyboard_layouts.is_empty() {
            ui.Text("This device offers one layout.", role: .caption, secondary: true)
        }
        for layout in i.keyboard_layouts {
            ui.ListRow(
                layout,
                secondary: if layout == i.keyboard_layout { "In use" } else { "" },
                selected: layout == i.keyboard_layout,
                icon: .keyboard,
            ) {
                on activate: settings.choose_layout(settings.input, layout: layout)
            }
        }
        ui.Text(
            "The device's layouts, as the keyboard names them: us, de, fr.",
            role: .caption,
            secondary: true,
        )
    }
    ui.Section("Paste shortcuts") {
        ui.Text(
            "When the desktop pastes for an app, this is the chord it presses there. A terminal pastes with Ctrl+Shift+V unless it is given another.",
            role: .caption,
            secondary: true,
        )
        if i.paste_shortcuts.keys().is_empty() {
            ui.Text("No app has a chord of its own.", role: .caption, secondary: true)
        }
        for app_id in i.paste_shortcuts.keys() {
            if let chosen = i.paste_shortcuts.get(app_id) {
                ui.ListRow(app_id, secondary: paste_words(chosen), icon: .clipboard) {
                    trailing: {
                        ui.Button("Use Ctrl+V", size: .small) {
                            on press: settings.set_paste_shortcut(
                                settings.input,
                                app: app_id,
                                shortcut: "ctrl_v",
                            )
                        }
                    }
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.TextInput(
                bind: paste_app,
                placeholder: "An app's id, such as org.example.MyTerm",
                fill,
            )
            ui.Dropdown(
                bind: paste_chord,
                options: ["Ctrl+V", "Ctrl+Shift+V"],
                caption: "It pastes with",
            )
            ui.Button("Set it", role: .primary, size: .small) {
                on press: settings.set_paste_shortcut(
                    settings.input,
                    app: paste_app,
                    shortcut: paste_key(paste_chord),
                )
            }
        }
    }
    ui.Section("Drawing tablets") {
        let tablets = device.input_devices.devices.filter(
            x -> x.kind == .tablet or x.kind == .tablet_pad,
        )
        if tablets.is_empty() {
            ui.Text("No drawing tablet is attached.", role: .caption, secondary: true)
        }
        let screens = device.displays.displays
        for tablet in tablets {
            let mapping = d.tablets.get(tablet.key)
            ui.ListRow(
                tablet.name,
                secondary: if tablet.kind == .tablet_pad {
                    "Its buttons, rings and strips"
                } else { "Its pen" },
                icon: .pen_tool,
            )
            setting("Maps onto", "The screen the pen reaches across") {
                ui.ToggleGroup(
                    selected: .some(output_index(screens, tablet_output(mapping))),
                    caption: "Maps onto",
                ) {
                    ui.Segment("Every display") {
                        on press: settings.set_tablet_output(
                            settings.input_device,
                            tablet.key,
                            "desktop",
                        )
                    }
                    for screen in screens {
                        ui.Segment(screen.name) {
                            on press: settings.set_tablet_output(
                                settings.input_device,
                                tablet.key,
                                screen.connector,
                            )
                        }
                    }
                }
            }
            setting("Keep the tablet's shape", "Do not stretch strokes to fit the screen") {
                ui.Button(
                    if tablet_aspect(mapping) { "Kept" } else { "Stretched" },
                    toggled: tablet_aspect(mapping),
                ) {
                    on press: settings.set_tablet_keep_aspect(
                        settings.input_device,
                        tablet.key,
                        not tablet_aspect(mapping),
                    )
                }
            }
            setting("Left-handed", "Turn it half a turn for its buttons on the right") {
                ui.Button(
                    if tablet_left_handed(mapping) { "Turned" } else { "As it is" },
                    toggled: tablet_left_handed(mapping),
                ) {
                    on press: settings.set_tablet_left_handed(
                        settings.input_device,
                        tablet.key,
                        not tablet_left_handed(mapping),
                    )
                }
            }
            if tablet.kind == .tablet {
                setting("Tip feel", "How firmly the pen must press") {
                    ui.ToggleGroup(selected: .some(tip_feel(mapping)), caption: "Tip feel") {
                        ui.Segment("Softest") {
                            on press: settings.set_tablet_tip_feel(
                                settings.input_device,
                                tablet.key,
                                0.0,
                            )
                        }
                        ui.Segment("Soft") {
                            on press: settings.set_tablet_tip_feel(
                                settings.input_device,
                                tablet.key,
                                0.17,
                            )
                        }
                        ui.Segment("Softer") {
                            on press: settings.set_tablet_tip_feel(
                                settings.input_device,
                                tablet.key,
                                0.33,
                            )
                        }
                        ui.Segment("Even") {
                            on press: settings.set_tablet_tip_feel(
                                settings.input_device,
                                tablet.key,
                                0.5,
                            )
                        }
                        ui.Segment("Firmer") {
                            on press: settings.set_tablet_tip_feel(
                                settings.input_device,
                                tablet.key,
                                0.67,
                            )
                        }
                        ui.Segment("Firm") {
                            on press: settings.set_tablet_tip_feel(
                                settings.input_device,
                                tablet.key,
                                0.83,
                            )
                        }
                        ui.Segment("Firmest") {
                            on press: settings.set_tablet_tip_feel(
                                settings.input_device,
                                tablet.key,
                                1.0,
                            )
                        }
                    }
                }
                for button in ["lower", "upper", "third"] {
                    setting(
                        "{button_words(button)} pen button",
                        "What the button on the pen's barrel does",
                    ) {
                        ui.ToggleGroup(
                            selected: .some(pen_index(mapping, button)),
                            caption: "{button_words(button)} pen button",
                        ) {
                            ui.Segment("As itself") {
                                on press: settings.set_tablet_button(
                                    settings.input_device,
                                    tablet.key,
                                    button,
                                    "default",
                                )
                            }
                            ui.Segment("Left click") {
                                on press: settings.set_tablet_button(
                                    settings.input_device,
                                    tablet.key,
                                    button,
                                    "left",
                                )
                            }
                            ui.Segment("Middle click") {
                                on press: settings.set_tablet_button(
                                    settings.input_device,
                                    tablet.key,
                                    button,
                                    "middle",
                                )
                            }
                            ui.Segment("Right click") {
                                on press: settings.set_tablet_button(
                                    settings.input_device,
                                    tablet.key,
                                    button,
                                    "right",
                                )
                            }
                            ui.Segment("Back") {
                                on press: settings.set_tablet_button(
                                    settings.input_device,
                                    tablet.key,
                                    button,
                                    "back",
                                )
                            }
                            ui.Segment("Forward") {
                                on press: settings.set_tablet_button(
                                    settings.input_device,
                                    tablet.key,
                                    button,
                                    "forward",
                                )
                            }
                            ui.Segment("Nothing") {
                                on press: settings.set_tablet_button(
                                    settings.input_device,
                                    tablet.key,
                                    button,
                                    "disabled",
                                )
                            }
                        }
                    }
                }
                setting("The eraser end", "What turning the pen over does") {
                    ui.ToggleGroup(
                        selected: .some(eraser_index(mapping)),
                        caption: "The eraser end",
                    ) {
                        ui.Segment("Erases") {
                            on press: settings.set_tablet_eraser(
                                settings.input_device,
                                tablet.key,
                                "native",
                            )
                        }
                        ui.Segment("Left click") {
                            on press: settings.set_tablet_eraser(
                                settings.input_device,
                                tablet.key,
                                "left",
                            )
                        }
                        ui.Segment("Middle click") {
                            on press: settings.set_tablet_eraser(
                                settings.input_device,
                                tablet.key,
                                "middle",
                            )
                        }
                        ui.Segment("Right click") {
                            on press: settings.set_tablet_eraser(
                                settings.input_device,
                                tablet.key,
                                "right",
                            )
                        }
                        ui.Segment("Back") {
                            on press: settings.set_tablet_eraser(
                                settings.input_device,
                                tablet.key,
                                "back",
                            )
                        }
                        ui.Segment("Forward") {
                            on press: settings.set_tablet_eraser(
                                settings.input_device,
                                tablet.key,
                                "forward",
                            )
                        }
                    }
                }
            }
            if tablet.pad_buttons > 0 {
                ui.Text("Its pad's buttons", role: .caption, secondary: true)
                for button in range_of(tablet.pad_buttons) {
                    ui.ListRow(
                        "Button {button + 1}",
                        secondary: pad_button_words(mapping, button),
                        icon: .tablet,
                    ) {
                        trailing: {
                            row(gap: .tight, align: .center, wrap: true) {
                                ui.Button("Show the pad's map", size: .small) {
                                    on press: settings.set_pad_button(
                                        settings.input_device,
                                        tablet.key,
                                        button,
                                        "show-map",
                                    )
                                }
                                ui.Button("Nothing", size: .small) {
                                    on press: settings.set_pad_button(
                                        settings.input_device,
                                        tablet.key,
                                        button,
                                        "none",
                                    )
                                }
                            }
                        }
                    }
                }
            }
            for kind in ["ring", "strip"] {
                let count = if kind == "ring" {
                    tablet.pad_rings.len()
                } else { tablet.pad_strips.len() }
                for index in range_of(count) {
                    setting(
                        if kind == "ring" { "Ring {index + 1}" } else { "Strip {index + 1}" },
                        "What turning or sliding it does",
                    ) {
                        ui.ToggleGroup(
                            selected: .some(dial_index(dial_now(mapping, kind, index))),
                            caption: if kind == "ring" {
                                "Ring {index + 1}"
                            } else { "Strip {index + 1}" },
                        ) {
                            ui.Segment("Nothing") {
                                on press: settings.set_pad_dial(
                                    settings.input_device,
                                    tablet.key,
                                    kind,
                                    index,
                                    0,
                                    "nothing",
                                )
                            }
                            ui.Segment("Scroll") {
                                on press: settings.set_pad_dial(
                                    settings.input_device,
                                    tablet.key,
                                    kind,
                                    index,
                                    0,
                                    "scroll",
                                )
                            }
                            ui.Segment("Zoom") {
                                on press: settings.set_pad_dial(
                                    settings.input_device,
                                    tablet.key,
                                    kind,
                                    index,
                                    0,
                                    "zoom",
                                )
                            }
                            ui.Segment("Brush size") {
                                on press: settings.set_pad_dial(
                                    settings.input_device,
                                    tablet.key,
                                    kind,
                                    index,
                                    0,
                                    "brush-size",
                                )
                            }
                            ui.Segment("Volume") {
                                on press: settings.set_pad_dial(
                                    settings.input_device,
                                    tablet.key,
                                    kind,
                                    index,
                                    0,
                                    "volume",
                                )
                            }
                            ui.Segment("Brightness") {
                                on press: settings.set_pad_dial(
                                    settings.input_device,
                                    tablet.key,
                                    kind,
                                    index,
                                    0,
                                    "brightness",
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The screens a tablet can map onto, `desktop` standing for all of them: which of the choices the
/// mapping picked.
fn output_index(screens: List<device.Display>, output: Text) -> Int {
    if output == "desktop" {
        return 0
    }
    (screens.map(s -> s.connector).index_of(output) ?? -1) + 1
}

/// The screen a tablet maps onto; `desktop` while it spans every display.
fn tablet_output(mapping: Option<settings.TabletConfig>) -> Text {
    if let held = mapping { held.output ?? "desktop" } else { "desktop" }
}

fn tablet_aspect(mapping: Option<settings.TabletConfig>) -> Bool {
    if let held = mapping { held.keep_aspect } else { true }
}

fn tablet_left_handed(mapping: Option<settings.TabletConfig>) -> Bool {
    if let held = mapping { held.left_handed } else { false }
}

/// What a pen's button is called on the pen: its place along the barrel.
fn button_words(button: Text) -> Text {
    if button == "lower" {
        "The lower"
    } else if button == "upper" { "The upper" } else { "The third" }
}

/// The numbers a pad offers, from zero.
fn range_of(count: Int) -> List<Int> {
    numbers(0, count, [])
}

fn numbers(at: Int, count: Int, out: List<Int>) -> List<Int> {
    if at >= count { out } else { numbers(at + 1, count, out + [at]) }
}

/// Which of the pen's choices the button at `place` takes now.
fn pen_index(mapping: Option<settings.TabletConfig>, place: Text) -> Int {
    let chosen = if let held = mapping {
        if place == "lower" {
            held.lower_button
        } else if place == "upper" { held.upper_button } else { held.third_button }
    } else { .default }
    if chosen == .default {
        0
    } else if chosen == .left {
        1
    } else if chosen == .middle {
        2
    } else if chosen == .right {
        3
    } else if chosen == .back { 4 } else if chosen == .forward { 5 } else { 6 }
}

/// Which of the eraser's choices it takes now.
fn eraser_index(mapping: Option<settings.TabletConfig>) -> Int {
    let chosen = if let held = mapping { held.eraser } else { .native }
    if chosen == .native {
        0
    } else if chosen == .left {
        1
    } else if chosen == .middle {
        2
    } else if chosen == .right { 3 } else if chosen == .back { 4 } else { 5 }
}

/// The tip feel a pressure curve stands for, counting from the softest: a curve of the pane's own
/// is the even one.
fn tip_feel(mapping: Option<settings.TabletConfig>) -> Int {
    let curve = if let held = mapping { held.pressure_curve } else { [] }
    if curve_equal(curve, [0.0, 0.75, 0.25, 1.0]) {
        0
    } else if curve_equal(curve, [0.0, 0.5, 0.5, 1.0]) {
        1
    } else if curve_equal(curve, [0.0, 0.25, 0.75, 1.0]) {
        2
    } else if curve_equal(curve, [0.0, 0.0, 1.0, 1.0]) {
        3
    } else if curve_equal(curve, [0.25, 0.0, 1.0, 0.75]) {
        4
    } else if curve_equal(curve, [0.5, 0.0, 1.0, 0.5]) {
        5
    } else if curve_equal(curve, [0.75, 0.0, 1.0, 0.25]) { 6 } else { 3 }
}

/// Whether `curve` is the preset `preset`.
fn curve_equal(curve: List<Float>, preset: List<Float>) -> Bool {
    curve.len() == preset.len()
        and curve
            .map((value, i) -> (value - (preset.at(i) ?? 0.0)).abs() < 0.001)
            .all(close -> close)
}

/// What a pad's button does now.
fn pad_button_words(mapping: Option<settings.TabletConfig>, button: Int) -> Text {
    if let held = mapping {
        if held.pad_map_button == .some(button) {
            "Shows the pad's map"
        } else if held.pad_buttons.get(button.to_text()) != .none {
            "Runs a shortcut"
        } else { "Does nothing" }
    } else { "Does nothing" }
}

/// What a ring or strip does in the modes the pane offers: the first mode's action.
fn dial_now(
    mapping: Option<settings.TabletConfig>,
    kind: Text,
    index: Int,
) -> settings.PadDialAction {
    let dial = if let held = mapping {
        if kind == "ring" {
            held.pad_rings.get(index.to_text())
        } else { held.pad_strips.get(index.to_text()) }
    } else { .none }
    if let modes = dial { modes.at(0) ?? .nothing } else { .nothing }
}

/// Which of a ring's or strip's choices it takes now.
fn dial_index(chosen: settings.PadDialAction) -> Int {
    if chosen == .nothing {
        0
    } else if chosen == .scroll {
        1
    } else if chosen == .zoom {
        2
    } else if chosen == .brush_size { 3 } else if chosen == .volume { 4 } else { 5 }
}

/// What a stored paste shortcut is called.
fn paste_words(shortcut: Text) -> Text {
    if shortcut == "ctrl_shift_v" { "Pastes with Ctrl+Shift+V" } else { "Pastes with Ctrl+V" }
}

/// The stored name of the chord a person picked.
fn paste_key(chord: Text) -> Text {
    if chord == "Ctrl+Shift+V" { "ctrl_shift_v" } else { "ctrl_v" }
}
