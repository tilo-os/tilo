//! The band across the window's foot ([`NavShell::bottom_band`]).

use super::*;

/// The band spans the window's foot under every column: a docked sidebar, the content and an
/// open drawer all stop above it, and its control answers a press while the drawer is open.
#[test]
fn every_column_and_an_open_drawer_stop_above_the_band() {
    for (kind, (w, h_px), drawer) in [
        (ContentKind::Grid, (1400, 800), false),
        (ContentKind::Document, (400, 700), true),
    ] {
        let presses = SharedRc::new(Cell::new(0usize));
        let counted = SharedRc::clone(&presses);
        let band =
            Button::new("band-play", "Play").on_click(move || counted.set(counted.get() + 1));
        let shell = NavShell::new()
            .content_kind(kind)
            .sidebar(TestLeaf::fill("sidebar").boxed().0)
            .content(TestLeaf::fill("content").boxed().0)
            .bottom_band(Box::new(band));
        let mut h = harness(shell, w, h_px, Theme::dark());
        settle(&mut h, w, h_px);
        if drawer {
            h.root_mut::<NavShell>().open_sidebar();
            settle(&mut h, w, h_px);
        }
        let g = geometry(&h);
        let expected = if drawer {
            SidebarPlacement::Drawer
        } else {
            SidebarPlacement::Docked
        };
        assert_eq!(g.sidebar, expected, "{kind:?}");
        assert!(h.root::<NavShell>().is_sidebar_open());
        assert!(g.band.height() > 0.0, "{kind:?}: as tall as its control");
        assert_eq!(
            (g.band.x0, g.band.x1, g.band.y1),
            (0.0, f64::from(w), f64::from(h_px)),
            "{kind:?}: across the window's foot"
        );
        assert!(
            g.sidebar_column.y1 <= g.band.y0 + 0.5,
            "{kind:?}: the sidebar stops above it"
        );
        if !drawer {
            assert!(
                g.content_column.y1 <= g.band.y0 + 0.5,
                "the content stops above it"
            );
        }
        let button = child_rect(&h, "band-play").expect("the band's control is laid out");
        assert!(g.band.contains(button.center()));
        h.click_at(button.center().x, button.center().y);
        assert_eq!(presses.get(), 1, "{kind:?}: a press reaches the band");
    }
}
