//! The declarations that attach views to a type: the row a list shows for each item of it, and the views a
//! window of it switches between. A view neither names is the app's own helper, which no window offers.

use crate::ast;
use crate::types::Ty;

use super::{CKind, Resolver, ViewsInfo};

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

    /// `views a, b for T`: the views a window of the type switches between, in the order written. Each is a
    /// view of one `T` and nothing else; a view not named stays the app's own helper.
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
        for entry in &d.views {
            let view = &entry.view;
            if kept.iter().any(|k: &ViewsInfo| k.name == view.name) {
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
                Some(true) => {
                    let (label, icon) = self.view_options(&entry.opts);
                    kept.push(ViewsInfo {
                        name: view.name.clone(),
                        label,
                        icon,
                    });
                }
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

    /// How a view of a window's switch is offered: `(label: "Board")` and `(icon: .columns)`. Without a label
    /// the switch derives one from the view's, leaving the type's name off.
    fn view_options(&mut self, opts: &[ast::ActionOpt]) -> (Option<String>, Option<String>) {
        let mut label = None;
        let mut icon = None;
        for opt in opts {
            match opt.name.name.as_str() {
                "label" if label.is_some() => {
                    self.err("E090", opt.name.span, "`label:` is given twice");
                }
                "label" => match crate::resolve::plain_text(&opt.value) {
                    Some(text) if !text.trim().is_empty() => label = Some(text),
                    _ => self.err(
                        "E090",
                        opt.value.span,
                        "`label:` takes the short name of the view in quotes, such as `label: \"Board\"`",
                    ),
                },
                "icon" if icon.is_some() => {
                    self.err("E090", opt.name.span, "`icon:` is given twice");
                }
                "icon" => match &opt.value.kind {
                    ast::ExprKind::Variant { name, args: None } => icon = self.icon(name),
                    _ => self.err(
                        "E090",
                        opt.value.span,
                        "`icon:` takes an icon written with a dot, such as `icon: .columns`",
                    ),
                },
                other => self.err(
                    "E090",
                    opt.name.span,
                    format!(
                        "a view of a window's switch takes the options `label:` and `icon:`; `{other}:` is not one"
                    ),
                ),
            }
        }
        (label, icon)
    }
}
