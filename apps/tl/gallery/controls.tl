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

/// The controls a form is made of: boxes and switches, a number, a choice from a short list, a
/// person, a strip of choices, tabs, a day and a time, a colour, a shortcut, a completion, a
/// picture choice, a row of controls, and a link. Each is bound to a real field of the document,
/// so a change made in the window is the document's.
type Controls {
    title: Text
    /// Whether the report goes by email.
    by_email: Bool = false
    /// Whether it repeats.
    repeating: Bool = false
    /// How many fit.
    seats: Int = 3
    /// How much of the time it runs.
    share: Float = 0.6
    /// The room it is in.
    room: Text = "Studio"
    /// How soon the report is due.
    urgency: Urgency = .none
    /// Who looks after the report.
    owner: Option<Principal> = .none
    /// Who reviews it, nobody picked yet.
    reviewer: Option<Principal> = .none
    /// Which view shows.
    showing: Int = 1
    /// Which step the run is on.
    step: Int = 1
    /// The day it starts.
    starts: Date = Date("2026-11-03")
    /// The time of day it starts.
    clock: Time = Time("2026-11-03T09:30:00Z")
    /// The day it ends, which may be none.
    ends: Option<Date> = .none
    /// The accent the report wears.
    accent: Text = "#3a7bd5"
    /// The key it opens with.
    shortcut: Text = "Ctrl+K"
    /// Who it goes to, typed.
    recipient: Text = ""
    /// Whether the archive had everything.
    archived: Bool = false
    /// The size of the text in the report.
    text_size: Int = 1
}

default view controls for Controls

/// The controls window: each kind in a section of its own page.
view controls(c: Controls) {
    state last: Text = ""
    std.page(c) {
        ui.Section("Boxes and switches") {
            ui.Checkbox(value: c.by_email, label: .some("Send by email"))
            ui.Toggle(value: c.repeating, label: .some("Repeat weekly"))
            ui.ControlRow(label: .some("Archived"), selected: c.archived) {
                ui.Text("Archived items stay for 30 days.", role: .caption, secondary: true)
            }
        }
        ui.Section("Numbers") {
            ui.NumberInput(
                value: c.share,
                label: .some("Share"),
                min: .some(0.0),
                max: .some(1.0),
                step: .some(0.05),
                precision: .some(2),
            )
            ui.Slider(
                value: c.share,
                label: .some("Time"),
                max: .some(1.0),
                step: .some(0.05),
                shows_value: true,
            )
        }
        ui.Section("Choices") {
            ui.Dropdown(
                value: c.room,
                options: ["Studio", "Gallery", "Annex"],
                placeholder: .some("Choose a room"),
            )
            ui.ToggleGroup(selected: .some(c.step), caption: .some("Steps")) {
                ui.Segment("Grid")
                ui.Segment("List")
                ui.Segment("Board")
            }
            ui.ViewTabs(selected: .some(c.showing), label: .some("Views")) {
                ui.Segment("Summary")
                ui.Segment("Details")
                ui.Segment("History")
            }
        }
        ui.Section("Cases") {
            ui.EnumField(value: c.urgency, label: .some("Urgency")) {
                ui.EnumOption("No hurry", name: "none")
                ui.EnumOption("Low", name: "low")
                ui.EnumOption("Medium", name: "medium")
                ui.EnumOption("High", name: "high", icon: .flag, tone: .warning)
            }
        }
        ui.Section("People") {
            ui.Text("Who looks after it", role: .caption, secondary: true)
            ui.PersonField(
                value: c.owner,
                people: .some(workspace.members),
                me: .some(workspace.me),
            )
            ui.Text("Who reviews it, still to be picked", role: .caption, secondary: true)
            ui.PersonField(
                value: c.reviewer,
                people: .some(workspace.members),
                me: .some(workspace.me),
            )
        }
        ui.Section("Steps") {
            ui.Stepper(steps: ["Draft", "Review", "Send"], current: .some(c.step)) {
                on station: set(last, "Step")
            }
        }
        ui.Section("Days and times") {
            ui.DateField(value: c.starts, label: .some("Starts"), optional: false)
            ui.DateField(value: c.ends, label: .some("Ends"), optional: true)
            ui.DatePicker(value: c.starts)
            ui.TimeField(value: c.clock)
            ui.TimePicker(value: c.clock, label: .some("At"))
        }
        ui.Section("Colours") {
            ui.ColorPicker(
                value: c.accent,
                swatches: ["#3a7bd5", "#d55a3a", "#3ad57b"],
                columns: .some(3),
            )
            ui.ColorSwatchPicker(
                swatches: ["#3a7bd5", "#d55a3a", "#3ad57b", "#d5c23a"],
                columns: 4,
                selected: .some(0),
            )
        }
        ui.Section("Text") {
            ui.EditableLabel(value: c.title)
            ui.ShortcutRecorder(
                value: c.shortcut,
                default: .some("Ctrl+K"),
                label: .some("Open with"),
            )
            ui.Autocomplete(value: c.recipient, placeholder: .some("Who it goes to")) {
                ui.Suggestion("ada@example.com")
                ui.Suggestion("bo@example.com", label: .some("Bo"))
            }
            ui.Link("https://tilo.coffee", label: .some("The site"))
        }
        ui.Section("Pictures") {
            ui.PictureChoice(selected: .some(c.text_size)) {
                ui.PictureOption("Small", picture: .text_size_small)
                ui.PictureOption("Regular", picture: .text_size_regular)
                ui.PictureOption("Large", picture: .text_size_large)
            }
        }
    }
}

/// The controls group, with a report a week from starting.
scenario "controls" {
    let mia = person("Mia")
    let g = root Gallery
    g.controls.insert(
        Controls(
            title: "Controls",
            starts: 2026-11-03,
            clock: 2026-11-03T09:30Z,
            owner: .some(mia),
        ),
    )
}
