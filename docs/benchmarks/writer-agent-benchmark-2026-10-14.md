# Writer agent benchmark: typed and input arms, export and reopen

One fixed task — a short field report with two headings, a bold phrase, a
two-item bullet list and an italic date — done twice against one checked-in
reference, then saved as DOCX and PDF and reopened. All 15 qualifications
passed on the run recorded here.
[The receipt](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/benchmarks/writer-agent-benchmark-2026-10-14.json) is the test's own output.

The benchmark is a test, so it runs in CI with the rest of Writer's tests:

```sh
cargo test --locked -p word-processor --test it writer_benchmark::
```

`TILO_WRITER_BENCHMARK_RECEIPT=PATH` writes the receipt.
`TILO_WRITER_BENCHMARK_FRAMES=DIR` keeps the editor frames as PNGs, plus the
exported DOCX and PDF, so a person can inspect the actual output. The source
is `apps/word-processor/tests/writer_benchmark.rs`.

## The two arms

- **Typed.** An agent (`agent:writer-benchmark`, acting for the owner, with a
  participant caret of its own) reads the actions the document publishes. It
  then drives Writer only through 22 invocations of 5 of them (`type-text`,
  `new-paragraph`, `set-paragraph-style`, `find-text`, `set-format`), by id
  with JSON arguments. It names no paragraph ids and no coordinates: it types
  at its caret and finds the phrases it formats. That is the bound-action door
  reached by `tilo-control actions invoke`, the MCP bridge and the palette.
- **Input.** A computer-use script clicks into the page body and types,
  presses keys and holds modifiers. That is the `click`, `type`, `key` and
  `modifiers` vocabulary of the control socket's input half, delivered to the
  same headless frame harness. Markdown prefixes (`# `, `## `, `- `) make the
  headings and the list; Ctrl+B and Ctrl+I set the formats. It reads nothing
  but the frame.

Both arms are graded from the document the system owns: paragraph kinds,
heading levels, list kinds, text and the exact phrases each inline format
covers. What an arm reports about itself is not graded.

## Qualifications observed

| Qualification | Observed |
| --- | --- |
| `typed.discovers_every_action` | the 5 actions used are among the 67 the document publishes |
| `typed.every_action_admitted` | 22 invocations, none refused |
| `typed.document_matches_reference` | 6 paragraphs and 2 formatted phrases match |
| `typed.history_attributed_to_agent` | 17 history entries, every one names the agent |
| `visual.editor_draws_the_document` | 1.42% of the frame differs from a blank document's |
| `input.document_matches_reference` | 6 paragraphs and 2 formatted phrases match |
| `visual.arms_draw_the_same_page` | 0.000% of pixels differ (at most 0.01%) |
| `docx.exported` | 2496 bytes, a ZIP container with `word/document.xml` |
| `docx.reopened_matches_reference` | launched in Writer: 6 paragraphs and 2 phrases match |
| `docx.reopened_draws_the_same_page` | 0.000% of pixels differ from the original editor |
| `pdf.exported` | 1282 bytes, a PDF |
| `pdf.reopened_text` | one page; all 6 paragraphs, in order |
| `pdf.page_pixels` | white paper, 1.12% of a 600×776 render is text |
| `pdf.inline_formats` | bold changes 0.152% of the page, italic 0.042% |
| `cleanup.scratch_removed` | the exported files and their directory are gone |

Save As runs through Writer's published `save-as` action, with the
confirmation the host asks a person for before a file write. The DOCX is
reopened the way a file is launched in Writer. The PDF is reopened in the PDF
viewer's portable engine, which renders it and reads its text layer.

The pixel threshold is calibrated against a negative control. Two renders of
the report with the caret in the same place are identical. Losing the italic
on the ten characters `3 November` changes 0.036% of the frame, and
`a_page_missing_one_format_is_not_the_same_picture` checks that the comparison
reports this. `pdf.inline_formats` compares the exported page with a direct
render of the same document. They must be identical. It then compares the
exported page with a render that drops one format. It must differ.

## What the benchmark found

On its first run, the PDF export drew `steering group` in regular weight and
`3 November` upright. `tilo-doc-pdf` set bold only on headings and ignored
inline `strong` and `em` marks. The DOCX round trip and the editor were
correct. The same change makes the PDF exporter lay out a line as mixed runs
in the four base-14 Helvetica faces: regular, bold, oblique and bold oblique.
Without that change, `pdf.inline_formats` fails, because removing either mark
leaves the page unchanged. The run also showed that a click near the top of an
empty page edits the page header, not the body. The input arm clicks into the
body.

## Not qualified by this run

These are untested. They are not passes.

- **A model in the loop.** Both arms are scripted plans. No model was called
  and nothing was spent. Whether a given model discovers these actions, or
  finds these click targets from pixels, is not measured.
- **A live session.** The arms drive the app through the same frame harness
  that the control socket feeds. This run did not use a running compositor
  or `tilo-control` over a socket. The shell does not build on macOS. A live
  run (`scripts/headless-examples/spawn-headless.sh`, then `tilo-control
  actions invoke` with `--args-stdin`) remains for a Linux host.
- **Other readers.** No third-party word processor opened the DOCX. Only the
  portable engine opened the PDF, not the pdfium helper.
- **Formats beyond the reference.** The task has no tables, images,
  underline, colour or multi-page text. The PDF exporter still does not draw
  underline, strike, colour, size or font family.
- **Themes.** Editor frames were compared only in the default theme.
- **Speed.** The receipt records elapsed time, 185 ms for the typed arm and
  1209 ms for the input arm. These come from a debug build on a shared Apple
  M5 Max running macOS 27. They are not performance claims.
