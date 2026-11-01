//! What the Alt+Tab switcher knows of a host: the artwork each window shows
//! and what may be pictured of it ([`SwitcherWindows`]), and the pictures
//! themselves, drawn by the host through the same channel as the overview's
//! ([`SwitcherPictures`]).
//!
//! A card is the same picture as the overview's tile: a client window's own
//! buffer, drawn by the compositor where the card's well stands, never
//! captured or read back here. A tab that names no window this host holds — an
//! app still on its way, a window hosted elsewhere — keeps its app mark; and a
//! window whose app keeps what it shows to itself is marked withheld and never
//! pictured.

use ui::icons::AppImage;
use ui::kurbo::{Affine, Rect};
use ui::theme::Theme;
use vello::Scene;

use tilo_desktop_model::{GroupId, TileContent};
use tilo_desktop_surface::DesktopHandle;

use crate::window_flight::{WindowFlight, WindowPicture};
use crate::window_switcher::{PreviewSource, SwitcherWindow, WindowSource, WindowThumbnails};

/// The switcher's view of a host: a window shows the icon it set for itself
/// and keeps its app's glyph when it set none; a client's window is pictured
/// from its own buffer by the host ([`SwitcherPictures`]), and a window whose
/// app keeps what it shows to itself is never pictured.
pub(crate) struct SwitcherWindows<'a> {
    pub(crate) handle: &'a DesktopHandle,
}

impl WindowSource for SwitcherWindows<'_> {
    fn app_icon(&self, tile: &TileContent) -> Option<AppImage> {
        let TileContent::Foreign { surface_id, .. } = tile else {
            return None;
        };
        self.handle.window_artwork(*surface_id)
    }

    fn preview_source(
        &self,
        tile: &TileContent,
        _group: GroupId,
        _tab: u64,
        _shown: bool,
    ) -> PreviewSource {
        // Only a client's tab names a window this host holds: an app still on
        // its way has no window yet, a window hosted elsewhere is another
        // device's to draw, and a placeholder is no window at all. Those keep
        // their app mark, as they do in the overview.
        let TileContent::Foreign { app_id, surface_id } = tile else {
            return PreviewSource::Unavailable;
        };
        if self.handle.withholds(app_id) {
            PreviewSource::Withheld
        } else {
            PreviewSource::Client {
                surface: *surface_id,
            }
        }
    }
}

/// The switcher's pictures, drawn by the host through the same
/// [`WindowFlight`] the overview flies its own on: a card names the window it
/// shows and where its picture rests, and the compositor draws that window's
/// live buffer there — so a window looks the same on a card as in the
/// overview, and nothing is captured, copied or read back. A host with no
/// picture channel draws none, and every card keeps its app mark.
#[derive(Default)]
pub(crate) struct SwitcherPictures {
    /// The host's picture channel, when it pictures windows at all.
    flight: Option<WindowFlight>,
    /// The pictures the frame being drawn named, in drawing order.
    drawn: Vec<WindowPicture>,
    /// The panel the pictures stay inside.
    clip: Option<Rect>,
}

impl SwitcherPictures {
    pub(crate) fn set_flight(&mut self, flight: WindowFlight) {
        self.flight = Some(flight);
    }

    /// Say where the cards of the frame about to be drawn stand, so no
    /// picture reaches past the panel.
    pub(crate) fn clip_to(&mut self, panel: Rect) {
        self.clip = Some(panel);
    }

    /// Hand the pictures of the frame just drawn to the host. A frame that
    /// names none writes nothing: the panels drawn before this one write the
    /// channel every frame, and the last of them to name a picture decides
    /// what the host draws.
    pub(crate) fn publish(&mut self) {
        let Some(flight) = &self.flight else {
            return;
        };
        let drawn = std::mem::take(&mut self.drawn);
        if !drawn.is_empty() {
            flight.show(drawn, self.clip);
        }
    }
}

impl WindowThumbnails for SwitcherPictures {
    fn has(&self, window: &SwitcherWindow) -> bool {
        // A client's own buffer is the host's to draw: the card holds a
        // picture whenever the host pictures windows at all, and a window
        // that has presented nothing stays blank.
        self.flight.is_some() && matches!(window.preview, PreviewSource::Client { .. })
    }

    fn paint(
        &mut self,
        window: &SwitcherWindow,
        _scene: &mut Scene,
        _transform: Affine,
        rect: Rect,
        theme: &Theme,
    ) -> Option<Rect> {
        if !self.has(window) {
            return None;
        }
        self.drawn.push(WindowPicture {
            tab: window.address.tab,
            rect,
            radius: f64::from(theme.tokens.radii.control),
            alpha: 1.0,
            flies: false,
            lifted: false,
        });
        Some(rect)
    }
}

