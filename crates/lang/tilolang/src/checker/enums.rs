//! Sum types: declared enums, the built-in `Option` and `Result`, and the platform's built-in
//! enums (`space` and the options of UI primitives). Variant literals, `match` (as a value, as
//! a statement and in a view body) and the tests and reads both share.

use super::expr::dotted;
use super::{Checker, Ex, conv, load_op, store_op};
use crate::abi::Imp;
use crate::ast::{self, Arg, Pattern};
use crate::diag::Span;
use crate::host::{RESULT_ERR, RESULT_ERROR, RESULT_OK, RESULT_TYPE, RESULT_VALUE};
use crate::ir::{self, Ek, LocalId, Stmt};
use crate::prims;
use crate::resolve::EnumInfo;
use crate::types::{Ty, TypeKey};

/// How the values of an enum type are held, and so how they are built, tested and read.
#[derive(Clone, Debug)]
pub(super) enum Shape {
    /// `Option<T>`: the host's option values (`some_*`, `none`, `is_some`, `unwrap_*`).
    Option(Ty),
    /// `Result<T, E>`: a host enum value with the built-in ids.
    Result(Ty, Ty),
    /// A declared enum: a host enum value with the declaration's ids.
    Enum(EnumInfo),
    /// A built-in enum of the platform: the variant's index.
    Choice(&'static str),
}

/// One variant as the lowering sees it: its name, the id the host knows it by, and its payload
/// fields (name, id, type).
pub(super) struct VariantShape {
    pub name: String,
    pub id: String,
    pub fields: Vec<(String, String, Ty)>,
}

impl Shape {
    pub fn of(checker: &Checker<'_>, ty: &Ty) -> Option<Self> {
        Some(match ty {
            Ty::Option(t) => Self::Option((**t).clone()),
            Ty::Result(t, e) => Self::Result((**t).clone(), (**e).clone()),
            Ty::Enum(key) => Self::Enum(checker.env.enum_info(key)?.clone()),
            Ty::Choice(kind) => Self::Choice(kind),
            _ => return None,
        })
    }

    pub fn variants(&self) -> Vec<VariantShape> {
        let plain = |name: &str| VariantShape {
            name: name.to_string(),
            id: name.to_string(),
            fields: Vec::new(),
        };
        match self {
            Self::Option(t) => vec![
                plain("none"),
                VariantShape {
                    name: "some".into(),
                    id: "some".into(),
                    fields: vec![("value".into(), "value".into(), t.clone())],
                },
            ],
            Self::Result(t, e) => vec![
                VariantShape {
                    name: RESULT_OK.into(),
                    id: RESULT_OK.into(),
                    fields: if *t == Ty::Unit {
                        Vec::new()
                    } else {
                        vec![(RESULT_VALUE.into(), RESULT_VALUE.into(), t.clone())]
                    },
                },
                VariantShape {
                    name: RESULT_ERR.into(),
                    id: RESULT_ERR.into(),
                    fields: vec![(RESULT_ERROR.into(), RESULT_ERROR.into(), e.clone())],
                },
            ],
            Self::Enum(info) => info
                .variants
                .iter()
                .map(|v| VariantShape {
                    name: v.name.clone(),
                    id: v.id.clone(),
                    fields: v
                        .fields
                        .iter()
                        .map(|f| (f.name.clone(), f.id.clone(), f.ty.clone()))
                        .collect(),
                })
                .collect(),
            Self::Choice(kind) => prims::choice(kind)
                .map(|c| c.variants.iter().map(|v| plain(v)).collect())
                .unwrap_or_default(),
        }
    }

