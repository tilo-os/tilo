/// One photo of the library: what it shows, where, and its picture, carried in its address.
type Photo {
    title: Text
    place: Text
    picture: Text
    /// Taken standing: taller than it is wide.
    tall: Bool = false
}

/// A page shown in a picture, from the first to the last.
action picture_from(first: Int) {
}

/// The media page: each kind in a section of its own.
view media(g: Gallery) {
    state last: Text = ""
    state ask: Bool = false
    state head: Float = 3.0
    state span: Float = 20.0
    state scroll: Float = 0.0
    state curve: List<Float> = [0.0, 0.0, 0.25, 0.15, 0.75, 0.85, 1.0, 1.0]
    ui.Text(
        "Pictures, players, a library, codes and surfaces, each as an app places it.",
        role: .caption,
        secondary: true,
    )
    stack(max: 72rem, gap: .section) {
        part("Image", "A picture at its own shape, as wide as the column lets it be.") {
            grid(columns: 2) {
                ui.Image(photo(), alt: "Hills at dawn, the sun low over the last ridge")
            }
        }
        part(
            "Image fit",
            "How a picture fills a box of another shape: whole, covering it, or stretched.",
        ) {
            row(gap: .related) {
                ui.Image(photo(), alt: "Hills at dawn, whole", fit: .contain, aspect: 1.0, fill)
                ui.Image(photo(), alt: "Hills at dawn, covering", fit: .cover, aspect: 1.0, fill)
                ui.Image(photo(), alt: "Hills at dawn, stretched", fit: .fill, aspect: 1.0, fill)
            }
        }
        part(
            "Image viewer",
            "A picture to look at closely: scroll to zoom at the pointer, drag to pan.",
        ) {
            grid(columns: 2) {
                ui.ImageViewer(photo(), alt: "Hills at dawn, to zoom", aspect: 1.5)
            }
        }
        part("Crop editor", "The part of a picture to keep, by its corners.") {
            grid(columns: 2) {
                ui.ImageCropEditor(
                    photo(),
                    left: 0.1,
                    top: 0.1,
                    right: 0.9,
                    bottom: 0.9,
                    aspect: 1.5,
                )
            }
        }
        part(
            "Curve editor",
            "A tone curve shaped by hand: a press adds a point, a drag bends it, and letting go outside takes it off.",
        ) {
            ui.CurveEditor(points: curve, label: "A tone curve") {
                on change(moved: List<Float>): set(curve, moved)
            }
        }
        part(
            "Paint surface",
            "A surface a person paints on: the artwork beneath it, and the brush that lays each stroke.",
        ) {
            layers(aspect: 4.0) {
                ui.PaintSurface(
                    picture: photo(),
                    width: 1200.0,
                    height: 800.0,
                    size: 12.0,
                    zoom: .some(1.0),
                    label: "A painting over a photograph",
                ) {
                    on stroke_start(p: List<Float>): mark_paint(last, p)
                    on stroke_end(p: List<Float>): mark_paint(last, p)
                }
            }
        }
        part(
            "Slideshow",
            "A picture shown large for its time, then the next asked for: a presentation a view opens.",
        ) {
            ui.Slideshow(
                image: photo(),
                alt: "Hills at dawn, in a presentation",
                seconds: 30.0,
                aspect: 1.8,
            ) {
                on next: set(last, "Next picture")
            }
        }
        part(
            "Call tile",
            "A person in a call, at their camera or the screen they share: the ring says they are talking now, and the mark says their microphone is off.",
        ) {
            row(gap: .related, align: .stretch) {
                // The person reading, talking at their camera, and someone a bridge carries in,
                // sharing a screen with their microphone off.
                layers(fill, aspect: 16.0 / 9.0) {
                    ui.CallTile(who: invoker, source: .camera, speaking: true)
                }
                layers(fill, aspect: 16.0 / 9.0) {
                    ui.CallTile(name: "Sam Okafor", source: .screen, muted: true)
                }
            }
        }
        part(
            "Media grid",
            "A library in equal columns: covers, albums, places, with a line or two under each.",
        ) {
            ui.MediaGrid(size: .small) {
                for p in g.media_photos {
                    ui.MediaCard(p, title: p.title, picture: p.picture, meta: [p.place]) {
                        on activate: note_photo(last, p)
                    }
                }
                ui.MediaCard(g, title: "Imports", icon: .folder, meta: ["Today", "3 files"]) {
                    on activate: set(last, "Opened Imports")
                }
            }
        }
        part("Justified photos", "Photos at their own shape, in rows that span the width.") {
            ui.MediaGrid(justified: true) {
                for p in g.media_photos {
                    ui.MediaCard(
                        p,
                        picture: p.picture,
                        aspect: if p.tall { .portrait } else { .wide },
                    ) {
                        on activate: note_photo(last, p)
                    }
                }
            }
        }
        part(
            "A card on its own",
            "A card the view gives outside a grid: one thing, a grid column wide, where the view puts it.",
        ) {
            ui.MediaCard(g, title: "Imports", icon: .folder, meta: ["Today", "3 files"]) {
                on activate: set(last, "Opened Imports")
            }
        }
        part(
            "Media transport and video",
            "What plays, where it is, and the controls a person reaches for.",
        ) {
            ui.MediaTransport(
                title: "Low Light",
                subtitle: "Hanna Berg",
                art: photo(),
                playing: true,
                position: g.media_at,
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
                on act_seek(place: Float): seek_to(g, place)
            }
            ui.VideoPlayer(
                title: "Hills at dawn, a timelapse",
                poster: photo(),
                playing: false,
                duration: 60.0,
            ) {
                on seek(place: Float): seek_to(g, place)
            }
        }
        part("Code and drop zone", "A code a camera reads, and a place a person drops files on.") {
            row(gap: .related) {
                ui.QrCode("tilo://invite/gallery", alt: "Pairing code")
                ui.QrCode("tilo://invite/gallery", secret: true)
            }
            ui.DropZone("Drop files to import", message: "Any file up to 50 MB", icon: .upload) {
                on drop(files: List<File>): mark_files(last, files)
            }
            ui.DropZone("Drop a picture", action: "Choose") {
                on choose: set(last, "Choose")
            }
        }
        part(
            "Page preview and rail",
            "A document's pages as pictures: one, and the rail beside a viewer.",
        ) {
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
        part("Code editor and text line", "Source with its colours, and one line of rich text.") {
            ui.CodeEditor(
                bind: g.media_source,
                language: .tilolang,
                min_lines: 4,
                placeholder: "Write a view",
            )
            ui.RichTextLine(bind: g.media_source, placeholder: "A line of text", role: .body) {
                on enter(at: Int): mark_page(last, at)
            }
        }
        part("Surfaces", "What stands over a window: a dialog, a popover, a floating panel.") {
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
        part(
            "Emoji pane",
            "The picker every app shares, placed as a card a view opens: its search, groups, tones and recents.",
        ) {
            layers(aspect: 2.4) {
                ui.EmojiPane(open: true) {
                    on pick(e: Text): set(last, e)
                    on dismiss: set(last, "Dismissed")
                }
            }
        }
        part(
            "Track timeline",
            "A ruler over tracks: spans a person drags and trims, markers, and the playhead.",
        ) {
            ui.TrackTimeline(
                label: "A cut",
                span: span,
                scroll: scroll,
                playhead: head,
                length: 24.0,
                step: 0.04,
            ) {
                on scrub(t: Float): set(head, t)
                on seek(t: Float): set(head, t)
                on zoomed(s: Float): set(span, s)
                on scrolled(s: Float): set(scroll, s)
                ui.TimelineTrack(label: "V1") {
                    ui.TimelineSpan(
                        label: "Harbour",
                        start: 0.0,
                        length: 8.0,
                        selected: true,
                        keys: [2.0, 5.5],
                    )
                    ui.TimelineSpan(
                        label: "Quay",
                        start: 8.0,
                        length: 6.0,
                        tone: .info,
                        lead_in: 1.0,
                    )
                }
                ui.TimelineTrack(label: "A1") {
                    ui.TimelineSpan(
                        label: "Gulls",
                        start: 2.0,
                        length: 12.0,
                        tone: .success,
                        peaks: [0.2, 0.5, 0.9, 0.6, 0.3, 0.7, 1.0, 0.4, 0.2, 0.6, 0.8, 0.3],
                    )
                }
                markers: {
                    ui.TimelineMarker(label: "Beat", at: 6.0, tone: .warning)
                }
            }
        }
        part("Web view", "A web page shown live, in the window.") {
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

/// Note a stroke on the paint surface, in the page's answer.
action mark_paint(last: Text, p: List<Float>) {
    last = "Painter at {p.len()} places"
}

/// Note the files dropped, in the window's last answer.
action mark_files(last: Text, files: List<File>) {
    last = "Dropped {files.len()}"
}

/// Move the song to a place in it, as a share of its length.
action seek_to(g: Gallery, place: Float) {
    g.media_at = place * 180.0
}

/// The picture the gallery carries in its own source, so a page shows an image with no file to
/// fetch and no app behind it.
fn photo() -> Text {
    "data:image/jpeg;base64,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MM74Lo0wzx3wsgiRd10aP4I4kIok8E13aH/zzzzznvzcYY7zw5/+4zzzzxzjTPvjXPLvOsEAAAAFITfzzzwT3rnHsMAAAAgAAAAAAAAAAAAAHPvIMIJIMNLMMfEPMEAYAAAAAAAAAAAAAAAoYIIIIIIIIIIIIOCo4AAAAYEQmMBMnyx8oIIIIIIIIIIIIIOYV5CIYI4IIoLzzyzsM88I4Yc4oIIIIIIIIazzzzzzzzzzzzzzzzzzzyysIIYoY4/zzzzzzzzzzzzzzzzzzzzzzyyxzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz/xAAfEQEBAQACAwADAQAAAAAAAAARAAEQIDFAQTBRYHH/2gAIAQMBAT8QIiIiIjgREREREREcCIiIiIiIiIiIiIiI4ERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERaZ5s1+xERERETMzMzMzM25y3U5ylK0LdZ5t/GzMzMzMwjDgzCEOO63vueJs3yyerspT33EYwjHiY27t17Kz97Nzev8AmzSzT7zuflu7vqZuPF9WVm+bdzNm8szL6D+fNZZ+9m5vVyEYznOPL6Gb+xtu63l6vVmZmZmfxPrPVmf4X//EACIRAQEBAAMAAgICAwAAAAAAABEAARAgQTBAMWEhUVBwcf/aAAgBAgEBPxBmZmZmZmZmegegeRmZmZmZmZmZmeR+IAPIzMzMzMz9YAAAMzMzMzweD9AAAPxgA9wHkZnsB5GZmZmegegZmZmZmZnkZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZs/ngZmZmZn5kzHu9X/f7LxW5uXtzm3luXnbN/OPcfLcvI2zTy/Tfrs38s9PgTN8FJ+ci8szM/HO5m26+X67d78JnfN7Kc5ylKVmPbMO2423+lubnX/q3K3WecFm/bMZ9Tc7KUOxkOQciX+AMth1I2XAQh9RvIM/0D//xAArEAEAAgAEBAcAAgMBAAAAAAABABEhMVFhEEBxgSAwQVCR8PFwobHB0eH/2gAIAQEAAT8Q/goAAAAAAAAAAAAAAAAAAAAAACpXKAAAOidE6OTAAAqVKlSpUqVKlSpUqVKlSuCpXEr2EAAAAArwCv4JAAAAAAAAAAAAAAAAAAAAAACpXEryABz4AAAACuVAAADmwAAAA88AAAAAecAAHvIAAAAAAAAAPCA5AAAHgPsQAAAAAHNAAAADxDwHlAAAA4g8kAEVDgHIgAAeIqVxHngAAAAAgcBxBByAAAMPEJKlewgAAAABBwDyQBXkAHgPOgAAAAPKAAgIEOICBKleAVwVKleAVK4GHnwAAAABBxFQIRUqVK4lSpUqVK4lRJXErxArnAAAABXECVwBKlSpUqVKlSpUqVKlSpUqJK4KjFcFcR5gAAACvAKhFSpUqVKlSpUqadaxEyWYu6RUgpJUqVKlcFSpUYqV4BUrlAAAFeEK4DrM3YfYloETGnfThD/wSfhE/KJ+UT8In4xPpUNJj6tT1gRnK0KOLoQEaNMqVKlSpUqVKlSpXBXEqVKlSpUqVKlSpUqVKlSuJXgFRWnYQgnYZx22Wq+Y4txnYXKYBXiimu0KChY3iSuLursmZAIaypUSOhh6rHSrNG59Kn0qBhMXWAAgjkjKlSpUqVywAAAqUCoA9WIIv6Eti9vkgRINiNVGypd5P8o6LKYtZz8Z/wCy1bCEQWbo1FOa+Gx6sSEwdY+sT8JhBd0GE+/4iTAO2MUAAuiq4KogeqxihvTFMp0lj8Jn4TBXslNRRg3bGIBUL6Iz8ZLbAM6bjYVPRj8ZPtGI11YT8Y/7Pxj/ALPxiUmy+l1wwddSEevqD5xbRehnFy7rCqX2wglKN2CUI2ZlK74wtAg/POmMosq6kUpTsy/PswjeFGMVtTd8kUyZa8qLYBuz16ppBGflZGOjCqH3xh3DTaOVat4IliJzAFoDeZNVG2CH3hPvCJYp1HBPrc+tx+WdcYooBuE+sn7k/dn7sGofefeE+8JnK7YRVbVeRXtiMwNOpDbBNnydw+YI5J5RFkEzgd0atl9mYthlBV25Mib08FxShO7EWv8AGbj8yxxbOmpQUxd1ihTGLzVS/tToz+IWFbq1BhBHJGbh8zcPmZE3pGbKt2MWy+VZrLNfYBKEdZ9ZM5XbCZmOr5NtWbj8wBsXzP1IJQ+8JVCG5hArD3ljk8RLA6swe7bS9hBvM2fbCKLVWW6xKx94XKZoZ0Znl7gpaHRn60/Slr6+TmERf9hNDOjBHJH3xCxRlfmN5l/OTIgOjydhmzMj2xiK0KfS59LiKsEyI98Jd5cct4g2mRX+0NtCc85hY0ZrJBEsbPJuaSvSZMr0IMxF3n0uLrDF1mx8IBS+0UtHq+PMg6MA422TwIWkdpl/CgC0JtzzPoaRtAtr6QSwej4MyC6EX/YzNDpyzdvqTC3oPPijYpMmffGKFf4wKker4xLA6wDBXoTZ+ES003SIUHvBlDPWWayzWJUJ3YpSe2MG4W9CbPwmz8PIwC9hlfbbRgC0JtxuWayzXn7A0RHA0asQtK7+K3Vlur54o2KT9SfqRS0XdlurLdWZkHRmSPvjBmNPUlLBXZn0uODDF14Z95F2/wCyCUPvjCVQEr7baPJfIh6R20r7VlLZowul2HzsBvYJhuWg9uwC9hmFrbeCJYj4EFqBES/7J95BuFvQgzEXeKyTrM0a09yQtIxazuMzR9sIoU1dZbr/ACb/AP/Z"
}

/// Note the picture a card opened, in the page's answer.
action note_photo(last: Text, p: Photo) {
    last = "Opened {p.title}"
}

/// The media group, with a library of photos the gallery carries itself, each in its address.
scenario "media" {
    root Gallery
}
