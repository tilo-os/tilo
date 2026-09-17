# Final-empty-line repair preflight

Read-only source anchors checked after diagnosis: RichTextLayout::shape_styled at ui/src/richtext.rs:317; caret_geometry_at:544, visual_line_index:599, logical_line_bands:617, logical_line_origins:634. Existing rich-text regression module begins around855; wrapped attributed tests around1263. TextInput::update_ml_scroll at1684 uses shaped caret top to recenter only after cursor movement; draw calls it around1872. Existing multiline measurement/draw scale regression around6949 is a useful neighbor.

This confirms the Task7 caret-follow requirement is observable without changing scroll policy: preserve final empty shaped row, move cursor to EOF, verify scroll follows that row. No need to modify Scrollable. Typed final newline, one/multiple newline-only values and split spans distinguish true final-line normalization from masking measure/draw mismatch.

No code/test changes or execution in this preflight. See trailing-line-diagnosis.md for actual public-API runtime proof.
