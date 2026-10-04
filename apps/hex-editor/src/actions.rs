//! The hex editor's vocabulary: the byte edits that change a buffer, the
//! effects that reach outside it (open and save a file), and the views of
//! the editor (search, go to offset).
//!
//! Every byte edit — a keystroke, the palette, an agent, a script — is one
//! [`HexEditorAction`] submitted through the framework's door, so it is
//! checked, undone per author, shown in history and synced with the record.
//! Presentation — the cursor, the selection, the scroll position, the
//! insert-vs-overwrite mode, the hex-vs-ASCII focus — stays in the view.

use serde::{Deserialize, Serialize};

use ui::tilo_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionSpec, ChangeKind,
    OperationKind, ParamType,
};
use ui::tilo_resource::{ActionDecl, Doc, Effect};

use crate::model::BufferEdit;
use crate::resource::HexEditor;

/// Registry `type_id` for every hex-editor action.
pub const HEX_EDITOR_ACTION_TYPE: &str = "hex_editor.action";
/// Operation type of the editor's views (search, go to offset).
pub const HEX_EDITOR_VIEW_TYPE: &str = "hex_editor.view";

/// One change to the buffer. Every keystroke that changes the bytes reduces
/// to an insert, delete or overwrite at a concrete offset; opening a file
/// replaces the whole buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HexEditorAction {
    /// Show `bytes`, read from the file `name`, as the buffer.
    Open {
        name: String,
        #[serde(with = "hex_text")]
        bytes: Vec<u8>,
    },
    /// Splice `bytes` in *before* `offset`, growing the buffer.
    InsertBytes {
        offset: usize,
        #[serde(with = "hex_text")]
        bytes: Vec<u8>,
    },
    /// Remove `len` bytes starting at `offset`.
    DeleteBytes { offset: usize, len: usize },
    /// Replace the bytes at `offset..offset + bytes.len()` in place; the
    /// buffer never grows, so bytes past its end are dropped.
    OverwriteBytes {
        offset: usize,
        #[serde(with = "hex_text")]
        bytes: Vec<u8>,
    },
}

impl Action for HexEditorAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        crate::resource::apply(self, doc)
    }

    /// A file's bytes come from outside: opening one syncs to everyone, but
    /// no undo takes it back.
    fn change_kind(&self) -> ChangeKind {
        match self {
            Self::Open { .. } => ChangeKind::External,
            _ => ChangeKind::Content,
        }
    }
}

impl HexEditorAction {
    /// The action carrying one edit the view made to its buffer.
    pub fn from_edit(edit: BufferEdit) -> Self {
        match edit {
            BufferEdit::Insert { offset, bytes } => Self::InsertBytes { offset, bytes },
            BufferEdit::Delete { offset, len } => Self::DeleteBytes { offset, len },
            BufferEdit::Overwrite { offset, bytes } => Self::OverwriteBytes { offset, bytes },
        }
    }

    /// The edited span `(offset, len)` of a typing or deleting edit.
    fn span(&self) -> Option<(usize, usize)> {
        match self {
            Self::InsertBytes { offset, bytes } | Self::OverwriteBytes { offset, bytes } => {
                Some((*offset, bytes.len()))
            }
            Self::DeleteBytes { offset, len } => Some((*offset, *len)),
            Self::Open { .. } => None,
        }
    }

    /// Whether `next`, right after `self`, continues the same run of typing
    /// or deleting: the next nibble or byte typed, the next byte deleted.
    pub fn continues(&self, next: &Self) -> bool {
        let (Some((at, len)), Some((next_at, next_len))) = (self.span(), next.span()) else {
            return false;
        };
        match (self, next) {
            (
                Self::InsertBytes { .. } | Self::OverwriteBytes { .. },
                Self::InsertBytes { .. } | Self::OverwriteBytes { .. },
            ) => next_at >= at && next_at <= at.saturating_add(len),
            // Delete steps in place (Delete) or backwards (Backspace).
            (Self::DeleteBytes { .. }, Self::DeleteBytes { .. }) => {
                next_at == at || next_at.saturating_add(next_len) == at
            }
            _ => false,
        }
    }
}

/// Bytes as hexadecimal text in an action's payload: compact, and what a
/// person or an agent writes (`"de ad be ef"`).
mod hex_text {
    use std::fmt::Write as _;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        serializer.serialize_str(&text)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        super::parse_bytes(&text).map_err(serde::de::Error::custom)
    }
}

/// Parse hexadecimal bytes as a person writes them: `ff`, `0xFF`,
/// `de ad be ef`, `DE-AD`. An empty text is no bytes.
///
/// # Errors
/// Names what is not hexadecimal bytes.
pub fn parse_bytes(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    crate::search::parse_hex_query(text)
        .ok_or_else(|| format!("{text:?} is not hexadecimal bytes (e.g. `de ad be ef`)"))
}

