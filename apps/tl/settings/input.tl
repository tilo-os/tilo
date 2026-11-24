/// How this person's keyboards and pointing devices behave: the pointer's speed and its
/// acceleration, the touchpad's gestures, key repeat, the layouts each keyboard types, the keys
/// of the layout being inspected, and what Caps Lock and the compose key do.
view input_pane() {
    state paste_app: Text = ""
    state paste_chord: Text = "Ctrl+V"
    state tried: Text = ""
    state add_shared: Text = ""
    state add_own: Text = ""
    state draw_another: Text = ""
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
            ui.ToggleGroup(selected: .some(model_index(d.keyboard_model)), caption: "Its model") {
                ui.Segment("ANSI") {
                    on press: settings.set_keyboard_model(settings.input_device, model: "pc104")
                }
                ui.Segment("ISO") {
                    on press: settings.set_keyboard_model(settings.input_device, model: "pc105")
                }
                ui.Segment("JIS") {
                    on press: settings.set_keyboard_model(settings.input_device, model: "jp106")
                }
                ui.Segment("Korean") {
                    on press: settings.set_keyboard_model(settings.input_device, model: "kr106")
                }
            }
        }
        setting("Caps Lock", "What the key does: Caps Lock itself, or the key it stands in for") {
            ui.ToggleGroup(selected: .some(caps_lock_index(i.caps_lock)), caption: "Caps Lock") {
                ui.Segment("Caps Lock") {
                    on press: settings.set_caps_lock(settings.input, behaviour: "caps")
                }
                ui.Segment("Control") {
                    on press: settings.set_caps_lock(settings.input, behaviour: "ctrl")
                }
                ui.Segment("Escape") {
                    on press: settings.set_caps_lock(settings.input, behaviour: "escape")
                }
                ui.Segment("Backspace") {
                    on press: settings.set_caps_lock(settings.input, behaviour: "backspace")
                }
            }
        }
        setting(
            "Compose key",
            "The key that begins a sequence for an accent or a character the keyboard has no key for",
        ) {
            ui.ToggleGroup(selected: .some(compose_index(i.compose_key)), caption: "Compose key") {
                ui.Segment("None") {
                    on press: settings.set_compose_key(settings.input, key: "disabled")
                }
                ui.Segment("Right Alt") {
                    on press: settings.set_compose_key(settings.input, key: "ralt")
                }
                ui.Segment("Menu") {
                    on press: settings.set_compose_key(settings.input, key: "menu")
                }
                ui.Segment("Caps Lock") {
                    on press: settings.set_compose_key(settings.input, key: "caps")
                }
                ui.Segment("Right Super") {
                    on press: settings.set_compose_key(settings.input, key: "rwin")
                }
                ui.Segment("Left Super") {
                    on press: settings.set_compose_key(settings.input, key: "lwin")
                }
            }
        }
        setting("Try it", "Type here to try the layout, Caps Lock and the compose key you chose") {
            ui.TextInput(bind: tried, placeholder: "Type here")
        }
    }
    ui.Section("Keyboard layouts") {
        let k = device.keyboard
        ui.Text(
            "Each keyboard types its own list of layouts, or the shared ones. Switching between them is the desktop's own shortcut.",
            role: .caption,
            secondary: true,
        )
        if k.layouts_error != .none {
            ui.StatusLine(
                "The layouts this machine installs could not be read; a short list is offered instead.",
                status: .warning,
            )
        }
        ui.ListRow(
            "The shared layouts",
            secondary: shared_words(k.layouts, i.keyboard_layouts, i.keyboard_layout),
            icon: .keyboard,
        )
        for index in numbers(0, i.keyboard_layouts.len(), []) {
            if let layout = i.keyboard_layouts.at(index) {
                ui.ListRow(
                    layout_label(k.layouts, layout),
                    secondary: if layout == i.keyboard_layout { "Typed with" } else { "" },
                    selected: layout == i.keyboard_layout,
                    icon: .keyboard,
                ) {
                    on activate: settings.choose_layout(settings.input, layout: layout)
                    trailing: {
                        row(gap: .tight, align: .center, wrap: true) {
                            ui.Button(
                                "Type with it",
                                size: .small,
                                accessible_name: "Type with {layout_label(k.layouts, layout)}",
                            ) {
                                on press: settings.choose_layout(settings.input, layout: layout)
                            }
                            ui.Button(
                                "Earlier",
                                size: .small,
                                accessible_name: "Move {layout_label(k.layouts, layout)} earlier in the shared layouts",
                                enabled: index > 0,
                                description: "It is the first of the shared layouts.",
                            ) {
                                on press: settings.set_layouts(
                                    settings.input,
                                    layouts: moved(i.keyboard_layouts, index, index - 1),
                                    active: i.keyboard_layout,
                                )
                            }
                            ui.Button(
                                "Later",
                                size: .small,
                                accessible_name: "Move {layout_label(k.layouts, layout)} later in the shared layouts",
                                enabled: index + 1 < i.keyboard_layouts.len(),
                                description: "It is the last of the shared layouts.",
                            ) {
                                on press: settings.set_layouts(
                                    settings.input,
                                    layouts: moved(i.keyboard_layouts, index, index + 1),
                                    active: i.keyboard_layout,
                                )
                            }
                            ui.Button(
                                "Remove",
                                size: .small,
                                accessible_name: "Remove {layout_label(k.layouts, layout)} from the shared layouts",
                                enabled: i.keyboard_layouts.len() > 1,
                                description: "The shared layouts keep at least one.",
                            ) {
                                on press: settings.set_layouts(
                                    settings.input,
                                    layouts: i.keyboard_layouts.filter(x -> x != layout),
                                    active: active_without(
                                        i.keyboard_layouts,
                                        i.keyboard_layout,
                                        layout,
                                    ),
                                )
                            }
                        }
                    }
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Dropdown(
                bind: add_shared,
                options: addable_layouts(k.layouts, i.keyboard_layouts),
                placeholder: "Add a shared layout",
                caption: "Another layout",
                fill,
            )
            ui.Button(
                "Add it",
                size: .small,
                enabled: add_shared != "",
                description: "Choose a layout to add first.",
                accessible_name: "Add the chosen layout to the shared layouts",
            ) {
                on press: settings.set_layouts(
                    settings.input,
                    layouts: i.keyboard_layouts + [layout_id(k.layouts, add_shared)],
                    active: i.keyboard_layout,
                )
            }
        }
        let keyboards = device.input_devices.devices.filter(x -> x.kind == .keyboard)
        if keyboards.is_empty() {
            ui.Text(
                "No keyboard of its own is attached, so the shared layouts are typed with.",
                role: .caption,
                secondary: true,
            )
        }
        for keyboard in keyboards {
            let own = d.keyboard_assignments.get(keyboard.key)
            ui.ListRow(
                keyboard.name,
                secondary: keyboard_words(own, k.layouts, i.keyboard_layout),
                icon: .keyboard,
            )
            if own != .none {
                let own_layouts = keyboard_sources(own, i.keyboard_layouts)
                for index in numbers(0, own_layouts.len(), []) {
                    if let layout = own_layouts.at(index) {
                        ui.ListRow(
                            layout_label(k.layouts, layout),
                            secondary: if layout == keyboard_active(own, i.keyboard_layout) {
                                "Typed with"
                            } else { "" },
                            selected: layout == keyboard_active(own, i.keyboard_layout),
                            icon: .keyboard,
                        ) {
                            trailing: {
                                row(gap: .tight, align: .center, wrap: true) {
                                    ui.Button(
                                        "Earlier",
                                        size: .small,
                                        accessible_name: "Move {layout_label(k.layouts, layout)} earlier in {keyboard.name}",
                                        enabled: index > 0,
                                        description: "It is the first layout {keyboard.name} types.",
                                    ) {
                                        on press: settings.set_keyboard_sources(
                                            settings.input_device,
                                            keyboard: keyboard.key,
                                            sources: moved(own_layouts, index, index - 1),
                                            active: keyboard_active(own, i.keyboard_layout),
                                        )
                                    }
                                    ui.Button(
                                        "Later",
                                        size: .small,
                                        accessible_name: "Move {layout_label(k.layouts, layout)} later in {keyboard.name}",
                                        enabled: index + 1 < own_layouts.len(),
                                        description: "It is the last layout {keyboard.name} types.",
                                    ) {
                                        on press: settings.set_keyboard_sources(
                                            settings.input_device,
                                            keyboard: keyboard.key,
                                            sources: moved(own_layouts, index, index + 1),
                                            active: keyboard_active(own, i.keyboard_layout),
                                        )
                                    }
                                    if own_layouts.len() > 1 {
                                        ui.Button(
                                            "Remove",
                                            size: .small,
                                            accessible_name: "Remove {layout_label(k.layouts, layout)} from {keyboard.name}",
                                        ) {
                                            on press: settings.set_keyboard_sources(
                                                settings.input_device,
                                                keyboard: keyboard.key,
                                                sources: own_layouts.filter(x -> x != layout),
                                                active: keyboard_active_without(
                                                    own,
                                                    i.keyboard_layout,
                                                    layout,
                                                ),
                                            )
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Dropdown(
                    bind: add_own,
                    options: k.layouts.map(l -> l.label),
                    placeholder: "Give it a layout",
                    caption: "{keyboard.name} types",
                    fill,
                )
                ui.Button(
                    "Add it",
                    size: .small,
                    enabled: add_own != "",
                    description: "Choose a layout to add first.",
                    accessible_name: "Give {keyboard.name} the chosen layout",
                ) {
                    on press: settings.set_keyboard_sources(
                        settings.input_device,
                        keyboard: keyboard.key,
                        sources: adding_layout(
                            own,
                            i.keyboard_layouts,
                            layout_id(k.layouts, add_own),
                        ),
                        active: layout_id(k.layouts, add_own),
                    )
                }
                if own != .none {
                    ui.Button(
                        "Type the shared layouts again",
                        size: .small,
                        accessible_name: "Type the shared layouts again for {keyboard.name}",
                    ) {
                        on press: settings.use_shared_layouts(
                            settings.input_device,
                            keyboard: keyboard.key,
                        )
                    }
                }
            }
        }
    }
    ui.Section("The keyboard's keys") {
        let k = device.keyboard
        if let drawn = k.drawing {
            let level = key_level(drawn.level_count, d.keyboard_preview_level)
            ui.Text(
                "What each key types in {layout_label(k.layouts, drawn.layout)}, on the {layer_words(level)} level. Press a key to inspect it.",
                role: .caption,
                secondary: true,
            )
            ui.Drawing(
                keyboard_shapes(drawn, level, d.keyboard_inspected_shape),
                meaning: .picture,
                label: "The keys of {layout_label(k.layouts, drawn.layout)} and what they type",
                width: .some(720.0),
                height: .some(720.0 * drawn.height / drawn.width.max(1.0)),
            ) {
                on press(at: Int): settings.inspect_keyboard(
                    settings.input_device,
                    shape: at,
                    level: level,
                    extra_levels: d.keyboard_preview_extra_levels,
                )
            }
            ui.ToggleGroup(
                selected: level_choice(drawn.level_count, level, d.keyboard_preview_extra_levels),
                caption: "The level shown",
            ) {
                for index in levels_offered(drawn.level_count, d.keyboard_preview_extra_levels) {
                    ui.Segment(layer_words(index)) {
                        on press: settings.inspect_keyboard(
                            settings.input_device,
                            shape: d.keyboard_inspected_shape,
                            level: index,
                            extra_levels: d.keyboard_preview_extra_levels,
                        )
                    }
                }
            }
            if drawn.level_count > 4 {
                ui.Button(
                    if d.keyboard_preview_extra_levels {
                        "Show the usual levels"
                    } else { "Show every level" },
                    size: .small,
                ) {
                    on press: settings.inspect_keyboard(
                        settings.input_device,
                        shape: d.keyboard_inspected_shape,
                        level: 0,
                        extra_levels: not d.keyboard_preview_extra_levels,
                    )
                }
            }
            if let key = drawn.keys.at(d.keyboard_inspected_shape / 2) {
                ui.PropertyList {
                    for index in numbers(0, key.levels.len(), []) {
                        ui.Property(layer_words(index), key.levels.at(index) ?? "Nothing")
                    }
                }
            } else {
                ui.Text(
                    "Press a key of the drawing to see what it types.",
                    role: .caption,
                    secondary: true,
                )
            }
        } else if let problem = k.drawing_error {
            ui.StatusLine(problem, status: .warning)
        } else {
            ui.Text("The keyboard is being drawn.", role: .caption, secondary: true)
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Dropdown(
                bind: draw_another,
                options: k.layouts.map(l -> l.label),
                placeholder: "Preview a layout",
                caption: "Draw another layout",
                fill,
            )
            ui.Button(
                "Draw it",
                size: .small,
                enabled: draw_another != "",
                description: "Choose a layout to draw first.",
            ) {
                on press: settings.show_keyboard_layout(
                    settings.input_device,
                    layout: .some(layout_id(k.layouts, draw_another)),
                )
            }
            if d.keyboard_preview != .none {
                ui.Button("Show what the keyboard types", size: .small) {
                    on press: settings.show_keyboard_layout(settings.input_device, layout: .none)
                }
            }
        }
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
                ui.ListRow(
                    app_name(device.apps.installed, app_id),
                    secondary: paste_words(chosen),
                    icon: .clipboard,
                ) {
                    trailing: {
                        row(gap: .tight, align: .center, wrap: true) {
                            ui.ToggleGroup(
                                selected: .some(paste_index(chosen)),
                                caption: "{app_name(device.apps.installed, app_id)} pastes with",
                            ) {
                                ui.Segment("Ctrl+V") {
                                    on press: settings.set_paste_shortcut(
                                        settings.input,
                                        app: app_id,
                                        shortcut: "ctrl_v",
                                    )
                                }
                                ui.Segment("Ctrl+Shift+V") {
                                    on press: settings.set_paste_shortcut(
                                        settings.input,
                                        app: app_id,
                                        shortcut: "ctrl_shift_v",
                                    )
                                }
                            }
                            ui.Button(
                                "Use its own",
                                size: .small,
                                accessible_name: "Use {app_name(device.apps.installed, app_id)}'s own shortcut",
                            ) {
                                on press: settings.set_paste_shortcut(
                                    settings.input,
                                    app: app_id,
                                    shortcut: "default",
                                )
                            }
                        }
                    }
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Dropdown(
                bind: paste_app,
                options: device.apps.installed.map(a -> a.name),
                placeholder: "An app",
                caption: "The app",
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
                    app: app_key(device.apps.installed, paste_app),
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
    if shortcut == "ctrl_shift_v" {
        std.say("Pastes with Ctrl+Shift+V")
    } else { std.say("Pastes with Ctrl+V") }
}

/// The stored name of the chord a person picked.
fn paste_key(chord: Text) -> Text {
    if chord == "Ctrl+Shift+V" { "ctrl_shift_v" } else { "ctrl_v" }
}

/// Which of the keyboard's models it is drawn as.
fn model_index(model: Text) -> Int {
    if model == "pc104" {
        0
    } else if model == "jp106" { 2 } else if model == "kr106" { 3 } else { 1 }
}

/// Which of the Caps Lock choices it takes now.
fn caps_lock_index(held: Option<Text>) -> Int {
    let chosen = if let behaviour = held { behaviour } else { "caps" }
    if chosen == "ctrl" {
        1
    } else if chosen == "escape" { 2 } else if chosen == "backspace" { 3 } else { 0 }
}

/// Which of the compose choices it takes now.
fn compose_index(held: Option<Text>) -> Int {
    let chosen = if let key = held { key } else { "disabled" }
    if chosen == "ralt" {
        1
    } else if chosen == "menu" {
        2
    } else if chosen == "caps" {
        3
    } else if chosen == "rwin" { 4 } else if chosen == "lwin" { 5 } else { 0 }
}

/// What a layout the catalog knows is called; an id it does not know reads as itself.
fn layout_label(layouts: List<device.KeyboardLayout>, id: Text) -> Text {
    if let known = layouts.find(l -> l.id == id) { known.label } else { id }
}

/// The id of the layout a catalog label names; the label itself when the catalog names none.
fn layout_id(layouts: List<device.KeyboardLayout>, label: Text) -> Text {
    if let known = layouts.find(l -> l.label == label) { known.id } else { label }
}

/// The catalog's layouts that `held` does not type yet, as the labels a person reads.
fn addable_layouts(layouts: List<device.KeyboardLayout>, held: List<Text>) -> List<Text> {
    layouts.filter(l -> not held.contains(l.id)).map(l -> l.label)
}

/// A list of layouts as a person reads them.
fn shared_words(layouts: List<device.KeyboardLayout>, ids: List<Text>, active: Text) -> Text {
    if ids.is_empty() {
        std.say("None yet")
    } else {
        ids
            .map(
                id -> if id == active {
                    std.say("{layout_label(layouts, id)} (typed with)")
                } else { layout_label(layouts, id) },
            )
            .join(", ")
    }
}

/// The layout a list types with once `removed` is taken out of it: the one it already types with,
/// or the first left.
fn active_without(ids: List<Text>, active: Text, removed: Text) -> Text {
    let left = ids.filter(x -> x != removed)
    if active != removed and left.contains(active) { active } else { left.first() ?? "" }
}

/// The layouts one keyboard types: its own, or the shared ones where it has none.
fn keyboard_sources(own: Option<settings.KeyboardProfile>, shared: List<Text>) -> List<Text> {
    if let held = own { held.sources } else { shared }
}

/// The layout one keyboard types with.
fn keyboard_active(own: Option<settings.KeyboardProfile>, shared_active: Text) -> Text {
    if let held = own { held.active } else { shared_active }
}

/// What a keyboard types, as its row says it.
fn keyboard_words(
    own: Option<settings.KeyboardProfile>,
    layouts: List<device.KeyboardLayout>,
    shared_active: Text,
) -> Text {
    if let held = own {
        std.say("Its own layouts, typing with {layout_label(layouts, held.active)}")
    } else {
        std.say("The shared layouts, typing with {layout_label(layouts, shared_active)}")
    }
}

/// A keyboard's own layouts with `layout` added: the ones it types, or the shared ones where it
/// types none, and the layout it is given.
fn adding_layout(
    own: Option<settings.KeyboardProfile>,
    shared: List<Text>,
    layout: Text,
) -> List<Text> {
    let sources = keyboard_sources(own, shared)
    if sources.contains(layout) { sources } else { sources + [layout] }
}

/// The layout a keyboard types with once `removed` is taken out of its list.
fn keyboard_active_without(
    own: Option<settings.KeyboardProfile>,
    shared_active: Text,
    removed: Text,
) -> Text {
    let active = keyboard_active(own, shared_active)
    if active != removed {
        active
    } else if let held = own {
        held.sources.filter(x -> x != removed).first() ?? shared_active
    } else { shared_active }
}

/// The level a drawing shows: the inspected one, kept inside the levels the layout supplies.
fn key_level(count: Int, level: Int) -> Int {
    level.max(0).min(count.max(1) - 1)
}

/// Which of the level choices is made, when the strip offers one.
fn level_choice(count: Int, level: Int, extra: Bool) -> Option<Int> {
    if levels_offered(count, extra).contains(level) { .some(level) } else { .none }
}

/// The levels a person can look at: the four usual ones, or every level the layout supplies.
fn levels_offered(count: Int, extra: Bool) -> List<Int> {
    let shown = if extra { count.max(1) } else { count.max(1).min(4) }
    numbers(0, shown, [])
}

/// What a key types on a level, as a person reads the level.
fn layer_words(index: Int) -> Text {
    if index == 0 {
        std.say("Base")
    } else if index == 1 {
        std.say("Shift")
    } else if index == 2 {
        std.say("AltGr")
    } else if index == 3 { std.say("Shift+AltGr") } else { std.say("Level {index + 1}") }
}

/// A keyboard's keys as a drawing: every key's body and the legend it types on `level`, the key
/// of the pressed shape standing out.
fn keyboard_shapes(drawn: device.KeyboardDrawing, level: Int, inspected: Int) -> List<ui.Shape> {
    drawn.keys.flat_map((key, index) -> key_shapes(drawn, key, level, index == inspected / 2))
}

/// One key as a drawing: its body, and what it types at the level shown written inside it.
fn key_shapes(
    drawn: device.KeyboardDrawing,
    key: device.KeyboardKey,
    level: Int,
    inspected: Bool,
) -> List<ui.Shape> {
    let width = drawn.width.max(1.0)
    let height = drawn.height.max(1.0)
    let box = outline_box(key.outline)
    [
        ui.Shape.path(
            points: key.outline.flat_map(p -> [p.x / width, p.y / height]),
            closed: true,
            fill: .some(if inspected { .accent } else { .track }),
            stroke: .some(.muted),
            weight: .hairline,
        ),
        ui.Shape.text(
            x: ((box.at(0) ?? 0.0) + (box.at(2) ?? 0.0)) / 2.0 / width,
            y: ((box.at(1) ?? 0.0) + (box.at(3) ?? 0.0)) / 2.0 / height,
            text: key.levels.at(level) ?? "",
            role: .caption,
            ink: .ink,
            anchor: .middle,
        ),
    ]
}

/// The box `points` fill: its left, top, right and bottom.
fn outline_box(points: List<device.KeyboardPoint>) -> List<Float> {
    [
        points.map(p -> p.x).sort_by(x -> x).first() ?? 0.0,
        points.map(p -> p.y).sort_by(y -> y).first() ?? 0.0,
        points.map(p -> p.x).sort_by(x -> x).last() ?? 0.0,
        points.map(p -> p.y).sort_by(y -> y).last() ?? 0.0,
    ]
}

/// What an app is called on this device, or its id where no app of that id is installed.
fn app_name(apps: List<device.App>, id: Text) -> Text {
    if let known = apps.find(a -> a.key == id) { known.name } else { id }
}

/// The id of the app of the device whose name is `name`.
fn app_key(apps: List<device.App>, name: Text) -> Text {
    if let known = apps.find(a -> a.name == name) { known.key } else { name }
}

/// Which of a paste shortcut's choices an app takes now.
fn paste_index(shortcut: Text) -> Int {
    if shortcut == "ctrl_shift_v" { 1 } else { 0 }
}

/// `ids` with the layouts at `from` and `to` swapped: a layout moved one place along the order
/// they are switched in.
fn moved(ids: List<Text>, from: Int, to: Int) -> List<Text> {
    let here = ids.at(from) ?? ""
    let there = ids.at(to) ?? ""
    ids.map((id, index) -> if index == from { there } else if index == to { here } else { id })
}
