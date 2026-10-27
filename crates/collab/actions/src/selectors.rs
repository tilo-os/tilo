//! Reading a published resource the way a skill's selector reads it.
//!
//! Every resource a running app publishes says what it is: its kind, its
//! label, and the words it shows beside it. A skill's
//! [`Selector`](tilo_selector::Selector) reads exactly that, so which skills
//! a resource may be asked with is answered from the live catalogue an agent,
//! a script and a screen reader see — never from anything a window draws and
//! never from a second index of the apps.

use crate::remote::CatalogueRow;
use tilo_selector::Subject;

/// The catalogue's field naming the resource kind a row is of, as
/// `Selector::to_syntax` writes it: `kind in [ebook.book]`.
pub const KIND: &str = "kind";

/// The catalogue's field naming a resource's label: the words it presents
/// itself with.
pub const LABEL: &str = "label";

/// The catalogue's field naming what a resource shows beside its label.
pub const BADGE: &str = "badge";

/// A catalogue row's resource, as a selector reads it.
pub struct RowSubject<'a>(pub &'a CatalogueRow);

impl Subject for RowSubject<'_> {
    fn kind(&self) -> &str {
        self.0.resource.kind.as_str()
    }

    fn says(&self, field: &str) -> Vec<&str> {
        match field {
            LABEL => std::iter::once(self.0.resource.label.as_str()).collect(),
            KIND => std::iter::once(self.0.resource.kind.as_str()).collect(),
            BADGE => self
                .0
                .resource
                .presentation
                .as_ref()
                .and_then(|presentation| presentation.badge.as_deref())
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// `row`'s resource, read the way a skill's selector reads it.
#[must_use]
pub fn subject(row: &CatalogueRow) -> RowSubject<'_> {
    RowSubject(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{CatalogueResource, CatalogueRow};
    use crate::resource::ResourcePresentation;
    use tilo_selector::{Predicate, Selector};

    fn row(kind: &str, label: &str, badge: Option<&str>) -> CatalogueRow {
        CatalogueRow {
            provider: "1".into(),
            app_id: "so.tilo.files".into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: "entry".into(),
                kind: kind.into(),
                label: label.into(),
                presentation: badge.map(|badge| ResourcePresentation {
                    detail: None,
                    badge: Some(badge.into()),
                    preview: None,
                }),
            },
            action: None,
            has_children: false,
            visibility: None,
        }
    }

    #[test]
    fn a_selector_reads_a_published_resource_by_its_kind_and_label() {
        let invoice = row("files.file", "Invoice 04.pdf", Some("PDF"));
        let notes = row("files.file", "Notes.txt", Some("Text"));
        let selector = Selector::parse(r#"files.file { label in ["Invoice 04.pdf"] }"#).unwrap();
        assert!(selector.matches(&subject(&invoice)));
        assert!(!selector.matches(&subject(&notes)));

        let pdfs = Selector::kinds(["files.file".to_owned()])
            .where_(Predicate::new("badge", ["PDF".to_owned()]));
        assert!(pdfs.matches(&subject(&invoice)));
        assert!(!pdfs.matches(&subject(&notes)));
        assert!(
            !Selector::kinds(["files.file".to_owned()])
                .where_(Predicate::new("author", ["Ada".to_owned()]))
                .matches(&subject(&invoice)),
            "a field the resource does not publish is never met"
        );
    }

    #[test]
    fn a_selector_of_another_kind_picks_out_nothing_here() {
        let invoices = row("files.file", "Invoice 04.pdf", None);
        assert!(!Selector::kinds(["ebook.book".to_owned()]).matches(&subject(&invoices)));
    }
}
