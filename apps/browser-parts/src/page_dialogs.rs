//! A page's own questions: `alert`, `confirm`, `prompt`, and "leave this
//! page?" from a `beforeunload` handler.
//!
//! The page waits on its question until it is answered. Each is the
//! toolkit's shared dialog over the page's window, named by the site that
//! asks, so a person can tell the page's words from the browser's; and the
//! window's [`ROW`] offers the same answers, so a screen reader, a script
//! and an agent answer the one question the page waits on the way a person
//! does.
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use ui::tilo_actions::{Resource, ResourcePresentation};
use ui::tokens::TextRole;
use ui::webview::{
    MAX_PAGE_DIALOG_ANSWER, PageDialog, PageDialogKind, SharedEngine, WebViewCommand,
    WebViewEngine, WebViewEvent,
};
use ui::widget::{ModalDialog, PromptBody, Text, Widget};

use crate::CATALOG;
use crate::effects::BrowserEffect;

/// The window's row for the question the page waits on, and the dialog's
/// test id.
pub const ROW: &str = "browser.page-dialog";

/// What the person pressed in the dialog, not yet sent to the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pressed {
    Accept,
    Decline,
}

enum Sheet {
    Message(Box<ModalDialog<Text>>),
    Prompt(Box<ModalDialog<PromptBody>>),
}

struct Open {
    dialog: PageDialog,
    /// Who asks, or what: the dialog's title.
    title: String,
    /// The answers' names: accepting, and declining when it can be.
    accept: String,
    decline: Option<String>,
    sheet: Sheet,
    pressed: Rc<Cell<Option<Pressed>>>,
}

/// The question the page waits on, if any.
#[derive(Default)]
pub struct PageDialogs {
    /// The page engine asked for its questions; another engine (a restarted
    /// one) is asked again, and the question of the old one goes.
    asked: Option<Weak<dyn WebViewEngine>>,
    open: Option<Open>,
}

