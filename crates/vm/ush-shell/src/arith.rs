//! Arithmetic: `$(( ))`, `(( ))`, `let`, array indices and `for (( ))`.
//!
//! Integers are 64-bit and wrap as bash's do. A variable's value is itself
//! read as an expression, so `a=b; b=3; echo $((a+1))` is 4.

use brush_parser::ast::{
    ArithmeticExpr, ArithmeticTarget, BinaryOperator, UnaryAssignmentOperator, UnaryOperator,
};

use crate::shell::{Exec, Flow, Shell};
use crate::vars::Value;

/// How deep a variable may name another before the shell gives up.
const MAX_INDIRECTION: u32 = 64;

impl Shell {
    /// Evaluate the text of an arithmetic expression: its `$` expansions
    /// first, then the expression.
    pub(crate) fn arith_text(&mut self, text: &str) -> Result<i64, Flow> {
        let expanded = self.expand_string(text)?;
        self.arith_str(&expanded, 0)
    }

    fn arith_str(&mut self, text: &str, depth: u32) -> Result<i64, Flow> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(0);
        }
        if let Some(n) = parse_number(trimmed) {
            return Ok(n);
        }
        let expr = brush_parser::arithmetic::parse(trimmed).map_err(|_| {
            self.error(format!("{trimmed}: arithmetic syntax error"));
            Flow::Error(1)
        })?;
        self.arith(&expr, depth)
    }

    pub(crate) fn arith(&mut self, expr: &ArithmeticExpr, depth: u32) -> Result<i64, Flow> {
        if depth > MAX_INDIRECTION {
            self.error("arithmetic: expression recursion level exceeded");
            return Err(Flow::Error(1));
        }
        match expr {
            ArithmeticExpr::Literal(n) => Ok(*n),
            ArithmeticExpr::Reference(target) => self.arith_read(target, depth),
            ArithmeticExpr::UnaryOp(op, inner) => {
                let v = self.arith(inner, depth)?;
                Ok(match op {
                    UnaryOperator::UnaryPlus => v,
                    UnaryOperator::UnaryMinus => v.wrapping_neg(),
                    UnaryOperator::BitwiseNot => !v,
                    UnaryOperator::LogicalNot => i64::from(v == 0),
                })
            }
            ArithmeticExpr::BinaryOp(op, left, right) => match op {
                BinaryOperator::LogicalAnd => {
                    if self.arith(left, depth)? == 0 {
                        return Ok(0);
                    }
                    Ok(i64::from(self.arith(right, depth)? != 0))
                }
                BinaryOperator::LogicalOr => {
                    if self.arith(left, depth)? != 0 {
                        return Ok(1);
                    }
                    Ok(i64::from(self.arith(right, depth)? != 0))
                }
                _ => {
                    let l = self.arith(left, depth)?;
                    let r = self.arith(right, depth)?;
                    self.binary(*op, l, r)
                }
            },
            ArithmeticExpr::Conditional(cond, then, otherwise) => {
                if self.arith(cond, depth)? == 0 {
                    self.arith(otherwise, depth)
                } else {
                    self.arith(then, depth)
                }
            }
            ArithmeticExpr::Assignment(target, value) => {
                let v = self.arith(value, depth)?;
                self.arith_write(target, v, depth)?;
                Ok(v)
            }
            ArithmeticExpr::BinaryAssignment(op, target, value) => {
                let l = self.arith_read(target, depth)?;
                let r = self.arith(value, depth)?;
                let v = self.binary(*op, l, r)?;
                self.arith_write(target, v, depth)?;
                Ok(v)
            }
            ArithmeticExpr::UnaryAssignment(op, target) => {
                let old = self.arith_read(target, depth)?;
                let new = match op {
                    UnaryAssignmentOperator::PrefixIncrement
                    | UnaryAssignmentOperator::PostfixIncrement => old.wrapping_add(1),
                    UnaryAssignmentOperator::PrefixDecrement
                    | UnaryAssignmentOperator::PostfixDecrement => old.wrapping_sub(1),
                };
                self.arith_write(target, new, depth)?;
                Ok(match op {
                    UnaryAssignmentOperator::PrefixIncrement
                    | UnaryAssignmentOperator::PrefixDecrement => new,
                    _ => old,
                })
            }
        }
    }

    fn binary(&self, op: BinaryOperator, l: i64, r: i64) -> Result<i64, Flow> {
        let zero = || {
            self.error("division by 0");
            Flow::Error(1)
        };
        Ok(match op {
            BinaryOperator::Power => {
                if r < 0 {
                    self.error("exponent less than 0");
                    return Err(Flow::Error(1));
                }
                l.wrapping_pow(u32::try_from(r).unwrap_or(u32::MAX))
            }
            BinaryOperator::Multiply => l.wrapping_mul(r),
            BinaryOperator::Divide => {
                if r == 0 {
                    return Err(zero());
                }
                // Only `i64::MIN / -1` overflows; it wraps, as bash's does.
                l.checked_div(r).unwrap_or(i64::MIN)
            }
            BinaryOperator::Modulo => {
                if r == 0 {
                    return Err(zero());
                }
                // Only `i64::MIN % -1` overflows; its remainder is 0.
                l.checked_rem(r).unwrap_or(0)
            }
            BinaryOperator::Comma => r,
            BinaryOperator::Add => l.wrapping_add(r),
            BinaryOperator::Subtract => l.wrapping_sub(r),
            BinaryOperator::ShiftLeft => l.wrapping_shl(u32::try_from(r & 63).unwrap_or(0)),
            BinaryOperator::ShiftRight => l.wrapping_shr(u32::try_from(r & 63).unwrap_or(0)),
            BinaryOperator::LessThan => i64::from(l < r),
            BinaryOperator::LessThanOrEqualTo => i64::from(l <= r),
            BinaryOperator::GreaterThan => i64::from(l > r),
            BinaryOperator::GreaterThanOrEqualTo => i64::from(l >= r),
            BinaryOperator::Equals => i64::from(l == r),
            BinaryOperator::NotEquals => i64::from(l != r),
            BinaryOperator::BitwiseAnd => l & r,
            BinaryOperator::BitwiseXor => l ^ r,
            BinaryOperator::BitwiseOr => l | r,
            BinaryOperator::LogicalAnd => i64::from(l != 0 && r != 0),
            BinaryOperator::LogicalOr => i64::from(l != 0 || r != 0),
        })
    }

    fn arith_read(&mut self, target: &ArithmeticTarget, depth: u32) -> Result<i64, Flow> {
        let text = match target {
            ArithmeticTarget::Variable(name) => {
                if name == "RANDOM" {
                    return Ok(i64::from(self.next_random()));
                }
                match self.vars.scalar(name) {
                    Some(value) => value,
                    None => return self.unset_in_arith(name),
                }
            }
            ArithmeticTarget::ArrayElement(name, index) => {
                let key = self.element_key(name, index, depth)?;
                let value = self.vars.get(name).and_then(|v| v.value.clone());
                let element = match (value, key) {
                    (Some(Value::Assoc(map)), Key::Assoc(k)) => map.get(&k).cloned(),
                    (Some(Value::Indexed(map)), Key::Index(i)) => map.get(&i).cloned(),
                    (Some(Value::Scalar(s)), Key::Index(0)) => Some(s),
                    _ => None,
                };
                match element {
                    Some(value) => value,
                    None => return self.unset_in_arith(name),
                }
            }
        };
        self.arith_str(&text, depth.saturating_add(1))
    }

    fn unset_in_arith(&self, name: &str) -> Result<i64, Flow> {
        if self.options.nounset {
            self.error(format!("{name}: unbound variable"));
            return Err(Flow::Fatal(1));
        }
        Ok(0)
    }

    fn arith_write(&mut self, target: &ArithmeticTarget, value: i64, depth: u32) -> Exec {
        let result = match target {
            ArithmeticTarget::Variable(name) => self.vars.set(name, value.to_string()),
            ArithmeticTarget::ArrayElement(name, index) => {
                match self.element_key(name, index, depth)? {
                    Key::Assoc(key) => self.vars.set_key(name, key, value.to_string()),
                    Key::Index(i) => self.vars.set_index(name, i, value.to_string()),
                }
            }
        };
        result.map_err(|e| {
            self.error(e);
            Flow::Fatal(1)
        })?;
        Ok(0)
    }

    fn element_key(&mut self, name: &str, index: &ArithmeticExpr, depth: u32) -> Result<Key, Flow> {
        let assoc = matches!(
            self.vars.get(name).and_then(|v| v.value.as_ref()),
            Some(Value::Assoc(_))
        );
        if assoc {
            // The index of an associative array is a word, not arithmetic:
            // a bare name in it is that name.
            return Ok(Key::Assoc(match index {
                ArithmeticExpr::Reference(ArithmeticTarget::Variable(k)) => k.clone(),
                other => self.arith(other, depth)?.to_string(),
            }));
        }
        let i = self.arith(index, depth)?;
        Ok(Key::Index(self.index_from_end(name, i)))
    }

    /// A negative index counts from the end, as bash's does.
    pub(crate) fn index_from_end(&self, name: &str, index: i64) -> i64 {
        if index >= 0 {
            return index;
        }
        let last = match self.vars.get(name).and_then(|v| v.value.as_ref()) {
            Some(Value::Indexed(map)) => map.keys().next_back().copied().unwrap_or(-1),
            Some(Value::Scalar(_)) => 0,
            _ => -1,
        };
        last.wrapping_add(1).wrapping_add(index)
    }
}

enum Key {
    Index(i64),
    Assoc(String),
}

/// A number as arithmetic reads one: decimal, `0x` hex, `0` octal, or
/// `base#digits`.
pub(crate) fn parse_number(text: &str) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() {
        return None;
    }
    let value = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).ok()?
    } else if let Some((base, rest)) = digits.split_once('#') {
        let base: u32 = base.parse().ok()?;
        if !(2..=36).contains(&base) {
            return None;
        }
        i64::from_str_radix(rest, base).ok()?
    } else if digits.len() > 1 && digits.starts_with('0') {
        i64::from_str_radix(digits.get(1..)?, 8).ok()?
    } else {
        digits.parse().ok()?
    };
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}
