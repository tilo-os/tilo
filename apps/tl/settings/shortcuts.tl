/// The chords the desktop's own shortcuts are on. Every change is the shortcuts section's own
/// verb, checked against every other chord, so the shell, the shortcut guide and an agent keep
/// one table.
view shortcuts_pane() {
    state shortcut: Text = "search"
    state chord: Text = ""
    let table = settings.shortcuts_table
    ui.Section("The desktop's shortcuts") {
        for binding in table.bindings {
            ui.ListRow(
                shortcut_label(binding.id),
                secondary: if binding.chord != "" { binding.chord } else { "No keys" },
                icon: .command,
            )
        }
    }
    ui.Section("Chords that run an action") {
        if table.actions.is_empty() {
            ui.Text(
                "No action is on a chord of its own yet.",
                role: .caption,
                secondary: true,
            )
        }
        for action in table.actions {
            ui.ListRow(action.label, secondary: action.chord, icon: .zap)
        }
    }
    ui.Section("Change one") {
        ui.Dropdown(bind: shortcut, options: table.bindings.map(b -> b.id), caption: "The shortcut")
        ui.ShortcutRecorder(bind: chord, label: "The keys it stands on")
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Put it on these keys", role: .primary) {
                on press: settings.rebind(
                    settings.shortcuts,
                    shortcut: shortcut,
                    chord: chord,
                )
            }
            ui.Button("Its usual keys", size: .small) {
                on press: settings.reset(settings.shortcuts, shortcuts: [shortcut])
            }
        }
        ui.Text(
            "A chord written as Ctrl+Shift+K. A chord another shortcut or an action already runs is refused, naming which.",
            role: .caption,
            secondary: true,
        )
    }
    ui.Section("Every shortcut") {
        ui.Button("Put every shortcut back", icon: .rotate_ccw) {
            on press: settings.reset_all(settings.shortcuts)
        }
    }
}

/// What a shortcut is called, in words.
fn shortcut_label(id: Text) -> Text {
    if id == "search" {
        "Search"
    } else if id == "workspace-overview" {
        "Every window at once"
    } else if id == "lock" {
        "Lock the screen"
    } else if id == "quick-settings" {
        "Quick settings"
    } else if id == "shortcut-help" {
        "This guide"
    } else if id == "focus-previous-window" {
        "The window before"
    } else if id == "focus-next-window" {
        "The window after"
    } else if id == "previous-tab" {
        "The tab before"
    } else if id == "next-tab" {
        "The tab after"
    } else if id == "tab-menu" {
        "The tabs' menu"
    } else if id == "previous-strip" {
        "The strip above"
    } else if id == "next-strip" {
        "The strip below"
    } else if id == "previous-workspace" {
        "The workspace before"
    } else if id == "next-workspace" {
        "The workspace after"
    } else if id == "toggle-maximize" {
        "Fill the screen, or not"
    } else if id == "maximize-window" {
        "Fill the screen"
    } else if id == "restore-window" {
        "Take the screen no more"
    } else if id == "detach-tab" {
        "Take the tab out"
    } else if id == "attach-tab-left" {
        "Attach the tab to the left"
    } else if id == "attach-tab-right" {
        "Attach the tab to the right"
    } else if id == "move-group-left" {
        "Move the window to the left"
    } else if id == "move-group-right" {
        "Move the window to the right"
    } else if id == "move-group-to-previous-strip" {
        "Move the window above"
    } else if id == "move-group-to-next-strip" {
        "Move the window below"
    } else if id == "move-group-to-previous-display" {
        "Move the window to the screen before"
    } else if id == "move-group-to-next-display" {
        "Move the window to the screen after"
    } else if id == "resize-narrower" {
        "Narrower"
    } else if id == "resize-wider" {
        "Wider"
    } else if id == "reopen-closed-tab" {
        "Bring back a closed tab"
    } else if id == "open-files" {
        "Files"
    } else if id == "open-terminal" {
        "A terminal"
    } else if id == "clipboard-history" {
        "What was copied before"
    } else if id == "notifications" {
        "Notifications"
    } else if id == "toggle-fullscreen" {
        "Full screen, or not"
    } else if id == "toggle-magnifier" {
        "The magnifier"
    } else if id == "magnifier-zoom-in" {
        "Magnify more"
    } else if id == "magnifier-zoom-out" {
        "Magnify less"
    } else if id == "power-menu" {
        "The power menu"
    } else if id == "next-input-source" {
        "The next keyboard layout"
    } else if id == "emoji-picker" {
        "Emoji"
    } else if id == "ask-agent" {
        "Ask an agent"
    } else if id == "say-something" {
        "Say something"
    } else if id == "report-problem" {
        "Report a problem"
    } else if id == "dictation-toggle" {
        "Start or stop dictation"
    } else if id == "dictation-finish" {
        "Finish the dictation"
    } else if id == "dictation-cancel" {
        "Leave the dictation"
    } else if id == "toggle-float" {
        "Float the window, or not"
    } else {
        id
    }
}
