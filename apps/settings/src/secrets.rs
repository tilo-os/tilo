//! The one password prompt in Settings: a pane asks for a secret here.
//! ([`SecretAsks::ask`]) and the window shows it as the shared prompt.
//! (`ui::widget::SecretPrompt`) in a sheet over the pane, never as a field.

use std::cell::RefCell;
use std::rc::Rc;

use ui::widget::{ModalDialog, PromptOutcome, SecretPrompt, Widget};

pub(crate) enum Ask {
    Open(String, Box<SecretPrompt>),
    Reject {
        key: String,
        field: usize,
        message: String,
    },
    Busy(String, Option<String>),
    Close(String),
}

#[derive(Default)]
struct Queue {
    asks: Vec<Ask>,
    answers: Vec<(String, PromptOutcome)>,
    open: Option<String>,
}

#[derive(Clone, Default)]
pub struct SecretAsks(Rc<RefCell<Queue>>);

impl std::fmt::Debug for SecretAsks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretAsks")
            .field("open", &self.0.borrow().open)
            .finish_non_exhaustive()
    }
}

impl SecretAsks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ask(&self, key: impl Into<String>, prompt: SecretPrompt) {
        let key = key.into();
        let mut queue = self.0.borrow_mut();
        if let Some(open) = queue.open.take()
            && open != key
        {
            queue.answers.push((open, PromptOutcome::Cancel));
        }
        queue.open = Some(key.clone());
        queue.asks.push(Ask::Open(key, Box::new(prompt)));
    }

    #[must_use]
    pub fn is_asking(&self, key: &str) -> bool {
        self.0.borrow().open.as_deref() == Some(key)
    }

    pub fn take_answer(&self, key: &str) -> Option<PromptOutcome> {
        let mut queue = self.0.borrow_mut();
        let at = queue.answers.iter().position(|(asked, _)| asked == key)?;
        Some(queue.answers.remove(at).1)
    }

    /// The secret given to the prompt under `key` was refused: say why
    /// under its first field and ask again.
    pub fn reject(&self, key: &str, message: impl Into<String>) {
        self.reject_at(key, 0, message);
    }

    pub fn reject_at(&self, key: &str, field: usize, message: impl Into<String>) {
        self.push(
            key,
            Ask::Reject {
                key: key.to_owned(),
                field,
                message: message.into(),
            },
        );
    }

    pub fn busy(&self, key: &str, busy: Option<String>) {
        self.push(key, Ask::Busy(key.to_owned(), busy));
    }

    pub fn close(&self, key: &str) {
        let mut queue = self.0.borrow_mut();
        if queue.open.as_deref() == Some(key) {
            queue.open = None;
            queue.asks.push(Ask::Close(key.to_owned()));
        }
    }

    fn push(&self, key: &str, ask: Ask) {
        let mut queue = self.0.borrow_mut();
        if queue.open.as_deref() == Some(key) {
            queue.asks.push(ask);
        }
    }

    pub(crate) fn take_asks(&self) -> Vec<Ask> {
        std::mem::take(&mut self.0.borrow_mut().asks)
    }

    pub(crate) fn answer(&self, key: String, outcome: PromptOutcome) {
        let mut queue = self.0.borrow_mut();
        if matches!(outcome, PromptOutcome::Cancel) && queue.open.as_ref() == Some(&key) {
            queue.open = None;
        }
        queue.answers.push((key, outcome));
    }
}

struct ShownPrompt {
    key: String,
    dialog: ModalDialog<SecretPrompt>,
}

impl ShownPrompt {
    fn follow(shown: &mut Option<Self>, asks: &SecretAsks) -> bool {
        let mut changed = false;
        for ask in asks.take_asks() {
            match ask {
                Ask::Open(key, prompt) => {
                    let mut dialog = prompt.into_dialog();
                    Widget::set_focus(dialog.content_mut(), true);
                    *shown = Some(Self { key, dialog });
                    changed = true;
                }
                Ask::Reject {
                    key,
                    field,
                    message,
                } => {
                    if let Some(shown) = shown.as_mut().filter(|shown| shown.key == key) {
                        let prompt = shown.dialog.content_mut();
                        prompt.set_busy(None);
                        prompt.reject_at(field, message);
                    }
                }
                Ask::Busy(key, busy) => {
                    if let Some(shown) = shown.as_mut().filter(|shown| shown.key == key) {
                        shown.dialog.content_mut().set_busy(busy);
                    }
                }
                Ask::Close(key) => {
                    if shown.as_ref().is_some_and(|shown| shown.key == key) {
                        *shown = None;
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    fn answered(shown: &mut Option<Self>, asks: &SecretAsks) -> bool {
        let Some(prompt) = shown.as_mut() else {
            return false;
        };
        if !prompt.dialog.content().answer_pending() {
            return false;
        }
        match prompt.dialog.content_mut().take_outcome() {
            Some(PromptOutcome::Cancel) => {
                let key = prompt.key.clone();
                *shown = None;
                asks.answer(key, PromptOutcome::Cancel);
                true
            }
            Some(outcome) => {
                asks.answer(prompt.key.clone(), outcome);
                false
            }
            None => false,
        }
    }
}

pub(crate) struct SecretSheet {
    asks: SecretAsks,
    shown: Option<ShownPrompt>,
    changed: bool,
    content: ui::widget::Element,
}

impl SecretSheet {
    pub(crate) fn new(asks: SecretAsks, content: ui::widget::Element) -> Self {
        Self {
            asks,
            shown: None,
            changed: false,
            content,
        }
    }
}

impl ui::widget::Decorator for SecretSheet {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn name(&self) -> &'static str {
        "SettingsSecretSheet"
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        let own = ui::widget::StructureChange::of(std::mem::take(&mut self.changed));
        own.join(self.content.take_structure_change())
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        if ShownPrompt::follow(&mut self.shown, &self.asks) {
            self.changed = true;
        }
        self.content.sync(state, path);
        if let Some(shown) = &mut self.shown {
            shown.dialog.sync(state, path);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let Some(shown) = &mut self.shown else {
            self.content.handle_event(event, ctx);
            return;
        };
        shown.dialog.handle_detached(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.path,
        );
        *ctx.handled = true;
        if ShownPrompt::answered(&mut self.shown, &self.asks) {
            self.changed = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        match &self.shown {
            Some(shown) => vec![&shown.dialog as &dyn Widget],
            None => self.content.children(),
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match &mut self.shown {
            Some(shown) => vec![&mut shown.dialog as &mut (dyn Widget + '_)],
            None => self.content.children_mut(),
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        match &self.shown {
            Some(shown) => visitor(&shown.dialog),
            None => self.content.for_each_child(visitor),
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.content
            .draw_overlay(scene, transform, text, theme, presence);
        if let Some(shown) = &mut self.shown {
            shown
                .dialog
                .draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        match &self.shown {
            Some(shown) => shown.dialog.modal_capture(),
            None => self.content.modal_capture(),
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.shown
            .as_ref()
            .is_some_and(|shown| shown.dialog.needs_animation_frame())
            || self.content.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let prompt = self
            .shown
            .as_ref()
            .and_then(|shown| shown.dialog.next_wake_deadline());
        match (prompt, self.content.next_wake_deadline()) {
            (Some(prompt), Some(content)) => Some(prompt.min(content)),
            (prompt, content) => prompt.or(content),
        }
    }
}