impl PageDialogs {
    /// Ask `page` for its questions, once per engine. Returns whether what
    /// is shown changed.
    pub fn sync(&mut self, page: &SharedEngine) -> bool {
        let asked = self
            .asked
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|asked| Arc::ptr_eq(&asked, page));
        if asked {
            return false;
        }
        page.accept_page_dialogs();
        self.asked = Some(Arc::downgrade(page));
        self.open.take().is_some()
    }

    /// Follow what the engine said of the page's questions. Returns whether
    /// what is shown changed.
    pub fn apply(&mut self, event: &WebViewEvent) -> bool {
        match event {
            // The engine asks one question of a page at a time: a new one
            // means the last is no longer waited on.
            WebViewEvent::PageDialog(dialog) => {
                self.open = Some(Open::new(dialog.clone()));
                true
            }
            WebViewEvent::PageDialogClosed { id } => {
                self.open.take_if(|open| open.dialog.id == *id).is_some()
            }
            _ => false,
        }
    }

    /// The question the page waits on.
    pub fn current(&self) -> Option<&PageDialog> {
        self.open.as_ref().map(|open| &open.dialog)
    }

    /// Send the answer the person gave in the dialog, if they gave one.
    /// Returns whether what is shown changed.
    pub fn poll(&mut self, page: &SharedEngine) -> bool {
        let Some(open) = &self.open else {
            return false;
        };
        let Some(pressed) = open.pressed.take() else {
            return false;
        };
        let id = open.dialog.id;
        self.answer(page, id, pressed == Pressed::Accept, None)
            .is_ok()
    }

    /// Answer the question `id`: OK (or Leave, or Reload) when `accept`, else
    /// Cancel. A prompt accepted without `text` answers what its field
    /// holds.
    ///
    /// # Errors
    /// When the page no longer waits on that question, or `text` is longer
    /// than an answer can be.
    pub fn answer(
        &mut self,
        page: &SharedEngine,
        id: u32,
        accept: bool,
        text: Option<String>,
    ) -> Result<(), String> {
        let open = self
            .open
            .as_ref()
            .filter(|open| open.dialog.id == id)
            .ok_or("the page no longer waits on that question")?;
        let text = match (&open.sheet, accept) {
            (Sheet::Prompt(sheet), true) => {
                Some(text.unwrap_or_else(|| sheet.content().value().to_owned()))
            }
            _ => None,
        };
        if text
            .as_ref()
            .is_some_and(|text| text.len() > MAX_PAGE_DIALOG_ANSWER)
        {
            return Err("the answer is too long for the page".into());
        }
        page.send(WebViewCommand::AnswerPageDialog { id, accept, text });
        self.open = None;
        Ok(())
    }

    /// The dialog over the page while it asks.
    pub fn widget(&self) -> Option<&dyn Widget> {
        self.open.as_ref().map(|open| match &open.sheet {
            Sheet::Message(sheet) => &**sheet as &dyn Widget,
            Sheet::Prompt(sheet) => &**sheet as &dyn Widget,
        })
    }

    pub fn widget_mut(&mut self) -> Option<&mut dyn Widget> {
        self.open.as_mut().map(|open| match &mut open.sheet {
            Sheet::Message(sheet) => &mut **sheet as &mut dyn Widget,
            Sheet::Prompt(sheet) => &mut **sheet as &mut dyn Widget,
        })
    }

    /// The question as the window's row: who asks, what (the page's own
    /// words), and its answers, each bound to this question so an answer
    /// meant for it never reaches the next.
    pub fn resource(&self) -> Option<Resource> {
        use ui::tilo_resource::Effect as _;
        let open = self.open.as_ref()?;
        let dialog = &open.dialog;
        let mut row =
            Resource::new(ROW, ROW, open.title.clone()).with_presentation(ResourcePresentation {
                detail: None,
                badge: None,
                preview: Some(preview(&open.body())),
            });
        let prompt = dialog.kind == PageDialogKind::Prompt;
        // A prompt's answer is asked; absent, it is what the field holds,
        // which starts as the page's suggestion.
        let mut accept = BrowserEffect::AcceptPageDialog {
            dialog: dialog.id,
            text: None,
        }
        .bound_asking(if prompt { &["text"] } else { &[] });
        accept.label.clone_from(&open.accept);
        row.actions.push(accept);
        if let Some(label) = &open.decline {
            let mut decline = BrowserEffect::DismissPageDialog { dialog: dialog.id }.bound();
            decline.label.clone_from(label);
            row.actions.push(decline);
        }
        Some(row)
    }
}

impl Open {
    fn new(dialog: PageDialog) -> Self {
        let t = |key: &str| CATALOG.t(key);
        let site = site(&dialog.origin);
        let titled = |key: &str| match &site {
            Some(site) => CATALOG.t_args(key, &[("site", ui::i18n::ArgValue::Text(site.clone()))]),
            None => t(&format!("{key}-page")),
        };
        let (title, accept, decline) = match dialog.kind {
            PageDialogKind::Alert => (
                titled("browser-page-dialog-message-title"),
                t("browser-page-dialog-close"),
                None,
            ),
            PageDialogKind::Confirm => (
                titled("browser-page-dialog-question-title"),
                t("browser-page-dialog-allow"),
                Some(t("browser-page-dialog-cancel")),
            ),
            PageDialogKind::Prompt => (
                titled("browser-page-dialog-question-title"),
                t("browser-page-dialog-send"),
                Some(t("browser-page-dialog-cancel")),
            ),
            PageDialogKind::Leave { reload: false } => (
                t("browser-page-dialog-leave-title"),
                t("browser-page-dialog-leave"),
                Some(t("browser-page-dialog-cancel")),
            ),
            PageDialogKind::Leave { reload: true } => (
                t("browser-page-dialog-reload-title"),
                t("browser-page-dialog-reload"),
                Some(t("browser-page-dialog-cancel")),
            ),
        };
        let pressed = Rc::new(Cell::new(None));
        let press = |answer| {
            let pressed = pressed.clone();
            move || pressed.set(Some(answer))
        };
        // Escape and a click beside the sheet decline; an alert has only its
        // one answer.
        let dismissed = if decline.is_some() {
            Pressed::Decline
        } else {
            Pressed::Accept
        };
        let sheet = match dialog.kind {
            PageDialogKind::Prompt => {
                let body = PromptBody::new("", "")
                    .with_caption(&dialog.message)
                    .with_initial(&dialog.default_text);
                let mut sheet = ModalDialog::new(title.clone(), body)
                    .open(true)
                    .with_test_id(ROW)
                    .primary_action(accept.clone(), press(Pressed::Accept))
                    .on_dismiss(press(dismissed));
                if let Some(decline) = &decline {
                    sheet = sheet.secondary_action(decline.clone(), press(Pressed::Decline));
                }
                Sheet::Prompt(Box::new(sheet))
            }
            kind => {
                let body = if matches!(kind, PageDialogKind::Leave { .. }) {
                    std::borrow::Cow::Owned(t("browser-page-dialog-unsaved"))
                } else {
                    std::borrow::Cow::Borrowed(dialog.message.as_str())
                };
                let mut sheet =
                    ModalDialog::new(title.clone(), Text::new(&body).with_role(TextRole::Body))
                        .open(true)
                        .with_test_id(ROW)
                        // Leaving loses what the page warned of: Enter stays.
                        .with_destructive(matches!(kind, PageDialogKind::Leave { .. }))
                        .primary_action(accept.clone(), press(Pressed::Accept))
                        .on_dismiss(press(dismissed));
                if let Some(decline) = &decline {
                    sheet = sheet.secondary_action(decline.clone(), press(Pressed::Decline));
                }
                Sheet::Message(Box::new(sheet))
            }
        };
        Self {
            dialog,
            title,
            accept,
            decline,
            sheet,
            pressed,
        }
    }

