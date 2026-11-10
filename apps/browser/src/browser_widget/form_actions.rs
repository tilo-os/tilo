//! A page's forms as the page's own actions, in the window: what it
//! publishes for them, how one runs, and what each read and each answer
//! teaches. [`crate::page_forms`] is what the window knows of the forms;
//! [`crate::site_memory`] is what the document keeps of them.
//!
//! A form action runs as `fill-page-form` does: the same request, under the
//! actor the action door admitted, in its turn, answered with the same
//! result for each answer. It names its form by signature, so the engine
//! answers it only while the page holds that form.
//!
//! The forms are read by the window itself once its page has loaded
//! ([`super::forms_read`]), and by whoever runs `read-page-forms`. Either
//! read is what the page publishes from then on.

use std::collections::BTreeMap;

use ui::webview::{PageActor, PageFailure, PageReply, PageRequest};

use super::{BrowserWidget, PageOperation};
use crate::page_forms::{self, FORM_CHANGED, FormRun, Standing, answer_id};
use crate::resource::{Browser, browser};
use crate::site_memory::{self, Place, RememberedForm};

/// What a form action is refused with when the page holds no form of the
/// signature it was made for: it holds other forms (the engine's
/// `form_changed`), or none at all (`none`, its `no_form`).
fn gone_message(label: &str, standing: Standing, none: bool) -> String {
    let name = if label.is_empty() {
        "The form".to_owned()
    } else {
        format!("The form \"{label}\"")
    };
    let known = match standing {
        Standing::Remembered => "remembered",
        Standing::Read | Standing::Changed => "read",
    };
    let why = if none {
        "this page holds no form now"
    } else {
        "it has changed, or this page does not hold it"
    };
    format!(
        "{name} is not on this page as it was {known}: {why}. Nothing was placed or sent. \
         The page reads its forms again by itself and \
         publishes the action of each form as it is now: list the page's actions and run \
         that one. If none appears, read the page's forms (read-page-forms)."
    )
}

/// Say in a form action's answer what running it changed for its caller:
/// that the form asks more now, and which forms followed it on this site
/// before. The engine's own report of each answer is left as it is.
fn note_run(reply: &mut PageReply, grew: bool, then: &[(String, String)]) {
    if grew {
        reply.message.push_str(
            "; the form asks something else now, so this action is withdrawn: the page reads \
             its forms again by itself and publishes the action of the form as it is now \
             (read-page-forms reads them at once)",
        );
    }
    if !then.is_empty()
        && let Some(serde_json::Value::Object(data)) = reply.data.as_mut()
    {
        let next: Vec<serde_json::Value> = then
            .iter()
            .map(|(label, signature)| {
                serde_json::json!({
                    "label": label,
                    "signature": signature,
                    "action": answer_id(signature),
                })
            })
            .collect();
        data.insert("then".into(), next.into());
    }
}

impl BrowserWidget {
    /// The actions the page publishes for its forms: for each form an
    /// action named after it with its questions as typed parameters, and
    /// one that sends it. From a read of this page, or before any read from
    /// what was remembered of its site. After them, the action that lists
    /// what is remembered of the site.
    pub(super) fn form_actions(&self) -> Vec<ui::tilo_actions::ActionSpec> {
        let mut actions =
            Place::of(&self.current_uri).map_or_else(Vec::new, |place| self.forms.actions(&place));
        actions.push(self.remembered_forms_action());
        actions
    }

    /// Follow the document's site memory. A private window follows none: it
    /// publishes nothing that was remembered, as it remembers nothing.
    pub(super) fn present_forms(&mut self, memory: &BTreeMap<String, RememberedForm>) {
        if self.browsing_mode.is_private() || self.forms.memory() == memory {
            return;
        }
        self.forms.present(memory);
        // The site panel lists what is remembered of the site in front.
        self.structure_dirty |= self.site.open;
    }

    /// The window's own read of the page at `place` answered `data`: its
    /// forms are what the page publishes from now on, and what the site is
    /// remembered to ask.
    pub(super) fn heard_own_read(&mut self, place: &Place, mut data: serde_json::Value) {
        // Nobody was admitted for this read. On a page that holds a sign-in
        // the answers a question offers may be the person's own, so they are
        // not published until an actor the person admitted reads the page.
        if self.page_holds_sign_in() {
            page_forms::without_offered_answers(&mut data);
        }
        let shapes = self.forms.heard_read(place, &data);
        self.remember_forms(place, shapes, true);
    }

