//! The `RichText` imports: `RichText(...)` (with a time as a part, `rich_time`),
//! `RichText.from_markdown(text)`, and the members `r.excerpt`, `r.text`, `r.markdown`,
//! `r.references`, `r.people` and `r.is_empty()` (see [`crate::rich`]).
//!
//! `r.excerpt`, `r.text` and `r.markdown` are the text as the person running the code reads
//! it: each reference to a resource they hold by its title now, one they cannot read as only
//! being there ([`Core::read_rich`]), never the title the text was written with. `r.markdown`
//! is the document as Markdown, each picture written where its bytes are
//! (`![](tilo-blob://…)`).

use std::collections::BTreeMap;
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
/// `r.markdown`: the document as Markdown.
pub const PART_MARKDOWN: i32 = 4;

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
                    PART_EXCERPT => Value::Text(self.read_rich(&r, true)),
                    PART_TEXT => Value::Text(self.read_rich(&r, false)),
                    PART_MARKDOWN => {
                        Value::Text(tilo_doc_codecs::rich::markdown_of_blocks(&r.blocks))
                    }
                    PART_REFERENCES => Value::List(self.referenced(&r)?),
                    PART_PEOPLE => Value::List(
                        r.references()
                            .into_iter()
                            .filter_map(|r| match r {
                                Reference::Person(id) => Some(Value::Principal(Principal::new(id))),
                                Reference::Resource(_) => None,
                            })
                            .collect(),
                    ),
                    other => return trap(format!("rich_part: no part {other}")),
                };
                self.ret_h(part)
            }
            Imp::RichEmpty => ret_i32(i32::from(self.rich_arg(a.h(0)?)?.is_empty())),
            // `RichText.from_markdown(text, links: pages, from: folder)`: the document a
            // Markdown text says, read the one way every app reads one (`tilo_doc_codecs::rich`),
            // a picture kept as the Markdown it is written as, and a link to a page of the
            // bundle the reference to that page — a note whose file the export wrote, which a
            // reader opens by the reference and what links to it lists.
            Imp::RichFromMarkdown => {
                let markdown = self.text(a.h(0)?)?;
                let pages = self.text_map(a.h(1)?)?;
                let from = self.text(a.h(2)?)?;
                let app = self.text(a.h(3)?)?;
                match tilo_doc_codecs::rich::blocks_of_markdown(&markdown) {
                    Ok(blocks) => {
                        let blocks = pages_read(blocks, &pages, &from, &app);
                        self.ret_h(Value::Rich(RichValue { blocks }))
                    }
                    Err(why) => trap(format!("this text is not a document: {why}")),
                }
            }
            // `r.markdown(links: pages, from: folder)`: the document as Markdown, each reference
            // to a page of the bundle written as the link to that page — the path from the
            // folder this one is written in, so a reader of the file follows it to the note.
            Imp::RichMarkdown => {
                let r = self.rich_arg(a.h(0)?)?;
                let pages = self.text_map(a.h(1)?)?;
                let from = self.text(a.h(2)?)?;
                let linked = pages_written(&r.blocks, &pages, &from);
                self.ret_h(Value::Text(tilo_doc_codecs::rich::markdown_of_blocks(
                    &linked,
                )))
            }
            // `RichText.embed(r)`: the text whose one paragraph projects `r`, which is what a
            // card of it shows where the text is drawn.
            Imp::RichEmbed => {
                let referred = match self.val(a.h(0)?)? {
                    Value::Ref(referred) => referred,
                    other => {
                        return trap(format!(
                            "rich_embed: expected a resource, got {}",
                            other.kind_name()
                        ));
                    }
                };
                let (title, reference) = self.reference_of(&referred)?;
                let mut out = RichValue::default();
                out.push_projection(&title, &reference);
                self.ret_h(Value::Rich(out))
            }
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
    pub fn read_rich(&self, r: &RichValue, excerpt: bool) -> String {
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
            self.note_reads(reads);
        }
        text
    }

    /// What a reference to a resource the reader cannot read says, in their language: the
    /// platform's phrase, the chip's wording.
    #[must_use]
    pub fn unshared(&self) -> String {
        let id = tilo_reference::embed::UNSHARED_PHRASE;
        self.word_args(id, &[], tilo_reference::embed::UNSHARED)
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

    /// A map the code holds (`Map<Text>()`, or one a view was handed), as its entries: the text
    /// each key leads to.
    fn text_map(&self, h: i32) -> HResult<BTreeMap<String, String>> {
        let Value::Map(entries) = self.val(h)? else {
            return trap("expected a map of text");
        };
        Ok(entries
            .into_iter()
            .filter_map(|(key, value)| {
                Some((key.as_text()?.to_owned(), value.as_text()?.to_owned()))
            })
            .collect())
    }
}

