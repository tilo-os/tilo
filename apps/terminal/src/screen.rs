//! The screen a terminal session shows: its main buffer with scrollback, the
//! alternate buffer full-screen programs draw on, the cursor, the OSC 8
//! hyperlinks and the image placements.
//!
//! The emulator keeps one [`Screen`] for the process it runs; the session
//! record keeps another, which every collaborator and every past version
//! reads. After the emulator has parsed a batch of output, the difference
//! between the two is one list of [`ScreenEdit`]s ([`edits`]), which the
//! session applies as one action. Lines are shared ([`Rc`]): a line the
//! emulator did not touch is the same allocation on both screens, which is
//! how the difference is found without comparing text.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{Delta, Doc, Touch};

use crate::graphics::TerminalImagePlacement;

pub(crate) const GRID: &str = "terminal_grid";
pub(crate) const ALT_GRID: &str = "terminal_grid_alt";
pub(crate) const CURSOR: &str = "cursor";
pub(crate) const LINKS: &str = "terminal_links";
pub(crate) const IMAGES: &str = "terminal_images";
pub(crate) const ALTERNATE: &str = "terminal_alternate";

/// The most lines one buffer may hold: retained scrollback, its trim slack
/// and a generous screen.
const MAX_LINES: usize = 16 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_LINKS: usize = 4096;
const MAX_LINK_BYTES: usize = 2 * 1024;
const MAX_IMAGES: usize = 4096;

/// One row of a buffer: its text, one character per cell (a wide glyph is
/// followed by a padding space), and the packed attributes of each cell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub attrs: Vec<u8>,
}

/// What a terminal shows. See the [module docs](self).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Screen {
    /// The main buffer, scrollback first: its last rows are the screen.
    pub main: Vec<Rc<Line>>,
    /// The alternate buffer, exactly one screen tall.
    pub alt: Vec<Rc<Line>>,
    /// Whether a full-screen program shows the alternate buffer.
    pub alternate: bool,
    /// The cursor: its column, and its row counted from the top of the
    /// active buffer (scrollback included).
    pub cursor: (u64, u64),
    /// OSC 8 hyperlink targets by the id their cells carry.
    pub links: BTreeMap<u16, String>,
    /// Graphics placed on the grid; their bytes are in the content store.
    pub images: Vec<TerminalImagePlacement>,
}

impl Screen {
    /// The lines of one buffer.
    pub fn lines(&self, alternate: bool) -> &Vec<Rc<Line>> {
        if alternate { &self.alt } else { &self.main }
    }

