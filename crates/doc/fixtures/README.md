# Shared document fixtures

These are the canonical frozen document inputs for document crates and their
cross-domain consumers. Original archives live in the verified artifact cache;
`artifacts.json` pins one immutable copy of each input. See [the originals and
offline preparation](artifact-README.md).

- `sample.docx` and `sample.odt` are genuine office packages with the same
  heading, styled runs, list, table, and metadata structure.
- `xxe.docx` and `xxe.odt` exercise external-entity rejection.
- `zipbomb.docx` exercises decompression and package-size limits.

Build scripts prepare the original bytes in `OUT_DIR`; tests embed them with
`include_bytes!`, so their runtime remains hermetic and independent of the
process working directory. Generation produces review candidates under `target/`;
publish and verify new originals before changing their pins.

## LibreOffice-authored fidelity fixtures

Written by LibreOffice (26.8, flatpak `org.libreoffice.LibreOffice`) from the flat-ODF
sources in `src/`, converted with `soffice --headless --convert-to docx|odt`; run
`src/regenerate.sh` to rebuild them all, and `src/regenerate.sh --check FILE…` to have
LibreOffice re-open exported files. `tilo-doc-codecs`' `tests/office_fidelity.rs` imports, edits
and exports each one and checks that what the model does not represent survives.

- `lo-structure.docx`, `lo-structure.odt` — from `src/structure.fodt`: headings, custom
  paragraph and character styles, an external hyperlink, a bookmark, nested numbered and
  bulleted lists, a table with merged cells, a two-column section, a page break, a header
  and a "Page N of M" footer, and custom document properties.
- `lo-annotations.docx`, `lo-annotations.odt` — from `src/annotations.fodt`: a footnote,
  an endnote, a comment, a tracked insertion and deletion, and a bookmark reference.
- `lo-fields.docx`, `lo-fields.odt` — from `src/fields.fodt`: a table of contents, page
  number/count, title and date fields, variables, a caption sequence, and plain-text,
  checkbox, dropdown and date content controls (`w:sdt` in docx).
- `lo-drawings.docx`, `lo-drawings.odt` — from `src/drawings.fodt`: an inline image, a text
  box, a custom shape, a formula (OMML in docx, a MathML object in odt) and a chart.

These four source documents are original repository-authored material under the
root [AGPL-3.0 license](../../../LICENSE), not documents borrowed from the
LibreOffice upstream test suite. LibreOffice is their producer, not their source
copyright holder.

## External merged-table document

`python-docx-merged-cells.docx` is an **unmodified** copy of
[`features/steps/test_files/tbl-cell-access.docx`](https://github.com/python-openxml/python-docx/blob/e45454602b53e8e572b179ccf1c91093ec9f4ed7/features/steps/test_files/tbl-cell-access.docx)
from `python-openxml/python-docx` revision
`e45454602b53e8e572b179ccf1c91093ec9f4ed7`. Its four tables exercise ordinary,
horizontally merged, vertically merged and rectangular merged cells: 3 `gridSpan`
and 4 `vMerge` elements. The visible cell content is numeric; this is not a
complex prose, image or math document. The LibreOffice inputs supply those
complementary structures.

Upstream distributes the repository under the **MIT license**, copyright (c)
2013 Steve Canny. The exact revision's
[LICENSE](https://github.com/python-openxml/python-docx/blob/e45454602b53e8e572b179ccf1c91093ec9f4ed7/LICENSE)
is retained verbatim in [python-docx-LICENSE.txt](python-docx-LICENSE.txt).
The license grants copying, redistribution and modification with that notice;
no separate fixture license or exception is declared for this path. Package
inspection found Word-generated XML, a thumbnail and numeric table text, not
third-party embedded artwork. Office author/company metadata is not itself a
separate permission grant. We rely on the upstream repository's explicit MIT
grant, not on a document being publicly downloadable. License file SHA-256:
`7652f271e46d0d533e9dc463f3b5fcbdcacf4d6a9c8d6b554d15efd0f37f6132`.

## Writer application compatibility evidence

Run `cargo test --locked -p word-processor --test it e2e_docx::`. In addition to
the app-authored rich document case, five frozen-document cases open a real
local `.docx` through the framework `WordProcessorApp` launch/import path, lay
out the production editor and inspect its exposed text, append Unicode text to
one identified imported block through the typed resource edit action, invoke
its actual Save As action, and launch a fresh instance on the saved `.docx`.
No UI or model-only alternate importer is used. All five check the edited text,
block kinds, selected style/table attributes, formatting marks, header/footer,
note/comment contents, image content addresses, package-part inventory, exact
media/embedding bytes and rich-body element counts. Original source files must
remain unchanged. The application adopts comments into framework resources;
the test reads the same combined document/comment view that Save As uses.

This is **not** pixel-equivalence or universal Word compatibility:

- Paragraphs, heading/list styles, merged table cells, inline images, Unicode,
  header/footer text, notes and comments are represented by the document model.
  Tests prove these structures and editor text are available, not that every
  possible style has identical visual layout to Microsoft Word.
- Field/control displayed results are available as text. The tests do not prove
  field recalculation or an interactive Word content-control implementation.
- Math, charts, shapes, text boxes and other unsupported rich body XML use
  fidelity carriers; retained XML/payload is **not** proof of editable/rendered
  equivalents. Their body element counts and original binary payloads survive.
- Note/comment side-part paragraphs currently import/export plain text, not all
  paragraph/run formatting: for example the LibreOffice endnote paragraph's
  `w:pStyle` is lost. Side-part style equivalence is not claimed.
- Word's private `_GoBack` navigation bookmark is intentionally discarded.
  Redundant default single-column `w:cols` can be normalized away; the modeled
  column count must remain equal. Local CRDT author attribution is not exported
  as Word text formatting.

The external document adds independent authored bytes and real merged-cell
coverage. It does not replace strict third-party engine diagnostics, and no
paid Word compatibility engine or vendor credential is required for these
local import/edit/save/reopen tests.
