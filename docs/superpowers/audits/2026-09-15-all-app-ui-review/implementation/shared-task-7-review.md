### Spec Compliance

- ✅ Spec compliant. The production change is confined to the attributed shaping boundary: only terminal-LF input appends one empty `BufferLine`, with `LineEnding::None`, `Shaping::Advanced`, final effective attributes, and the requested alignment before normal sizing, wrapping, and shaping (`ui/src/richtext.rs:350-386`). There is no sentinel character or synthetic source byte; `line_starts`, `line_lens`, `offset_map`, and `text_len` remain derived from the original text (`ui/src/richtext.rs:326-338`, `ui/src/richtext.rs:393-400`).
- ✅ Final effective style and metrics are preserved. Existing run construction drops only empty spans, resolves each non-empty run's family/weight/size/line-height/italic/color, and leaves the final effective run at `built.last()` (`ui/src/richtext.rs:430-477`); the terminal row clones those attributes and alignment (`ui/src/richtext.rs:361-372`). The regression checks empty text, no line ending, centered alignment, bold/italic, and 24px/30px metrics (`ui/src/richtext.rs:960-988`).
- ✅ One and multiple trailing LFs, plain controls, split attributed spans, and 1×/2× scale are covered. The regression compares plain/styled height and EOF caret geometry and checks visual row, logical-line band, and below-content hit behavior for `"x"`, `"x\ny"`, `"x\n"`, `"\n"`, `"x\n\n"`, and a split-span terminal LF (`ui/src/richtext.rs:888-957`).
- ✅ TextInput integration covers measure/draw height agreement, EOF caret placement, final-row hit-to-EOF, caret-follow scrolling, syntax attribution, gutter presence, and two logical line origins at both scales (`ui/src/widget/text_input.rs:6924-7002`). Current-line and gutter painting consume the same repaired `logical_line_bands` and `logical_line_origins` APIs (`ui/src/widget/text_input.rs:1733-1802`), so the new authoritative row participates without a separate geometry policy.
- ✅ No extra production behavior was introduced. The supplied effective diff changes production code only in `ui/src/richtext.rs`; `ui/src/widget/text_input.rs` is test-only, and Scrollable/app sources, fixed-size interpretation, and primitive padding are untouched.

### Strengths

- The fix matches cosmic-text's own plain-text representation closely while remaining local to the compatibility boundary. The focused dependency check confirmed `BufferLine::new` owns exactly the supplied empty string, attributes, ending, and shaping mode, and `set_align` changes only alignment/cache state (`cosmic-text-0.19.0/src/buffer_line.rs:14-44`, `cosmic-text-0.19.0/src/buffer_line.rs:138-155`).
- The change is placed before `set_size`, `set_wrap`, and `shape_until_scroll`, so measurement, drawing, caret, hit testing, logical bands, gutter rows, and scrolling all consume the same shaped row (`ui/src/richtext.rs:361-390`).
- The tests are behavior-focused and include the failure modes from the brief. The report records genuine pre-fix RED failures, all affected focused suites GREEN, the three targeted regressions GREEN after formatting, `cargo check` GREEN, and clean formatting/diff checks (`.superpowers/sdd/2026-09-15-measured-ui-components/task-7-report.md:23-111`).

### Issues

#### Critical (Must Fix)

None.

#### Important (Should Fix)

None.

#### Minor (Nice to Have)

None.

### Evidence Limits

- Review scope was the supplied `ca5b92f36..fd7b0964f` package. The controller separately verified that the commits between original base `c7b2d576b` and effective base `ca5b92f36` do not touch `richtext` or `TextInput`; this review did not re-derive that history.
- Per the reviewer contract, tests were not rerun because the report contains exact RED/GREEN commands and results for this code. The known macOS `__eh_frame` linker warning is recorded as baseline noise and does not affect the verdict.
- The only source read beyond the diff was the named API risk: whether directly appending a public cosmic-text `BufferLine` preserves empty content, attributes/metrics, alignment, and later shaping. The installed cosmic-text 0.19.0 implementation supports the chosen construction.

### Assessment

**Task quality:** Approved

**Reasoning:** The implementation repairs the missing terminal paragraph at one boundary without altering source bytes or unrelated geometry policy. The targeted regressions and focused suite evidence cover the specified rich-text and TextInput contracts, and no blocking or minor quality issue was found.

