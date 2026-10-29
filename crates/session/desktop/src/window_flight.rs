//! Pictures of windows that a surface names and the compositor draws.
//!
//! A surface that shows other apps' windows (the workspace overview) never
//! draws their buffers: they belong to other clients, and only the
//! compositor holds them. The surface names each window it pictures and the
//! rect its picture rests at ([`WindowFlight::show`]); the compositor draws
//! every named window's latest buffer there, flying each one from where its
//! window stands on its strip to that rect while the surface comes up, and
//! back while it goes, on one clock it reports to the surface
//! ([`WindowFlight::progress`]) so the surface's own drawing (titles,
//! selection, its panel) moves in step.
//!
//! Everything here is plain data in the surface's own coordinates, shared
//! by the two sides in one process.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::TabId;
use vello::kurbo::Rect;

/// One window the surface pictures.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowPicture {
    /// The window, by its tab.
    pub tab: TabId,
    /// Where its picture rests.
    pub rect: Rect,
    /// The picture's corner radius at rest.
    pub radius: f64,
    /// How much of the picture shows at rest: a window being carried
    /// elsewhere is left faint in its place.
    pub alpha: f32,
    /// It flies from where its window stands on screen: the tab its window
    /// shows. Any other fades in where it rests.
    pub flies: bool,
    /// Held up over everything else, with a shadow: the window being
    /// carried under the pointer or finger.
    pub lifted: bool,
}

#[derive(Default)]
struct Shared {
    pictures: Vec<WindowPicture>,
    clip: Option<Rect>,
    progress: f64,
    sources: HashMap<TabId, Rect>,
    generation: u64,
}

/// The surface's and the compositor's side of the pictures, shared.
#[derive(Clone, Default)]
pub struct WindowFlight(Rc<RefCell<Shared>>);

impl WindowFlight {
    pub fn new() -> Self {
        Self::default()
    }

    /// The surface's side: picture `pictures`, in drawing order, clipped at
    /// rest to `clip` (in flight they may cross the whole display).
    pub fn show(&self, pictures: Vec<WindowPicture>, clip: Option<Rect>) {
        let mut shared = self.0.borrow_mut();
        if shared.pictures != pictures || shared.clip != clip {
            shared.pictures = pictures;
            shared.clip = clip;
            shared.generation = shared.generation.wrapping_add(1);
        }
    }

    /// Picture nothing.
    pub fn clear(&self) {
        self.show(Vec::new(), None);
    }

    /// How far the pictures are from where their windows stand (0) to where
    /// they rest (1), as the compositor draws them now.
    pub fn progress(&self) -> f64 {
        self.0.borrow().progress
    }

    /// Where window `tab` stands on screen, which its picture flies from;
    /// `None` for one that is not on screen.
    pub fn source(&self, tab: TabId) -> Option<Rect> {
        self.0.borrow().sources.get(&tab).copied()
    }

    /// The compositor's side: what the surface pictures now.
    pub fn pictures(&self) -> Vec<WindowPicture> {
        self.0.borrow().pictures.clone()
    }

    /// Where the pictures are clipped at rest.
    pub fn clip(&self) -> Option<Rect> {
        self.0.borrow().clip
    }

    /// Bumped each time the surface pictures something else.
    pub fn generation(&self) -> u64 {
        self.0.borrow().generation
    }

    /// The compositor draws the pictures at `progress` now, and the windows
    /// stand at `sources` (in the surface's coordinates).
    pub fn set_clock(&self, progress: f64, sources: HashMap<TabId, Rect>) {
        let mut shared = self.0.borrow_mut();
        shared.progress = progress.clamp(0.0, 1.0);
        shared.sources = sources;
    }
}

impl std::fmt::Debug for WindowFlight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shared = self.0.borrow();
        f.debug_struct("WindowFlight")
            .field("pictures", &shared.pictures.len())
            .field("progress", &shared.progress)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_names_pictures_and_the_compositor_reports_its_clock() {
        let flight = WindowFlight::new();
        let picture = WindowPicture {
            tab: 7,
            rect: Rect::new(10.0, 10.0, 110.0, 70.0),
            radius: 8.0,
            alpha: 1.0,
            flies: true,
            lifted: false,
        };
        flight.show(vec![picture.clone()], None);
        let generation = flight.generation();
        flight.show(vec![picture.clone()], None);
        assert_eq!(flight.generation(), generation, "the same pictures again");
        assert_eq!(flight.pictures(), vec![picture]);

        let compositor = flight.clone();
        compositor.set_clock(1.5, HashMap::from([(7, Rect::new(0.0, 0.0, 800.0, 600.0))]));
        assert!((flight.progress() - 1.0).abs() < f64::EPSILON);
        assert_eq!(flight.source(7), Some(Rect::new(0.0, 0.0, 800.0, 600.0)));
        assert_eq!(flight.source(8), None);
        flight.clear();
        assert!(compositor.pictures().is_empty());
    }
}
