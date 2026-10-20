use std::path::PathBuf;

use super::{AutoCommit, CodeApp, EditorDoc, LspStatus, TreeId};

/// A file the language server is formatting.
pub(crate) struct PendingFormat {
    node: TreeId,
    /// The text the server was asked about: its edits apply to this text only.
    base: String,
    then: AfterFormat,
}

/// What follows once a file was formatted (or the formatter said why not).
pub(crate) enum AfterFormat {
    Nothing,
    /// The person's Save: written unless the file changed on disk meanwhile.
    Save,
    /// The save an effect asked for, to its own file or a new destination.
    SaveEffect(Option<PathBuf>),
}

impl CodeApp {
    /// Whether formatting `node` is what the person wants of a save.
    fn formats_on_save(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.get().format_on_save)
    }

    /// Start a save of `node`: with format on save, ask its language server
    /// to format it first and return `None`, the save following the answer.
    /// Otherwise hand `after` back to save now. A second save while the first
    /// still waits on the formatter saves as the file is.
    pub(super) fn format_before_saving(
        &mut self,
        doc: &AutoCommit,
        node: TreeId,
        after: AfterFormat,
    ) -> Option<AfterFormat> {
        if !self.formats_on_save() || self.formatting.take().is_some() {
            self.formatted = None;
            return Some(after);
        }
        if self.request_format(doc, node, after) {
            return None;
        }
        self.formatting.take().map(|pending| pending.then)
    }

    /// Ask the language server serving `node` for the edits that format it,
    /// `then` to follow its answer. Returns `false` when no running server
    /// serves the file; `then` is kept in [`Self::formatting`] either way so
    /// a caller can take it back.
    pub(super) fn request_format(
        &mut self,
        doc: &AutoCommit,
        node: TreeId,
        then: AfterFormat,
    ) -> bool {
        let base = EditorDoc::read_buffer_key(doc, &self.buffer_key(node)).unwrap_or_default();
        let indent = self
            .session
            .buffer_of(node)
            .map_or(4, |buffer| buffer.language.indent_width());
        self.formatted = None;
        self.formatting = Some(PendingFormat {
            node,
            base: base.clone(),
            then,
        });
        let uri = self.node_uri(node);
        let serving = matches!(
            self.lsp_status,
            LspStatus::Ready { .. } | LspStatus::Busy { .. }
        ) && self.lsp_uri.as_deref() == Some(uri.as_str());
        let Some(client) = self.lsp.as_mut().filter(|_| serving) else {
            return false;
        };
        // The server formats the text it was last told of: tell it the
        // latest first.
        if base != self.lsp_sent_text {
            self.lsp_version += 1;
            client.did_change(&uri, self.lsp_version, &base);
            self.lsp_sent_text.clone_from(&base);
        }
        client.request_formatting(&uri, u32::try_from(indent).unwrap_or(4), true)
    }

    /// Take up the formatter's answer: apply its edits as one text edit of
    /// the file, unless the file changed since it was asked, then do what
    /// was to follow. A server that stopped answers that it did not format.
    pub(super) fn apply_formatted(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if self.formatting.is_none() {
            self.formatted = None;
            return;
        }
        let answer = match self.formatted.take() {
            Some(answer) => answer,
            None if self.lsp.is_none() => Err("the language server stopped".into()),
            None => return,
        };
        let Some(pending) = self.formatting.take() else {
            return;
        };
        let key = self.buffer_key(pending.node);
        let current = EditorDoc::read_buffer_key(doc, &key).unwrap_or_default();
        let formatted = answer.and_then(|edits| {
            if current == pending.base {
                crate::lsp::apply_text_edits(&current, &edits)
            } else {
                Err("the file changed while it was being formatted".into())
            }
        });
        match formatted {
            Ok(next) if next != current => {
                if let Err(error) = self.dispatch_text_action(doc, key, &current, &next) {
                    self.note(format!("Could not format the file: {error}"), true);
                }
            }
            Ok(_) => {}
            Err(error) => self.note(format!("Not formatted: {error}"), true),
        }
        self.after_format(doc, pending.node, pending.then);
    }

    /// Do what was to follow formatting `node`.
    pub(super) fn after_format(&mut self, doc: &AutoCommit, node: TreeId, then: AfterFormat) {
        match then {
            AfterFormat::Nothing => {}
            AfterFormat::Save => {
                self.save_buffer(doc, node, false);
            }
            AfterFormat::SaveEffect(target) => {
                let saved = self.save_to(doc, node, target);
                self.finish_file_effect(if saved {
                    Ok(())
                } else {
                    Err("file could not be saved".into())
                });
            }
        }
    }
}
