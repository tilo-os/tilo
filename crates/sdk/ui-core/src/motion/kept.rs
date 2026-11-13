//! Motion that survives a rebuild: a widget built again from fresh data (a view that makes its banner
//! or its table anew on every change) picks up where the widget it replaces was, on its way or at rest.
//! A widget that moves leaves its motion state when it is dropped ([`keep`]), under its identity (its
//! id) and where it was laid out. Otherwise the next window to lay out a container of the same shape
//! (the same panel opened again) would take up their motion and draw a band leaving that it never
//! showed.

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};

use vello::kurbo::Rect;

struct Kept {
    kind: TypeId,
    id: String,
    at: Option<Rect>,
    value: Box<dyn Any>,
    stale: u8,
}

thread_local! {
    static KEPT: RefCell<Vec<Kept>> = const { RefCell::new(Vec::new()) };
    static WINDOW_SHOWN: Cell<bool> = const { Cell::new(false) };
    static CLOSING: Cell<u32> = const { Cell::new(0) };
}

/// Leave `value`, the motion state of a widget being dropped, for the widget a rebuild puts in its
/// place: the one with the same `id`, or else one laid out where it was (`at`).
pub fn keep<T: 'static>(id: impl Into<String>, at: Option<Rect>, value: T) {
    if CLOSING.try_with(Cell::get).is_ok_and(|closing| closing > 0) {
        return;
    }
    let id = id.into();
    let kind = TypeId::of::<T>();
    let _ = KEPT.try_with(|kept| {
        let Ok(mut kept) = kept.try_borrow_mut() else {
            return;
        };
        // Left again from the same place under the same name: the latest.
        // Widgets of one kind and name in different places (rows that share
        // an id) each leave theirs, taken up in the order they were left.
        kept.retain(|entry| !(entry.kind == kind && entry.id == id && entry.at == at));
        kept.push(Kept {
            kind,
            id,
            at,
            value: Box::new(value),
            stale: 0,
        });
    });
}

/// What a widget of the same kind with this `id` left when a rebuild dropped it, if anything.
#[must_use]
pub fn take_kept<T: 'static>(id: &str) -> Option<T> {
    take_where::<T>(|entry| entry.id == id)
}

/// What a widget of the same kind left when a rebuild dropped it from where this one is laid out (`at`:
/// the same top left corner and width), if anything.
#[must_use]
pub fn take_kept_at<T: 'static>(at: Rect) -> Option<T> {
    let near = |a: f64, b: f64| (a - b).abs() < 0.5;
    take_where::<T>(|entry| {
        entry.at.is_some_and(|was| {
            near(was.x0, at.x0) && near(was.y0, at.y0) && near(was.width(), at.width())
        })
    })
}

/// What a widget of `T`'s kind left that `matches`, if anything.
#[must_use]
pub fn take_kept_where<T: 'static>(matches: impl Fn(&T) -> bool) -> Option<T> {
    take_where::<T>(|entry| entry.value.downcast_ref::<T>().is_some_and(&matches))
}

fn take_where<T: 'static>(matches: impl Fn(&Kept) -> bool) -> Option<T> {
    let kind = TypeId::of::<T>();
    let entry = KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        let index = kept
            .iter()
            .position(|entry| entry.kind == kind && matches(entry))?;
        Some(kept.remove(index))
    })?;
    entry.value.downcast::<T>().ok().map(|value| *value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carry {
    Fresh,
    Placing,
    Bound,
}

/// How a widget found what a dropped one left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// By its identity: the same widget, built again from fresh data.
    Same,
    /// By its place: the widget standing where another was, which may show something else.
    Where,
}

/// The hand-on of a widget its view may build anew on every change: the widget built in its place goes
/// on from the motion this one leaves ([`keep`]), found by the identity it was given, else by its kind
/// and place.
#[derive(Debug)]
pub struct HandOn<T: 'static> {
    identity: Option<String>,
    at: Option<Rect>,
    carry: Carry,
    kind: std::marker::PhantomData<T>,
}

impl<T: 'static> Default for HandOn<T> {
    fn default() -> Self {
        Self {
            identity: None,
            at: None,
            carry: Carry::Fresh,
            kind: std::marker::PhantomData,
        }
    }
}

