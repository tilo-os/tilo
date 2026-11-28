/// What the devices of this machine are doing with the person's privacy, and what the workspaces
/// let their apps reach beyond their own documents.
///
/// The device's side is the session's own reading (`device.privacy`): whether the camera, the
/// microphone, the screen and the location are here, whether a switch blocks them, which
/// application uses one now, and every answer the person gave about the devices of theirs, on
/// whichever of their devices the answer stands. The workspace's side is the grants its own
/// Permissions tab lists (`workspace.access`), read here and changed there.
view permissions_pane() {
    let p = device.privacy
    let access = workspace.access
    ui.Section("This device") {
        ui.Text(
            "These permissions belong to this device. They do not change what another workspace allows.",
            role: .caption,
            secondary: true,
        )
        if p.capabilities.is_empty() {
            ui.Text(
                "This device's privacy can't be read here, so these are unknown rather than unused.",
                role: .caption,
                secondary: true,
            )
        }
        for posture in p.capabilities {
            ui.ListRow(
                capability_title(posture.capability),
                secondary: posture_words(posture),
                icon: if posture.capability == .camera {
                    .camera
                } else if posture.capability == .microphone {
                    .mic
                } else if posture.capability == .screen {
                    .monitor
                } else if posture.capability == .location { .map_pin } else { .keyboard },
            ) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        if blocked(posture) {
                            ui.Chip("Blocked", tone: .warning)
                        }
                        for used in posture.in_use {
                            ui.Chip("In use by {used.name}", tone: .warning)
                        }
                        if posture.watched and posture.in_use.is_empty() {
                            ui.Chip("Not in use", tone: .neutral)
                        }
                    }
                }
            }
        }
    }
    ui.Section("Your answers") {
        ui.Text(
            "What you answered when an application asked for the camera, the microphone, the screen or the keyboard and pointer, or a program asked to act as you. An answer holds only on the device it names.",
            role: .caption,
            secondary: true,
        )
        if p.answers.is_empty() {
            ui.Text("No application has a lasting answer on any of your devices.", role: .caption)
        }
        for answered in p.answers {
            ui.ListRow(
                answered.name,
                secondary: answer_words(answered),
                icon: if answered.capability == .camera {
                    .camera
                } else if answered.capability == .microphone {
                    .mic
                } else if answered.capability == .screen {
                    .monitor
                } else if answered.capability == .location {
                    .map_pin
                } else if answered.capability == .api { .terminal } else { .keyboard },
            ) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        if answered.allowed {
                            ui.Chip("Allowed", tone: .neutral)
                        } else {
                            ui.Chip("Refused", tone: .warning)
                        }
                        ui.Button("Take back", size: .small, role: .plain) {
                            on press: answered.forget()
                        }
                    }
                }
            }
        }
    }
    ui.Section("Saved passwords and keys") {
        if let saved = p.secrets {
            ui.Text(
                "Applications that stored or read saved passwords and keys on this device. Stopping one denies it all saved secrets; its own items stay until you forget it.",
                role: .caption,
                secondary: true,
            )
            if saved.read_error != .none {
                ui.Text(
                    "The saved-secrets service can't be reached, so its applications can't be listed.",
                    role: .caption,
                    secondary: true,
                )
            } else if saved.apps.is_empty() {
                ui.Text("No application has used saved passwords or keys yet.", role: .caption)
            }
            for stored in saved.apps {
                ui.ListRow(stored.name, secondary: secret_words(stored), icon: .key) {
                    trailing: {
                        row(gap: .tight, align: .center, wrap: true) {
                            if stored.allowed {
                                ui.Chip("Allowed", tone: .neutral)
                            } else {
                                ui.Chip("Refused", tone: .warning)
                            }
                            ui.Button(
                                if stored.allowed { "Stop" } else { "Allow" },
                                size: .small,
                                role: .plain,
                            ) {
                                on press: p.set_secrets_access(
                                    app: stored.app,
                                    allowed: not stored.allowed,
                                )
                            }
                            ui.Button("Forget", size: .small, role: .destructive) {
                                on press: p.forget_secrets(app: stored.app)
                            }
                        }
                    }
                }
            }
        } else {
            ui.Text(
                "The applications that use saved passwords and keys can't be read here.",
                role: .caption,
                secondary: true,
            )
        }
    }
    ui.Section("Device protection") {
        if let protected = p.security {
            ui.Text(
                "What protects this device itself. These belong to the machine, and not to any workspace.",
                role: .caption,
                secondary: true,
            )
            ui.ListRow(
                "Disk encryption",
                secondary: protection_words(protected.disk_encryption, true),
                icon: .hard_drive,
            )
            ui.ListRow(
                "Secure Boot",
                secondary: protection_words(protected.secure_boot, false),
                icon: .shield,
            )
            ui.ListRow("Firewall", secondary: firewall_words(protected), icon: .shield)
        } else {
            ui.Text(
                "What protects this device itself can't be read here.",
                role: .caption,
                secondary: true,
            )
        }
    }
    ui.Section("Workspace access") {
        ui.Text(
            "What the workspaces here give their apps, people and agents beyond their own documents. Changing one belongs to the workspace itself, under its permissions.",
            role: .caption,
            secondary: true,
        )
        if not access.known {
            ui.Text(
                "Workspace access can't be checked, so it is unknown what the workspaces gave. Try again once the desktop has finished starting.",
                role: .caption,
                secondary: true,
            )
        } else if access.workspaces.is_empty() {
            ui.Text("No workspace here gives anything beyond its own documents.", role: .caption)
        }
        for granted_in in access.workspaces {
            ui.Text(
                if granted_in.name == "" { "This workspace" } else { granted_in.name },
                role: .label,
            )
            for grant in granted_in.grants {
                ui.ListRow(grant.holder, secondary: grant_words(grant), icon: .shield) {
                    trailing: {
                        ui.Chip(grant.section, tone: .neutral)
                    }
                }
            }
        }
    }
}

