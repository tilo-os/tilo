//! Imports over plain values: text, lists, tuples, options and enum values.

use std::rc::Rc;

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
            // Every count, place and cut is `tilolang::text`'s: graphemes, so what a person
            // sees as one character is one.
            Imp::StrLen => ret_i64(len_to_i64(tilolang::text::len(&self.text(a.h(0)?)?))),
            Imp::IntToStr => self.ret_h(Value::Text(a.long(0)?.to_string())),
            Imp::StrToInt => {
                let number = self.text(a.h(0)?)?.trim().parse::<i64>().ok();
                self.ret_h(Value::Opt(number.map(|n| Box::new(Value::Int(n)))))
            }
            Imp::FloatToStr => self.ret_h(Value::Text(a.float(0)?.to_string())),
            Imp::DurationText => self.ret_h(Value::Text(tilolang::duration::text(a.long(0)?))),
            Imp::StrCmp => {
                let o = self.text(a.h(0)?)?.cmp(&self.text(a.h(1)?)?);
                ret_i32(ordering_to_i32(o))
            }
            // Where `needle` first stands in `text`, counted as `len` counts, or none.
            Imp::StrFind => {
                let text = self.text(a.h(0)?)?;
                let needle = self.text(a.h(1)?)?;
                let at = tilolang::text::find(&text, &needle)
                    .map_or_else(Value::none, |at| Value::some(Value::Int(len_to_i64(at))));
                self.ret_h(at)
            }
            Imp::StrSlice => {
                let text = self.text(a.h(0)?)?;
                let sliced = tilolang::text::slice(&text, a.long(1)?, a.long(2)?);
                self.ret_h(Value::Text(sliced))
            }
            Imp::StrTrim => {
                let text = self.text(a.h(0)?)?;
                self.ret_h(Value::Text(tilolang::text::trim(&text).to_owned()))
            }
            Imp::StrLower => {
                let text = self.text(a.h(0)?)?;
                self.ret_h(Value::Text(tilolang::text::lower(&text)))
            }
            Imp::StrUpper => {
                let text = self.text(a.h(0)?)?;
                self.ret_h(Value::Text(tilolang::text::upper(&text)))
            }
            // `text` is the receiver, `what` the text the method was given.
            Imp::StrStartsWith | Imp::StrEndsWith | Imp::StrContains | Imp::StrMatches => {
                let text = self.text(a.h(0)?)?;
                let what = self.text(a.h(1)?)?;
                let yes = match imp {
                    Imp::StrStartsWith => tilolang::text::starts_with(&text, &what),
                    Imp::StrEndsWith => tilolang::text::ends_with(&text, &what),
                    Imp::StrContains => tilolang::text::contains(&text, &what),
                    _ => tilolang::text::matches(&what, &text),
                };
                ret_i32(i32::from(yes))
            }
            Imp::StrSplit => {
                let text = self.text(a.h(0)?)?;
                let sep = self.text(a.h(1)?)?;
                let pieces = tilolang::text::split(&text, &sep)
                    .into_iter()
                    .map(Value::Text)
                    .collect();
                self.ret_h(Value::List(pieces))
            }
            Imp::StrJoin => {
                let pieces = self.list_text(a.h(0)?, "join")?;
                let sep = self.text(a.h(1)?)?;
                let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
                self.ret_h(Value::Text(tilolang::text::join(&refs, &sep)))
            }
            Imp::StrReplace => {
                let text = self.text(a.h(0)?)?;
                let from = self.text(a.h(1)?)?;
                let to = self.text(a.h(2)?)?;
                let replaced = if from.is_empty() {
                    text
                } else {
                    text.replace(&from, &to)
                };
                self.ret_h(Value::Text(replaced))
            }
            _ => trap("not a text import"),
        }
    }

    /// The number surface: what a number's own methods lower to, and what a list of numbers
    /// sums and averages.
    pub(crate) fn num_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::IntMin => ret_i64(a.long(0)?.min(a.long(1)?)),
            Imp::IntMax => ret_i64(a.long(0)?.max(a.long(1)?)),
            // A low bound above the high one clamps to the high one, as either bound alone
            // would; nothing traps.
            Imp::IntClamp => ret_i64(a.long(0)?.max(a.long(1)?).min(a.long(2)?)),
            Imp::IntAbs => ret_i64(a.long(0)?.saturating_abs()),
            Imp::FloatMin => ret_f64(a.float(0)?.min(a.float(1)?)),
            Imp::FloatMax => ret_f64(a.float(0)?.max(a.float(1)?)),
            Imp::FloatClamp => ret_f64(a.float(0)?.max(a.float(1)?).min(a.float(2)?)),
            Imp::FloatAbs => ret_f64(a.float(0)?.abs()),
            Imp::FloatRound => ret_i64(crate::num::round_whole(a.float(0)?)),
            Imp::FloatFloor => ret_i64(crate::num::floor_whole(a.float(0)?)),
            Imp::FloatCeil => ret_i64(crate::num::ceil_whole(a.float(0)?)),
            Imp::FloatRoundTo => ret_f64(crate::num::round_to(a.float(0)?, a.long(1)?)),
            Imp::FloatFloorTo => ret_f64(crate::num::floor_to(a.float(0)?, a.long(1)?)),
            Imp::FloatCeilTo => ret_f64(crate::num::ceil_to(a.float(0)?, a.long(1)?)),
            Imp::ListSumI => {
                let mut sum: i64 = 0;
                for item in self.list_items(a.h(0)?)? {
                    match item {
                        Value::Int(n) => sum = sum.saturating_add(n),
                        other => {
                            return trap(format!(
                                "expected a list of ints, got a {}",
                                other.kind_name()
                            ));
                        }
                    }
                }
                ret_i64(sum)
            }
            Imp::ListSumF => {
                let mut sum = 0.0;
                for item in self.list_items(a.h(0)?)? {
                    sum += float_of(&item)?;
                }
                ret_f64(sum)
            }
            // An average of nothing is nothing: no number divides by no items.
            Imp::ListAvg => {
                let items = self.list_items(a.h(0)?)?;
                let mut sum = 0.0;
                for item in &items {
                    sum += match item {
                        Value::Int(n) => crate::num::int_to_float(*n),
                        other => float_of(other)?,
                    };
                }
                let count = crate::num::int_to_float(len_to_i64(items.len()));
                ret_f64(if items.is_empty() { 0.0 } else { sum / count })
            }
            _ => trap("not a number import"),
        }
    }

    /// The texts of a list of text.
    fn list_text(&self, h: i32, what: &str) -> HResult<Vec<String>> {
        let mut out = Vec::new();
        for item in self.list_items(h)? {
            match item {
                Value::Text(t) => out.push(t),
                other => {
                    return trap(format!(
                        "expected a list of text to {what}, got a {}",
                        other.kind_name()
                    ));
                }
            }
        }
        Ok(out)
    }

    /// A list in another order: sorted by the keys beside it, or reversed (`sort_by(k, desc)`).
    fn ordered(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let mut items = self.list_items(a.h(0)?)?;
        if imp == Imp::ListReverse {
            items.reverse();
            return self.ret_h(Value::List(items));
        }
        let keys = self.list_items(a.h(1)?)?;
        self.ret_h(Value::List(sorted_by_keys(&items, &keys)?))
    }

    /// Where an item stands: in a list as it stands, or in a collection's items.
    fn index_of(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        // A scalar item comes boxed (`some_i`, `some_f`), as a set's elements do.
        let want = crate::host_res::unboxed(self.val_whole(a.h(1)?)?);
        let at = self.with_val(a.h(0)?, |v| match v {
            Value::List(items) => Ok(Some(items.iter().position(|item| same_value(item, &want)))),
            Value::Coll(_) => Ok(None),
            other => trap(format!("expected a list, got {}", other.kind_name())),
        })?;
        // A collection's items are the store's to order, so it is read through its items.
        let at = match at {
            Some(at) => at,
            None => self
                .items_of(self.val(a.h(0)?)?, "index_of")?
                .iter()
                .position(|item| same_value(item, &want)),
        };
        let at = at.map_or_else(Value::none, |at| {
            Value::some(Value::Int(i64::try_from(at).unwrap_or(i64::MAX)))
        });
        self.ret_h(at)
    }

    /// The item before `x` in a list, as the view's loop shows it: a row's place is the host's,
    /// so the row above it is read without walking the rows before it. None past the first, and
    /// none for an item the list does not hold.
    fn previous(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let want = self.val_whole(a.h(1)?)?;
        let list = self.with_val(a.h(0)?, |v| match v {
            Value::List(items) => {
                let at = items
                    .iter()
                    .position(|item| same_value(item, &want))
                    .and_then(|at| at.checked_sub(1))
                    .and_then(|at| items.get(at).cloned());
                Ok(Ok(at))
            }
            Value::Coll(coll) => Ok(Err(Rc::clone(coll))),
            other => trap(format!("expected a list, got {}", other.kind_name())),
        })?;
        let value = match list {
            Ok(item) => item.map_or_else(Value::none, Value::some),
            Err(coll) => {
                let at = match self.place_of(&coll, &want) {
                    Some(at) => Some(at),
                    None => match Self::item_id(&want) {
                        Some(id) => match self.coll_listing(&coll)?.position(id) {
                            Some(at) => at,
                            None => self
                                .coll_listing(&coll)?
                                .iter()
                                .position(|other| *other == *id),
                        },
                        None => None,
                    },
                };
                match (at, Self::item_id(&want)) {
                    (Some(at), Some(item)) => self.coll_before(&coll, at, item)?,
                    _ => Value::none(),
                }
            }
        };
        self.ret_h(value)
    }

    /// Where `item` stands in `coll` while a view renders the row of it: the place the host gave
    /// the row, which the row's own body knows without reading the rows before it.
    fn place_of(&self, coll: &Rc<crate::value::CollRef>, item: &Value) -> Option<usize> {
        let place = self.row_place.borrow().clone();
        let (rows, at) = place?;
        (Rc::ptr_eq(&rows, coll) && Self::item_id(item).is_some()).then_some(at)
    }

    /// The item `v` is a reference to, if it is a reference.
    fn item_id(v: &Value) -> Option<&str> {
        match v {
            Value::Ref(reference) => reference.target.item.as_deref(),
            _ => None,
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
            // `len` is the length the store knows without reading the run; `count` is how many
            // items it holds, read to the run's end where the store cannot say; whether it
            // holds nothing reads the first item, not the run.
            Imp::ListLen | Imp::CollCount | Imp::CollEmpty => match self.list_or_coll(a.h(0)?)? {
                ListOrColl::Len(len) if imp == Imp::CollEmpty => ret_i32(i32::from(len == 0)),
                ListOrColl::Len(len) => ret_i64(len_to_i64(len)),
                ListOrColl::Coll(coll) if imp == Imp::CollEmpty => {
                    ret_i32(i32::from(self.coll_empty(&coll)?))
                }
                ListOrColl::Coll(coll) => {
                    let count = imp == Imp::CollCount;
                    ret_i64(len_to_i64(self.coll_len_or_count(&coll, count)?))
                }
            },
            // A list's count is the app's own, but what it was built from is the store's: until
            // the store has read all of the workspace, either is the part read so far, which a
            // view shows as not yet known rather than as the count.
            Imp::CollCounted => match self.list_or_coll(a.h(0)?)? {
                ListOrColl::Len(len) => self.ret_h(option_int(self.settled().then_some(len))),
                ListOrColl::Coll(coll) => self.ret_h(option_int(self.coll_counted(&coll)?)),
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
            Imp::ListIndexOf => self.index_of(a),
            Imp::ListPrevious => self.previous(a),
            Imp::ListReverse | Imp::ListSortByKey => self.ordered(imp, a),
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
            Imp::SomeH
            | Imp::SomeI
            | Imp::SomeF
            | Imp::NoneV
            | Imp::IsSome
            | Imp::UnwrapI
            | Imp::UnwrapF
            | Imp::UnwrapH => self.option_import(imp, a),
            _ => trap("not an aggregate import"),
        }
    }

    /// The imports over options: `some`, `none`, `is_some` and `unwrap`.
    fn option_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
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
            _ => trap("not an option import"),
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
                    // A `json.Value` has a case per kind of JSON (`json.parse`).
                    Value::Json(json) => ret_i32(i32::from(crate::json::kind_of(json) == variant)),
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
                    Value::Json(json) => crate::json::payload_of(json, &field).map_or_else(
                        || {
                            trap(format!(
                                "a json {} has no `{field}`",
                                crate::json::kind_of(json)
                            ))
                        },
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

    /// `Map<T>.keys()` and `Map<T>.get(key)` on a map the code holds as a value (one
    /// `json.decode<Map<T>>` read, or a `json.Value` object's fields), as against the map a
    /// record's field is.
    pub(crate) fn map_value_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let Value::Map(entries) = self.val(a.h(0)?)? else {
            return trap("expected a map");
        };
        if imp == Imp::MapKeysValue {
            return self.ret_h(Value::List(
                entries.into_iter().map(|(key, _)| key).collect(),
            ));
        }
        let key = self.text(a.h(1)?)?;
        let found = entries
            .into_iter()
            .find(|(held, _)| held.as_text() == Some(key.as_str()))
            .map(|(_, value)| Box::new(value));
        self.ret_h(Value::Opt(found))
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

    /// The items of the list behind `h`, as values: what an import that draws one of them or
    /// reorders them works on.
    pub(crate) fn list_items(&self, h: i32) -> HResult<Vec<Value>> {
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

/// How many items a view shows, as a handle: some when the store has read the whole workspace,
/// none before it has ([`Core::coll_counted`]).
fn option_int(count: Option<usize>) -> Value {
    count.map_or_else(Value::none, |n| Value::some(Value::Int(len_to_i64(n))))
}
