//! A painted layer's simulation, brought up to its marks away from the
//! window.
//!
//! Simulating a stroke of a slow medium (oil over a big grid) takes longer
//! than a frame. A live view never waits for it: the layer's
//! [`LayerPainter`] is handed to a worker with the marks to paint, and the
//! view keeps showing the last finished picture until the worker hands the
//! painter back with the new one. A stroke still being painted is previewed
//! the same way, the newest stroke asked for when the worker is free, and a
//! stroke just laid keeps showing as its preview until its picture is
//! ready, so it never blinks out.
//!
//! What is painted is the same wherever it runs: the same painter over the
//! same marks. A `look`, an export or a comparison waits for the exact
//! picture; a platform without threads paints in place.

use std::sync::{Arc, Condvar, Mutex, PoisonError};

use tilo_wet_media::{LayerPainter, Painted};
use ui::vello::peniko::ImageData;

use crate::picture::image_of;
use ui::lock::MutexRecover as _;

/// What a painted layer is asked to show.
#[derive(Clone)]
pub struct Ask {
    /// The layer's marks and medium, which its picture is kept under.
    pub key: blake3::Hash,
    pub marks: Arc<[Painted]>,
    /// A stroke still being painted over them, and what it is.
    pub wet: Option<(blake3::Hash, Arc<[Painted]>)>,
    /// The simulation grid, in cells.
    pub size: (usize, usize),
}

/// A stroke being painted as its medium paints it, over the layer's
/// picture under `over`.
#[derive(Clone)]
struct Preview {
    image: ImageData,
    over: blake3::Hash,
    stroke: blake3::Hash,
}

/// What a worker hands back.
struct Done {
    key: blake3::Hash,
    image: Option<ImageData>,
    preview: Option<Preview>,
}

/// Where a worker leaves the painter and what it painted.
#[derive(Default)]
struct Slot {
    done: Mutex<Option<(LayerPainter, Done)>>,
    ready: Condvar,
}

impl Slot {
    fn put(&self, painter: LayerPainter, done: Done) {
        *self.done.lock_recover() = Some((painter, done));
        self.ready.notify_all();
    }

    fn take(&self) -> Option<(LayerPainter, Done)> {
        self.done.lock_recover().take()
    }

