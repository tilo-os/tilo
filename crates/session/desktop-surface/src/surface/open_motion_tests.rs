//! Where a window opened from search lands: whole on screen beside the
//! window the person was in, which keeps its name in view, as the native
//! desktop places it.

use super::*;

struct Apps;

impl TileSource for Apps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str) -> Option<Tab> {
        Some(Tab {
            id,
            title: app.to_owned(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.to_owned(),
                label: app.to_owned(),
                note: String::new(),
            },
            app_title: None,
        })
    }
}

/// A desktop `width` px wide showing one window, Chat, run on a held clock.
struct Desk {
    frame: ui::headless::FrameHarness,
    handle: DesktopHandle,
    now: Instant,
    width: f64,
}

impl Desk {
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
        };
        let tree = seed_tree(&mut minter, "Desk", vec![chat]);
        let (surface, handle) = DesktopSurface::new(tree, 9, Box::new(Apps));
        handle.set_viewport_width(f64::from(width));
        let frame = ui::headless::FrameHarness::for_root_with(
            ui::state::State::new(),
            surface,
            (width, 800),
            ui::headless::FrameOptions::default(),
        );
        let mut desk = Self {
            frame,
            handle,
            now: ui::time::now(),
            width: f64::from(width),
        };
        desk.frames(4);
        desk
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.now = self.now.checked_add(Duration::from_millis(16)).unwrap();
            tilo_clock::testing::with_monotonic_time(self.now, || self.frame.relayout());
        }
    }

    fn launch(&mut self, app: &str) {
        self.handle
            .dispatch(
                &navigation::Command::Launch {
                    generation: "g".into(),
                    app: app.into(),
                },
                "g",
            )
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
        let mut desk = Desk::with_chat(width);
        desk.launch("so.tilo.weather");
        let row = desk.row();
        assert_eq!(row.groups.len(), 2, "{width}: Weather opened");
        let (chat, weather) = (&row.groups[0], &row.groups[1]);
        let (chat_rect, weather_rect) = (chat.get_rect(), weather.get_rect());
        let title = chat.tab_rects()[0];
        assert!(
            title.x0 >= 0.0 && title.x1 <= desk.width,
            "{width}: Chat's name stays on screen: {title:?} of {chat_rect:?}"
        );
        assert!(
            chat_rect.x0 >= 0.0,
            "{width}: the window in use stays whole on screen: {chat_rect:?}"
        );
        assert!(
            weather_rect.x1 <= desk.width - STRIP_EDGE_GUTTER_PX + 0.5,
            "{width}: the new window keeps the strip's gutter, so its border \
             and corners show: {weather_rect:?}"
        );
        assert!(
            weather.tab_rects()[0].x1 <= desk.width,
            "{width}: and its name"
        );
        let widths = desk.handle.read(|tree| {
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .map(|g| g.width)
                .collect::<Vec<_>>()
        });
        assert!(
            widths.iter().any(|w| matches!(w, WindowWidth::Fraction(_))),
            "{width}: {widths:?}"
        );
    }
}

/// A window's tab wears its app's mark, the one the native title bar
/// shows, both for a window already there and for one just opened.
#[test]
fn each_tab_wears_its_apps_mark() {
    let mut desk = Desk::with_chat(1440);
    desk.launch("so.tilo.weather");
    let marks: Vec<_> = desk
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
