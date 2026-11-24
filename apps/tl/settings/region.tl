/// How this person reads numbers, money, dates and the time: their formats, the language the
/// workspace speaks, and the week's first day.
view region_pane() {
    let r = settings.locale
    let a = settings.appearance
    let spoken = settings.languages
    ui.Section("Language") {
        ui.ListRow(
            "Follow the device's language",
            secondary: "The language this computer is set up in",
            icon: .monitor,
        ) {
            trailing: {
                if (a.language ?? "") == "" {
                    ui.Chip("In use", tone: .success)
                } else {
                    ui.Button("Use it", size: .small) {
                        on press: settings.choose_language(a, language: .none)
                    }
                }
            }
        }
        for language in spoken.languages {
            ui.ListRow(language.name, icon: .globe) {
                trailing: {
                    if (a.language ?? "") == language.tag {
                        ui.Chip("In use", tone: .success)
                    } else {
                        ui.Button("Use it", size: .small) {
                            on press: settings.choose_language(a, language: .some(language.tag))
                        }
                    }
                }
            }
        }
    }
    ui.Section("Formats") {
        setting("Money", "The currency prices are read in") {
            ui.EnumField(bind: r.currency, label: "Currency")
        }
        setting("Dates", "The order of a date's parts; the language's own unless chosen") {
            ui.EnumField(bind: r.date_format, label: "Date format")
        }
        setting("Times", "A twenty-four hour clock, or one with morning and evening") {
            ui.EnumField(bind: r.time_format, label: "Time format")
        }
        setting("Numbers", "A point or a comma between a number's whole part and its fraction") {
            ui.EnumField(bind: r.number_style, label: "Number style")
        }
        setting("Measurements", "Metres and litres, or feet and gallons") {
            ui.EnumField(bind: r.measurement, label: "Measurements")
        }
    }
    ui.Section("The week") {
        setting("Its first day", "The day calendars and planners start a week on") {
            ui.EnumField(bind: r.first_day_of_week, label: "First day of the week")
        }
    }
}
