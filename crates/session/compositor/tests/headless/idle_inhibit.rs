//! A window that asks the screen to stay on holds the machine only while the person can see it: a
//! video left in a hidden tab or on a workspace nobody is on holds nothing.

use std::cell::Cell;

use super::*;

/// Where a parked window goes: far off every output, as the desktop puts a hidden tab or a
/// workspace nobody is on.
const PARKED: i32 = 100_000;

/// Places one window where `parked` says: on the output, or off it.
#[derive(Clone)]
struct Parkable {
    parked: Rc<Cell<bool>>,
}

impl WindowPolicy for Parkable {
    fn initial_size(
        &mut self,
        _: &WindowInfo,
        _: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        Some((250, 200))
    }

    fn place(
        &mut self,
        _: &WindowInfo,
        _: (i32, i32),
        _: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        Point::from((0, 0))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, Rectangle<i32, Logical>)],
        _: &[Rectangle<i32, Logical>],
    ) -> Option<Vec<Arrangement>> {
        let x = if self.parked.get() { PARKED } else { 0 };
        Some(
            windows
                .iter()
                .map(|(info, _)| Arrangement {
                    id: info.id,
                    location: Point::from((x, 0)),
                    size: None,
                })
                .collect(),
        )
    }
}

#[test]
fn an_idle_inhibitor_holds_only_while_its_window_is_on_screen() {
    let parked = Rc::new(Cell::new(false));
    let mut compositor = headless(Box::new(Parkable {
        parked: Rc::clone(&parked),
    }));
    let mut video = mapped_client(&mut compositor, "video", (250, 200), [255, 0, 0, 255]);
    assert!(!compositor.idle_inhibited(), "nothing holds the screen on");

    video.inhibit_idle();
    pump(&mut compositor, &mut [&mut video], |compositor, _| {
        compositor.idle_inhibited()
    });
    let holding = compositor.visible_idle_inhibitors();
    assert_eq!(holding.len(), 1, "the window the person watches holds it");
    assert_eq!(holding[0].app_id.as_deref(), Some("video"));

    // The same window parked off every output, as a hidden tab is: nobody is watching, so nothing
    // holds.
    parked.set(true);
    compositor.draw_frame().expect("a frame");
    assert!(!compositor.idle_inhibited());
    assert!(compositor.visible_idle_inhibitors().is_empty());

    // Brought back, it holds again until the client gives the inhibitor up.
    parked.set(false);
    compositor.draw_frame().expect("a frame");
    assert!(compositor.idle_inhibited());
    video
        .seen
        .idle_inhibitor
        .take()
        .expect("inhibitor")
        .destroy();
    pump(&mut compositor, &mut [&mut video], |compositor, _| {
        !compositor.idle_inhibited()
    });
}
