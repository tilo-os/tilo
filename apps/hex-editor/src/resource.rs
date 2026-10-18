//! The hex buffer as a record: a file's bytes, kept in the document so
//! they sync, undo and show in history like any document.
//!
//! The bytes are a [`ByteBuffer`], stored in pages and edited at its path
//! in its own vocabulary ([`BufferEdit`]): a keystroke writes the page it
//! lands in. The verbs below offer the same edits by offset, for the
//! palette, agents and scripts; loading a file is a verb of its own, which
//! no undo takes back.

use ui::framework::{LaunchInput, ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::record::RecordId;
use ui::tilo_resource::{self, Atom, Invoke, Model, Refusal, resource};
use ui::widget::Element;

use crate::buffer::{ByteBuffer, Hex};
use crate::model::BufferEdit;
use crate::view::HexView;

ui::embedded_catalog!(static CATALOG, ["en-US", "de-DE"]);

/// The open buffer.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct HexEditor {
    /// File name
    ///
    /// The opened file's name; `None` until a file is opened. Written whole
    /// by a load.
    #[model(readonly)]
    pub name: Option<Atom>,
    /// Loads
    ///
    /// How many times a file was loaded into this record: a view takes the
    /// bytes after a change of it as freshly loaded, not as edits.
    #[model(readonly)]
    pub opened: u64,
    /// Bytes
    pub bytes: ByteBuffer,
}

impl HexEditor {
    pub fn has_file(&self) -> bool {
        self.name.is_some()
    }

    /// The opened file's name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|name| name.0.as_str())
    }
}

#[resource(kind = "hex-editor.document", label = "Hex document")]
impl HexEditor {
    /// Load a file's bytes.
    ///
    /// The bytes come from outside: loading them syncs to everyone, but no
    /// undo takes them back. Unchanged bytes of the same file change nothing.
    #[action(external)]
    pub fn load(
        &mut self,
        #[param("The file's name")] name: String,
        #[param("The file's bytes")] bytes: Hex,
    ) {
        if self.name() == Some(name.as_str()) && self.bytes == bytes.0 {
            return;
        }
        self.name = Some(Atom(name));
        self.opened = self.opened.saturating_add(1);
        self.bytes = ByteBuffer::of(&bytes.0);
    }

    /// Insert bytes before an offset, growing the buffer.
    #[action]
    pub fn insert_bytes(
        &mut self,
        #[param("Byte offset, from 0")] offset: usize,
        #[param("Hexadecimal bytes, e.g. `de ad be ef`")] bytes: Hex,
    ) -> Result<(), Refusal> {
        self.bytes.edit(&BufferEdit::Insert { offset, bytes })
    }

    /// Replace the bytes at an offset in place.
    #[action]
    pub fn overwrite_bytes(
        &mut self,
        #[param("Byte offset, from 0")] offset: usize,
        #[param("Hexadecimal bytes, e.g. `de ad be ef`")] bytes: Hex,
    ) -> Result<(), Refusal> {
        self.bytes.edit(&BufferEdit::Overwrite { offset, bytes })
    }

    /// Remove bytes starting at an offset.
    #[action(destructive)]
    pub fn delete_bytes(
        &mut self,
        #[param("Byte offset, from 0")] offset: usize,
        #[param("How many bytes")] len: usize,
    ) -> Result<(), Refusal> {
        self.bytes.edit(&BufferEdit::Delete { offset, len })
    }
}

