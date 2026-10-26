//! Imports over plain values: text, lists, tuples, options and enum values.

use crate::abi::Imp;
use crate::core::{
    Args, Core, HResult, ordering_to_i32, ret_f64, ret_i32, ret_i64, ret_void, trap,
};
use crate::engine::Wasm;
use crate::num::{len_to_i64, to_usize};
use crate::value::{EnumValue, Value, same_value};

fn index(v: i64, what: &str) -> HResult<usize> {
    match to_usize(v) {
        Some(i) => Ok(i),
        None => trap(format!("{what} index {v} is negative")),
    }
}

fn int_of(v: &Value) -> HResult<i64> {
    v.as_int().map_or_else(
        || trap(format!("expected an int, got {}", v.kind_name())),
        Ok,
    )
}

fn float_of(v: &Value) -> HResult<f64> {
    match v {
        Value::Float(f) => Ok(*f),
        other => trap(format!("expected a float, got {}", other.kind_name())),
    }
}

impl Core {
    pub(crate) fn text_import(&self, imp: Imp, mem: &[u8], a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::StrLit => {
                let ptr = a.h(0)?.cast_unsigned() as usize;
                let len = a.h(1)?.cast_unsigned() as usize;
                let bytes = ptr
                    .checked_add(len)
                    .and_then(|end| mem.get(ptr..end))
                    .map_or_else(|| trap("string literal is outside guest memory"), Ok)?;
                match std::str::from_utf8(bytes) {
                    Ok(s) => self.ret_h(Value::text(s)),
                    Err(e) => trap(format!("string literal is not utf-8: {e}")),
                }
            }
            Imp::StrConcat => {
                let mut s = self.text(a.h(0)?)?;
                s.push_str(&self.text(a.h(1)?)?);
                self.ret_h(Value::Text(s))
            }
            Imp::StrEq => {
                let eq = self.text(a.h(0)?)? == self.text(a.h(1)?)?;
                ret_i32(i32::from(eq))
            }
            Imp::StrLen => ret_i64(len_to_i64(self.text(a.h(0)?)?.chars().count())),
            Imp::IntToStr => self.ret_h(Value::Text(a.long(0)?.to_string())),
            Imp::FloatToStr => self.ret_h(Value::Text(a.float(0)?.to_string())),
            Imp::StrCmp => {
                let o = self.text(a.h(0)?)?.cmp(&self.text(a.h(1)?)?);
                ret_i32(ordering_to_i32(o))
            }
            _ => trap("not a text import"),
        }
    }

    pub(crate) fn aggregate_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::ListNew => self.ret_h(Value::List(Vec::new())),
            Imp::ListPushI => self.list_push(a.h(0)?, Value::Int(a.long(1)?)),
            Imp::ListPushF => self.list_push(a.h(0)?, Value::Float(a.float(1)?)),
            Imp::ListPushH => {
                let v = self.val_whole(a.h(1)?)?;
                self.list_push(a.h(0)?, v)
            }
            Imp::ListLen => match self.list_or_coll(a.h(0)?)? {
                ListOrColl::Len(len) => ret_i64(len_to_i64(len)),
                ListOrColl::Coll(coll) => ret_i64(len_to_i64(self.coll_len(&coll)?)),
            },
            Imp::ListGetI | Imp::ListGetF | Imp::ListGetH => {
                let at = index(a.long(1)?, "list")?;
                let item = self.with_val(a.h(0)?, |v| match v {
                    Value::List(items) => items.get(at).cloned().map_or_else(
                        || {
                            trap(format!(
                                "list index {at} out of range for length {}",
                                items.len()
                            ))
                        },
                        Ok,
                    ),
                    Value::Coll(coll) => Ok(Value::Coll(std::rc::Rc::clone(coll))),
                    other => trap(format!("expected a list, got {}", other.kind_name())),
                })?;
                let item = match item {
                    Value::Coll(coll) => self.coll_get(&coll, at)?,
                    item => item,
                };
                self.element(imp, item)
            }
            Imp::ListEnd => {
                let last = a.h(1)? != 0;
                let end = self.with_val(a.h(0)?, |v| match v {
                    Value::List(items) => Ok(Ok(if last { items.last() } else { items.first() }
                        .cloned()
                        .map_or_else(Value::none, Value::some))),
                    Value::Coll(coll) => Ok(Err(std::rc::Rc::clone(coll))),
                    other => trap(format!("expected a list, got {}", other.kind_name())),
                })?;
                let end = match end {
                    Ok(end) => end,
                    Err(coll) => self.coll_end(&coll, last)?,
                };
                self.ret_h(end)
            }
            Imp::ListSortByKey => {
                let items = self.list_items(a.h(0)?)?;
                let keys = self.list_items(a.h(1)?)?;
                self.ret_h(Value::List(sorted_by_keys(&items, &keys)?))
            }
            Imp::TupleNew => {
                let n = index(i64::from(a.h(0)?), "tuple size")?;
                self.ret_h(Value::Tuple(vec![Value::Unit; n]))
            }
            Imp::TupleSetI => self.tuple_set(a.h(0)?, a.h(1)?, Value::Int(a.long(2)?)),
            Imp::TupleSetF => self.tuple_set(a.h(0)?, a.h(1)?, Value::Float(a.float(2)?)),
            Imp::TupleSetH => {
                let v = self.val_whole(a.h(2)?)?;
                self.tuple_set(a.h(0)?, a.h(1)?, v)
            }
            Imp::TupleGetI | Imp::TupleGetF | Imp::TupleGetH => {
                let at = index(i64::from(a.h(1)?), "tuple")?;
                let item = self.with_val(a.h(0)?, |v| match v {
                    Value::Tuple(items) => items.get(at).cloned().map_or_else(
                        || {
                            trap(format!(
                                "tuple index {at} out of range for size {}",
                                items.len()
                            ))
                        },
                        Ok,
                    ),
                    other => trap(format!("expected a tuple, got {}", other.kind_name())),
                })?;
                self.element(imp, item)
            }
            Imp::SomeH => {
                let v = self.val_whole(a.h(0)?)?;
                self.ret_h(Value::some(v))
            }
            Imp::SomeI => self.ret_h(Value::some(Value::Int(a.long(0)?))),
            Imp::SomeF => self.ret_h(Value::some(Value::Float(a.float(0)?))),
            Imp::NoneV => self.ret_h(Value::none()),
            Imp::IsSome => self.with_val(a.h(0)?, |v| match v {
                Value::Opt(o) => ret_i32(i32::from(o.is_some())),
                other => trap(format!("expected an option, got {}", other.kind_name())),
            }),
            Imp::UnwrapI | Imp::UnwrapF | Imp::UnwrapH => {
                let inner = self.with_val(a.h(0)?, |v| match v {
                    Value::Opt(Some(x)) => Ok((**x).clone()),
                    Value::Opt(None) => trap("unwrap of none"),
                    other => trap(format!("expected an option, got {}", other.kind_name())),
                })?;
                self.element(imp, inner)
            }
            _ => trap("not an aggregate import"),
        }
    }

    /// The imports over enum values, and `value_eq`.
    pub(crate) fn enum_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::EnumNew => {
                let (ty, variant) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                self.ret_h(Value::Enum(EnumValue {
                    ty,
                    variant,
                    fields: std::collections::BTreeMap::new(),
                }))
            }
            Imp::EnumSetI => self.enum_set(a.h(0)?, a.h(1)?, Value::Int(a.long(2)?)),
            Imp::EnumSetF => self.enum_set(a.h(0)?, a.h(1)?, Value::Float(a.float(2)?)),
            Imp::EnumSetH => {
                let v = self.val_whole(a.h(2)?)?;
                self.enum_set(a.h(0)?, a.h(1)?, v)
            }
            Imp::EnumIs => {
                let variant = self.text(a.h(1)?)?;
                self.with_val(a.h(0)?, |v| match v {
                    Value::Enum(e) => ret_i32(i32::from(e.variant == variant)),
                    other => trap(format!("expected an enum value, got {}", other.kind_name())),
                })
            }
            Imp::EnumGetI | Imp::EnumGetF | Imp::EnumGetH => {
                let field = self.text(a.h(1)?)?;
                let item = self.with_val(a.h(0)?, |v| match v {
                    Value::Enum(e) => e.fields.get(&field).cloned().map_or_else(
                        || trap(format!("variant `{}` has no field `{field}`", e.variant)),
                        Ok,
                    ),
                    other => trap(format!("expected an enum value, got {}", other.kind_name())),
                })?;
                self.element(imp, item)
            }
            Imp::ValueEq => {
                let (x, y) = (self.val_whole(a.h(0)?)?, self.val_whole(a.h(1)?)?);
                ret_i32(i32::from(same_value(&x, &y)))
            }
            _ => trap("not an enum import"),
        }
    }

    fn enum_set(&self, e: i32, field: i32, item: Value) -> HResult<Option<Wasm>> {
        let field = self.text(field)?;
        let mut frames = self.frames.borrow_mut();
        match frames.get_mut(e)? {
            Value::Enum(v) => {
                v.fields.insert(field, item);
            }
            other => return trap(format!("expected an enum value, got {}", other.kind_name())),
        }
        ret_void()
    }

    fn list_items(&self, h: i32) -> HResult<Vec<Value>> {
        match self.val_whole(h)? {
            Value::List(items) => Ok(items),
            other => trap(format!("expected a list, got {}", other.kind_name())),
        }
    }

    /// The length of the list behind `h`, or the collection to read it from.
    fn list_or_coll(&self, h: i32) -> HResult<ListOrColl> {
        self.with_val(h, |v| match v {
            Value::List(items) => Ok(ListOrColl::Len(items.len())),
            Value::Coll(coll) => Ok(ListOrColl::Coll(std::rc::Rc::clone(coll))),
            other => trap(format!("expected a list, got {}", other.kind_name())),
        })
    }

    /// Hand an element, tuple member or unwrapped value to the guest in the form the import
    /// name promises.
    fn element(&self, imp: Imp, item: Value) -> HResult<Option<Wasm>> {
        match imp {
            Imp::ListGetI | Imp::TupleGetI | Imp::UnwrapI | Imp::EnumGetI => {
                ret_i64(int_of(&item)?)
            }
            Imp::ListGetF | Imp::TupleGetF | Imp::UnwrapF | Imp::EnumGetF => {
                ret_f64(float_of(&item)?)
            }
            _ => self.ret_h(item),
        }
    }

    fn list_push(&self, list: i32, item: Value) -> HResult<Option<Wasm>> {
        // A collection pushed onto is a list of its items from then on.
        if self.with_val(list, |v| Ok(matches!(v, Value::Coll(_))))? {
            let whole = self.val_whole(list)?;
            *self.frames.borrow_mut().get_mut(list)? = whole;
        }
        let mut frames = self.frames.borrow_mut();
        match frames.get_mut(list)? {
            Value::List(items) => items.push(item),
            other => return trap(format!("expected a list, got {}", other.kind_name())),
        }
        ret_void()
    }

    fn tuple_set(&self, tuple: i32, at: i32, item: Value) -> HResult<Option<Wasm>> {
        let at = index(i64::from(at), "tuple")?;
        let mut frames = self.frames.borrow_mut();
        match frames.get_mut(tuple)? {
            Value::Tuple(items) => match items.get_mut(at) {
                Some(slot) => *slot = item,
                None => return trap(format!("tuple index {at} out of range")),
            },
            other => return trap(format!("expected a tuple, got {}", other.kind_name())),
        }
        ret_void()
    }
}

