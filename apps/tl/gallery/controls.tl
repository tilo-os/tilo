use tilo.workspace as workspace

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

/// How many times `query` stands in `note`, in the case the find matches in: what the bar's count
/// says, as a document's own find would.
fn matches_in(note: Text, query: Text, match_case: Bool) -> Int {
    if query.trim() == "" {
        return 0
    }
    let hay = if match_case { note } else { note.lower() }
    let needle = if match_case { query } else { query.lower() }
    hay.split(needle).len() - 1
}

/// The find's case, after choosing `chosen` among its options: matching case, or as it was for an
/// option that leaves the case alone.
fn toggled_case(chosen: Text, current: Bool) -> Bool {
    if chosen == "case" { not current } else { current }
}

/// `note` with every `query` replaced by `with`: what the bar's replace writes, and the note as
/// it stands when there is nothing to find.
fn replaced(note: Text, query: Text, with: Text) -> Text {
    if query == "" { note } else { note.replace(query, with) }
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
    // The find's own: the note it works through, what is typed, the match it stands on, and
    // whether its case matters.
    state find_note: Text = "The ship sails at dawn. The Ship returns at dusk. One ship waits."
    state find_query: Text = "ship"
    state find_at: Int = 0
    state find_case: Bool = false
    let found = matches_in(find_note, find_query, find_case)
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
        ui.Section("Find in a page") {
            ui.Text(
                "The window bar's find over the page's own note: typing narrows the count, the arrows step through the matches, an option changes what matches, and replacing writes the note.",
                role: .caption,
                secondary: true,
            )
            ui.FindControls(
                bind: find_query,
                placeholder: "Find in the note",
                in_page: true,
                acts_on: "document",
                current: if found == 0 { 0 } else { find_at % found + 1 },
                total: found,
                replaces: true,
                regex: false,
                hex: false,
                match_case: find_case,
                match_whole_word: false,
                match_regex: false,
                match_hex: false,
                window: "The note",
                app: "Gallery",
            ) {
                on changed(text): set(find_query, text).then(set(find_at, 0))
                on step(forward): set(
                    find_at,
                    if forward { find_at + 1 } else { find_at + found - 1 },
                )
                on option(chosen): set(find_case, toggled_case(chosen, find_case))
                on replace(text): set(find_note, replaced(find_note, find_query, text))
                on replace_all(text): set(find_note, replaced(find_note, find_query, text))
                on close: set(find_query, "")
            }
            ui.Text(find_note)
        }
        ui.Section("People") {
            form {
                field("Owner") {
                    ui.PersonField(
                        bind: g.controls_owner,
                        people: workspace.members.people,
                        me: invoker,
                    )
                }
                field("Reviewer") {
                    ui.PersonField(
                        bind: g.controls_reviewer,
                        people: workspace.members.people,
                        me: invoker,
                    )
                }
            }
        }
        ui.Section("Progress") {
            ui.Stepper(steps: ["Draft", "Review", "Send"], current: step) {
                on station(i): set(step, i)
            }
            // A page of a short sequence: where it stands, the page that scrolls, and the ways
            // forward under it.
            ui.GuidedPage(
                steps: ["Draft", "Review", "Send"],
                current: step,
                scroll_label: "The report",
                shows_bar: true,
            ) {
                on station(i): set(step, i)
                body: {
                    ui.Text(
                        "Where the page holds more than the card does, it scrolls under the track.",
                        secondary: true,
                    )
                }
                actions: {
                    ui.Button("Continue", role: .primary) {
                        on press: set(step, (step + 1) % 3)
                    }
                }
            }
        }
        ui.Section("Meters") {
            ui.BarGauge(0.36, label: "Storage", readout: "91 of 256 GB", neutral: true)
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
            ui.Text("Accent", role: .caption, secondary: true)
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
        ui.Section("Keys") {
            // The on-screen keys an app draws: rows laid out by their widths, raw touch with one
            // finger per thumb, a long press that offers a letter's forms and a space bar that
            // steers the caret. The words around them — the field, the suggestions — are the
            // app's.
            ui.KeyPad(
                keys: [
                    ui.PadKey.key(
                        label: "Q",
                        name: "Q",
                        icon: "",
                        row: 0,
                        column: 0,
                        holds: ["1"],
                    ),
                    ui.PadKey.key(
                        label: "W",
                        name: "W",
                        icon: "",
                        row: 0,
                        column: 1,
                        holds: ["2"],
                    ),
                    ui.PadKey.key(
                        label: "E",
                        name: "E",
                        icon: "",
                        row: 0,
                        column: 2,
                        holds: ["3"],
                    ),
                    ui.PadKey.key(
                        label: "Shift",
                        name: "Shift",
                        icon: "arrow-big-up",
                        tone: .modifier,
                        width: 1.5,
                        row: 1,
                        column: 0,
                        holds: [],
                    ),
                    ui.PadKey.key(
                        label: "space",
                        name: "Space",
                        icon: "",
                        width: 3.0,
                        row: 1,
                        column: 1,
                        holds: [],
                        caret: true,
                    ),
                    ui.PadKey.key(
                        label: "Backspace",
                        name: "Backspace",
                        icon: "delete",
                        tone: .modifier,
                        width: 1.5,
                        row: 1,
                        column: 3,
                        holds: [],
                        repeat: true,
                    ),
                ],
                size: .compact,
                label: .some("On-screen keyboard"),
            ) {
                on press(i, at): set(cadence, i + at.len())
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
