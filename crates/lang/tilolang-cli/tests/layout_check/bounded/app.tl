app so.tilo.lang.layoutbounded "Bounded" (icon: .layout_grid, category: .utility)

type Sample {
    name: Text
}

root Sample

default view samples for Sample

/// A desk as the shell lays one out: a bar over a middle longer than the window, and a dock
/// over its foot holding more items than the window is wide. The desk is the window's height:
/// the middle scrolls between the bar and the dock, and the dock's body is the window's width
/// at most, its items scrolling along it.
view samples(s: Sample) {
    layers {
        stack(gap: .tight, fill) {
            ui.Text(s.name, role: .heading)
            scroll(fill) {
                ui.Text("Line 1 of what the middle shows")
                ui.Text("Line 2 of what the middle shows")
                ui.Text("Line 3 of what the middle shows")
                ui.Text("Line 4 of what the middle shows")
                ui.Text("Line 5 of what the middle shows")
                ui.Text("Line 6 of what the middle shows")
                ui.Text("Line 7 of what the middle shows")
                ui.Text("Line 8 of what the middle shows")
                ui.Text("Line 9 of what the middle shows")
                ui.Text("Line 10 of what the middle shows")
                ui.Text("Line 11 of what the middle shows")
                ui.Text("Line 12 of what the middle shows")
                ui.Text("Line 13 of what the middle shows")
                ui.Text("Line 14 of what the middle shows")
                ui.Text("Line 15 of what the middle shows")
                ui.Text("Line 16 of what the middle shows")
                ui.Text("Line 17 of what the middle shows")
                ui.Text("Line 18 of what the middle shows")
                ui.Text("Line 19 of what the middle shows")
                ui.Text("Line 20 of what the middle shows")
                ui.Text("Line 21 of what the middle shows")
                ui.Text("Line 22 of what the middle shows")
                ui.Text("Line 23 of what the middle shows")
                ui.Text("Line 24 of what the middle shows")
                ui.Text("Line 25 of what the middle shows")
                ui.Text("Line 26 of what the middle shows")
                ui.Text("Line 27 of what the middle shows")
                ui.Text("Line 28 of what the middle shows")
                ui.Text("Line 29 of what the middle shows")
                ui.Text("Line 30 of what the middle shows")
                ui.Text("Line 31 of what the middle shows")
                ui.Text("Line 32 of what the middle shows")
                ui.Text("Line 33 of what the middle shows")
                ui.Text("Line 34 of what the middle shows")
                ui.Text("Line 35 of what the middle shows")
                ui.Text("Line 36 of what the middle shows")
                ui.Text("Line 37 of what the middle shows")
                ui.Text("Line 38 of what the middle shows")
                ui.Text("Line 39 of what the middle shows")
                ui.Text("Line 40 of what the middle shows")
            }
        }
        row(at: .bottom, fill, align: .center, distribute: .center) {
            ui.Chrome(name: "Dock", shape: .rounded, edge: .bottom) {
                scroll(axis: .horizontal) {
                    row {
                        ui.Button("Item 1", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 2", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 3", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 4", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 5", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 6", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 7", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 8", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 9", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 10", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 11", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 12", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 13", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 14", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 15", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 16", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 17", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 18", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 19", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 20", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 21", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 22", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 23", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                        ui.Button("Item 24", icon: .file_text, icon_only: true, role: .plain) {
                            on press: rename(s)
                        }
                    }
                }
            }
        }
    }
}

/// Rename the sample: what the dock's items do.
action rename(s: Sample) {
    s.name = "Renamed"
}

/// A sample with a name.
scenario "a sample" {
    let s = root Sample
    s.name = "Sample"
}