    pub(crate) fn lines_mut(&mut self, alternate: bool) -> &mut Vec<Rc<Line>> {
        if alternate {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    /// The lines of the buffer on screen.
    pub fn active(&self) -> &Vec<Rc<Line>> {
        self.lines(self.alternate)
    }
}

/// One change to the screen a session record keeps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScreenEdit {
    /// Drop both buffers, the cursor, the links and the images.
    Clear,
    /// Insert a line at `index`.
    Insert {
        alternate: bool,
        index: u32,
        text: String,
        attrs: Vec<u8>,
    },
    /// Replace the line at `index`.
    Write {
        alternate: bool,
        index: u32,
        text: String,
        attrs: Vec<u8>,
    },
    /// Drop `count` lines from `index` on: scrollback trimmed from the
    /// front, a scrolling region moving up.
    Delete {
        alternate: bool,
        index: u32,
        count: u32,
    },
    /// Move the cursor.
    Cursor { x: u64, y: u64 },
    /// Show the alternate buffer, or the main one.
    Alternate(bool),
    /// Register one OSC 8 hyperlink.
    Link { id: u16, url: String },
    /// Replace the image placements.
    Images(Vec<TerminalImagePlacement>),
}

/// The edits that turn `before` (`None`: whatever the record holds) into
/// `after`. Lines are matched by allocation: a line `after` shares with
/// `before` is unchanged, and lines between two unchanged ones were
/// rewritten in place, inserted or dropped.
pub fn edits(before: Option<&Screen>, after: &Screen) -> Vec<ScreenEdit> {
    let empty = Screen::default();
    let mut out = Vec::new();
    let before = before.unwrap_or_else(|| {
        out.push(ScreenEdit::Clear);
        &empty
    });
    let reset = !out.is_empty();
    for alternate in [false, true] {
        line_edits(
            before.lines(alternate),
            after.lines(alternate),
            alternate,
            &mut out,
        );
    }
    if reset || before.cursor != after.cursor {
        out.push(ScreenEdit::Cursor {
            x: after.cursor.0,
            y: after.cursor.1,
        });
    }
    if reset || before.alternate != after.alternate {
        out.push(ScreenEdit::Alternate(after.alternate));
    }
    for (id, url) in &after.links {
        if before.links.get(id) != Some(url) {
            out.push(ScreenEdit::Link {
                id: *id,
                url: url.clone(),
            });
        }
    }
    if before.images != after.images {
        out.push(ScreenEdit::Images(after.images.clone()));
    }
    out
}

fn line_edits(before: &[Rc<Line>], after: &[Rc<Line>], alternate: bool, out: &mut Vec<ScreenEdit>) {
    let kept: HashMap<*const Line, usize> = before
        .iter()
        .enumerate()
        .map(|(index, line)| (Rc::as_ptr(line), index))
        .collect();
    // Where the next edit lands in the record's buffer, and the first line
    // of `before` not yet accounted for.
    let mut at = 0usize;
    let mut next_old = 0usize;
    let mut pending: Vec<&Rc<Line>> = Vec::new();
    for line in after {
        match kept.get(&Rc::as_ptr(line)) {
            Some(&old) if old >= next_old => {
                at = segment(old - next_old, &pending, at, alternate, out);
                pending.clear();
                at += 1;
                next_old = old + 1;
            }
            _ => pending.push(line),
        }
    }
    segment(before.len() - next_old, &pending, at, alternate, out);
}

/// The edits for one stretch between unchanged lines: `old` lines of the
/// record become `new`. Returns where the record's buffer continues.
fn segment(
    old: usize,
    new: &[&Rc<Line>],
    mut at: usize,
    alternate: bool,
    out: &mut Vec<ScreenEdit>,
) -> usize {
    let rewritten = old.min(new.len());
    let index = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
    for line in new.iter().take(rewritten) {
        out.push(ScreenEdit::Write {
            alternate,
            index: index(at),
            text: line.text.clone(),
            attrs: line.attrs.clone(),
        });
        at += 1;
    }
    if old > rewritten {
        out.push(ScreenEdit::Delete {
            alternate,
            index: index(at),
            count: index(old - rewritten),
        });
    }
    for line in new.iter().skip(rewritten) {
        out.push(ScreenEdit::Insert {
            alternate,
            index: index(at),
            text: line.text.clone(),
            attrs: line.attrs.clone(),
        });
        at += 1;
    }
    at
}

fn grid_key(alternate: bool) -> &'static str {
    if alternate { ALT_GRID } else { GRID }
}

fn object_at(
    doc: &mut Doc,
    parent: &ObjId,
    key: &str,
    kind: ObjType,
) -> Result<ObjId, ActionError> {
    match doc.get(parent, key)? {
        Some((Value::Object(found), existing)) if found == kind => Ok(existing),
        _ => Ok(doc.put_object(parent, key, kind)?),
    }
}

fn refuse(reason: &str) -> ActionError {
    ActionError::Refused(reason.into())
}

/// Apply `edits` to the screen stored under `parent`.
///
/// # Errors
/// Refuses an edit that names a line the buffer does not have or carries
/// more than a screen may hold.
pub(crate) fn apply(
    doc: &mut Doc,
    parent: &ObjId,
    edits: &[ScreenEdit],
) -> Result<(), ActionError> {
    for edit in edits {
        apply_one(doc, parent, edit)?;
    }
    Ok(())
}

