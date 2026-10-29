//! Rich text as the person a store holds resources for reads it.
//!
//! A reference's run keeps the title its resource had when the text was written. Wherever
//! rich text becomes plain text here (what names a resource, what search finds and shows, a
//! row's excerpt, what an agent reads) each reference is read for the reader instead
//! ([`tilolang_runtime::rich::Reading`]): a resource they hold by what it is called now, one
//! they do not hold as only being there, in the chip's wording. A room's store holds only the
//! records its reader was sent, so what the store holds is what they may read.
//!
//! The store ([`crate::store::TiloStore`]) and a snapshot of it read rich text this one way
//! ([`Holdings`]); what names a resource that itself names others is read the same way, each
//! resource once.

use tilolang_runtime::rich::{Reading, Reference};
use tilolang_runtime::store::FieldValue;
use tilolang_runtime::value::ResourceRef;

/// What the person reading holds, as rich text is read for them.
pub trait Holdings {
    /// The type of the resource `at`, when the reader holds it.
    fn held(&self, at: &ResourceRef) -> Option<String>;
    /// The first text of the resource `at` as stored (a text or rich text), which names it.
    fn first_value(&self, at: &ResourceRef) -> Option<FieldValue>;
    /// Whether resources of the app `app` are kept here: a package of it is loaded.
    fn keeps(&self, app: &str) -> bool;
    /// What a reference to a resource the reader does not hold says, in their language.
    fn unshared(&self) -> std::sync::Arc<str>;
}

/// How `reference` reads for the reader `holdings` holds resources for: a person as the text
/// names them, a resource of a package kept here by what it is called now when they hold it
/// and as not shared when they do not, a resource only another app can say how it reads as
/// its run.
pub fn reading(holdings: &dyn Holdings, reference: &Reference) -> Reading {
    reading_within(holdings, reference, &mut Vec::new())
}

/// The text value `value` ([`crate::reads::text_value`]) of the resource `of` (its id) as the
/// reader reads it; none for a value that holds no text. A reference in it back to `of` reads
/// it by its type.
pub fn said(holdings: &dyn Holdings, of: &str, value: FieldValue) -> Option<String> {
    said_within(holdings, value, &mut vec![of.to_owned()])
}

/// [`reading`], `within` the resources whose names are being read: a name that refers back to
/// one of them reads it by its type.
fn reading_within(
    holdings: &dyn Holdings,
    reference: &Reference,
    within: &mut Vec<String>,
) -> Reading {
    let Some(at) = reference.as_resource() else {
        return Reading::AsWritten;
    };
    if !holdings.keeps(&at.app) {
        return Reading::AsWritten;
    }
    let Some(mut target) = tilolang_runtime::rich::resource_id(reference)
        .and_then(|id| ResourceRef::parse_id(id, String::new()))
    else {
        return Reading::Unshared;
    };
    let Some(ty) = holdings.held(&target) else {
        return Reading::Unshared;
    };
    target.ty = ty;
    let id = target.id_string();
    let spoken = || {
        crate::schema::lookup(&target.ty)
            .map_or_else(|| target.ty.clone(), crate::schema::TypeSchema::spoken_name)
    };
    if within.contains(&id) {
        return Reading::Title(spoken());
    }
    within.push(id);
    let named = holdings
        .first_value(&target)
        .and_then(|value| said_within(holdings, value, within));
    within.pop();
    Reading::Title(named.unwrap_or_else(spoken))
}

fn said_within(
    holdings: &dyn Holdings,
    value: FieldValue,
    within: &mut Vec<String>,
) -> Option<String> {
    match value {
        FieldValue::Text(text) => Some(text),
        FieldValue::Rich(rich) => Some(rich.read_text(
            &mut |reference| reading_within(holdings, reference, within),
            &holdings.unshared(),
        )),
        _ => None,
    }
}