/// `blocks` as Markdown writes a bundle of pages: each reference to a page of `pages` — the
/// address the page stands at, by the resource it is — a link to it, the path from `from`, the
/// folder the page this text is in stands at. Every other reference stays what it was: text
/// naming a resource no file of the bundle holds.
fn pages_written(
    blocks: &[tilo_doc::snapshot::BlockView],
    pages: &BTreeMap<String, String>,
    from: &str,
) -> Vec<tilo_doc::snapshot::BlockView> {
    blocks
        .iter()
        .map(|block| {
            let mut out = block.clone();
            for mark in &mut out.marks {
                if mark.name != crate::rich::MENTION && mark.name != crate::rich::PROJECTION {
                    continue;
                }
                let Some(reference) = mark.value.as_str().and_then(Reference::of_mark) else {
                    continue;
                };
                let Some(path) = crate::rich::resource_id(&reference).and_then(|id| pages.get(id))
                else {
                    continue;
                };
                tilo_doc::schema::MARK_LINK.clone_into(&mut mark.name);
                mark.value = tilo_doc::document::MarkValue::Str(format!(
                    "<{}>",
                    crate::rich::relative_path(from, path)
                ));
            }
            out
        })
        .collect()
}

/// `blocks` as a bundle of pages reads: each link to a page of `pages` — the path it stands at,
/// by the resource it is, read from `from`, the folder this page stands at — the reference to
/// that resource, of the app `app`. A paragraph that is one such link and nothing else is the
/// card of the page it names, which is how an embed of it is written.
fn pages_read(
    blocks: Vec<tilo_doc::snapshot::BlockView>,
    pages: &BTreeMap<String, String>,
    from: &str,
    app: &str,
) -> Vec<tilo_doc::snapshot::BlockView> {
    blocks
        .into_iter()
        .map(|mut block| {
            let mut found: Vec<(usize, usize, String)> = Vec::new();
            for mark in &block.marks {
                if mark.name != tilo_doc::schema::MARK_LINK {
                    continue;
                }
                let Some(id) = mark
                    .value
                    .as_str()
                    .and_then(|href| crate::rich::bundle_path(from, href))
                    .and_then(|path| pages.get(&path))
                else {
                    continue;
                };
                found.push((mark.start, mark.end, id.clone()));
            }
            if found.is_empty() {
                return block;
            }
            let len = block.text.chars().count();
            let alone = block.kind == "paragraph"
                && found.len() == 1
                && found
                    .first()
                    .is_some_and(|(start, end, _)| *start == 0 && *end == len);
            block.marks.retain(|mark| {
                !(mark.name == tilo_doc::schema::MARK_LINK
                    && found
                        .iter()
                        .any(|(start, end, _)| *start == mark.start && *end == mark.end))
            });
            for (start, end, id) in found {
                block.marks.push(tilo_doc::document::MarkSpan {
                    name: if alone {
                        crate::rich::PROJECTION
                    } else {
                        crate::rich::MENTION
                    }
                    .to_owned(),
                    start,
                    end,
                    value: tilo_doc::document::MarkValue::Str(
                        Reference::resource(
                            app,
                            String::new(),
                            crate::rich::published_id(app, &id),
                        )
                        .mark_value(),
                    ),
                });
            }
            block
        })
        .collect()
}
