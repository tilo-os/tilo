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
