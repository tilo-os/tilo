//! How what the person does reaches the room: the one door a verb or a field
//! edit is applied through — at once, as the frame ends, or once they agreed
//! to a removal — and the chain the chat's messages are read and written
//! through.

#[cfg(test)]
use automerge::AutoCommit;

use super::ChatApp;

impl ChatApp {
    /// Apply a verb or a field edit through the resource's dispatcher now:
    /// the single document-mutation path of every on-screen affordance, the
    /// same door the palette and agents use. The projection is reconciled
    /// from the document on the next sync.
    pub(crate) fn apply_now(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::resource::Chat>,
    ) {
        let Some(invoke) = self.dispatched(submit) else {
            return;
        };
        if let Some(dispatch) = &self.dispatch {
            // A refusal changes nothing; the dispatcher reports it.
            let _ = dispatch.submit_now(doc, invoke);
        }
    }

    /// [`Self::apply_now`] of a removal the person chose from a menu: asked
    /// first, unless they hold Shift and Undo takes it back
    /// ([`ui::framework::Dispatcher::submit_confirming`]).
    pub(crate) fn apply_confirming(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::resource::Chat>,
    ) {
        let Some(invoke) = self.dispatched(submit) else {
            return;
        };
        if let Some(dispatch) = &self.dispatch {
            // A refusal changes nothing; the dispatcher reports it.
            let _ = dispatch.submit_confirming(doc, invoke);
        }
    }

    /// applied after this pass: from a pass that only reads the document (a
    /// frame's sync, a worker's answer). A refusal is reported by the
    /// dispatcher.
    pub(crate) fn submit(&self, submit: impl ui::framework::Submit<crate::resource::Chat>) {
        let Some(invoke) = self.dispatched(submit) else {
            return;
        };
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit(invoke);
        }
    }

    /// `submit` what Chat does on its own, for nobody's gesture (a read
    /// receipt): a refusal is Chat's to handle, never said over the window.
    pub(super) fn submit_on_its_own(
        &self,
        submit: impl ui::framework::Submit<crate::resource::Chat>,
    ) {
        let Some(invoke) = self.dispatched(submit) else {
            return;
        };
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit_on_its_own(invoke);
        }
    }

    /// Whether the door refused the read receipt last sent: it then is not
    /// offered to this person, and the same receipt is not sent again.
    pub(super) fn note_refused_read(&mut self) {
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        let refused = dispatch.refused();
        let read_refused = refused.with(|refused| {
            refused
                .as_ref()
                .is_some_and(|refused| refused.action == crate::resource::Chat::MARK_READ.id)
        });
        if read_refused {
            refused.set(None);
            if let Some(asked) = self.read_asked.take() {
                self.read_refused = Some(asked);
            }
        }
    }

    /// `submit` as the invocation it is, noted for tests.
    #[cfg_attr(
        not(test),
        allow(clippy::unused_self, reason = "a test also keeps what was submitted")
    )]
    fn dispatched(
        &self,
        submit: impl ui::framework::Submit<crate::resource::Chat>,
    ) -> Option<ui::tilo_resource::Invoke<crate::resource::Chat>> {
        let invoke = submit.into_invoke().ok()?;
        #[cfg(test)]
        if let Ok(action) = invoke.build(0) {
            self.dispatched.borrow_mut().push(action.operation);
        }
        Some(invoke)
    }

    /// The last action an on-screen affordance applied.
    #[cfg(test)]
    pub(crate) fn last_dispatched(&self) -> Option<crate::record::ChatAction> {
        self.dispatched.borrow().last().cloned()
    }

    /// Test-only: apply what this window submitted to `doc`, as the host
    /// applies it after a pass.
    #[cfg(test)]
    pub(crate) fn land(&self, doc: &mut AutoCommit) {
        if let Some(dispatch) = &self.dispatch {
            ui::framework::testing::resolve(dispatch, doc).expect("submitted edits apply");
        }
        // What was submitted on the messages applies to their parts.
        if let Some(chain) = &self.chain {
            ui::framework::testing::resolve(&chain.dispatcher(), doc)
                .expect("submitted edits apply");
        }
    }

    /// Test-only: apply what this window asked the person to confirm (a
    /// destructive verb), as the door does once they agree, and read the
    /// record again.
    #[cfg(test)]
    pub(crate) fn confirm_asked(&mut self, doc: &mut AutoCommit) {
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        let mut chat = ui::tilo_resource::Replica::<crate::resource::Chat>::from_doc(doc.clone())
            .as_invoker(&self.me.id);
        for action in dispatch.take_asked().expect("what was asked builds") {
            chat.apply_action(&action)
                .expect("a confirmed change applies");
        }
        *doc = chat.doc().clone();
        if let Some(chain) = &self.chain {
            chain
                .confirm_asked()
                .expect("a confirmed change of a message applies");
        }
        self.reload(doc);
    }

    /// Read and write the chat's messages through `chain` from now on.
    pub(crate) fn attach_chain(&mut self, chain: ui::framework::Chain<crate::parts::ChatPart>) {
        self.chain = Some(chain);
        self.chain_seen = None;
        self.reselect();
    }
}
