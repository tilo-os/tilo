//! What a type's `derive`d keys mean, read against the type's fields, its parent field and the
//! keys declared before them ([`crate::derived`]).

use super::{Expr, ExprKind, FieldInfo, IdsLock, MergeKind, Resolver, Span, Ty, UnOp, ast, id_of};
use crate::derived::{Cmp, Derived};

/// Whether a derived key's value is a flag or a count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Bool,
    Int,
}

/// What a key is read against: the type's name, the key's own name, the type's fields, the keys
/// declared so far (the key itself last), every key the type declares, and the parent field an
/// item of the type stands under.
struct Context<'a> {
    owner: &'a str,
    key: &'a str,
    fields: &'a [FieldInfo],
    keys: &'a [(String, Kind)],
    declared: &'a [String],
    parent: Option<&'a str>,
}

/// Whether an expression names the items standing under the item (`children.count`).
fn is_children(base: &Expr) -> bool {
    matches!(&base.kind, ExprKind::Ident(name) if name == "children")
}

/// Whether an expression names the items standing above the item, through the type's parent
/// field (`parent.all(done)`).
fn is_parent(base: &Expr, parent: Option<&str>) -> bool {
    match (&base.kind, parent) {
        (ExprKind::Ident(name), Some(parent)) => name == parent,
        _ => false,
    }
}

/// The comparison an operator is, when it is one.
fn comparison(op: ast::BinOp) -> Option<Cmp> {
    Some(match op {
        ast::BinOp::Lt => Cmp::Lt,
        ast::BinOp::Le => Cmp::Le,
        ast::BinOp::Gt => Cmp::Gt,
        ast::BinOp::Ge => Cmp::Ge,
        ast::BinOp::Eq => Cmp::Eq,
        ast::BinOp::Ne => Cmp::Ne,
        _ => return None,
    })
}

/// Whether a derived key's expression holds a flag or a count.
fn kind_of(cx: &Context<'_>, expr: &Derived<String>) -> Option<Kind> {
    Some(match expr {
        Derived::Bool(_)
        | Derived::Not(_)
        | Derived::And(_)
        | Derived::Or(_)
        | Derived::Compare { .. }
        | Derived::All { .. }
        | Derived::Any { .. }
        | Derived::Above { .. } => Kind::Bool,
        Derived::Int(_) | Derived::Children | Derived::Count { .. } => Kind::Int,
        Derived::Field(name) => {
            if let Some((_, kind)) = cx.keys.iter().find(|(k, _)| k == name) {
                return Some(*kind);
            }
            match cx.fields.iter().find(|f| &f.name == name)?.ty {
                Ty::Int => Kind::Int,
                _ => Kind::Bool,
            }
        }
    })
}

