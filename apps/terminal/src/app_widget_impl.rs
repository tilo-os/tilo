use crate::TerminalWidget;
use ui::{AppKind, AppWidget, IconDescriptor};

impl AppWidget for TerminalWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Terminal
    }

    fn app_title(&self) -> &str {
        "Terminal"
    }

    fn app_icon(&self) -> IconDescriptor {
        // Nerd Font terminal glyph — matches the icon used by the iced
        // file-explorer's "Open Terminal" button for visual consistency.
        IconDescriptor::Glyph("󰆍")
    }

    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Terminal)
    }
}
