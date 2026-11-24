app so.tilo.lang.layoutdrawer "Drawer" (icon: .layout_grid, category: .utility)

use so.tilo.lang.layoutdrawn as drawn

type Page {
}

root sheet: Page

default view drawer for Page

/// A page drawing another app's row, as Today draws Chat's Unread place.
view drawer(p: Page) {
    drawn.line(drawn.files)
}

scenario "a page" {
}

/// A screen this app's host shows by name, as a phone's shell shows its `phone` view: it takes
/// nothing, and no view of the package calls it.
view phone() {
    page(full) {
        ui.Text("Phone", role: .title)
    }
}

scenario "a phone screen" {
}