fn offset_arg(args: &ActionArgs, name: &str) -> Result<usize, String> {
    let value = args
        .integer(name)
        .ok_or_else(|| format!("`{name}` is required"))?;
    usize::try_from(value).map_err(|_| format!("`{name}` must not be negative"))
}

fn bytes_arg(args: &ActionArgs) -> Result<Vec<u8>, String> {
    parse_bytes(args.text("bytes").unwrap_or_default())
}

const OFFSET: ActionParam =
    ActionParam::required("offset", ParamType::Integer, "byte offset, from 0");
const BYTES: ActionParam = ActionParam::required(
    "bytes",
    ParamType::Text,
    "hexadecimal bytes, e.g. `de ad be ef`",
);

/// The byte edits offered by name, at any offset.
pub(crate) fn declarations() -> Vec<ActionDecl<HexEditor>> {
    vec![
        ActionDecl::new(
            "insert-bytes",
            "Insert bytes",
            vec![OFFSET, BYTES],
            |args, _| {
                Ok(HexEditorAction::InsertBytes {
                    offset: offset_arg(args, "offset")?,
                    bytes: bytes_arg(args)?,
                })
            },
        )
        .describe("Insert bytes before an offset, growing the buffer."),
        ActionDecl::new(
            "overwrite-bytes",
            "Overwrite bytes",
            vec![OFFSET, BYTES],
            |args, _| {
                Ok(HexEditorAction::OverwriteBytes {
                    offset: offset_arg(args, "offset")?,
                    bytes: bytes_arg(args)?,
                })
            },
        )
        .describe("Replace the bytes at an offset in place."),
        ActionDecl::new(
            "delete-bytes",
            "Delete bytes",
            vec![
                OFFSET,
                ActionParam::required("len", ParamType::Integer, "how many bytes"),
            ],
            |args, _| {
                Ok(HexEditorAction::DeleteBytes {
                    offset: offset_arg(args, "offset")?,
                    len: offset_arg(args, "len")?,
                })
            },
        )
        .describe("Remove bytes starting at an offset.")
        .destructive(),
    ]
}

const BYTE: ActionParam = ActionParam::required("byte", ParamType::Text, "Byte (hex), e.g. ff");

fn one_byte(args: &ActionArgs) -> Result<Vec<u8>, String> {
    let bytes = parse_bytes(args.text("byte").unwrap_or_default())?;
    if bytes.len() == 1 {
        Ok(bytes)
    } else {
        Err("give one byte, e.g. `ff`".into())
    }
}

/// The byte edits bound to where the user is: insert or overwrite at the
/// cursor, delete the selection or the byte under the cursor.
pub(crate) fn cursor_declarations(
    cursor: usize,
    len: usize,
    selection: Option<(usize, usize)>,
) -> Vec<ActionDecl<HexEditor>> {
    // The append position is valid for an insert even in an empty buffer.
    let at = cursor.min(len);
    let mut decls = vec![ActionDecl::new(
        "insert-byte",
        "Insert byte",
        vec![BYTE],
        move |args, _| {
            Ok(HexEditorAction::InsertBytes {
                offset: at,
                bytes: one_byte(args)?,
            })
        },
    )];
    if cursor < len {
        decls.push(ActionDecl::new(
            "overwrite-byte",
            "Overwrite byte",
            vec![BYTE],
            move |args, _| {
                Ok(HexEditorAction::OverwriteBytes {
                    offset: cursor,
                    bytes: one_byte(args)?,
                })
            },
        ));
    }
    if let Some((lo, hi)) = selection {
        decls.push(
            ActionDecl::new(
                "delete-selection",
                "Delete selection",
                vec![],
                move |_, _| {
                    Ok(HexEditorAction::DeleteBytes {
                        offset: lo,
                        len: hi - lo + 1,
                    })
                },
            )
            .destructive(),
        );
    } else if cursor < len {
        decls.push(
            ActionDecl::new("delete-byte", "Delete byte", vec![], move |_, _| {
                Ok(HexEditorAction::DeleteBytes {
                    offset: cursor,
                    len: 1,
                })
            })
            .destructive(),
        );
    }
    decls
}

/// Work outside the buffer: reading a file into it and writing it out.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "hex_editor.effects")]
pub enum HexEffect {
    /// Open a file.
    #[effect(job, cancellable, label = "Open file")]
    Open {
        #[param("file to open; omit to use the system file picker")]
        path: Option<String>,
    },
    /// Save the buffer to the file it was opened from, or choose one.
    #[effect(job, cancellable, label = "Save file")]
    Save,
    /// Save the buffer to another file, replacing what is there.
    #[effect(destructive, label = "Save as")]
    SaveAs {
        #[param("save destination")]
        path: String,
    },
}

