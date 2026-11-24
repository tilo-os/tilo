/// The optional components that let this computer play media and run Windows programs: the media
/// codecs and the Windows compatibility layer, each read from its resource and changed by its own
/// actions.
view compatibility_pane() {
    let codecs = device.codec_pack
    let windows = device.windows_compat
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
    }
    ui.Section("Windows support") {
        if not windows.available {
            ui.Text("This system does not offer Windows support.", role: .caption, secondary: true)
        }
        ui.ListRow("Windows programs", secondary: windows.summary, icon: .puzzle) {
            trailing: {
                row(gap: .tight, align: .center, wrap: true) {
                    if windows.state == .needs-install or windows.state == .failed {
                        ui.Button(
                            if windows.state == .failed { "Try again" } else { "Install" },
                            size: .small,
                            role: .primary,
                        ) {
                            on press: device.install(device.windows_compat)
                        }
                    }
                    if windows.state == .ready or windows.state == .waiting-for-network {
                        ui.Button("Remove", size: .small, role: .destructive) {
                            on press: device.remove(device.windows_compat)
                        }
                    }
                }
            }
        }
        if let size = windows.size {
            ui.Text("Installing it downloads {size}.", role: .caption, secondary: true)
        }
    }
    if let problem = codecs.problem {
        ui.StatusLine(problem, status: .error)
    }
    if let problem = windows.problem {
        ui.StatusLine(problem, status: .error)
    }
}
