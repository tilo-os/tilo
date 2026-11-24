//! Where the shell's windows and the edges that resize them stand, for control hints: each
//! window frame ([`crate::widget::WindowFrame`]) and each surface that resizes what it holds by an
//! edge ([`crate::widget::Map`]) says so here as it lays out and draws, and the hints
//! ([`super::ControlHints`]) read it as they place a hint. The same thread-local account as
//! [`super::performed`]: whichever view draws the window, the hint over it reads one record.
//!
//! What a hint reads is in the scene's coordinates, as each part was drawn since the hints last
//! drew: the hints stand over everything else a display shows, so they draw last, and what was
//! drawn since their last draw is what this display's frame shows. A hint drawn over a scrolled
//! strip or a panned canvas finds a window where the person sees it.

use std::cell::{Cell, RefCell};

use vello::kurbo::{Affine, Line, Rect};

/// A window as a hint reads it, in the scene's coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    /// The draw pass it was last drawn in ([`pass`]).
    pass: u64,
    /// Where it was last drawn, whole, its bar, and where its bar and content show text and
    /// controls (only while a hint asked for it, [`wants_ink`]). `None` until it is drawn.
    pub(crate) drawn: Option<Drawn>,
    /// Whether it is the window in front.
    pub(crate) focused: bool,
    /// Whether the pointer rests on its bar.
    pub(crate) hovered: bool,
    /// It is moved by its bar: the bar carries what it shows.
    pub(crate) movable: bool,
}

/// Where a window was last drawn, in the scene's coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drawn {
    pub(crate) whole: Rect,
    pub(crate) bar: Rect,
    pub(crate) ink: Vec<Rect>,
}

/// One surface's edges that resize what it holds, in the scene's coordinates.
#[derive(Clone, Debug, Default)]
struct Edges {
    pass: u64,
    lines: Vec<Line>,
}

thread_local! {
    static NEXT: Cell<u64> = const { Cell::new(1) };
    static WINDOWS: RefCell<Vec<(u64, Window)>> = const { RefCell::new(Vec::new()) };
    static EDGES: RefCell<Vec<(u64, Edges)>> = const { RefCell::new(Vec::new()) };
    static INK: Cell<bool> = const { Cell::new(false) };
    /// The draw pass: what was drawn since the hints last drew.
    static PASS: Cell<u64> = const { Cell::new(1) };
    /// How many windows were drawn this pass, so a surface that draws them knows it holds some.
    static DRAWN: Cell<u64> = const { Cell::new(0) };
    /// Whether windows lie out of view this pass, before and after what shows.
    static BEYOND: Cell<(bool, bool)> = const { Cell::new((false, false)) };
}

fn pass() -> u64 {
    PASS.with(Cell::get)
}

/// The hints drew: what is drawn next is the next pass.
pub(crate) fn next_pass() {
    PASS.with(|pass| pass.set(pass.get().wrapping_add(1)));
    BEYOND.with(|beyond| beyond.set((false, false)));
}

/// How many windows were drawn so far: a surface reads it before and after drawing what it holds,
/// to tell whether it holds windows.
#[must_use]
pub fn windows_drawn() -> u64 {
    DRAWN.with(Cell::get)
}

/// Windows lie out of view before (`before`) or after (`after`) what a surface shows of them: a
/// strip scrolled along its windows, a canvas panned away from some.
pub fn windows_beyond(before: bool, after: bool) {
    BEYOND.with(|beyond| {
        let (b, a) = beyond.get();
        beyond.set((b || before, a || after));
    });
}

/// Whether windows lie out of view this pass, before and after what shows.
pub(crate) fn beyond() -> (bool, bool) {
    BEYOND.with(Cell::get)
}

fn next() -> u64 {
    NEXT.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1));
        id
    })
}

/// Whether a hint may show soon, so windows read where they show text and controls as they draw.
#[must_use]
pub fn wants_ink() -> bool {
    INK.with(Cell::get)
}

pub(crate) fn want_ink(wanted: bool) {
    INK.with(|ink| ink.set(wanted));
}

