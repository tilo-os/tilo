//! The system's Export of a resource that publishes no format of its own: a resource with a
//! document body exports as that body in Markdown under its title, and anything else as its
//! structured reading, which the host makes ([`crate::view_instance::ViewEvent::Export`]). An
//! app's own formats are export actions on its resource (`export:`), which the one Export…
//! lists instead.

use crate::error::RuntimeError;
use crate::meta::FieldKind;
use crate::runtime::Runtime;
use crate::value::{ResourceRef, Value};

impl Runtime {
    /// The Markdown `at` exports as, as its document body under its title. `None` for a
    /// resource that holds no document: its export is its structured reading.
    ///
    /// # Errors
    ///
    /// `NotFound` for a type this runtime does not run; a store failure.
    pub fn export_markdown(&self, at: &ResourceRef) -> Result<Option<String>, RuntimeError> {
        let (_, info) = self
            .core
            .type_of_ref(&at.ty)
            .map_err(RuntimeError::NotFound)?;
        // Its document: the rich text it names `body`, else the one it holds.
        let Some(body) = info
            .fields
            .iter()
            .filter(|field| field.kind == FieldKind::RichText)
            .min_by_key(|field| field.name != "body")
        else {
            return Ok(None);
        };
        let Value::Rich(rich) = self
            .core
            .read_value(at, body, None)
            .map_err(RuntimeError::Package)?
        else {
            return Ok(None);
        };
        let markdown = tilo_doc_codecs::rich::markdown_of_blocks(&rich.blocks);
        // A document named by its own first line says its title already.
        if info.name_field().is_some_and(|named| named.id == body.id) {
            return Ok(Some(markdown));
        }
        let title = self.title(at)?;
        Ok(Some(format!("# {title}\n\n{markdown}")))
    }
}
