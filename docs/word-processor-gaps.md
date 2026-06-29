# Word Processor — Missing Features & Gaps

A running inventory of what the qOS word processor (`editor/` over `qdoc`/`ui`)
lacks. Categorized; each item is a feature (F), technical/model (T), or UX (X)
gap — many are several at once.

## 1. Document content (things you can't put in a doc yet)
- **Images** (F/T): insert, resize, crop, alt text, inline vs. float, text-wrap, storage in the CRDT + P2P fs.
- **Footnotes / endnotes** (F).
- **Headers & footers** (F): per page/section, page numbers, running heads.
- **Page / column / section breaks** (F).
- **Multi-column layout** (F).
- **Table of contents** (auto from headings), lists of figures/tables (F).
- **Cross-references, bookmarks, captions** (F).
- **Hyperlinks — insert/edit UI** (F/X): currently render-only; no way to create or edit a link.
- **Math / equations**, special-character / symbol / emoji picker (F).
- **Subscript / superscript, small caps, all caps** (F).
- **Text highlight color** (distinct from text color) (F).
- **Drop caps, watermarks, page background** (F).
- **Fields** (F): date, page X of Y, filename, author, auto-update.
- **Code blocks** (monospace + background, optional syntax highlight) (F).
- **Checklists / task lists** (F).
- **Lists, deeper** (F): nested/multi-level, custom bullet glyphs, custom numbering, restart/continue, indent controls.

## 2. Styles & formatting (the component-instance model)
- **Styles panel** (X): UI to edit a style's font/size/color/align/spacing/indent/line-height — the *model* exists, the editor only edits spacing (via drag). This is the headline gap.
- **Character styles** (T): named inline styles, not just paragraph styles + ad-hoc marks.
- **Style inheritance / "based on"** (T): cascading base styles.
- **Create / update / rename / delete styles** from the UI (X).
- **Document theme** (F): palette + font pairing that styles reference.
- **Line spacing** control (X): single/1.5/double/custom — currently a fixed 1.4 multiplier, no UI.
- **Paragraph indents** (F): first-line indent, hanging indent, right indent (only left indent exists).
- **Tab stops** (F): left/right/center/decimal + a ruler.
- **Letter spacing / kerning / ligatures** (F).
- **Paragraph shading** (F): background fill behind a paragraph/style.
- **Clear formatting** + **format painter** (X).
- **Templates** + default style for new documents (F).

## 3. Page & layout
- **Real pagination** (F/T): discrete pages with page breaks — currently one continuous sheet.
- **Page size picker** (F): A4 / Letter / Legal / custom + units (in/cm/px).
- **Orientation** portrait/landscape (F).
- **Print / print preview / PDF export** (F).
- **Per-section page setup** (F).
- **Facing pages / mirror margins / gutter** (F).
- **Zoom** in/out (X): the page is fixed-size but not zoomable.
- **Horizontal scroll** when the window is narrower than the page (X): currently clips.
- **Persist page margins in the document** (T): margins are widget-local and reset on reopen.

## 4. Editing & interaction (UX)
- **Find & Replace** (X): case, whole-word, regex, replace-all, in-selection.
- **Spell check + grammar**, dictionary, autocorrect, autocomplete (F/X).
- **Right-click context menu** (X): cut/copy/paste/format/link/…
- **Undo/redo UI + shortcuts** (X): CRDT undo exists but no buttons, no Ctrl+Z/Y, no visible history.
- **Full keyboard shortcuts** (X): today only Ctrl+B/I/U, C/V/X, A. Missing Ctrl+S, open, find, headings, lists, etc.
- **Save / Open / Save-As dialogs + file picker + recent files** (F/X): currently CLI-arg only.
- **Autosave + crash recovery** (T/X).
- **Drag-to-move text; drag to reorder paragraphs/blocks** (X).
- **Multiple cursors / multi-selection** (X).
- **Paste menu** (X): paste-as-plain / match-style (Ctrl+Shift+V exists, no menu).
- **Hover states, tooltips, disabled states**; toolbar overflow on narrow windows (X).

## 5. Navigation & view
- **Outline / document map / navigation pane** (X): jump by heading.
- **Ruler** (X): margins, indents, tab stops, drag.
- **Show non-printing characters** (¶, tabs, spaces) (X).
- **Focus / typewriter / distraction-free mode** (X).
- **Split view / side-by-side / multiple windows** of one doc (X).
- **Go to page / line / bookmark** (X).
- **Selection word/char count + reading time + readability** (X): doc totals exist; selection does not.
- **Minimap / scroll thumbnail** (X).

## 6. Collaboration (the OS is collab-native — largely unused here)
- **Real-time co-editing** with remote cursors/selections (F/T): `ui` has presence primitives, not wired into the editor.
- **Comments / annotation threads**; resolve/reply (F).
- **Suggestions / track-changes**; accept/reject (F).
- **@mentions, presence avatars, "who's here"** (F/X).
- **Share / permissions UI**; per-cluster sandbox integration (F).

## 7. File formats & I/O
- **.docx** (in progress), **.doc**, **.rtf**, **.html**, **.txt**, **.pdf export**, **.epub** (F).
- **Style round-trip** (T): export the stylesheet to real **ODF/OOXML named styles** — today it lives only in qdoc `meta` JSON and never becomes real document styles on save.
- **Image/media storage** in the doc + via P2P fs (hydrate on demand) (T).
- **Embedded fonts** (T).
- **Web/Word paste cleanup** (T).
- **Version-history UI** (X): qdoc has a timeline + time-travel; no UI surface.

## 8. Data model / technical
- **Incremental layout** (T): only re-shape changed blocks — today every block re-shapes every frame.
- **Virtualized rendering** for very long docs (T): lay out only visible blocks.
- **Undo granularity** (T): finer than full-document snapshots; coalescing.
- **IME / dead-key / complex-script input** (T): CJK/Indic composition.
- **RTL & bidirectional text; vertical text** (T).
- **Font fallback chains, discovery/loading, missing-glyph handling** (T).
- **Unit/DPI system** (T): everything is hardcoded px.
- **Large-document memory + streaming load** (T).
- **Round-trip proptests** for each new feature (T).

## 9. Accessibility & input
- **AT-SPI accessibility tree / screen reader** (T/X): a stated daily-driver goal.
- **Keyboard-only operation** of every control (toolbar, panels, tables) (X).
- **High-contrast / reduced-motion / font scaling** (X).

## 10. Theming & visual polish (UX)
- **Use the OS `Theme`** instead of hardcoded colors (X): CLAUDE.md requires widgets to read the Theme; the editor hardcodes INK/PAPER/etc.
- **Dark document mode** (X): chrome is dark, the page is always white paper.
- **Caret blink + smooth caret/selection motion** (X).
- **i18n via `fluent`** (T/X): user-facing strings are hardcoded English ("Start writing…", toolbar labels, status text).
- **Empty-state, loading, and error UI** (X): failed open/save surfaces nothing.
- **Real icons + responsive toolbar** (X): text labels ("B/I/U") vs. iconography.
