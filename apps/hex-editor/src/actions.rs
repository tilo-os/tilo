//! The hex editor's effects: work that reaches outside the buffer (open
//! and save a file) and the views of the editor (search, go to offset).
//! The byte edits themselves are the record's ([`HexEditor`](crate::HexEditor)).
//!
//! Presentation — the cursor, the selection, the scroll position, the
//! insert-vs-overwrite mode, the hex-vs-ASCII focus — stays in the view.

// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::Effect;

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

/// Work outside the buffer: reading a file into it and writing it out.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "hex_editor.effects", device_only)]
pub enum HexEffect {
    /// Open a file.
    #[effect(job, cancellable, label = "Open file")]
    OpenFile {
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
    /// Show the byte search bar.
    #[effect(view, idempotent, label = "Search bytes")]
    Search,
    /// Show the go-to-offset prompt.
    #[effect(view, idempotent, label = "Go to offset")]
    GotoOffset,
}

/// A view of the editor a caller opens: the search bar, the go-to prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexPanel {
    Search,
    GotoOffset,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Hex;
    use crate::model::BufferEdit;
    use ui::tilo_actions::OperationKind;

    #[test]
    fn bytes_parse_as_people_write_them() {
        assert_eq!(parse_bytes("0xFF"), Ok(vec![0xff]));
        assert_eq!(parse_bytes("DE-AD be ef"), Ok(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(parse_bytes(""), Ok(Vec::new()));
        assert!(parse_bytes("xyz").is_err());
        assert!(
            parse_bytes("abc").is_err(),
            "an odd digit count is no bytes"
        );
    }

    #[test]
    fn a_run_of_typing_or_deleting_continues_while_it_stays_in_place() {
        let overwrite = |offset, bytes: &[u8]| BufferEdit::Overwrite {
            offset,
            bytes: Hex(bytes.to_vec()),
        };
        let insert = |offset, bytes: &[u8]| BufferEdit::Insert {
            offset,
            bytes: Hex(bytes.to_vec()),
        };
        let delete = |offset| BufferEdit::Delete { offset, len: 1 };
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
    }

    #[test]
    fn effects_and_views_are_published_with_their_contracts() {
        let specs = HexEffect::specs();
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(
            ids,
            ["open-file", "save", "save-as", "search", "goto-offset"]
        );
        for spec in &specs[..3] {
            assert_eq!(spec.kind, OperationKind::Effect);
        }
        for spec in &specs[3..] {
            assert_eq!(spec.kind, OperationKind::View);
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
    }
}
