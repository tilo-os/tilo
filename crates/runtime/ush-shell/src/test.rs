//! `test`, `[` and `[[ ]]`. Files are asked about through the host.

use brush_parser::ast::{BinaryPredicate, ExtendedTestExpr, TestExpr, UnaryPredicate};

use crate::host::Kind;
use crate::shell::{Exec, Flow, Shell};
use crate::vars::Value;

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
pub(crate) fn test(sh: &mut Shell, args: &[String]) -> Exec {
    Ok(run_test(sh, args))
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
pub(crate) fn bracket(sh: &mut Shell, args: &[String]) -> Exec {
    match args.split_last() {
        Some((last, rest)) if last == "]" => Ok(run_test(sh, rest)),
        _ => {
            sh.error("[: missing `]'");
            Ok(2)
        }
    }
}

fn run_test(sh: &mut Shell, args: &[String]) -> i32 {
    if args.is_empty() {
        return 1;
    }
    let expr = match brush_parser::test_command::parse(args) {
        Ok(expr) => expr,
        Err(e) => {
            sh.error(format!("test: {e}"));
            return 2;
        }
    };
    match eval(sh, &expr) {
        Ok(ok) => i32::from(!ok),
        Err(message) => {
            sh.error(format!("test: {message}"));
            2
        }
    }
}

fn eval(sh: &mut Shell, expr: &TestExpr) -> Result<bool, String> {
    Ok(match expr {
        TestExpr::False => false,
        TestExpr::Literal(s) => !s.is_empty(),
        TestExpr::And(a, b) => eval(sh, a)? && eval(sh, b)?,
        TestExpr::Or(a, b) => eval(sh, a)? || eval(sh, b)?,
        TestExpr::Not(a) => !eval(sh, a)?,
        TestExpr::Parenthesized(a) => eval(sh, a)?,
        TestExpr::UnaryTest(pred, operand) => unary(sh, pred, operand),
        TestExpr::BinaryTest(pred, left, right) => match pred {
            BinaryPredicate::StringExactlyMatchesPattern
            | BinaryPredicate::StringExactlyMatchesString => left == right,
            BinaryPredicate::StringDoesNotExactlyMatchPattern
            | BinaryPredicate::StringDoesNotExactlyMatchString => left != right,
            BinaryPredicate::ArithmeticEqualTo
            | BinaryPredicate::ArithmeticNotEqualTo
            | BinaryPredicate::ArithmeticLessThan
            | BinaryPredicate::ArithmeticLessThanOrEqualTo
            | BinaryPredicate::ArithmeticGreaterThan
            | BinaryPredicate::ArithmeticGreaterThanOrEqualTo => {
                let l = integer(left)?;
                let r = integer(right)?;
                compare(pred, l, r)
            }
            other => binary_other(sh, other, left, right)?,
        },
    })
}

fn integer(text: &str) -> Result<i64, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("{text}: integer expression expected"))
}

fn compare(pred: &BinaryPredicate, l: i64, r: i64) -> bool {
    match pred {
        BinaryPredicate::ArithmeticEqualTo => l == r,
        BinaryPredicate::ArithmeticNotEqualTo => l != r,
        BinaryPredicate::ArithmeticLessThan => l < r,
        BinaryPredicate::ArithmeticLessThanOrEqualTo => l <= r,
        BinaryPredicate::ArithmeticGreaterThan => l > r,
        _ => l >= r,
    }
}

fn binary_other(
    sh: &mut Shell,
    pred: &BinaryPredicate,
    left: &str,
    right: &str,
) -> Result<bool, String> {
    let meta = |p: &str| sh.host.metadata(&crate::paths::join(&sh.cwd, p)).ok();
    Ok(match pred {
        BinaryPredicate::FilesReferToSameDeviceAndInodeNumbers => match (meta(left), meta(right)) {
            (Some(a), Some(b)) => a.dev == b.dev && a.ino == b.ino,
            _ => false,
        },
        BinaryPredicate::LeftFileIsNewerOrExistsWhenRightDoesNot => match (meta(left), meta(right))
        {
            (Some(a), Some(b)) => a.modified > b.modified,
            (Some(_), None) => true,
            _ => false,
        },
        BinaryPredicate::LeftFileIsOlderOrDoesNotExistWhenRightDoes => {
            match (meta(left), meta(right)) {
                (Some(a), Some(b)) => a.modified < b.modified,
                (None, Some(_)) => true,
                _ => false,
            }
        }
        BinaryPredicate::LeftSortsBeforeRight => left < right,
        BinaryPredicate::LeftSortsAfterRight => left > right,
        BinaryPredicate::StringContainsSubstring => left.contains(right),
        BinaryPredicate::StringMatchesRegex => regex_match(sh, left, right)?,
        _ => left == right,
    })
}

