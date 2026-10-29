//! The editor as a bound view: the [`Store`] the window shares with the
//! view's effect handler and what the view publishes, and [`window`], which
//! takes the window into the view.
//!
//! Every effect is something the window does: the handler leaves it in the
//! store, and the window does it when it next looks. A file effect answers
//! its caller once its read, its write or its chooser settles.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow, Publishes, ViewCx};
use ui::widget::Element;

use super::{ResourceIo, TypstApp};
use crate::actions::TypstEffect;
use crate::resource::Workspace;

/// What the window and the view's effect handler share, and what the view
/// publishes beside the workspace.
#[derive(Default)]
pub(crate) struct Store {
    /// Effects the window does when it next looks.
    pub(super) asks: Vec<TypstEffect>,
    /// The answer owed to the file effect still running, given when its
    /// chooser or its write settles.
    pub(super) file_reply: Option<EffectReply>,
    /// A file effect's read or write, done on the next sync.
    pub(super) pending_resource_io: Option<ResourceIo>,
    /// The Problems rows and preview pages the window shows.
    pub(super) rows: Vec<LiveRow>,
    /// The scenario a capture staged, staged by the window when it next
    /// looks.
    pub(super) staged: Option<Staged>,
}

/// A capture's scenario (`None` for a plain capture).
pub(super) struct Staged(pub(super) Option<String>);

/// The [`Store`] as the window and the view share it.
pub(crate) type Shared = Rc<RefCell<Store>>;

impl Store {
    /// Whether the window has something to do that it was asked to.
    pub(super) fn waiting(&self) -> bool {
        !self.asks.is_empty() || self.pending_resource_io.is_some() || self.staged.is_some()
    }

    /// Take `effect` for the window: a file read or write with a path waits
    /// for the next sync, one without asks the file chooser; the rest are
    /// done when the window next looks.
    fn answer(&mut self, effect: TypstEffect, cx: &mut EffectCx<Workspace>) -> EffectAnswer {
        let file = matches!(
            effect,
            TypstEffect::OpenFile { .. }
                | TypstEffect::Save { .. }
                | TypstEffect::SaveAs
                | TypstEffect::ExportPdf { .. }
        );
        if !file {
            self.asks.push(effect);
            return Ok(Effected::done());
        }
        if self.file_reply.is_some() || self.pending_resource_io.is_some() {
            return Err("another file operation is already running".into());
        }
        match effect {
            TypstEffect::OpenFile { path: Some(path) } => {
                self.pending_resource_io = Some(ResourceIo::Open(path.into()));
            }
            TypstEffect::Save { path: Some(path) } => {
                self.pending_resource_io = Some(ResourceIo::Save(path.into()));
            }
            TypstEffect::ExportPdf { path: Some(path) } => {
                self.pending_resource_io = Some(ResourceIo::ExportPdf(path.into()));
            }
            other => self.asks.push(other),
        }
        self.file_reply = Some(cx.reply());
        Ok(Effected::Pending)
    }

    /// The live row `id`: a Problems row, a preview page or a heading, or
    /// one of their children.
    fn row(&self, id: &str) -> Option<LiveRow> {
        self.rows
            .iter()
            .cloned()
            .flat_map(|row| {
                let resource = row.into_resource();
                std::iter::once(resource.clone()).chain(resource.children)
            })
            .find(|resource| resource.id.as_str() == id)
            .map(LiveRow::from_resource)
    }
}

/// The window as the view lays it out: it shows the workspace the document
/// holds, does what the effects ask and publishes its Problems rows and
/// preview pages.
pub(crate) fn window(cx: &ViewCx<Workspace>) -> Element {
    let mut window = TypstApp::new();
    window.attach(cx);
    let store = Rc::clone(&window.store);
    {
        let store = Rc::clone(&store);
        cx.effects(move |effect: TypstEffect, cx| store.borrow_mut().answer(effect, cx));
    }
    {
        let store = Rc::clone(&store);
        cx.on_stage(move |scenario| {
            store.borrow_mut().staged = Some(Staged(scenario.map(str::to_owned)));
        });
    }
    let rows = Rc::clone(&store);
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows.borrow().rows.clone())
            .live_row(move |id| store.borrow().row(id)),
        Box::new(window),
    )
}
