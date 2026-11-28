/// Every app that can reach outside the workspace or reach a file, and what each one reaches.
///
/// The outside capabilities are each app's own package declares (`smtp.send`): a switch turns one
/// off, writing the workspace's denial through the access root's own action, and turning it back
/// on takes that denial back. What the app queues for a capability that is off settles as refused
/// and never leaves the workspace.
///
/// Below them is the same view Files draws for a file, grouped by app: a workspace's grant on a
/// file's node, or this device's record of a file chosen outside any workspace. Taking one back
/// writes the workspace's change or the device's record, as whoever gave it.
view apps_access_pane() {
    let access = workspace.access
    let entries = access.workspaces.flat_map(w -> w.grants)
    ui.Page {
        ui.Section("Beyond the workspace") {
            ui.Text(
                "Each app's code can queue these to go out of the workspace, such as sending a message or calling a service. Turning one off makes what it queues for that capability settle as refused, and nothing leaves; turning it back on lets it go.",
                role: .caption,
                secondary: true,
            )
            if not access.known {
                ui.Text(
                    "What the apps here can reach beyond the workspace can't be checked, so it is unknown rather than nothing.",
                    role: .caption,
                    secondary: true,
                )
            } else if access.outside.is_empty() {
                ui.Text(
                    "No app here reaches beyond the workspace.",
                    role: .caption,
                    secondary: true,
                )
            }
            for granted_in in access.outside {
                ui.Text(
                    if granted_in.workspace_name == "" {
                        "This workspace"
                    } else { granted_in.workspace_name },
                    role: .label,
                )
                for each in granted_in.apps {
                    ui.Text(each.name, role: .body)
                    for capability in each.capabilities {
                        ui.ListRow(
                            outside_words(capability.capability),
                            secondary: outside_standing(capability),
                            icon: .arrow_up_right,
                        ) {
                            trailing: {
                                if capability.can_change {
                                    ui.Toggle(
                                        shown: capability.allowed,
                                        label: if capability.allowed {
                                            "Allowed"
                                        } else { "Blocked" },
                                    ) {
                                        on check(on): workspace.access.set_outside(
                                            workspace: granted_in.workspace,
                                            app: each.app,
                                            capability: capability.capability,
                                            allowed: on,
                                        )
                                    }
                                } else if capability.allowed {
                                    ui.Chip("Allowed", tone: .neutral)
                                } else {
                                    ui.Chip("Blocked", tone: .warning)
                                }
                            }
                        }
                    }
                }
            }
        }
        ui.Section("Files and folders") {
            if entries.is_empty() {
                ui.Text("No app or person was given a file.", role: .caption, secondary: true)
            } else {
                files.access_list(
                    entries: entries.sort_by(e -> e.path).sort_by(e -> e.holder),
                    empty: "",
                    by_app: true,
                    in_column: false,
                )
            }
        }
    }
}

/// What one of an app's outside capabilities does, in plain words: what its code may send out.
fn outside_words(capability: Text) -> Text {
    if capability == "smtp.send" {
        "Send email"
    } else if capability == "imap.check" {
        "Check the mail account"
    } else if capability == "imap.search" {
        "Search the mail server"
    } else if capability == "imap.older" {
        "Fetch older mail"
    } else if capability == "imap.save_draft" {
        "Save a draft on the mail server"
    } else if capability == "imap.purge" {
        "Delete mail on the mail server"
    } else if capability == "caldav.check" {
        "Check the calendar account"
    } else if capability == "caldav.answer" {
        "Answer a calendar invitation"
    } else if capability == "caldav.import" {
        "Import a calendar"
    } else if capability == "music.spotify" {
        "Use the Spotify account"
    } else if capability == "music.stream" {
        "Stream a song"
    } else if capability == "music.search" {
        "Search music"
    } else if capability == "music.home" {
        "Load music's home"
    } else if capability == "music.artist" {
        "Load an artist"
    } else if capability == "music.album" {
        "Load an album"
    } else if capability == "music.playlist" {
        "Load a playlist"
    } else if capability == "music.lyrics" {
        "Fetch a song's lyrics"
    } else if capability == "music.pin" {
        "Keep a song on this device"
    } else if capability == "youtube.search" {
        "Search YouTube"
    } else if capability == "youtube.subscriptions" {
        "Load subscriptions"
    } else if capability == "youtube.trending" {
        "Load trending videos"
    } else if capability == "youtube.channel" {
        "Load a channel"
    } else if capability == "youtube.playlist" {
        "Load a video playlist"
    } else if capability == "youtube.stream" {
        "Stream a video"
    } else if capability == "youtube.comments" { "Load comments" } else { capability }
}

/// How one capability stands now, said with what it does or what turning it off stops.
fn outside_standing(capability: workspace.OutsideCapability) -> Text {
    if capability.allowed {
        "Sends beyond the workspace"
    } else {
        "Stopped: what it queues for this settles as refused"
    }
}
