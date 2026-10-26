//! Predicates over the scalar fields of a collection's items, which a store answers itself.
//!
//! `items.filter(x -> not x.done)` and `items.count(x -> x.pinned)` over a collection field,
//! with a lambda that reads only registers and counters of the item and literals, compile to a
//! [`Pred`] as well as to their loop. In a view the host hands the predicate to the store, which
//! lists the matching items (and keeps that listing up to date as items change) without the
//! view building a reference per item or running the lambda once per item. Elsewhere, or where
//! the host cannot answer it, the loop runs.
//!
//! The compiler writes field operands as field ids ([`Pred<String>`]); the host resolves them to
//! what the store reads (how the field merges, what an unwritten one holds).

use serde::{Deserialize, Serialize};

/// A predicate over one item; `F` names a field of it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pred<F> {
    Const(bool),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
    Cmp(CmpOp, Operand<F>, Operand<F>),
}

/// How a comparison compares. Text compares by equality alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// One side of a comparison: a field of the item, or a literal.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operand<F> {
    Field(F),
    Lit(Lit),
}

/// A value a predicate compares.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lit {
    Bool(bool),
    Int(i64),
    Text(String),
}

impl<F> Pred<F> {
    /// The same predicate with each field `f` resolved to `map(f)`; `None` when one does not
    /// resolve.
    pub fn resolve<G>(&self, map: &mut impl FnMut(&F) -> Option<G>) -> Option<Pred<G>> {
        Some(match self {
            Self::Const(b) => Pred::Const(*b),
            Self::Not(p) => Pred::Not(Box::new(p.resolve(map)?)),
            Self::And(ps) => Pred::And(ps.iter().map(|p| p.resolve(map)).collect::<Option<_>>()?),
            Self::Or(ps) => Pred::Or(ps.iter().map(|p| p.resolve(map)).collect::<Option<_>>()?),
            Self::Cmp(op, a, b) => Pred::Cmp(*op, a.resolve(map)?, b.resolve(map)?),
        })
    }

    /// Every field it reads.
    pub fn fields(&self) -> Vec<&F> {
        let mut out = Vec::new();
        self.each_field(&mut |f| out.push(f));
        out
    }

    fn each_field<'a>(&'a self, f: &mut impl FnMut(&'a F)) {
        match self {
            Self::Const(_) => {}
            Self::Not(p) => p.each_field(f),
            Self::And(ps) | Self::Or(ps) => ps.iter().for_each(|p| p.each_field(f)),
            Self::Cmp(_, a, b) => {
                for side in [a, b] {
                    if let Operand::Field(field) = side {
                        f(field);
                    }
                }
            }
        }
    }

    /// Whether it holds of an item whose field `f` holds `value(f)`.
    ///
    /// # Errors
    ///
    /// The first error `value` returns; a comparison of values of different kinds, or of text
    /// by order, as a message.
    pub fn eval<E: From<String>>(
        &self,
        value: &mut impl FnMut(&F) -> Result<Lit, E>,
    ) -> Result<bool, E> {
        Ok(match self {
            Self::Const(b) => *b,
            Self::Not(p) => !p.eval(value)?,
            Self::And(ps) => {
                for p in ps {
                    if !p.eval(value)? {
                        return Ok(false);
                    }
                }
                true
            }
            Self::Or(ps) => {
                for p in ps {
                    if p.eval(value)? {
                        return Ok(true);
                    }
                }
                false
            }
            Self::Cmp(op, a, b) => {
                let (a, b) = (a.value(value)?, b.value(value)?);
                let ord = match (&a, &b) {
                    (Lit::Bool(x), Lit::Bool(y)) if matches!(op, CmpOp::Eq | CmpOp::Ne) => x.cmp(y),
                    (Lit::Text(x), Lit::Text(y)) if matches!(op, CmpOp::Eq | CmpOp::Ne) => x.cmp(y),
                    (Lit::Int(x), Lit::Int(y)) => x.cmp(y),
                    _ => return Err(E::from(format!("cannot compare {a:?} with {b:?}"))),
                };
                match op {
                    CmpOp::Eq => ord.is_eq(),
                    CmpOp::Ne => ord.is_ne(),
                    CmpOp::Lt => ord.is_lt(),
                    CmpOp::Le => ord.is_le(),
                    CmpOp::Gt => ord.is_gt(),
                    CmpOp::Ge => ord.is_ge(),
                }
            }
        })
    }
}

