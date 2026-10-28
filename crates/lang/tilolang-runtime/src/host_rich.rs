//! The `RichText` imports: `RichText(...)`, `r.excerpt`, `r.text`, `r.references`,
//! `r.people` and `r.is_empty()` (see [`crate::rich`]).

use std::rc::Rc;

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_i32, trap};
use crate::engine::Wasm;
use crate::ids::Principal;
use crate::rich::{Reference, RichValue};
use crate::value::{RefValue, ResourceRef, Value};

/// `r.excerpt`: the first line with text.
pub const PART_EXCERPT: i32 = 0;
/// `r.text`: the whole text, a line per paragraph.
pub const PART_TEXT: i32 = 1;
/// `r.references`: the resources it refers to, as `host.Resource`s.
pub const PART_REFERENCES: i32 = 2;
/// `r.people`: the people it mentions.
pub const PART_PEOPLE: i32 = 3;

impl Core {
    pub(crate) fn rich_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::RichNew => {
                let parts = match self.val(a.h(0)?)? {
                    Value::List(parts) => parts,
                    other => {
                        return trap(format!(
                            "rich_new: expected a list, got {}",
                            other.kind_name()
                        ));
                    }
                };
                let mut out = RichValue::default();
                for part in parts {
                    self.push_part(&mut out, part)?;
                }
                self.ret_h(Value::Rich(out))
            }
            Imp::RichPart => {
                let r = self.rich_arg(a.h(0)?)?;
                let part = match a.h(1)? {
                    PART_EXCERPT => Value::Text(r.excerpt()),
                    PART_TEXT => Value::Text(r.plain_text()),
                    PART_REFERENCES => Value::List(self.referenced(&r)?),
                    PART_PEOPLE => Value::List(
                        r.references()
                            .into_iter()
                            .filter_map(|r| match r {
                                Reference::Person(id) => Some(Value::Principal(Principal(id))),
                                Reference::Resource(_) => None,
                            })
                            .collect(),
                    ),
                    other => return trap(format!("rich_part: no part {other}")),
                };
                self.ret_h(part)
            }
            Imp::RichEmpty => ret_i32(i32::from(self.rich_arg(a.h(0)?)?.is_empty())),
            _ => trap("not a rich text import"),
        }
    }

    fn rich_arg(&self, h: i32) -> HResult<RichValue> {
        self.with_val(h, |v| match v {
            Value::Rich(r) => Ok(r.clone()),
            other => trap(format!("expected rich text, got {}", other.kind_name())),
        })
    }

    /// Append one part of `RichText(...)`: text as it is, rich text with its formatting, a
    /// person or a resource as a run naming it and referring to it.
    fn push_part(&self, out: &mut RichValue, part: Value) -> HResult<()> {
        match part {
            Value::Text(text) => out.push_text(&text),
            Value::Rich(rich) => out.push_rich(&rich),
            Value::Principal(p) => {
                let name = self
                    .names
                    .borrow()
                    .as_ref()
                    .map_or_else(|| p.0.clone(), |host| host.name(&p));
                out.push_reference(&format!("@{name}"), &Reference::Person(p.0));
            }
            Value::Ref(r) => {
                let (title, reference) = self.reference_of(&r)?;
                out.push_reference(&title, &reference);
            }
            other => {
                return trap(format!(
                    "a {} cannot be a part of rich text: give text, rich text, a person or a resource",
                    other.kind_name()
                ));
            }
        }
        Ok(())
    }

    /// The run a reference to `r` reads as (its title) and the reference.
    fn reference_of(&self, r: &RefValue) -> HResult<(String, Reference)> {
        let (pkg, _) = self
            .registry
            .borrow()
            .type_info(&r.target.ty)
            .ok_or_else(|| {
                format!(
                    "type `{}` is not declared by any loaded package",
                    r.target.ty
                )
            })?;
        let meta = &pkg.meta;
        let Some(declared) = meta.types.iter().find(|t| t.id == r.target.ty) else {
            return trap(format!("type `{}` is not declared", r.target.ty));
        };
        let title = self.title_of(&r.target, meta, declared)?;
        Ok((title, crate::rich::resource_reference(&meta.app, &r.target)))
    }

    /// The resources `r` refers to that this store holds, as references.
    fn referenced(&self, r: &RichValue) -> HResult<Vec<Value>> {
        let author = Some(self.module_of_frame()?);
        let store = self.store.borrow();
        let mut out = Vec::new();
        for reference in r.references() {
            let Reference::Resource(name) = reference else {
                continue;
            };
            let Some(id) = crate::rich::resource_id(&name) else {
                continue;
            };
            let Some(mut target) = ResourceRef::parse_id(id, String::new()) else {
                continue;
            };
            // What this reader cannot reach is left out: a reference never grants a read.
            let Ok(ty) = store.type_of(&target) else {
                continue;
            };
            target.ty = ty;
            out.push(Value::Ref(RefValue {
                target: Rc::new(target),
                author: author.clone(),
            }));
        }
        Ok(out)
    }
}
