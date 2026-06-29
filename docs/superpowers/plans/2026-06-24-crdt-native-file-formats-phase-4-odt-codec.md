# CRDT-Native File Formats — Phase 4 (`.odt` codec) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans (inline). Phases 0–1 are complete and green (qdoc model + convergence, Markdown codec, history, ui::State binding). This plan implements the headline deliverable: a lossless-preserving `.odt` codec on the existing `FormatProvider` contract.

**Goal:** `qdoc::codecs::odt::OdtCodec` implementing `FormatProvider` — import a real `.odt` into the canonical model and export back **losslessly** (the originating tool sees no data loss). ODF first (clean, well-specified) before `.docx`.

**Architecture:** ODF `.odt` is a ZIP (OPC-like) of XML parts. The codec stores every part *except* `content.xml`'s body verbatim in the model's `preserved` map; it maps the modeled subset of `content.xml` (`text:h`/`text:p`/`text:list`/`text:span`) to blocks + marks, and stashes unmodeled inline/block constructs as **fidelity atoms**. Export reconstructs `content.xml` from the model + fidelity atoms and re-emits all preserved parts verbatim, then re-zips (mimetype first, stored/uncompressed). The byte-cache (Phase 2) makes untouched files byte-identical; here the bar is *semantic* losslessness.

**Tech Stack:** `zip` (ODF container), `quick-xml` (already in the tree — parse/serialize `content.xml`, preserve unknown subtrees as raw events), the Phase-0 `qdoc` model (`DocMut`/`DocRef`/`materialize`), `proptest`.

## Global Constraints

- **No external LibreOffice** in CI/dev — fixtures are hand-crafted minimal ODF built programmatically in tests (a `fixture::minimal_odt(body_xml)` helper). If `soffice` is ever present, an `#[ignore]`-free gated test may also round-trip a real file, but the committed corpus is self-contained.
- **`mimetype` must be the first zip entry, stored (uncompressed)** — ODF requirement for magic-byte detection. The zip writer must honor this.
- **Preserve everything unmodeled** verbatim: parts (`styles.xml`, `meta.xml`, `settings.xml`, `META-INF/manifest.xml`, `Pictures/*`, `Thumbnails/*`) → `preserved["odt/parts/<name>"]`; the `content.xml` prologue (`office:automatic-styles` etc.) → `preserved["odt/content-prologue"]`; unknown body elements → fidelity-atom blocks (`kind="fidelity"`, raw XML in an attr).
- **Reuse `FormatProvider`** (`import(&self, &[u8], &mut DocMut)`, `export(&self, DocRef) -> Vec<u8>`). Register the codec at `ProviderTier::Builtin` for `odt`.
- **automerge 0.8 / qdoc Phase-0 model** unchanged. **Verification covenant** applies.
- **Build/test:** `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc odt'`.

## ODF → canonical mapping

| ODF (content.xml) | Canonical block/mark |
|---|---|
| `text:h` + `text:outline-level="N"` | block `heading`, attr `level=N` |
| `text:p` | block `paragraph` |
| `text:list` > `text:list-item` > `text:p` | block `list_item`, attr `ordered` from list style |
| `text:span text:style-name="S"` | mark, by resolving `S` in automatic-styles: `fo:font-weight=bold`→`strong`, `fo:font-style=italic`→`em`, `style:text-underline-*`→`underline` |
| text runs | spliced into the block's `Text` |
| any other element | **fidelity atom** (preserved verbatim, re-emitted in place) |

Export inverts this: headings/paragraphs/list items → `text:h`/`text:p`/`text:list`; marks → `text:span` referencing a small set of codec-authored automatic styles **appended** to the preserved prologue (so existing styles are untouched).

## Tasks

