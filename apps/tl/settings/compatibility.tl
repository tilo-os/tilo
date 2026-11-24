/// The optional components that let this computer play media and run Windows programs: the media
/// codecs and the Windows compatibility layer, each read from its resource and changed by its own
/// actions.
view compatibility_pane() {
    media_codecs_section()
    windows_support_section()
}

/// The media codec pack: whether it is installed and what installing it takes.
view media_codecs_section() {
    let codecs = device.codec_pack
    ui.Section("Media codecs") {
        if not codecs.available {
            ui.Text("This system does not offer the media codecs.", role: .caption, secondary: true)
        }
        ui.ListRow("H.264 and HEVC", secondary: codecs.summary, icon: .film) {
            trailing: {
                row(gap: .tight, align: .center, wrap: true) {
                    if codecs.offered.any(x -> x == "install") {
                        ui.Button("Install", size: .small, role: .primary) {
                            on press: device.install(device.codec_pack)
                        }
                    }
                    if codecs.offered.any(x -> x == "refresh") {
                        ui.Button("Check again", size: .small) {
                            on press: device.refresh(device.codec_pack)
                        }
                    }
                    if codecs.offered.any(x -> x == "remove") {
                        ui.Button("Remove", size: .small, role: .destructive) {
                            on press: device.remove(device.codec_pack)
                        }
                    }
                }
            }
        }
        if let problem = codecs.problem {
            ui.StatusLine(problem, status: .error)
        }
    }
}

/// The Windows compatibility layer: how it stands, how far an install has got, and the actions it
/// offers here. The Updates page shows the same card, beside the system, the firmware and the
/// codecs.
view windows_support_section() {
    let windows = device.windows_compat
    ui.Section("Windows support") {
        ui.Text(
            "Run Windows programs on this computer. Downloaded to this device from Tilo; you can remove it here at any time, and Windows app data stays on this device.",
            role: .caption,
            secondary: true,
        )
        if not windows.available {
            ui.Text("This system does not offer Windows support.", role: .caption, secondary: true)
        }
        // A request the helper has not picked up yet has no progress and offers nothing: say it
        // is starting rather than repeat the offer.
        let starting = not windows.pending.is_empty()
            and windows.offered.is_empty()
            and windows.state == .needs-install
        let status = if starting {
            std.say("Starting…")
        } else if windows.state == .installing {
            if let step = windows.step {
                windows_step_words(step)
            } else { windows_state_words(windows.state) }
        } else { windows_state_words(windows.state) }
        ui.ListRow("Windows programs", secondary: status, icon: .puzzle) {
            trailing: {
                row(gap: .tight, align: .center, wrap: true) {
                    // A change the helper has not picked up yet is already on its way: offering
                    // the same one again would queue it twice.
                    if windows.pending.is_empty() {
                        for offer in windows.offered {
                            if offer == .install {
                                ui.Button(
                                    if windows.state == .failed {
                                        "Try again"
                                    } else { "Install" },
                                    size: .small,
                                    role: .primary,
                                ) {
                                    on press: device.install(device.windows_compat)
                                }
                            }
                            if offer == .remove {
                                ui.Button(
                                    if windows.state == .waiting-for-network {
                                        "Cancel"
                                    } else { "Remove" },
                                    size: .small,
                                    role: .destructive,
                                ) {
                                    on press: device.remove(device.windows_compat)
                                }
                            }
                        }
                    }
                }
            }
        }
        if let size = windows.size {
            if windows.state == .needs-install or windows.state == .failed {
                ui.Text("Installing it downloads {size}.", role: .caption, secondary: true)
            }
        }
        if let progress = windows.progress {
            ui.ProgressBar(value: progress, label: "Installing Windows support")
        }
        if let problem = windows.problem {
            ui.StatusLine(problem, status: .error)
        }
    }
}

/// How the Windows layer stands, in the words the card shows beside its name.
fn windows_state_words(state: device.WindowsState) -> Text {
    if state == .ready {
        std.say("Installed")
    } else if state == .needs-install {
        std.say("Not installed")
    } else if state == .installing {
        std.say("Downloading…")
    } else if state == .waiting-for-network {
        std.say(
            "Waiting for an internet connection; it installs by itself once this device is online",
        )
    } else if state == .failed {
        std.say("Could not install")
    } else { std.say("Not available for this device") }
}

/// The step an install is taking, with how far its download has got where that is known.
fn windows_step_words(step: device.WindowsStep) -> Text {
    if let done = step.megabytes_done {
        if let total = step.megabytes_total {
            std.say("{step.label} {done} of {total} MB (step {step.number} of {step.count})…")
        } else {
            std.say("{step.label} (step {step.number} of {step.count})…")
        }
    } else {
        std.say("{step.label} (step {step.number} of {step.count})…")
    }
}
