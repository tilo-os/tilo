//! Notes-owned presentation only. Canonical content is always projected from the record.
use crate::{PageId, model::TagId};
use serde::{Deserialize, Serialize};
use ui::{application_view::ApplicationLocalView, view::ThawError};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotesLocalViewV1 {
    #[serde(deserialize_with = "optional_key")]
    pub selected_page: Option<PageId>,
    #[serde(deserialize_with = "optional_key")]
    pub active_tag: Option<TagId>,
    pub drawer_open: bool,
    #[serde(deserialize_with = "keys")]
    pub expanded_pages: Vec<PageId>,
    pub editor_scroll: f64,
    pub focus: NotesFocus,
    pub anchor: Option<BlockPosition>,
    pub caret: Option<BlockPosition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotesFocus {
    None,
    Sidebar,
    Title,
    Editor,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockPosition {
    pub block_id: String,
    pub byte_offset: usize,
}

fn key<E: serde::de::Error>(value: &str) -> Result<PageId, E> {
    let id = value.parse::<PageId>().map_err(E::custom)?;
    if id.is_nil() || id.to_string() != value {
        return Err(E::custom("invalid canonical page/tag UUID"));
    }
    Ok(id)
}
fn optional_key<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<PageId>, D::Error> {
    Option::<String>::deserialize(de)?
        .as_deref()
        .map(key)
        .transpose()
}
fn keys<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<PageId>, D::Error> {
    Vec::<String>::deserialize(de)?
        .iter()
        .map(|id| key(id))
        .collect()
}

impl NotesLocalViewV1 {
    pub(crate) fn decode(local: &ApplicationLocalView) -> Result<Self, ThawError> {
        if local.app_id != <crate::NotesShell as ui::framework::ResourceApp>::ID {
            return Err(ThawError::BindingMismatch);
        }
        if local.version != 1 {
            return Err(ThawError::UnsupportedVersion {
                found: local.version,
                supported: 1,
            });
        }
        let view: Self =
            serde_json::from_slice(&local.payload).map_err(|e| ThawError::State(e.to_string()))?;
        view.validate()?;
        Ok(view)
    }
    pub(crate) fn encode(&self) -> Result<ApplicationLocalView, ThawError> {
        self.validate()?;
        Ok(ApplicationLocalView {
            app_id: <crate::NotesShell as ui::framework::ResourceApp>::ID.into(),
            version: 1,
            payload: serde_json::to_vec(self).map_err(|e| ThawError::State(e.to_string()))?,
        })
    }
    fn validate(&self) -> Result<(), ThawError> {
        let invalid = |message: &str| ThawError::State(message.into());
        if !self.editor_scroll.is_finite() || self.editor_scroll < 0.0 {
            return Err(invalid("invalid editor scroll"));
        }
        if self.selected_page.is_some_and(|id| id.is_nil())
            || self.active_tag.is_some_and(|id| id.is_nil())
            || self.expanded_pages.iter().any(uuid::Uuid::is_nil)
        {
            return Err(invalid("nil page/tag UUID"));
        }
        if self.anchor.is_some() && self.caret.is_none() {
            return Err(invalid("anchor requires a caret"));
        }
        for position in self.anchor.iter().chain(&self.caret) {
            let id = &position.block_id;
            if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
                return Err(invalid("invalid block id"));
            }
        }
        Ok(())
    }
}

pub(crate) struct PreparedFocus {
    pub focus: NotesFocus,
    pub anchor: Option<ui::widget::DocumentPosition>,
    pub caret: Option<ui::widget::DocumentPosition>,
}
impl PreparedFocus {
    /// Where `view`'s caret and selection are in `editor`, among the blocks
    /// `durable` names: the ones the note stores, not a placeholder.
    pub fn resolve(
        view: &NotesLocalViewV1,
        editor: &ui::widget::BlockEditor,
        durable: &dyn Fn(&str) -> bool,
    ) -> Self {
        let blocks = editor.blocks();
        let first = blocks.iter().position(|block| durable(&block.id));
        let resolve = |position: &BlockPosition| {
            blocks
                .iter()
                .enumerate()
                .find(|(_, block)| block.id == position.block_id && durable(&block.id))
                .map(|(index, block)| {
                    let text = block.text();
                    let mut byte = position.byte_offset.min(text.len());
                    while !text.is_char_boundary(byte) {
                        byte = byte.saturating_sub(1);
                    }
                    ui::widget::DocumentPosition { block: index, byte }
                })
        };
        let anchor = view.anchor.as_ref().and_then(resolve);
        let caret = view.caret.as_ref().and_then(resolve);
        let stale =
            view.anchor.is_some() && anchor.is_none() || view.caret.is_some() && caret.is_none();
        Self {
            focus: view.focus,
            anchor: if stale { None } else { anchor },
            caret: if stale {
                first.map(|block| ui::widget::DocumentPosition { block, byte: 0 })
            } else {
                caret
            },
        }
    }
}
