//! The `RichText` imports: `RichText(...)` (with a time as a part, `rich_time`), `r.excerpt`,
//! `r.text`, `r.references`, `r.people` and `r.is_empty()` (see [`crate::rich`]).
//!
//! `r.excerpt` and `r.text` are the text as the person running the code reads it: each
//! reference to a resource they hold by its title now, one they cannot read as only being
//! there ([`Core::read_rich`]), never the title the text was written with.

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
                    PART_EXCERPT => Value::Text(self.read_rich(&r, true)?),
                    PART_TEXT => Value::Text(self.read_rich(&r, false)?),
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
            Imp::RichTime => {
                let at = match self.val(a.h(0)?)? {
                    Value::Text(at) => at,
                    other => {
                        return trap(format!(
                            "rich_time: expected a time, got {}",
                            other.kind_name()
                        ));
                    }
                };
                let mut out = RichValue::default();
                out.push_time(&at);
                self.ret_h(Value::Rich(out))
            }
            _ => trap("not a rich text import"),
        }
    }

    /// The text of `r` as the person running the code reads it (its first line with text,
    /// for `excerpt`): each reference as the store says it reads for them
    /// ([`crate::store::Store::reading`]). A view that reads it is read again when a resource
    /// it refers to changes.
    ///
    /// # Errors
    /// A trap when the frame cannot note what it read.
    pub fn read_rich(&self, r: &RichValue, excerpt: bool) -> HResult<String> {
        let unshared = self.unshared();
        let mut reads: Vec<(String, String)> = Vec::new();
        let text = {
            let store = self.store.borrow();
            let registry = self.registry.borrow();
            let mut read = |reference: &Reference| {
                let reading = store.reading(reference);
                // What it is called now is read from the resource: a view reading this reads
                // again when the resource is renamed or arrives.
                if let Some(target) = crate::rich::resource_id(reference)
                    .and_then(|id| ResourceRef::parse_id(id, String::new()))
                {
                    let id = target.id_string();
                    reads.push((id.clone(), crate::store::ANY_FIELD.to_owned()));
                    if let Ok(ty) = store.type_of(&target)
                        && let Some((_, info)) = registry.type_info(&ty)
                    {
                        reads.extend(info.fields.iter().map(|f| (id.clone(), f.id.clone())));
                    }
                }
                reading
            };
            if excerpt {
                r.read_excerpt(&mut read, &unshared)
            } else {
                r.read_text(&mut read, &unshared)
            }
        };
        if !reads.is_empty() && self.frames.borrow().depth() > 0 {
            self.note_reads(reads)?;
        }
        Ok(text)
    }

    /// What a reference to a resource the reader cannot read says, in their language: the
    /// platform's phrase, the chip's wording.
    #[must_use]
    pub fn unshared(&self) -> String {
        let id = tilo_reference::embed::UNSHARED_PHRASE;
        match self.phrases.borrow().as_ref() {
            Some(phrases) => {
                let said = phrases(id, &[]);
                if said == id {
                    tilo_reference::embed::UNSHARED.to_owned()
                } else {
                    said
                }
            }
            None => tilo_reference::embed::UNSHARED.to_owned(),
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
        Ok((
            title,
            crate::rich::resource_reference(&meta.app, &declared.name, &r.target),
        ))
    }

    /// The resources `r` refers to that this store holds, as references.
    fn referenced(&self, r: &RichValue) -> HResult<Vec<Value>> {
        let author = Some(self.module_of_frame()?);
        let store = self.store.borrow();
        let mut out = Vec::new();
        for reference in r.references() {
            let Some(id) = crate::rich::resource_id(&reference) else {
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
