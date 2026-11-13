//! Rewriting an imported package's `template view` bodies so their names resolve in the package
//! that defines them, not the one that calls them (`shell.md` §6: a template's body is its
//! package's, a caller's block is the caller's).
//!
//! A template's body is source in the defining package's namespace: it calls that package's
//! helpers (`desk_lists`), reads its roots (`shell`) and names its types (`Arrangement`). When
//! another package registers the template at import, every such unqualified reference is
//! rewritten to the qualified `alias.item` the importer's resolver reads, marked
//! [`ast::Ident::from_template_of`] so the resolver still admits the package's unoffered helpers.
//! Names bound inside the template — its parameters, `let`s, loop variables, view state — stand
//! as they are, and so do `std`'s and the built-ins: the caller's block is not in this AST (it is
//! inserted at the slot when the template expands), so it keeps the caller's own scope.

use std::collections::HashSet;

use crate::ast::{self, Expr, ExprKind, TypeExpr, TypeExprKind, UiKind, UiStmt};

/// The names of a package's top-level items a template's body may write by their plain name:
/// its views, fns, actions, types, enums, named roots and their state, and the names of its
/// types' fields' own functions and of its own aliases. What a qualified reference may reach.
pub struct PackageNames {
    pub items: HashSet<String>,
}

impl PackageNames {
    /// The names `meta` declares by their plain names.
    #[must_use]
    pub fn of(meta: &crate::meta::PackageMeta) -> Self {
        let mut items = HashSet::new();
        for v in &meta.views {
            items.insert(v.name.clone());
        }
        for f in &meta.fns {
            items.insert(f.name.clone());
        }
        for a in &meta.actions {
            items.insert(a.name.clone());
        }
        for t in &meta.types {
            items.insert(t.name.clone());
        }
        for e in &meta.enums {
            items.insert(e.name.clone());
        }
        for a in &meta.aliases {
            items.insert(a.name.clone());
        }
        for r in &meta.named_roots {
            items.insert(r.name.clone());
        }
        Self { items }
    }

    fn holds(&self, name: &str) -> bool {
        self.items.contains(name)
    }
}

/// Rewrite `view`'s body so every unqualified reference to one of `names` — a top-level item of
/// the package it came from — is qualified with `alias`, marked [`ast::Ident::from_template_of`]
/// with `package`. A name bound inside the template (a parameter, a `let`, a loop variable, a
/// `state`) is left as written, as are built-ins and `std`'s names (`names` holds none of them).
pub fn qualify(view: &mut ast::ViewDecl, alias: &str, package: &str, names: &PackageNames) {
    let mut scope = Scope::default();
    for p in &view.params {
        scope.bind(p.name.name.clone());
    }
    // The parameters' types name the package's types too (`a: Arrangement`).
    for p in &mut view.params {
        qualify_type(&mut p.ty, alias, package, names);
        if let Some(default) = &mut p.default {
            qualify_expr(default, alias, package, names, &scope);
        }
    }
    qualify_ui(&mut view.body, alias, package, names, &mut scope);
}

/// The names bound so far, innermost last: a plain name found here is the template's own and is
/// never qualified.
#[derive(Default)]
struct Scope {
    bound: Vec<HashSet<String>>,
}

impl Scope {
    fn bind(&mut self, name: String) {
        if let Some(top) = self.bound.last_mut() {
            top.insert(name);
        }
    }

    fn push(&mut self) {
        self.bound.push(HashSet::new());
    }

    fn pop(&mut self) {
        self.bound.pop();
    }

    fn holds(&self, name: &str) -> bool {
        self.bound.iter().any(|s| s.contains(name))
    }
}

