//! What a window's tab is made of: the content its source builds for it,
//! the mark it wears, and the stand-in it starts from.

use super::TileSource;
use tilo_desktop_model::{Tab, Workspace};
use ui::widget::{TabDescriptor, TileContent, TileHost};

/// `tab`'s content from `tiles`, its application named to a panic hook
/// while it is built and whenever it runs, and bound to the host's
/// `controls` when the host already bound the surface.
pub(super) fn build_tile(
    tiles: &mut dyn TileSource,
    controls: Option<&ui::ControlDomain>,
    workspace: &Workspace,
    tab: &Tab,
) -> TileHost {
    let _running = ui::widget::panic_scope::enter(Some(tab.id));
    let mut host = tiles.tile(workspace, tab);
    host.set_panic_scope(tab.id);
    // Its app's bar is drawn here: what it declares goes on its window.
    let window = ui::hosted_window::HostedWindow::new();
    window.set_workspace(tiles.workspace_room());
    host.set_hosted_window(window);
    if let Some(domain) = controls {
        ui::widget::controls::install_control_custody(
            &mut host,
            domain,
            ui::control_custody::CustodyAttachment::Dynamic,
        );
    }
    host
}

pub(super) fn descriptor(tab: &Tab) -> TabDescriptor {
    TabDescriptor {
        app_kind: Some(tile_app_kind(&tab.tile)),
        ..TabDescriptor::new(tab.id, tab.title.clone())
    }
}

/// The mark a tab for `tile` wears: its app's own symbol for a window,
/// the kind's glyph for a stand-in. Every host's title bar, switcher and
/// overview name a window's app by this one mapping.
pub fn tile_app_kind(tile: &tilo_desktop_model::TileContent) -> ui::widget::AppKind {
    use ui::widget::AppKind as Kind;
    match tile {
        tilo_desktop_model::TileContent::AppPlaceholder { kind, .. }
        | tilo_desktop_model::TileContent::Stub { kind, .. } => match kind {
            tilo_desktop_model::AppKind::Terminal => Kind::Terminal,
            tilo_desktop_model::AppKind::FileExplorer => Kind::Files,
            tilo_desktop_model::AppKind::Browser => Kind::Browser,
            tilo_desktop_model::AppKind::Other(id)
                if ["Notes", "Word", "Editor"]
                    .iter()
                    .any(|known| id.eq_ignore_ascii_case(known))
                    || ["so.tilo.word-processor", "so.tilo.notes"]
                        .iter()
                        .any(|known| id == known) =>
            {
                Kind::Editor
            }
            _ => Kind::Settings,
        },
        // A window wears its own app's glyph, never another app's.
        tilo_desktop_model::TileContent::Foreign { app_id, .. }
        | tilo_desktop_model::TileContent::LocalApp { app_id, .. }
        | tilo_desktop_model::TileContent::PeerWindow { app_id, .. }
        | tilo_desktop_model::TileContent::HostedWindow { app_id, .. } => {
            Kind::App(ui::icons::app_glyph(app_id, None))
        }
    }
}

pub(super) fn placeholder() -> TileContent {
    TileContent::Notice {
        title: String::new(),
        detail: String::new(),
    }
}