impl<F> Operand<F> {
    fn resolve<G>(&self, map: &mut impl FnMut(&F) -> Option<G>) -> Option<Operand<G>> {
        Some(match self {
            Self::Field(f) => Operand::Field(map(f)?),
            Self::Lit(l) => Operand::Lit(l.clone()),
        })
    }

    fn value<E>(&self, value: &mut impl FnMut(&F) -> Result<Lit, E>) -> Result<Lit, E> {
        match self {
            Self::Field(f) => value(f),
            Self::Lit(l) => Ok(l.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CmpOp, Lit, Operand, Pred};

    fn field(name: &str) -> Operand<&str> {
        Operand::Field(name)
    }

    fn eval(pred: &Pred<&str>, item: &[(&str, Lit)]) -> Result<bool, String> {
        pred.eval(&mut |f: &&str| {
            item.iter()
                .find(|(name, _)| name == f)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("no {f}"))
        })
    }

    #[test]
    fn a_predicate_compares_an_items_fields_with_literals() {
        let open_and_voted = Pred::And(vec![
            Pred::Not(Box::new(Pred::Cmp(
                CmpOp::Eq,
                field("done"),
                Operand::Lit(Lit::Bool(true)),
            ))),
            Pred::Cmp(CmpOp::Ge, field("votes"), Operand::Lit(Lit::Int(2))),
        ]);
        let item = |done, votes| [("done", Lit::Bool(done)), ("votes", Lit::Int(votes))];
        assert_eq!(eval(&open_and_voted, &item(false, 2)), Ok(true));
        assert_eq!(eval(&open_and_voted, &item(false, 1)), Ok(false));
        assert_eq!(eval(&open_and_voted, &item(true, 9)), Ok(false));
        assert_eq!(open_and_voted.fields(), [&"done", &"votes"]);

        let titled = Pred::Or(vec![
            Pred::Const(false),
            Pred::Cmp(
                CmpOp::Ne,
                field("title"),
                Operand::Lit(Lit::Text("x".into())),
            ),
        ]);
        assert_eq!(eval(&titled, &[("title", Lit::Text("y".into()))]), Ok(true));
        assert_eq!(
            eval(&titled, &[("title", Lit::Text("x".into()))]),
            Ok(false)
        );
    }

    #[test]
    fn text_by_order_or_values_of_two_kinds_do_not_compare() {
        let ordered = Pred::Cmp(
            CmpOp::Lt,
            field("title"),
            Operand::Lit(Lit::Text("m".into())),
        );
        assert!(eval(&ordered, &[("title", Lit::Text("a".into()))]).is_err());
        let mixed = Pred::Cmp(CmpOp::Eq, field("done"), Operand::Lit(Lit::Int(1)));
        assert!(eval(&mixed, &[("done", Lit::Bool(true))]).is_err());
    }

    #[test]
    fn resolving_a_field_that_does_not_resolve_resolves_nothing() {
        let pred = Pred::Cmp(CmpOp::Eq, field("a"), field("b"));
        assert_eq!(pred.resolve(&mut |f: &&str| (*f == "a").then_some(1)), None);
        assert_eq!(
            pred.resolve(&mut |f: &&str| Some(f.len())),
            Some(Pred::Cmp(CmpOp::Eq, Operand::Field(1), Operand::Field(1)))
        );
    }
}