    fn wait(&self) -> (LayerPainter, Done) {
        let mut done = self.done.lock_recover();
        loop {
            if let Some(done) = done.take() {
                return done;
            }
            done = self
                .ready
                .wait(done)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// One painted layer's simulation and its pictures.
pub struct Settle {
    /// The painter, while no worker has it.
    painter: Option<LayerPainter>,
    /// The worker painting, while one is.
    running: Option<Arc<Slot>>,
    /// The last finished picture.
    image: Option<ImageData>,
    /// What it shows, while that is still what the painter has painted.
    key: Option<blake3::Hash>,
    preview: Option<Preview>,
}

impl Settle {
    pub fn new(painter: LayerPainter) -> Self {
        Self {
            painter: Some(painter),
            running: None,
            image: None,
            key: None,
            preview: None,
        }
    }

    /// Whether a worker is painting: the view should draw again soon.
    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// Start again with `painter`, keeping what is shown until it has
    /// painted. A worker still painting finishes for nobody.
    pub fn replace(&mut self, painter: LayerPainter) {
        self.running = None;
        self.painter = Some(painter);
        self.key = None;
        self.preview = None;
    }

    /// The painter, while no worker has it.
    pub fn painter(&self) -> Option<&LayerPainter> {
        self.painter.as_ref()
    }

    /// Take in what a worker finished; with `wait` (or without threads),
    /// wait for it to finish.
    pub fn collect(&mut self, wait: bool) {
        let wait = wait || !threads();
        let Some(slot) = &self.running else {
            return;
        };
        let finished = if wait { Some(slot.wait()) } else { slot.take() };
        if let Some((painter, done)) = finished {
            self.running = None;
            self.adopt(painter, done);
        }
    }

    fn adopt(&mut self, painter: LayerPainter, done: Done) {
        self.painter = Some(painter);
        if let Some(image) = done.image {
            self.image = Some(image);
        }
        self.key = Some(done.key);
        // A picture painted without a stroke wet over it ends the
        // stroke's preview.
        self.preview = done.preview;
    }

    /// Whether `ask` needs painting beyond what is shown.
    fn wanted(&self, ask: &Ask) -> bool {
        self.key != Some(ask.key)
            || ask.wet.as_ref().is_some_and(|(stroke, _)| {
                self.preview
                    .as_ref()
                    .is_none_or(|preview| preview.stroke != *stroke || preview.over != ask.key)
            })
    }

    /// What `ask`'s layer shows now. With `wait`, the exact picture,
    /// painted here if need be; otherwise the last finished one, while a
    /// worker paints the one asked for.
    pub fn show(&mut self, ask: &Ask, wait: bool) -> Option<ImageData> {
        let wait = wait || !threads();
        self.collect(wait);
        if self.wanted(ask)
            && let Some(mut painter) = self.painter.take()
        {
            let have = self.key;
            if wait {
                let done = paint(&mut painter, ask, have);
                self.adopt(painter, done);
            } else {
                let slot = Arc::new(Slot::default());
                self.running = Some(Arc::clone(&slot));
                let ask = ask.clone();
                drop(ui::rt::app().spawn_blocking(move || {
                    let done = paint(&mut painter, &ask, have);
                    slot.put(painter, done);
                }));
            }
        }
        self.shown(ask)
    }

    /// The picture to show for `ask` from what is finished: the stroke
    /// being painted while there is one, and the stroke just laid as it
    /// was previewed until its picture is ready.
    fn shown(&self, ask: &Ask) -> Option<ImageData> {
        let have = self.key;
        if let Some(preview) = &self.preview
            && (ask.wet.is_some() || (have != Some(ask.key) && have == Some(preview.over)))
        {
            return Some(preview.image.clone());
        }
        self.image.clone()
    }
}

/// Whether painting can be handed to another thread here: a browser page
/// has one thread, and waiting there for a job would wait forever.
fn threads() -> bool {
    ui::rt::available(ui::rt::Feature::Threads)
}

/// Paint `ask` with `painter`: the layer's picture when it is not the one
/// under `have`, and the stroke being painted over it.
fn paint(painter: &mut LayerPainter, ask: &Ask, have: Option<blake3::Hash>) -> Done {
    let (width, height) = ask.size;
    let image = (have != Some(ask.key))
        .then(|| image_of(painter.paint(&ask.marks).to_vec(), width, height));
    let preview = ask.wet.as_ref().map(|(stroke, wet)| Preview {
        image: image_of(painter.preview(&ask.marks, wet), width, height),
        over: ask.key,
        stroke: *stroke,
    });
    Done {
        key: ask.key,
        image,
        preview,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tilo_wet_media::{
        Canvas, Engines, Heightfield, Mark, Settings, Stroke, StrokePoint, Texture, parse_paint,
    };

    use super::*;

    const SIZE: (usize, usize) = (64, 48);

    fn painter() -> LayerPainter {
        let engine = Engines::builtin().get("oil").unwrap();
        let settings = Settings::new(engine.parameters(), &BTreeMap::new());
        let ground = Arc::new(Heightfield::new(SIZE.0, SIZE.1, Texture::Linen, 1.0, 7));
        LayerPainter::new(engine, Canvas::whole(1.0, ground), settings)
    }

    fn stroke(id: &str, y: f32, colour: &str) -> Painted {
        Painted::new(
            id,
            Mark::Stroke(Stroke {
                points: [(6.0, y), (32.0, y + 4.0), (58.0, y)]
                    .iter()
                    .map(|(x, y)| StrokePoint::new(*x, *y, 1.0))
                    .collect(),
                size: 9.0,
                paint: parse_paint(colour, &[]).unwrap(),
                opacity: 1.0,
                wet: true,
                params: BTreeMap::new(),
                form: None,
                clip: Vec::new(),
            }),
        )
    }

    fn ask(marks: &[Painted], wet: Option<&Painted>) -> Ask {
        let key = |marks: &[Painted]| {
            let mut key = blake3::Hasher::new();
            for mark in marks {
                key.update(&mark.fingerprint);
            }
            key.finalize()
        };
        Ask {
            key: key(marks),
            marks: marks.to_vec().into(),
            wet: wet.map(|wet| (key(std::slice::from_ref(wet)), vec![wet.clone()].into())),
            size: SIZE,
        }
    }

    fn pixels(image: Option<ImageData>) -> Vec<u8> {
        image.unwrap().data.data().to_vec()
    }

    fn fresh(marks: &[Painted]) -> Vec<u8> {
        painter().paint(marks).to_vec()
    }

    /// A live view keeps the last finished picture while the next one is
    /// simulated in the background; a `look` waits for the exact one.
    #[test]
    fn a_live_view_shows_the_last_picture_until_the_next_is_painted() {
        let first = [stroke("a", 12.0, "#8a3c1e")];
        let both = [stroke("a", 12.0, "#8a3c1e"), stroke("b", 30.0, "#2a5c8e")];
        let mut settle = Settle::new(painter());
        assert_eq!(pixels(settle.show(&ask(&first, None), true)), fresh(&first));

        let shown = settle.show(&ask(&both, None), false);
        assert_eq!(pixels(shown), fresh(&first), "the window did not wait");
        assert!(settle.running() || settle.key == Some(ask(&both, None).key));

        assert_eq!(pixels(settle.show(&ask(&both, None), true)), fresh(&both));
        assert!(!settle.running());
    }

    /// A stroke just laid shows as it was previewed until its picture is
    /// painted, so it never blinks out; laid as previewed, the picture is
    /// the preview.
    #[test]
    fn a_stroke_just_laid_shows_as_its_preview_until_it_is_painted() {
        let first = [stroke("a", 12.0, "#8a3c1e")];
        let wet = stroke("b", 30.0, "#2a5c8e");
        let both = [first[0].clone(), wet.clone()];
        let mut settle = Settle::new(painter());
        let preview = pixels(settle.show(&ask(&first, Some(&wet)), true));
        assert_eq!(preview, fresh(&both), "the preview is the stroke as laid");

        // A picture of the layer alone, as a thumbnail draws it, does not
        // end the preview the window is showing.
        assert_eq!(pixels(settle.show(&ask(&first, None), true)), fresh(&first));
        assert_eq!(
            pixels(settle.show(&ask(&first, Some(&wet)), false)),
            preview
        );
        assert!(!settle.running(), "nothing new to paint");

        // Laid: the preview shows until the layer's picture is painted.
        assert_eq!(pixels(settle.show(&ask(&both, None), false)), preview);
        assert_eq!(pixels(settle.show(&ask(&both, None), true)), fresh(&both));
        assert!(settle.preview.is_none(), "the preview ends once painted");
    }
}
