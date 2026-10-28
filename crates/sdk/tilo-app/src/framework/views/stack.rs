//! A window's views as a stack: back and forward always work, and returning
//! to a view puts it back as it was left, scrolled to the same place with the
//! same selection.

/// Where a person was in a view: what a return to it restores.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewPlace {
    /// How far the view was scrolled, in logical pixels.
    pub scroll: f64,
    /// The ids of the items selected in it, in order.
    pub selection: Vec<String>,
    /// The id of the item that held the keyboard focus.
    pub focus: Option<String>,
}

/// A view the stack holds and where the person was in it.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewEntry<V> {
    pub view: V,
    pub place: ViewPlace,
}

/// The views of one window: the one shown, the ones Back returns to and the
/// ones Forward goes on to.
///
/// Every move hands in where the person is in the view being left, so the
/// stack can give it back on return. Opening a view drops what Forward held,
/// as a browser does.
#[derive(Clone, Debug)]
pub struct ViewStack<V> {
    back: Vec<ViewEntry<V>>,
    current: ViewEntry<V>,
    forward: Vec<ViewEntry<V>>,
}

impl<V: Clone + PartialEq> ViewStack<V> {
    /// A stack showing `root`, with nothing to go back or forward to.
    #[must_use]
    pub fn new(root: V) -> Self {
        Self {
            back: Vec::new(),
            current: ViewEntry {
                view: root,
                place: ViewPlace::default(),
            },
            forward: Vec::new(),
        }
    }

    /// The view shown.
    #[must_use]
    pub fn current(&self) -> &V {
        &self.current.view
    }

    /// Where the person is in the view shown, as last handed in.
    #[must_use]
    pub fn place(&self) -> &ViewPlace {
        &self.current.place
    }

    #[must_use]
    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    #[must_use]
    pub fn previous(&self) -> Option<&V> {
        self.back.last().map(|entry| &entry.view)
    }

    /// Open `view`, leaving the one shown at `left`. Opening the view
    /// already shown changes nothing. Returns whether the view changed; a new
    /// view starts at the top.
    pub fn open(&mut self, view: V, left: ViewPlace) -> bool {
        if view == self.current.view {
            return false;
        }
        self.forward.clear();
        let entry = ViewEntry {
            view,
            place: ViewPlace::default(),
        };
        let mut leaving = std::mem::replace(&mut self.current, entry);
        leaving.place = left;
        self.back.push(leaving);
        true
    }

    /// Show `view` in place of the one shown, without a step to go back
    /// through: a query view whose text changed as it was typed. Where the
    /// person was is kept.
    pub fn replace(&mut self, view: V) {
        self.current.view = view;
    }

    /// Go back, leaving the view shown at `left`. Returns the view now shown
    /// and where to put the person in it; `None` with nothing to go back to.
    pub fn back(&mut self, left: ViewPlace) -> Option<&ViewEntry<V>> {
        let entry = self.back.pop()?;
        let mut leaving = std::mem::replace(&mut self.current, entry);
        leaving.place = left;
        self.forward.push(leaving);
        Some(&self.current)
    }

    /// Go forward again, leaving the view shown at `left`.
    pub fn forward(&mut self, left: ViewPlace) -> Option<&ViewEntry<V>> {
        let entry = self.forward.pop()?;
        let mut leaving = std::mem::replace(&mut self.current, entry);
        leaving.place = left;
        self.back.push(leaving);
        Some(&self.current)
    }

    /// Hand in where the person is in the view shown, without moving.
    pub fn note_place(&mut self, place: ViewPlace) {
        self.current.place = place;
    }

    /// Forget every held view `gone` says no longer exists (a deleted
    /// playlist's page); the view shown is kept, and its caller decides.
    pub fn forget(&mut self, mut gone: impl FnMut(&V) -> bool) {
        self.back.retain(|entry| !gone(&entry.view));
        self.forward.retain(|entry| !gone(&entry.view));
    }
}
