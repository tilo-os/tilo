//! The pure surface of `Text` and of numbers: the methods every value of those types has and
//! what each lowers to, and the text and number methods a list has (`join`, `sum`, `avg`,
//! `contains`). Every one is a host call over a value: none of them changes anything, reads a
//! clock or leaves the workspace, so a view and an action may both write them.
//!
//! What a text counts, finds and cuts in — graphemes — is [`crate::text`]'s, the same reading the
//! index and search's identifier patterns use; the names a message lists are
//! [`super::methods`]'.

use super::{Checker, Ex, methods};
use crate::abi::Imp;
use crate::ast;
use crate::diag::Span;
use crate::ir::{BinOp, Conv, Ek};
use crate::types::Ty;

impl Checker<'_> {
    /// Lowers a method of the text and number surface, `None` when `name` is not one of them.
    pub(super) fn text_math(
        &mut self,
        r: Ex,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<Ex> {
        let m = name.name.as_str();
        let ex = match (&r.ty.clone(), m) {
            (Ty::Text, _) if methods::TEXT.contains(&m) => self.text_method(r, m, name, args, span),
            (Ty::Int, _) if methods::INT.contains(&m) => self.int_method(r, m, name, args, span),
            (Ty::Float, _) if methods::FLOAT.contains(&m) => {
                self.float_method(r, m, name, args, span)
            }
            (Ty::Bool, "to_text") => self
                .arity(name, args, 0, span)
                .then(|| self.text_of(r, span)),
            (Ty::List(item), "join") if **item == Ty::Text => {
                self.arity(name, args, 1, span).then(|| {
                    let Some(a) = args.first() else {
                        return Ex::error();
                    };
                    let sep = self.expr_as(&a.value, &Ty::Text);
                    Ex::host(Imp::StrJoin, vec![r, sep], Ty::Text)
                })
            }
            // A sum keeps the items' type; an average is fractional however its items are.
            (Ty::List(item), "sum" | "avg") if matches!(**item, Ty::Int | Ty::Float) => {
                self.arity(name, args, 0, span).then(|| {
                    let fractional = m == "avg" || **item == Ty::Float;
                    let imp = match (m, fractional) {
                        ("avg", _) => Imp::ListAvg,
                        (_, true) => Imp::ListSumF,
                        (_, false) => Imp::ListSumI,
                    };
                    let ty = if fractional { Ty::Float } else { Ty::Int };
                    Ex::host(imp, vec![r], ty)
                })
            }
            // `xs.contains(x)`: whether the item is one of them, read as `index_of` finds it.
            (Ty::List(item) | Ty::Set(item) | Ty::Collection(item), "contains") => {
                self.arity(name, args, 1, span).then(|| {
                    let Some(a) = args.first() else {
                        return Ex::error();
                    };
                    let what = Self::handle_of(self.expr_as(&a.value, item));
                    let at = Ex::host(
                        Imp::ListIndexOf,
                        vec![r, what],
                        Ty::Option(Box::new(Ty::Int)),
                    );
                    Ex::host(Imp::IsSome, vec![at], Ty::Bool)
                })
            }
            _ => return None,
        };
        Some(ex.unwrap_or_else(Ex::error))
    }

    /// The one `Text` argument of a method that takes just it; `None` after reporting a call
    /// with another shape.
    fn one_text(&mut self, name: &ast::Ident, args: &[ast::Arg], span: Span) -> Option<Ex> {
        if !self.arity(name, args, 1, span) {
            return None;
        }
        let a = args.first()?;
        Some(self.expr_as(&a.value, &Ty::Text))
    }

    fn text_method(
        &mut self,
        r: Ex,
        m: &str,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<Ex> {
        Some(match m {
            "len" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                Ex::host(Imp::StrLen, vec![r], Ty::Int)
            }
            // How many words it holds: its runs of non-whitespace.
            "words" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                Ex::host(Imp::StrWords, vec![r], Ty::Int)
            }
            "is_empty" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                let len = Ex::host(Imp::StrLen, vec![r], Ty::Int);
                Ex::new(
                    Ek::Bin(BinOp::Eq, Box::new(len), Box::new(Ex::int(0))),
                    Ty::Bool,
                )
            }
            "trim" | "lower" | "upper" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                let imp = match m {
                    "trim" => Imp::StrTrim,
                    "lower" => Imp::StrLower,
                    _ => Imp::StrUpper,
                };
                Ex::host(imp, vec![r], Ty::Text)
            }
            // `t.find(what)`: where it stands, counted in graphemes, or none.
            "find" => {
                let what = self.one_text(name, args, span)?;
                Ex::host(Imp::StrFind, vec![r, what], Ty::Option(Box::new(Ty::Int)))
            }
            "starts_with" | "ends_with" | "contains" | "matches" => {
                let what = self.one_text(name, args, span)?;
                let imp = match m {
                    "starts_with" => Imp::StrStartsWith,
                    "ends_with" => Imp::StrEndsWith,
                    "contains" => Imp::StrContains,
                    _ => Imp::StrMatches,
                };
                Ex::host(imp, vec![r, what], Ty::Bool)
            }
            "split" => {
                let sep = self.one_text(name, args, span)?;
                Ex::host(Imp::StrSplit, vec![r, sep], Ty::List(Box::new(Ty::Text)))
            }
            "slice" => {
                if !self.arity(name, args, 2, span) {
                    return None;
                }
                let (Some(from), Some(to)) = (args.first(), args.get(1)) else {
                    return None;
                };
                let from = self.expr_as(&from.value, &Ty::Int);
                let to = self.expr_as(&to.value, &Ty::Int);
                Ex::host(Imp::StrSlice, vec![r, from, to], Ty::Text)
            }
            "replace" => {
                if !self.arity(name, args, 2, span) {
                    return None;
                }
                let (Some(from), Some(to)) = (args.first(), args.get(1)) else {
                    return None;
                };
                let from = self.expr_as(&from.value, &Ty::Text);
                let to = self.expr_as(&to.value, &Ty::Text);
                Ex::host(Imp::StrReplace, vec![r, from, to], Ty::Text)
            }
            // The number a text writes: an id another app gave as text, passed where its action
            // takes the number.
            "to_int" => Ex::host(Imp::StrToInt, vec![r], Ty::Option(Box::new(Ty::Int))),
            _ => return None,
        })
    }

    fn int_method(
        &mut self,
        r: Ex,
        m: &str,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<Ex> {
        Some(match m {
            "min" | "max" => {
                if !self.arity(name, args, 1, span) {
                    return None;
                }
                let a = args.first()?;
                let other = self.expr_as(&a.value, &Ty::Int);
                let imp = if m == "min" { Imp::IntMin } else { Imp::IntMax };
                Ex::host(imp, vec![r, other], Ty::Int)
            }
            "clamp" => {
                if !self.arity(name, args, 2, span) {
                    return None;
                }
                let (Some(lo), Some(hi)) = (args.first(), args.get(1)) else {
                    return None;
                };
                let lo = self.expr_as(&lo.value, &Ty::Int);
                let hi = self.expr_as(&hi.value, &Ty::Int);
                Ex::host(Imp::IntClamp, vec![r, lo, hi], Ty::Int)
            }
            "abs" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                Ex::host(Imp::IntAbs, vec![r], Ty::Int)
            }
            "to_float" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                Ex::new(Ek::Conv(Conv::IntToFloat, Box::new(r)), Ty::Float)
            }
            "to_text" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                self.text_of(r, span)
            }
            _ => return None,
        })
    }

    fn float_method(
        &mut self,
        r: Ex,
        m: &str,
        name: &ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<Ex> {
        Some(match m {
            "min" | "max" => {
                if !self.arity(name, args, 1, span) {
                    return None;
                }
                let a = args.first()?;
                let other = self.expr_as(&a.value, &Ty::Float);
                let imp = if m == "min" {
                    Imp::FloatMin
                } else {
                    Imp::FloatMax
                };
                Ex::host(imp, vec![r, other], Ty::Float)
            }
            "clamp" => {
                if !self.arity(name, args, 2, span) {
                    return None;
                }
                let (Some(lo), Some(hi)) = (args.first(), args.get(1)) else {
                    return None;
                };
                let lo = self.expr_as(&lo.value, &Ty::Float);
                let hi = self.expr_as(&hi.value, &Ty::Float);
                Ex::host(Imp::FloatClamp, vec![r, lo, hi], Ty::Float)
            }
            "abs" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                Ex::host(Imp::FloatAbs, vec![r], Ty::Float)
            }
            // `f.round()`: the nearest whole number, as an Int; `f.round(2)`: that many places,
            // as a Float. The same for `floor` and `ceil`.
            "round" | "floor" | "ceil" => {
                match args.len() {
                    0 => {}
                    1 => {
                        let places = args.first()?;
                        if places.name.is_some() {
                            self.err("E022", span, format!("`.{m}` takes the places to keep"));
                            return None;
                        }
                        let places = self.expr_as(&places.value, &Ty::Int);
                        let imp = match m {
                            "round" => Imp::FloatRoundTo,
                            "floor" => Imp::FloatFloorTo,
                            _ => Imp::FloatCeilTo,
                        };
                        return Some(Ex::host(imp, vec![r, places], Ty::Float));
                    }
                    _ => {
                        self.err(
                            "E022",
                            span,
                            format!("`.{m}` takes no argument, or the places to keep: `.{m}()` or `.{m}(2)`"),
                        );
                        return None;
                    }
                }
                let imp = match m {
                    "round" => Imp::FloatRound,
                    "floor" => Imp::FloatFloor,
                    _ => Imp::FloatCeil,
                };
                Ex::host(imp, vec![r], Ty::Int)
            }
            "to_int" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                Ex::new(Ek::Conv(Conv::FloatToInt, Box::new(r)), Ty::Int)
            }
            "to_text" => {
                if !self.arity(name, args, 0, span) {
                    return None;
                }
                self.text_of(r, span)
            }
            _ => return None,
        })
    }
}
