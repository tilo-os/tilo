/// How soon a report is due, as a case the report holds.
enum Urgency {
    /// No hurry
    none
    /// Low
    low
    /// Medium
    medium
    /// High
    high (icon: .flag, tone: .warning)
}

/// The controls' page: each kind in a section of its own, each in the states a person meets it
/// in. Every value is the document's or the page's own, so each control can be played with.
view controls(g: Gallery) {
    state cadence: Int = 1
    state showing: Int = 0
    state step: Int = 1
    state layer: Int = 0
    state swatch: Int = 0
    state text_size: Int = 1
    state delivery: Int = 0
    ui.Text(
        "Fields, switches, sliders, pickers, dates and colours, each bound to the document's own.",
        role: .caption,
        secondary: true,
    )
    stack(max: 44rem, gap: .section) {
        ui.Section("Boxes and switches") {
            ui.Checkbox(bind: g.controls_by_email, label: "Send by email")
            ui.Toggle(bind: g.controls_repeating, label: "Repeat weekly")
        }
        ui.Section("Numbers") {
            form {
                field("Margin") {
                    ui.NumberInput(
                        bind: g.controls_margin,
                        label: "Margin",
                        min: 0.0,
                        max: 64.0,
                        step: 4.0,
                        suffix: "px",
                    )
                }
                field("Share of the time") {
                    ui.Slider(
                        bind: g.controls_share,
                        label: "Share of the time",
                        step: 0.05,
                        shows_value: true,
                    )
                }
            }
        }
        ui.Section("Choices") {
            form {
                field("Room") {
                    ui.Dropdown(
                        bind: g.controls_room,
                        options: ["Studio", "Gallery", "Annex"],
                        placeholder: "Choose a room",
                    )
                }
                field("Urgency") {
                    ui.EnumField(bind: g.controls_urgency, label: "Urgency")
                }
                field("Repeats") {
                    ui.ToggleGroup(selected: cadence, label: "Repeats") {
                        ui.Segment("Daily")
                        ui.Segment("Weekly")
                        ui.Segment("Monthly")
                        on change(i): set(cadence, i)
                    }
                }
            }
        }
        ui.Section("Radio buttons") {
            ui.RadioGroup(selected: delivery, label: "Send the report by") {
                ui.Radio("Email", detail: "A copy to everyone on the team")
                ui.Radio("Chat", detail: "A message in the team's channel")
                ui.Radio(
                    "Post",
                    detail: "Printed and sent on Monday",
                    enabled: false,
                    description: "The print room is closed this week",
                )
                on change(i): set(delivery, i)
            }
            ui.RadioGroup(
                selected: 1,
                label: "Who can open it",
                enabled: false,
                description: "Only the page's owner changes who can open it",
            ) {
                ui.Radio("Only the people it is shared with")
                ui.Radio("Everyone in the workspace, including guests who joined for one project")
            }
        }
        ui.Section("Views") {
            ui.ViewTabs(selected: showing, label: "Report") {
                ui.Segment("Summary")
                ui.Segment("Details")
                ui.Segment("History")
                on change(i): set(showing, i)
            }
        }
        ui.Section("People") {
            form {
                field("Owner") {
                    ui.PersonField(
                        bind: g.controls_owner,
                        people: workspace.members,
                        me: invoker,
                    )
                }
                field("Reviewer") {
                    ui.PersonField(
                        bind: g.controls_reviewer,
                        people: workspace.members,
                        me: invoker,
                    )
                }
            }
        }
        ui.Section("Progress") {
            ui.Stepper(steps: ["Draft", "Review", "Send"], current: step) {
                on station(i): set(step, i)
            }
        }
        ui.Section("Meters") {
            ui.LevelMeter(source: "alsa_input.example", label: "Microphone level")
        }
        ui.Section("Days and times") {
            form {
                field("Starts") {
                    ui.DateField(bind: g.controls_starts, label: "Starts", optional: false) {
                        time: {
                            ui.TimeField(bind: g.controls_clock)
                        }
                    }
                }
                field("Ends") {
                    ui.DateField(bind: g.controls_ends, label: "Ends", optional: true)
                }
            }
            ui.DatePicker(bind: g.controls_starts, label: "Starts")
            ui.TimePicker(bind: g.controls_clock, label: "At")
        }
        ui.Section("Colours") {
            // A chooser of its own parts, named by the caption over it.
            ui.Text("Highlight", role: .caption, secondary: true)
            ui.Labelled("Highlight") {
                ui.ColorPicker(
                    bind: g.controls_accent,
                    swatches: ["#3a7bd5", "#d55a3a", "#3ad57b"],
                    columns: 3,
                )
            }
            ui.ColorSwatchPicker(
                swatches: ["#3a7bd5", "#d55a3a", "#3ad57b", "#d5c23a"],
                columns: 4,
                selected: swatch,
                label: "Accent",
            ) {
                on change(i): set(swatch, i)
            }
        }
        ui.Section("Text") {
            form {
                field("Title") {
                    ui.EditableLabel(bind: g.controls_title, label: "Title")
                }
                field("Open with") {
                    ui.ShortcutRecorder(
                        bind: g.controls_shortcut,
                        default: "Ctrl+K",
                        label: "Open with",
                    )
                }
                field("Send to") {
                    ui.Autocomplete(bind: g.controls_recipient, placeholder: "Name or address") {
                        ui.Suggestion("ada@example.com", label: "Ada")
                        ui.Suggestion("bo@example.com", label: "Bo")
                    }
                }
                field("Note", help: "Shown with the title.") {
                    ui.TextInput(bind: g.controls_note, placeholder: "A note for the team")
                }
                field("Notes") {
                    ui.TextInput(
                        bind: g.controls_notes,
                        lines: 3,
                        placeholder: "Anything else worth saying",
                    )
                }
            }
            ui.Link("https://tilo.sh", label: "Read about Tilo")
        }
        ui.Section("Layers") {
            ui.ControlRow(label: "Background", selected: layer == 0) {
                ui.Text("Background")
                spacer
                ui.Toggle(bind: g.controls_background)
                on choose: set(layer, 0)
            }
            ui.ControlRow(label: "Lettering", selected: layer == 1) {
                ui.Text("Lettering")
                spacer
                ui.Toggle(bind: g.controls_lettering)
                on choose: set(layer, 1)
            }
        }
        ui.Section("Pictures") {
            ui.PictureChoice(selected: text_size, label: "Text size") {
                ui.PictureOption("Small", picture: .text_size_small)
                ui.PictureOption("Regular", picture: .text_size_regular)
                ui.PictureOption("Large", picture: .text_size_large)
                on change(i): set(text_size, i)
            }
        }
    }
}

/// The controls' page, with a report a week from starting, looked after by Mia.
scenario "controls" {
    let g = root Gallery
    g.controls_owner = .some(person("Mia"))
    g.controls_starts = 2026-11-03
    g.controls_clock = 2026-11-03T09:30Z
}
