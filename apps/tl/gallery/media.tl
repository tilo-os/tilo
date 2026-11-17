/// What the gallery holds of media and surfaces: pictures, a playing band, a video, a library
/// of cards, a code a scanner reads, a drop zone, a page preview and rail, a code editor, a
/// line of text, the window's own surfaces (a modal, a popover, a floating panel) and a page
/// shown live.
type Media {
    title: Text
    /// A picture's address: its bytes, carried in the address itself.
    photo: Text
    /// The photos of the library, in the order they were taken.
    photos: Collection<Photo>
    /// How far into the video the player is, in seconds.
    at: Float
    /// The code a scanner reads.
    code: Text
    /// The source being written.
    source: Text
}

/// One photo of the library: what it shows, where, and its picture, carried in its address.
type Photo {
    title: Text
    place: Text
    picture: Text
    /// Taken standing: taller than it is wide.
    tall: Bool = false
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
        part("Image", "A picture at its own shape, as wide as the column lets it be.") {
            ui.Image(m.photo, alt: "Hills at dawn, the sun low over the last ridge")
        }
        part(
            "Image fit",
            "How a picture fills a box of another shape: whole, covering it, or stretched.",
        ) {
            row(gap: .related) {
                ui.Image(m.photo, alt: "Hills at dawn, whole", fit: .contain, aspect: 1.0, fill)
                ui.Image(m.photo, alt: "Hills at dawn, covering", fit: .cover, aspect: 1.0, fill)
                ui.Image(m.photo, alt: "Hills at dawn, stretched", fit: .fill, aspect: 1.0, fill)
            }
        }
        part(
            "Image viewer",
            "A picture to look at closely: scroll to zoom at the pointer, drag to pan.",
        ) {
            ui.ImageViewer(m.photo, alt: "Hills at dawn, to zoom", aspect: 1.5)
        }
        part("Crop editor", "The part of a picture to keep, by its corners.") {
            ui.ImageCropEditor(m.photo, left: 0.1, top: 0.1, right: 0.9, bottom: 0.9, aspect: 1.5)
        }
        part(
            "Media grid",
            "A library in equal columns: covers, albums, places, with a line or two under each.",
        ) {
            ui.MediaGrid(size: .small) {
                for p in m.photos {
                    ui.MediaCard(p, title: p.title, picture: p.picture, meta: [p.place])
                }
                ui.MediaCard(m, title: "Imports", icon: .folder, meta: ["Today", "3 files"])
            }
        }
        part("Justified photos", "Photos at their own shape, in rows that span the width.") {
            ui.MediaGrid(justified: true) {
                for p in m.photos {
                    ui.MediaCard(
                        p,
                        picture: p.picture,
                        aspect: if p.tall { .portrait } else { .wide },
                    )
                }
            }
        }
        part(
            "Media transport and video",
            "What plays, where it is, and the controls a person reaches for.",
        ) {
            ui.MediaTransport(
                title: "Low Light",
                subtitle: "Hanna Berg",
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
            ui.VideoPlayer(
                title: "Hills at dawn, a timelapse",
                poster: m.photo,
                playing: false,
                duration: 60.0,
            ) {
                on seek(place: Float): seek_to(m, place)
            }
        }
        part("Code and drop zone", "A code a camera reads, and a place a person drops files on.") {
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
                bind: m.source,
                language: .tilolang,
                min_lines: 4,
                placeholder: "Write a view",
            )
            ui.RichTextLine(bind: m.source, placeholder: "A line of text", role: .body) {
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

/// Note the files dropped, in the window's last answer.
action mark_files(last: Text, files: List<Text>) {
    last = "Dropped {files.len()}"
}

/// Move the song to a place in it, as a share of its length.
action seek_to(m: Media, place: Float) {
    m.at = place * 180.0
}

/// The media group: a library of photos the gallery carries itself, each in its address.
scenario "media" {
    let g = root Gallery
    let m = g.media.insert(
        Media(
            title: "Media",
            photo: "data:image/jpeg;base64,/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAkGBwgHBgkIBwgKCgkLDRYPDQwMDRsUFRAWIB0iIiAdHx8kKDQsJCYxJx8fLT0tMTU3Ojo6Iys/RD84QzQ5Ojf/2wBDAQoKCg0MDRoPDxo3JR8lNzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzc3Nzf/wgARCAFAAeADASIAAhEBAxEB/8QAGgABAQEBAQEBAAAAAAAAAAAAAAIBBAMFBv/EABgBAQEBAQEAAAAAAAAAAAAAAAABAgME/9oADAMBAAIQAxAAAAHhU9fGVCVCVCVCVCVCVCVCVCVCVCVaSoSoStEqErELLCxCxCxCxCxCiQsQpU5YheEqFKZ1KhKhKhKhKhKhKhKhKhK0RtCVaQssq0jaEq2I2hKtIWIWIWIXhCxC8qFiF4QsQokqFKZ3KhKhKhKhKhKhKhKhKhKtI2hKhO0iViVaRtCVoja0hZYWIWIz0xIWPNaoXhC8JyxClSoapjUqEqEqEqEqEqEqEqGKEqEq0lQnaErRKyyrSVaQtErELELwhYjPTKheELwheJOWqFCFjVM6lQlQlQlQlQlQlQlQlWkqEq2JUWdoTtCdpE7QnaErEbSJUJyxC1eaxGWIyx5rVGWIUIXiatncLELELELELELELEbQlQlWkqE7Qnd2J2hO0J2hO0idoSrSFiF4TliMsRlqjLEZeE5eE5aoUNUzqVCVCVCVCVCVCVCVaRtCVInaEq0lWk7uk7uxO0J2hO0JUJWIUJy8JyxGXhOXhC8qF4QvCcsFs6hYhYhYhYhYhYhYlQlQnd0lQnaE7QnaRihihiiYoTujFCcoSoQoQosZeE5eE5eVOWIUNUzuVCVCVCVCVCVCVCVaSoSoTu7JKhihihiiYoYoY3SVWnm64s5l5NSrCVYSoQosKEKEKE5Y1SWVCVCVCVCVCVCVCVCVCVDFCXnzanb5cGdM908au9wDvcA+g+eT6d/JS/f7vyva5/efO7riOL6XDz6eSsx0zKkZMV6550U1LKhCiyoarEx5eOp1uFXc4vSOl5ekaZGsivR4ZXQ5x0OeDrcHjqd3J4twNgAAABKU5/OzscKu7eKo+pXzaze+eXZfXzxQUBt+aPeuZHVXGju9fmJernluBQAAG4AB5eVnU5Cdbko6XPq+7nlOpySdriHa49OtyYdfnyrPTzNQAAABuC78UvX6cGy9zx9c3QoAAB8Pd8/tvke8v0HN6516M0ElZ4eNnv4Y1kKAAAM0AAAAAAAAAAAAAbg9/biZve4bl63HJ2zxrOEdOQAGsG7IuvIvRfIjucWr2ZySdXn4rLnCKke3ryF7nH6zXuipdAAAAAAAAAAAAAABwubdc+h5ehoAAAAAAAAAAAG4L9PAvXfDcvW8vSa0AAABMHq8cs93hJ0uSU6p5ie7wV01yI7nFa/NFw3B7+3FR2PP0UAAAAAAAAAAAABUjo9eJNdzkHXHLh7+UrAQYa88PV5UWzQYajD0eQ5ggAD08x1XxadrjHY4sO5w6drkpelz6e7wk6XLKdjkpel5+gAAAAAAAAzz5z18sIAAAAAAAAAAAAAAAAAAAuB1enD6HU8/RQBhqcLecns5/M6fDzIAAAAAAAAAAAAAAAAAAAAAAA3BWwKkAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAP/xAAmEAACAgIBBQACAgMAAAAAAAAAAQISERMDECAwQFAEMRQhInCg/9oACAEBAAEFAv8AgjUWzUPjfzf0bIm1G43RN0TdE3RN0TdE2wLxIRs+s45XyZTiiXK35VKUSP5U0Q/LgxSUuk1/l1/ovE2wNsC0fflyxRLkcvSTwLn5EfyJm+Q+aY5yffeRtkbpG5m4XMhckX0/Q+WKN5ukbpC5mLmRtgWiJplkWibID5om6Juibom6JvHzSG2/M5JD5CzMsyyzNjNhdGV6D5pDbfwHNDm348lmbBTT9pzRsZdl2XZsZsNhsLsuzLMsyyzLsuyz9JPAuT1c4HyGc/GzgXIKSfblF10Umhcs0L8gXNE2R7HJIfIZz8vLLsszPkyyzLsuWXa5IuWfWzLikvpZZl+JNoUxSX3rMuKSfqWRdGw2F0WXW7Lin76k0Ka8WUXRc2Fy7LPwZLvtU/fTaFPtckhzG2/WTwKWfgWZZmfBdF0XRZGV1yWRdF0XXgUmhSXxW8Dn7GWZfZksy7Lmw2F2WZllmXYpL0pT+WpNClnzOSQ5N/OUmhTXdlFkXRcuNt/TyyzMv/Z3/8QAHxEBAQABBAMBAQAAAAAAAAAAEQABEiAwQAIQcCFQ/9oACAEDAQE/AeE5jsn1TVjrNqmZmbyxnMXhniZmfbMzO9mb83s8DMzMzMzvZ3a7Xice3qszO5mbVPtn+AzvZmZmemzM/Af/xAAgEQEBAAICAwEAAwAAAAAAAAAAEQESECACMEATUGBw/9oACAECAQE/AffVXiqvFX46vFVVVVVeKvsqqqqqqqqqqqqqr6rxeKqqqqqqqqq8VVVVVVXiqqqvFXpVVV4q+mqvFVfbV/qUz82PHOX5tMNMNMNMPzwx4zjyxzMpntrlplplM8TLXLXLXL88seOMd41as+LXHXXDXDTDGMY7xqiI1atWrXvGvbRrlOYnyRq1a94iJzE/gZ6IiIifHET/AAL/xAAjEAACAQMFAAIDAAAAAAAAAAAAMSEBIEEQMEBQYCIyUXCg/9oACAEBAAY/Av4VVpkyZMmRj7CI3YrWhMnygit7GMfP/PDg+xgxo72MwIRNB2IwYJoTQY6EVGOg9MmTIhaTXfix8WfHoVjGOx8OeNHUTsxUZNNHbHXPdfjp8uhdohcCegm6PeRyGPYQtXxY9g+4f7Q//8QAKRABAQEAAQMDBAICAwEAAAAAABEBMRBBYSAhQDBQUXGR8KHxcIHR4f/aAAgBAQABPyGIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiInWIiIiIiIiIiIiIiIiIiIiIiIiIifCnWJ0iIiIiIiIiIiIiIiIiIiIidIidIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiJ1idIiIiIiIiIiIiIiIiIiIiIiIiInSIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiJ0iIiIiIiIiIiIiInWIiIiIiIiIiIiIiIiIiIiJ1iIiIiIiIiIiIiIiInoiIiIiIiIiIiIiIiIiIiIiIiIiIiIidIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiJ1iJ6YidJ6IidYiIiIiIiIiIiIiIiIiIiIiIiJ1npieuJ6InSIiJ0iIiIiIiIiIiIiIiIiJ0idZ8WdJ0nSIiIiIiIiIiIiIiIiJ0iJ0n0+Nxn56zce7cnPqnonWInWIiIiIiIiIiIidIiJ0jdzluY3LvXj1/WvBrwHgPAeI8R/RjNc4v1DMzM9uudM5T6s6RPhROkROndbvh+CG7u87fqf4zNdv/ALuBbtlXPf1vSGvRu4y7uY3HLL+jH9GLdv8AK5vG+uIiIiIiIiIiIiInTfblwPu8PFZ4+Fpq63GfM3/37t05yniNG+25jWu7X01mHf8Ay/qx4jL++I/vXdD/ANIjdzF3Y73f03lP83iPEdka7sYH+6cAa3HZuf5f7rpMPF14jxHiN7Wn9a0cTHKm/W5DX4Dd+7dOd1mHG6zMzuTWY74zXuzTjc+Bu4mOQbvz93M51i493cp+vp5rON1mfdmu+PBfJ3czli4929iY8rysz8vBnWt12xvjeV5jzHmMz7vK8rd/hNOGsjNzeN+jc+nuOWv/ALW65b9mzXDX4jgN9O4c7jyrrkjH5l+2nZ39NfNz9s1yyzc3jVxcchr8BuuW/a8w43Xlbubred36NXV38vMZj3Z3vdme+M07+jdzOdYvLd9sbp3Xd6Zh3ZvvjV3+45u5xrza82/S4TX4TV3++5u4zLuz88dw+JuHd+7qGeZmnfr4sZvvmMd59mbm8fO7g0c+30ty51uDc9s6l+pu/du7vO+vNZxrPy+jN3OHa/kzbx87hNY+DNzeN9HcH4TlN+NrwZ+G/YMw7vO3W87693M51uL9zPMzfup3zruM53G492j9z930POY1eFzePsmGLvTbu7z8fzHmN3d53rms41mPdgzffOoeLHneYzHuzve7V4+Fhnt7tbu7z9rDPw363nNfqPt3nMaPHpuY8R52jc9sbvtjlN+5Zu5wzLubr3ebf+Tv/9oADAMBAAIAAwAAABAIIIIIIIIIY1zkF3M9PTzzziI/O1HXz3333333333OdvYiRqYtPMMfoTATK8swwwwwwwwxziRyZMm1esAABePkWdqwj777777778N/s2XdZgj4IIbpQSadk3M000000013kHsaSSaaRW89+GhorhorfLLLLLLLKIboSS6aSSaZnCVJphhZZAj33333333iA6Lpii6aSDzCxRh5aSSZLPPPPPPPMd+tWWXZZZVG0F2Gmp5ZVG3zzzzzzzzgAyt9ZZddRjcbASeuqqu9MMMMMMMMMM74Lo0wzx3wsgiRd10aP4I4kIok8E13aH/zzzzznvzcYY7zw5/+4zzzzxzjTPvjXPLvOsEAAAAFITfzzzwT3rnHsMAAAAgAAAAAAAAAAAAAHPvIMIJIMNLMMfEPMEAYAAAAAAAAAAAAAAAoYIIIIIIIIIIIIOCo4AAAAYEQmMBMnyx8oIIIIIIIIIIIIIOYV5CIYI4IIoLzzyzsM88I4Yc4oIIIIIIIIazzzzzzzzzzzzzzzzzzzyysIIYoY4/zzzzzzzzzzzzzzzzzzzzzzyyxzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz/xAAfEQEBAQACAwADAQAAAAAAAAARAAEQIDFAQTBRYHH/2gAIAQMBAT8QIiIiIjgREREREREcCIiIiIiIiIiIiIiI4ERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERaZ5s1+xERERETMzMzMzM25y3U5ylK0LdZ5t/GzMzMzMwjDgzCEOO63vueJs3yyerspT33EYwjHiY27t17Kz97Nzev8AmzSzT7zuflu7vqZuPF9WVm+bdzNm8szL6D+fNZZ+9m5vVyEYznOPL6Gb+xtu63l6vVmZmZmfxPrPVmf4X//EACIRAQEBAAMAAgICAwAAAAAAABEAARAgQTBAMWEhUVBwcf/aAAgBAgEBPxBmZmZmZmZmegegeRmZmZmZmZmZmeR+IAPIzMzMzMz9YAAAMzMzMzweD9AAAPxgA9wHkZnsB5GZmZmegegZmZmZmZnkZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZs/ngZmZmZn5kzHu9X/f7LxW5uXtzm3luXnbN/OPcfLcvI2zTy/Tfrs38s9PgTN8FJ+ci8szM/HO5m26+X67d78JnfN7Kc5ylKVmPbMO2423+lubnX/q3K3WecFm/bMZ9Tc7KUOxkOQciX+AMth1I2XAQh9RvIM/0D//xAArEAEAAgAEBAcAAgMBAAAAAAABABEhMVFhEEBxgSAwQVCR8PFwobHB0eH/2gAIAQEAAT8Q/goAAAAAAAAAAAAAAAAAAAAAACpXKAAAOidE6OTAAAqVKlSpUqVKlSpUqVKlSuCpXEr2EAAAAArwCv4JAAAAAAAAAAAAAAAAAAAAAACpXEryABz4AAAACuVAAADmwAAAA88AAAAAecAAHvIAAAAAAAAAPCA5AAAHgPsQAAAAAHNAAAADxDwHlAAAA4g8kAEVDgHIgAAeIqVxHngAAAAAgcBxBByAAAMPEJKlewgAAAABBwDyQBXkAHgPOgAAAAPKAAgIEOICBKleAVwVKleAVK4GHnwAAAABBxFQIRUqVK4lSpUqVK4lRJXErxArnAAAABXECVwBKlSpUqVKlSpUqVKlSpUqJK4KjFcFcR5gAAACvAKhFSpUqVKlSpUqadaxEyWYu6RUgpJUqVKlcFSpUYqV4BUrlAAAFeEK4DrM3YfYloETGnfThD/wSfhE/KJ+UT8In4xPpUNJj6tT1gRnK0KOLoQEaNMqVKlSpUqVKlSpXBXEqVKlSpUqVKlSpUqVKlSuJXgFRWnYQgnYZx22Wq+Y4txnYXKYBXiimu0KChY3iSuLursmZAIaypUSOhh6rHSrNG59Kn0qBhMXWAAgjkjKlSpUqVywAAAqUCoA9WIIv6Eti9vkgRINiNVGypd5P8o6LKYtZz8Z/wCy1bCEQWbo1FOa+Gx6sSEwdY+sT8JhBd0GE+/4iTAO2MUAAuiq4KogeqxihvTFMp0lj8Jn4TBXslNRRg3bGIBUL6Iz8ZLbAM6bjYVPRj8ZPtGI11YT8Y/7Pxj/ALPxiUmy+l1wwddSEevqD5xbRehnFy7rCqX2wglKN2CUI2ZlK74wtAg/POmMosq6kUpTsy/PswjeFGMVtTd8kUyZa8qLYBuz16ppBGflZGOjCqH3xh3DTaOVat4IliJzAFoDeZNVG2CH3hPvCJYp1HBPrc+tx+WdcYooBuE+sn7k/dn7sGofefeE+8JnK7YRVbVeRXtiMwNOpDbBNnydw+YI5J5RFkEzgd0atl9mYthlBV25Mib08FxShO7EWv8AGbj8yxxbOmpQUxd1ihTGLzVS/tToz+IWFbq1BhBHJGbh8zcPmZE3pGbKt2MWy+VZrLNfYBKEdZ9ZM5XbCZmOr5NtWbj8wBsXzP1IJQ+8JVCG5hArD3ljk8RLA6swe7bS9hBvM2fbCKLVWW6xKx94XKZoZ0Znl7gpaHRn60/Slr6+TmERf9hNDOjBHJH3xCxRlfmN5l/OTIgOjydhmzMj2xiK0KfS59LiKsEyI98Jd5cct4g2mRX+0NtCc85hY0ZrJBEsbPJuaSvSZMr0IMxF3n0uLrDF1mx8IBS+0UtHq+PMg6MA422TwIWkdpl/CgC0JtzzPoaRtAtr6QSwej4MyC6EX/YzNDpyzdvqTC3oPPijYpMmffGKFf4wKker4xLA6wDBXoTZ+ES003SIUHvBlDPWWayzWJUJ3YpSe2MG4W9CbPwmz8PIwC9hlfbbRgC0JtxuWayzXn7A0RHA0asQtK7+K3Vlur54o2KT9SfqRS0XdlurLdWZkHRmSPvjBmNPUlLBXZn0uODDF14Z95F2/wCyCUPvjCVQEr7baPJfIh6R20r7VlLZowul2HzsBvYJhuWg9uwC9hmFrbeCJYj4EFqBES/7J95BuFvQgzEXeKyTrM0a09yQtIxazuMzR9sIoU1dZbr/ACb/AP/Z",
            at: 12.0,
            code: "tilo://invite/gallery",
            source: "text(\"Hills at dawn\")",
        ),
    )
    m.photos.insert(Photo(title: "Dawn", place: "Black Forest", picture: m.photo))
    m.photos.insert(
        Photo(
            title: "Blue hour",
            place: "Lake Constance",
            picture: "data:image/jpeg;base64,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",
        ),
    )
    m.photos.insert(
        Photo(
            title: "Dunes",
            place: "Erg Chebbi",
            picture: "data:image/jpeg;base64,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NxuI3ERG4iIiIiIiIiIiIiIiIiIiIiIiIiMxmMxmM+sBpG43G43G4iIiIiIiIiIiIiIiIiIiIiIiIiMxERmMxmMxmMz6Q3G43G43G43EREREREREREREREREREREREREREZjMZjMZjMZjMREREbjcbjcbjcbjcRERERERERERERERERERERERERGYjMZjMZjMZjMZjBzZM3qwbLmdG43G43G43G43EbiIiIiIiIiIiIiIiIiIiIiIiIiIjMZjMZjMZjMRlfFwzMzJjczcmsr4mEbjcbjcbjcbjcbiIiIiIiIiIiIiIiIiIiIiIiIjMRmMxmMxmMxmIw0OfjhgckbjcbjcbjcbiNxERERERERERERERERERERERERmIjMZjMZjMZnhrdZubNxHGY4c1dZjjza3W7vnu+G4jcbjcbiI3ERERERERERERERERERERERERERGYjMYz0da3ERERERERERERERERERERERERERERERmIzERn16jcbjcbiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIzETwn0zxjcRERERERERERERERERERERERERERERERET0YiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiJ6MRERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERERE8IiIieERERERERERERERERERERERERERERERmOTDBvnub74wb5G+yHzM3s1TgvVrC6zfbURERERERERERERERERERERERERERERERERERPqu44Fv8+bPwNZXFru4Mb7aiIiIiJ4REREREREREREREREREREREREUI5ucqx8N32Y0nk57unPb0rucHCd7ndr/9MTeDd5awLrNzt4X/AC3OuLnXHAjPfXyj5R8o+UfKPlHyj5R8o+UcUz+PN/XGHZld8xi3zdzvqvyMzu0bcjtmMKa7+f8AFoXW5vZI4szqi8nd7Ne+dz3xhXM/lmC5ubnoXOrEuGZ31jXfxa3LOb8Nl5UfxfBN23k/Dim/x5PlF3rq711ftF3qwprmdtfKP64/rj+uNK7v8PlG6Lu7urv/AATcZl3cxuWWH8MbXhrsnZOydk7Ov4YzbTs/2zbhjVzr6lVVVVVVVVVVVVVVVVVVVW4zjrqT2bjl+zdeExu++evWrMsma/buv2zDJ6CSSSbtx0qqqqqqqqqqqqqqqqqq4objlmtfOezd3eO7v+7ta7Wu677sP2zG8NVVVVVVVVVVVW4cdbnlm60cPJunHd+0Zu5w13juvYexmOeOxrF3ned5q7Gt6MbrlmN05t3d47v/AEqAAAAAAAANzmXdzG4b54/DBxNQeRnu3bJkz2f2xum3d/t80+a+4AAAAAAAA3OcdjDzvs05P26A9m7u7d3d9HHzvuzfPGbcbjNubM7w37QAAAAAANy5t6caec9m7u8d/wAWbucNdzXcdx3Hcdx3Hcdx3Hcdx3HcZ1HtM3ze1PRmXcT18FVVVVVfBPVuG9ON6jd3eO/brvVd6q6q6q6q6q6q6rvX/wADgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAf/2Q==",
        ),
    )
    m.photos.insert(
        Photo(
            title: "Pines in fog",
            place: "Harz",
            picture: "data:image/jpeg;base64,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",
            tall: true,
        ),
    )
}
