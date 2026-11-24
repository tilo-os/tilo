# tilo-doc

The document engine behind Tilo's rich-text and workbook resources. A document is a tree of
blocks, each with its text, marks and attributes, stored as an Automerge document: every edit
is a change on a CRDT history, concurrent edits merge, and a replay produces the same text on
every device. The crate reads a document back as a plain snapshot and compares two snapshots
with a structural diff that names the blocks and cells that changed.

Rich text and sheets have separate models in this crate. `Document` and `DocSnapshot` cover
paragraphs, headings, list items, tables, images, footnotes, comments and page headers and
footers; `Workbook`, `WorkbookSnapshot` and the `sheet` module cover cells, formulas and their
values.

Binary inputs are referenced by content address and live in the caller's `tilo_content` store,
and file formats (Markdown, DOCX, ODT, RTF, EPUB, HTML, PDF) are `tilo-doc-codecs`' work.

Apps normally edit a document through the typed actions of the rich-text resource,
`tilo-document`, which wraps this crate, so permissions, history and undo apply as they do to
any other resource. Depend on `tilo-doc` directly for a document editor, an importer or
exporter, or code that reads and compares snapshots. The `DocMut` handle this crate exports is
for that resource's `Action::apply` and is not an application API.

## Use it

```toml
[dependencies]
tilo-doc = "0.1"
```

```rust
use tilo_doc::{Document, diff_snapshots};

let mut doc = Document::new();
let block = doc.insert_block(None, "paragraph");
doc.splice_text(&block, 0, 0, "Hello, world");

let before = doc.materialize();

doc.splice_text(&block, 12, 0, "!");
let after = doc.materialize();

let diff = diff_snapshots(&before, &after);
assert_eq!(diff.summary.changed, 1, "the paragraph's text changed");
```

`Document` edits one block or one range at a time: `insert_block`, `remove_block`,
`move_block`, `splice_text`, and `add_mark` for a span of a block. `materialize` reads the whole
document as a `DocSnapshot`, whose `blocks` are `BlockView`s with their id, kind, text, marks,
attributes and children. `Document::save` writes the CRDT document as bytes and
`Document::load` reads it back; a document stored under a field of a larger record opens with
`Document::from_automerge_under`.

`diff_snapshots` and `diff_workbooks` return the changes between two versions, with a summary of
what was added, removed and changed, which is what a history panel or a review shows.

## Learn more

- Rust app documentation: https://tilolang.dev/docs/rust-apps
- Repository: https://code.tilo.so/tilo/tilo

## License

MIT
