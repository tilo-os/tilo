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
