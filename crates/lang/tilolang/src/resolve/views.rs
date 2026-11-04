//! The declarations that attach views to a type: the row a list shows for each item of it,
//! and the views a window of it switches between. A view neither names is the app's own
//! helper, which no window offers.

use crate::ast;
use crate::types::Ty;

use super::{CKind, Resolver};

impl Resolver<'_> {
    /// `row view v for T`: a view of one `T` whose body is one list row, `row(title, ...)`.
    pub(super) fn row_view_decl(&mut self, d: &ast::DefaultViewDecl) {
        let Some(key) = self.own_record(&d.ty, "row view") else {
            return;
        };
        if self.env.row_views.iter().any(|(k, _)| *k == key) {
            self.err(
                "E093",
                d.span,
                format!("`{}` has two row views; a type has one", key.name),
            );
            return;
        }
        let fits = self
            .env
            .own_callable(&d.view.name)
            .filter(|c| c.kind == CKind::View)
            .map(|c| matches!(c.params.as_slice(), [p] if p.ty == Ty::Record(key.clone())));
        match fits {
            Some(true) => self.env.row_views.push((key, d.view.name.clone())),
            Some(false) => self.err(
                "E093",
                d.view.span,
                format!(
                    "the row view of `{}` takes the item and nothing else: `view {}(x: {})`",
                    key.name, d.view.name, key.name
                ),
            ),
            None => self.err(
                "E093",
                d.view.span,
                format!("`{}` is not a view of this package", d.view.name),
            ),
        }
    }

    /// `views a, b for T`: the views a window of the type switches between, in the order
    /// written. Each is a view of one `T` and nothing else; a view not named stays the app's own
    /// helper, which no window offers.
    pub(super) fn views_decl(&mut self, d: &ast::ViewsDecl) {
        let Some(key) = self.own_record(&d.ty, "views") else {
            return;
        };
        if self.env.views_of.iter().any(|(k, _, _)| *k == key) {
            self.err(
                "E011",
                d.span,
                format!("`{}` declares its views twice", key.name),
            );
            return;
        }
        let mut kept = Vec::new();
        for view in &d.views {
            if kept.contains(&view.name) {
                self.err(
                    "E011",
                    view.span,
                    format!(
                        "`{}` is named twice in the views of `{}`",
                        view.name, key.name
                    ),
                );
                continue;
            }
            let fits = self
                .env
                .own_callable(&view.name)
                .filter(|c| c.kind == CKind::View)
                .map(|c| matches!(c.params.as_slice(), [p] if p.ty == Ty::Record(key.clone())));
            match fits {
                Some(true) => kept.push(view.name.clone()),
                Some(false) => self.err(
                    "E018",
                    view.span,
                    format!(
                        "a view of `{}` takes the resource and nothing else: `view {}(x: {})`",
                        key.name, view.name, key.name
                    ),
                ),
                None => self.err(
                    "E018",
                    view.span,
                    format!("`{}` is not a view of this package", view.name),
                ),
            }
        }
        self.env.views_of.push((key, kept, d.span));
    }
}