fn apply_one(doc: &mut Doc, parent: &ObjId, edit: &ScreenEdit) -> Result<(), ActionError> {
    match edit {
        ScreenEdit::Clear => clear(doc, parent)?,
        ScreenEdit::Insert {
            alternate,
            index,
            text,
            attrs,
        } => {
            if text.len() > MAX_LINE_BYTES || attrs.len() > MAX_LINE_BYTES {
                return Err(refuse("grid line is too large"));
            }
            let grid = object_at(doc, parent, grid_key(*alternate), ObjType::List)?;
            let at = *index as usize;
            let len = doc.length(&grid);
            if at > len {
                return Err(refuse("grid line is missing"));
            }
            if len >= MAX_LINES {
                return Err(refuse("grid is too tall"));
            }
            let line = doc.insert_object(&grid, at, ObjType::Map)?;
            let text_id = doc.put_object(&line, "text", ObjType::Text)?;
            doc.splice_text(&text_id, 0, 0, text)?;
            doc.put(&line, "attrs", attrs.clone())?;
        }
        ScreenEdit::Write {
            alternate,
            index,
            text,
            attrs,
        } => {
            if text.len() > MAX_LINE_BYTES || attrs.len() > MAX_LINE_BYTES {
                return Err(refuse("grid line is too large"));
            }
            let grid = object_at(doc, parent, grid_key(*alternate), ObjType::List)?;
            let Some((Value::Object(ObjType::Map), line)) = doc.get(&grid, *index as usize)? else {
                return Err(refuse("grid line is missing"));
            };
            let text_id = match doc.get(&line, "text")? {
                Some((Value::Object(ObjType::Text), id)) => id,
                _ => doc.put_object(&line, "text", ObjType::Text)?,
            };
            ui::doc::splice_text_diffed(doc, &text_id, text);
            doc.put(&line, "attrs", attrs.clone())?;
        }
        ScreenEdit::Delete {
            alternate,
            index,
            count,
        } => {
            let grid = object_at(doc, parent, grid_key(*alternate), ObjType::List)?;
            let at = *index as usize;
            let count = *count as usize;
            if at.saturating_add(count) > doc.length(&grid) {
                return Err(refuse("grid line is missing"));
            }
            for _ in 0..count {
                doc.delete(&grid, at)?;
            }
        }
        ScreenEdit::Cursor { x, y } => {
            let cursor = object_at(doc, parent, CURSOR, ObjType::Map)?;
            doc.put(&cursor, "x", *x)?;
            doc.put(&cursor, "y", *y)?;
        }
        ScreenEdit::Alternate(alternate) => doc.put(parent, ALTERNATE, *alternate)?,
        ScreenEdit::Link { id, url } => {
            if *id == 0 || url.is_empty() || url.len() > MAX_LINK_BYTES {
                return Err(refuse("hyperlink is invalid"));
            }
            let links = object_at(doc, parent, LINKS, ObjType::Map)?;
            let key = id.to_string();
            if doc.get(&links, &key)?.is_none() && doc.length(&links) >= MAX_LINKS {
                return Err(refuse("hyperlink table is full"));
            }
            doc.put(&links, key, url.as_str())?;
        }
        ScreenEdit::Images(images) => write_images(doc, parent, images)?,
    }
    Ok(())
}

fn write_images(
    doc: &mut Doc,
    parent: &ObjId,
    images: &[TerminalImagePlacement],
) -> Result<(), ActionError> {
    if images.len() > MAX_IMAGES || !images.iter().all(placement_is_valid) {
        return Err(refuse("image placement is invalid"));
    }
    let list = doc.put_object(parent, IMAGES, ObjType::List)?;
    for (index, image) in images.iter().enumerate() {
        let placement = doc.insert_object(&list, index, ObjType::Map)?;
        doc.put(&placement, "hash", image.hash.as_str())?;
        doc.put(&placement, "mime", image.mime.as_str())?;
        doc.put(&placement, "byte_len", image.byte_len)?;
        doc.put(&placement, "row", image.row as u64)?;
        doc.put(&placement, "col", image.col as u64)?;
        doc.put(&placement, "columns", u64::from(image.columns))?;
        doc.put(&placement, "rows", u64::from(image.rows))?;
        doc.put(&placement, "pixel_width", u64::from(image.pixel_width))?;
        doc.put(&placement, "pixel_height", u64::from(image.pixel_height))?;
        doc.put(&placement, "alternate_screen", image.alternate_screen)?;
        if let Some(protocol_id) = image.protocol_id {
            doc.put(&placement, "protocol_id", u64::from(protocol_id))?;
        }
    }
    Ok(())
}

fn placement_is_valid(image: &TerminalImagePlacement) -> bool {
    !image.hash.is_empty()
        && image.hash.len() <= 128
        && !image.mime.is_empty()
        && image.mime.len() <= 64
        && (1..=1024).contains(&image.columns)
        && (1..=1024).contains(&image.rows)
}