fn qualify_ui(
    body: &mut [UiStmt],
    alias: &str,
    package: &str,
    names: &PackageNames,
    scope: &mut Scope,
) {
    scope.push();
    for stmt in body.iter_mut() {
        match &mut stmt.kind {
            UiKind::Call(call) => {
                // A block opens names (`list(xs) { x -> ... }`): they are the caller's own.
                let opens = !call.params.is_empty();
                if opens {
                    scope.push();
                    for p in &call.params {
                        scope.bind(p.name.clone());
                    }
                }
                qualify_expr(&mut call.call, alias, package, names, scope);
                if let Some(block) = &mut call.block {
                    qualify_ui(block, alias, package, names, scope);
                }
                if opens {
                    scope.pop();
                }
            }
            UiKind::For { var, iter, body } => {
                qualify_expr(iter, alias, package, names, scope);
                scope.push();
                scope.bind(var.name.clone());
                qualify_ui(body, alias, package, names, scope);
                scope.pop();
            }
            UiKind::If { cond, then, els } => {
                qualify_expr(cond, alias, package, names, scope);
                qualify_ui(then, alias, package, names, scope);
                if let Some(els) = els {
                    qualify_ui(els, alias, package, names, scope);
                }
            }
            UiKind::Match { scrutinee, arms } => {
                qualify_expr(scrutinee, alias, package, names, scope);
                for arm in arms {
                    scope.push();
                    for b in &arm.pattern.binds {
                        scope.bind(b.name.clone());
                    }
                    qualify_ui(&mut arm.body, alias, package, names, scope);
                    scope.pop();
                }
            }
            UiKind::Let { name, ty, value } => {
                qualify_expr(value, alias, package, names, scope);
                if let Some(ty) = ty {
                    qualify_type(ty, alias, package, names);
                }
                scope.bind(name.name.clone());
            }
            UiKind::IfLet { binds, then, els } => {
                // The bindings' values are read before the names are bound.
                for b in binds.iter_mut() {
                    qualify_expr(&mut b.value, alias, package, names, scope);
                }
                scope.push();
                for b in binds.iter() {
                    scope.bind(b.name.name.clone());
                }
                qualify_ui(then, alias, package, names, scope);
                scope.pop();
                if let Some(els) = els {
                    qualify_ui(els, alias, package, names, scope);
                }
            }
            UiKind::State(state) => {
                qualify_type(&mut state.ty, alias, package, names);
                if let Some(default) = &mut state.default {
                    qualify_expr(default, alias, package, names, scope);
                }
                scope.bind(state.name.name.clone());
            }
            UiKind::Fact { label, value, .. } => {
                qualify_expr(label, alias, package, names, scope);
                qualify_expr(value, alias, package, names, scope);
            }
            UiKind::Slot { body, .. } => {
                qualify_ui(body, alias, package, names, scope);
            }
            UiKind::On {
                param, at, action, ..
            } => {
                scope.push();
                for p in [param, at].into_iter().flatten() {
                    if let Some(ty) = &mut p.ty {
                        qualify_type(ty, alias, package, names);
                    }
                    scope.bind(p.name.name.clone());
                }
                qualify_expr(action, alias, package, names, scope);
                scope.pop();
            }
        }
    }
    scope.pop();
}

fn qualify_type(ty: &mut TypeExpr, alias: &str, package: &str, names: &PackageNames) {
    match &mut ty.kind {
        TypeExprKind::Named(path) => {
            if path.len() == 1 && names.holds(&path[0].name) {
                let span = path[0].span;
                path.insert(0, ast::Ident::plain(alias.to_owned(), span));
                if let Some(last) = path.last_mut() {
                    last.from_template_of = Some(package.to_owned());
                }
            }
        }
        TypeExprKind::Generic(_, args) => {
            for a in args {
                qualify_type(a, alias, package, names);
            }
        }
        TypeExprKind::Tuple(items) | TypeExprKind::Union(items) => {
            for a in items {
                qualify_type(a, alias, package, names);
            }
        }
        TypeExprKind::Fn(params, result) => {
            for a in params {
                qualify_type(a, alias, package, names);
            }
            qualify_type(result, alias, package, names);
        }
    }
}

