/// Which screen is which, said on the screen itself: while the person asks Settings to identify
/// the displays, each display says its number and name in its bottom-left corner for a moment —
/// the number its tile carries on Settings' placement canvas, and the name beside it, so "2" on
/// the canvas is the "2" the person is looking at.
///
/// The session answers the request and publishes it on the display's own reading
/// (`device.screen.identify`), so each part says only what its own display is. Nothing while
/// nothing was asked.
view identify_card() {
    let screen = device.screen
    if screen.identify {
        row(at: .bottom, fill, align: .center, distribute: .start, padding: .related) {
            ui.Card {
                row(gap: .related, align: .center) {
                    ui.Text("{screen.identify_number}", role: .display, numeric: true)
                    ui.Text(screen.identify_name, role: .subheading)
                }
            }
        }
    }
}
