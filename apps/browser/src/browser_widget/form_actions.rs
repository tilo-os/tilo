//! A page's forms as the page's own actions, in the window: what it
//! publishes for them, how one runs, and what each read and each answer
//! teaches. [`crate::page_forms`] is what the window knows of the forms;
//! [`crate::site_memory`] is what the document keeps of them.
//!
//! A form action runs as `fill-page-form` does: the same request, under the
//! actor the action door admitted, in its turn, answered with the same
//! result for each answer. It names its form by signature, so the engine
//! answers it only while the page holds that form.

use ui::webview::{PageActor, PageFailure, PageReply, PageRequest};

use super::{BrowserWidget, PageOperation};
use crate::page_forms::{FORM_CHANGED, FormRun, Standing, answer_id};
use crate::resource::{Browser, browser};
use crate::site_memory::{self, Place, RememberedForm};

/// What a form action is refused with when the page holds no form of the
/// signature it was made for.
fn gone_message(label: &str, standing: Standing) -> String {
    let name = if label.is_empty() {
        "The form".to_owned()
    } else {
        format!("The form \"{label}\"")
    };
    let known = match standing {
        Standing::Remembered => "remembered",
        Standing::Read | Standing::Changed => "read",
    };
    format!(
        "{name} is not on this page as it was {known}: it has changed, or this page does not \
         hold it. Nothing was placed or sent. Read the page's forms again (read-page-forms) \
         and run the action that read publishes."
    )
}

/// Say in a form action's answer what running it changed for its caller:
/// that the form asks more now, and which forms followed it on this site
/// before. The engine's own report of each answer is left as it is.
fn note_run(reply: &mut PageReply, grew: bool, then: &[(String, String)]) {
    if grew {
        reply.message.push_str(
            "; the form asks something else now, so its action is withdrawn until the page's \
             forms are read again (read-page-forms)",
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
    /// what was remembered of its site.
    pub(super) fn form_actions(&self) -> Vec<ui::tilo_actions::ActionSpec> {
        Place::of(&self.current_uri).map_or_else(Vec::new, |place| self.forms.actions(&place))
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
            (PageRequest::FillForm { .. }, Ok(mut reply)) => {
                let heard = reply
                    .data
                    .as_ref()
                    .map(|data| self.forms.heard_answer(&place, data));
                if let Some(heard) = heard {
                    if let Some((at, shape)) = heard.learned {
                        self.remember_forms(&at, vec![shape], false);
                    }
                    if run.is_some() {
                        note_run(&mut reply, heard.grew, &heard.then);
                    }
                }
                Ok(reply)
            }
            (PageRequest::FillForm { form, .. }, Err(failure))
                if run.is_some() && failure.code == "no_form" =>
            {
                let (label, standing) = self.forms.heard_gone(&place, form);
                Err(PageFailure {
                    code: FORM_CHANGED.to_owned(),
                    message: gone_message(&label, standing),
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
        // A private window keeps nothing, and a window outside a document
        // has nowhere to keep it.
        if self.browsing_mode.is_private() {
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