    /// What the dialog says beneath its title.
    fn body(&self) -> String {
        match self.dialog.kind {
            PageDialogKind::Leave { .. } => CATALOG.t("browser-page-dialog-unsaved"),
            _ => self.dialog.message.clone(),
        }
    }
}

/// The site an origin names, as a person reads it: `example.org` of
/// `https://example.org`, with its port when it has one.
fn site(origin: &str) -> Option<String> {
    let site = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))?;
    (!site.is_empty()).then(|| site.to_owned())
}

/// `text` as a row's preview: within its bound, cut where a character
/// starts, without the characters a preview may not hold.
fn preview(text: &str) -> String {
    let mut preview: String = text
        .chars()
        .filter(|character| !matches!(character, '\r' | '\0'))
        .collect();
    if preview.len() > ResourcePresentation::MAX_PREVIEW_BYTES {
        let mut end = ResourcePresentation::MAX_PREVIEW_BYTES;
        while !preview.is_char_boundary(end) {
            end -= 1;
        }
        preview.truncate(end);
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::webview::test_engine::TestEngine;

    fn asks(kind: PageDialogKind, id: u32) -> WebViewEvent {
        WebViewEvent::PageDialog(PageDialog {
            id,
            kind,
            origin: "https://shop.example".into(),
            message: "Empty the basket?".into(),
            default_text: "Ada".into(),
        })
    }

    fn answers(engine: &TestEngine) -> Vec<(u32, bool, Option<String>)> {
        engine
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::AnswerPageDialog { id, accept, text } => Some((id, accept, text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_question_is_asked_of_each_engine_and_answered_once() {
        let engine = TestEngine::new();
        let page: SharedEngine = engine.clone();
        let mut dialogs = PageDialogs::default();
        assert!(!dialogs.sync(&page));
        assert!(
            engine.accepts_page_dialogs(),
            "the page's questions come here"
        );
        assert!(dialogs.apply(&asks(PageDialogKind::Confirm, 3)));
        let row = dialogs.resource().expect("the question is a row");
        assert_eq!(row.label, "shop.example asks");
        assert_eq!(
            row.presentation.and_then(|p| p.preview).as_deref(),
            Some("Empty the basket?")
        );
        let labels: Vec<_> = row.actions.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, ["Allow", "Cancel"]);

        assert!(
            dialogs.answer(&page, 2, true, None).is_err(),
            "an answer meant for another question"
        );
        dialogs.answer(&page, 3, true, None).unwrap();
        assert_eq!(answers(&engine), [(3, true, None)]);
        assert!(dialogs.widget().is_none() && dialogs.resource().is_none());
        assert!(
            dialogs.answer(&page, 3, false, None).is_err(),
            "answered once"
        );

        // The engine stops waiting on a question by itself: it goes.
        dialogs.apply(&asks(PageDialogKind::Alert, 4));
        assert!(!dialogs.apply(&WebViewEvent::PageDialogClosed { id: 3 }));
        assert!(dialogs.apply(&WebViewEvent::PageDialogClosed { id: 4 }));
        assert!(dialogs.current().is_none());

        // A restarted engine is asked again; the old one's question goes.
        dialogs.apply(&asks(PageDialogKind::Alert, 5));
        let restarted = TestEngine::new();
        let page: SharedEngine = restarted.clone();
        assert!(dialogs.sync(&page));
        assert!(restarted.accepts_page_dialogs() && dialogs.current().is_none());
    }

    #[test]
    fn a_prompt_answers_its_field_or_the_text_given() {
        let engine = TestEngine::new();
        let page: SharedEngine = engine.clone();
        let mut dialogs = PageDialogs::default();
        dialogs.sync(&page);
        dialogs.apply(&asks(PageDialogKind::Prompt, 1));
        let row = dialogs.resource().unwrap();
        let send = &row.actions[0];
        assert_eq!(send.label, "Send");
        assert_eq!(
            send.params()
                .iter()
                .map(|param| &*param.name)
                .collect::<Vec<_>>(),
            ["text"],
            "only the answer is asked; the question is bound"
        );
        dialogs.answer(&page, 1, true, None).unwrap();
        dialogs.apply(&asks(PageDialogKind::Prompt, 2));
        dialogs
            .answer(&page, 2, true, Some("Grace".into()))
            .unwrap();
        dialogs.apply(&asks(PageDialogKind::Prompt, 3));
        assert!(
            dialogs
                .answer(&page, 3, true, Some("x".repeat(MAX_PAGE_DIALOG_ANSWER + 1)))
                .is_err()
        );
        dialogs
            .answer(&page, 3, false, Some("ignored".into()))
            .unwrap();
        assert_eq!(
            answers(&engine),
            [
                (1, true, Some("Ada".into())),
                (2, true, Some("Grace".into())),
                (3, false, None),
            ]
        );
    }

    #[test]
    fn leaving_names_what_it_risks_and_an_alert_only_closes() {
        let page: SharedEngine = TestEngine::new();
        let mut dialogs = PageDialogs::default();
        dialogs.sync(&page);
        dialogs.apply(&asks(PageDialogKind::Leave { reload: false }, 1));
        let row = dialogs.resource().unwrap();
        assert_eq!(row.label, "Leave this page?");
        assert_eq!(
            row.presentation.and_then(|p| p.preview).as_deref(),
            Some("Changes you made may not be saved.")
        );
        let labels: Vec<_> = row.actions.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, ["Leave", "Cancel"]);
        dialogs.apply(&asks(PageDialogKind::Leave { reload: true }, 2));
        assert_eq!(dialogs.resource().unwrap().label, "Reload this page?");

        let mut alert = PageDialog {
            id: 3,
            kind: PageDialogKind::Alert,
            origin: String::new(),
            message: "Saved\r\n".repeat(200),
            default_text: String::new(),
        };
        dialogs.apply(&WebViewEvent::PageDialog(alert.clone()));
        let row = dialogs.resource().unwrap();
        assert_eq!(row.label, "Message from this page");
        assert_eq!(
            row.actions
                .iter()
                .map(|a| a.label.as_str())
                .collect::<Vec<_>>(),
            ["Close"]
        );
        let preview = row.presentation.and_then(|p| p.preview).unwrap();
        assert!(
            preview.len() <= ResourcePresentation::MAX_PREVIEW_BYTES && !preview.contains('\r')
        );
        alert.id = 4;
        alert.origin = "http://127.0.0.1:8080".into();
        dialogs.apply(&WebViewEvent::PageDialog(alert));
        assert_eq!(
            dialogs.resource().unwrap().label,
            "Message from 127.0.0.1:8080"
        );
    }
}
