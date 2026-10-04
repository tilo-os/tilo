//! The hex buffer as a [`Resource`]: a file's bytes, kept in the record so
//! they sync, undo and show in history like any document.
//!
//! The bytes are stored as a list of pages of a few KiB each. An edit
//! rewrites only the pages it touches: typing over one byte of a large file
//! writes one page, an insert grows one page (split when it gets long), and
//! collaborators editing different pages merge cleanly. Two people editing
//! the same page concurrently keep one of their versions of it.

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ObjType, ROOT, ReadDoc as _, ScalarValue, Value};
use ui::framework::{LaunchInput, ResourceApp, ViewCx};
use ui::record::RecordId;
use ui::tilo_actions::{ActionError, ActionSpec};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{HEX_EDITOR_ACTION_TYPE, HexEditorAction};
use crate::model::common_suffix;
use crate::view::HexView;

/// The bytes a page holds when a file is opened or a long page is split.
const PAGE: usize = 4096;
/// The longest a page grows by inserts before it is split.
const MAX_PAGE: usize = 2 * PAGE;

/// The open buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HexState {
    /// The opened file's name; `None` until a file is opened.
    pub name: Option<String>,
    pub bytes: Vec<u8>,
    /// How many times a file was opened into this record: a view takes the
    /// bytes after a change of it as freshly loaded, not as edits.
    pub opened: u64,
}

impl HexState {
    pub fn has_file(&self) -> bool {
        self.name.is_some()
    }
}

/// The hex buffer resource.
pub struct HexEditor;

impl Resource for HexEditor {
    const KIND: &'static str = "hex_editor.document";
    const LABEL: &'static str = "Hex document";
    const ACTION_TYPE: &'static str = HEX_EDITOR_ACTION_TYPE;

    type State = HexState;
    type Action = HexEditorAction;

    fn read(doc: &Doc) -> Result<HexState, ModelError> {
        let name = match doc.get(ROOT, "name")? {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Str(name) => Some(name.to_string()),
                _ => return Err(ModelError::shape("text").at("name")),
            },
            Some(_) => return Err(ModelError::shape("text").at("name")),
            None => None,
        };
        let opened = match doc.get(ROOT, "opened")? {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Uint(count) => *count,
                _ => return Err(ModelError::shape("count").at("opened")),
            },
            _ => 0,
        };
        let bytes = read_pages(doc)?.concat();
        Ok(HexState {
            name,
            bytes,
            opened,
        })
    }

    fn coalesces(previous: &HexEditorAction, next: &HexEditorAction) -> bool {
        previous.continues(next)
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        crate::actions::declarations()
    }
}

/// The pages list, when the record holds one.
fn pages_list(doc: &Doc) -> Result<Option<ObjId>, ModelError> {
    match doc.get(ROOT, "pages")? {
        Some((Value::Object(ObjType::List), id)) => Ok(Some(id)),
        Some(_) => Err(ModelError::shape("list").at("pages")),
        None => Ok(None),
    }
}

fn read_pages(doc: &Doc) -> Result<Vec<Vec<u8>>, ModelError> {
    let Some(list) = pages_list(doc)? else {
        return Ok(Vec::new());
    };
    (0..doc.length(&list))
        .map(|index| match doc.get(&list, index)? {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(page) => Ok(page.clone()),
                _ => Err(ModelError::shape("bytes").at("pages")),
            },
            _ => Err(ModelError::shape("bytes").at("pages")),
        })
        .collect()
}

/// Write `new` over the pages `old` read from the document, touching only
/// the pages that differ.
fn write_pages(doc: &mut Doc, old: &[Vec<u8>], new: &[Vec<u8>]) -> Result<(), ActionError> {
    let list = match pages_list(doc).map_err(refused)? {
        Some(list) => list,
        None => doc.put_object(ROOT, "pages", ObjType::List)?,
    };
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let old_rest = old.get(prefix..).unwrap_or_default();
    let new_rest = new.get(prefix..).unwrap_or_default();
    let suffix = common_suffix(old_rest, new_rest);
    let old_middle = old_rest.get(..old_rest.len() - suffix).unwrap_or_default();
    let new_middle = new_rest.get(..new_rest.len() - suffix).unwrap_or_default();
    for (index, (was, page)) in old_middle.iter().zip(new_middle).enumerate() {
        if was != page {
            doc.put(&list, prefix + index, ScalarValue::Bytes(page.clone()))?;
        }
    }
    let common = old_middle.len().min(new_middle.len());
    for _ in common..old_middle.len() {
        doc.delete(&list, prefix + common)?;
    }
    for (index, page) in new_middle.iter().enumerate().skip(common) {
        doc.insert(&list, prefix + index, ScalarValue::Bytes(page.clone()))?;
    }
    Ok(())
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the shape `map_err` hands an error to"
)]
fn refused(error: ModelError) -> ActionError {
    ActionError::Refused(error.to_string())
}

fn paginate(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes.chunks(PAGE).map(<[u8]>::to_vec).collect()
}

