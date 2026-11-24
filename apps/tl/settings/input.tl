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
}

/// What a stored paste shortcut is called.
fn paste_words(shortcut: Text) -> Text {
    if shortcut == "ctrl_shift_v" { "Pastes with Ctrl+Shift+V" } else { "Pastes with Ctrl+V" }
}

/// The stored name of the chord a person picked.
fn paste_key(chord: Text) -> Text {
    if chord == "Ctrl+Shift+V" { "ctrl_shift_v" } else { "ctrl_v" }
}
