//! The root a test mounts ([`FrameHarness::for_root`]), read and changed
//! between frames: changed through a guard that, let go of, takes what the
//! widgets report changed, as the live host does after an event.

use super::*;

/// A [`FrameHarness`] over one concrete root that reads as that root.
///
/// Dereferencing yields the widget; a mutable dereference makes the next
/// frame recompose from scratch: a plain reference runs nothing when it is
/// let go of, so the harness cannot take what the widgets report changed
/// then, as [`FrameHarness::root_mut`] does. Harness
/// operations are associated functions so they never shadow the root's own
/// methods.
pub struct Framed<W: Widget + 'static> {
    harness: FrameHarness,
    root: std::marker::PhantomData<fn() -> W>,
}

impl<W: Widget + 'static> Framed<W> {
    /// Mount `root` over `state` and run its first frame at `viewport`.
    pub fn new(state: State, root: W, viewport: (u32, u32), theme: Theme) -> Self {
        Self::from_harness(FrameHarness::for_root(state, root, viewport, theme))
    }

    /// [`Self::new`] with every construction option spelled out.
    pub fn with_options(
        state: State,
        root: W,
        viewport: (u32, u32),
        options: FrameOptions,
    ) -> Self {
        Self::from_harness(FrameHarness::for_root_with(state, root, viewport, options))
    }

    fn from_harness(harness: FrameHarness) -> Self {
        let _: &W = harness.root();
        Self {
            harness,
            root: std::marker::PhantomData,
        }
    }

    pub fn harness(this: &Self) -> &FrameHarness {
        &this.harness
    }

    pub fn harness_mut(this: &mut Self) -> &mut FrameHarness {
        &mut this.harness
    }

    /// The root and the live document at once; the next frame recomposes
    /// from scratch, as after a mutable dereference.
    pub fn split(this: &mut Self) -> (&mut W, &mut State) {
        this.harness.root_and_state_recomposed()
    }
}

impl<W: Widget + 'static> std::ops::Deref for Framed<W> {
    type Target = W;

    fn deref(&self) -> &W {
        self.harness.root()
    }
}

impl<W: Widget + 'static> std::ops::DerefMut for Framed<W> {
    fn deref_mut(&mut self) -> &mut W {
        self.harness.root_recomposed()
    }
}

/// The root of a [`FrameHarness`], to change ([`FrameHarness::root_mut`]).
/// Letting go of it takes what the widgets report changed, as the live host
/// does after each event.
pub struct RootMut<'a, W: Widget + 'static> {
    harness: &'a mut FrameHarness,
    root: std::marker::PhantomData<fn() -> W>,
}

impl<W: Widget + 'static> std::ops::Deref for RootMut<'_, W> {
    type Target = W;

    fn deref(&self) -> &W {
        self.harness.root()
    }
}

impl<W: Widget + 'static> std::ops::DerefMut for RootMut<'_, W> {
    fn deref_mut(&mut self) -> &mut W {
        // SAFETY: `root_mut` checked the type, and `drop` checks the maps.
        unsafe { self.harness.lent_root() }
    }
}

impl<W: Widget + 'static> RootMut<'_, W> {
    /// Put `root` in the root's place, as a view that builds its widget
    /// anew does: the replacement is reported, as its parent reports it to
    /// the live host, and the next frame builds the tree again.
    pub fn replace(self, root: W) {
        // SAFETY: `root_mut` checked the type, and `drop` checks the maps.
        *unsafe { self.harness.lent_root::<W>() } = root;
        self.harness.composition_replaced = true;
    }
}

impl<W: Widget + 'static> Drop for RootMut<'_, W> {
    fn drop(&mut self) {
        self.harness.forget_replaced_widgets();
    }
}

/// The root of a [`FrameHarness`] and its live document, to change
/// ([`FrameHarness::root_and_state_mut`]); let go of as a [`RootMut`].
pub struct RootAndStateMut<'a, W: Widget + 'static> {
    harness: &'a mut FrameHarness,
    root: std::marker::PhantomData<fn() -> W>,
}

impl<W: Widget + 'static> RootAndStateMut<'_, W> {
    /// The root and the document, at once.
    pub fn split(&mut self) -> (&mut W, &mut State) {
        // SAFETY: `root_and_state_mut` checked the type, and `drop` checks
        // the maps.
        let harness = &mut *self.harness;
        let root =
            unsafe { &mut *std::ptr::from_mut::<dyn Widget>(harness.root.as_mut()).cast::<W>() };
        (root, &mut harness.state)
    }
}

impl<W: Widget + 'static> Drop for RootAndStateMut<'_, W> {
    fn drop(&mut self) {
        self.harness.forget_replaced_widgets();
    }
}

impl FrameHarness {
    /// Mount one concrete widget as the root so a test can read and adjust
    /// it through [`Self::root`] and [`Self::root_mut`] between frames.
    pub fn for_root<W: Widget + 'static>(
        state: State,
        root: W,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Self {
        Self::for_root_with(
            state,
            root,
            viewport,
            FrameOptions {
                theme: Some(theme),
                ..FrameOptions::default()
            },
        )
    }

