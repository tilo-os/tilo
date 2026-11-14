//! The sign-in a site asks for, in this window ([`crate::page_sign_in`]).
//!
//! An answer leaves one way, whoever gives it: the person in the prompt, a
//! screen reader activating its button, or an agent or script through the
//! row's action all end in [`BrowserWidget::answer_sign_in`]. So the rules
//! of a secret this window sends hold for each of them alike: the password
//! is remembered by its digest and never written to the tab's record, and a
//! window that sent credentials holds a sign-in from then on, so reading its
//! page takes the authority to operate it.

use ui::webview::HttpAuthCredentials;
use ui::widget::Widget;

use super::BrowserWidget;
use crate::page_dialogs::PageDialogs;
use crate::page_sign_in::PageSignIn;

impl BrowserWidget {
    /// Send what the person answered in the sign-in prompt, if they did.
    pub(super) fn poll_sign_in(&mut self) {
        if let Some((sign_in, credentials)) = self.page_sign_in.take_answer() {
            // The prompt answered is the one open: nothing refuses it.
            let _ = self.answer_sign_in(sign_in, credentials);
        }
    }

    /// Answer the sign-in `sign_in` with `credentials`, or cancel it with
    /// none. Says what the answer did, never what it was.
    pub(super) fn answer_sign_in(
        &mut self,
        sign_in: u32,
        credentials: Option<HttpAuthCredentials>,
    ) -> Result<String, String> {
        if let Some(credentials) = &credentials {
            self.page_record.sent_secret(credentials.password());
        }
        let answered = self
            .page_sign_in
            .answer(&self.engine, sign_in, credentials)?;
        self.structure_dirty = true;
        // Credentials the site was given are the person's: the page holds a
        // sign-in from the moment they are sent.
        if answered.credentials {
            self.hold_sign_in();
        }
        Ok(answered.said())
    }
}

/// What stands over the page: the sign-in a request waits on, else the
/// page's own question. One at a time; a question under a sign-in shows
/// again once the sign-in is answered.
pub(super) fn modal<'a>(
    sign_in: &'a PageSignIn,
    dialogs: &'a PageDialogs,
) -> Option<&'a dyn Widget> {
    sign_in.widget().or_else(|| dialogs.widget())
}

pub(super) fn modal_mut<'a>(
    sign_in: &'a mut PageSignIn,
    dialogs: &'a mut PageDialogs,
) -> Option<&'a mut dyn Widget> {
    if sign_in.current().is_some() {
        sign_in.widget_mut()
    } else {
        dialogs.widget_mut()
    }
}
