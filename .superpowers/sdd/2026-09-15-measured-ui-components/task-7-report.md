# Task 7 report: Preserve the final empty attributed line

## Scope and result

- Owned production path: `ui/src/richtext.rs`.
- Owned regression path: `ui/src/widget/text_input.rs` (including the colocated `RichTextLayout` tests in `richtext.rs`).
- Normalized the attributed shaping boundary after `Buffer::set_rich_text`: when the source ends in LF, append the final empty `BufferLine` that cosmic-text 0.19.0 omits.
- The appended line contains no visible character and changes no source byte offset. It uses `LineEnding::None`, `Shaping::Advanced`, the final effective non-empty run attributes/metrics (base attributes as the empty-input fallback), and the requested block alignment.
- No Scrollable or app source, dependencies, manifests, or unrelated dirty/untracked files were changed for this task. No push was performed.

## Root cause and API confirmation

The supplied runtime diagnosis was reproduced by the regression tests. `Buffer::set_text` adds an empty terminal line after a line ending, while cosmic-text 0.19.0 `Buffer::set_rich_text` derives paragraphs from `BidiParagraphs`, which omits the paragraph after a terminal LF. `RichTextLayout` still retains its own final logical-line metadata, so the absent cosmic-text row caused the height, caret, visual-line, logical-band, hit, gutter, current-line, and caret-follow geometry to disagree.

The installed cosmic-text 0.19.0 API was read before implementation. `Buffer::lines` is public, and `BufferLine::new`, `AttrsList::new`, `LineEnding::None`, and `BufferLine::set_align` provide the required normalization without changing dependency code or inserting a sentinel byte.

## TDD evidence

### RED

An initial exact-filter attempt was too strict and selected no test; it did not count as RED:

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib attributed_terminal_lf_preserves_the_plain_layout_final_row -- --exact --nocapture
```

Result: exit 0, `0 passed; 3162 filtered out`.

The corrected focused commands failed for the expected missing-row behavior before production code changed:

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib attributed_terminal_lf_preserves_the_plain_layout_final_row -- --nocapture
```

Result: exit 101. `"x\n" styled and plain heights at 1x: expected 40, got 20`.

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib multiline_trailing_lf_keeps_measure_draw_hit_and_caret_scroll_in_sync -- --nocapture
```

Result: exit 101. TextInput measured `56.0` but the attributed draw path reported `36` at 1×.

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib terminal_empty_row_inherits_the_final_run_metrics_and_alignment -- --nocapture
```

Result: exit 101. The final buffer line was `"x"`, not the required empty row.

### GREEN

After the single boundary repair:

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib attributed_terminal_lf_preserves_the_plain_layout_final_row -- --nocapture
```

Result: exit 0, `1 passed; 0 failed`. Covers `"x"`, `"x\ny"`, `"x\n"`, `"\n"`, multiple terminal LFs, and split attributed spans at 1× and 2×; asserts plain/styled height, EOF caret, visual row, logical-line band, and below-content hit behavior.

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib terminal_empty_row_inherits_the_final_run_metrics_and_alignment -- --nocapture
```

Result: exit 0, `1 passed; 0 failed`. Confirms empty text, `LineEnding::None`, final run bold/italic/24px metrics, and centered alignment.

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib multiline_trailing_lf_keeps_measure_draw_hit_and_caret_scroll_in_sync -- --nocapture
```

Result: exit 0, `1 passed; 0 failed`. At 1× and 2×, measure and attributed draw agree within the documented subpixel ceiling, EOF remains the final visual row, the last-row click returns `text.len()`, caret-follow scroll reaches the end, syntax attribution remains present, and gutter/current-line row geometry includes both logical lines.

The first post-fix TextInput run exposed a test-only assumption that 2× text would not wrap (`visual_line_index == 1`). The assertion was narrowed to the intended contract: EOF visual index plus one equals the shaped run count, and EOF caret bottom equals layout height. The production fix did not change.

## Focused verification

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib richtext::tests -- --nocapture
```

Result: exit 0, `20 passed; 0 failed`.

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::text_input::tests -- --nocapture
```

Result: exit 0, `100 passed; 0 failed`.

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::code_editor::tests -- --nocapture
```

Result: exit 0, `23 passed; 0 failed`.

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --test it scrollable_test -- --nocapture
```

Result: exit 0, `12 passed; 0 failed`.

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check -p ui
```

Result: exit 0.

```sh
rustfmt --edition 2024 ui/src/richtext.rs ui/src/widget/text_input.rs
git diff --check -- ui/src/richtext.rs ui/src/widget/text_input.rs
rustfmt --edition 2024 --check ui/src/richtext.rs ui/src/widget/text_input.rs
```

Result: formatting applied only to the two owned Rust paths; both checks exited 0.

Cargo test emitted the existing macOS linker warning that `__eh_frame` is too large for compact unwind encoding. It did not fail compilation or tests. Some commands briefly waited for the intentionally shared target-directory lock.

## Self-review

- Scope: the only production branch is `full_text.ends_with('\n')`; non-terminal and plain shaping remain unchanged.
- Data integrity: the source string, `text_len`, `line_starts`, `line_lens`, offset map, and decoration ranges are unchanged. No sentinel character or virtual source byte was added.
- Style: `built.last()` is the final effective non-empty run produced by the existing normalization; an empty trailing `RichSpan` is not an effective run and remains ignored consistently with `build_rich_runs`.
- Geometry: the new row participates in cosmic-text shaping before size, wrap, and layout resolution, so existing caret, hit, logical-band, gutter, current-line, and scroll code consume one authoritative layout.
- Source reasoning: deleting the append corresponds to the already executed pre-fix RED state and reintroduces its failures. The dedicated style/alignment assertions are designed to fail if the appended line uses base metrics or omits alignment; those additional temporary mutations were not executed.
- No architectural ambiguity or scope expansion was found.

## Limits and excluded work

- This compatibility normalization is specific to terminal LF and cosmic-text 0.19.0. It can be removed after an upstream version preserves the final rich-text paragraph and the regressions remain green.
- CR/CRLF behavior was not expanded because Task 7 specifies LF and `RichTextLayout::line_map` models LF.
- LF-only selection decoration redesign remains excluded as required.
- No screenshot was requested or needed for this geometry/API regression.
- No broad workspace suite was run; verification stayed within the requested UI scopes.