/// Drop the screen stored under `parent`.
pub(crate) fn clear(doc: &mut Doc, parent: &ObjId) -> Result<(), ActionError> {
    for key in [GRID, ALT_GRID, CURSOR, LINKS, IMAGES, ALTERNATE] {
        if doc.get(parent, key)?.is_some() {
            doc.delete(parent, key)?;
        }
    }
    Ok(())
}

/// The objects of the lines a [`Screen`] was read from, so a later change
/// re-reads only the lines it touched.
#[derive(Clone, Debug, Default)]
pub struct ScreenObjects {
    main: Vec<ObjId>,
    alt: Vec<ObjId>,
}

/// The screen stored under `parent`.
pub(crate) fn read(doc: &Doc, parent: &ObjId) -> (Screen, ScreenObjects) {
    let mut screen = Screen::default();
    let mut objects = ScreenObjects::default();
    for alternate in [false, true] {
        let (lines, ids) = read_lines(doc, parent, alternate, &[], &[], None);
        *screen.lines_mut(alternate) = lines;
        *objects.lines_mut(alternate) = ids;
    }
    read_cursor(doc, parent, &mut screen);
    screen.alternate = read_bool(doc, parent, ALTERNATE);
    screen.links = read_links(doc, parent);
    screen.images = read_images(doc, parent);
    (screen, objects)
}

/// Bring `screen`, read from the document root, up to date with `delta`.
pub(crate) fn refresh(screen: &mut Screen, objects: &mut ScreenObjects, doc: &Doc, delta: &Delta) {
    let root = automerge::ROOT;
    for alternate in [false, true] {
        let touched = match delta.key(&root, grid_key(alternate)) {
            Touch::Untouched => continue,
            Touch::Written => None,
            Touch::Below => Some(delta),
        };
        let (lines, ids) = read_lines(
            doc,
            &root,
            alternate,
            screen.lines(alternate),
            objects.lines(alternate),
            touched,
        );
        *screen.lines_mut(alternate) = lines;
        *objects.lines_mut(alternate) = ids;
    }
    if delta.key(&root, CURSOR) != Touch::Untouched {
        read_cursor(doc, &root, screen);
    }
    if delta.key(&root, ALTERNATE) != Touch::Untouched {
        screen.alternate = read_bool(doc, &root, ALTERNATE);
    }
    if delta.key(&root, LINKS) != Touch::Untouched {
        screen.links = read_links(doc, &root);
    }
    if delta.key(&root, IMAGES) != Touch::Untouched {
        screen.images = read_images(doc, &root);
    }
}

impl ScreenObjects {
    fn lines(&self, alternate: bool) -> &[ObjId] {
        if alternate { &self.alt } else { &self.main }
    }

    fn lines_mut(&mut self, alternate: bool) -> &mut Vec<ObjId> {
        if alternate {
            &mut self.alt
        } else {
            &mut self.main
        }
    }
}

/// The lines of one buffer. With `touched`, a line read before
/// (`previous`, from the objects `previous_ids`) that the change did not
/// touch is kept rather than read again.
fn read_lines(
    doc: &Doc,
    parent: &ObjId,
    alternate: bool,
    previous: &[Rc<Line>],
    previous_ids: &[ObjId],
    touched: Option<&Delta>,
) -> (Vec<Rc<Line>>, Vec<ObjId>) {
    let Ok(Some((Value::Object(ObjType::List), grid))) = doc.get(parent, grid_key(alternate))
    else {
        return (Vec::new(), Vec::new());
    };
    let known: HashMap<&ObjId, &Rc<Line>> = match touched {
        Some(_) => previous_ids.iter().zip(previous).collect(),
        None => HashMap::new(),
    };
    let mut lines = Vec::new();
    let mut ids = Vec::new();
    for item in doc.list_range(&grid, ..).take(MAX_LINES) {
        if !matches!(item.value, automerge::ValueRef::Object(ObjType::Map)) {
            continue;
        }
        let id = item.id();
        let line = match (known.get(&id), touched) {
            (Some(line), Some(delta)) if !delta.touches(&id) => Rc::clone(line),
            _ => Rc::new(read_line(doc, &id)),
        };
        lines.push(line);
        ids.push(id);
    }
    (lines, ids)
}

