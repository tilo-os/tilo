//! The values the language makes from a literal: a `Date` or a `Time` written as its ISO text,
//! and the standard library's text-held types (`std.Identifier`, `std.Repeat`, `std.Reminder`),
//! whose value is the text it is written with.

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

/// The standard library's types whose value is the text it is made from, by the path source
/// writes: `std.Identifier`, `std.Repeat` and `std.Reminder`.
pub fn std_text_ty(dotted: &[&str]) -> Option<Ty> {
    match dotted {
        ["std", "Identifier"] => Some(Ty::Identifier),
        ["std", "Repeat"] => Some(Ty::Repeat),
        ["std", "Reminder"] => Some(Ty::Reminder),
        _ => None,
    }
}

/// The std type and the arguments of a call that makes one of those values, when `e` is one:
/// `std.Repeat("FREQ=DAILY")`, `std.Identifier("mailto:jane@x.com")`.
pub fn std_text_call(e: &Expr) -> Option<(Ty, &[ast::Arg])> {
    let ExprKind::Call { callee, args } = &e.kind else {
        return None;
    };
    let ExprKind::Field { base, name } = &callee.kind else {
        return None;
    };
    let ExprKind::Ident(ns) = &base.kind else {
        return None;
    };
    if ns != crate::stdlib::NS {
        return None;
    }
    std_text_ty(&[ns, name.name.as_str()]).map(|ty| (ty, args.as_slice()))
}

/// The text a value of a text-held std type is made from, checked at compile time: the value
/// is its text, so a value computed at run time would be one no reader knows.
pub fn std_text_value(ty: &Ty, args: &[ast::Arg]) -> Result<String, String> {
    let (what, example) = match ty {
        Ty::Repeat => ("std.Repeat", "std.Repeat(\"FREQ=WEEKLY\")"),
        Ty::Reminder => ("std.Reminder", "std.Reminder(\"-PT15M\")"),
        _ => (
            "std.Identifier",
            "std.Identifier(\"mailto:jane@example.com\")",
        ),
    };
    let text = match args {
        [ast::Arg { name: None, value }] => plain_text(value),
        _ => None,
    };
    let Some(text) = text else {
        return Err(format!(
            "`{what}(...)` takes one string literal, such as `{example}`; a {what} cannot be made from computed text"
        ));
    };
    match ty {
        Ty::Repeat => crate::repeat::Repeat::parse(&text).map(|repeat| repeat.text()),
        Ty::Reminder => crate::repeat::Reminder::parse(&text).map(|reminder| reminder.text()),
        _ => crate::identifier::normalize(&text),
    }
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
