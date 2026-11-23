app so.tilo.settings "Settings" (icon: .settings, category: .system)

/// The person's settings window: every page is a view of the system resources that keep what it
/// shows — the device's sound, speakers, network and displays, the person's own settings, and the
/// accounts and providers they set up — so a change here is the same change a press in the shell,
/// a script or an agent makes.
type Settings {
}

root Settings

default view settings_window for Settings

/// The settings window, its pages down the side.
view settings_window(s: Settings) {
    state page: Text = "speakers"
    ui.NavShell(
        content_kind: .page,
        title: if page == "speakers" { "Speakers" } else { "Settings" },
        sidebar_title: "Settings",
    ) {
        for each in ["speakers"] {
            ui.NavItem(page_title(each), icon: .speaker, chosen: page == each) {
                on press: set(page, each)
            }
        }
        content: {
            if page == "speakers" {
                speakers_page()
            }
        }
    }
}

/// What a page is called, down the side and at the top.
fn page_title(page: Text) -> Text {
    if page == "speakers" { "Speakers" } else { page }
}