/// The page holding `offset` and where in it; the end of the buffer is the
/// end of its last page.
fn locate(pages: &[Vec<u8>], offset: usize) -> Option<(usize, usize)> {
    let mut start = 0;
    for (index, page) in pages.iter().enumerate() {
        if offset < start + page.len() {
            return Some((index, offset - start));
        }
        start += page.len();
    }
    let (last, page) = pages.iter().enumerate().next_back()?;
    (offset == start).then_some((last, page.len()))
}

/// Apply `action` to the record.
pub(crate) fn apply(action: &HexEditorAction, doc: &mut Doc) -> Result<(), ActionError> {
    let old = read_pages(doc).map_err(refused)?;
    let len: usize = old.iter().map(Vec::len).sum();
    let mut pages = old.clone();
    match action {
        HexEditorAction::Open { name, bytes } => {
            let state = HexEditor::read(doc).map_err(refused)?;
            if state.name.as_deref() == Some(name.as_str()) && state.bytes == *bytes {
                return Ok(());
            }
            if state.name.as_deref() != Some(name.as_str()) {
                doc.put(ROOT, "name", name.as_str())?;
            }
            doc.put(
                ROOT,
                "opened",
                ScalarValue::Uint(state.opened.saturating_add(1)),
            )?;
            pages = paginate(bytes);
        }
        HexEditorAction::InsertBytes { offset, bytes } => {
            if *offset > len {
                return Err(beyond(*offset, len));
            }
            if bytes.is_empty() {
                return Ok(());
            }
            match locate(&pages, *offset) {
                Some((index, at)) => {
                    let Some(page) = pages.get_mut(index) else {
                        return Err(beyond(*offset, len));
                    };
                    page.splice(at..at, bytes.iter().copied());
                    if page.len() > MAX_PAGE {
                        let split = paginate(page);
                        pages.splice(index..=index, split);
                    }
                }
                None => pages = paginate(bytes),
            }
        }
        HexEditorAction::DeleteBytes { offset, len: count } => {
            if *count == 0 {
                return Ok(());
            }
            if *offset >= len {
                return Err(beyond(*offset, len));
            }
            let end = offset.saturating_add(*count).min(len);
            let mut start = 0;
            for page in &mut pages {
                let (from, to) = (start, start + page.len());
                start = to;
                let cut_from = (*offset).max(from);
                let cut_to = end.min(to);
                if cut_from < cut_to {
                    page.drain(cut_from - from..cut_to - from);
                }
            }
            pages.retain(|page| !page.is_empty());
        }
        HexEditorAction::OverwriteBytes { offset, bytes } => {
            if bytes.is_empty() {
                return Ok(());
            }
            if *offset >= len {
                return Err(beyond(*offset, len));
            }
            let end = offset.saturating_add(bytes.len());
            let mut start = 0;
            for page in &mut pages {
                let page_start = start;
                start += page.len();
                if start <= *offset || page_start >= end {
                    continue;
                }
                for (index, slot) in page.iter_mut().enumerate() {
                    if let Some(byte) = (page_start + index)
                        .checked_sub(*offset)
                        .and_then(|at| bytes.get(at))
                    {
                        *slot = *byte;
                    }
                }
            }
        }
    }
    write_pages(doc, &old, &pages)
}