/// What a device of this machine is called.
fn capability_title(capability: device.Capability) -> Text {
    if capability == .camera {
        "Camera"
    } else if capability == .microphone {
        "Microphone"
    } else if capability == .screen {
        "Screen"
    } else if capability == .location {
        "Location"
    } else if capability == .input {
        "Keyboard and pointer"
    } else if capability == .api { "Acting as you" } else { "Sound the machine plays" }
}

/// Whether a device is blocked by a switch, as the reading says.
fn blocked(posture: device.Posture) -> Bool {
    posture.switch == .hardware-blocked or posture.switch == .software-blocked
}

/// What a device's own state is, in a line: whether the machine has it, what blocks it, and what
/// uses it.
fn posture_words(posture: device.Posture) -> Text {
    let uses = posture.in_use.len()
    if not posture.present {
        if posture.capability == .location {
            "This machine can't tell where it is"
        } else { "Not on this device" }
    } else if posture.switch == .hardware-blocked {
        "Blocked by a switch on the device itself"
    } else if posture.switch == .software-blocked {
        "Blocked in software"
    } else if posture.capability == .microphone and posture.mute_known {
        if posture.muted { "Muted" } else { "Not muted" }
    } else if posture.capability == .location {
        "The system asks you each time an application wants it, so this list cannot say who uses it"
    } else if not posture.watched {
        "This desktop cannot say which application uses it"
    } else if uses == 0 {
        "No application is using it now"
    } else if uses == 1 {
        "One application is using it now"
    } else { "{uses} applications are using it now" }
}

/// One answer the person gave, said plainly: what it allows or refuses, on which device, and for
/// how long.
fn answer_words(answered: device.Answer) -> Text {
    let what = if answered.allowed {
        capability_title(answered.capability)
    } else if answered.capability == .microphone {
        "May not use the microphone"
    } else if answered.capability == .camera {
        "May not use the camera"
    } else if answered.capability == .screen {
        "May not use the screen"
    } else if answered.capability == .input {
        "May not use the keyboard and pointer"
    } else if answered.capability == .location {
        "May not use the location"
    } else if answered.capability == .api {
        "May not act as you"
    } else {
        "May not use the sound the machine plays"
    }
    let where = if answered.this_device {
        "on this device"
    } else if answered.device == "" { "on another of your devices" } else { "on {answered.device}" }
    "{what}, {where} · {standing_words(answered)}"
}

/// How long an answer stands, as the reading says it.
fn standing_words(answered: device.Answer) -> Text {
    if answered.standing == .until-revoked {
        "until you take it back"
    } else if answered.standing == .session {
        "until this session ends"
    } else if answered.standing == .once {
        "for one use"
    } else if answered.until_unix_ms > 0 {
        "until {moment_text(answered.until_unix_ms / 1000)}"
    } else { "until it lapses" }
}

/// The instant a count of Unix seconds names, as a clock reads it.
fn moment_text(at: Int) -> Text {
    (Time("1970-01-01T00:00:00Z") + Duration(at)).clock()
}

/// One grant a workspace gave, said with what it allows and how long it lasts.
fn grant_words(grant: workspace.AccessEntry) -> Text {
    if grant.granted_by == "" {
        "{grant.allows} · {grant.expiry}"
    } else { "Given by {grant.granted_by}" }
}

/// What an application of the saved secrets keeps and how often it used them.
fn secret_words(stored: device.SecretApp) -> Text {
    "{stored.items} saved, used {stored.uses} times"
}

/// Whether a protection of this device is on, in the words the row reads it by: a disk is
/// encrypted or not, and firmware verifies what it boots or not.
fn protection_words(state: device.Protection, disk: Bool) -> Text {
    if state == .on {
        if disk { "Encrypted" } else { "Enabled" }
    } else if state == .off {
        if disk { "Not encrypted" } else { "Disabled" }
    } else { "This device can't say" }
}

/// The firewall in force on this device, or why it could not be read.
fn firewall_words(security: device.Security) -> Text {
    if security.firewall == .firewalld {
        "Active, firewalld"
    } else if security.firewall == .nftables {
        "Active, nftables"
    } else if security.firewall == .off {
        "Not active"
    } else if let why = security.firewall_error {
        "Can't read the firewall state: {why}"
    } else { "Can't read the firewall state" }
}
