/// This person's accessibility settings: what makes the pointer, the keyboard and the screen
/// reader easier to follow on this device and, when asked, at its sign-in screen.
view accessibility_pane() {
    let a = settings.accessibility
    let on_device = settings.accessibility_device
    ui.Section("Seeing") {
        setting("Screen reader", "Speak what is on the screen and what it changes") {
            ui.Button(if a.screen_reader { "On" } else { "Off" }, toggled: a.screen_reader) {
                on press: settings.set_screen_reader(
                    settings.accessibility,
                    on: not a.screen_reader,
                )
            }
        }
        setting("Speaking pace", "Words a minute, before the voice's own") {
            ui.NumberInput(
                bind: a.screen_reader_rate,
                label: "Words a minute",
                min: 80.0,
                max: 500.0,
                step: 10.0,
            )
        }
        setting("Voice", "Which voice reads the screen") {
            ui.EnumField(bind: a.screen_reader_voice, label: "Screen reader voice")
        }
        setting("Magnifier", "The zoom a magnifier opens at") {
            ui.NumberInput(
                bind: a.magnifier_zoom,
                label: "Magnification",
                min: 1.0,
                max: 16.0,
                step: 0.5,
            )
        }
        setting("How it shows", "A lens that follows the pointer, or the whole screen") {
            ui.EnumField(bind: a.magnifier_view, label: "Magnifier view")
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Magnify more", icon: .zoom_in) {
                on press: settings.step_magnifier(settings.accessibility, step: "zoom-in")
            }
            ui.Button("Magnify less", icon: .zoom_out) {
                on press: settings.step_magnifier(settings.accessibility, step: "zoom-out")
            }
            ui.Button("Turn it on or off", size: .small) {
                on press: settings.step_magnifier(settings.accessibility, step: "toggle")
            }
        }
        setting("Colour filter", "Shift the colours everything is drawn in") {
            ui.EnumField(bind: a.color_filter, label: "Colour filter")
        }
        setting("Thicker text caret", "A wider mark where typing goes") {
            ui.Toggle(bind: a.thick_text_caret)
        }
        setting("Crosshair pointer", "A pointer that marks its row and column on the screen") {
            ui.Toggle(bind: a.cursor_crosshair)
        }
    }
    ui.Section("The pointer") {
        setting("Larger pointer", "A pointer easy to see against any background") {
            ui.Toggle(bind: a.large_cursor)
        }
        setting("Pointer size", "How large the pointer is drawn") {
            ui.NumberInput(
                bind: a.cursor_size,
                label: "Pointer size",
                min: 16.0,
                max: 96.0,
                step: 4.0,
            )
        }
        setting("Pointer theme", "The pointer artwork's name; empty follows the theme") {
            ui.TextInput(bind: a.cursor_theme, placeholder: "Follow the theme")
        }
    }
    ui.Section("Typing") {
        setting("Sticky keys", "A modifier stays down for the key after it") {
            ui.Toggle(bind: a.sticky_keys)
        }
        setting("Slow keys", "A key has to be held before it counts") {
            ui.Toggle(bind: a.slow_keys)
        }
        setting(
            "One key at a time",
            "The keyboard's keys pressed one after another stand for a chord",
        ) {
            ui.Toggle(bind: a.mouse_keys)
        }
        setting(
            "On-screen keyboard",
            "When the keyboard's own keys show at the bottom of the screen",
        ) {
            ui.EnumField(bind: a.on_screen_keyboard, label: "On-screen keyboard")
        }
        setting(
            "Dictation",
            "Written words are taken as they are said, without guessing at intent",
        ) {
            ui.Toggle(bind: a.dictation_literal)
        }
        setting(
            "Dictation's language",
            "A language tag such as de-DE; empty follows the workspace's language",
        ) {
            ui.TextInput(bind: a.dictation_language, placeholder: "The workspace's language")
        }
    }
    ui.Section("Key feedback") {
        setting("Bounce keys", "A key pressed again too soon is ignored") {
            ui.Toggle(bind: a.bounce_keys)
        }
        setting("How soon it counts again", "Milliseconds before the same key counts again") {
            ui.NumberInput(
                bind: a.bounce_keys_delay_ms,
                label: "Milliseconds",
                min: 100.0,
                max: 2000.0,
                step: 50.0,
            )
        }
        setting("Say when a key is held back", "A sound or a flash when a press is ignored") {
            ui.Toggle(bind: a.bounce_keys_alert)
        }
        setting("Visible alerts", "Flash the screen instead of playing a sound") {
            ui.Toggle(bind: a.visual_alerts)
        }
    }
    ui.Section("Switch control") {
        setting("Switch control", "Move through what is on the screen with one or two switches") {
            ui.Toggle(bind: a.switch_control)
        }
        setting("The switches", "The key pair the switches stand for") {
            ui.EnumField(bind: a.switch_key_pair, label: "Switch keys")
        }
        setting("How long each stops", "Milliseconds the highlight rests on each choice") {
            ui.NumberInput(
                bind: a.switch_scan_ms,
                label: "Milliseconds",
                min: 200.0,
                max: 5000.0,
                step: 100.0,
            )
        }
    }
    ui.Section("At the sign-in screen") {
        setting(
            "Use these settings there",
            "The person who signs in finds them before the desktop starts",
        ) {
            ui.Toggle(bind: on_device.apply_to_sign_in)
        }
    }
}
