//! The glyph that stands for an application where no artwork exists. Tilo apps ship no raster icon, so
//! every surface that lists applications (search palette, switcher, tab strip) asks here and gets the
//! same recognisable symbol per app, falling back by menu category and finally to [`Icon::AppWindow`].
//! It is never a magnifier or a grid: those name the *kind of row*, not the app.

use super::Icon;

/// The glyph for the package `package_id` (`so.tilo.notes`, or a local build's `dev.tilo.notes`) in
/// menu category `category`, if it has one.
#[must_use]
pub fn app_glyph(package_id: &str, category: Option<&str>) -> Icon {
    // A window of an app the machine installed says so by its desktop
    // entry's id (`host:so.tilo.notes.desktop`): the app is the same one
    // whichever way it was opened, so it gets the same glyph.
    let id = package_id.strip_prefix("host:").unwrap_or(package_id);
    let id = id.strip_suffix(".desktop").unwrap_or(id);
    let name = id
        .strip_prefix("so.tilo.")
        .or_else(|| id.strip_prefix("dev.tilo."))
        .unwrap_or(id);
    named(name).unwrap_or_else(|| category.map_or(Icon::AppWindow, category_glyph))
}

fn named(name: &str) -> Option<Icon> {
    Some(match name {
        "browser" => Icon::Globe,
        "file-explorer" => Icon::Folder,
        "notes" | "word-processor" | "scratchpad" => Icon::FileText,
        "terminal" => Icon::SquareTerminal,
        "settings" => Icon::Settings,
        "store" => Icon::ShoppingBag,
        "calendar" => Icon::Calendar,
        "tasks" => Icon::ListChecks,
        "mail" => Icon::Mail,
        "contacts" => Icon::Users,
        "chat" | "messages" => Icon::MessageSquare,
        "calculator" => Icon::Hash,
        "sheets" | "db-client" => Icon::Table,
        "slides" => Icon::PictureInPicture2,
        "pdf-viewer" | "ebook" | "publisher" => Icon::File,
        "typst-editor" | "code-editor" | "snippets" => Icon::Code,
        "music" => Icon::Music,
        "tube" | "video-player" | "video-editor" => Icon::Film,
        "photos" => Icon::Image,
        "camera" => Icon::Camera,
        "canvas" => Icon::PenTool,
        "weather" => Icon::Cloud,
        "maps" => Icon::MapPin,
        "feeds" => Icon::Rss,
        "totp" | "passwords" => Icon::Lock,
        "clocks" => Icon::Clock,
        "monitor" => Icon::Gauge,
        "disks" | "disk-usage" => Icon::HardDrive,
        "logs" => Icon::List,
        "firewall" | "vpn" => Icon::Shield,
        "remote" => Icon::ScreenShare,
        "backups" => Icon::Archive,
        "imager" => Icon::SdCard,
        "themes" => Icon::Palette,
        "print-centre" => Icon::Printer,
        "shortcuts" => Icon::Keyboard,
        "speed-test" => Icon::Timer,
        "api-tester" => Icon::Send,
        "containers" => Icon::Box,
        "diffmerge" | "git-client" => Icon::Columns2,
        "hex-editor" => Icon::FileCode,
        "windows" => Icon::LayoutGrid,
        _ => return None,
    })
}

fn category_glyph(category: &str) -> Icon {
    match category.to_ascii_lowercase().as_str() {
        "network" => Icon::Globe,
        "office" => Icon::FileText,
        "development" => Icon::Code,
        "multimedia" | "audiovideo" => Icon::Film,
        "system" | "settings" => Icon::Settings,
        "utility" => Icon::Box,
        "graphics" => Icon::Image,
        _ => Icon::AppWindow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_apps_get_their_own_glyph_whatever_the_build_prefix() {
        assert_eq!(app_glyph("so.tilo.notes", Some("Office")), Icon::FileText);
        assert_eq!(app_glyph("dev.tilo.terminal", None), Icon::SquareTerminal);
        assert_eq!(app_glyph("so.tilo.browser", None), Icon::Globe);
        assert_eq!(app_glyph("so.tilo.file-explorer", None), Icon::Folder);
    }

    #[test]
    fn unknown_apps_fall_back_by_category_then_to_a_window() {
        assert_eq!(app_glyph("org.example.x", Some("Development")), Icon::Code);
        assert_eq!(app_glyph("org.example.x", Some("Weird")), Icon::AppWindow);
        assert_eq!(app_glyph("org.example.x", None), Icon::AppWindow);
    }

    #[test]
    fn a_host_apps_desktop_entry_has_the_glyph_of_the_app_it_opens() {
        assert_eq!(
            app_glyph("host:so.tilo.notes", None),
            Icon::FileText,
            "a window of an app the machine installed shows that app's glyph"
        );
        assert_eq!(
            app_glyph("host:so.tilo.file-explorer.desktop", None),
            Icon::Folder
        );
    }

    #[test]
    fn no_application_glyph_is_a_search_or_generic_grid() {
        for id in ["browser", "notes", "terminal", "calendar", "weather"] {
            let glyph = app_glyph(id, None);
            assert!(!matches!(glyph, Icon::Search | Icon::LayoutGrid), "{id}");
        }
    }
}