/// One window's place in the record, for as long as the window lives.
#[derive(Debug)]
pub struct WindowAnchor(u64);

impl Default for WindowAnchor {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowAnchor {
    #[must_use]
    pub fn new() -> Self {
        let id = next();
        WINDOWS.with(|windows| windows.borrow_mut().push((id, Window::default())));
        Self(id)
    }

    fn edit(&self, change: impl FnOnce(&mut Window)) {
        WINDOWS.with(|windows| {
            if let Some((_, window)) = windows
                .borrow_mut()
                .iter_mut()
                .find(|(id, _)| *id == self.0)
            {
                change(window);
            }
        });
    }

    /// What the window is now: in front, the pointer on its bar, moved by its bar.
    pub fn state(&self, focused: bool, hovered: bool, movable: bool) {
        self.edit(|window| {
            window.focused = focused;
            window.hovered = hovered;
            window.movable = movable;
        });
    }

    /// The window was drawn through `transform`, whole at `whole` and its bar at `bar` in its
    /// layout's coordinates; `ink` where it shows text and controls there, when it read them.
    pub fn drawn(&self, transform: Affine, whole: Rect, bar: Rect, ink: Option<Vec<Rect>>) {
        DRAWN.with(|drawn| drawn.set(drawn.get().wrapping_add(1)));
        let pass = pass();
        self.edit(|window| {
            window.pass = pass;
            let kept = window
                .drawn
                .take()
                .map(|drawn| drawn.ink)
                .unwrap_or_default();
            window.drawn = Some(Drawn {
                whole: transform.transform_rect_bbox(whole),
                bar: transform.transform_rect_bbox(bar),
                ink: ink.map_or(kept, |ink| {
                    ink.into_iter()
                        .map(|rect| transform.transform_rect_bbox(rect))
                        .collect()
                }),
            });
        });
    }
}

impl Drop for WindowAnchor {
    fn drop(&mut self) {
        // A window torn down while the thread's record is going too (a test's end) has nothing
        // left to take out.
        let _ = WINDOWS.try_with(|windows| windows.borrow_mut().retain(|(id, _)| *id != self.0));
    }
}

/// One surface's resizing edges in the record, for as long as the surface lives.
#[derive(Debug)]
pub struct EdgesAnchor(u64);

impl Default for EdgesAnchor {
    fn default() -> Self {
        Self::new()
    }
}

impl EdgesAnchor {
    #[must_use]
    pub fn new() -> Self {
        let id = next();
        EDGES.with(|edges| edges.borrow_mut().push((id, Edges::default())));
        Self(id)
    }

    /// The surface was drawn through `transform` with these edges in its layout's coordinates:
    /// each a line a person drags to resize what stands beside it.
    pub fn drawn(&self, transform: Affine, lines: impl IntoIterator<Item = Line>) {
        let lines: Vec<Line> = lines.into_iter().map(|line| transform * line).collect();
        let pass = pass();
        EDGES.with(|edges| {
            if let Some((_, edges)) = edges.borrow_mut().iter_mut().find(|(id, _)| *id == self.0) {
                edges.pass = pass;
                edges.lines = lines;
            }
        });
    }
}

impl Drop for EdgesAnchor {
    fn drop(&mut self) {
        let _ = EDGES.try_with(|edges| edges.borrow_mut().retain(|(id, _)| *id != self.0));
    }
}

/// Every window drawn this pass.
pub(crate) fn windows() -> Vec<Window> {
    let pass = pass();
    WINDOWS.with(|windows| {
        windows
            .borrow()
            .iter()
            .filter(|(_, window)| window.pass == pass && window.drawn.is_some())
            .map(|(_, window)| window.clone())
            .collect()
    })
}

/// Every resizing edge drawn this pass, in the scene's coordinates.
pub(crate) fn edges() -> Vec<Line> {
    let pass = pass();
    EDGES.with(|edges| {
        edges
            .borrow()
            .iter()
            .filter(|(_, edges)| edges.pass == pass)
            .flat_map(|(_, edges)| edges.lines.iter().copied())
            .collect()
    })
}
