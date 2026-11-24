//! What is being dragged, across the widget trees on one thread: the widgets of one window, or
//! the trees of a desktop that each stand on a display of their own.
//!
//! Every tree reads and writes the bus in its own coordinates. The bus keeps a drag in the
//! coordinates the trees share (a desktop's logical pixels), and [`placed_at`] says where the tree
//! that runs now stands in them: a tree on a second display sees a drag over it where it is, and a
//! tree elsewhere does not see it at all.

use std::cell::{Cell, RefCell};

use vello::kurbo::{Point, Rect, Vec2};

/// What a drag in flight is.
#[derive(Clone, Debug)]
pub struct InFlight {
    pub source: String,
    pub payload: crate::dnd::DragPayload,
    pub label: String,
    pub pointer: Point,
    pub runs: Vec<(String, Rect)>,
}

/// A drag that was let go outside its own run, waiting for the run it landed in to take it.
#[derive(Clone, Debug)]
pub struct Landed {
    pub source: String,
    pub payload: crate::dnd::DragPayload,
    pub pointer: Point,
}

/// A drag of device files a source here handed to the desktop: what it carries on this bus, and
/// the files the desktop carries for it. When those files are dropped back on a tree of this
/// thread, the drop is the source's own drag, not files from another app.
#[derive(Clone, Debug)]
struct Offered {
    source: String,
    payload: crate::dnd::DragPayload,
    label: String,
    /// The files not yet dropped back: the desktop hands a drop of several over one by one.
    files: Vec<std::path::PathBuf>,
    /// Whether one of them came back already: the first lands the drag, the rest go with it.
    landed: bool,
}

thread_local! {
    static IN_FLIGHT: RefCell<Option<InFlight>> = const { RefCell::new(None) };
    static LANDED: RefCell<Option<Landed>> = const { RefCell::new(None) };
    static CARRIED_AWAY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REVISION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static ORIGIN: Cell<Vec2> = const { Cell::new(Vec2::ZERO) };
    static OFFERED: RefCell<Option<Offered>> = const { RefCell::new(None) };
}

/// Where the tree that runs now stands, until it is dropped.
#[must_use = "the tree stands there only while this is held"]
pub struct Placed {
    before: Vec2,
}

impl Drop for Placed {
    fn drop(&mut self) {
        ORIGIN.with(|origin| origin.set(self.before));
    }
}

/// The tree that runs while the answer is held stands at `origin` in the coordinates the trees on
/// this thread share: what it writes to the bus is moved by it, what it reads moved back. Outside
/// every tree the bus is read and written in the shared coordinates themselves.
pub fn placed_at(origin: Point) -> Placed {
    Placed {
        before: ORIGIN.with(|placed| placed.replace(origin.to_vec2())),
    }
}

fn origin() -> Vec2 {
    ORIGIN.with(Cell::get)
}

/// Something in flight came, moved, went or landed.
fn changed() {
    REVISION.with(|revision| revision.set(revision.get().wrapping_add(1)));
}

/// Moves with every change to what is in flight or landed: a tree that shows where a drag would
/// land runs again when this moved, even when no input of its own came with the change (a drag
/// carried here from another of the person's devices, or one another tree on this thread holds).
#[must_use]
pub fn revision() -> u64 {
    REVISION.with(std::cell::Cell::get)
}

/// `source` lifted the resource `address` over `rect` : a window's bar, a view a person picks up.
pub fn carry(source: String, address: Vec<u8>, label: String, pointer: Point, rect: Rect) {
    carry_payload(
        source,
        crate::dnd::DragPayload::Custom {
            kind: crate::widget::drag_source::RESOURCE_PAYLOAD.to_owned(),
            blob: address,
        },
        label,
        pointer,
        rect,
    );
}

/// `source` lifted `payload` over `rect`: a resource, or device files a view stands for.
pub fn carry_payload(
    source: String,
    payload: crate::dnd::DragPayload,
    label: String,
    pointer: Point,
    rect: Rect,
) {
    publish(InFlight {
        source: source.clone(),
        payload,
        label,
        pointer,
        runs: vec![(source, rect)],
    });
}

