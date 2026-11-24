/// The states page: each interactive control of the toolkit in the states a person meets it in,
/// side by side. Each sample is the control itself, held in its state by `ui.InState` and taking no
/// input; disabled, checked, chosen and invalid are the control's own props.
view states(g: Gallery) {
    // What a sample would do if pressed: a sample takes no press, but a button with nothing
    // to do is drawn as one that cannot be used.
    state pressed: Text = ""
    ui.Text(
        "Each control at rest, under the pointer, held down, with the keyboard's focus, unavailable, and chosen or wrong where it can be.",
        role: .caption,
        secondary: true,
    )
    stack(max: 64rem, gap: .section) {
        part("Primary button", "The one action a place is for.") {
            grid(min: 7rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Button("Save", icon: .check, role: .primary) {
                            on press: set(pressed, "Save")
                        }
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Button("Save", icon: .check, role: .primary) {
                            on press: set(pressed, "Save")
                        }
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.Button("Save", icon: .check, role: .primary) {
                            on press: set(pressed, "Save")
                        }
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Button("Save", icon: .check, role: .primary) {
                            on press: set(pressed, "Save")
                        }
                    }
                }
                sample("Disabled") {
                    ui.InState(.rest) {
                        ui.Button(
                            "Save",
                            icon: .check,
                            role: .primary,
                            enabled: false,
                            description: "Nothing to save yet",
                        ) {
                            on press: set(pressed, "Save")
                        }
                    }
                }
            }
        }
        part("Button", "An action of the usual kind.") {
            grid(min: 7rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Button("Upload", icon: .upload) {
                            on press: set(pressed, "Upload")
                        }
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Button("Upload", icon: .upload) {
                            on press: set(pressed, "Upload")
                        }
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.Button("Upload", icon: .upload) {
                            on press: set(pressed, "Upload")
                        }
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Button("Upload", icon: .upload) {
                            on press: set(pressed, "Upload")
                        }
                    }
                }
                sample("Disabled") {
                    ui.InState(.rest) {
                        ui.Button(
                            "Upload",
                            icon: .upload,
                            enabled: false,
                            description: "Nothing to upload yet",
                        ) {
                            on press: set(pressed, "Upload")
                        }
                    }
                }
            }
        }
        part("Destructive button", "An action that takes something away.") {
            grid(min: 7rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Button("Delete", icon: .trash, role: .destructive) {
                            on press: set(pressed, "Delete")
                        }
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Button("Delete", icon: .trash, role: .destructive) {
                            on press: set(pressed, "Delete")
                        }
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.Button("Delete", icon: .trash, role: .destructive) {
                            on press: set(pressed, "Delete")
                        }
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Button("Delete", icon: .trash, role: .destructive) {
                            on press: set(pressed, "Delete")
                        }
                    }
                }
                sample("Disabled") {
                    ui.InState(.rest) {
                        ui.Button(
                            "Delete",
                            icon: .trash,
                            role: .destructive,
                            enabled: false,
                            description: "Nothing to delete yet",
                        ) {
                            on press: set(pressed, "Delete")
                        }
                    }
                }
            }
        }
        part("Plain button", "An action that stays quiet beside its content.") {
            grid(min: 7rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Button("Details", role: .plain) {
                            on press: set(pressed, "Details")
                        }
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Button("Details", role: .plain) {
                            on press: set(pressed, "Details")
                        }
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.Button("Details", role: .plain) {
                            on press: set(pressed, "Details")
                        }
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Button("Details", role: .plain) {
                            on press: set(pressed, "Details")
                        }
                    }
                }
                sample("Disabled") {
                    ui.InState(.rest) {
                        ui.Button(
                            "Details",
                            role: .plain,
                            enabled: false,
                            description: "Nothing to show yet",
                        ) {
                            on press: set(pressed, "Details")
                        }
                    }
                }
            }
        }
        part("Icon button", "An action named by its icon, its name in its tooltip.") {
            grid(min: 7rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Button("Settings", icon: .settings, icon_only: true) {
                            on press: set(pressed, "Settings")
                        }
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Button("Settings", icon: .settings, icon_only: true) {
                            on press: set(pressed, "Settings")
                        }
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.Button("Settings", icon: .settings, icon_only: true) {
                            on press: set(pressed, "Settings")
                        }
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Button("Settings", icon: .settings, icon_only: true) {
                            on press: set(pressed, "Settings")
                        }
                    }
                }
                sample("Disabled") {
                    ui.InState(.rest) {
                        ui.Button(
                            "Settings",
                            icon: .settings,
                            icon_only: true,
                            enabled: false,
                            description: "Nothing to set yet",
                        ) {
                            on press: set(pressed, "Settings")
                        }
                    }
                }
            }
        }
        part("Checkbox", "A box marked or not.") {
            grid(min: 7rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Checkbox(bind: g.states_off, label: "Send")
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Checkbox(bind: g.states_off, label: "Send")
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.Checkbox(bind: g.states_off, label: "Send")
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Checkbox(bind: g.states_off, label: "Send")
                    }
                }
                sample("Checked") {
                    ui.InState(.rest) {
                        ui.Checkbox(bind: g.states_on, label: "Send")
                    }
                }
                sample("Mixed") {
                    ui.InState(.rest) {
                        ui.Checkbox(bind: g.states_off, label: "Send", indeterminate: true)
                    }
                }
            }
        }
        part("Switch", "A setting that is on or off at once.") {
            grid(min: 8rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Toggle(bind: g.states_off, label: "Wi-Fi")
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Toggle(bind: g.states_off, label: "Wi-Fi")
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.Toggle(bind: g.states_off, label: "Wi-Fi")
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Toggle(bind: g.states_off, label: "Wi-Fi")
                    }
                }
                sample("On") {
                    ui.InState(.rest) {
                        ui.Toggle(bind: g.states_on, label: "Wi-Fi")
                    }
                }
            }
        }
        part("Radio button", "One choice of several, exactly one of which is made.") {
            grid(min: 7rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.RadioGroup(label: "Send by") {
                            ui.Radio("Email")
                        }
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.RadioGroup(label: "Send by") {
                            ui.Radio("Email")
                        }
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.RadioGroup(label: "Send by") {
                            ui.Radio("Email")
                        }
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.RadioGroup(label: "Send by") {
                            ui.Radio("Email")
                        }
                    }
                }
                sample("Disabled") {
                    ui.InState(.rest) {
                        ui.RadioGroup(
                            label: "Send by",
                            enabled: false,
                            description: "Only email reaches the team this week",
                        ) {
                            ui.Radio("Email")
                        }
                    }
                }
                sample("Chosen") {
                    ui.InState(.rest) {
                        ui.RadioGroup(selected: 0, label: "Send by") {
                            ui.Radio("Email")
                        }
                    }
                }
                sample("Chosen, disabled") {
                    ui.InState(.rest) {
                        ui.RadioGroup(
                            selected: 0,
                            label: "Send by",
                            enabled: false,
                            description: "Only email reaches the team this week",
                        ) {
                            ui.Radio("Email")
                        }
                    }
                }
            }
        }
        part("Text field", "One line a person types.") {
            grid(min: 12rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.TextInput(bind: g.states_name, placeholder: "Your name")
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.TextInput(bind: g.states_name, placeholder: "Your name")
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.TextInput(bind: g.states_name, placeholder: "Your name")
                    }
                }
                sample("Wrong") {
                    form {
                        field("", error: "Use letters and spaces only") {
                            ui.InState(.rest) {
                                ui.TextInput(bind: g.states_wrong, placeholder: "Your name")
                            }
                        }
                    }
                }
            }
        }
        part("Slider", "A value along a range, dragged or stepped.") {
            grid(min: 12rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Slider(bind: g.states_share, label: "Volume")
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Slider(bind: g.states_share, label: "Volume")
                    }
                }
                sample("Dragged") {
                    ui.InState(.pressed) {
                        ui.Slider(bind: g.states_share, label: "Volume")
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Slider(bind: g.states_share, label: "Volume")
                    }
                }
            }
        }
        part("Dropdown", "One of a list of choices, opened to choose.") {
            grid(min: 12rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Dropdown(bind: g.states_room, options: ["Studio", "Gallery", "Annex"])
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Dropdown(bind: g.states_room, options: ["Studio", "Gallery", "Annex"])
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.Dropdown(bind: g.states_room, options: ["Studio", "Gallery", "Annex"])
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Dropdown(bind: g.states_room, options: ["Studio", "Gallery", "Annex"])
                    }
                }
            }
        }
        part("Chip", "A tag to pick or take off.") {
            grid(min: 7rem) {
                sample("Rest") {
                    ui.InState(.rest) {
                        ui.Chip("Design", icon: .tag, selected: false)
                    }
                }
                sample("Hovered") {
                    ui.InState(.hovered) {
                        ui.Chip("Design", icon: .tag, selected: false)
                    }
                }
                sample("Pressed") {
                    ui.InState(.pressed) {
                        ui.Chip("Design", icon: .tag, selected: false)
                    }
                }
                sample("Focused") {
                    ui.InState(.focused) {
                        ui.Chip("Design", icon: .tag, selected: false)
                    }
                }
                sample("Selected") {
                    ui.InState(.rest) {
                        ui.Chip("Design", icon: .tag, selected: true)
                    }
                }
                sample("Selected, hovered") {
                    ui.InState(.hovered) {
                        ui.Chip("Design", icon: .tag, selected: true)
                    }
                }
            }
        }
    }
}

/// A sample of a control: what state it shows, over it.
template view sample(name: Text, content: View) {
    stack(gap: .tight) {
        ui.Text(name, role: .caption, secondary: true)
        content
    }
}

/// The states page, as the gallery's first start fills it.
scenario "states" {
    root Gallery
}
