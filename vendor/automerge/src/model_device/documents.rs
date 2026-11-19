//! The host's side of a model module's documents.

use std::collections::HashMap;
use std::ops::Bound;

use super::wire::{Malformed, Put, Reader, Writer};
use super::{op, ANSWERED, REFUSED};
use crate::exid::ExId;
use crate::hydrate;
use crate::iter::Span;
use crate::marks::{ExpandMark, Mark, UpdateSpansConfig};
use crate::op_set2::ValueRef;
use crate::transaction::{CommitOptions, Transactable};
use crate::{
    AutoCommit, ChangeHash, Cursor, CursorPosition, MoveCursor, ObjType, Prop, ReadDoc,
    ScalarValue, TextEncoding, Value,
};

/// The documents one call of a model module has open: the request's own as
/// handle 0, and any the module made or forked since, each released when the
/// module lets go of it.
#[derive(Debug)]
pub struct Documents {
    docs: Vec<Option<AutoCommit>>,
}

/// Why a call was not answered.
enum Refusal {
    Malformed,
    Said(String),
}

impl From<Malformed> for Refusal {
    fn from(_: Malformed) -> Self {
        Refusal::Malformed
    }
}

impl<E: std::fmt::Display> From<E> for Refusal
where
    E: std::error::Error,
{
    fn from(error: E) -> Self {
        Refusal::Said(error.to_string())
    }
}

type Answer = Result<Writer, Refusal>;

fn answer<T: Put + ?Sized>(value: &T) -> Answer {
    let mut w = Writer::new();
    w.put(value);
    Ok(w)
}

fn nothing() -> Answer {
    Ok(Writer::new())
}

