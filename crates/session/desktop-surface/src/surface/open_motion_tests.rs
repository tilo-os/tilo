//! Where a window opened from search lands: whole on screen beside the window the person was in,
//! which keeps its name in view, as the native desktop places it.

use super::*;
use ui::widget::TileContent;

struct Apps;

impl TileSource for Apps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str, _: Option<TabId>) -> Option<Tab> {
        Some(Tab {
            id,
            title: app.to_owned(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.to_owned(),
                label: app.to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
    }
}

/// A desktop `width` px wide showing one window, Chat, run on a held clock.
struct Shell {
    frame: ui::headless::FrameHarness,
    handle: DesktopHandle,
    now: Instant,
    width: f64,
}

impl Shell {
    fn with_chat(width: u32) -> Self {
        let mut minter = IdMinter::new(9);
        let chat = Tab {
            id: minter.mint(),
            title: "Chat".into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: "so.tilo.chat".into(),
                label: "Chat".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tree = tilo_session_core::desktop::seed_tree(&mut minter, "Office", vec![chat]);
        let (surface, handle) = DesktopSurface::new(tree, 9, Box::new(Apps));
        handle.set_viewport_width(f64::from(width));
        let frame = ui::headless::FrameHarness::for_root_with(
            ui::state::State::new(),
            surface,
            (width, 800),
            ui::headless::FrameOptions::default(),
        );
        let mut shell = Self {
            frame,
            handle,
            now: ui::time::now(),
            width: f64::from(width),
        };
        shell.frames(4);
        shell
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.now = self.now.checked_add(Duration::from_millis(16)).unwrap();
            ui::time::testing::with_monotonic_time(self.now, || self.frame.relayout());
        }
    }

    fn launch(&mut self, app: &str) {
        self.handle
            .dispatch(navigation::ChromeEffect::OpenApp {
                app: app.into(),
                beside: None,
                background: None,
                float: None,
            })
            .unwrap();
        self.frames(60);
    }

    fn row(&self) -> &GroupRow {
        self.frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .expect("a strip is shown")
    }
}

#[test]
fn an_opened_window_lands_whole_beside_the_one_in_use_which_keeps_its_name() {
    for width in [1440, 1280, 1024] {
        let mut shell = Shell::with_chat(width);
        shell.launch("so.tilo.weather");
        let row = shell.row();
        assert_eq!(row.groups.len(), 2, "{width}: Weather opened");
        let (chat, weather) = (&row.groups[0], &row.groups[1]);
        let (chat_rect, weather_rect) = (chat.get_rect(), weather.get_rect());
        let title = chat.tab_rects()[0];
        assert!(
            title.x0 >= 0.0 && title.x1 <= shell.width,
            "{width}: Chat's name stays on screen: {title:?} of {chat_rect:?}"
        );
        assert!(
            chat_rect.x0 >= 0.0,
            "{width}: the window in use stays whole on screen: {chat_rect:?}"
        );
        assert!(
            weather_rect.x1 <= shell.width - STRIP_EDGE_GUTTER_PX + 0.5,
            "{width}: the new window keeps the strip's gutter, so its border \
             and corners show: {weather_rect:?}"
        );
        assert!(
            weather.tab_rects()[0].x1 <= shell.width,
            "{width}: and its name"
        );
        let widths = shell.handle.read(|tree| {
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .map(|g| g.width)
                .collect::<Vec<_>>()
        });
        // The shell shares the strip between them: a share it gives is not a width the person
        // chose, so the window in use takes the strip again wherever it is left alone.
        assert!(
            widths.iter().all(|w| matches!(w, WindowWidth::Share(_))),
            "{width}: the shell shares the strip: {widths:?}"
        );
    }
}

/// A window's tab wears its app's mark, the one the native title bar shows, both for a window
/// already there and for one just opened.
#[test]
fn each_tab_wears_its_apps_mark() {
    let mut shell = Shell::with_chat(1440);
    shell.launch("so.tilo.weather");
    let marks: Vec<_> = shell
        .row()
        .groups
        .iter()
        .map(|group| group.tabs()[0].app_kind)
        .collect();
    assert_eq!(
        marks,
        [
            Some(ui::widget::AppKind::App(ui::icons::Icon::MessageSquare)),
            Some(ui::widget::AppKind::App(ui::icons::Icon::Cloud)),
        ]
    );
}
