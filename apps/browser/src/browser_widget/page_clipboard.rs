//! The page's clipboard where the engine has none of its own (Linux, where
//! every page renders offscreen): what a page copies (a site's copy button,
//! a script's copy, the person's Ctrl+C, an extension's offscreen document)
//! goes on the system clipboard here, and what a page pastes or reads comes
//! from it (`apps/browser/chromium/README.md`, "Page clipboard").

use std::sync::mpsc::{Receiver, TryRecvError};
use ui::clipboard::ClipboardTarget;
use ui::webview::{WebViewCommand, WebViewEngine};

/// How long a password a page copied stays on the clipboard, as long as
/// Passwords keeps one there.
const SECRET_ON_CLIPBOARD_FOR: std::time::Duration = std::time::Duration::from_secs(30);

/// The page's reads of the system clipboard still waiting on it.
#[derive(Default)]
pub(super) struct PageClipboard {
    reads: Vec<(u32, Receiver<Option<String>>)>,
}

impl PageClipboard {
    /// Put what the page copied on the system clipboard: with its formatting
    /// when it has any, and as a secret (kept out of the clipboard history
    /// and device sync) when it came from a password field.
    pub(super) fn copied(text: &str, html: Option<&str>, secret: bool) {
        if secret {
            let _ = ui::try_clipboard_copy_secret(text, SECRET_ON_CLIPBOARD_FOR);
        } else if let Some(html) = html {
            let _ = ui::clipboard_copy_rich_accepted(html, text);
        } else {
            ui::clipboard_copy(text);
        }
    }

    /// The page pastes or reads: ask for the system clipboard's text without
    /// waiting on it, and answer once it comes ([`Self::answer_ready`]).
    pub(super) fn read(&mut self, request_id: u32) {
        let reply = ui::host::current()
            .clipboard
            .request_text(ClipboardTarget::Clipboard);
        self.reads.push((request_id, reply));
    }

    /// Answer each read whose text has come, in the order they were asked.
    pub(super) fn answer_ready(&mut self, engine: &dyn WebViewEngine) {
        self.reads.retain(|(request_id, reply)| {
            let text = match reply.try_recv() {
                Ok(text) => text,
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => None,
            };
            engine.send(WebViewCommand::AnswerClipboardRead {
                request_id: *request_id,
                text,
            });
            false
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex, mpsc};
    use ui::clipboard::ClipboardBackend;
    use ui::webview::WebViewEvent;
    use ui::webview::test_engine::TestEngine;

    /// A system clipboard that keeps each copy as it was offered, and answers
    /// a read when the test says.
    #[derive(Default)]
    struct Recorded {
        copies: Mutex<Vec<(String, Option<String>, bool)>>,
        reply: Mutex<Option<mpsc::Sender<Option<String>>>>,
    }
    impl ClipboardBackend for Recorded {
        fn set_text(&self, _: ClipboardTarget, text: &str) -> bool {
            self.copies.lock().unwrap().push((text.into(), None, false));
            true
        }
        fn set_rich_text(&self, html: &str, plain: &str) -> bool {
            self.copies
                .lock()
                .unwrap()
                .push((plain.into(), Some(html.into()), false));
            true
        }
        fn set_secret_text(&self, text: &str) -> bool {
            self.copies.lock().unwrap().push((text.into(), None, true));
            true
        }
        fn text(&self, _: ClipboardTarget) -> Option<String> {
            None
        }
        fn request_text(&self, _: ClipboardTarget) -> mpsc::Receiver<Option<String>> {
            let (reply, result) = mpsc::channel();
            *self.reply.lock().unwrap() = Some(reply);
            result
        }
        fn name(&self) -> &'static str {
            "recorded-page-clipboard"
        }
    }

    #[test]
    fn what_a_page_copied_lands_on_the_system_clipboard_and_a_password_as_a_secret() {
        let clipboard = Arc::new(Recorded::default());
        let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
        let _host = ui::host::enter(&services);
        let engine = TestEngine::new();
        engine.push_event(WebViewEvent::ClipboardWrite {
            text: "https://example.test/share".into(),
            html: None,
            secret: false,
        });
        engine.push_event(WebViewEvent::ClipboardWrite {
            text: "the words".into(),
            html: Some("<b>the words</b>".into()),
            secret: false,
        });
        engine.push_event(WebViewEvent::ClipboardWrite {
            text: "hunter2".into(),
            html: None,
            secret: true,
        });
        let mut widget = super::super::BrowserWidget::with_engine(engine, true);
        widget.apply_engine_events();
        assert_eq!(
            *clipboard.copies.lock().unwrap(),
            [
                ("https://example.test/share".to_owned(), None, false),
                (
                    "the words".to_owned(),
                    Some("<b>the words</b>".to_owned()),
                    false
                ),
                ("hunter2".to_owned(), None, true),
            ]
        );
    }

    #[test]
    fn a_page_paste_is_answered_with_the_system_clipboard_once_it_comes() {
        let clipboard = Arc::new(Recorded::default());
        let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
        let _host = ui::host::enter(&services);
        let engine = TestEngine::new();
        engine.push_event(WebViewEvent::ClipboardRead { request_id: 4 });
        let mut widget = super::super::BrowserWidget::with_engine(engine.clone(), true);
        widget.apply_engine_events();
        let answered = |engine: &TestEngine| {
            engine
                .commands()
                .into_iter()
                .filter(|command| matches!(command, WebViewCommand::AnswerClipboardRead { .. }))
                .collect::<Vec<_>>()
        };
        assert!(answered(&engine).is_empty(), "nothing to answer with yet");
        clipboard
            .reply
            .lock()
            .unwrap()
            .take()
            .expect("the system clipboard was asked")
            .send(Some("copied elsewhere".into()))
            .unwrap();
        widget.apply_engine_events();
        assert_eq!(
            answered(&engine),
            [WebViewCommand::AnswerClipboardRead {
                request_id: 4,
                text: Some("copied elsewhere".into()),
            }]
        );
    }
}