fn value_of(value: &ValueRef<'_>) -> Value<'static> {
    match value {
        ValueRef::Object(kind) => Value::Object(*kind),
        ValueRef::Scalar(scalar) => Value::from(scalar),
    }
}

impl Documents {
    /// A host whose module's request is about `document` (handle 0).
    pub fn new(document: AutoCommit) -> Self {
        Self {
            docs: vec![Some(document)],
        }
    }

    /// The request's document, as the module has left it so far.
    pub fn document(&mut self) -> &mut AutoCommit {
        self.docs[0].get_or_insert_with(AutoCommit::new)
    }

    /// The request's document, once the module is done with it.
    pub fn into_document(mut self) -> AutoCommit {
        self.docs.swap_remove(0).unwrap_or_default()
    }

    /// Answer one call, as [`wire`](super::wire) wrote it: [`ANSWERED`] and
    /// what was asked, or [`REFUSED`] and why not.
    pub fn answer(&mut self, call: &[u8]) -> Vec<u8> {
        match self.run(&mut Reader::new(call)) {
            Ok(Writer(mut bytes)) => {
                bytes.insert(0, ANSWERED);
                bytes
            }
            Err(refusal) => {
                let reason = match refusal {
                    Refusal::Malformed => "the document call is malformed".to_owned(),
                    Refusal::Said(reason) => reason,
                };
                let mut w = Writer::new();
                w.byte(REFUSED);
                w.put(reason.as_str());
                w.0
            }
        }
    }

    fn open(&mut self, doc: AutoCommit) -> Answer {
        let handle = self.docs.len() as u32;
        let mut w = Writer::new();
        w.put(&handle).put(doc.get_actor()).put(&doc.text_encoding());
        self.docs.push(Some(doc));
        Ok(w)
    }

    fn doc(&mut self, handle: u32) -> Result<&mut AutoCommit, Refusal> {
        self.docs
            .get_mut(handle as usize)
            .and_then(Option::as_mut)
            .ok_or_else(|| Refusal::Said(format!("the model has no document {handle}")))
    }

    fn run(&mut self, r: &mut Reader<'_>) -> Answer {
        let call = r.byte()?;
        match call {
            op::NEW => {
                let encoding: TextEncoding = r.take()?;
                return self.open(AutoCommit::new_with_encoding(encoding));
            }
            op::LOAD => {
                let doc = AutoCommit::load(r.bytes()?)?;
                return self.open(doc);
            }
            _ => {}
        }
        let handle: u32 = r.take()?;
        match call {
            op::CLONE => {
                let doc = self.doc(handle)?.clone();
                self.open(doc)
            }
            op::FORK => {
                let doc = self.doc(handle)?.fork();
                self.open(doc)
            }
            op::FORK_AT => {
                let heads: Vec<ChangeHash> = r.take()?;
                let doc = self.doc(handle)?.fork_at(&heads)?;
                self.open(doc)
            }
            op::COMMITTED_FORK_AT => {
                let heads: Vec<ChangeHash> = r.take()?;
                let doc = self.doc(handle)?.committed_fork_at(&heads)?;
                self.open(doc)
            }
            op::RELEASE => {
                // The request's own document stays the host's: it is what
                // the call made.
                if handle != 0 {
                    if let Some(slot) = self.docs.get_mut(handle as usize) {
                        *slot = None;
                    }
                }
                nothing()
            }
            op::MERGE => {
                let other: u32 = r.take()?;
                let mut theirs = self
                    .docs
                    .get_mut(other as usize)
                    .and_then(Option::take)
                    .ok_or(Refusal::Malformed)?;
                let merged = self.doc(handle).map(|doc| doc.merge(&mut theirs));
                self.docs[other as usize] = Some(theirs);
                answer(&merged??)
            }
            op::SET_ACTOR => {
                let actor = r.take()?;
                self.doc(handle)?.set_actor(actor);
                nothing()
            }
            op::GET_HEADS => answer(&self.doc(handle)?.get_heads()),
            op::COMMITTED_HEADS => answer(&self.doc(handle)?.committed_heads()),
            op::COMMIT_WITH => {
                let message: Option<String> = r.take()?;
                let time: Option<i64> = r.take()?;
                let options = CommitOptions { message, time };
                answer(&self.doc(handle)?.commit_with(options))
            }
            op::ROLLBACK => answer(&self.doc(handle)?.rollback()),
            op::EMPTY_CHANGE => {
                let message: Option<String> = r.take()?;
                let time: Option<i64> = r.take()?;
                let options = CommitOptions { message, time };
                answer(&self.doc(handle)?.empty_change(options))
            }
            op::PENDING_OPS => answer(&self.doc(handle)?.pending_ops()),
            op::BASE_HEADS => answer(&self.doc(handle)?.base_heads()),
            op::ISOLATE => {
                let heads: Vec<ChangeHash> = r.take()?;
                self.doc(handle)?.isolate(&heads);
                nothing()
            }
            op::INTEGRATE => {
                self.doc(handle)?.integrate();
                nothing()
            }
            op::SAVE => answer(self.doc(handle)?.save().as_slice()),
            op::SAVE_NOCOMPRESS => answer(self.doc(handle)?.save_nocompress().as_slice()),
            op::SAVE_AFTER => {
                let heads: Vec<ChangeHash> = r.take()?;
                answer(self.doc(handle)?.save_after(&heads).as_slice())
            }
            op::SAVE_INCREMENTAL => answer(self.doc(handle)?.save_incremental().as_slice()),
            op::SAVE_COMMITTED => answer(self.doc(handle)?.save_committed().as_slice()),
            op::LOAD_INCREMENTAL => {
                let bytes = r.bytes()?;
                answer(&self.doc(handle)?.load_incremental(bytes)?)
            }
            op::MISSING_DEPS => {
                let heads: Vec<ChangeHash> = r.take()?;
                answer(&self.doc(handle)?.get_missing_deps(&heads))
            }
            op::IMPORT => {
                let text: String = r.take()?;
                answer(&self.doc(handle)?.import(&text)?)
            }
            op::HASH_FOR_OPID => {
                let id: ExId = r.take()?;
                answer(&self.doc(handle)?.hash_for_opid(&id))
            }
            op::IS_EMPTY => answer(&self.doc(handle)?.is_empty()),
            op::DESCRIBE => {
                let doc = self.doc(handle)?;
                let mut w = Writer::new();
                w.put(&handle).put(doc.get_actor()).put(&doc.text_encoding());
                Ok(w)
            }
            op::CHANGE_META => {
                let hash: ChangeHash = r.take()?;
                answer(&self.doc(handle)?.committed_change_meta(&hash))
            }
            op::HASH_OF_ACTOR_SEQ => {
                let actor: crate::ActorId = r.take()?;
                let seq: u64 = r.take()?;
                answer(&self.doc(handle)?.committed_hash_of_actor_seq(&actor, seq))
            }
            op::APPLIED_SEQ => {
                let actor: crate::ActorId = r.take()?;
                answer(&self.doc(handle)?.committed_applied_seq(&actor))
            }
            op::RESET_DIFF_CURSOR => {
                self.doc(handle)?.reset_diff_cursor();
                nothing()
            }
            op::UPDATE_DIFF_CURSOR => {
                self.doc(handle)?.update_diff_cursor();
                nothing()
            }
            op::DIFF_CURSOR => answer(&self.doc(handle)?.diff_cursor()),
            op::REPLACE_VIEW => {
                let view: Option<Vec<ChangeHash>> = r.take()?;
                answer(&self.doc(handle)?.replace_view(view))
            }
            _ => self.read_or_write(call, handle, r),
        }
    }

    fn read_or_write(&mut self, call: u8, handle: u32, r: &mut Reader<'_>) -> Answer {
        let doc = self.doc(handle)?;
        let obj: ExId = r.take()?;
        match call {
            op::KEYS => {
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let keys: Vec<String> = match &at {
                    Some(heads) => doc.keys_at(&obj, heads).collect(),
                    None => doc.keys(&obj).collect(),
                };
                answer(&keys)
            }
            op::MAP_RANGE => {
                let range: (Bound<String>, Bound<String>) = r.take()?;
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let items: Vec<(String, Value<'static>, bool, ExId)> = match &at {
                    Some(heads) => doc
                        .map_range_at(&obj, range, heads)
                        .map(|item| {
                            let id = item.id();
                            (item.key.into_owned(), value_of(&item.value), item.conflict, id)
                        })
                        .collect(),
                    None => doc
                        .map_range(&obj, range)
                        .map(|item| {
                            let id = item.id();
                            (item.key.into_owned(), value_of(&item.value), item.conflict, id)
                        })
                        .collect(),
                };
                answer(&items)
            }
            op::LIST_RANGE => {
                let range: (Bound<usize>, Bound<usize>) = r.take()?;
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let items: Vec<(usize, Value<'static>, bool, ExId)> = match &at {
                    Some(heads) => doc
                        .list_range_at(&obj, range, heads)
                        .map(|item| (item.index, value_of(&item.value), item.conflict, item.id()))
                        .collect(),
                    None => doc
                        .list_range(&obj, range)
                        .map(|item| (item.index, value_of(&item.value), item.conflict, item.id()))
                        .collect(),
                };
                answer(&items)
            }
            op::VALUES => {
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let values: Vec<(Value<'static>, ExId)> = match &at {
                    Some(heads) => doc
                        .values_at(&obj, heads)
                        .map(|(value, id)| (value.into_owned(), id))
                        .collect(),
                    None => doc
                        .values(&obj)
                        .map(|(value, id)| (value.into_owned(), id))
                        .collect(),
                };
                answer(&values)
            }
            op::LENGTH => {
                let at: Option<Vec<ChangeHash>> = r.take()?;
                answer(&match &at {
                    Some(heads) => doc.length_at(&obj, heads),
                    None => doc.length(&obj),
                })
            }
            op::OBJECT_TYPE => answer(&doc.object_type(&obj)?),
            op::MARKS => {
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let marks: Vec<Mark> = match &at {
                    Some(heads) => doc.marks_at(&obj, heads)?,
                    None => doc.marks(&obj)?,
                };
                answer(&marks)
            }
            op::GET_MARKS => {
                let index: usize = r.take()?;
                let at: Option<Vec<ChangeHash>> = r.take()?;
                answer(&doc.get_marks(&obj, index, at.as_deref())?)
            }
            op::TEXT => {
                let at: Option<Vec<ChangeHash>> = r.take()?;
                answer(&match &at {
                    Some(heads) => doc.text_at(&obj, heads)?,
                    None => doc.text(&obj)?,
                })
            }
            op::SPANS => {
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let spans: Vec<Span> = match &at {
                    Some(heads) => doc.spans_at(&obj, heads)?.collect(),
                    None => doc.spans(&obj)?.collect(),
                };
                answer(&spans)
            }
            op::GET => {
                let prop: Prop = r.take()?;
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let got = match &at {
                    Some(heads) => doc.get_at(&obj, prop, heads)?,
                    None => doc.get(&obj, prop)?,
                };
                answer(&got.map(|(value, id)| (value.into_owned(), id)))
            }
            op::GET_ALL => {
                let prop: Prop = r.take()?;
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let all = match &at {
                    Some(heads) => doc.get_all_at(&obj, prop, heads)?,
                    None => doc.get_all(&obj, prop)?,
                };
                let all: Vec<(Value<'static>, ExId)> = all
                    .into_iter()
                    .map(|(value, id)| (value.into_owned(), id))
                    .collect();
                answer(&all)
            }
            op::PARENTS => {
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let parents = match &at {
                    Some(heads) => doc.parents_at(&obj, heads)?,
                    None => doc.parents(&obj)?,
                };
                let parents: Vec<(ExId, Prop, bool, ObjType)> = parents
                    .map(|parent| (parent.obj, parent.prop, parent.visible, parent.typ))
                    .collect();
                answer(&parents)
            }
            op::GET_CURSOR => {
                let position: CursorPosition = r.take()?;
                let at: Option<Vec<ChangeHash>> = r.take()?;
                let moving: Option<MoveCursor> = r.take()?;
                let cursor: Cursor = match moving {
                    Some(moving) => doc.get_cursor_moving(&obj, position, at.as_deref(), moving)?,
                    None => doc.get_cursor(&obj, position, at.as_deref())?,
                };
                answer(&cursor)
            }
            op::GET_CURSOR_POSITION => {
                let cursor: Cursor = r.take()?;
                let at: Option<Vec<ChangeHash>> = r.take()?;
                answer(&doc.get_cursor_position(&obj, &cursor, at.as_deref())?)
            }
            op::HYDRATE => {
                let at: Option<Vec<ChangeHash>> = r.take()?;
                answer(&doc.hydrate(&obj, at.as_deref())?)
            }
            op::PUT => {
                let prop: Prop = r.take()?;
                let value: ScalarValue = r.take()?;
                doc.put(&obj, prop, value)?;
                nothing()
            }
            op::PUT_OBJECT => {
                let prop: Prop = r.take()?;
                let kind: ObjType = r.take()?;
                answer(&doc.put_object(&obj, prop, kind)?)
            }
            op::INSERT => {
                let index: usize = r.take()?;
                let value: ScalarValue = r.take()?;
                doc.insert(&obj, index, value)?;
                nothing()
            }
            op::INSERT_OBJECT => {
                let index: usize = r.take()?;
                let kind: ObjType = r.take()?;
                answer(&doc.insert_object(&obj, index, kind)?)
            }
            op::INCREMENT => {
                let prop: Prop = r.take()?;
                let by: i64 = r.take()?;
                doc.increment(&obj, prop, by)?;
                nothing()
            }
            op::DELETE => {
                let prop: Prop = r.take()?;
                doc.delete(&obj, prop)?;
                nothing()
            }
            op::SPLICE => {
                let pos: usize = r.take()?;
                let del: isize = r.take()?;
                let values: Vec<hydrate::Value> = r.take()?;
                doc.splice(&obj, pos, del, values)?;
                nothing()
            }
            op::SPLICE_TEXT => {
                let pos: usize = r.take()?;
                let del: isize = r.take()?;
                let text = r.str()?;
                doc.splice_text(&obj, pos, del, text)?;
                nothing()
            }
            op::MARK => {
                let mark: Mark = r.take()?;
                let expand: ExpandMark = r.take()?;
                doc.mark(&obj, mark, expand)?;
                nothing()
            }
            op::UNMARK => {
                let name: String = r.take()?;
                let start: usize = r.take()?;
                let end: usize = r.take()?;
                let expand: ExpandMark = r.take()?;
                doc.unmark(&obj, &name, start, end, expand)?;
                nothing()
            }
            op::SPLIT_BLOCK => {
                let index: usize = r.take()?;
                answer(&doc.split_block(&obj, index)?)
            }
            op::JOIN_BLOCK => {
                let index: usize = r.take()?;
                doc.join_block(&obj, index)?;
                nothing()
            }
            op::REPLACE_BLOCK => {
                let index: usize = r.take()?;
                answer(&doc.replace_block(&obj, index)?)
            }
            op::UPDATE_TEXT => {
                let text = r.str()?;
                doc.update_text(&obj, text)?;
                nothing()
            }
            op::UPDATE_SPANS => {
                let default_expand: ExpandMark = r.take()?;
                let per_mark: Vec<(String, ExpandMark)> = r.take()?;
                let spans: Vec<Span> = r.take()?;
                let config = UpdateSpansConfig {
                    default_expand,
                    per_mark_expands: per_mark.into_iter().collect::<HashMap<_, _>>(),
                };
                doc.update_spans(&obj, config, spans)?;
                nothing()
            }
            op::UPDATE_OBJECT => {
                let value: hydrate::Value = r.take()?;
                doc.update_object(&obj, &value)?;
                nothing()
            }
            op::BATCH_CREATE_OBJECT => {
                let prop: Prop = r.take()?;
                let value: hydrate::Value = r.take()?;
                let insert: bool = r.take()?;
                answer(&doc.batch_create_object(&obj, prop, &value, insert)?)
            }
            op::INIT_ROOT_FROM_HYDRATE => {
                let value: hydrate::Map = r.take()?;
                doc.init_root_from_hydrate(&value)?;
                nothing()
            }
            _ => Err(Refusal::Said(format!("no document call {call}"))),
        }
    }
}