fn regex_match(sh: &mut Shell, text: &str, pattern: &str) -> Result<bool, String> {
    let re =
        regex::Regex::new(pattern).map_err(|_| format!("{pattern}: invalid regular expression"))?;
    let Some(caps) = re.captures(text) else {
        let _ = sh.vars.unset("BASH_REMATCH");
        return Ok(false);
    };
    let map = caps
        .iter()
        .enumerate()
        .map(|(i, m)| {
            (
                i64::try_from(i).unwrap_or(0),
                m.map(|m| m.as_str().to_string()).unwrap_or_default(),
            )
        })
        .collect();
    let _ = sh.vars.set_value("BASH_REMATCH", Value::Indexed(map));
    Ok(true)
}

fn unary(sh: &mut Shell, pred: &UnaryPredicate, operand: &str) -> bool {
    if matches!(pred, UnaryPredicate::ShellVariableIsSetAndAssigned) {
        return variable_is_set(sh, operand);
    }
    let path = crate::paths::join(&sh.cwd, operand);
    let meta = || sh.host.metadata(&path).ok();
    match pred {
        UnaryPredicate::StringHasZeroLength => operand.is_empty(),
        UnaryPredicate::StringHasNonZeroLength => !operand.is_empty(),
        UnaryPredicate::FileExists
        | UnaryPredicate::FileExistsAndOwnedByEffectiveUserId
        | UnaryPredicate::FileExistsAndOwnedByEffectiveGroupId
        | UnaryPredicate::FileExistsAndModifiedSinceLastRead => meta().is_some(),
        UnaryPredicate::FileExistsAndIsDir => meta().is_some_and(|m| m.kind == Kind::Dir),
        UnaryPredicate::FileExistsAndIsRegularFile => meta().is_some_and(|m| m.kind == Kind::File),
        UnaryPredicate::FileExistsAndIsSymlink => sh
            .host
            .symlink_metadata(&path)
            .is_ok_and(|m| m.kind == Kind::Symlink),
        UnaryPredicate::FileExistsAndIsNotZeroLength => meta().is_some_and(|m| m.len > 0),
        UnaryPredicate::FileExistsAndIsReadable => meta().is_some_and(|m| m.mode & 0o444 != 0),
        UnaryPredicate::FileExistsAndIsWritable => meta().is_some_and(|m| m.mode & 0o222 != 0),
        UnaryPredicate::FileExistsAndIsExecutable => meta().is_some_and(|m| m.mode & 0o111 != 0),
        UnaryPredicate::FileExistsAndIsSetuid => meta().is_some_and(|m| m.mode & 0o4000 != 0),
        UnaryPredicate::FileExistsAndIsSetgid => meta().is_some_and(|m| m.mode & 0o2000 != 0),
        UnaryPredicate::FileExistsAndHasStickyBit => meta().is_some_and(|m| m.mode & 0o1000 != 0),
        UnaryPredicate::FileExistsAndIsBlockSpecialFile
        | UnaryPredicate::FileExistsAndIsCharSpecialFile
        | UnaryPredicate::FileExistsAndIsFifo
        | UnaryPredicate::FileExistsAndIsSocket => meta().is_some_and(|m| m.kind == Kind::Other),
        UnaryPredicate::FdIsOpenTerminal => operand.parse::<u32>().ok().is_some_and(|fd| match sh
            .fds
            .get(&fd)
        {
            Some(crate::shell::Fd::In(i)) => i.host_stream().is_some(),
            Some(crate::shell::Fd::Out(o)) => o.host_stream().is_some(),
            None => false,
        }),
        UnaryPredicate::ShellOptionEnabled => sh.option_flags().contains(match operand {
            "errexit" => 'e',
            "nounset" => 'u',
            "xtrace" => 'x',
            "noglob" => 'f',
            "noclobber" => 'C',
            _ => '\0',
        }),
        UnaryPredicate::ShellVariableIsSetAndAssigned
        | UnaryPredicate::ShellVariableIsSetAndNameRef => false,
    }
}