    /// [`Self::for_root`] for an app: its window's bar
    /// ([`Self::present_in_window_bar`]) wears the app's icon and title, as
    /// a desktop window's tab does.
    pub fn for_app<W: crate::widget::AppWidget + 'static>(
        state: State,
        root: W,
        viewport: (u32, u32),
        theme: Theme,
    ) -> Self {
        let app = (root.app_kind(), root.app_title().to_owned());
        let mut harness = Self::for_root(state, root, viewport, theme);
        harness.window_app = Some(app);
        harness
    }

    /// [`Self::for_root`] with every construction option spelled out.
    pub fn for_root_with<W: Widget + 'static>(
        state: State,
        root: W,
        viewport: (u32, u32),
        options: FrameOptions,
    ) -> Self {
        let FrameOptions {
            theme,
            text,
            host_style,
            content_height,
        } = options;
        let mut harness = Self::with_inputs_styled(
            state,
            Box::new(root),
            viewport,
            theme.unwrap_or_else(Theme::dark),
            text.unwrap_or_default(),
            HostLayout {
                style: host_style.unwrap_or_default(),
                content_height,
            },
        );
        harness.root_type = Some(std::any::TypeId::of::<W>());
        harness
    }

    /// The root mounted by [`Self::for_root`].
    ///
    /// # Panics
    /// When the root was not mounted through [`Self::for_root`] as a `W`.
    #[must_use]
    pub fn root<W: Widget + 'static>(&self) -> &W {
        assert_eq!(
            self.root_type,
            Some(std::any::TypeId::of::<W>()),
            "FrameHarness::root: the root was not mounted as this type"
        );
        // SAFETY: `root_type` records the concrete type `root` was boxed from
        // and is cleared whenever `root` is replaced.
        unsafe { &*std::ptr::from_ref::<dyn Widget>(self.root.as_ref()).cast::<W>() }
    }

    /// Mutable access to the root mounted by [`Self::for_root`]. When it is
    /// let go of, the harness takes what the widgets report changed, as the
    /// live host does after each event: a change they carry out in place is
    /// remounted by the next frame, any other change (or a widget that
    /// replaced children without saying so) lets go of the layout tree, and
    /// the next frame rebuilds it. A change the widgets do not report keeps
    /// the tree, as it would live.
    ///
    /// # Panics
    /// When the root was not mounted through [`Self::for_root`] as a `W`.
    pub fn root_mut<W: Widget + 'static>(&mut self) -> RootMut<'_, W> {
        self.invalidate_local();
        self.lend_root::<W>("FrameHarness::root_mut");
        RootMut {
            harness: self,
            root: std::marker::PhantomData,
        }
    }

    /// The root mounted by [`Self::for_root`] together with the live
    /// document, for calls that take both ([`RootAndStateMut::split`]).
    /// Letting go of them is as for [`Self::root_mut`].
    ///
    /// # Panics
    /// When the root was not mounted through [`Self::for_root`] as a `W`.
    pub fn root_and_state_mut<W: Widget + 'static>(&mut self) -> RootAndStateMut<'_, W> {
        self.invalidate_local();
        self.lend_root::<W>("FrameHarness::root_and_state_mut");
        RootAndStateMut {
            harness: self,
            root: std::marker::PhantomData,
        }
    }

    fn lend_root<W: Widget + 'static>(&self, caller: &str) {
        assert_eq!(
            self.root_type,
            Some(std::any::TypeId::of::<W>()),
            "{caller}: the root was not mounted as this type"
        );
    }

    /// The root as a `W`, to change, while its layout maps stay: they are
    /// checked when it is let go of ([`Self::forget_replaced_widgets`]),
    /// before anything calls through them.
    ///
    /// # Safety
    /// `root_type` is `W` ([`Self::lend_root`]), and the caller runs
    /// [`Self::forget_replaced_widgets`] once the borrow ends.
    unsafe fn lent_root<W: Widget + 'static>(&mut self) -> &mut W {
        // SAFETY: as in `root`.
        unsafe { &mut *std::ptr::from_mut::<dyn Widget>(self.root.as_mut()).cast::<W>() }
    }

    /// What the live host does after each event: the widgets' report of
    /// what changed is taken before anything calls through the layout maps.
    /// A change carried out in place waits for the next frame's remount;
    /// any other change, or a widget that replaced children without saying
    /// so, lets go of the maps, and the next frame rebuilds the tree.
    fn forget_replaced_widgets(&mut self) {
        if self.take_composition_dirty() {
            self.discard_layout_tree();
        }
    }

    /// [`Self::root_and_state_mut`] for [`Framed::split`], which hands out
    /// plain references: nothing runs when they are let go of, so the next
    /// frame recomposes from scratch.
    fn root_and_state_recomposed<W: Widget + 'static>(&mut self) -> (&mut W, &mut State) {
        self.invalidate_local();
        self.lend_root::<W>("Framed::split");
        self.discard_layout_tree();
        // SAFETY: as in `root`; the raw layout maps that pointed into this
        // tree were discarded above.
        let root =
            unsafe { &mut *std::ptr::from_mut::<dyn Widget>(self.root.as_mut()).cast::<W>() };
        (root, &mut self.state)
    }

    /// [`Framed`]'s mutable dereference, which hands out a plain reference:
    /// nothing runs when it is let go of, so the next frame recomposes from
    /// scratch.
    fn root_recomposed<W: Widget + 'static>(&mut self) -> &mut W {
        self.root_and_state_recomposed::<W>().0
    }
}