impl<T: 'static> HandOn<T> {
    /// At its first build, what the widget with the same `identity` left, and that it is the same
    /// widget ([`Found::Same`]); `None` waits for its place ([`Self::place`]).
    pub fn build(&mut self, identity: Option<&str>) -> Option<(T, Found)> {
        if self.carry != Carry::Fresh {
            return None;
        }
        self.identity = identity.map(str::to_owned);
        let left = identity.and_then(take_kept::<T>);
        self.carry = if left.is_some() {
            Carry::Bound
        } else {
            Carry::Placing
        };
        left.map(|left| (left, Found::Same))
    }

    /// Laid out at `at`.
    pub fn place(&mut self, at: Rect) -> Option<(T, Found)> {
        self.place_by(at, |was| {
            let near = |a: f64, b: f64| (a - b).abs() < 0.5;
            near(was.x0, at.x0) && near(was.y0, at.y0) && near(was.width(), at.width())
        })
    }

    /// [`Self::place`], for a widget whose box changes with what it shows (a badge's count): `near`
    /// says whether a predecessor laid out at a place stood where this one stands.
    pub fn place_by(&mut self, at: Rect, near: impl Fn(Rect) -> bool) -> Option<(T, Found)> {
        self.at = Some(at);
        if self.carry != Carry::Placing {
            return None;
        }
        self.carry = Carry::Bound;
        if let Some(left) = self.identity.as_deref().and_then(take_kept::<T>) {
            return Some((left, Found::Same));
        }
        take_where::<T>(|entry| entry.at.is_some_and(&near)).map(|left| (left, Found::Where))
    }

    /// The widget is dropped: leave `value` for the one a rebuild puts in its place. A widget never
    /// laid out leaves nothing.
    pub fn leave(&mut self, value: T) {
        if self.carry != Carry::Bound {
            return;
        }
        let identity = self
            .identity
            .take()
            .unwrap_or_else(|| format!("\u{1f}{:?}", self.at));
        keep(identity, self.at, value);
    }
}

/// The frame ends a left motion state waits for the widget a rebuild puts in its place.
const KEPT_FRAMES: u8 = 2;

/// A frame's layout and drawing ended: what nothing took up in the frames since it was left is let go.
pub(crate) fn frame_ended() {
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.is_empty() {
            return;
        }
        kept.retain(|entry| entry.stale < KEPT_FRAMES);
        for entry in kept.iter_mut() {
            entry.stale = entry.stale.saturating_add(1);
        }
    });
}

/// Whether the window being laid out has shown a frame before: a widget in its first frame shows at
/// rest (first paint shows the resting state; motion explains changes).
#[must_use]
pub fn window_has_shown() -> bool {
    WINDOW_SHOWN.with(Cell::get)
}

/// While the guard lives, the window being laid out has (`shown`) or has not shown a frame before
/// ([`window_has_shown`]).
#[must_use = "the answer holds only while the guard lives"]
pub fn laying_out_window(shown: bool) -> LayingOutWindow {
    LayingOutWindow(WINDOW_SHOWN.with(|cell| cell.replace(shown)))
}

/// While the guard lives, a window is dropping its widgets as it closes: they leave nothing ([`keep`]),
/// since no rebuild of theirs follows.
pub(crate) fn closing_window() -> ClosingWindow {
    CLOSING.with(|closing| closing.set(closing.get() + 1));
    ClosingWindow(())
}

/// Ends what [`closing_window`] began.
pub(crate) struct ClosingWindow(());

impl Drop for ClosingWindow {
    fn drop(&mut self) {
        let _ = CLOSING.try_with(|closing| closing.set(closing.get().saturating_sub(1)));
    }
}

/// Restores what [`laying_out_window`] replaced.
pub struct LayingOutWindow(bool);

