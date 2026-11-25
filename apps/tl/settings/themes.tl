/// The person's themes: which one each mode shows, the looks Tilo ships, the theme packages
/// installed on this device, and every theme of their own, each role a field of the appearance
/// resource. A package is applied by the appearance's own action, which names it so their
/// devices that have it draw it and one without it says so. A slider here, the shell's style
/// panel and an agent change the same field with the same edit, so the desktop takes it live
/// and History keeps who changed it. An edit that would leave a pair of ink and surface under
/// its contrast floor is refused, and the refusal names the pair.
view themes_pane(s: Settings) {
    state editing: Text = ""
    state new_id: Text = ""
    state base: Text = "tilo-dark"
    let a = settings.appearance
    let packages = settings.theme_packages
    if let chosen = a.theme_package {
        if not packages.packages.any(p -> p.key == chosen) {
            ui.Banner(
                "The theme package is not installed on this device",
                message: "Every app shows Tilo's theme until the package is installed here; your other devices that have it show it.",
                kind: .warning,
            )
        }
    }
    ui.Section("Shown") {
        setting("In dark mode", "A look's dark side, or one of your themes") {
            ui.Text(theme_title(a.dark_theme), role: .body)
        }
        setting("In light mode", "A look's light side, or one of your themes") {
            ui.Text(theme_title(a.light_theme), role: .body)
        }
    }
    ui.Section("Looks") {
        for look in ["Tilo", "Paper", "Moss", "Dusk", "Mono"] {
            ui.ListRow(look, secondary: "Designed for light and for dark", icon: .palette) {
                on activate: settings.choose_theme(a, theme: look)
            }
        }
    }
    ui.Section("Installed theme packages") {
        if packages.packages.is_empty() {
            ui.Text(
                "A theme package brings its own painters; install one from the Store and it appears here.",
                role: .caption,
                secondary: true,
            )
        }
        for package in packages.packages {
            ui.ListRow(
                package.name,
                secondary: package.key,
                selected: a.theme_package == .some(package.key),
                icon: .palette,
            ) {
                on activate: settings.apply_theme(a, package: package.key)
                trailing: {
                    ui.Button("Use", size: .small) {
                        on press: settings.apply_theme(a, package: package.key)
                    }
                }
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
                on activate: set(editing, t.id).then(review_contrast_now(t.id))
                trailing: {
                    row(gap: .related, align: .center) {
                        ui.Button("Use", size: .small) {
                            on press: settings.use_theme(a, theme: t.id)
                        }
                        ui.Button(
                            "Delete",
                            icon: .trash,
                            icon_only: true,
                            role: .plain,
                            size: .small,
                        ) {
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
            // A theme saved with Export on another workspace, taken in whole: every role, tuned
            // value and glass shader, under an id made from its name.
            ui.Button("Import", icon: .upload, size: .small) {
                on press: file.choose(import_theme(s), accept: [".theme.json", "application/json"])
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
    ui.Section("Contrast") {
        if t.findings.is_empty() {
            ui.Text(
                "Every ink and surface pairing keeps its contrast floor.",
                role: .caption,
                secondary: true,
            )
        } else {
            ui.Text(
                "The last value tried would put these pairs under their floor:",
                role: .caption,
                secondary: true,
            )
            for finding in t.findings {
                ui.Fact(finding, icon: .triangle_alert)
            }
        }
    }
    ui.Section("Name and polarity") {
        setting("Name", "What the theme is called") {
            ui.TextInput(bind: t.name)
        }
        setting("Dark", "Light ink on dark surfaces; turning it keeps the hue") {
            ui.Toggle(shown: t.dark, label: "Dark") {
                on check(on): settings
                    .switch_polarity(settings.appearance, theme: t.id, dark: on)
                    .then(review_contrast_now(t.id))
            }
        }
        setting("High contrast", "The theme's own high-contrast palette") {
            ui.Toggle(bind: t.high_contrast) {
                on check(on): review_contrast_on(t.id, "", on)
            }
        }
    }
    ui.Section("Colour") {
        setting("Hue", "The temperature every surface leans to; empty is grey") {
            ui.Slider(
                bind: t.hue,
                label: "Hue",
                min: 0.0,
                max: 360.0,
                step: 1.0,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "hue", value)
            }
        }
        setting("Chroma", "How much of the hue the surfaces carry") {
            ui.Slider(
                bind: t.chroma,
                label: "Chroma",
                min: 0.0,
                max: 0.05,
                step: 0.001,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "chroma", value)
            }
        }
        setting(
            "Accent hue",
            "Presses and selections take this hue; empty takes each person's colour",
        ) {
            ui.Slider(
                bind: t.tint,
                label: "Accent hue",
                min: 0.0,
                max: 360.0,
                step: 1.0,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "tint", value)
            }
        }
        setting("Inverted accent", "Presses and selections in ink, with no hue") {
            ui.Toggle(bind: t.monochrome) {
                on check(on): review_contrast_on(t.id, "monochrome", on)
            }
        }
        setting("Presence lightness", "How light people's colours are drawn") {
            ui.Slider(
                bind: t.presence.lightness,
                label: "Presence lightness",
                min: 0.35,
                max: 0.9,
                step: 0.01,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "presence.lightness", value)
            }
        }
        setting("Presence chroma", "How vivid people's colours are drawn") {
            ui.Slider(
                bind: t.presence.chroma,
                label: "Presence chroma",
                min: 0.04,
                max: 0.2,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "presence.chroma", value)
            }
        }
    }
    ui.Section("Surfaces") {
        setting("Desktop", "Lightness, from black to white") {
            ui.Slider(
                bind: t.surfaces.base,
                label: "Desktop",
                min: 0.0,
                max: 1.0,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "surfaces.base", value)
            }
        }
        setting("Window", "Lightness, from black to white") {
            ui.Slider(
                bind: t.surfaces.content,
                label: "Window",
                min: 0.0,
                max: 1.0,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "surfaces.content", value)
            }
        }
        setting("Sidebar", "Lightness, from black to white") {
            ui.Slider(
                bind: t.surfaces.sidebar,
                label: "Sidebar",
                min: 0.0,
                max: 1.0,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "surfaces.sidebar", value)
            }
        }
        setting("Inspector", "Lightness, from black to white") {
            ui.Slider(
                bind: t.surfaces.inspector,
                label: "Inspector",
                min: 0.0,
                max: 1.0,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "surfaces.inspector", value)
            }
        }
        setting("Raised", "Lightness, from black to white") {
            ui.Slider(
                bind: t.surfaces.raised,
                label: "Raised",
                min: 0.0,
                max: 1.0,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "surfaces.raised", value)
            }
        }
        setting("Floating chrome", "Lightness, from black to white") {
            ui.Slider(
                bind: t.surfaces.overlay,
                label: "Floating chrome",
                min: 0.0,
                max: 1.0,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "surfaces.overlay", value)
            }
        }
    }
    ui.Section("Ink") {
        setting("Primary ink", "Lightness, from black to white") {
            ui.Slider(
                bind: t.ink.primary,
                label: "Primary ink",
                min: 0.0,
                max: 1.0,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "ink.primary", value)
            }
        }
        setting("Secondary ink", "Lightness, from black to white") {
            ui.Slider(
                bind: t.ink.secondary,
                label: "Secondary ink",
                min: 0.0,
                max: 1.0,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "ink.secondary", value)
            }
        }
        setting("Tertiary ink", "Lightness, from black to white") {
            ui.Slider(
                bind: t.ink.tertiary,
                label: "Tertiary ink",
                min: 0.0,
                max: 1.0,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "ink.tertiary", value)
            }
        }
    }
    ui.Section("Shape and type") {
        setting("Window corners", "How round windows and panels are") {
            ui.Slider(
                bind: t.window_radius,
                label: "Window corners",
                min: 0.0,
                max: 28.0,
                step: 1.0,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "", value)
            }
        }
        setting("Density", "How much room there is around everything") {
            ui.Slider(
                bind: t.density,
                label: "Density",
                min: 0.75,
                max: 1.5,
                step: 0.05,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "", value)
            }
        }
        setting("Interface face", "The typeface the desktop is drawn in") {
            ui.TextInput(bind: t.face)
        }
        setting("Text size", "The body text's size, in points") {
            ui.Slider(
                bind: t.base_size,
                label: "Text size",
                min: 12.0,
                max: 18.0,
                step: 0.5,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "", value)
            }
        }
        setting("Motion", "How lively the movement is; none lands every change at once") {
            ui.Slider(
                bind: t.motion,
                label: "Motion",
                min: 0.0,
                max: 2.0,
                step: 0.05,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "", value)
            }
        }
        setting(
            "Distinguish similar letters",
            "Set I, l, 1, 0 and O apart in this theme's text; empty follows its typeface",
        ) {
            ui.Toggle(bind: t.distinct_letters) {
                on check(on): review_contrast_on(t.id, "", on)
            }
        }
    }
    ui.Section("Status colours") {
        ui.Text(
            "What a thing that failed, warns, went well or informs is drawn in. Each is the status's hue, this theme's lightness and how much colour it carries.",
            role: .caption,
            secondary: true,
        )
        status_tone("Something failed", status_role(0), t.status.danger, t.id)
        status_tone("A warning", status_role(1), t.status.warning, t.id)
        status_tone("Went well", status_role(2), t.status.success, t.id)
        status_tone("Information", status_role(3), t.status.info, t.id)
    }
    ui.Section("Material") {
        setting("Blur", "How much what lies behind floating chrome is blurred") {
            ui.Slider(
                bind: t.blur,
                label: "Blur",
                min: 0.0,
                max: 48.0,
                step: 1.0,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "", value)
            }
        }
        setting("Refraction", "How far glass bends what lies behind it at its edge") {
            ui.Slider(
                bind: t.refraction,
                label: "Refraction",
                min: 0.0,
                max: 32.0,
                step: 0.5,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "", value)
            }
        }
        setting("Tint", "How much of the panel's own colour covers what shows through") {
            ui.Slider(
                bind: t.tint_opacity,
                label: "Tint",
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "tint_opacity", value)
            }
        }
        setting("Frosted tab strip", "The tab strip takes the floating material") {
            ui.Toggle(bind: t.frosted_strip) {
                on check(on): review_contrast_on(t.id, "frosted_strip", on)
            }
        }
        setting("Rim light", "How far round a selected tab its rim light reaches") {
            ui.Slider(
                bind: t.rim_span,
                label: "Rim light",
                min: 0.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "", value)
            }
        }
        setting("Rim brightness", "How strongly the rim light shows") {
            ui.Slider(
                bind: t.rim_alpha,
                label: "Rim brightness",
                min: 0.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "", value)
            }
        }
        setting("Hover wash", "How much a surface lightens under the pointer") {
            ui.Slider(
                bind: t.hover_alpha,
                label: "Hover wash",
                min: 0.0,
                max: 0.5,
                step: 0.01,
                shows_value: true,
            ) {
                on change(value): review_contrast(t.id, "", value)
            }
        }
    }
    row(gap: .related, align: .center, wrap: true) {
        ui.Button("Duplicate", icon: .copy) {
            on press: settings.new_theme(settings.appearance, id: t.id + "-copy", base: t.id)
        }
        // Saved where the person chooses, through the system's save path: the file is the theme
        // itself, which importing it on another workspace reads back role by role.
        ui.Button("Export", icon: .download) {
            on press: file.save(export_theme(t), name: "{t.name}.theme.json")
        }
        ui.Button("Start over from where it came from", icon: .rotate_ccw) {
            on press: settings.reset_theme(settings.appearance, theme: t.id)
        }
    }
}

