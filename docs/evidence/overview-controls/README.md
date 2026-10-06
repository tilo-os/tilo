# Native overview controls evidence

Production native `ShellOverlayHost::draw_overview` captures, rendered remotely.
Before: current-main baseline 88a46ec509a3acb3a572c3f2e2c18af28f175000.
After / verified UI source: d7ef1033321936bb3a4c5556e93aa67f1c1da547.

The baseline rejects window/create accessibility focus and keyboard Tab creation;
the compact enlarged-text panel hides destination and creation controls. The
candidate uses the shared ListRowFace and focus ring, follows the toolkit's
compact opaque-page rule, and scrolls the body when its header crowds the list.
Roomy panels retain their material, composition and animation.

All 36 PNGs are retained as native ChatGPT Library images, not source binaries.
[artifacts.json](artifacts.json) records their file IDs, dimensions, source
revisions and SHA-256 checksums.

Representative pairs delivered to the user:

| Capture | Before Library file | After Library file |
| --- | --- | --- |
| Dark creation, 1280 × 800 | file_00000000685881f7b6c24ce15fdc0136 | file_00000000166c81f7ab5ee1f0b5fa08c8 |
| Compact creation, 200% text, 960 × 540 | file_0000000005d481f6bdaefc1ee4e4e1ca | file_00000000de3081f5b885bb89b1e37a93 |

Matrix: dark/light/high contrast at 1280 × 800; 200% text at 1280 × 800;
150% UI/130% text at 1280 × 800; compact 200% text at 960 × 540.
Each theme/viewport captures workspace, preview and creation focus.

Actual screenshot inspection covered creation, room and preview focus,
light/high contrast, combined scaling, compact scroll position and ring clipping.
The preview focus test samples real rendered pixels at the outline's outer edge.

Remote verification: 22 overview tests; shell test-target compilation;
shell all-target Clippy with warnings denied; exact staged repository hook.
Tests cover native Tab/Shift-Tab, cancel, reopen, repeated Enter, semantic child
order, focused geometry, resized compact creation and hidden preview hit targets.
Existing pointer/scroll/panel/transition and reduced-motion tests also pass.
The two original control regressions fail on the recorded baseline.

This is offscreen production rendering and native host input routing. Live
compositor/VM/device validation remains a separate layer. No real session or
device effects were executed. Broker, file-transfer, undo, credentials, grants,
signing trust and security policy are unchanged.