    /// Start a form action for `invocation`: the request `fill-page-form`
    /// makes, in the tab's turn, under `actor`.
    pub(super) fn start_form_run(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        actor: Option<PageActor>,
        run: &FormRun,
    ) -> Result<(), String> {
        // The action's parameters are named for its questions; the engine is
        // answered by the names the form asks under.
        let answers = Place::of(&self.current_uri).map_or_else(
            || run.answers.clone(),
            |place| self.forms.named(&place, &run.form, &run.answers),
        );
        self.forms.started(invocation, &run.form);
        let started = self.start_page_operation(invocation, actor, run.request(answers));
        if started.is_err() {
            self.forms.take_run(invocation);
        }
        started
    }

    /// The engine answered `operation`. A read of the page's forms is what
    /// the page publishes from now on, and what the site is remembered to
    /// ask; an answered form teaches which questions the typed path refuses
    /// and what followed; and a form action whose form is gone is refused as
    /// changed, never answered by another form.
    pub(super) fn hear_forms(
        &mut self,
        operation: &PageOperation,
        result: Result<PageReply, PageFailure>,
    ) -> Result<PageReply, PageFailure> {
        let run = self.forms.take_run(operation.invocation);
        let Some(place) = Place::of(&self.current_uri) else {
            return result;
        };
        match (&operation.acted, result) {
            // The forms of the page itself. A frame's forms take their
            // fields' own actions: the form action reaches no frame.
            (PageRequest::Forms { frame: None }, Ok(reply)) => {
                if let Some(data) = &reply.data {
                    let shapes = self.forms.heard_read(&place, data);
                    self.remember_forms(&place, shapes, true);
                }
                Ok(reply)
            }
            // A form of the page itself: one answered in a frame is that
            // frame's, and is none of the forms the page publishes.
            (PageRequest::FillForm { frame: None, .. }, Ok(mut reply)) => {
                let heard = reply
                    .data
                    .as_ref()
                    .map(|data| self.forms.heard_answer(&place, data));
                if let Some(heard) = heard {
                    if let Some((at, shape)) = heard.learned {
                        self.remember_forms(&at, vec![shape], false);
                    }
                    if heard.grew || heard.refresh {
                        // The form is not the one the page publishes any
                        // more, or offers other answers than it publishes:
                        // the window reads the page again.
                        self.read_forms_again();
                    }
                    if run.is_some() {
                        note_run(&mut reply, heard.grew, &heard.then);
                    }
                }
                Ok(reply)
            }
            // The page holds forms, none of them the one named
            // (`form_changed`), or no form at all (`no_form`). Either way it
            // does not hold the form, and nothing was placed.
            (
                PageRequest::FillForm {
                    form, frame: None, ..
                },
                Err(failure),
            ) if matches!(failure.code.as_str(), "no_form" | FORM_CHANGED) => {
                if run.is_none() {
                    // `fill-page-form` named the form itself and is told
                    // what the engine said. A form the page publishes under
                    // that signature is withdrawn, and the page read again.
                    if self.forms.standing(&place, form).is_some() {
                        self.forms.heard_gone(&place, form);
                        self.read_forms_again();
                    }
                    return Err(failure);
                }
                let none = failure.code == "no_form";
                let (label, standing) = self.forms.heard_gone(&place, form);
                // The page does not hold what it publishes: it is read again.
                self.read_forms_again();
                Err(PageFailure {
                    code: FORM_CHANGED.to_owned(),
                    message: gone_message(&label, standing, none),
                    record: failure.record,
                })
            }
            (_, result) => result,
        }
    }

    /// Remember `forms` as forms the page at `place` asks, all of them when
    /// `whole`, through the document's own action. Nothing is written when
    /// memory already says so.
    fn remember_forms(&self, place: &Place, mut forms: Vec<RememberedForm>, whole: bool) {
        // A private window keeps nothing, nor does one whose history is
        // paused; a window outside a document has nowhere to keep it.
        if self.browsing_mode.is_private() || self.history_paused() {
            return;
        }
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        if self.page_holds_sign_in() {
            // On a page that holds a sign-in the answers a question offers
            // may be the person's own: their saved addresses, their accounts.
            for question in forms.iter_mut().flat_map(|form| form.questions.iter_mut()) {
                question.choices.clear();
            }
        }
        let at = tilo_clock::unix_now();
        let mut next = self.forms.memory().clone();
        let refused = site_memory::remember(
            &mut next,
            &place.origin,
            &place.page,
            forms.clone(),
            whole,
            at,
        )
        .is_err();
        if refused || next == *self.forms.memory() {
            return;
        }
        dispatch.submit_on_its_own(Browser::REMEMBER_FORMS.with(browser::RememberForms {
            origin: place.origin.clone(),
            page: place.page.clone(),
            forms,
            whole,
            at,
        }));
    }
}
