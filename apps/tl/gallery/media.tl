/// What the gallery holds of media and surfaces: pictures, a playing band, a video, a library
/// of cards, a code a scanner reads, a drop zone, a page preview and rail, a code editor, a
/// line of text, the window's own surfaces (a modal, a popover, a floating panel) and a page
/// shown live.
type Media {
    title: Text
    /// A picture's address: its bytes, carried in the address itself.
    photo: Text
    /// How far into the video the player is, in seconds.
    at: Float
    /// The code a scanner reads.
    code: Text
    /// The source being written.
    source: Text
}

default view media for Media

/// A page shown in a picture, from the first to the last.
action picture_from(first: Int) {
}

/// The media window: each kind in a section of its own page.
view media(m: Media) {
    state last: Text = ""
    state ask: Bool = false
    std.page(m) {
        ui.Section("Pictures") {
            ui.Image(m.photo, alt: "A blue tile")
            ui.Image(m.photo, alt: "A blue tile, cropped", fit: .cover)
            ui.ImageViewer(m.photo, alt: "The same tile, to zoom", aspect: 2.0)
            ui.ImageCropEditor(m.photo, left: 0.1, top: 0.1, right: 0.9, bottom: 0.9, aspect: 2.0)
        }
        ui.Section("Cards") {
            ui.MediaGrid(size: .small) {
                ui.MediaCard(m, title: "Blue", picture: m.photo)
                ui.MediaCard(m, title: "Folder", icon: .folder)
                ui.MediaCard(m, title: "Notes", picture: m.photo, meta: ["Today", "3 files"])
            }
            ui.MediaGrid(justified: true) {
                ui.MediaCard(m, title: "Hills", picture: m.photo, aspect: .wide)
                ui.MediaCard(m, title: "Portrait", picture: m.photo, aspect: .portrait)
            }
        }
        ui.Section("Playing") {
            ui.MediaTransport(
                title: "A quiet song",
                subtitle: "Someone",
                art: m.photo,
                playing: true,
                position: m.at,
                duration: 180.0,
                volume: 0.6,
                shuffle: false,
                repeat: .all,
                liked: true,
                queue: false,
            ) {
                on act_play_pause: set(last, "Play")
                on act_previous: set(last, "Previous")
                on act_next: set(last, "Next")
                on act_mute: set(last, "Mute")
                on act_seek(place: Float): seek_to(m, place)
            }
            ui.VideoPlayer(title: "A film", poster: m.photo, playing: false, duration: 60.0) {
                on seek(place: Float): seek_to(m, place)
            }
        }
        ui.Section("Codes and drops") {
            row(gap: .related) {
                ui.QrCode(m.code, alt: "Pairing code")
                ui.QrCode(m.code, secret: true)
            }
            ui.DropZone("Drop files to import", message: "Any file up to 50 MB", icon: .upload) {
                on drop(files: List<Text>): mark_files(last, files)
            }
            ui.DropZone("Drop a picture", action: "Choose") {
                on choose: set(last, "Choose")
            }
        }
        ui.Section("Pages") {
            row(gap: .related) {
                ui.PagePreview("Report.pdf", page: 2, page_count: 5)
                ui.PagePreview("Scan.pdf", page: 1, page_count: 1)
            }
            ui.PageSidebar(aspect: 1.414, count: 5, current: 1, header: "Pages", add: "Import…") {
                on select(index: Int): mark_page(last, index)
                on reorder(index: Int, at: Int): mark_move(last, index, at)
                on add_page: set(last, "Add")
            }
        }
        ui.Section("Text") {
            ui.CodeEditor(
                bind: m.source,
                language: .tilolang,
                min_lines: 4,
                placeholder: "Write a view",
            )
            ui.RichTextLine(bind: m.source, placeholder: "A line of text", role: .body) {
                on enter(at: Int): mark_page(last, at)
            }
        }
        ui.Section("Surfaces") {
            row(gap: .related, wrap: true) {
                ui.Button("Ask", icon: .circle_check) {
                    on press: set(ask, true)
                }
                ui.PopoverButton("Options", icon: .settings, side: .below, trailing: true) {
                    ui.ListRow("First choice")
                    ui.ListRow("Second choice")
                }
                ui.Button("Show the page", icon: .eye) {
                    on press: set(ask, true)
                }
            }
            ui.FloatingPanel(width: 280.0) {
                ui.Text("A floating panel holds a canvas's own inspector.", role: .caption)
            }
            ui.Popover(label: "A popover beside nothing in particular", open: false, side: .below) {
                ui.ListRow("First choice")
                ui.ListRow("Second choice")
            }
            if ask {
                ui.ModalDialog(
                    title: "Delete 3 notes?",
                    secondary: "Cancel",
                    primary: "Delete",
                    destructive: true,
                    open: true,
                ) {
                    on act_primary: set(ask, false)
                    on act_secondary: set(ask, false)
                    on dismiss: set(ask, false)
                    ui.Text("They are kept in History for 30 days.", role: .caption)
                }
            }
        }
        ui.Section("The web") {
            ui.WebView(title: "A page", aspect: 1.6)
        }
    }
}

/// Note the page the rail chose, in the window's last answer.
action mark_page(last: Text, index: Int) {
    last = "{index}"
}

/// Note where a page moved to, in the window's last answer.
action mark_move(last: Text, index: Int, at: Int) {
    last = "{index} to {at}"
}

/// Note the files dropped, in the window's last answer.
action mark_files(last: Text, files: List<Text>) {
    last = "Dropped {files.len()}"
}

/// Move the song to a place in it, as a share of its length.
action seek_to(m: Media, place: Float) {
    m.at = place * 180.0
}

/// The media group: its picture the gallery carries itself, in the address.
scenario "media" {
    let g = root Gallery
    g.media.insert(
        Media(
            title: "Media",
            photo: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAEklEQVR4nGOI6jnxHx9mGBkKAA3Qq0EzFRlyAAAAAElFTkSuQmCC",
            at: 12.0,
            code: "tilo://invite/gallery",
            source: "text(\"The blue tile\")",
        ),
    )
}