#[cfg(test)]
mod tests {
    use ui::theme::Theme;
    use ui::time::Duration;
    use vello::Scene;

    use tilo_desktop_model::{
        DesktopTree, Strip, StripId, Tab, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };

    use super::*;
    use crate::window_switcher::{
        NoThumbnails, PreviewState, REVEAL_DELAY, SwitcherScope, WindowSwitcher,
    };

    /// A desk whose windows have no content of their own: what a card shows
    /// of one comes from its app, never from the desk.
    struct NoTiles;

    impl tilo_desktop_surface::TileSource for NoTiles {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    fn window(id: u64, app: &str, surface: u64) -> Tab {
        Tab {
            id,
            title: format!("{app} {id}"),
            tile: TileContent::Foreign {
                app_id: app.into(),
                surface_id: surface,
            },
            app_title: None,
        }
    }

    fn group(id: GroupId, tab: Tab) -> WindowGroup {
        WindowGroup {
            id,
            active_tab: tab.id,
            tabs: vec![tab],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// Three windows of their own apps: one that is pictured, one beside it,
    /// and one whose app keeps what it shows to itself.
    fn desk() -> (DesktopTree, DesktopHandle) {
        let mut workspace = Workspace::new(WorkspaceId(1), "Work");
        workspace.strips = vec![Strip {
            id: StripId(2),
            name: "Main".into(),
            window_groups: vec![
                group(10, window(20, "so.tilo.notes", 120)),
                group(30, window(40, "so.tilo.chat", 140)),
                group(50, window(60, "org.keepassxc.KeePassXC", 160)),
            ],
        }];
        let mut tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..Default::default()
        };
        tree.active_strip.insert(WorkspaceId(1), StripId(2));
        tree.active_group.insert(StripId(2), 10);
        let (_surface, handle) =
            tilo_desktop_surface::DesktopSurface::new(tree.clone(), 3, Box::new(NoTiles));
        (tree, handle)
    }

    /// A card shows its window's own picture, named to the host at the card's
    /// well for the compositor to draw the window's live buffer into — the
    /// same picture the overview shows. A window whose app keeps what it
    /// shows to itself is never pictured, and a host that pictures nothing
    /// leaves every card its app mark.
    #[test]
    fn a_switcher_card_pictures_its_window_unless_the_app_withholds_it() {
        let (tree, handle) = desk();
        handle.set_withheld_apps(vec!["org.keepassxc.KeePassXC".into()]);
        let source = SwitcherWindows { handle: &handle };
        let rows = WindowSwitcher::default().ordered_windows(&tree, &source);
        let row = |tab: u64| {
            rows.iter()
                .find(|row| row.address.tab == tab)
                .expect("the desk holds that window")
        };
        assert_eq!(row(20).preview, PreviewSource::Client { surface: 120 });
        assert_eq!(
            row(60).preview,
            PreviewSource::Withheld,
            "an app that keeps its content is never pictured"
        );

        let mut switcher = WindowSwitcher::default();
        let now = ui::time::now();
        assert!(switcher.begin(&tree, &source, SwitcherScope::Workspace, false, now));
        switcher.elapse(REVEAL_DELAY + Duration::from_millis(100));

        let flight = WindowFlight::new();
        let mut pictures = SwitcherPictures::default();
        pictures.set_flight(flight.clone());
        let theme = Theme::dark();
        let mut text = ui::text_render::TextRenderer::new();
        let mut scene = Scene::new();
        switcher.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            Rect::new(0.0, 0.0, 1280.0, 800.0),
            now,
            &mut pictures,
        );
        pictures.publish();

        let named: Vec<u64> = flight
            .pictures()
            .iter()
            .map(|picture| picture.tab)
            .collect();
        assert!(
            named.contains(&20) && named.contains(&40),
            "each card names its window's picture: {named:?}"
        );
        assert!(
            !named.contains(&60),
            "a window that hides its content is never named: {named:?}"
        );
        let panel = switcher.panel_rect();
        for picture in flight.pictures() {
            assert!(
                panel.contains(picture.rect.center()),
                "a picture rests on the panel: {picture:?} in {panel:?}"
            );
        }

        assert_eq!(
            WindowSwitcher::preview_state(row(20), &pictures),
            PreviewState::Live
        );
        assert_eq!(
            WindowSwitcher::preview_state(row(60), &pictures),
            PreviewState::Withheld
        );
        assert_eq!(
            WindowSwitcher::preview_state(row(20), &NoThumbnails),
            PreviewState::None,
            "the picturable window is no longer the no-picture stub"
        );
    }
}
