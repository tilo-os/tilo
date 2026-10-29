//! Unions of resource types (`type Channel = TextChannel | Thread`): a value is a reference to a
//! resource of one of the types, and `match c { t: TextChannel => ..., x: Thread => ... }` takes
//! it apart by its type, naming every case once, as any `match` does.

use super::{Checker, Ex};
use crate::abi::Imp;
use crate::ast::Pattern;
use crate::diag::Span;
use crate::ir::{Ek, LocalId, Stmt};
use crate::types::{Ty, TypeKey};

/// One arm of a match over a union: the case's type, its id, and the name it binds.
pub(super) struct UnionArm {
    pub key: TypeKey,
    pub id: String,
    pub bind: String,
}

impl Checker<'_> {
    /// Whether a match is over a union: its scrutinee is one, or its patterns name types.
    pub(super) fn is_union_match(ty: &Ty, patterns: &[&Pattern]) -> bool {
        matches!(ty, Ty::Union(_)) || patterns.iter().any(|p| p.ty.is_some())
    }

    /// Checks the type patterns of a match over a value of the union `ty`: each names one of its
    /// types once, and every type has an arm.
    pub(super) fn plan_union(
        &mut self,
        ty: &Ty,
        patterns: &[&Pattern],
        span: Span,
    ) -> Option<Vec<UnionArm>> {
        if ty.is_error() {
            return None;
        }
        let cases = ty.records();
        if cases.is_empty() {
            let t = self.show(ty);
            self.err("E036", span, format!("a pattern such as `t: TextChannel` takes a value of a union apart by its type; this is {t}, so match its cases (`.some(x)`) or compare it"));
            return None;
        }
        let names = || {
            cases
                .iter()
                .map(|k| Ty::Record(k.clone()).display(&self.env.app))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut arms = Vec::new();
        let mut ok = true;
        for p in patterns {
            let Some(te) = &p.ty else {
                self.err(
                    "E036",
                    p.span,
                    format!(
                        "`.{}` is a case of an enum; a value of {} is matched by its type: `x: {}`",
                        p.variant.name,
                        self.show(ty),
                        names().split(", ").next().unwrap_or("")
                    ),
                );
                ok = false;
                continue;
            };
            let pty = crate::resolve::resolve_type_expr(self.env, te, self.diags);
            let Ty::Record(key) = pty else {
                ok = false;
                continue;
            };
            if !cases.contains(&key) {
                self.err(
                    "E036",
                    te.span,
                    format!(
                        "{} is not one of the types of {}; its types: {}",
                        key.name,
                        self.show(ty),
                        names()
                    ),
                );
                ok = false;
                continue;
            }
            if arms.iter().any(|a: &UnionArm| a.key == key) {
                self.err("E036", p.span, format!("`{}` already has an arm", key.name));
                ok = false;
                continue;
            }
            let id = self
                .env
                .type_info(&key)
                .map(|t| t.id.clone())
                .unwrap_or_default();
            arms.push(UnionArm {
                key,
                id,
                bind: p.variant.name.clone(),
            });
        }
        let missing: Vec<String> = cases
            .iter()
            .filter(|k| !arms.iter().any(|a| a.key == **k))
            .map(|k| k.name.clone())
            .collect();
        if !missing.is_empty() && ok {
            self.err(
                "E035",
                span,
                format!(
                    "this match does not cover {}; add an arm for each, such as `x: {} => ...`",
                    missing.join(", "),
                    missing[0]
                ),
            );
            ok = false;
        }
        ok.then_some(arms)
    }

    /// Whether the resource in `tmp` is of the arm's type.
    fn union_test(tmp: LocalId, ty: &Ty, arm: &UnionArm) -> Ex {
        Ex::host(
            Imp::RefIs,
            vec![Ex::local(tmp, ty.clone()), Ex::str(&arm.id)],
            Ty::Bool,
        )
    }

    /// Binds the arm's name to the value in `tmp`, as its type.
    fn union_bind(&mut self, tmp: LocalId, ty: &Ty, arm: &UnionArm) {
        if arm.bind == "_" {
            return;
        }
        let id = self.declare(&arm.bind, Ty::Record(arm.key.clone()));
        self.emit(Stmt::Set(
            id,
            Ex::local(tmp, ty.clone()).typed(Ty::Record(arm.key.clone())),
        ));
    }

    /// A match over a union, its arms lowered by `arm`; the bodies, in arm order.
    pub(super) fn union_arms<T>(
        &mut self,
        tmp: LocalId,
        ty: &Ty,
        arms: &[UnionArm],
        mut arm: impl FnMut(&mut Self, usize) -> T,
    ) -> Vec<(Vec<Stmt>, T)> {
        let mut out = Vec::new();
        for (i, a) in arms.iter().enumerate() {
            self.push_sink();
            self.union_bind(tmp, ty, a);
            let v = arm(self, i);
            out.push((self.pop_sink(), v));
        }
        out
    }

    /// `if is(a) { a } else if is(b) { b } else { last }`: the last arm needs no test.
    pub(super) fn union_chain(
        tmp: LocalId,
        ty: &Ty,
        arms: &[UnionArm],
        bodies: Vec<Vec<Stmt>>,
    ) -> Vec<Stmt> {
        let mut acc: Option<Vec<Stmt>> = None;
        for (body, arm) in bodies.into_iter().zip(arms).rev() {
            acc = Some(match acc {
                None => body,
                Some(rest) => vec![Stmt::If(Self::union_test(tmp, ty, arm), body, rest)],
            });
        }
        acc.unwrap_or_default()
    }

    /// The value of a match over a union.
    pub(super) fn union_value(
        tmp: LocalId,
        ty: &Ty,
        arms: &[UnionArm],
        bodies: Vec<Ex>,
        want: &Ty,
    ) -> Ex {
        let mut acc: Option<Ex> = None;
        for (body, arm) in bodies.into_iter().zip(arms).rev() {
            acc = Some(match acc {
                None => body,
                Some(rest) => Ex::new(
                    Ek::If(
                        Box::new(Self::union_test(tmp, ty, arm)),
                        Box::new(body),
                        Box::new(rest),
                    ),
                    want.clone(),
                ),
            });
        }
        acc.unwrap_or_else(Ex::error)
    }
}
