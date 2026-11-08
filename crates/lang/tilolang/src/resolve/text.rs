//! The values the language makes from a literal: a `Date` or a `Time` written as its ISO text,
//! and the standard library's text-held type (`std.Identifier`), whose value is the text it is
//! written with.

use crate::ast::{self, Expr, ExprKind};
use crate::types::Ty;

/// `Date("2026-10-24")` or `Time("2026-10-21T09:30:00Z")`: the type and the ISO text, checked
/// at compile time. `kind` is `Date` or `Time`.
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

/// The arguments of a `std.Identifier(...)` call, when `e` is one: an identifier's value is the
/// text it is written with.
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

/// The text a `std.Identifier(...)` is made from, checked at compile time: the value is its
/// text, so a value computed at run time would be one no reader knows.
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

/// The text of a string literal without interpolation.
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
