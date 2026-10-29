//! An imager window as a view of the imager resource: the window answers
//! its effects itself, and what it publishes (its rows, which effects it
//! can run now) is kept as of its last sync for the view's publications,
//! which cannot reach into the window.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, EffectsHandle, Live, LiveRow, Publishes, ViewCx,
};
use ui::tilo_resource::Effect;
use ui::widget::{Element, Widget};

use crate::resource::Imager;

/// What a window shares with the view it stands in.
#[derive(Default)]
pub(crate) struct Published {
    rows: Vec<LiveRow>,
    statuses: Vec<EffectStatus>,
    /// A screenshot scenario to stage on the next sync.
    staged: Option<Staged>,
    /// The document the window shows, for a window that follows it, and
    /// the revision it showed last.
    document: Option<(Live<Imager>, Option<(u64, u64)>)>,
}

/// A screenshot scenario to stage, `None` for the plain window.
pub(crate) struct Staged(pub(crate) Option<String>);

impl Published {
    /// Whether a scenario waits to be staged.
    pub(crate) fn staging(&self) -> bool {
        self.staged.is_some()
    }

    /// The scenario to stage now, once.
    pub(crate) fn take_staged(&mut self) -> Option<Staged> {
        self.staged.take()
    }

    /// The document, when it changed since the window showed it last.
    fn document_changed(&mut self) -> Option<Rc<Imager>> {
        let (live, shown) = self.document.as_mut()?;
        live.changed_since(shown)
    }
}

/// A window that publishes through [`Published`].
pub(crate) trait PublishingWindow: Widget + Sized + 'static {
    /// What the window shares with its view.
    fn published(&self) -> &Rc<RefCell<Published>>;

    /// The rows the window shows beneath the resource.
    fn rows(&self) -> Vec<LiveRow> {
        Vec::new()
    }

    /// Which of its effects the window cannot run now, or names otherwise.
    fn statuses(&self) -> Vec<EffectStatus>;

    /// Show the document as it stands, for a window that follows it.
    fn present(&mut self, _state: &Imager) {}

    /// Show the document when it changed since the window last showed it.
    fn follow_document(&mut self) {
        let shown = self.published().borrow_mut().document_changed();
        if let Some(state) = shown {
            self.present(&state);
        }
    }

    /// Keep what the window publishes now for the view's publications.
    fn publish(&self) {
        let (rows, statuses) = (self.rows(), self.statuses());
        let mut published = self.published().borrow_mut();
        published.rows = rows;
        published.statuses = statuses;
    }
}

/// `window` as a view of the resource: it answers the effects `E` with
/// `effect`, stages a capture's scenario on its next sync, publishes its
/// rows and effect statuses as of its last sync or effect, and shows the
/// document now and whenever its sync finds it changed
/// ([`PublishingWindow::follow_document`]).
#[cfg(any(feature = "installation", feature = "destination"))]
pub(crate) fn window_view<W: PublishingWindow, E: Effect>(
    cx: &ViewCx<Imager>,
    window: W,
    effect: impl FnMut(&mut W, E, &mut EffectCx<Imager>) -> EffectAnswer + 'static,
) -> Element {
    wrapped_window_view(cx, window, |window| Box::new(window), effect)
}

/// [`window_view`] for a window the view shows inside `wrap`.
#[cfg(any(feature = "installation", feature = "destination"))]
pub(crate) fn wrapped_window_view<W: PublishingWindow, E: Effect>(
    cx: &ViewCx<Imager>,
    mut window: W,
    wrap: impl FnOnce(W) -> Element,
    mut effect: impl FnMut(&mut W, E, &mut EffectCx<Imager>) -> EffectAnswer + 'static,
) -> Element {
    let published = share(cx, &mut window);
    let answers = cx.window_effects(
        move |window: &mut W, invoked: E, cx: &mut EffectCx<Imager>| {
            let answer = effect(window, invoked, cx);
            // Publications read what the effect changed before the next sync.
            window.publish();
            answer
        },
    );
    publishing(cx, published, answers, wrap(window))
}

/// [`window_view`] for a window that hosts its work: what its effects
/// finish later runs on the window (`EffectCx<Imager, W>`), as do the jobs
/// it starts on [`ViewCx::window_jobs`].
pub(crate) fn hosting_window_view<W: PublishingWindow, E: Effect>(
    cx: &ViewCx<Imager>,
    mut window: W,
    mut effect: impl FnMut(&mut W, E, &mut EffectCx<Imager, W>) -> EffectAnswer + 'static,
) -> Element {
    let published = share(cx, &mut window);
    let answers = cx.window_host_effects(
        move |window: &mut W, invoked: E, cx: &mut EffectCx<Imager, W>| {
            let answer = effect(window, invoked, cx);
            window.publish();
            answer
        },
    );
    publishing(cx, published, answers, Box::new(window))
}

/// Have `window` show the document and keep what it publishes now, and
/// what it shares with its view.
fn share<W: PublishingWindow>(cx: &ViewCx<Imager>, window: &mut W) -> Rc<RefCell<Published>> {
    window.published().borrow_mut().document = Some((cx.live().clone(), None));
    window.follow_document();
    window.publish();
    Rc::clone(window.published())
}

fn publishing(
    cx: &ViewCx<Imager>,
    published: Rc<RefCell<Published>>,
    answers: EffectsHandle,
    content: Element,
) -> Element {
    answers.status({
        let published = Rc::clone(&published);
        move || published.borrow().statuses.clone()
    });
    {
        let published = Rc::clone(&published);
        cx.on_stage(move |scenario| {
            published.borrow_mut().staged = Some(Staged(scenario.map(str::to_owned)));
        });
    }
    cx.publishing(
        Publishes::new().live_rows(move || published.borrow().rows.clone()),
        content,
    )
}