/// `run` lifted an item: everything else in the window may take it now.
pub fn publish(mut in_flight: InFlight) {
    let origin = origin();
    in_flight.pointer += origin;
    for run in &mut in_flight.runs {
        run.1 = run.1 + origin;
    }
    IN_FLIGHT.with(|slot| *slot.borrow_mut() = Some(in_flight));
    changed();
}

/// The pointer moved while something is in flight: what every run needs to show where it
/// would land.
pub fn moved(pointer: Point, run: (String, Rect)) {
    let origin = origin();
    let (pointer, run) = (pointer + origin, (run.0, run.1 + origin));
    IN_FLIGHT.with(|slot| {
        if let Some(in_flight) = slot.borrow_mut().as_mut() {
            in_flight.pointer = pointer;
            match in_flight.runs.iter_mut().find(|(key, _)| *key == run.0) {
                Some(entry) => entry.1 = run.1,
                None => in_flight.runs.push(run),
            }
            changed();
        }
    });
}

/// What is in flight now, if anything.
#[must_use]
pub fn current() -> Option<InFlight> {
    let origin = origin();
    let mut in_flight = IN_FLIGHT.with(|slot| slot.borrow().clone())?;
    in_flight.pointer -= origin;
    for run in &mut in_flight.runs {
        run.1 = run.1 - origin;
    }
    Some(in_flight)
}

/// The drag ended: `None` for a cancel, or where it was let go for a drop another run
/// takes. A drag carried away lands nowhere here, wherever it is let go.
pub fn end(landed: Option<Landed>) {
    let origin = origin();
    let landed = landed
        .filter(|_| !CARRIED_AWAY.with(std::cell::Cell::take))
        .map(|landed| Landed {
            pointer: landed.pointer + origin,
            ..landed
        });
    IN_FLIGHT.with(|slot| *slot.borrow_mut() = None);
    LANDED.with(|slot| *slot.borrow_mut() = landed);
    changed();
}

/// What is in flight went on somewhere else (to another of the person's devices): nothing here
/// shows it any more, and the release that ends it here drops nothing.
pub fn carry_away() {
    if IN_FLIGHT.with(|slot| slot.borrow_mut().take()).is_some() {
        CARRIED_AWAY.with(|carried| carried.set(true));
        changed();
    }
}

/// `source`'s drag of `files` went to the desktop, which carries it over every app: on this bus it
/// is `payload`, named `label`. Should the files come back to a tree here, they
/// land as that drag ([`came_back`]); a drag offered since replaces it.
pub fn offer(
    source: String,
    payload: crate::dnd::DragPayload,
    label: String,
    files: Vec<std::path::PathBuf>,
) {
    OFFERED.with(|slot| {
        *slot.borrow_mut() = Some(Offered {
            source,
            payload,
            label,
            files,
            landed: false,
        });
    });
}

/// What the drag a source here handed to the desktop is called, while one is out: what a target
/// it hovers says it takes.
#[must_use]
pub fn offered() -> Option<String> {
    OFFERED.with(|slot| slot.borrow().as_ref().map(|offered| offered.label.clone()))
}

/// The desktop dropped `path` on this tree at `pointer`: whether it is one of the files a source
/// here offered. The first to come back lands that source's drag at `pointer`, for the target
/// there to take as any drag of this bus; the rest of the same drag come back with it and land
/// nothing more. A file no source here offered is another app's, and is left to whatever takes
/// those.
pub fn came_back(path: &std::path::Path, pointer: Point) -> bool {
    returned(path, Some(pointer))
}

/// The desktop dropped `path` on this tree where no target took it: whether it is one of the
/// files a source here offered, which then lands nowhere. A window lets a drag of its own go
/// where nothing takes it, rather than take its own files in again as another app's.
pub fn came_back_nowhere(path: &std::path::Path) -> bool {
    returned(path, None)
}