/// The role path of the status tone at `index` (`0` danger, `1` warning, `2` success, `3` info),
/// as the review names it: the path, never words a person reads.
fn status_role(index: Int) -> Text {
    if index == 0 {
        "status.danger"
    } else if index == 1 {
        "status.warning"
    } else if index == 2 { "status.success" } else { "status.info" }
}

/// One status colour: how light it is drawn and how much of its hue it carries.
template view status_tone(label: Text, role: Text, tone: settings.StatusTone, theme: Text) {
    setting(label, "Its lightness, and how much colour it carries") {
        row(gap: .related, align: .center, wrap: true) {
            ui.Slider(
                bind: tone.lightness,
                label: "{label}: lightness",
                min: 0.0,
                max: 1.0,
                step: 0.01,
                shows_value: true,
            ) {
                on change(value): review_contrast(theme, "{role}.lightness", value)
            }
            ui.Slider(
                bind: tone.chroma,
                label: "{label}: colour",
                min: 0.0,
                max: 0.25,
                step: 0.005,
                shows_value: true,
            ) {
                on change(value): review_contrast(theme, "{role}.chroma", value)
            }
        }
    }
}

/// Lists every contrast floor the theme would miss with `role` at the control's value, so its
/// editor shows each pairing under its floor while the theme is tuned. The theme keeps the
/// roles it admits; a value that would break a floor is refused, and this says which pairs it
/// would break.
action review_contrast(theme: Text, role: Text, value: Float) {
    settings.review_theme(settings.appearance, theme: theme, role: role, value: value)
}

