//! A setup window as a view of the imager resource: the window answers its
//! effects itself, and what it publishes (its rows, which effects it can
//! run now) is kept as of its last sync for the view's publications, which
//! cannot reach into the window.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, EffectStatus, LiveRow, Publishes, ViewCx};
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
}

/// A window that publishes through [`Published`].
pub(crate) trait PublishingWindow: Widget + Sized + 'static {
    /// What the window shares with its view.
    fn published(&self) -> &Rc<RefCell<Published>>;

    /// The rows the window shows beneath the resource.
    fn rows(&self) -> Vec<LiveRow>;

    /// Which of its effects the window cannot run now, or names otherwise.
    fn statuses(&self) -> Vec<EffectStatus>;

    /// Keep what the window publishes now for the view's publications.
    fn publish(&self) {
        let (rows, statuses) = (self.rows(), self.statuses());
        let mut published = self.published().borrow_mut();
        published.rows = rows;
        published.statuses = statuses;
    }
}

/// `window` as a view of the resource: it answers the effects `E` with
/// `effect`, stages a capture's scenario on its next sync, and publishes
/// its rows and effect statuses as of its last sync or effect.
pub(crate) fn window_view<W: PublishingWindow, E: Effect>(
    cx: &ViewCx<Imager>,
    window: W,
    mut effect: impl FnMut(&mut W, E, &mut EffectCx<Imager>) -> EffectAnswer + 'static,
) -> Element {
    window.publish();
    let published = Rc::clone(window.published());
    cx.window_effects(
        move |window: &mut W, invoked: E, cx: &mut EffectCx<Imager>| {
            let answer = effect(window, invoked, cx);
            // Publications read what the effect changed before the next sync.
            window.publish();
            answer
        },
    )
    .status({
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
        Box::new(window),
    )
}
