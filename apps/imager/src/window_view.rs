//! What a window shares with the view it stands in, and the effects it cannot run.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, EffectsHandle, Live, LiveRow, Publishes, ViewCx,
};
use ui::tilo_resource::Effect;
use ui::widget::{Element, Widget};

use crate::resource::Imager;

#[derive(Default)]
pub(crate) struct Published {
    rows: Vec<LiveRow>,
    statuses: Vec<EffectStatus>,
    staged: Option<Staged>,
    document: Option<(Live<Imager>, Option<(u64, u64)>)>,
}

pub(crate) struct Staged(pub(crate) Option<String>);

impl Published {
    pub(crate) fn staging(&self) -> bool {
        self.staged.is_some()
    }

    pub(crate) fn take_staged(&mut self) -> Option<Staged> {
        self.staged.take()
    }

    fn document_changed(&mut self) -> Option<Rc<Imager>> {
        let (live, shown) = self.document.as_mut()?;
        live.changed_since(shown)
    }
}

pub(crate) trait PublishingWindow: Widget + Sized + 'static {
    fn published(&self) -> &Rc<RefCell<Published>>;

    fn rows(&self) -> Vec<LiveRow> {
        Vec::new()
    }

    fn statuses(&self) -> Vec<EffectStatus>;

    fn present(&mut self, _state: &Imager) {}

    fn follow_document(&mut self) {
        let shown = self.published().borrow_mut().document_changed();
        if let Some(state) = shown {
            self.present(&state);
        }
    }

    fn publish(&self) {
        let (rows, statuses) = (self.rows(), self.statuses());
        let mut published = self.published().borrow_mut();
        published.rows = rows;
        published.statuses = statuses;
    }
}

#[cfg(any(feature = "installation", feature = "destination"))]
pub(crate) fn window_view<W: PublishingWindow, E: Effect>(
    cx: &ViewCx<Imager>,
    window: W,
    effect: impl FnMut(&mut W, E, &mut EffectCx<Imager>) -> EffectAnswer + 'static,
) -> Element {
    wrapped_window_view(cx, window, |window| Box::new(window), effect)
}

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
            window.publish();
            answer
        },
    );
    publishing(cx, published, answers, wrap(window))
}

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