/// The same after a switch was tried: its on is `1` and its off `0`.
action review_contrast_on(theme: Text, role: Text, on: Bool) {
    settings.review_theme(
        settings.appearance,
        theme: theme,
        role: role,
        value: if on { 1.0 } else { 0.0 },
    )
}

/// Records what the theme misses as it stands, when its editor opens or its polarity changes,
/// before any control has carried a value.
action review_contrast_now(theme: Text) {
    settings.review_theme(settings.appearance, theme: theme, role: "")
}

/// The theme as a file: what exporting it saves, and importing it on another workspace reads.
action export_theme(t: settings.ThemeRecord) -> File (label: "Export a theme") {
    File.of_text("{t.name}.theme.json", json.encode(t))
}

/// What a mode shows, as a person reads it.
fn theme_title(reference: Text) -> Text {
    if reference.starts_with("custom:") {
        reference.split(":").at(1) ?? reference
    } else { reference }
}

/// Which mode shows a theme of the person's, if one does.
fn shown_as(a: settings.Appearance, id: Text, dark: Bool) -> Text {
    let reference = "custom:" + id
    if dark and a.dark_theme == reference {
        "Shown in dark mode"
    } else if not dark and a.light_theme == reference {
        "Shown in light mode"
    } else if dark { "Dark" } else { "Light" }
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

/// Takes in a theme saved with Export, from this workspace or another: every role, tuned value
/// and glass shader, once it is admitted and keeps every contrast floor. Importing names it by
/// its own name, so what a person exported under a name comes back under one.
action import_theme(s: Settings, file: File) (
    icon: .upload,
    label: "Import a theme…",
    did: "imported a theme",
    opens: ["application/json"],
) {
    let text = match file.text() {
        .ok(said) => said,
        .err(_) => "",
    }
    if text == "" {
        return
    }
    settings.import_theme(settings.appearance, id: "", document: text)
}
