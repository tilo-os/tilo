use std::path::PathBuf;

use super::{AutoCommit, CodeApp, EditorDoc, LspStatus, TreeId};
use ui::widget::toast::ToastKind;

pub(crate) struct PendingFormat {
    node: TreeId,
    /// The text the server was asked about: its edits apply to this text only.
    base: String,
    then: AfterFormat,
}

pub(crate) enum AfterFormat {
    Nothing,
    Save,
    SaveEffect(Option<PathBuf>),
}

impl CodeApp {
    fn formats_on_save(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.get().format_on_save)
    }

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
        if base != self.lsp_sent_text {
            self.lsp_version += 1;
            client.did_change(&uri, self.lsp_version, &base);
            self.lsp_sent_text.clone_from(&base);
        }
        client.request_formatting(&uri, u32::try_from(indent).unwrap_or(4), true)
    }

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
                    Self::note(
                        format!("Could not format the file: {error}"),
                        ToastKind::Error,
                    );
                }
            }
            Ok(_) => {}
            Err(error) => Self::note(format!("Not formatted: {error}"), ToastKind::Error),
        }
        self.after_format(doc, pending.node, pending.then);
    }

    pub(super) fn after_format(&mut self, doc: &AutoCommit, node: TreeId, then: AfterFormat) {
        match then {
            AfterFormat::Nothing => {}
            AfterFormat::Save => {
                self.save_buffer(doc, node, false);
            }
            AfterFormat::SaveEffect(target) => {
                self.save_to(doc, node, target, super::file_workflow::AfterSave::Effect);
            }
        }
    }
}