impl Resolver<'_> {
    /// The type's derived keys (`derive folded: Bool = done and children.all(folded)`): values
    /// the store works out from the collection, held as the type's fields so a view reads them
    /// as it reads any other, and written by nothing ([`crate::derived`]). They are read once the
    /// fields and roles are known, as a key names its own fields and the parent field it reads
    /// the items under it through.
    pub(super) fn derived_keys(
        &mut self,
        owner: &str,
        decl: &ast::TypeDecl,
        fields: &mut Vec<FieldInfo>,
        lock: &IdsLock,
    ) {
        if decl.derives.is_empty() {
            return;
        }
        let parent = fields
            .iter()
            .find(|f| {
                f.role
                    .as_ref()
                    .is_some_and(crate::types::FieldRole::is_parent)
            })
            .map(|f| f.name.clone());
        // What every key of the type is declared as, for a key that names one before it.
        let mut declared: Vec<String> = Vec::new();
        let mut kinds: Vec<Option<Kind>> = Vec::new();
        for d in &decl.derives {
            let name = &d.name.name;
            if fields.iter().any(|f| &f.name == name) || declared.iter().any(|k| k == name) {
                self.err(
                    "E119",
                    d.name.span,
                    format!("`{owner}` already has a field `{name}`; a derived key is a field of the type, and one name is one value"),
                );
                kinds.push(None);
                continue;
            }
            let ty = self.resolve_type(&d.ty, true);
            let want = match ty {
                Ty::Bool => Kind::Bool,
                Ty::Int => Kind::Int,
                other => {
                    let shown = other.display(&self.env.app);
                    self.err(
                        "E119",
                        d.ty.span,
                        format!("a derived key is a `Bool` or an `Int`, a flag or a count; `{name}` is declared {shown}"),
                    );
                    declared.push(name.clone());
                    kinds.push(None);
                    continue;
                }
            };
            declared.push(name.clone());
            kinds.push(Some(want));
        }
        // A key reads the keys the type declares before it, and itself: `folded` reads the
        // `folded` of the items under it, which is the same key of another item of the type.
        let mut keys: Vec<(String, Kind)> = Vec::new();
        for (d, want) in decl.derives.iter().zip(kinds) {
            let name = &d.name.name;
            let Some(want) = want else { continue };
            keys.push((name.clone(), want));
            let cx = Context {
                owner,
                key: name,
                fields,
                keys: &keys,
                declared: &declared,
                parent: parent.as_deref(),
            };
            let Some(expr) = self.derived_expr(&cx, &d.expr) else {
                continue;
            };
            if expr.reads_children() && expr.reads_above() {
                self.err(
                    "E119",
                    d.expr.span,
                    format!(
                        "`{owner}.{name}` reads the items under the item and the items above it: a key reads one way, as two keys read both"
                    ),
                );
                continue;
            }
            if kind_of(&cx, &expr) != Some(want) {
                let written = if kind_of(&cx, &expr).is_some_and(|k| k == Kind::Int) {
                    "an `Int`"
                } else {
                    "a `Bool`"
                };
                self.err(
                    "E119",
                    d.expr.span,
                    format!(
                        "`{owner}.{name}` is declared {} and its expression is {written}; write the one the declaration says",
                        if want == Kind::Int { "an `Int`" } else { "a `Bool`" }
                    ),
                );
                continue;
            }
            let default = match want {
                Kind::Bool => serde_json::Value::Bool(false),
                Kind::Int => serde_json::Value::Number(0.into()),
            };
            let ty = match want {
                Kind::Bool => Ty::Bool,
                Kind::Int => Ty::Int,
            };
            fields.push(FieldInfo {
                name: name.clone(),
                id: id_of(lock, &format!("{owner}.{name}")),
                ty,
                merge: MergeKind::Register,
                doc: d.doc.join("\n"),
                default: Some(default),
                role: None,
                derived: Some(expr),
            });
        }
    }

    /// What a derived key's expression computes, checked against the type's fields and its
    /// parent field: a field's value, `not`, `and`, `or`, comparisons of counts, and
    /// `children.count`, `children.count(x)`, `children.all(x)`, `children.any(x)` and
    /// `parent.all(x)`.
    fn derived_expr(&mut self, cx: &Context<'_>, e: &Expr) -> Option<Derived<String>> {
        let at = format!("`{}.{}`", cx.owner, cx.key);
        let no_parent = |me: &mut Self, span: Span| {
            me.err(
                "E119",
                span,
                format!(
                    "{at} reads the items under or above the item, so `{}` says which field is the one an item stands under: write `parent: Option<{}> (parent) = .none`",
                    cx.owner, cx.owner
                ),
            );
        };
        Some(match &e.kind {
            ExprKind::Bool(b) => Derived::Bool(*b),
            ExprKind::Int(i) => Derived::Int(*i),
            ExprKind::Ident(name) => {
                // Another key of the type, or one of its own: a key reads the same key of
                // another item (`children.all(folded)`).
                if cx.keys.iter().any(|(k, _)| k == name) {
                    return Some(Derived::Field(name.clone()));
                }
                let Some(f) = cx.fields.iter().find(|f| &f.name == name) else {
                    if cx.declared.iter().any(|k| k == name) {
                        self.err(
                            "E119",
                            e.span,
                            format!("{at} reads `{name}`, which is declared after it: a derived key reads the keys declared before it, and itself"),
                        );
                        return None;
                    }
                    let names: Vec<&str> = cx.fields.iter().map(|f| f.name.as_str()).collect();
                    self.err(
                        "E119",
                        e.span,
                        format!(
                            "{at} reads `{name}`, which `{}` has no field of; its fields are: {}",
                            cx.owner,
                            names.join(", ")
                        ),
                    );
                    return None;
                };
                if !matches!(f.ty, Ty::Bool | Ty::Int) {
                    let shown = f.ty.display(&self.env.app);
                    self.err(
                        "E119",
                        e.span,
                        format!("{at} reads `{name}`, a {shown}: a derived key is worked out from flags and counts, never from text, a day or a record"),
                    );
                    return None;
                }
                Derived::Field(name.clone())
            }
            ExprKind::Unary {
                op: UnOp::Not,
                operand,
            } => {
                let inner = self.derived_bool(cx, operand)?;
                Derived::Not(Box::new(inner))
            }
            ExprKind::Binary { op, .. } if matches!(op, ast::BinOp::And | ast::BinOp::Or) => {
                let mut parts = self.derived_parts(cx, e, *op)?;
                if parts.len() == 1 {
                    parts.pop()?
                } else if matches!(op, ast::BinOp::And) {
                    Derived::And(parts)
                } else {
                    Derived::Or(parts)
                }
            }
            ExprKind::Binary { op, lhs, rhs } if comparison(*op).is_some() => {
                let cmp = comparison(*op)?;
                let left = self.derived_expr(cx, lhs)?;
                let right = self.derived_expr(cx, rhs)?;
                for (side, expr) in [(lhs, &left), (rhs, &right)] {
                    if kind_of(cx, expr) != Some(Kind::Int) {
                        self.err(
                            "E119",
                            side.span,
                            format!("{at} compares something that is not a count. A comparison is of two counts, each a number or `children.count`"),
                        );
                        return None;
                    }
                }
                Derived::Compare {
                    op: cmp,
                    left: Box::new(left),
                    right: Box::new(right),
                }
            }
            ExprKind::Field { base, name } if is_children(base) && name.name == "count" => {
                if cx.parent.is_none() {
                    no_parent(self, e.span);
                    return None;
                }
                Derived::Children
            }
            ExprKind::Call { callee, args } => {
                let ExprKind::Field { base, name } = &callee.kind else {
                    self.err("E119", e.span, format!("{at} computes {}. A derived key reads a field, or the items under or above it: `children.count`, `children.count(x)`, `children.all(x)`, `children.any(x)` or `parent.all(x)`", "a call it has no way to work out"));
                    return None;
                };
                if is_parent(base, cx.parent) {
                    if cx.parent.is_none() {
                        no_parent(self, e.span);
                        return None;
                    }
                    if name.name != "all" || args.len() != 1 || args[0].name.is_some() {
                        self.err("E119", e.span, format!("`{}(…)` is not something a derived key computes; the items above the item are read by `{}.all(x)`", name.name, name.name));
                        return None;
                    }
                    let holds = self.derived_bool(cx, &args[0].value)?;
                    return Some(Derived::Above {
                        holds: Box::new(holds),
                    });
                }
                if !is_children(base) {
                    self.err("E119", e.span, format!("{at} computes `{}(…)`; a derived key reads a field, the items under it through `children`, or the items above it through its parent field", name.name));
                    return None;
                }
                if cx.parent.is_none() {
                    no_parent(self, e.span);
                    return None;
                }
                if args.len() > 1 || args[0].name.is_some() {
                    self.err("E119", e.span, format!("`children.{}` takes the one condition the items under the item are counted by or checked for, written plainly: `children.{}(not done)`", name.name, name.name));
                    return None;
                }
                match name.name.as_str() {
                    "count" if args.is_empty() => Derived::Children,
                    "count" => {
                        let holds = self.derived_bool(cx, &args[0].value)?;
                        Derived::Count {
                            holds: Box::new(holds),
                        }
                    }
                    "all" | "any" if args.len() == 1 => {
                        let holds = self.derived_bool(cx, &args[0].value)?;
                        if name.name == "all" {
                            Derived::All {
                                holds: Box::new(holds),
                            }
                        } else {
                            Derived::Any {
                                holds: Box::new(holds),
                            }
                        }
                    }
                    _ => {
                        self.err("E119", e.span, format!("`children.{}` is not something a derived key computes; the items under the item are `children.count`, `children.count(x)`, `children.all(x)` and `children.any(x)`", name.name));
                        return None;
                    }
                }
            }
            _ => {
                self.err("E119", e.span, format!("{at} computes {}, which is not worked out from the collection; a derived key reads a field, the items under it or the items above it, and combines them with `not`, `and`, `or` and a comparison of counts", "an expression no derivation reads"));
                return None;
            }
        })
    }

    /// A part of a derived key's expression that must be a flag.
    fn derived_bool(&mut self, cx: &Context<'_>, e: &Expr) -> Option<Derived<String>> {
        let expr = self.derived_expr(cx, e)?;
        if kind_of(cx, &expr) == Some(Kind::Bool) {
            return Some(expr);
        }
        self.err(
            "E119",
            e.span,
            format!("`{}.{}` checks whether this holds, and it is a count; write a flag, or compare the count where a view reads it", cx.owner, cx.key),
        );
        None
    }

    /// The parts of a chain of `and`s (or of `or`s), each a flag.
    fn derived_parts(
        &mut self,
        cx: &Context<'_>,
        e: &Expr,
        op: ast::BinOp,
    ) -> Option<Vec<Derived<String>>> {
        match &e.kind {
            ExprKind::Binary {
                op: inner,
                lhs,
                rhs,
            } if *inner == op => {
                let mut parts = self.derived_parts(cx, lhs, op)?;
                parts.push(self.derived_bool(cx, rhs)?);
                Some(parts)
            }
            _ => Some(vec![self.derived_bool(cx, e)?]),
        }
    }
}
