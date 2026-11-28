//! The values the language makes from a literal: a `Date` or a `Time` written as its ISO text, and the
//! standard library's text-held type (`std.Identifier`), whose value is the text it is written with.

use std::fmt::Write as _;

use crate::ast::{self, Expr, ExprKind};
use crate::types::Ty;

/// `Date("2026-10-24")` or `Time("2026-10-21T09:30:00Z")`: the type and the ISO text, checked at compile
/// time. `kind` is `Date` or `Time`.
pub fn date_value(kind: &str, args: &[ast::Arg]) -> Result<(Ty, String), String> {
    let ty = if kind == "Date" { Ty::Date } else { Ty::Time };
    let example = if kind == "Date" {
        "Date(\"2026-10-24\")"
    } else {
        "Time(\"2026-10-21T09:30:00Z\")"
    };
    let text = match args {
        [ast::Arg { name: None, value }] => plain_text(value),
        _ => None,
    };
    let Some(text) = text else {
        return Err(format!(
            "`{kind}(...)` takes one string literal, such as `{example}`; a {kind} cannot be made from computed text"
        ));
    };
    if kind == "Date" {
        crate::types::check_date(&text)?;
    } else {
        crate::types::check_time(&text)?;
    }
    Ok((ty, text))
}

pub fn std_identifier_call(e: &Expr) -> Option<&[ast::Arg]> {
    let ExprKind::Call { callee, args, .. } = &e.kind else {
        return None;
    };
    let ExprKind::Field { base, name } = &callee.kind else {
        return None;
    };
    let ExprKind::Ident(ns) = &base.kind else {
        return None;
    };
    (ns == crate::stdlib::NS && name.name == "Identifier").then_some(args.as_slice())
}

/// The words `e` is written as, with its interpolation kept as `{count}`: `"wrote {count}
/// messages"` keeps the number a history entry fills in (`did_many:`). `None` for text that is
/// not a literal, or whose interpolation is not the count.
#[must_use]
pub fn counted_text(e: &Expr) -> Option<String> {
    let ExprKind::Str(parts) = &e.kind else {
        return None;
    };
    let mut out = String::new();
    for part in parts {
        match part {
            ast::StrPart::Lit(text) => out.push_str(text),
            ast::StrPart::Interp(inner) => match &inner.kind {
                ExprKind::Ident(name) if name == "count" => out.push_str("{count}"),
                _ => return None,
            },
        }
    }
    Some(out)
}

/// How `did:` is written: `"turned it on"`, words naming a parameter (`"scanned at {dpi} dpi"`),
/// or words a parameter's value picks.
const DID_FORMS: &str = "`did:` takes words in quotes, which may name a parameter (`did: \"scanned at {dpi} dpi\"`) or a field of the changed record as it was and became (`did: \"set the limit from {limit.before} to {limit.after}\"`), or words a parameter's value picks: `did: if on { \"turned it on\" } else { \"turned it off\" }`, `did: if mode == \"gray\" { … } else { … }`, `did: match route { .wine => \"…\", .vm => \"…\" }`";