### Task 1: ODF container read/write (zip) with verbatim part preservation
- Add `zip = "2"` to `qdoc/Cargo.toml`. Create `qdoc/src/codecs/odt/mod.rs` + `container.rs`.
- `OdtContainer { parts: IndexMap<String, Vec<u8>> }` with `read(bytes) -> Result<Self>` (preserve entry order) and `write(&self) -> Vec<u8>` (mimetype first + stored, rest deflated).
- **Test:** build a minimal `.odt` (fixture helper), `read` then `write`, `read` again → identical part set + `mimetype` first/stored; the bytes re-open as a valid zip. Commit.

### Task 2: parse `content.xml` body → blocks
- `content.rs`: stream `content.xml` with `quick-xml`. On `office:text` children, map `text:h`/`text:p`/`text:list` to blocks via `DocMut`; splice text. Capture everything before `office:text` (the prologue incl. `office:automatic-styles`) verbatim into `preserved["odt/content-prologue"]` and everything after into a suffix preserved key.
- **Test:** import a fixture with a heading + two paragraphs → blocks reflect kinds + text + heading level. Commit.

### Task 3: spans → marks (resolve automatic styles)
- Pre-scan `office:automatic-styles` for `style:style style:family="text"` → map style-name → set of {strong,em,underline} from `fo:font-weight`/`fo:font-style`/`style:text-underline-style`. During body parse, `text:span` with a known style-name opens a mark over its text range (same offset tracking as the md codec).
- Unknown style-names → preserve the span as a fidelity atom (don't drop it).
- **Test:** a paragraph with a bold span → block has a `strong` mark over the right range. Commit.

### Task 4: export model → `.odt`
- `export.rs`: rebuild `content.xml` = preserved prologue + generated `office:text` body (blocks → `text:h`/`text:p`/`text:list`; marks → `text:span` referencing appended codec styles `qdoc-strong`/`qdoc-em`/`qdoc-underline` injected into the prologue's automatic-styles) + preserved suffix. Reassemble the container: replace `content.xml`, re-emit all other parts verbatim, `write`.
- **Test:** import→export→import idempotency on a fixture (materialized models equal, ignoring block ids); exported bytes re-open as a valid `.odt` zip with `mimetype` first. Commit.

### Task 5: preservation layer + lossless round-trip
- Fidelity atoms: unmodeled body elements become `kind="fidelity"` blocks holding raw XML (attr `xml`), re-emitted in document order on export.
- Unmodeled parts already preserved (Task 1). **Test:** a fixture containing an unmodeled part (`settings.xml`) and an unmodeled body element (`text:soft-page-break` or a `table:table`) round-trips: after import→export, the unknown part is byte-identical and the unknown element reappears verbatim. Commit.

### Task 6: register + proptest + corpus
- Register `OdtCodec` at `ProviderTier::Builtin` for `odt` in a `qdoc::codecs::builtin_registry()` helper (also registers Markdown). **Test:** registry resolves `odt`→OdtCodec; a proptest builds random modeled ODF bodies and asserts export idempotency; commit a small hand-authored `.odt` corpus under `qdoc/tests/fixtures/odt/` with a round-trip test. Commit.

## Self-Review
- Mapping covers headings/paragraphs/lists/spans→marks (Tasks 2–4); preservation makes it lossless (Tasks 1, 5); registration + proptest + corpus give the testing pillar (Task 6).
- Deferred: tables/images as *editable* structure (preserved as fidelity atoms, not lost); `.docx`/OOXML (Phase 5); FS deployment (Phase 2).
- Risk: ODF style indirection (formatting via named styles, not inline) — handled by the automatic-styles pre-scan (Task 3) with verbatim fallback. `zip`/`quick-xml` exact APIs confirmed against crate source at execution (same discipline that made Phases 0–1 compile first-try).

## Execution
Inline. Land Task 1 (container) first — it's the lossless backbone. Hand-crafted fixtures keep the suite self-contained (no LibreOffice). After `.odt` is green, Phase 3 (sandboxed registration) makes the same codec contract runnable for untrusted third-party providers.
