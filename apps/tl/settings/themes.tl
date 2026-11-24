/// The person's themes: which one each mode shows, the looks Tilo ships, and every theme of
/// their own, each role a field of the appearance resource. A slider here, the shell's style
/// panel and an agent change the same field with the same edit, so the desktop takes it live
/// and History keeps who changed it. An edit that would leave a pair of ink and surface under
/// its contrast floor is refused, and the refusal names the pair.
view themes_pane() {
    state editing: Text = ""
    state new_id: Text = ""
    state base: Text = "tilo-dark"
    let a = settings.appearance
    ui.Section("Shown") {
        setting(
            "In dark mode",
            "A look's dark side, or one of your themes",
            ui.Text(theme_title(a.dark_theme), role: .body),
        )
        setting(
            "In light mode",
            "A look's light side, or one of your themes",
            ui.Text(theme_title(a.light_theme), role: .body),
        )
    }
    ui.Section("Looks") {
        for look in ["Tilo", "Paper", "Moss", "Dusk", "Mono"] {
            ui.ListRow(look, secondary: "Designed for light and for dark", icon: .palette) {
                on activate: settings.choose_theme(a, theme: look)
            }
        }
    }
    ui.Section("Your themes") {
        if a.themes.is_empty() {
            ui.Text(
                "Start one from a look or a theme, and change any of its roles.",
                role: .caption,
                secondary: true,
            )
        }
        for t in a.themes {
            ui.ListRow(
                t.name,
                secondary: shown_as(a, t.id, t.dark),
                selected: editing == t.id,
                icon: if t.dark { .moon } else { .sun },
            ) {
                on activate: set(editing, t.id)
                trailing: {
                    row(gap: .related, align: .center) {
                        ui.Button("Use", size: .small) {
                            on press: settings.use_theme(a, theme: t.id)
                        }
                        ui.Button("Delete", icon: .trash, icon_only: true, role: .plain, size: .small) {
                            on press: settings.delete_theme(a, theme: t.id)
                        }
                    }
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.TextInput(bind: new_id, placeholder: "A name for it, such as harbour", fill)
            ui.Dropdown(
                bind: base,
                options: preset_ids() + a.themes.map(t -> t.id),
                caption: "Start from",
            )
            ui.Button("New theme", icon: .plus, role: .primary, size: .small) {
                on press: settings.new_theme(a, id: new_id, base: base)
            }
        }
    }
    for t in a.themes {
        if t.id == editing {
            theme_editor(t)
        }
    }
}

/// One theme's roles, each control bound to its field.
template view theme_editor(t: settings.ThemeRecord) {
    ui.Section("Name and polarity") {
        setting("Name", "What the theme is called", ui.TextInput(bind: t.name))
        setting(
            "Dark",
            "Light ink on dark surfaces; turning it keeps the hue",
            ui.Toggle(
                on: t.dark,
                label: "Dark",
            ) {
                on change(on): settings.switch_polarity(settings.appearance, theme: t.id, dark: on)
            },
        )
        setting(
            "High contrast",
            "The theme's own high-contrast palette",
            ui.Toggle(bind: t.high_contrast),
        )
    }
    ui.Section("Colour") {
        setting(
            "Hue",
            "The temperature every surface leans to; empty is grey",
            ui.Slider(bind: t.hue, label: "Hue", min: 0.0, max: 360.0, step: 1.0, shows_value: true),
        )
        setting(
            "Chroma",
            "How much of the hue the surfaces carry",
            ui.Slider(bind: t.chroma, label: "Chroma", min: 0.0, max: 0.05, step: 0.001, shows_value: true),
        )
        setting(
            "Accent hue",
            "Presses and selections take this hue; empty takes each person's colour",
            ui.Slider(bind: t.tint, label: "Accent hue", min: 0.0, max: 360.0, step: 1.0, shows_value: true),
        )
        setting(
            "Inverted accent",
            "Presses and selections in ink, with no hue",
            ui.Toggle(bind: t.monochrome),
        )
        setting(
            "Presence lightness",
            "How light people's colours are drawn",
            ui.Slider(
                bind: t.presence.lightness,
                label: "Presence lightness",
                min: 0.35,
                max: 0.9,
                step: 0.01,
                shows_value: true,
            ),
        )
        setting(
            "Presence chroma",
            "How vivid people's colours are drawn",
            ui.Slider(
                bind: t.presence.chroma,
                label: "Presence chroma",
                min: 0.04,
                max: 0.2,
                step: 0.005,
                shows_value: true,
            ),
        )
    }
    ui.Section("Surfaces") {
        lightness("Desktop", t.surfaces.base)
        lightness("Window", t.surfaces.content)
        lightness("Sidebar", t.surfaces.sidebar)
        lightness("Inspector", t.surfaces.inspector)
        lightness("Raised", t.surfaces.raised)
        lightness("Floating chrome", t.surfaces.overlay)
    }
    ui.Section("Ink") {
        lightness("Primary ink", t.ink.primary)
        lightness("Secondary ink", t.ink.secondary)
        lightness("Tertiary ink", t.ink.tertiary)
    }
    ui.Section("Shape and type") {
        setting(
            "Window corners",
            "How round windows and panels are",
            ui.Slider(bind: t.window_radius, label: "Window corners", min: 0.0, max: 28.0, step: 1.0, shows_value: true),
        )
        setting(
            "Density",
            "How much room there is around everything",
            ui.Slider(bind: t.density, label: "Density", min: 0.75, max: 1.5, step: 0.05, shows_value: true),
        )
        setting("Interface face", "The typeface the desktop is drawn in", ui.TextInput(bind: t.face))
        setting(
            "Text size",
            "The body text's size, in points",
            ui.Slider(bind: t.base_size, label: "Text size", min: 12.0, max: 18.0, step: 0.5, shows_value: true),
        )
        setting(
            "Motion",
            "How lively the movement is; none lands every change at once",
            ui.Slider(bind: t.motion, label: "Motion", min: 0.0, max: 2.0, step: 0.05, shows_value: true),
        )
    }
    ui.Section("Material") {
        setting(
            "Blur",
            "How much what lies behind floating chrome is blurred",
            ui.Slider(bind: t.blur, label: "Blur", min: 0.0, max: 48.0, step: 1.0, shows_value: true),
        )
        setting(
            "Refraction",
            "How far glass bends what lies behind it at its edge",
            ui.Slider(bind: t.refraction, label: "Refraction", min: 0.0, max: 32.0, step: 0.5, shows_value: true),
        )
        setting(
            "Tint",
            "How much of the panel's own colour covers what shows through",
            ui.Slider(bind: t.tint_opacity, label: "Tint", min: 0.0, max: 1.0, step: 0.01, shows_value: true),
        )
        setting(
            "Frosted tab strip",
            "The tab strip takes the floating material",
            ui.Toggle(bind: t.frosted_strip),
        )
        setting(
            "Rim light",
            "How far round a selected tab its rim light reaches",
            ui.Slider(bind: t.rim_span, label: "Rim light", min: 0.0, max: 1.0, step: 0.05, shows_value: true),
        )
    }
    row(gap: .related, align: .center, wrap: true) {
        ui.Button("Duplicate", icon: .copy) {
            on press: settings.new_theme(settings.appearance, id: t.id + "-copy", base: t.id)
        }
        // Saved where the person chooses, through the system's save path: the file is the theme
        // itself, which importing it on another workspace reads back role by role.
        ui.Button("Export", icon: .download) {
            on press: file.save(File.of_text("{t.name}.theme.json", json.encode(t)), name: "{t.name}.theme.json")
        }
        ui.Button("Start over from where it came from", icon: .rotate_ccw) {
            on press: settings.reset_theme(settings.appearance, theme: t.id)
        }
    }
}

/// One lightness of the surface or ink ladder.
template view lightness(title: Text, value: Float) {
    setting(
        title,
        "Lightness, from black to white",
        ui.Slider(bind: value, label: title, min: 0.0, max: 1.0, step: 0.005, shows_value: true),
    )
}

/// What a mode shows, as a person reads it.
fn theme_title(reference: Text) -> Text {
    if reference.starts_with("custom:") {
        reference.strip_prefix("custom:")
    } else {
        reference
    }
}

/// Which mode shows a theme of the person's, if one does.
fn shown_as(a: settings.AppearancePrefs, id: Text, dark: Bool) -> Text {
    let reference = "custom:" + id
    if dark and a.dark_theme == reference {
        "Shown in dark mode"
    } else if not dark and a.light_theme == reference {
        "Shown in light mode"
    } else if dark {
        "Dark"
    } else {
        "Light"
    }
}

/// The presets a new theme can start from: each look's two sides.
fn preset_ids() -> List<Text> {
    [
        "tilo-dark",
        "tilo-light",
        "paper-dark",
        "paper-light",
        "moss-dark",
        "moss-light",
        "dusk-dark",
        "dusk-light",
        "mono-dark",
        "mono-light",
    ]
}
