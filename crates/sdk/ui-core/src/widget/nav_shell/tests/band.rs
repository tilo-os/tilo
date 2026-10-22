//! The band across the window's foot ([`NavShell::bottom_band`]).

use super::*;

const W: u32 = 1400;
const H: u32 = 800;

/// A shell with a sidebar, a content column and a one-control band whose
/// presses are counted.
fn banded(kind: ContentKind) -> (NavShell, SharedRc<Cell<usize>>) {
    let presses = SharedRc::new(Cell::new(0usize));
    let counted = SharedRc::clone(&presses);
    let band = Button::new("band-play", "Play").on_click(move || counted.set(counted.get() + 1));
    let shell = NavShell::new()
        .with_id("banded")
        .content_kind(kind)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .bottom_band(Box::new(band));
    (shell, presses)
}

/// The band spans the window's whole width at its foot, as tall as what it
/// holds; the docked sidebar and the content stand over it, and a press on
/// it reaches what it holds.
#[test]
fn the_band_spans_the_window_under_every_column() {
    let (shell, presses) = banded(ContentKind::Grid);
    let mut h = harness(shell, W, H, Theme::dark());
    settle(&mut h, W, H);
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Docked);
    assert!(g.band.height() > 0.0, "as tall as its control");
    assert!((g.band.x0, g.band.x1, g.band.y1) == (0.0, f64::from(W), f64::from(H)));
    assert!(
        g.sidebar_column.y1 <= g.band.y0 + 0.5,
        "the sidebar stops above it"
    );
    assert!(
        g.content_column.y1 <= g.band.y0 + 0.5,
        "the content stops above it"
    );
    let at = g.band.center();
    let button = child_rect(&h, "band-play").expect("the band's control is laid out");
    assert!(
        g.band.contains(button.center()),
        "{button:?} in {:?}",
        g.band
    );
    h.click_at(button.center().x, button.center().y);
    assert_eq!(presses.get(), 1, "a press at {at:?} reaches the band");
}

/// In a narrow window the sidebar floats as a drawer over the content and
/// stops above the band too: the band's controls stay at hand while the
/// drawer is open.
#[test]
fn a_drawer_stops_above_the_band() {
    let (shell, presses) = banded(ContentKind::Document);
    let (w, h_px) = (400, 700);
    let mut h = harness(shell, w, h_px, Theme::dark());
    settle(&mut h, w, h_px);
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, w, h_px);
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Drawer);
    assert!(h.root::<NavShell>().is_sidebar_open());
    assert!(g.band.height() > 0.0);
    assert!(
        g.sidebar_column.y1 <= g.band.y0 + 0.5,
        "the drawer stops above it"
    );
    let button = child_rect(&h, "band-play").expect("the band's control is laid out");
    h.click_at(button.center().x, button.center().y);
    assert_eq!(presses.get(), 1, "the band answers beside the open drawer");
}