/// A list's length, or the collection whose length a read records.
enum ListOrColl {
    Len(usize),
    Coll(std::rc::Rc<crate::value::CollRef>),
}

/// The items ordered by their parallel keys, ascending and stable. Keys of one list must be
/// all ints, all floats, all text or all bools.
fn sorted_by_keys(items: &[Value], keys: &[Value]) -> HResult<Vec<Value>> {
    if items.len() != keys.len() {
        return trap(format!("{} keys for {} items", keys.len(), items.len()));
    }
    let mut order: Vec<usize> = (0..items.len()).collect();
    let mut failure: Option<String> = None;
    order.sort_by(|x, y| match (keys.get(*x), keys.get(*y)) {
        (Some(a), Some(b)) => compare_keys(a, b).unwrap_or_else(|e| {
            failure.get_or_insert(e);
            std::cmp::Ordering::Equal
        }),
        _ => std::cmp::Ordering::Equal,
    });
    if let Some(e) = failure {
        return trap(e);
    }
    Ok(order
        .into_iter()
        .filter_map(|i| items.get(i).cloned())
        .collect())
}

fn compare_keys(a: &Value, b: &Value) -> Result<std::cmp::Ordering, String> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => Ok(x.total_cmp(y)),
        (Value::Text(x), Value::Text(y)) => Ok(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        _ => Err(format!(
            "sort keys must be of one type, found {} and {}",
            a.kind_name(),
            b.kind_name()
        )),
    }
}