fn qualify_expr(expr: &mut Expr, alias: &str, package: &str, names: &PackageNames, scope: &Scope) {
    match &mut expr.kind {
        ExprKind::Ident(name) => {
            if !scope.holds(name) && names.holds(name) {
                // `shell.name`: a field of the alias, marked as the template's own.
                let span = expr.span;
                let base = Expr {
                    kind: ExprKind::Ident(alias.to_owned()),
                    span,
                };
                let name_ident = ast::Ident {
                    name: std::mem::take(name),
                    span,
                    from_template_of: Some(package.to_owned()),
                };
                expr.kind = ExprKind::Field {
                    base: Box::new(base),
                    name: name_ident,
                };
            }
        }
        ExprKind::Field { base, .. } => {
            qualify_expr(base, alias, package, names, scope);
            // `a.field` of a template value: the field is not the package's item to qualify.
        }
        ExprKind::OptChain { base, .. } => qualify_expr(base, alias, package, names, scope),
        ExprKind::Call { callee, args, .. } => {
            qualify_expr(callee, alias, package, names, scope);
            for a in args {
                qualify_expr(&mut a.value, alias, package, names, scope);
            }
        }
        ExprKind::Str(parts) => {
            for part in parts.iter_mut() {
                if let ast::StrPart::Interp(inner) = part {
                    qualify_expr(inner, alias, package, names, scope);
                }
            }
        }
        ExprKind::Range { lo, hi } => {
            for e in [lo, hi].into_iter().flatten() {
                qualify_expr(e, alias, package, names, scope);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            qualify_expr(scrutinee, alias, package, names, scope);
            for arm in arms.iter_mut() {
                qualify_expr(&mut arm.body, alias, package, names, scope);
            }
        }
        ExprKind::Variant {
            args: Some(args), ..
        } => {
            for a in args {
                qualify_expr(&mut a.value, alias, package, names, scope);
            }
        }
        ExprKind::Unary { operand, .. } => qualify_expr(operand, alias, package, names, scope),
        ExprKind::Binary { lhs, rhs, .. } => {
            qualify_expr(lhs, alias, package, names, scope);
            qualify_expr(rhs, alias, package, names, scope);
        }
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            for x in items {
                qualify_expr(x, alias, package, names, scope);
            }
        }
        ExprKind::If { cond, then, els } => {
            qualify_expr(cond, alias, package, names, scope);
            qualify_block(then, alias, package, names, scope);
            if let Some(els) = els {
                qualify_expr(els, alias, package, names, scope);
            }
        }
        ExprKind::IfLet { then, els, .. } => {
            qualify_block(then, alias, package, names, scope);
            if let Some(els) = els {
                qualify_expr(els, alias, package, names, scope);
            }
        }
        ExprKind::Block(block) => qualify_block(block, alias, package, names, scope),
        ExprKind::Lambda { body, .. } => qualify_expr(body, alias, package, names, scope),
        ExprKind::Try(inner) | ExprKind::Suggest(inner) => {
            qualify_expr(inner, alias, package, names, scope);
        }
        ExprKind::Root(ty) => qualify_type(ty, alias, package, names),
        _ => {}
    }
}

fn qualify_block(
    block: &mut ast::Block,
    alias: &str,
    package: &str,
    names: &PackageNames,
    scope: &Scope,
) {
    for stmt in &mut block.stmts {
        match &mut stmt.kind {
            ast::StmtKind::Let { value, ty, .. } => {
                qualify_expr(value, alias, package, names, scope);
                if let Some(ty) = ty {
                    qualify_type(ty, alias, package, names);
                }
            }
            ast::StmtKind::Assign { value, place, .. } => {
                qualify_expr(value, alias, package, names, scope);
                for seg in &mut place.segs {
                    if !scope.holds(&seg.name) && names.holds(&seg.name) {
                        seg.from_template_of = Some(package.to_owned());
                    }
                }
            }
            ast::StmtKind::For { var, iter, body } => {
                let _ = var;
                qualify_expr(iter, alias, package, names, scope);
                qualify_block(body, alias, package, names, scope);
            }
            ast::StmtKind::Return(e) => {
                if let Some(e) = e {
                    qualify_expr(e, alias, package, names, scope);
                }
            }
            ast::StmtKind::Expr(e) => qualify_expr(e, alias, package, names, scope),
        }
    }
}