    fn type_id(&self) -> String {
        match self {
            Self::Result(..) => RESULT_TYPE.to_string(),
            Self::Enum(info) => info.id.clone(),
            Self::Option(_) | Self::Choice(_) => String::new(),
        }
    }
}

fn names(vs: &[VariantShape]) -> String {
    vs.iter()
        .map(|v| format!(".{}", v.name))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Checker<'_> {
    /// The enum a dotted path names (`Status`, `todo.Status`), when it is one and no variable
    /// hides it.
    pub(super) fn enum_path(&self, e: &ast::Expr) -> Option<TypeKey> {
        let path = dotted(e)?;
        if path.first().is_some_and(|f| self.lookup(f).is_some()) {
            return None;
        }
        match path.as_slice() {
            [one] => self.env.own_enum(one).map(|e| e.key.clone()),
            [prefix @ .., last] => {
                let import = self.env.find_import(&prefix.join("."))?;
                let key = TypeKey {
                    app: import.app.clone(),
                    name: last.clone(),
                };
                self.env.enum_info(&key).map(|_| key)
            }
            [] => None,
        }
    }

    /// `.name` or `.name(args)`, resolved against the type the context expects.
    pub(super) fn variant(
        &mut self,
        name: &ast::Ident,
        args: Option<&[Arg]>,
        exp: Option<&Ty>,
        span: Span,
    ) -> Ex {
        let Some(want) = exp else {
            self.err("E037", span, format!("cannot tell which enum `.{}` belongs to here; write it with its type, such as `Status.{}`, or use it where the type is known", name.name, name.name));
            self.skip_args(args);
            return Ex::error();
        };
        if want.is_error() {
            self.skip_args(args);
            return Ex::error();
        }
        let Some(shape) = Shape::of(self, want) else {
            let w = self.show(want);
            self.err(
                "E020",
                span,
                format!("expected {w}, found the variant `.{}`", name.name),
            );
            self.skip_args(args);
            return Ex::error();
        };
        self.construct_variant(&shape, want, name, args, span)
    }

    fn skip_args(&mut self, args: Option<&[Arg]>) {
        for a in args.unwrap_or(&[]) {
            let _ = self.expr(&a.value, None);
        }
    }

    /// Builds a value of variant `name` of `shape` (whose type is `ty`).
    pub(super) fn construct_variant(
        &mut self,
        shape: &Shape,
        ty: &Ty,
        name: &ast::Ident,
        args: Option<&[Arg]>,
        span: Span,
    ) -> Ex {
        let variants = shape.variants();
        let Some(index) = variants.iter().position(|v| v.name == name.name) else {
            let t = self.show(ty);
            self.err(
                "E036",
                name.span,
                format!(
                    "{t} has no variant `.{}`; its variants: {}",
                    name.name,
                    names(&variants)
                ),
            );
            self.skip_args(args);
            return Ex::error();
        };
        let v = &variants[index];
        let given = args.unwrap_or(&[]);
        if v.fields.is_empty() && args.is_some_and(|a| !a.is_empty()) {
            self.err(
                "E022",
                span,
                format!("`.{}` carries no payload; write `.{}`", v.name, v.name),
            );
            self.skip_args(args);
            return Ex::error();
        }
        if !v.fields.is_empty() && given.len() != v.fields.len() {
            let fields: Vec<&str> = v.fields.iter().map(|f| f.0.as_str()).collect();
            self.err(
                "E022",
                span,
                format!(
                    "`.{}` takes its payload: `.{}({})`",
                    v.name,
                    v.name,
                    fields.join(", ")
                ),
            );
            self.skip_args(args);
            return Ex::error();
        }
        let mut values: Vec<Option<Ex>> = vec![None; v.fields.len()];
        for (i, a) in given.iter().enumerate() {
            let slot = match &a.name {
                Some(n) => v.fields.iter().position(|f| f.0 == n.name),
                None => Some(i),
            };
            let Some(slot) = slot.filter(|s| *s < v.fields.len()) else {
                let fields: Vec<&str> = v.fields.iter().map(|f| f.0.as_str()).collect();
                self.err(
                    "E022",
                    a.value.span,
                    format!("`.{}` has the payload fields {}", v.name, fields.join(", ")),
                );
                let _ = self.expr(&a.value, None);
                continue;
            };
            let fty = v.fields[slot].2.clone();
            let value = self.expr_as(&a.value, &fty);
            values[slot] = Some(value);
        }
        let values: Vec<Ex> = values
            .into_iter()
            .map(|x| x.unwrap_or_else(Ex::error))
            .collect();
        match shape {
            Shape::Choice(_) => Ex::new(Ek::Int(index as i64), ty.clone()),
            Shape::Option(_) => match values.into_iter().next() {
                None => Ex::host(Imp::NoneV, Vec::new(), ty.clone()),
                Some(x) => Self::some(x, ty),
            },
            Shape::Result(..) | Shape::Enum(_) => {
                let tmp = self.fresh(ty.clone());
                let mut stmts = vec![Stmt::Set(
                    tmp,
                    Ex::host(
                        Imp::EnumNew,
                        vec![Ex::str(&shape.type_id()), Ex::str(&v.id)],
                        ty.clone(),
                    ),
                )];
                for ((_, fid, fty), x) in v.fields.iter().zip(values) {
                    let (imp, c) = store_op(fty, Imp::EnumSetI, Imp::EnumSetF, Imp::EnumSetH);
                    stmts.push(Stmt::Eval(Ex::host(
                        imp,
                        vec![
                            Ex::local(tmp, ty.clone()),
                            Ex::str(fid),
                            conv(c, x, Ty::Int),
                        ],
                        Ty::Unit,
                    )));
                }
                Ex::new(
                    Ek::Seq(stmts, Box::new(Ex::local(tmp, ty.clone()))),
                    ty.clone(),
                )
            }
        }
    }

    /// Whether the value in `tmp` is variant `v`.
    fn variant_test(shape: &Shape, tmp: LocalId, ty: &Ty, index: usize, v: &VariantShape) -> Ex {
        let value = Ex::local(tmp, ty.clone());
        match shape {
            Shape::Option(_) => {
                let some = Ex::host(Imp::IsSome, vec![value], Ty::Bool);
                if v.name == "some" {
                    some
                } else {
                    Ex::new(Ek::Not(Box::new(some)), Ty::Bool)
                }
            }
            Shape::Choice(_) => Ex::new(
                Ek::Bin(
                    ir::BinOp::Eq,
                    Box::new(value),
                    Box::new(Ex::new(Ek::Int(index as i64), ty.clone())),
                ),
                Ty::Bool,
            ),
            Shape::Result(..) | Shape::Enum(_) => {
                Ex::host(Imp::EnumIs, vec![value, Ex::str(&v.id)], Ty::Bool)
            }
        }
    }

    /// Reads payload field `field` of the value in `tmp`, which is variant `v`.
    fn variant_field(shape: &Shape, tmp: LocalId, ty: &Ty, field: &(String, String, Ty)) -> Ex {
        let value = Ex::local(tmp, ty.clone());
        let fty = &field.2;
        match shape {
            Shape::Option(_) => Self::unwrap_opt(value, fty),
            _ => {
                let (imp, c) = load_op(fty, Imp::EnumGetI, Imp::EnumGetF, Imp::EnumGetH);
                let raw_ty = if c.is_some() { Ty::Int } else { fty.clone() };
                conv(
                    c,
                    Ex::host(imp, vec![value, Ex::str(&field.1)], raw_ty),
                    fty.clone(),
                )
            }
        }
    }

    /// Checks the patterns of a match over a value of type `ty`: each names a variant once, the
    /// binders fit its payload, and every variant is covered. Returns the shape and, per arm,
    /// the variant index.
    pub(super) fn plan_match(
        &mut self,
        ty: &Ty,
        patterns: &[&Pattern],
        span: Span,
    ) -> Option<(Shape, Vec<usize>)> {
        if ty.is_error() {
            return None;
        }
        let Some(shape) = Shape::of(self, ty) else {
            let t = self.show(ty);
            self.err(
                "E020",
                span,
                format!("`match` takes an enum value (a declared enum, an Option or a Result); this is {t}"),
            );
            return None;
        };
        let variants = shape.variants();
        let mut seen: Vec<usize> = Vec::new();
        let mut plan = Vec::new();
        let mut ok = true;
        for p in patterns {
            let Some(index) = variants.iter().position(|v| v.name == p.variant.name) else {
                let t = self.show(ty);
                self.err(
                    "E036",
                    p.variant.span,
                    format!(
                        "{t} has no variant `.{}`; its variants: {}",
                        p.variant.name,
                        names(&variants)
                    ),
                );
                ok = false;
                continue;
            };
            if seen.contains(&index) {
                self.err(
                    "E036",
                    p.span,
                    format!("`.{}` already has an arm", p.variant.name),
                );
                ok = false;
            }
            seen.push(index);
            let v = &variants[index];
            if !p.binds.is_empty() && p.binds.len() != v.fields.len() {
                let fields: Vec<&str> = v.fields.iter().map(|f| f.0.as_str()).collect();
                let msg = if v.fields.is_empty() {
                    format!("`.{}` carries no payload; write `.{}`", v.name, v.name)
                } else {
                    format!(
                        "`.{}` carries {} payload field(s) ({}); name each, `_` to skip one, or write `.{}` to skip them all",
                        v.name,
                        v.fields.len(),
                        fields.join(", "),
                        v.name
                    )
                };
                self.err("E036", p.span, msg);
                ok = false;
            }
            plan.push(index);
        }
        let missing: Vec<String> = variants
            .iter()
            .enumerate()
            .filter(|(i, _)| !seen.contains(i))
            .map(|(_, v)| format!(".{}", v.name))
            .collect();
        if !missing.is_empty() {
            self.err(
                "E035",
                span,
                format!(
                    "this match does not cover {}; add an arm for each",
                    missing.join(", ")
                ),
            );
            ok = false;
        }
        ok.then_some((shape, plan))
    }

    /// Declares the pattern's binders in the current scope, read from the value in `tmp`.
    fn bind_pattern(&mut self, shape: &Shape, tmp: LocalId, ty: &Ty, index: usize, p: &Pattern) {
        let variants = shape.variants();
        let Some(v) = variants.get(index) else { return };
        for (b, field) in p.binds.iter().zip(&v.fields) {
            if b.name == "_" {
                continue;
            }
            let read = Self::variant_field(shape, tmp, ty, field);
            let id = self.declare(&b.name, field.2.clone());
            self.emit(Stmt::Set(id, read));
        }
    }

    /// Evaluates the scrutinee into a local.
    fn scrutinee(&mut self, e: &ast::Expr) -> (LocalId, Ty, Stmt) {
        let ex = self.expr(e, None);
        let ty = ex.ty.clone();
        let tmp = self.fresh(ty.clone());
        (tmp, ty, Stmt::Set(tmp, ex))
    }

    /// `match` whose value is used.
    pub(super) fn match_value(
        &mut self,
        scrutinee: &ast::Expr,
        arms: &[ast::MatchArm],
        exp: Option<&Ty>,
        span: Span,
    ) -> Ex {
        let (tmp, ty, set) = self.scrutinee(scrutinee);
        let patterns: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
        let Some((shape, plan)) = self.plan_match(&ty, &patterns, span) else {
            return Ex::error();
        };
        let mut want: Option<Ty> = exp.filter(|t| !t.is_error()).cloned();
        let mut bodies: Vec<Ex> = Vec::new();
        for (arm, index) in arms.iter().zip(&plan) {
            self.push_sink();
            self.bind_pattern(&shape, tmp, &ty, *index, &arm.pattern);
            let v = self.expr(&arm.body, want.as_ref());
            let v = match &want {
                Some(w) => self.coerce(v, w, arm.body.span),
                None => {
                    if !v.ty.is_error() {
                        want = Some(v.ty.clone());
                    }
                    v
                }
            };
            let stmts = self.pop_sink();
            let vty = v.ty.clone();
            bodies.push(Ex::new(Ek::Seq(stmts, Box::new(v)), vty));
        }
        let want = want.unwrap_or(Ty::Error);
        let variants = shape.variants();
        let mut acc: Option<Ex> = None;
        for (body, index) in bodies.into_iter().zip(plan).rev() {
            acc = Some(match acc {
                None => body,
                Some(rest) => {
                    let test = Self::variant_test(&shape, tmp, &ty, index, &variants[index]);
                    Ex::new(
                        Ek::If(Box::new(test), Box::new(body), Box::new(rest)),
                        want.clone(),
                    )
                }
            });
        }
        let value = acc.unwrap_or_else(Ex::error);
        Ex::new(Ek::Seq(vec![set], Box::new(value)), want)
    }

    /// `match` as a statement: arms are run for their effects.
    pub(super) fn match_stmt(&mut self, scrutinee: &ast::Expr, arms: &[ast::MatchArm], span: Span) {
        let (tmp, ty, set) = self.scrutinee(scrutinee);
        self.emit(set);
        let patterns: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
        let Some((shape, plan)) = self.plan_match(&ty, &patterns, span) else {
            return;
        };
        let mut bodies = Vec::new();
        for (arm, index) in arms.iter().zip(&plan) {
            self.push_sink();
            self.bind_pattern(&shape, tmp, &ty, *index, &arm.pattern);
            self.expr_stmt(&arm.body);
            bodies.push(self.pop_sink());
        }
        let chain = Self::chain(&shape, tmp, &ty, bodies, plan);
        self.st_extend(chain);
    }

    /// `match` in a view body: the arm taken places its UI under `parent`.
    pub(super) fn match_ui(
        &mut self,
        scrutinee: &ast::Expr,
        arms: &[ast::UiArm],
        parent: LocalId,
        span: Span,
    ) {
        let (tmp, ty, set) = self.scrutinee(scrutinee);
        self.emit(set);
        let patterns: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
        let Some((shape, plan)) = self.plan_match(&ty, &patterns, span) else {
            return;
        };
        let mut bodies = Vec::new();
        for (arm, index) in arms.iter().zip(&plan) {
            self.push_sink();
            self.bind_pattern(&shape, tmp, &ty, *index, &arm.pattern);
            self.ui_stmts(&arm.body, parent);
            bodies.push(self.pop_sink());
        }
        let chain = Self::chain(&shape, tmp, &ty, bodies, plan);
        self.st_extend(chain);
    }

    /// `if test(a) { a } else if test(b) { b } else { last }`: the last arm needs no test, as
    /// the match is exhaustive.
    fn chain(
        shape: &Shape,
        tmp: LocalId,
        ty: &Ty,
        bodies: Vec<Vec<Stmt>>,
        plan: Vec<usize>,
    ) -> Vec<Stmt> {
        let variants = shape.variants();
        let mut acc: Option<Vec<Stmt>> = None;
        for (body, index) in bodies.into_iter().zip(plan).rev() {
            acc = Some(match acc {
                None => body,
                Some(rest) => {
                    let test = Self::variant_test(shape, tmp, ty, index, &variants[index]);
                    vec![Stmt::If(test, body, rest)]
                }
            });
        }
        acc.unwrap_or_default()
    }

    fn st_extend(&mut self, stmts: Vec<Stmt>) {
        for s in stmts {
            self.emit(s);
        }
    }

    /// The name of variant `index` of a platform enum, as a UI property takes it: a constant
    /// when the value is a literal, otherwise chosen at run time.
    pub(super) fn choice_text(&mut self, kind: &'static str, v: Ex) -> Ex {
        let variants = prims::choice(kind).map_or(&[][..], |c| c.variants);
        if let Ek::Int(i) = v.kind {
            let name = usize::try_from(i)
                .ok()
                .and_then(|i| variants.get(i))
                .copied()
                .unwrap_or("");
            return Ex::str(name);
        }
        let ty = Ty::Choice(kind);
        let tmp = self.fresh(ty.clone());
        let mut acc = Ex::str(variants.first().copied().unwrap_or(""));
        for (i, name) in variants.iter().enumerate().skip(1).rev() {
            let test = Ex::new(
                Ek::Bin(
                    ir::BinOp::Eq,
                    Box::new(Ex::local(tmp, ty.clone())),
                    Box::new(Ex::new(Ek::Int(i as i64), ty.clone())),
                ),
                Ty::Bool,
            );
            acc = Ex::new(
                Ek::If(Box::new(test), Box::new(Ex::str(name)), Box::new(acc)),
                Ty::Text,
            );
        }
        Ex::new(Ek::Seq(vec![Stmt::Set(tmp, v)], Box::new(acc)), Ty::Text)
    }
}