fn returned(path: &std::path::Path, pointer: Option<Point>) -> bool {
    let landing = OFFERED.with(|slot| {
        let mut slot = slot.borrow_mut();
        let offered = slot.as_mut()?;
        let at = offered.files.iter().position(|file| file == path)?;
        offered.files.remove(at);
        let first = !std::mem::replace(&mut offered.landed, true);
        let landing = pointer.filter(|_| first).map(|pointer| Landed {
            source: offered.source.clone(),
            payload: offered.payload.clone(),
            pointer,
        });
        if offered.files.is_empty() {
            *slot = None;
        }
        Some(landing)
    });
    match landing {
        None => false,
        Some(landing) => {
            if let Some(landed) = landing {
                end(Some(landed));
            }
            true
        }
    }
}

/// The drag a source here handed to the desktop is over: it was dropped in another app, or the
/// person pressed again here. Files dropped later are another app's, even the same ones.
pub fn forget_offer() {
    OFFERED.with(|slot| slot.borrow_mut().take());
}

/// The drop the last drag left, if any, as [`take`] would test it, without taking it: what a
/// target reads to decide whether the drop is its own before it takes the one landing there is.
#[must_use]
pub fn landed() -> Option<Landed> {
    let origin = origin();
    LANDED.with(|slot| {
        slot.borrow().as_ref().map(|landed| Landed {
            pointer: landed.pointer - origin,
            ..landed.clone()
        })
    })
}

/// Take the drop `run` takes, if the last drag ended inside its `extent` and came from
/// somewhere else.
pub fn take(run: &str, extent: Rect) -> Option<Landed> {
    let origin = origin();
    LANDED.with(|slot| {
        let mut slot = slot.borrow_mut();
        let landed = slot.as_ref()?;
        if landed.source == run || !extent.contains(landed.pointer - origin) {
            return None;
        }
        slot.take().map(|landed| Landed {
            pointer: landed.pointer - origin,
            ..landed
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flight(pointer: Point) -> InFlight {
        InFlight {
            source: "elsewhere".into(),
            payload: crate::dnd::DragPayload::Custom {
                kind: "resource".into(),
                blob: b"notes.page:one".to_vec(),
            },
            label: "Notes".into(),
            pointer,
            runs: Vec::new(),
        }
    }

    /// Two trees on two displays side by side, the second one 1000 pixels to the right: a drag
    /// over the second display is over the second tree, where it is on that display, and lands
    /// there.
    #[test]
    fn a_tree_on_a_second_display_sees_a_drag_where_it_is_on_that_display() {
        let first = Rect::new(0.0, 0.0, 1000.0, 700.0);
        let second = Rect::new(0.0, 0.0, 1000.0, 700.0);
        let on_second = Point::new(1100.0, 30.0);
        // Published outside every tree, in the desktop's own coordinates.
        publish(flight(on_second));
        let seen = |at: Point| {
            let _placed = placed_at(at);
            current().map(|flight| flight.pointer)
        };
        assert_eq!(seen(Point::ZERO), Some(on_second));
        assert!(!first.contains(on_second), "nothing over the first tree");
        assert_eq!(
            seen(Point::new(1000.0, 0.0)),
            Some(Point::new(100.0, 30.0)),
            "the second tree sees it over itself"
        );
        assert!(second.contains(Point::new(100.0, 30.0)));

        // A drag the second tree moves is where that tree says, for the first tree too.
        {
            let _placed = placed_at(Point::new(1000.0, 0.0));
            moved(Point::new(200.0, 40.0), ("second".into(), Rect::ZERO));
        }
        assert_eq!(
            current().map(|flight| flight.pointer),
            Some(Point::new(1200.0, 40.0))
        );

        end(Some(Landed {
            source: "elsewhere".into(),
            payload: flight(on_second).payload,
            pointer: on_second,
        }));
        {
            let _placed = placed_at(Point::ZERO);
            assert!(
                take("first", first).is_none(),
                "the first tree takes nothing"
            );
        }
        let _placed = placed_at(Point::new(1000.0, 0.0));
        assert_eq!(
            take("second", second).map(|landed| landed.pointer),
            Some(Point::new(100.0, 30.0)),
            "the second tree takes it where it was let go"
        );
    }
}