impl HexEditor {
    /// The byte edits bound to where the person is: insert or overwrite at
    /// the cursor, delete the selection or the byte under the cursor.
    pub(crate) fn at_cursor(
        cursor: usize,
        len: usize,
        selection: Option<(usize, usize)>,
    ) -> Vec<ui::tilo_resource::ActionDecl<Self>> {
        // The append position is valid for an insert even in an empty buffer.
        let at = cursor.min(len);
        let mut bound = vec![
            Self::INSERT_BYTES
                .bind(hex_editor::InsertBytes::paths().offset, at)
                .declaration("insert-byte", "Insert at cursor"),
        ];
        if cursor < len {
            bound.push(
                Self::OVERWRITE_BYTES
                    .bind(hex_editor::OverwriteBytes::paths().offset, cursor)
                    .declaration("overwrite-byte", "Overwrite at cursor"),
            );
        }
        let delete = |offset: usize, len: usize| {
            Self::DELETE_BYTES
                .bind(hex_editor::DeleteBytes::paths().offset, offset)
                .and(hex_editor::DeleteBytes::paths().len, len)
        };
        if let Some((lo, hi)) = selection {
            bound.push(delete(lo, hi - lo + 1).declaration("delete-selection", "Delete selection"));
        } else if cursor < len {
            bound.push(delete(cursor, 1).declaration("delete-byte", "Delete byte"));
        }
        bound.into_iter().flatten().collect()
    }

    /// The load of `name`'s `bytes`, as an invocation.
    pub(crate) fn loading(name: impl Into<String>, bytes: Vec<u8>) -> Invoke<Self> {
        Self::LOAD.with(hex_editor::Load {
            name: name.into(),
            bytes: Hex(bytes),
        })
    }
}

/// The hex editor app: one buffer per file, edited through [`HexView`].
pub struct HexEditorShell;

/// The bundled sample a capture shows.
const SAMPLE_NAME: &str = "sample.png";