fn read_line(doc: &Doc, line: &ObjId) -> Line {
    let text = match doc.get(line, "text") {
        Ok(Some((Value::Object(ObjType::Text), id))) => doc.text(&id).unwrap_or_default(),
        _ => String::new(),
    };
    let attrs = match doc.get(line, "attrs") {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => bytes.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    Line { text, attrs }
}

fn scalar_u64(doc: &Doc, obj: &ObjId, key: &str) -> Option<u64> {
    match doc.get(obj, key).ok()?? {
        (Value::Scalar(value), _) => match value.as_ref() {
            ScalarValue::Uint(n) => Some(*n),
            ScalarValue::Int(n) => u64::try_from(*n).ok(),
            _ => None,
        },
        (Value::Object(_), _) => None,
    }
}

fn scalar_string(doc: &Doc, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key).ok()?? {
        (Value::Scalar(value), _) => match value.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        (Value::Object(_), _) => None,
    }
}

fn read_bool(doc: &Doc, obj: &ObjId, key: &str) -> bool {
    matches!(
        doc.get(obj, key),
        Ok(Some((Value::Scalar(value), _))) if matches!(value.as_ref(), ScalarValue::Boolean(true))
    )
}

fn read_cursor(doc: &Doc, parent: &ObjId, screen: &mut Screen) {
    screen.cursor = match doc.get(parent, CURSOR) {
        Ok(Some((Value::Object(ObjType::Map), cursor))) => (
            scalar_u64(doc, &cursor, "x").unwrap_or(0),
            scalar_u64(doc, &cursor, "y").unwrap_or(0),
        ),
        _ => (0, 0),
    };
}

/// The hyperlinks, revalidated: a collaborator's build may not have
/// checked them.
fn read_links(doc: &Doc, parent: &ObjId) -> BTreeMap<u16, String> {
    let Ok(Some((Value::Object(ObjType::Map), links))) = doc.get(parent, LINKS) else {
        return BTreeMap::new();
    };
    doc.keys(&links)
        .take(MAX_LINKS)
        .filter_map(|key| {
            let id = key.parse::<u16>().ok().filter(|id| *id != 0)?;
            let url = scalar_string(doc, &links, &key)?;
            crate::inner::safe_osc_text(url.as_bytes(), MAX_LINK_BYTES).map(|url| (id, url))
        })
        .collect()
}

