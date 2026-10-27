//! What the window reads of a package's declarations: a type, a resource's title, an enum's
//! case labels. What a view shows of them it reads itself (`actions(r)`, `meta(r)`).

use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::ResourceRef;
use tilolang_tilo::runtime::meta::{Meta, TypeMeta};
use ui::icons::Icon;

use crate::reconcile::{Case, display};

/// An icon a package names (`.pin`), when the platform has it.
#[must_use]
pub fn icon_named(name: &str) -> Option<Icon> {
    Icon::from_name(&name.replace('_', "-"))
}

/// The package that declares the type `ty` (an id), with the type.
#[must_use]
pub fn type_of(host: &Host, ty: &str) -> Option<(Meta, TypeMeta)> {
    host.packages().into_iter().find_map(|package| {
        let declared = package
            .meta
            .types
            .iter()
            .find(|declared| declared.id == ty)?
            .clone();
        Some((package.meta, declared))
    })
}

/// A name in sentence case: `new_note` and `NewNote` read "New note".
#[must_use]
pub fn sentence(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for ch in name.chars() {
        if ch == '_' || ch == ' ' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if ch.is_uppercase() && !word.is_empty() {
            words.push(std::mem::take(&mut word));
            word.extend(ch.to_lowercase());
        } else {
            word.extend(ch.to_lowercase());
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let joined = words.join(" ");
    let mut chars = joined.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The title of a resource: its first `Text` field marked `title`; else, for a package's root,
/// the package's title; else its type's name.
#[must_use]
pub fn title_of(host: &Host, resource: &ResourceRef) -> String {
    let Some((meta, ty)) = type_of(host, &resource.ty) else {
        return String::new();
    };
    let titled = ty
        .fields
        .iter()
        .find(|field| field.role.as_deref() == Some("title"));
    if let Some(field) = titled
        && let Ok(value) = host.runtime().read(resource, &field.id)
    {
        let text = display(&value);
        if !text.trim().is_empty() {
            return text;
        }
    }
    if resource.path.is_empty() && meta.roots.contains(&ty.id) {
        return meta.title;
    }
    sentence(&ty.name)
}

/// The cases of the enum `id` with what a control shows for each.
#[must_use]
pub fn cases(host: &Host, id: &str) -> Vec<Case> {
    host.packages()
        .into_iter()
        .find_map(|package| {
            package
                .meta
                .enums
                .iter()
                .find(|declared| declared.id == id || declared.name == id)
                .map(|declared| {
                    declared
                        .variants
                        .iter()
                        .map(|variant| Case {
                            name: variant.name.clone(),
                            label: if variant.label.is_empty() {
                                sentence(&variant.name)
                            } else {
                                variant.label.clone()
                            },
                            icon: variant.icon.as_deref().and_then(icon_named),
                        })
                        .collect()
                })
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_in_sentence_case() {
        assert_eq!(sentence("new_note"), "New note");
        assert_eq!(sentence("NewNote"), "New note");
        assert_eq!(sentence("toggle_pin"), "Toggle pin");
        assert_eq!(sentence("Note"), "Note");
        assert_eq!(sentence(""), "");
    }
}