impl Drop for LayingOutWindow {
    fn drop(&mut self) {
        WINDOW_SHOWN.with(|cell| cell.set(self.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rebuilt_widget_takes_up_what_its_predecessor_left_by_id_or_by_place() {
        keep("sync", Some(Rect::new(0.0, 10.0, 100.0, 40.0)), 7_u32);
        assert_eq!(take_kept::<u32>("other"), None);
        assert_eq!(take_kept::<u8>("sync"), None, "only a widget of its kind");
        assert_eq!(take_kept::<u32>("sync"), Some(7));
        assert_eq!(take_kept::<u32>("sync"), None, "taken once");

        keep("old title", Some(Rect::new(0.0, 10.0, 100.0, 40.0)), 8_u32);
        assert_eq!(take_kept_at::<u32>(Rect::new(0.0, 50.0, 100.0, 90.0)), None);
        assert_eq!(
            take_kept_at::<u32>(Rect::new(0.0, 10.0, 100.0, 70.0)),
            Some(8),
            "a widget in the same place, whatever it now says"
        );
    }

    #[test]
    fn a_hand_on_goes_by_identity_else_by_place_and_in_order() {
        let at = |y: f64| Rect::new(0.0, y, 100.0, y + 30.0);
        let left = |identity: Option<&str>, y: f64, value: u16| {
            let mut hand_on = HandOn::<u16>::default();
            assert_eq!(hand_on.build(identity), None);
            assert_eq!(hand_on.place(at(y)), None);
            hand_on.leave(value);
        };
        left(Some("wifi"), 0.0, 5);
        let mut next = HandOn::<u16>::default();
        assert_eq!(next.build(Some("wifi")), Some((5, Found::Same)));
        assert_eq!(next.place(at(40.0)), None, "taken once");
        // Without one, by place: another's may have stood there.
        left(None, 0.0, 1);
        left(None, 40.0, 2);
        let mut second = HandOn::<u16>::default();
        assert_eq!(second.build(None), None);
        assert_eq!(second.place(at(40.0)), Some((2, Found::Where)));
        let mut first = HandOn::<u16>::default();
        first.build(None);
        assert_eq!(first.place(at(0.0)), Some((1, Found::Where)));
        // Rows sharing an identity go on in the order they were left.
        let mut rows: Vec<_> = (0..2).map(|_| HandOn::<u16>::default()).collect();
        for (y, row) in [0.0, 40.0].into_iter().zip(&mut rows) {
            assert_eq!(row.build(Some("row")), None);
            assert_eq!(row.place(at(y)), None);
        }
        for (value, mut row) in [7, 8].into_iter().zip(rows) {
            row.leave(value);
        }
        let mut row = HandOn::<u16>::default();
        assert_eq!(row.build(Some("row")), Some((7, Found::Same)));
        let mut row = HandOn::<u16>::default();
        assert_eq!(row.build(Some("row")), Some((8, Found::Same)));
        // A widget never laid out leaves nothing.
        HandOn::<u16>::default().leave(9);
        KEPT.with(|kept| assert!(kept.borrow().is_empty()));
    }

    #[test]
    fn what_nothing_takes_up_is_let_go_after_a_few_frames() {
        keep("sync", None, 1_u32);
        frame_ended();
        assert_eq!(
            take_kept::<u32>("sync"),
            Some(1),
            "a rebuild in the next frame"
        );
        keep("sync", None, 1_u32);
        frame_ended();
        frame_ended();
        assert_eq!(
            take_kept::<u32>("sync"),
            Some(1),
            "a rebuild whose successor is laid out a frame later"
        );
        frame_ended();
        assert_eq!(take_kept::<u32>("sync"), None);
        KEPT.with(|kept| assert!(kept.borrow().is_empty()));
    }

    #[test]
    fn a_closing_window_leaves_nothing_for_the_next_one() {
        let at = Rect::new(0.0, 0.0, 100.0, 30.0);
        let mut band = HandOn::<u16>::default();
        assert_eq!(band.build(Some("section")), None);
        assert_eq!(band.place(at), None);
        {
            let _closing = closing_window();
            band.leave(4);
        }
        let mut next = HandOn::<u16>::default();
        assert_eq!(next.build(Some("section")), None);
        assert_eq!(next.place(at), None, "nor by its place");
        KEPT.with(|kept| assert!(kept.borrow().is_empty()));
    }
}