/// A view of the editor a caller opens: the search bar, the go-to prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HexPanel {
    Search,
    GotoOffset,
}

/// The editor's views, published beside the resource's actions.
#[allow(
    clippy::expect_used,
    reason = "a unit enum always serializes; the spec builder has no error path"
)]
pub(crate) fn view_operations() -> Vec<ActionSpec> {
    [
        ("search", "Search bytes", HexPanel::Search),
        ("goto-offset", "Go to offset", HexPanel::GotoOffset),
    ]
    .into_iter()
    .map(|(id, label, view)| {
        ActionSpec::operation(id, label, OperationKind::View, HEX_EDITOR_VIEW_TYPE, &view)
            .expect("a hex view always serializes")
            .with_contract(ActionContract::view())
    })
    .collect()
}

/// The view an operation of [`HEX_EDITOR_VIEW_TYPE`] opens.
///
/// # Errors
/// Returns the decoder's error for a malformed payload.
pub fn decode_view(type_id: &str, payload: &[u8]) -> Result<Option<HexPanel>, String> {
    if type_id != HEX_EDITOR_VIEW_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_travel_as_hexadecimal_text_and_parse_as_people_write_them() {
        let action = HexEditorAction::OverwriteBytes {
            offset: 2,
            bytes: vec![0xde, 0xad],
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"OverwriteBytes": {"offset": 2, "bytes": "dead"}})
        );
        let parsed: HexEditorAction = serde_json::from_value(
            serde_json::json!({"InsertBytes": {"offset": 0, "bytes": "DE-AD be ef"}}),
        )
        .unwrap();
        assert_eq!(
            parsed,
            HexEditorAction::InsertBytes {
                offset: 0,
                bytes: vec![0xde, 0xad, 0xbe, 0xef]
            }
        );
        assert_eq!(parse_bytes("0xFF"), Ok(vec![0xff]));
        assert_eq!(parse_bytes(""), Ok(Vec::new()));
        assert!(parse_bytes("xyz").is_err());
        assert!(
            parse_bytes("abc").is_err(),
            "an odd digit count is no bytes"
        );
    }

    #[test]
    fn a_run_of_typing_or_deleting_continues_while_it_stays_in_place() {
        let overwrite = |offset, bytes: &[u8]| HexEditorAction::OverwriteBytes {
            offset,
            bytes: bytes.to_vec(),
        };
        let insert = |offset, bytes: &[u8]| HexEditorAction::InsertBytes {
            offset,
            bytes: bytes.to_vec(),
        };
        let delete = |offset| HexEditorAction::DeleteBytes { offset, len: 1 };
        // Two nibbles of one byte, then the next byte.
        assert!(overwrite(4, &[0xa0]).continues(&overwrite(4, &[0xab])));
        assert!(overwrite(4, &[0xab]).continues(&overwrite(5, &[0xc0])));
        assert!(!overwrite(4, &[0xab]).continues(&overwrite(9, &[0xc0])));
        // Insert mode: a byte inserted, completed in place, the next inserted.
        assert!(insert(4, &[0xa0]).continues(&overwrite(4, &[0xab])));
        assert!(overwrite(4, &[0xab]).continues(&insert(5, &[0xc0])));
        // Delete in place and Backspace.
        assert!(delete(4).continues(&delete(4)));
        assert!(delete(4).continues(&delete(3)));
        assert!(!delete(4).continues(&delete(7)));
        assert!(!delete(4).continues(&insert(4, &[1])));
        let open = HexEditorAction::Open {
            name: "a".into(),
            bytes: vec![],
        };
        assert!(!open.continues(&insert(0, &[1])));
        assert_eq!(open.change_kind(), ChangeKind::External);
    }

    #[test]
    fn effects_and_views_are_published_with_their_contracts() {
        let specs = HexEffect::specs();
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(ids, ["open", "save", "save-as"]);
        for spec in &specs {
            assert_eq!(spec.kind, OperationKind::Effect);
        }
        assert!(!specs[0].params()[0].required, "open may use the picker");
        assert!(specs[1].params().is_empty());
        assert!(specs[2].params()[0].required);
        assert!(
            matches!(
                specs[1].contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::None
            ),
            "saving in place is what Ctrl+S asks for"
        );
        assert!(
            !matches!(
                specs[2].contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::None
            ),
            "replacing another file is confirmed"
        );
        let views = view_operations();
        assert_eq!(
            views
                .iter()
                .map(|spec| spec.id.as_str())
                .collect::<Vec<_>>(),
            ["search", "goto-offset"]
        );
        let payload = serde_json::to_vec(&HexPanel::GotoOffset).unwrap();
        assert_eq!(
            decode_view(HEX_EDITOR_VIEW_TYPE, &payload).unwrap(),
            Some(HexPanel::GotoOffset)
        );
        assert_eq!(decode_view("other", &payload).unwrap(), None);
    }
}