fn read_images(doc: &Doc, parent: &ObjId) -> Vec<TerminalImagePlacement> {
    let Ok(Some((Value::Object(ObjType::List), images))) = doc.get(parent, IMAGES) else {
        return Vec::new();
    };
    let narrow = |value: Option<u64>| value.and_then(|value| u32::try_from(value).ok());
    let mut placements = Vec::new();
    for index in 0..doc.length(&images).min(MAX_IMAGES) {
        let Ok(Some((Value::Object(ObjType::Map), id))) = doc.get(&images, index) else {
            continue;
        };
        let (Some(hash), Some(mime)) = (
            scalar_string(doc, &id, "hash"),
            scalar_string(doc, &id, "mime"),
        ) else {
            continue;
        };
        placements.push(TerminalImagePlacement {
            hash,
            mime,
            byte_len: scalar_u64(doc, &id, "byte_len").unwrap_or(0),
            row: scalar_u64(doc, &id, "row")
                .and_then(|row| usize::try_from(row).ok())
                .unwrap_or(0),
            col: scalar_u64(doc, &id, "col")
                .and_then(|col| usize::try_from(col).ok())
                .unwrap_or(0),
            columns: narrow(scalar_u64(doc, &id, "columns")).unwrap_or(0),
            rows: narrow(scalar_u64(doc, &id, "rows")).unwrap_or(0),
            pixel_width: narrow(scalar_u64(doc, &id, "pixel_width")).unwrap_or(0),
            pixel_height: narrow(scalar_u64(doc, &id, "pixel_height")).unwrap_or(0),
            protocol_id: narrow(scalar_u64(doc, &id, "protocol_id")),
            alternate_screen: read_bool(doc, &id, "alternate_screen"),
        });
    }
    placements
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> Rc<Line> {
        Rc::new(Line {
            text: text.into(),
            attrs: vec![0; text.len()],
        })
    }

    fn texts(screen: &Screen, alternate: bool) -> Vec<String> {
        screen
            .lines(alternate)
            .iter()
            .map(|line| line.text.clone())
            .collect()
    }

    /// Apply `edits` to a document that holds `before`, and read it back.
    fn round_trip(doc: &mut Doc, edits: &[ScreenEdit]) -> Screen {
        apply(doc, &automerge::ROOT, edits).expect("edits apply");
        read(doc, &automerge::ROOT).0
    }

    #[test]
    fn the_edits_between_two_screens_rebuild_the_second_from_the_first() {
        let mut doc = Doc::new();
        let a = line("a");
        let b = line("b");
        let c = line("c");
        let d = line("d");
        let before = Screen {
            main: vec![Rc::clone(&a), Rc::clone(&b), Rc::clone(&c), Rc::clone(&d)],
            alt: vec![line("x"), line("y")],
            ..Screen::default()
        };
        let stored = round_trip(&mut doc, &edits(None, &before));
        assert_eq!(texts(&stored, false), ["a", "b", "c", "d"]);
        assert_eq!(texts(&stored, true), ["x", "y"]);

        // Scrollback trimmed off the front, a line rewritten in place, one
        // appended, and the alternate buffer scrolled up by one.
        let after = Screen {
            main: vec![Rc::clone(&b), line("C"), Rc::clone(&d), line("e")],
            alt: vec![Rc::clone(&before.alt[1]), line("z")],
            alternate: true,
            cursor: (3, 1),
            links: BTreeMap::from([(1, "https://tilo.so".to_owned())]),
            ..Screen::default()
        };
        let changes = edits(Some(&before), &after);
        assert!(
            changes.contains(&ScreenEdit::Delete {
                alternate: false,
                index: 0,
                count: 1
            }),
            "{changes:?}"
        );
        assert!(
            !changes.iter().any(
                |edit| matches!(edit, ScreenEdit::Write { text, .. } if text == "b" || text == "d")
            ),
            "lines that did not change are not written: {changes:?}"
        );
        let stored = round_trip(&mut doc, &changes);
        assert_eq!(texts(&stored, false), ["b", "C", "d", "e"]);
        assert_eq!(texts(&stored, true), ["y", "z"]);
        assert!(stored.alternate);
        assert_eq!(stored.cursor, (3, 1));
        assert_eq!(
            stored.links.get(&1).map(String::as_str),
            Some("https://tilo.so")
        );
    }

    #[test]
    fn a_change_rereads_only_the_lines_it_touched() {
        let mut doc = Doc::new();
        let before = Screen {
            main: vec![line("one"), line("two"), line("three")],
            ..Screen::default()
        };
        apply(&mut doc, &automerge::ROOT, &edits(None, &before)).unwrap();
        let (mut screen, mut objects) = read(&doc, &automerge::ROOT);
        let untouched = Rc::clone(&screen.main[0]);
        let heads = doc.get_heads();
        apply(
            &mut doc,
            &automerge::ROOT,
            &[ScreenEdit::Write {
                alternate: false,
                index: 1,
                text: "TWO".into(),
                attrs: vec![],
            }],
        )
        .unwrap();
        let after = doc.get_heads();
        let patches = doc.diff(&heads, &after);
        refresh(
            &mut screen,
            &mut objects,
            &doc,
            &Delta::from_patches(&patches),
        );
        assert_eq!(texts(&screen, false), ["one", "TWO", "three"]);
        assert!(
            Rc::ptr_eq(&screen.main[0], &untouched),
            "an untouched line is kept, not read again"
        );
    }

    #[test]
    fn edits_naming_missing_lines_are_refused() {
        let mut doc = Doc::new();
        let delete = ScreenEdit::Delete {
            alternate: false,
            index: 0,
            count: 1,
        };
        assert!(apply(&mut doc, &automerge::ROOT, &[delete]).is_err());
        let write = ScreenEdit::Write {
            alternate: true,
            index: 3,
            text: "x".into(),
            attrs: vec![],
        };
        assert!(apply(&mut doc, &automerge::ROOT, &[write]).is_err());
    }
}