/// `-v name` or `-v name[index]`: an indexed array's index is arithmetic,
/// and counts from the end when negative.
fn variable_is_set(sh: &mut Shell, operand: &str) -> bool {
    let Some((name, rest)) = operand.split_once('[') else {
        return sh.vars.is_set(operand);
    };
    let key = rest.strip_suffix(']').unwrap_or(rest);
    match sh.vars.get(name).and_then(|v| v.value.clone()) {
        Some(Value::Indexed(m)) => match sh.arith_text(key) {
            Ok(i) => {
                let i = sh.index_from_end(name, i);
                m.contains_key(&i)
            }
            Err(_) => false,
        },
        Some(Value::Assoc(m)) => m.contains_key(key),
        Some(Value::Scalar(_)) => matches!(sh.arith_text(key), Ok(0 | -1)),
        None => false,
    }
}

impl Shell {
    /// `[[ … ]]`: its words are expanded without splitting or globbing, the
    /// right of `==` is a pattern, and `=~` a regular expression.
    pub(crate) fn extended_test(&mut self, expr: &ExtendedTestExpr) -> Result<bool, Flow> {
        Ok(match expr {
            ExtendedTestExpr::And(a, b) => self.extended_test(a)? && self.extended_test(b)?,
            ExtendedTestExpr::Or(a, b) => self.extended_test(a)? || self.extended_test(b)?,
            ExtendedTestExpr::Not(a) => !self.extended_test(a)?,
            ExtendedTestExpr::Parenthesized(a) => self.extended_test(a)?,
            ExtendedTestExpr::UnaryTest(pred, word) => {
                let operand = self.expand_word(&word.value)?;
                unary(self, pred, &operand)
            }
            ExtendedTestExpr::BinaryTest(pred, left, right) => {
                let l = self.expand_word(&left.value)?;
                match pred {
                    BinaryPredicate::StringExactlyMatchesPattern
                    | BinaryPredicate::StringExactlyMatchesString => {
                        self.expand_pattern(&right.value)?.matches(&l)
                    }
                    BinaryPredicate::StringDoesNotExactlyMatchPattern
                    | BinaryPredicate::StringDoesNotExactlyMatchString => {
                        !self.expand_pattern(&right.value)?.matches(&l)
                    }
                    BinaryPredicate::StringMatchesRegex => {
                        let r = self.regex_word(&right.value)?;
                        match regex_match(self, &l, &r) {
                            Ok(ok) => ok,
                            Err(message) => {
                                self.error(message);
                                return Err(Flow::Fatal(2));
                            }
                        }
                    }
                    BinaryPredicate::ArithmeticEqualTo
                    | BinaryPredicate::ArithmeticNotEqualTo
                    | BinaryPredicate::ArithmeticLessThan
                    | BinaryPredicate::ArithmeticLessThanOrEqualTo
                    | BinaryPredicate::ArithmeticGreaterThan
                    | BinaryPredicate::ArithmeticGreaterThanOrEqualTo => {
                        let a = self.arith_text(&l)?;
                        let r = self.expand_word(&right.value)?;
                        let b = self.arith_text(&r)?;
                        compare(pred, a, b)
                    }
                    other => {
                        let r = self.expand_word(&right.value)?;
                        match binary_other(self, other, &l, &r) {
                            Ok(ok) => ok,
                            Err(message) => {
                                self.error(message);
                                return Err(Flow::Fatal(2));
                            }
                        }
                    }
                }
            }
        })
    }

    /// The right of `=~`: quoted parts match literally.
    fn regex_word(&mut self, text: &str) -> Result<String, Flow> {
        let pieces =
            brush_parser::word::parse(text, &Self::parser_options()).map_err(|_| Flow::Fatal(2))?;
        let mut out = String::new();
        for p in pieces {
            match &p.piece {
                brush_parser::word::WordPiece::Text(t) => out.push_str(t),
                brush_parser::word::WordPiece::SingleQuotedText(t) => {
                    out.push_str(&regex::escape(t));
                }
                other => {
                    let raw = text.get(p.start_index..p.end_index).unwrap_or("");
                    let value = self.expand_word(raw)?;
                    let quoted = matches!(
                        other,
                        brush_parser::word::WordPiece::DoubleQuotedSequence(_)
                    );
                    out.push_str(&if quoted { regex::escape(&value) } else { value });
                }
            }
        }
        Ok(out)
    }
}