fn beyond(offset: usize, len: usize) -> ActionError {
    ActionError::Refused(format!(
        "offset {offset} is beyond the end of the {len}-byte buffer"
    ))
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

    fn label(state: &HexState) -> String {
        state
            .name
            .clone()
            .unwrap_or_else(|| HexEditor::LABEL.to_owned())
    }

    fn local_operations() -> Vec<ActionSpec> {
        crate::actions::view_operations()
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
            .and_then(|file| {
                Invoke::apply(&HexEditorAction::Open {
                    name: file.name().to_owned(),
                    bytes: file.bytes().to_vec(),
                })
                .ok()
            })
            .into_iter()
            .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        &["empty", "search", "error"]
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1100, 720)
    }

    /// Every capture but the empty state shows the bundled sample.
    fn scenario(name: Option<&str>) -> Vec<Invoke<HexEditor>> {
        if name == Some("empty") {
            return Vec::new();
        }
        Invoke::apply(&HexEditorAction::Open {
            name: SAMPLE_NAME.into(),
            bytes: crate::sample::png_header(),
        })
        .into_iter()
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    fn open(bytes: &[u8]) -> HexEditorAction {
        HexEditorAction::Open {
            name: "buf.bin".into(),
            bytes: bytes.to_vec(),
        }
    }

    fn apply(
        replica: &mut Replica<HexEditor>,
        action: &HexEditorAction,
    ) -> Result<(), ActionError> {
        replica.apply_action(action, "edit").map(|_| ())
    }

    fn state(replica: &Replica<HexEditor>) -> HexState {
        replica.state().unwrap()
    }

    fn pages(replica: &Replica<HexEditor>) -> Vec<usize> {
        read_pages(replica.doc())
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect()
    }

    #[test]
    fn edits_change_the_bytes_and_rewrite_only_the_pages_they_touch() {
        let file: Vec<u8> = (0..3 * PAGE + 10).map(|i| (i % 251) as u8).collect();
        let mut replica = Replica::<HexEditor>::new();
        apply(&mut replica, &open(&file)).unwrap();
        assert_eq!(state(&replica).bytes, file);
        assert_eq!(state(&replica).name.as_deref(), Some("buf.bin"));
        assert_eq!(state(&replica).opened, 1);
        assert_eq!(pages(&replica), [PAGE, PAGE, PAGE, 10]);

        let mut expected = file;
        apply(
            &mut replica,
            &HexEditorAction::OverwriteBytes {
                offset: PAGE - 1,
                bytes: vec![0xAA, 0xBB],
            },
        )
        .unwrap();
        expected[PAGE - 1] = 0xAA;
        expected[PAGE] = 0xBB;
        assert_eq!(state(&replica).bytes, expected);

        let before = replica.heads();
        apply(
            &mut replica,
            &HexEditorAction::InsertBytes {
                offset: 5,
                bytes: vec![1, 2, 3],
            },
        )
        .unwrap();
        expected.splice(5..5, [1, 2, 3]);
        assert_eq!(state(&replica).bytes, expected);
        assert_eq!(pages(&replica), [PAGE + 3, PAGE, PAGE, 10]);
        let changes = replica.doc_mut().get_changes(&before);
        let ops: usize = changes.iter().map(automerge::Change::len).sum();
        assert_eq!(ops, 1, "an insert rewrites the one page it lands in");

        apply(
            &mut replica,
            &HexEditorAction::DeleteBytes {
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
        apply(
            &mut replica,
            &HexEditorAction::InsertBytes {
                offset: end,
                bytes: tail.clone(),
            },
        )
        .unwrap();
        expected.extend(&tail);
        assert_eq!(state(&replica).bytes, expected);
        assert!(
            pages(&replica).iter().all(|len| *len <= MAX_PAGE),
            "{:?}",
            pages(&replica)
        );
    }

    #[test]
    fn offsets_beyond_the_buffer_are_refused_and_empty_edits_change_nothing() {
        let mut replica = Replica::<HexEditor>::new();
        apply(&mut replica, &open(&[1, 2, 3])).unwrap();
        for action in [
            HexEditorAction::InsertBytes {
                offset: 4,
                bytes: vec![9],
            },
            HexEditorAction::OverwriteBytes {
                offset: 3,
                bytes: vec![9],
            },
            HexEditorAction::DeleteBytes { offset: 3, len: 1 },
        ] {
            assert!(apply(&mut replica, &action).is_err(), "{action:?}");
        }
        // Overwriting across the end keeps the length; deleting past it
        // stops at the end; appending at the end is allowed.
        apply(
            &mut replica,
            &HexEditorAction::OverwriteBytes {
                offset: 2,
                bytes: vec![8, 9],
            },
        )
        .unwrap();
        assert_eq!(state(&replica).bytes, [1, 2, 8]);
        apply(
            &mut replica,
            &HexEditorAction::DeleteBytes { offset: 1, len: 9 },
        )
        .unwrap();
        assert_eq!(state(&replica).bytes, [1]);
        apply(
            &mut replica,
            &HexEditorAction::InsertBytes {
                offset: 1,
                bytes: vec![5],
            },
        )
        .unwrap();
        assert_eq!(state(&replica).bytes, [1, 5]);

        let heads = replica.heads();
        apply(
            &mut replica,
            &HexEditorAction::DeleteBytes { offset: 0, len: 0 },
        )
        .unwrap();
        apply(&mut replica, &open(&[1, 5])).unwrap();
        assert_eq!(
            replica.heads(),
            heads,
            "reopening unchanged bytes and an empty delete write nothing"
        );
    }

    #[test]
    fn an_empty_record_has_no_file_and_typing_starts_its_first_page() {
        let mut replica = Replica::<HexEditor>::new();
        assert!(!state(&replica).has_file());
        apply(&mut replica, &open(&[])).unwrap();
        assert!(state(&replica).has_file());
        assert!(state(&replica).bytes.is_empty());
        apply(
            &mut replica,
            &HexEditorAction::InsertBytes {
                offset: 0,
                bytes: vec![0xAB],
            },
        )
        .unwrap();
        assert_eq!(state(&replica).bytes, [0xAB]);
    }

    #[test]
    fn collaborators_editing_different_pages_merge() {
        let file = vec![0u8; 2 * PAGE];
        let mut ours = Replica::<HexEditor>::new();
        apply(&mut ours, &open(&file)).unwrap();
        let mut theirs = ours.fork();
        apply(
            &mut ours,
            &HexEditorAction::OverwriteBytes {
                offset: 1,
                bytes: vec![1],
            },
        )
        .unwrap();
        apply(
            &mut theirs,
            &HexEditorAction::InsertBytes {
                offset: PAGE + 1,
                bytes: vec![2],
            },
        )
        .unwrap();
        ours.sync_with(&mut theirs).unwrap();
        let bytes = state(&ours).bytes;
        assert_eq!(bytes.len(), 2 * PAGE + 1);
        assert_eq!(bytes[1], 1);
        assert_eq!(bytes[PAGE + 1], 2);
        assert_eq!(state(&theirs).bytes, bytes);
    }
}