impl ResourceApp for HexEditorShell {
    type Resource = HexEditor;
    const ID: &'static str = "so.tilo.hex-editor";
    const TITLE: &'static str = "Hex Editor";
    const ICON: Option<&'static str> = Some("hex-editor");
    const OPENS: &'static [&'static str] = &[ui::application::ANY_MEDIA_TYPE];
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<HexEditor>) -> Element {
        let path = cx.launch_arguments().first().map(std::path::PathBuf::from);
        // In a container, the surface is a widget of the view's tree that
        // tests and headless drives address by name.
        Box::new(ui::widget::Stack::new().push(cx.surface(HexView::empty().with_path(path))))
    }

    fn label(state: &HexEditor) -> String {
        state.name().map_or_else(
            || <HexEditor as tilo_resource::Resource>::LABEL.to_owned(),
            str::to_owned,
        )
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// The buffer of a file is a record of its own, derived from the
    /// file's, so another app's record of the same file is left alone.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input
            .argument()
            .map(|path| RecordId::derived("hex", &RecordId::identify_file(path)))
    }

    /// The file is read afresh on every launch: what is on disk is what the
    /// buffer shows. Unchanged bytes change nothing.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<HexEditor>> {
        input
            .file()
            .map(|file| HexEditor::loading(file.name(), file.bytes().to_vec()))
            .into_iter()
            .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "empty",
            "find",
            "goto",
            "edited",
            "error",
            "narrow",
            "narrow-inspector",
        ]
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        scenario
            .and_then(|name| name.starts_with("narrow").then_some((640, 720)))
            .unwrap_or((1280, 800))
    }

    /// Every capture but the empty state shows the bundled sample.
    fn scenario(name: Option<&str>) -> Vec<Invoke<HexEditor>> {
        if name == Some("empty") {
            return Vec::new();
        }
        vec![HexEditor::loading(SAMPLE_NAME, crate::sample::png_image())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{MAX_PAGE, PAGE};
    use automerge::{ROOT, ReadDoc as _};
    use ui::tilo_resource::Replica;

    fn load(replica: &mut Replica<HexEditor>, bytes: &[u8]) {
        replica
            .apply(HexEditor::loading("buf.bin", bytes.to_vec()))
            .unwrap();
    }

    /// Make `edit` at the bytes' path, as a keystroke does.
    fn edit(replica: &mut Replica<HexEditor>, edit: &BufferEdit) -> Result<(), String> {
        let edit = HexEditor::paths().bytes.perform(edit).unwrap();
        replica
            .apply(Invoke::edit(&edit).unwrap())
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn insert(offset: usize, bytes: &[u8]) -> BufferEdit {
        BufferEdit::Insert {
            offset,
            bytes: Hex(bytes.to_vec()),
        }
    }

    fn overwrite(offset: usize, bytes: &[u8]) -> BufferEdit {
        BufferEdit::Overwrite {
            offset,
            bytes: Hex(bytes.to_vec()),
        }
    }

    fn state(replica: &Replica<HexEditor>) -> HexEditor {
        replica.state().unwrap()
    }

    /// The stored pages' lengths, read from the document.
    fn pages(replica: &Replica<HexEditor>) -> Vec<usize> {
        let doc = replica.doc();
        let Some((_, list)) = doc.get(ROOT, "bytes").unwrap() else {
            return Vec::new();
        };
        (0..doc.length(&list))
            .map(|index| match doc.get(&list, index).unwrap() {
                Some((automerge::Value::Scalar(value), _)) => match value.as_ref() {
                    automerge::ScalarValue::Bytes(page) => page.len(),
                    other => panic!("a page is bytes, not {other:?}"),
                },
                other => panic!("a page is bytes, not {other:?}"),
            })
            .collect()
    }

    fn ops_since(replica: &mut Replica<HexEditor>, heads: &[automerge::ChangeHash]) -> usize {
        let changes = replica.doc_mut().get_changes(heads);
        changes.iter().map(automerge::Change::len).sum()
    }

    #[test]
    fn edits_change_the_bytes_and_rewrite_only_the_pages_they_touch() {
        let file: Vec<u8> = (0..3 * PAGE + 10).map(|i| (i % 251) as u8).collect();
        let mut replica = Replica::<HexEditor>::new();
        load(&mut replica, &file);
        assert_eq!(state(&replica).bytes, file);
        assert_eq!(state(&replica).name(), Some("buf.bin"));
        assert_eq!(state(&replica).opened, 1);
        assert_eq!(pages(&replica), [PAGE, PAGE, PAGE, 10]);

        let mut expected = file;
        edit(&mut replica, &overwrite(PAGE - 1, &[0xAA, 0xBB])).unwrap();
        expected[PAGE - 1] = 0xAA;
        expected[PAGE] = 0xBB;
        assert_eq!(state(&replica).bytes, expected);

        let before = replica.heads();
        edit(&mut replica, &insert(5, &[1, 2, 3])).unwrap();
        expected.splice(5..5, [1, 2, 3]);
        assert_eq!(state(&replica).bytes, expected);
        assert_eq!(pages(&replica), [PAGE + 3, PAGE, PAGE, 10]);
        assert_eq!(
            ops_since(&mut replica, &before),
            1,
            "an insert rewrites the one page it lands in"
        );

        edit(
            &mut replica,
            &BufferEdit::Delete {
                offset: PAGE,
                len: PAGE + 10,
            },
        )
        .unwrap();
        expected.drain(PAGE..2 * PAGE + 10);
        assert_eq!(state(&replica).bytes, expected);

        // Appending at the end grows the last page; a long page splits.
        let end = expected.len();
        let tail = vec![7; MAX_PAGE];
        edit(&mut replica, &insert(end, &tail)).unwrap();
        expected.extend(&tail);
        assert_eq!(state(&replica).bytes, expected);
        assert!(
            pages(&replica).iter().all(|len| *len <= MAX_PAGE),
            "{:?}",
            pages(&replica)
        );
    }

    #[test]
    fn verbs_make_the_same_edits_and_write_only_the_pages_they_change() {
        let file: Vec<u8> = (0..3 * PAGE).map(|i| (i % 251) as u8).collect();
        let mut replica = Replica::<HexEditor>::new();
        load(&mut replica, &file);
        let before = replica.heads();
        replica
            .apply(HexEditor::OVERWRITE_BYTES.with(hex_editor::OverwriteBytes {
                offset: PAGE + 1,
                bytes: Hex(vec![0xEE]),
            }))
            .unwrap();
        assert_eq!(
            ops_since(&mut replica, &before),
            1,
            "a verb that changes one byte writes one page"
        );
        replica
            .apply(HexEditor::INSERT_BYTES.with(hex_editor::InsertBytes {
                offset: 0,
                bytes: Hex(vec![1, 2]),
            }))
            .unwrap();
        let mut expected = file;
        expected[PAGE + 1] = 0xEE;
        expected.splice(0..0, [1, 2]);
        assert_eq!(state(&replica).bytes, expected);
        assert_eq!(pages(&replica), [PAGE + 2, PAGE, PAGE]);
        assert!(
            replica
                .apply_confirmed(HexEditor::DELETE_BYTES.with(hex_editor::DeleteBytes {
                    offset: expected.len(),
                    len: 1,
                }))
                .is_err(),
            "an offset past the end is refused"
        );
    }

    #[test]
    fn offsets_beyond_the_buffer_are_refused_and_empty_edits_change_nothing() {
        let mut replica = Replica::<HexEditor>::new();
        load(&mut replica, &[1, 2, 3]);
        for action in [
            insert(4, &[9]),
            overwrite(3, &[9]),
            BufferEdit::Delete { offset: 3, len: 1 },
        ] {
            assert!(edit(&mut replica, &action).is_err(), "{action:?}");
        }
        // Overwriting across the end keeps the length; deleting past it
        // stops at the end; appending at the end is allowed.
        edit(&mut replica, &overwrite(2, &[8, 9])).unwrap();
        assert_eq!(state(&replica).bytes, [1, 2, 8]);
        edit(&mut replica, &BufferEdit::Delete { offset: 1, len: 9 }).unwrap();
        assert_eq!(state(&replica).bytes, [1]);
        edit(&mut replica, &insert(1, &[5])).unwrap();
        assert_eq!(state(&replica).bytes, [1, 5]);

        let heads = replica.heads();
        edit(&mut replica, &BufferEdit::Delete { offset: 0, len: 0 }).unwrap();
        load(&mut replica, &[1, 5]);
        assert_eq!(
            replica.heads(),
            heads,
            "reloading unchanged bytes and an empty delete write nothing"
        );
        assert_eq!(state(&replica).opened, 1);
    }

    #[test]
    fn an_empty_record_has_no_file_and_typing_starts_its_first_page() {
        let mut replica = Replica::<HexEditor>::new();
        assert!(!state(&replica).has_file());
        load(&mut replica, &[]);
        assert!(state(&replica).has_file());
        assert!(state(&replica).bytes.is_empty());
        edit(&mut replica, &insert(0, &[0xAB])).unwrap();
        assert_eq!(state(&replica).bytes, [0xAB]);
    }

    #[test]
    fn collaborators_editing_different_pages_merge() {
        let file = vec![0u8; 2 * PAGE];
        let mut ours = Replica::<HexEditor>::new();
        load(&mut ours, &file);
        let mut theirs = ours.fork();
        edit(&mut ours, &overwrite(1, &[1])).unwrap();
        edit(&mut theirs, &insert(PAGE + 1, &[2])).unwrap();
        ours.sync_with(&mut theirs).unwrap();
        let bytes = state(&ours).bytes.to_vec();
        assert_eq!(bytes.len(), 2 * PAGE + 1);
        assert_eq!(bytes[1], 1);
        assert_eq!(bytes[PAGE + 1], 2);
        assert_eq!(state(&theirs).bytes, bytes);
    }

    /// A reader kept up to date page by page shows what a fresh read shows.
    #[test]
    fn a_refreshed_state_matches_a_fresh_read() {
        let file: Vec<u8> = (0..3 * PAGE).map(|i| (i % 7) as u8).collect();
        let mut replica = Replica::<HexEditor>::new();
        load(&mut replica, &file);
        let mut reader = ui::tilo_resource::Reader::<HexEditor>::read(replica.doc()).unwrap();
        edit(&mut replica, &insert(PAGE + 3, &vec![4; MAX_PAGE])).unwrap();
        edit(&mut replica, &BufferEdit::Delete { offset: 2, len: 3 }).unwrap();
        assert!(reader.sync(replica.doc()).unwrap());
        assert!(
            !reader.last_delta().is_everything(),
            "the change is read in part"
        );
        assert_eq!(reader.state().bytes, state(&replica).bytes);
    }
}
