//! What a window's tab is made of: the content its source builds for it,
//! the mark it wears, and the stand-in it starts from.

use super::TileSource;
use tilo_desktop_model::{Tab, Workspace};
use ui::widget::{TabDescriptor, TileContent, TileHost};

/// `tab` 's content from `tiles` , its application named to a panic hook while it is built and
/// whenever it runs, and bound to the host's `controls` when the host already bound the surface.
pub fn build_tile(
    tiles: &mut dyn TileSource,
    controls: Option<&tilo_ui_base::ControlDomain>,
    workspace: &Workspace,
    tab: &Tab,
) -> TileHost {
    let _running = tilo_ui_base::widget::panic_scope::enter(Some(tab.id));
    let mut host = tiles.tile(workspace, tab);
    host.set_panic_scope(tab.id);
    // Its app's bar is drawn here: what it declares goes on its window.
    let window = ui::hosted_window::HostedWindow::new();
    window.set_workspace(tiles.workspace_id());
    host.set_hosted_window(window);
    if let Some(domain) = controls {
        tilo_ui_base::widget::controls::attach_controls(
            &mut host,
            domain,
            tilo_ui_base::control_domain::DomainAttachment::Dynamic,
        );
    }
    host
}

pub fn descriptor(tab: &Tab) -> TabDescriptor {
    TabDescriptor {
        app_kind: Some(tile_app_kind(&tab.tile)),
        named: tab.is_named(),
        app_name: app_name_of(tab),
        ..TabDescriptor::new(tab.id, tab.title.clone())
    }
}

/// What the window `tab` shows is called by the one rule every surface names a window by: the
/// window's title and the app it shows ("Groceries — Notes"), the app's name alone where the title
/// only names the app under another spelling (`chat` is `Chat`), and the title itself for a window
/// that runs no app. A surface never shows an app id.
pub fn window_name(tab: &Tab) -> String {
    tile_app_id(&tab.tile).map_or_else(
        || tab.title.clone(),
        |app| tilo_desktop_search::names::window_name(app, &tab.title),
    )
}

/// The title a surface shows for the window `tab` beside the app's own mark: the window's title,
/// or the app's name where the title only names the app under another spelling (`chat` is `Chat`).
/// The mark already says the app, so the title never repeats it.
pub fn window_title(tab: &Tab) -> String {
    tile_app_id(&tab.tile).map_or_else(
        || tab.title.clone(),
        |app| tilo_desktop_search::names::window_title(app, &tab.title),
    )
}

/// The app showing `tab`, named as a person knows it — the name the app is listed under, never the
/// id it installs under ("Notes", not "so.tilo.notes"). `None` for a tile that runs no app: a
/// stand-in, or a window whose app has not arrived.
pub fn app_name_of(tab: &Tab) -> Option<String> {
    tile_app_id(&tab.tile).map(tilo_desktop_search::names::app_name)
}

/// The app a window's tile shows; `None` for a stand-in.
pub fn tile_app_id(tile: &tilo_desktop_model::TileContent) -> Option<&str> {
    match tile {
        tilo_desktop_model::TileContent::Foreign { app_id, .. }
        | tilo_desktop_model::TileContent::LocalApp { app_id, .. }
        | tilo_desktop_model::TileContent::HostedWindow { app_id, .. } => Some(app_id),
        tilo_desktop_model::TileContent::AppPlaceholder { .. }
        | tilo_desktop_model::TileContent::Stub { .. } => None,
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
        | tilo_desktop_model::TileContent::HostedWindow { app_id, .. } => {
            Kind::App(tilo_ui_base::icons::app_glyph(app_id, None))
        }
    }
}

pub fn placeholder() -> TileContent {
    TileContent::Notice {
        title: String::new(),
        detail: String::new(),
    }
}
