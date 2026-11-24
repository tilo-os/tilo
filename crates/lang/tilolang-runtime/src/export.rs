//! The system's Export of a resource: what every resource's menu offers, saved as a file the
//! person keeps. A resource whose app says how its type exports (a `fn export(x: T) -> Text` of
//! its package, as Chat says a conversation is its messages in order) exports as that Markdown;
//! one with a document body as that body in Markdown under its title. Anything else is its
//! structured reading, which the host makes ([`crate::view_instance::ViewEvent::Export`]).

use crate::error::RuntimeError;
use crate::meta::FieldKind;
use crate::runtime::Runtime;
use crate::value::{ResourceRef, Value};

/// The name of the function an app declares to say how a type of its own exports: one parameter
/// naming the type, answering the Markdown.
pub const EXPORT_HOOK: &str = "export";

impl Runtime {
    /// The Markdown `at` exports as: what its app's `export` says of it, else its document body
    /// under its title. `None` for a resource whose app says nothing of it and that holds no
    /// document: its export is its structured reading.
    ///
    /// # Errors
    ///
    /// `NotFound` for a type this runtime does not run; the app's `export` failing, or answering
    /// something other than text; a store failure.
    pub fn export_markdown(&self, at: &ResourceRef) -> Result<Option<String>, RuntimeError> {
        let (pkg, info) = self
            .core
            .type_of_ref(&at.ty)
            .map_err(RuntimeError::NotFound)?;
        let hook = pkg.meta.fns.iter().find(|declared| {
            declared.name == EXPORT_HOOK
                && matches!(declared.params.as_slice(), [only]
                    if crate::platform::names_type(&pkg.meta, &only.ty, info.id.as_str()))
        });
        if let Some(hook) = hook {
            let made = self.core.call_pure(
                &pkg,
                &format!("n_{}", hook.id),
                &hook.params,
                &[Value::resource(at)],
                &hook.result,
            )?;
            return match made {
                Value::Text(markdown) => Ok(Some(markdown)),
                other => Err(RuntimeError::Package(format!(
                    "`{}`'s `export` answers the Markdown, not a {}",
                    pkg.meta.app,
                    other.kind_name()
                ))),
            };
        }
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
