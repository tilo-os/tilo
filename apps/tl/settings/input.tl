/// How this person's keyboards and pointing devices behave: the pointer's speed and its
/// acceleration, the touchpad's gestures, key repeat and the keyboard layouts this device has.
view input_pane() {
    let i = settings.input
    let d = settings.input_device
    let hardware = device.input
    let screens = device.displays
    let shown = d.keyboard_preview ?? i.keyboard_layout
    ui.Section("Pointing") {
        setting(
            "Pointer speed",
            "How far the pointer travels for a movement of the device",
            ui.Slider(
                bind: i.pointer_speed,
                label: "Pointer speed",
                min: -1.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            ),
        )
        setting(
            "Scroll speed",
            "How far a scroll travels for one notch",
            ui.Slider(
                bind: i.scroll_speed,
                label: "Scroll speed",
                min: 0.2,
                max: 3.0,
                step: 0.1,
                shows_value: true,
            ),
        )
        setting(
            "Natural scrolling",
            "A swipe up moves the page up, as on a touch screen",
            ui.Toggle(bind: i.touchpad_natural_scroll),
        )
        setting(
            "Natural scrolling with a mouse",
            "A turn of the wheel moves the page up",
            ui.Toggle(bind: i.mouse_natural_scroll),
        )
        setting(
            "Left-handed buttons",
            "The outer button is the main one",
            ui.Toggle(bind: i.left_handed),
        )
        setting(
            "Acceleration",
            "How the pointer's speed grows as it is moved faster",
            ui.TextInput(bind: i.accel_profile, placeholder: "adaptive"),
        )
    }
    ui.Section("Touchpad") {
        setting(
            "The touchpad",
            "Take presses and movements from it",
            ui.Toggle(bind: d.touchpad_enabled),
        )
        setting(
            "Tap to click",
            "A tap presses as a click does",
            ui.Toggle(bind: i.tap_to_click),
        )
        setting(
            "Tap and drag",
            "A finger put down again drags what is under it",
            ui.Toggle(bind: i.tap_drag),
        )
        setting(
            "Hold to drag",
            "A finger kept down drags without pressing",
            ui.Toggle(bind: i.tap_drag_lock),
        )
        setting(
            "Ignore the touchpad while typing",
            "A palm resting on it does not move the pointer",
            ui.Toggle(bind: i.disable_while_typing),
        )
        setting(
            "Middle click from both buttons",
            "Pressing both buttons is a middle press",
            ui.Toggle(bind: i.middle_emulation),
        )
        setting(
            "Gestures",
            "Three and four fingers swipe between what shows",
            ui.Toggle(bind: i.touchpad_gestures),
        )
        setting(
            "How a press counts",
            "Buttons under the pad, or a press of the whole pad",
            ui.EnumField(bind: i.touchpad_click_method, label: "Touchpad click"),
        )
        setting(
            "How it scrolls",
            "Two fingers, or an edge of the pad",
            ui.EnumField(bind: i.touchpad_scroll_method, label: "Touchpad scroll"),
        )
        setting(
            "Not with a mouse plugged in",
            "Leave the touchpad alone while a mouse is connected",
            ui.Toggle(bind: d.touchpad_disabled_on_external_mouse),
        )
        setting(
            "Touchpad speed",
            "Its own speed, where the device keeps one; empty follows the pointer's",
            ui.Slider(
                bind: i.touchpad_pointer_speed,
                label: "Touchpad speed",
                min: -1.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            ),
        )
    }
    ui.Section("Mouse") {
        setting(
            "Mouse speed",
            "Its own speed, where the device keeps one; empty follows the pointer's",
            ui.Slider(
                bind: i.mouse_pointer_speed,
                label: "Mouse speed",
                min: -1.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            ),
        )
    }
    ui.Section("Keyboard") {
        setting(
            "Key repeat",
            "How fast a key held down repeats",
            ui.Slider(
                bind: i.key_repeat,
                label: "Key repeat",
                min: 0.2,
                max: 3.0,
                step: 0.1,
                shows_value: true,
            ),
        )
        setting(
            "Before it repeats",
            "Milliseconds a key is held before it starts repeating",
            ui.NumberInput(
                bind: i.key_repeat_delay_ms,
                label: "Milliseconds",
                min: 100.0,
                max: 1000.0,
                step: 25.0,
            ),
        )
        setting(
            "Repeats each second",
            "How many times a held key repeats in a second",
            ui.NumberInput(
                bind: i.key_repeat_rate_hz,
                label: "Times a second",
                min: 2.0,
                max: 60.0,
                step: 1.0,
            ),
        )
        setting(
            "Its model",
            "What kind of keyboard this device has",
            ui.TextInput(bind: d.keyboard_model, placeholder: "The device's own"),
        )
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
        let shared = d.keyboard_assignments.get("all")
        for layout in i.keyboard_layouts {
            let options = shared?.options.get(layout)
            row(gap: .related, align: .center, wrap: true) {
                ui.Text(layout, role: .caption, secondary: true)
                ui.MenuButton("{layout}: its model") {
                    for model in ["pc104", "pc105", "jp106", "kr106"] {
                        ui.MenuItem(model, checked: options?.model == .some(model)) {
                            on choose: settings.set_layout_option(
                                settings.input_device,
                                keyboard: "all",
                                layout: layout,
                                option: "model",
                                value: .some(model),
                            )
                        }
                    }
                    ui.MenuItem("The device's own", checked: options?.model == .none) {
                        on choose: settings.set_layout_option(
                            settings.input_device,
                            keyboard: "all",
                            layout: layout,
                            option: "model",
                            value: .none,
                        )
                    }
                }
                ui.MenuButton("{layout}: its compose key") {
                    for key in ["ralt", "menu", "caps", "rwin", "lwin"] {
                        ui.MenuItem(compose_words(key), checked: options?.compose_key == .some(key)) {
                            on choose: settings.set_layout_option(
                                settings.input_device,
                                keyboard: "all",
                                layout: layout,
                                option: "compose",
                                value: .some(key),
                            )
                        }
                    }
                    ui.MenuItem("No compose key", checked: options?.compose_key == .none) {
                        on choose: settings.set_layout_option(
                            settings.input_device,
                            keyboard: "all",
                            layout: layout,
                            option: "compose",
                            value: .none,
                        )
                    }
                }
                ui.MenuButton("{layout}: its caps lock") {
                    for key in ["ctrl", "escape", "backspace"] {
                        ui.MenuItem(caps_words(key), checked: options?.caps_lock == .some(key)) {
                            on choose: settings.set_layout_option(
                                settings.input_device,
                                keyboard: "all",
                                layout: layout,
                                option: "caps",
                                value: .some(key),
                            )
                        }
                    }
                    ui.MenuItem("Caps Lock", checked: options?.caps_lock == .none) {
                        on choose: settings.set_layout_option(
                            settings.input_device,
                            keyboard: "all",
                            layout: layout,
                            option: "caps",
                            value: .none,
                        )
                    }
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button(
                "The live keyboard",
                size: .small,
                toggled: d.keyboard_preview == .none,
            ) {
                on press: settings.show_keyboard_layout(settings.input_device, layout: .none)
            }
            for layout in i.keyboard_layouts {
                ui.Button(layout, size: .small, toggled: shown == layout) {
                    on press: settings.show_keyboard_layout(settings.input_device, layout: layout)
                }
            }
        }
        ui.KeyboardMap(
            layout: shown,
            model: d.keyboard_model,
            compact: true,
            label: "The keys this layout prints",
        )
        ui.Text(
            "The device's layouts, as the keyboard names them: us, de, fr.",
            role: .caption,
            secondary: true,
        )
    }
    ui.Section("Tablets and pens") {
        let pens = hardware.devices.filter(a -> a.kind == "tablet")
        let pads = hardware.devices.filter(a -> a.kind == "tablet_pad")
        if pens.is_empty() and pads.is_empty() {
            ui.Text(
                "No drawing tablet is attached. One's buttons, dials and pen are set here while it is.",
                role: .caption,
                secondary: true,
            )
        }
        for pen in pens {
            let configured = d.tablets.get(pen.key)
            ui.Text(pen.name, role: .title)
            if let tablet = configured {
                ui.Text("Where the pen draws", role: .caption, secondary: true)
                row(gap: .related, align: .center, wrap: true) {
                    for display in screens.displays {
                        ui.Button(
                            display.name,
                            size: .small,
                            toggled: tablet.output == .some(display.connector),
                        ) {
                            on press: settings.set_tablet_output(
                                settings.input_device,
                                device: pen.key,
                                output: display.connector,
                            )
                        }
                    }
                    ui.Button("All of them", size: .small, toggled: tablet.output == .none) {
                        on press: settings.set_tablet_output(
                            settings.input_device,
                            device: pen.key,
                            output: "desktop",
                        )
                    }
                }
                row(gap: .related, align: .center, wrap: true) {
                    ui.Button(
                        "Keep its shape",
                        size: .small,
                        toggled: tablet.keep_aspect,
                    ) {
                        on press: settings.set_tablet_keep_aspect(
                            settings.input_device,
                            device: pen.key,
                            on: not tablet.keep_aspect,
                        )
                    }
                    ui.Button("Left hand", size: .small, toggled: tablet.left_handed) {
                        on press: settings.set_tablet_left_handed(
                            settings.input_device,
                            device: pen.key,
                            on: not tablet.left_handed,
                        )
                    }
                }
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text("How firmly it draws", role: .caption, secondary: true)
                    for feel in ["Soft", "Even", "Firm"] {
                        ui.Button(feel, size: .small) {
                            on press: settings.set_tablet_tip_feel(
                                settings.input_device,
                                device: pen.key,
                                feel: feel_value(feel),
                            )
                        }
                    }
                }
                row(gap: .related, align: .center, wrap: true) {
                    pen_menu(pen.key, "lower", "Lower button", tablet.lower_button)
                    pen_menu(pen.key, "upper", "Upper button", tablet.upper_button)
                    pen_menu(pen.key, "third", "Third button", tablet.third_button)
                    ui.MenuButton("Eraser: " + eraser_name(tablet.eraser)) {
                        for choice in ["native", "left", "middle", "right", "back", "forward"] {
                            ui.MenuItem(eraser_words(choice), checked: eraser_is(tablet.eraser, choice)) {
                                on choose: settings.set_tablet_eraser(
                                    settings.input_device,
                                    device: pen.key,
                                    action: choice,
                                )
                            }
                        }
                    }
                }
            }
        }
        for pad in pads {
            let configured = d.tablets.get(pad.key)
            ui.Text(pad.name, role: .title)
            ui.Text(
                "{pad.pad_buttons} buttons, {pad.pad_rings.len()} rings and {pad.pad_strips.len()} strips.",
                role: .caption,
                secondary: true,
            )
            if let tablet = configured {
                row(gap: .related, align: .center, wrap: true) {
                    for button in tablet.pad_buttons.keys() {
                        ui.MenuButton("Button {button}") {
                            ui.MenuItem("Show the map while held", checked: tablet.pad_map_button == .some(button.to_int() ?? 0)) {
                                on choose: settings.set_pad_button(
                                    settings.input_device,
                                    device: pad.key,
                                    button: button.to_int() ?? 0,
                                    action: "show-map",
                                )
                            }
                            ui.MenuItem("Nothing", checked: not (tablet.pad_map_button == .some(button.to_int() ?? 0))) {
                                on choose: settings.set_pad_button(
                                    settings.input_device,
                                    device: pad.key,
                                    button: button.to_int() ?? 0,
                                    action: "none",
                                )
                            }
                        }
                    }
                }
                for ring in tablet.pad_rings.keys() {
                    ui.Text("Ring {ring} turns as the app under the pen takes it.", role: .caption, secondary: true)
                }
                for strip in tablet.pad_strips.keys() {
                    ui.Text("Strip {strip} slides as the app under the pen takes it.", role: .caption, secondary: true)
                }
            }
        }
    }
    ui.Section("Paste shortcuts") {
        if i.paste_shortcuts.keys().is_empty() {
            ui.Text(
                "Every app pastes with the desktop's own shortcut.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// What a pen barrel button does, as the menu that sets it names it.
fn pen_name(action: settings.StylusButtonAction) -> Text {
    if action == .left {
        "Left click"
    } else if action == .middle {
        "Middle click"
    } else if action == .right {
        "Right click"
    } else if action == .back {
        "Back"
    } else if action == .forward {
        "Forward"
    } else if action == .disabled {
        "Nothing"
    } else if action == .default {
        "As it is"
    } else {
        "A shortcut"
    }
}

/// The pen buttons a menu offers: the clicks, and nothing. A shortcut is kept, and offered by
/// the shortcut it runs.
fn pen_choices() -> List<Text> {
    ["default", "left", "middle", "right", "back", "forward", "disabled"]
}

/// Whether `action` is `choice`, by the ids the pen-button verb takes.
fn pen_is(action: settings.StylusButtonAction, choice: Text) -> Bool {
    if choice == "default" {
        action == .default
    } else if choice == "left" {
        action == .left
    } else if choice == "middle" {
        action == .middle
    } else if choice == "right" {
        action == .right
    } else if choice == "back" {
        action == .back
    } else if choice == "forward" {
        action == .forward
    } else if choice == "disabled" {
        action == .disabled
    } else {
        false
    }
}

/// What the eraser end does, as the menu that sets it names it.
fn eraser_name(action: settings.EraserAction) -> Text {
    if action == .left {
        "Left click"
    } else if action == .middle {
        "Middle click"
    } else if action == .right {
        "Right click"
    } else if action == .back {
        "Back"
    } else if action == .forward {
        "Forward"
    } else if action == .native {
        "Erase"
    } else {
        "A shortcut"
    }
}

/// Whether `action` is `choice`, by the ids the eraser verb takes.
fn eraser_is(action: settings.EraserAction, choice: Text) -> Bool {
    if choice == "native" {
        action == .native
    } else if choice == "left" {
        action == .left
    } else if choice == "middle" {
        action == .middle
    } else if choice == "right" {
        action == .right
    } else if choice == "back" {
        action == .back
    } else if choice == "forward" {
        action == .forward
    } else {
        false
    }
}

/// One of a pen's buttons, as a menu of what it can do.
view pen_menu(key: Text, which: Text, title: Text, now: settings.StylusButtonAction) {
    ui.MenuButton("{title}: {pen_name(now)}") {
        for choice in pen_choices() {
            ui.MenuItem(pen_words(choice), checked: pen_is(now, choice)) {
                on choose: settings.set_tablet_button(
                    settings.input_device,
                    device: key,
                    button: which,
                    action: choice,
                )
            }
        }
    }
}

/// How firm a tip feel is, from soft to firm.
fn feel_value(feel: Text) -> Float {
    if feel == "Soft" {
        0.2
    } else if feel == "Firm" {
        0.8
    } else {
        0.5
    }
}

/// What a compose key choice runs, as the menu names it.
fn compose_words(key: Text) -> Text {
    if key == "ralt" {
        "Right Alt"
    } else if key == "menu" {
        "Menu"
    } else if key == "caps" {
        "Caps Lock"
    } else if key == "rwin" {
        "Right Super"
    } else if key == "lwin" {
        "Left Super"
    } else {
        key
    }
}

/// What a caps-lock choice does, as the menu names it.
fn caps_words(key: Text) -> Text {
    if key == "ctrl" {
        "Control"
    } else if key == "escape" {
        "Escape"
    } else if key == "backspace" {
        "Backspace"
    } else {
        key
    }
}

/// A pen-button choice's id in words.
fn pen_words(choice: Text) -> Text {
    if choice == "left" {
        "Left click"
    } else if choice == "middle" {
        "Middle click"
    } else if choice == "right" {
        "Right click"
    } else if choice == "back" {
        "Back"
    } else if choice == "forward" {
        "Forward"
    } else if choice == "disabled" {
        "Nothing"
    } else {
        "As it is"
    }
}

/// An eraser choice's id in words.
fn eraser_words(choice: Text) -> Text {
    if choice == "left" {
        "Left click"
    } else if choice == "middle" {
        "Middle click"
    } else if choice == "right" {
        "Right click"
    } else if choice == "back" {
        "Back"
    } else if choice == "forward" {
        "Forward"
    } else {
        "Erase"
    }
}