/// An action's `did:` words, with `{name}` where they read a parameter of `params`, and the
/// choice a parameter's value makes among them when they are an `if` over it or a `match` on it.
/// The words returned are the last branch's: what a value no branch names reads.
///
/// # Errors
/// What the option takes, when `e` is none of its forms.
pub(crate) fn did_words(
    e: &Expr,
    params: &[super::ParamInfo],
) -> Result<(String, Option<crate::meta::DidChoice>), String> {
    let words = |e: &Expr| -> Result<String, String> {
        let e = match &e.kind {
            ExprKind::Block(block) => match block.stmts.as_slice() {
                [
                    ast::Stmt {
                        kind: ast::StmtKind::Expr(e),
                        ..
                    },
                ] => e,
                _ => return Err(DID_FORMS.to_owned()),
            },
            _ => e,
        };
        said_text(e, params).ok_or_else(|| DID_FORMS.to_owned())
    };
    let mut choice = crate::meta::DidChoice::default();
    let pick = |choice: &mut crate::meta::DidChoice,
                param: &str,
                case: String,
                said: String|
     -> Result<(), String> {
        if !params.iter().any(|p| p.name == param) {
            return Err(format!(
                "`did:` picks its words by a parameter of the action, and `{param}` is none"
            ));
        }
        if !choice.param.is_empty() && choice.param != param {
            return Err(format!(
                "`did:` picks its words by one parameter: `{}` and `{param}` are two",
                choice.param
            ));
        }
        let key = case.as_str();
        let fits = key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            && key != "other";
        if !fits {
            return Err(format!(
                "`did:` picks by a value written in letters, digits and `-`, and `{case}` is not one"
            ));
        }
        param.clone_into(&mut choice.param);
        choice.cases.push((case, said));
        Ok(())
    };
    match &e.kind {
        ExprKind::Str(_) => Ok((words(e)?, None)),
        ExprKind::If { .. } => {
            let mut at = e;
            loop {
                match &at.kind {
                    ExprKind::If { cond, then, els } => {
                        let (param, case) =
                            did_case(cond, params).ok_or_else(|| DID_FORMS.to_owned())?;
                        let said = words(&Expr {
                            kind: ExprKind::Block(then.clone()),
                            span: then.span,
                        })?;
                        pick(&mut choice, &param, case, said)?;
                        at = els.as_deref().ok_or_else(|| {
                            "`did:` over a parameter ends with `else`: the words any other value reads"
                                .to_owned()
                        })?;
                    }
                    _ => {
                        let other = words(at)?;
                        return Ok((other, Some(choice)));
                    }
                }
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            let ExprKind::Ident(param) = &scrutinee.kind else {
                return Err(DID_FORMS.to_owned());
            };
            let Some((last, named)) = arms.split_last() else {
                return Err(DID_FORMS.to_owned());
            };
            for arm in named {
                pick(
                    &mut choice,
                    param,
                    arm.pattern.variant.name.clone(),
                    words(&arm.body)?,
                )?;
            }
            let other = words(&last.body)?;
            if choice.param.is_empty() {
                // One arm alone picks nothing: its words are the action's.
                return Ok((other, None));
            }
            Ok((other, Some(choice)))
        }
        _ => Err(DID_FORMS.to_owned()),
    }
}

/// The parameter an `if` of `did:` tests and the value it names: `on` is `true`, `not on` is
/// `false`, `mode == "gray"` is `gray`, `route == .wine` is `wine`.
fn did_case(cond: &Expr, params: &[super::ParamInfo]) -> Option<(String, String)> {
    let is_flag = |name: &str| {
        params
            .iter()
            .any(|p| p.name == name && matches!(p.ty, Ty::Bool))
    };
    match &cond.kind {
        ExprKind::Ident(name) if is_flag(name) => Some((name.clone(), "true".to_owned())),
        ExprKind::Unary {
            op: ast::UnOp::Not,
            operand,
        } => match &operand.kind {
            ExprKind::Ident(name) if is_flag(name) => Some((name.clone(), "false".to_owned())),
            _ => None,
        },
        ExprKind::Binary {
            op: ast::BinOp::Eq,
            lhs,
            rhs,
        } => {
            let ExprKind::Ident(name) = &lhs.kind else {
                return None;
            };
            let value = match &rhs.kind {
                ExprKind::Str(_) => plain_text(rhs)?,
                ExprKind::Variant { name, args: None } => name.name.clone(),
                ExprKind::Int(n) => n.to_string(),
                _ => return None,
            };
            Some((name.clone(), value))
        }
        _ => None,
    }
}

/// The fields `did:` words read as the change found or left them: `limit` for `{limit_before}`
/// or `{limit_after}`, as [`said_text`] writes `{limit.before}`.
pub(crate) fn field_reads(words: &str) -> Vec<String> {
    let mut reads = Vec::new();
    let mut rest = words;
    while let Some((_, after)) = rest.split_once('{') {
        let Some((read, next)) = after.split_once('}') else {
            break;
        };
        if let Some(field) = read
            .strip_suffix("_before")
            .or_else(|| read.strip_suffix("_after"))
            && !reads.iter().any(|held| held == field)
        {
            reads.push(field.to_owned());
        }
        rest = next;
    }
    reads
}

/// The words `e` is written as, with `{name}` where it interpolates a parameter of `params`, and
/// `{field_before}` or `{field_after}` where it reads a field of the changed record
/// (`{field.before}`). `None` for text that is not a literal, or that interpolates anything else.
fn said_text(e: &Expr, params: &[super::ParamInfo]) -> Option<String> {
    let ExprKind::Str(parts) = &e.kind else {
        return None;
    };
    let mut out = String::new();
    for part in parts {
        match part {
            ast::StrPart::Lit(text) => out.push_str(text),
            ast::StrPart::Interp(inner) => match &inner.kind {
                ExprKind::Ident(name) if params.iter().any(|p| p.name == *name) => {
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                }
                // `{enabled.after}`: a field of the changed record as the change left it, and
                // `{enabled.before}` as it found it, read by history from the record itself.
                ExprKind::Field { base, name: when }
                    if matches!(when.name.as_str(), "before" | "after") =>
                {
                    let ExprKind::Ident(field) = &base.kind else {
                        return None;
                    };
                    if params.iter().any(|p| p.name == *field) {
                        return None;
                    }
                    let _ = write!(out, "{{{field}_{}}}", when.name);
                }
                _ => return None,
            },
        }
    }
    Some(out)
}

/// The text a `std.Identifier(...)` is made from, checked at compile time: the value is its text, so a value
/// computed at run time would be one no reader knows.
pub fn std_identifier_value(args: &[ast::Arg]) -> Result<String, String> {
    const WHAT: &str = "std.Identifier";
    const EXAMPLE: &str = "std.Identifier(\"mailto:jane@example.com\")";
    let text = match args {
        [ast::Arg { name: None, value }] => plain_text(value),
        _ => None,
    };
    let Some(text) = text else {
        return Err(format!(
            "`{WHAT}(...)` takes one string literal, such as `{EXAMPLE}`; a {WHAT} cannot be made from computed text"
        ));
    };
    crate::identifier::normalize(&text)
}

pub fn plain_text(e: &Expr) -> Option<String> {
    let ExprKind::Str(parts) = &e.kind else {
        return None;
    };
    parts
        .iter()
        .map(|p| match p {
            ast::StrPart::Lit(t) => Some(t.as_str()),
            ast::StrPart::Interp(_) => None,
        })
        .collect()
}
