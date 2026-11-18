app so.tilo.lang.layoutnavigation "Navigation" (icon: .layout_grid, category: .utility)

/// A window whose sidebar names two pages: the one it opens on, and one behind a choice.
type Notebook {
    title: Text
}

root notebook: Notebook

default view pages for Notebook

/// The notebook's window: its list of pages, and the page chosen beside it.
view pages(n: Notebook) {
    state showing: Text = "list"
    ui.NavShell(content_kind: .page, title: "Navigation", sidebar_title: "Pages") {
        sidebar: {
            ui.NavList(selected: showing) {
                ui.NavItem("list", "The list", icon: .list) {
                    on select: set(showing, "list")
                }
                ui.NavItem("board", "The board", icon: .layout_grid) {
                    on select: set(showing, "board")
                }
            }
        }
        content: {
            if showing == "board" {
                board(n)
            } else {
                listing(n)
            }
        }
    }
}

/// The page the window opens on.
view listing(n: Notebook) {
    ui.Text(n.title)
}

/// The page only choosing its destination shows.
view board(n: Notebook) {
    ui.Text("{n.title}, as a board")
}

scenario "a notebook" {
    let n = root Notebook
    n.title = "Plans"
}
